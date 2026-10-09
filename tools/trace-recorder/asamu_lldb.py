"""LLDB front end of the ASAMU trace recorder (original Mac build, x86_64).

Attaches nothing by itself: run it inside an LLDB session that is already
attached to the running game (under Rosetta 2 on Apple silicon). It sets one
breakpoint at the entry of ``UWorld::Tick`` whose Python callback samples the
player and returns False, so the game continues at once (no stop is reported).
The only change to the process is the breakpoint instruction LLDB places; the
recorder never writes game memory. Full procedure: docs/TRACE_CAPTURE.md.

    (lldb) command script import tools/trace-recorder/asamu_lldb.py
    (lldb) asamu-rec check
    (lldb) asamu-rec start --scenario T1 --frames 900 --launch-options "-BENCHMARK -FPS=60"
    (lldb) continue
    ... play the scenario; recording ends after --frames, when the STOP file
    ... appears in the output folder, or after `process interrupt` + `asamu-rec stop`
    (lldb) asamu-rec status

Why ``UWorld::Tick``: ``UGameEngine::Tick`` calls it exactly once per frame,
after ``Client->Tick`` has dispatched the frame's input to ``PlayerInput`` and
before any actor ticks, so at its entry the pressed keys belong to the coming
frame and the pawn, gun and camera hold the state the previous frame ended
with (evidence in tools/trace-recorder/layout_mac_x86_64.json and
docs/TRACE_CAPTURE.md). ``APawn::performPhysics`` would run in the middle of
the frame (before the grapple gun's pull and release), once per pawn, and
``UGameEngine::Tick`` entry would come before the frame's input dispatch.
"""

import argparse
import os
import shlex
import sys
import time

import lldb

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.dont_write_bytecode = True  # keep the repository free of __pycache__
import asamu_recorder_core as core  # noqa: E402

RECORDING = None
PROGRESS_EVERY = 1800
STOP_CHECK_EVERY = 30


def __lldb_init_module(debugger, internal_dict):
    debugger.HandleCommand("command script add -f %s.cmd_rec asamu-rec" % __name__)
    print("asamu-rec loaded (%s). Commands: check, start, stop, status, convert." % core.RECORDER)


# ------------------------------------------------------------------ lldb glue


def _main_module(target):
    for i in range(target.GetNumModules()):
        m = target.GetModuleAtIndex(i)
        if m.GetFileSpec().GetFilename() == "ASAMU":
            return m
    return target.GetModuleAtIndex(0) if target.GetNumModules() else None


def resolve_symbols(target, layout, file_addresses=False):
    """mangled name (as in the layout) -> load address, plus missing names.

    ``file_addresses`` returns the unrelocated addresses instead (for the
    static check, which has no process).
    """
    module = _main_module(target)
    if module is None or not module.IsValid():
        return {}, [s["mangled"] for s in layout.data["symbols"].values()]
    wanted = {}
    for spec in layout.data["symbols"].values():
        m = spec["mangled"]
        # LLDB drops the Mach-O leading underscore.
        wanted[m[1:] if m.startswith("_") else m] = m
    found = {}
    for short, mangled in wanted.items():
        ctx_list = module.FindSymbols(short)
        for i in range(ctx_list.GetSize()):
            sym = ctx_list.GetContextAtIndex(i).GetSymbol()
            if short in (sym.GetMangledName(), sym.GetName()):
                found[mangled] = sym
                break
    if len(found) < len(wanted):
        for i in range(module.GetNumSymbols()):
            sym = module.GetSymbolAtIndex(i)
            short = sym.GetMangledName() or sym.GetName()
            if short in wanted and wanted[short] not in found:
                found[wanted[short]] = sym
    addrs = {}
    missing = []
    for short, mangled in wanted.items():
        sym = found.get(mangled)
        a = lldb.LLDB_INVALID_ADDRESS
        if sym is not None:
            start = sym.GetStartAddress()
            a = start.GetFileAddress() if file_addresses else start.GetLoadAddress(target)
        if sym is None or a == lldb.LLDB_INVALID_ADDRESS:
            missing.append(mangled)
        else:
            addrs[mangled] = a
    return addrs, missing


def make_reader(process):
    def read(addr, size):
        err = lldb.SBError()
        data = process.ReadMemory(addr, size, err)
        if not err.Success() or data is None:
            return None
        return data

    return read


def _executable_path(target):
    try:
        spec = target.GetExecutable()
        d, f = spec.GetDirectory(), spec.GetFilename()
        if d and f:
            return os.path.join(d, f)
    except Exception:  # pragma: no cover - depends on the LLDB build
        pass
    module = _main_module(target)
    if module is not None:
        spec = module.GetFileSpec()
        d, f = spec.GetDirectory(), spec.GetFilename()
        if d and f:
            return os.path.join(d, f)
    return None


