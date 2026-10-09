"""Exercises asamu_lldb.py against a stand-in ``lldb`` module and a fake game image.

No debugger and no game are needed: the stand-in implements only the LLDB
calls the front end makes (the names were checked against the real LLDB
Python module), and memory comes from ``asamu_recorder_core.FakeGame``. Run:

    python3 -I tools/trace-recorder/test_lldb_glue.py

(the asamu-trace test suite runs it when python3 is available).
"""

import json
import os
import shutil
import struct
import sys
import tempfile
import types

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True  # keep the repository free of __pycache__

import asamu_recorder_core as core  # noqa: E402

INVALID = 0xFFFFFFFFFFFFFFFF


class SBError:
    def __init__(self):
        self.ok = True

    def Success(self):
        return self.ok


class _Addr:
    def __init__(self, a):
        self.a = a

    def GetLoadAddress(self, target):
        return self.a

    def GetFileAddress(self):
        return self.a


class _Sym:
    def __init__(self, short, a):
        self.short = short
        self.a = a

    def GetMangledName(self):
        return self.short if self.short.startswith("_Z") else None

    def GetName(self):
        return self.short

    def GetStartAddress(self):
        return _Addr(self.a)


class _Ctx:
    def __init__(self, s):
        self.s = s

    def GetSymbol(self):
        return self.s


class _CtxList:
    def __init__(self, syms):
        self.syms = syms

    def GetSize(self):
        return len(self.syms)

    def GetContextAtIndex(self, i):
        return _Ctx(self.syms[i])


class _FileSpec:
    def __init__(self, directory=None):
        self.directory = directory

    def GetFilename(self):
        return "ASAMU"

    def GetDirectory(self):
        return self.directory


class _Module:
    def __init__(self, syms, directory=None):
        self.syms = syms
        self.directory = directory

    def IsValid(self):
        return True

    def GetFileSpec(self):
        return _FileSpec(self.directory)

    def FindSymbols(self, name):
        # Like LLDB, find C symbols by their name without the leading underscore,
        # and only some C++ symbols through the fast path (forces the fallback).
        return _CtxList([s for s in self.syms if s.short == name and "Names" not in name])

    def GetNumSymbols(self):
        return len(self.syms)

    def GetSymbolAtIndex(self, i):
        return self.syms[i]


class _Breakpoint:
    def __init__(self, addr, bid):
        self.addr = addr
        self.bid = bid
        self.callback = None
        self.enabled = True
        self.auto_continue = False

    def IsValid(self):
        return True

    def SetScriptCallbackFunction(self, name):
        self.callback = name

    def SetAutoContinue(self, v):
        self.auto_continue = v

    def SetEnabled(self, v):
        self.enabled = v

    def GetID(self):
        return self.bid


class _Process:
    def __init__(self, mem):
        self.mem = mem
        self.continued = 0

    def IsValid(self):
        return True

    def ReadMemory(self, addr, size, err):
        data = self.mem.read(addr, size)
        if data is None:
            err.ok = False
        return data

    def Continue(self):
        self.continued += 1


class _Target:
    def __init__(self, module, process):
        self.module = module
        self.process = process
        self.breakpoints = []
        self.deleted = []

    def IsValid(self):
        return True

    def GetNumModules(self):
        return 1

    def GetModuleAtIndex(self, i):
        return self.module

    def GetExecutable(self):
        return _FileSpec(self.module.directory)

    def GetProcess(self):
        return self.process

    def BreakpointCreateByAddress(self, addr):
        bp = _Breakpoint(addr, len(self.breakpoints) + 1)
        self.breakpoints.append(bp)
        return bp

    def BreakpointDelete(self, bid):
        self.deleted.append(bid)
        return True


class _Debugger:
    def __init__(self, target):
        self.target = target
        self.commands = []

    def GetSelectedTarget(self):
        return self.target

    def HandleCommand(self, c):
        self.commands.append(c)


class _Result:
    def __init__(self):
        self.out = []
        self.err = []

    def PutCString(self, s):
        self.out.append(s)

    def SetError(self, s):
        self.err.append(s)


class _Data:
    def __init__(self, f):
        self.f = f

    def GetFloat(self, err, off):
        return self.f


class _Value:
    def __init__(self, u=None, f=None):
        self.u = u
        self.f = f

    def IsValid(self):
        return self.u is not None or self.f is not None

    def GetValueAsUnsigned(self):
        return self.u or 0

    def GetData(self):
        return _Data(self.f)


