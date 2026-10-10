#!/usr/bin/env python3
"""Headless LLDB driver for the trace recorder.

Attaches LLDB (read-only use: the recorder only reads memory) to the running
original game, loads ``asamu_lldb.py`` and then executes commands written to a
control file, so recordings can be driven by a script or an agent without an
interactive LLDB prompt.

Run it with the Python that LLDB ships with (``/usr/bin/python3`` on macOS):

    /usr/bin/python3 tools/trace-recorder/asamu_drive.py --ctl research/local/traces/ctl \
        [--pid N | --wait-name ASAMU]

Control protocol (all files under ``--ctl``):

* write one command line to ``cmd``; the driver picks it up, removes ``cmd``, runs it
  and writes its output to ``out/NNNN.txt`` plus ``last.txt`` (and appends to
  ``driver.log``); ``state.json`` always holds the process state and the last
  command's sequence number.
* commands:
  ``rec <args>``    stop the process, run ``asamu-rec <args>``, continue
  ``recrun <args>`` run ``asamu-rec <args>`` without stopping (status / stop after STOP)
  ``lldb <cmd>``    stop, run a raw LLDB command, continue
  ``interrupt`` / ``continue`` / ``detach`` / ``quit``

Nothing here writes to the game's memory, registers or files.
"""

import argparse
import json
import os
import subprocess
import sys
import time


def _lldb_module():
    path = subprocess.check_output(["xcrun", "lldb", "-P"], text=True).strip()
    sys.path.insert(0, path)
    import lldb  # noqa: E402

    return lldb


lldb = _lldb_module()

PASS_SIGNALS = ["SIGPIPE", "SIGUSR1", "SIGUSR2", "SIGALRM", "SIGCHLD", "SIGVTALRM", "SIGPROF", "SIGURG", "SIGWINCH"]