def _xmm0_float(frame):
    try:
        reg = frame.FindRegister("xmm0")
        data = reg.GetData()
        err = lldb.SBError()
        v = data.GetFloat(err, 0)
        return v if err.Success() else None
    except Exception:  # register sets differ between LLDB versions
        return None


# ------------------------------------------------------------------ recording


class Recording:
    def __init__(self, target, process, sampler, opts, fh, path):
        self.target = target
        self.process = process
        self.sampler = sampler
        self.opts = opts
        self.path = path
        self.fh = fh
        self.header_written = False
        self.written = 0
        self.skipped = {}
        self.gaps = 0
        self.last_frame = None
        self.errors = 0
        self.ticks = 0
        self.finished = False
        self.message = None
        self.bp = None
        self.timing = sampler.timing()
        fixed = self.timing.get("fixed_delta_time")
        self.pace_dt = None
        if opts.pace > 0 and self.timing.get("benchmarking") and fixed:
            self.pace_dt = fixed / opts.pace
        self.t0 = None
        self.stop_file = os.path.join(os.path.dirname(path), "STOP")
        self.outputs = []

    def tick(self, frame):
        self.ticks += 1
        reg = frame.FindRegister("rdi")
        world_ptr = reg.GetValueAsUnsigned() if reg.IsValid() else None
        dt = _xmm0_float(frame)
        counter = self.sampler.frame_counter()
        rec, why = self.sampler.sample(counter, dt, world_ptr=world_ptr)
        if rec is None:
            self.skipped[why] = self.skipped.get(why, 0) + 1
            if why == "sentinel-mismatch":
                self.finish(
                    "layout check failed, nothing recorded from this object: "
                    + "; ".join(self.sampler.sentinel_failures[:4])
                )
                return
        else:
            if not self.header_written:
                header = core.make_header(
                    self.sampler.L,
                    self.sampler.bindings,
                    scenario=self.opts.scenario,
                    launch_options=self.opts.launch_options,
                    timing=self.timing,
                    notes=self.opts.note,
                )
                self.fh.write(core.dumps(header) + "\n")
                self.header_written = True
            if self.last_frame is not None and counter != self.last_frame + 1:
                self.gaps += 1
            self.last_frame = counter
            self.fh.write(core.dumps(rec) + "\n")
            self.written += 1
            if self.written % 60 == 0:
                self.fh.flush()
            if self.written % PROGRESS_EVERY == 0:
                print("asamu-rec: %d frames recorded" % self.written)
        if self.pace_dt is not None:
            now = time.perf_counter()
            if self.t0 is None:
                self.t0 = now
            ahead = self.t0 + self.ticks * self.pace_dt - now
            if ahead > 0:
                time.sleep(min(ahead, 0.25))
            elif ahead < -0.25:
                # Fell behind (slow frames, or the process was interrupted):
                # pace from now on instead of letting the game catch up.
                self.t0 = now - self.ticks * self.pace_dt
        if self.opts.frames and self.written >= self.opts.frames:
            self.finish("frame limit reached")
        elif self.ticks % STOP_CHECK_EVERY == 0 and os.path.exists(self.stop_file):
            self.finish("STOP file found")

    def finish(self, message):
        if self.finished:
            return
        self.finished = True
        self.message = message
        if self.bp is not None and self.bp.IsValid():
            self.bp.SetEnabled(False)
        self.fh.flush()
        self.fh.close()
        print("asamu-rec: stopped (%s); %d frames in %s" % (message, self.written, self.path))
        if self.skipped:
            print("asamu-rec: frames skipped: %s" % ", ".join("%s %d" % kv for kv in sorted(self.skipped.items())))
        if self.header_written and self.written >= 2:
            try:
                self.outputs = core.convert_file(self.path)
                for p in self.outputs:
                    print("asamu-rec: trace %s" % p)
            except Exception as e:  # keep the raw file; convert later with asamu-trace convert
                print("asamu-rec: conversion failed (%s); run `asamu-trace convert %s`" % (e, self.path))
        else:
            print("asamu-rec: fewer than 2 frames recorded; no trace written")


def on_world_tick(frame, bp_loc, *rest):
    rec = RECORDING
    if rec is None or rec.finished:
        return False
    try:
        rec.tick(frame)
    except Exception as e:  # a failed read must never stop the game
        try:
            rec.errors += 1
            rec.skipped["error"] = rec.skipped.get("error", 0) + 1
            if rec.errors in (1, 10, 100):
                print("asamu-rec: sample failed (%s)" % e)
            if rec.errors >= 600:
                rec.finish("too many failed samples")
        except Exception:  # e.g. the disk is full: never raise into LLDB
            rec.finished = True
            try:
                if rec.bp is not None:
                    rec.bp.SetEnabled(False)
            except Exception:
                pass
    return False