class _Frame:
    def __init__(self, rdi, xmm0):
        self.regs = {"rdi": _Value(u=rdi), "xmm0": _Value(f=xmm0)}

    def FindRegister(self, name):
        return self.regs.get(name, _Value())


def install_fake_lldb():
    fake = types.ModuleType("lldb")
    fake.LLDB_INVALID_ADDRESS = INVALID
    fake.SBError = SBError
    sys.modules["lldb"] = fake


def main():
    install_fake_lldb()
    import asamu_lldb

    g = core.FakeGame()
    m = g.m
    addrs = dict(g.symbols)
    o = g.o
    bench = m.alloc(4)
    m.write(bench, struct.pack("<I", 1))
    fixed_step = m.alloc(4)
    fixed = m.alloc(8)
    m.write(fixed, struct.pack("<d", struct.unpack("<f", struct.pack("<f", 1.0 / 60.0))[0]))
    delta = m.alloc(8)
    m.write(delta, struct.pack("<d", 1.0 / 60.0))
    addrs[o.sym_benchmarking] = bench
    addrs[o.sym_fixed_step] = fixed_step
    addrs[o.sym_fixed_delta_time] = fixed
    addrs[o.sym_delta_time] = delta
    addrs[o.sym_world_tick] = 0x1009107A0
    addrs[g.L.sym("UGameEngine::Tick")["mangled"]] = 0x100888250
    syms = [_Sym(k[1:], v) for k, v in addrs.items()]
    install = tempfile.mkdtemp(prefix="asamu-glue-install-")
    macos = os.path.join(install, "A Story About My Uncle.app", "Contents", "MacOS")
    target = _Target(_Module(syms, macos), _Process(m))
    dbg = _Debugger(target)

    asamu_lldb.__lldb_init_module(dbg, {})
    assert dbg.commands == ["command script add -f asamu_lldb.cmd_rec asamu-rec"], dbg.commands

    res = _Result()
    g.set_state(999, (), 0.0, 0, 1, False)
    asamu_lldb.cmd_rec(dbg, "check", res, {})
    assert not res.err, res.err
    text = "\n".join(res.out)
    assert "map AG-Workshop" in text and "layout sentinels: ok" in text, text
    assert "FName::Names[0]='None'" in text, text

    tmp = tempfile.mkdtemp(prefix="asamu-glue-")
    try:
        res = _Result()
        asamu_lldb.cmd_rec(
            dbg,
            'start --scenario glue --frames %d --pace 0 --out "%s" --launch-options "-BENCHMARK -FPS=60" --continue'
            % (len(core.FakeGame.FRAMES), tmp),
            res,
            {},
        )
        assert not res.err, res.err
        assert target.process.continued == 1
        bp = target.breakpoints[0]
        assert bp.addr == 0x1009107A0 and bp.callback == "asamu_lldb.on_world_tick", vars(bp)
        # A frame of another world is skipped, not recorded.
        assert asamu_lldb.on_world_tick(_Frame(g.world + 8, 1.0 / 60.0), None) is False
        for n, (ks, x, yaw, phys, grappling, pitch) in enumerate(core.FakeGame.FRAMES):
            g.set_state(1000 + n, ks, x, yaw, phys, grappling, pitch)
            assert asamu_lldb.on_world_tick(_Frame(g.world, 1.0 / 60.0), None) is False
        rec = asamu_lldb.RECORDING
        assert rec.finished and rec.message == "frame limit reached", (rec.finished, rec.message)
        assert rec.skipped == {"other-world": 1}, rec.skipped
        assert not bp.enabled
        assert len(rec.outputs) == 1, rec.outputs
        header, records = core.read_raw(rec.path)
        assert header["launch_options"] == "-BENCHMARK -FPS=60" and header["scenario"] == "glue"
        assert header["benchmarking"] is True and len(header["bindings"]) == 6
        assert [r["frame"] for r in records] == [1000, 1001, 1002, 1003, 1004]
        assert abs(records[0]["dt_arg"] - 1.0 / 60.0) < 1e-12, records[0]["dt_arg"]
        with open(rec.outputs[0], "r", encoding="utf-8") as fh:
            lines = [json.loads(x) for x in fh if x.strip()]
        core.check_selftest_trace(lines[0], lines[1:])
        # Further callbacks after the end do nothing.
        assert asamu_lldb.on_world_tick(_Frame(g.world, 1.0 / 60.0), None) is False
        res = _Result()
        asamu_lldb.cmd_rec(dbg, "status", res, {})
        assert "finished (frame limit reached); 5 frames recorded" in res.out[0], res.out
        res = _Result()
        asamu_lldb.cmd_rec(dbg, "stop", res, {})
        assert target.deleted == [1], target.deleted

        # A STOP file ends a recording too; a failing read is counted, not raised.
        res = _Result()
        asamu_lldb.cmd_rec(dbg, 'start --pace 0 --out "%s"' % tmp, res, {})
        assert not res.err, res.err
        g.set_state(2000, ("W",), 1.0, 0, 1, False)
        asamu_lldb.on_world_tick(_Frame(g.world, 1.0 / 60.0), None)
        open(os.path.join(tmp, "STOP"), "w").close()
        for n in range(1, asamu_lldb.STOP_CHECK_EVERY):
            g.set_state(2000 + n, ("W",), 1.0 + n, 0, 1, False)
            asamu_lldb.on_world_tick(_Frame(g.world, 1.0 / 60.0), None)
        rec = asamu_lldb.RECORDING
        assert rec.finished and rec.message == "STOP file found", rec.message
        assert rec.written == asamu_lldb.STOP_CHECK_EVERY
        second = rec.path
        # The next start removes the finished recording's breakpoint, and a
        # recording started in the same second never overwrites the last one.
        res = _Result()
        asamu_lldb.cmd_rec(dbg, 'start --pace 0 --out "%s"' % tmp, res, {})
        assert not res.err, res.err
        assert target.deleted == [1, 2], target.deleted
        third = asamu_lldb.RECORDING.path
        assert third != second and os.path.getsize(second) > 0, (second, third)
        asamu_lldb.cmd_rec(dbg, "stop", _Result(), {})

        # Nothing is ever written inside the game install, and a refused start
        # leaves no breakpoint behind. A hostile scenario name stays one file
        # name component.
        n_bp = len(target.breakpoints)
        res = _Result()
        inside = os.path.join(install, "A Story About My Uncle.app", "Contents", "traces")
        asamu_lldb.cmd_rec(dbg, 'start --pace 0 --out "%s"' % inside, res, {})
        assert res.err and "inside the game install" in res.err[0], res.err
        assert not os.path.exists(inside) and len(target.breakpoints) == n_bp
        res = _Result()
        asamu_lldb.cmd_rec(dbg, 'start --pace 0 --scenario "../../x y" --out "%s"' % tmp, res, {})
        assert not res.err, res.err
        path = asamu_lldb.RECORDING.path
        assert os.path.dirname(path) == os.path.abspath(tmp), path
        assert os.path.basename(path).endswith("-_.._x_y.raw.jsonl"), path
        asamu_lldb.cmd_rec(dbg, "stop", _Result(), {})

        # Failing reads during a recording are counted, never raised into
        # LLDB; 600 of them end the recording; an error while ending it still
        # never raises and disables the breakpoint.
        res = _Result()
        asamu_lldb.cmd_rec(dbg, 'start --pace 0 --out "%s"' % tmp, res, {})
        assert not res.err, res.err
        rec = asamu_lldb.RECORDING
        saved = target.process.mem
        target.process.mem = core.FakeMemory(base=0x50000000, size=16)
        assert asamu_lldb.on_world_tick(_Frame(g.world, 1.0 / 60.0), None) is False
        assert rec.errors == 1 and not rec.finished, (rec.errors, rec.finished)
        rec.errors = 599
        assert asamu_lldb.on_world_tick(_Frame(g.world, 1.0 / 60.0), None) is False
        assert rec.finished and rec.message == "too many failed samples", rec.message
        asamu_lldb.cmd_rec(dbg, "stop", _Result(), {})
        target.process.mem = saved
        res = _Result()
        asamu_lldb.cmd_rec(dbg, 'start --pace 0 --out "%s"' % tmp, res, {})
        assert not res.err, res.err
        rec = asamu_lldb.RECORDING
        target.process.mem = core.FakeMemory(base=0x50000000, size=16)

        def broken(message):
            raise OSError("disk full")

        rec.finish = broken
        rec.errors = 599
        assert asamu_lldb.on_world_tick(_Frame(g.world, 1.0 / 60.0), None) is False
        assert rec.finished and not rec.bp.enabled
        rec.fh.close()
        target.process.mem = saved

        # Unreadable memory (a running process) is an error message, not a traceback.
        saved = target.process.mem
        target.process.mem = core.FakeMemory(base=0x50000000, size=16)
        res = _Result()
        asamu_lldb.cmd_rec(dbg, "check", res, {})
        assert res.err and "process interrupt" in res.err[0], res.err
        target.process.mem = saved
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
        shutil.rmtree(install, ignore_errors=True)
    print("lldb glue test ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