class Driver:
    def __init__(self, ctl, recorder):
        self.ctl = os.path.abspath(ctl)
        os.makedirs(os.path.join(self.ctl, "out"), exist_ok=True)
        self.log_path = os.path.join(self.ctl, "driver.log")
        self.seq = 0
        self.recorder = recorder
        self.debugger = lldb.SBDebugger.Create()
        self.debugger.SetAsync(False)
        self.interp = self.debugger.GetCommandInterpreter()
        self.target = None
        self.process = None
        self.expect_stop = False
        self.unexpected = 0

    # -- helpers --------------------------------------------------------------------
    def log(self, text):
        stamp = time.strftime("%H:%M:%S")
        with open(self.log_path, "a") as f:
            f.write(f"[{stamp}] {text}\n")

    def run(self, command):
        res = lldb.SBCommandReturnObject()
        self.interp.HandleCommand(command, res)
        out = (res.GetOutput() or "") + (res.GetError() or "")
        return out

    def state(self):
        if not self.process or not self.process.IsValid():
            return "none"
        return lldb.SBDebugger.StateAsCString(self.process.GetState())

    def write_state(self, extra=None):
        data = {"state": self.state(), "seq": self.seq, "pid": self.process.GetProcessID() if self.process else None}
        if extra:
            data.update(extra)
        tmp = os.path.join(self.ctl, "state.json.tmp")
        with open(tmp, "w") as f:
            json.dump(data, f)
        os.replace(tmp, os.path.join(self.ctl, "state.json"))

    def pump(self, timeout=0.0):
        """Drain process events so the public state stays current (async mode)."""
        listener = self.debugger.GetListener()
        event = lldb.SBEvent()
        deadline = time.time() + timeout
        while True:
            remaining = max(0, int(deadline - time.time()))
            if not listener.WaitForEvent(remaining if timeout else 0, event):
                return
            if timeout and time.time() >= deadline:
                return

    def wait_state(self, states, timeout=10.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.pump(0.1)
            if self.process.GetState() in states:
                return True
            time.sleep(0.05)
        return False

    def stop(self):
        if self.process and self.process.GetState() == lldb.eStateRunning:
            self.expect_stop = True
            self.process.Stop()
            self.wait_state((lldb.eStateStopped, lldb.eStateExited, lldb.eStateCrashed))

    def cont(self):
        self.expect_stop = False
        if self.process and self.process.GetState() == lldb.eStateStopped:
            self.process.Continue()
            self.wait_state((lldb.eStateRunning, lldb.eStateExited, lldb.eStateCrashed), timeout=2.0)

    # -- lifecycle ------------------------------------------------------------------
    def attach(self, pid=None, wait_name=None):
        err = lldb.SBError()
        self.target = self.debugger.CreateTarget("")
        if pid:
            self.process = self.target.AttachToProcessWithID(self.debugger.GetListener(), int(pid), err)
        else:
            self.process = self.target.AttachToProcessWithName(self.debugger.GetListener(), wait_name, True, err)
        if not err.Success() or not self.process or not self.process.IsValid():
            raise RuntimeError(f"attach failed: {err.GetCString()}")
        self.log(f"attached pid={self.process.GetProcessID()} state={self.state()}")
        for sig in PASS_SIGNALS:
            self.run(f"process handle -p true -s false -n false {sig}")
        out = self.run(f"command script import {self.recorder}")
        self.log("import: " + out.strip())

    def execute(self, line):
        line = line.strip()
        if not line:
            return ""
        verb, _, rest = line.partition(" ")
        if verb == "rec":
            self.stop()
            out = self.run(f"asamu-rec {rest}")
            self.cont()
        elif verb == "recrun":
            out = self.run(f"asamu-rec {rest}")
        elif verb == "lldb":
            self.stop()
            out = self.run(rest)
            self.cont()
        elif verb == "interrupt":
            self.stop()
            out = f"state={self.state()}"
        elif verb == "continue":
            self.cont()
            out = f"state={self.state()}"
        elif verb == "detach":
            self.stop()
            self.process.Detach()
            out = "detached"
        elif verb == "quit":
            out = "quit"
        else:
            out = f"unknown command: {verb}"
        return out

    def handle_command_file(self, cmd_path):
        with open(cmd_path) as f:
            line = f.read()
        os.remove(cmd_path)
        self.seq += 1
        self.log(f"> {line.strip()}")
        try:
            out = self.execute(line)
        except Exception as e:  # keep the driver alive; report the error
            out = f"error: {e!r}"
        self.log(out.strip())
        for name in (os.path.join(self.ctl, "out", f"{self.seq:04d}.txt"), os.path.join(self.ctl, "last.txt")):
            with open(name, "w") as f:
                f.write(f"> {line.strip()}\n{out}")
        self.write_state({"last": line.strip()})
        return line.strip() in ("quit", "detach")

    def loop(self):
        """Event loop. In async mode a breakpoint's script callback runs when its stop
        event is taken off the listener, and an auto-continued stop arrives flagged as
        "restarted"; so events must be drained immediately or every recorded frame
        waits for us. Commands are checked between events (a cheap stat)."""
        cmd_path = os.path.join(self.ctl, "cmd")
        listener = self.debugger.GetListener()
        event = lldb.SBEvent()
        last_state_write = 0.0
        self.write_state()
        while True:
            if listener.WaitForEvent(1, event):
                if lldb.SBProcess.EventIsProcessEvent(event):
                    st = lldb.SBProcess.GetStateFromEvent(event)
                    restarted = lldb.SBProcess.GetRestartedFromEvent(event)
                    if st == lldb.eStateStopped and not restarted and not self.expect_stop:
                        thread = self.process.GetSelectedThread()
                        reason = thread.GetStopDescription(200) if thread else "?"
                        self.unexpected += 1
                        if self.unexpected <= 20 or self.unexpected % 500 == 0:
                            self.log(f"unexpected stop #{self.unexpected}: {reason}; continuing")
                        self.process.Continue()
                    elif st in (lldb.eStateExited, lldb.eStateCrashed, lldb.eStateDetached):
                        self.log(f"process state {lldb.SBDebugger.StateAsCString(st)}; driver exiting")
                        self.write_state()
                        return
            if os.path.exists(cmd_path):
                if self.handle_command_file(cmd_path):
                    return
            now = time.time()
            if now - last_state_write > 1.0:
                self.write_state({"unexpected_stops": self.unexpected})
                last_state_write = now


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--ctl", default="research/local/traces/ctl")
    ap.add_argument("--pid", type=int)
    ap.add_argument("--wait-name", default="ASAMU")
    ap.add_argument("--recorder", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "asamu_lldb.py"))
    args = ap.parse_args()
    d = Driver(args.ctl, args.recorder)
    d.attach(pid=args.pid, wait_name=None if args.pid else args.wait_name)
    d.log("attach check:\n" + d.run("asamu-rec check"))
    d.debugger.SetAsync(True)
    d.cont()
    d.loop()


if __name__ == "__main__":
    main()