# ------------------------------------------------------------------- commands


def _parser():
    ap = argparse.ArgumentParser(prog="asamu-rec", add_help=True)
    sub = ap.add_subparsers(dest="cmd")
    sub.add_parser("check", help="resolve symbols, check the layout on the live objects; records nothing")
    s = sub.add_parser("start", help="set the per-frame breakpoint and start a raw recording")
    s.add_argument("--out", default=None, help="output folder (default research/local/traces)")
    s.add_argument("--scenario", default=None, help="scenario id, e.g. T1 or A4-0.2")
    s.add_argument("--launch-options", default=None, help="the Steam launch options in use (recorded only)")
    s.add_argument("--frames", type=int, default=0, help="stop after this many recorded frames")
    s.add_argument("--pace", type=float, default=1.0,
                   help="in fixed-step mode, hold the game to this multiple of real time (0 = off)")
    s.add_argument("--ignore-sentinels", action="store_true", help="record even if the layout check fails")
    s.add_argument("--note", action="append", default=[], help="free-form note (repeatable)")
    s.add_argument("--continue", dest="resume", action="store_true", help="resume the process afterwards")
    sub.add_parser("stop", help="finish the recording (interrupt the process first)")
    sub.add_parser("status", help="counters of the current recording")
    c = sub.add_parser("convert", help="convert a raw recording to canonical trace(s)")
    c.add_argument("raw")
    c.add_argument("--out-dir", default=None)
    return ap


def _setup(debugger, result, ignore_sentinels=False):
    target = debugger.GetSelectedTarget()
    process = target.GetProcess() if target.IsValid() else None
    if process is None or not process.IsValid():
        result.SetError("no process: attach first (process attach --name ASAMU --waitfor)")
        return None
    layout = core.Layout.load()
    addrs, missing = resolve_symbols(target, layout)
    if missing:
        result.SetError("symbols not found: %s (is this the Steam build %s?)" % (", ".join(missing), layout.game_build))
        return None
    try:
        sampler = core.Sampler(layout, make_reader(process), addrs, ignore_sentinels=ignore_sentinels)
    except core.ReadError as e:
        result.SetError("cannot read the game's memory (%s); stop it first with `process interrupt`" % e)
        return None
    return target, process, layout, addrs, sampler


def _check(debugger, result):
    got = _setup(debugger, result)
    if got is None:
        return
    target, process, layout, addrs, sampler = got
    out = []
    out.append("layout %s: %d symbols resolved" % (layout.id, len(addrs)))
    t = sampler.timing()
    out.append(
        "GIsBenchmarking=%s GUseFixedTimeStep=%s GFixedDeltaTime=%r GDeltaTime=%r"
        % (t["benchmarking"], t["fixed_step"], t["fixed_delta_time"], t["delta_time"])
    )
    out.append("GFrameCounter=%d" % sampler.frame_counter())
    out.append("FName::Names[0]=%r (expected 'None')" % sampler.names.name(0, 0))
    try:
        rec, why = sampler.sample(sampler.frame_counter())
    except core.ReadError as e:
        rec, why = None, "memory read failed: %s" % e
    if rec is None:
        out.append("player: not sampled (%s)" % why)
        for f in sampler.sentinel_failures:
            out.append("  layout check failed: %s" % f)
    else:
        p = rec["player"]
        out.append("map %s; controller %s; pawn %s" % (rec["world"]["map"], p["controller_class"], p["pawn_class"]))
        out.append("location %r velocity %r physics %d base %r" % (p["location"], p["velocity"], p["physics"], p["base"]))
        out.append("view rotation %r; fov camera %r controller %r" % (p["view_rotation"], p["fov_camera"], p["fov_controller"]))
        out.append("keys %r; gun %r" % (p["keys"], p["gun"]))
        out.append("bindings read: %d; layout sentinels: ok" % len(sampler.bindings or []))
    out.append("memory reads: %d" % sampler.mem.reads)
    result.PutCString("\n".join(out))


def _start(debugger, result, opts):
    global RECORDING
    if RECORDING is not None and not RECORDING.finished:
        result.SetError("a recording is running; asamu-rec stop first")
        return
    if RECORDING is not None and RECORDING.bp is not None and RECORDING.bp.IsValid():
        RECORDING.target.BreakpointDelete(RECORDING.bp.GetID())
        RECORDING.bp = None
    got = _setup(debugger, result, ignore_sentinels=opts.ignore_sentinels)
    if got is None:
        return
    target, process, layout, addrs, sampler = got
    out_dir = os.path.abspath(opts.out or os.path.join("research", "local", "traces"))
    exe = _executable_path(target)
    if exe is None:
        result.SetError("cannot tell where the game is installed; refusing to write anything")
        return
    root = core.install_root(exe)
    if core.is_inside(out_dir, root):
        # Read only: nothing is ever written inside the install.
        result.SetError("output folder %s is inside the game install %s; start LLDB from the "
                        "repository root or pass --out" % (out_dir, root))
        return
    os.makedirs(out_dir, exist_ok=True)
    stop_file = os.path.join(out_dir, "STOP")
    if os.path.exists(stop_file):
        os.remove(stop_file)
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    stem = "%s-%s" % (stamp, core.safe_component(opts.scenario or "free"))
    # Everything that can fail happens before the breakpoint exists, so a
    # failure never leaves a breakpoint without its callback (LLDB would then
    # stop the game at every frame).
    fh, path = core.create_unique(out_dir, stem, ".raw.jsonl")
    try:
        rec = Recording(target, process, sampler, opts, fh, path)
    except core.ReadError as e:
        fh.close()
        os.remove(path)
        result.SetError("cannot read the game's memory (%s); stop it first with `process interrupt`" % e)
        return
    bp = target.BreakpointCreateByAddress(addrs[layout.sym("UWorld::Tick")["mangled"]])
    if not bp.IsValid():
        fh.close()
        os.remove(path)
        result.SetError("could not set the breakpoint on UWorld::Tick")
        return
    bp.SetScriptCallbackFunction("%s.on_world_tick" % __name__)
    if hasattr(bp, "SetAutoContinue"):
        bp.SetAutoContinue(True)
    rec.bp = bp
    RECORDING = rec
    msg = "asamu-rec: recording to %s" % path
    if rec.pace_dt is not None:
        msg += " (paced to %.2fx real time)" % opts.pace
    if not rec.timing.get("benchmarking"):
        msg += "; WARNING: GIsBenchmarking is 0, so frames have variable length (tick_rate null)"
    result.PutCString(msg + "; touch %s to stop" % stop_file)
    if opts.resume:
        process.Continue()


def _stop(debugger, result):
    global RECORDING
    rec = RECORDING
    if rec is None:
        result.PutCString("asamu-rec: no recording")
        return
    rec.finish("stopped by command")
    if rec.bp is not None and rec.bp.IsValid():
        rec.target.BreakpointDelete(rec.bp.GetID())
        rec.bp = None
    result.PutCString("asamu-rec: %d frames, %d gaps, outputs: %s" % (rec.written, rec.gaps, ", ".join(rec.outputs) or "none"))


def _status(result):
    rec = RECORDING
    if rec is None:
        result.PutCString("asamu-rec: no recording")
        return
    result.PutCString(
        "asamu-rec: %s; %d frames recorded, %d gaps, skipped %r, errors %d, file %s"
        % ("finished (%s)" % rec.message if rec.finished else "running", rec.written, rec.gaps,
           rec.skipped, rec.errors, rec.path)
    )


def cmd_rec(debugger, command, result, internal_dict):
    try:
        opts = _parser().parse_args(shlex.split(command))
    except SystemExit:
        return
    if opts.cmd == "check":
        _check(debugger, result)
    elif opts.cmd == "start":
        _start(debugger, result, opts)
    elif opts.cmd == "stop":
        _stop(debugger, result)
    elif opts.cmd == "status":
        _status(result)
    elif opts.cmd == "convert":
        for p in core.convert_file(opts.raw, opts.out_dir):
            result.PutCString(p)
    else:
        _parser().print_help()


def static_check(executable):
    """Resolve the layout's symbols in the executable file with LLDB (no process).

    xcrun python3 tools/trace-recorder/asamu_lldb.py <path to Contents/MacOS/ASAMU>
    (with PYTHONPATH set to the output of `lldb -P`).
    """
    debugger = lldb.SBDebugger.Create()
    debugger.SetAsync(False)
    target = debugger.CreateTarget(executable)
    if not target.IsValid():
        print("cannot open %s" % executable)
        return 2
    layout = core.Layout.load()
    addrs, missing = resolve_symbols(target, layout, file_addresses=True)
    for spec in layout.data["symbols"].values():
        m = spec["mangled"]
        print("%-36s %s" % (m, "0x%x" % addrs[m] if m in addrs else "MISSING"))
    lldb.SBDebugger.Destroy(debugger)
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(static_check(sys.argv[1]) if len(sys.argv) == 2 else 2)
