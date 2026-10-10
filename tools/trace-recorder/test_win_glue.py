"""Tests of asamu_win.py (the Windows front end of the trace recorder).

No game is needed. A fake 32-bit game image is built from
``layout_win_x86.json`` (4-byte pointers, UTF-16 strings, globals at module
base + RVA, a relocated base) and a small engine plays the frame order of
the Win32 build on it: time update, input dispatch, the ``RealTimeSeconds``
store at the start of the world tick, actor updates in two steps, the
``GFrameCounter`` increment.

    python3 -I tools/trace-recorder/test_win_glue.py            any OS
    python  test_win_glue.py --live-fake                        Windows only

The default run drives the engine one step per memory read, so every
interleaving is exact and repeatable: torn frames, missed frames, late
samples, variable frame lengths. ``--live-fake`` (Windows) starts the same
engine as a separate process running in real time and records it through the
real API path (``OpenProcess`` with read rights, ``ReadProcessMemory``); it
also checks the module search on a 32-bit system process.
"""

import contextlib
import io
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True  # keep the repository free of __pycache__

import asamu_recorder_core as core  # noqa: E402
import asamu_win as win  # noqa: E402

LAYOUT_PATH = os.path.join(HERE, win.LAYOUT_FILE)
BASE = 0x00D50000  # not the preferred base: every address has to be relocated
CHECKS = [0]


def ok(cond, what=""):
    CHECKS[0] += 1
    if not cond:
        raise AssertionError(what)


# ------------------------------------------------------------- fake image


class SparseMemory(core.FakeMemory):
    """Pages on demand; unmapped memory is unreadable, as in a real process.
    Allocations lie ``spread`` bytes apart, as objects do on a real heap, so
    each object costs the recorder a read of its own."""

    PAGE = 0x1000

    def __init__(self, base, heap, ptr_size=4, spread=0x3000):
        self.base = base
        self.ptr_size = ptr_size
        self.top = heap
        self.spread = spread
        self.pages = {}

    def alloc(self, size, align=16):
        a = (self.top + align - 1) // align * align
        self.top = a + size + self.spread
        self.map(a, size)
        return a

    def map(self, addr, size):
        for p in range(addr // self.PAGE, (addr + max(size, 1) - 1) // self.PAGE + 1):
            self.pages.setdefault(p, bytearray(self.PAGE))

    def write(self, addr, data):
        self.map(addr, len(data))
        pos = 0
        while pos < len(data):
            p, o = divmod(addr + pos, self.PAGE)
            n = min(self.PAGE - o, len(data) - pos)
            self.pages[p][o : o + n] = data[pos : pos + n]
            pos += n

    def read(self, addr, size):
        out = bytearray()
        pos = 0
        while pos < size:
            p, o = divmod(addr + pos, self.PAGE)
            page = self.pages.get(p)
            if page is None:
                return None
            n = min(self.PAGE - o, size - pos)
            out += page[o : o + n]
            pos += n
        return bytes(out)


class WinImage:
    """The objects the recorder walks, laid out as in the 32-bit build: the
    executable's header at the module base, the globals at base + RVA, the
    objects on a heap behind the image."""

    def __init__(self, base=BASE, memory=None, layout=None):
        L = self.L = layout or core.Layout.load(LAYOUT_PATH)
        image = L.data["image"]
        self.base = base
        self.mem = m = memory or SparseMemory(base, base + image["size_of_image"] + 0x10000)
        self.sym = {
            spec["mangled"]: base + int(spec["rva"], 16) for spec in L.data["symbols"].values() if "rva" in spec
        }
        self.g = g = core.FakeGame(L, memory=m, symbols_at=self.sym)
        o = self.o = g.o
        hdr = bytearray(0x400)
        hdr[:2] = b"MZ"
        struct.pack_into("<I", hdr, 0x3C, 0x100)
        hdr[0x100:0x104] = b"PE\0\0"
        struct.pack_into("<H", hdr, 0x104, 0x014C)
        struct.pack_into("<I", hdr, 0x108, image["time_date_stamp"])
        struct.pack_into("<I", hdr, 0x150, image["size_of_image"])
        m.write(base, bytes(hdr))
        self.a_delta_time = self.sym[o.sym_delta_time]
        m.write(self.a_delta_time, struct.pack("<d", 1.0 / 60.0))
        m.write(self.sym[o.sym_fixed_delta_time], struct.pack("<d", 1.0 / 30.0))
        m.write(self.sym[o.sym_benchmarking], struct.pack("<I", 0))
        m.write(self.sym[o.sym_fixed_step], struct.pack("<I", 0))
        rva = win.find_extra_rva(L, LAYOUT_PATH, "GCurrentTime")
        self.a_current_time = None if rva is None else base + rva
        if self.a_current_time:
            m.write(self.a_current_time, struct.pack("<d", 0.0))

    def set_benchmark(self, dt):
        self.mem.write(self.sym[self.o.sym_benchmarking], struct.pack("<I", 1))
        self.mem.write(self.sym[self.o.sym_fixed_delta_time], struct.pack("<d", dt))


def default_spec(i):
    """Frame ``i`` of the default script: lengths vary from frame to frame,
    W is held for a while, the space bar goes down for one frame, the left
    mouse button for a few."""
    keys = []
    if 12 <= i < 30:
        keys.append("W")
    if i == 20:
        keys.append("SpaceBar")
    if 36 <= i < 41:
        keys.append("LeftMouseButton")
    return {"dt": 1.0 / 60.0 + (i % 7) * 0.00031, "keys": tuple(keys)}


class Engine:
    """Plays one frame of the Win32 build as six events, in the order
    WINDOWS_BINARY.md 6 shows (the driver decides when each one happens)."""

    SPEED = 300.0

    def __init__(self, img, spec=default_spec, counter=1000):
        self.img = img
        self.g = g = img.g
        self.m = img.mem
        self.o = img.o
        self.spec = spec
        self.counter = counter
        self.index = 0
        self.x = 0.0
        self.v = 0.0
        self.yaw = 0
        self.rts = core.f32(100.0)
        self.ts = core.f32(40.0)
        self.ds = core.f32(1.0 / 60.0)
        self.dt = 1.0 / 60.0
        self.dilation = 1.0
        self.now = 16777216.0 + 500.0  # the engine's clock starts at 2^24 s
        self.keys = ()
        self.inputs = {}  # frame -> what the dispatch of that frame delivered
        self.truth = {}  # frame -> state when that frame had ended
        g.set_state(counter, (), 0.0, 0, 1, False)
        self.m.f(g.pawn + self.o.velocity, 0.0)
        self.m.f(g.wi + self.o.real_time_seconds, self.rts)
        self.m.f(g.wi + self.o.time_seconds, self.ts)
        self.m.f(g.wi + self.o.delta_seconds, self.ds)

    def time_update(self, f):
        self.dt = f["dt"]
        self.now += self.dt
        self.m.write(self.img.a_delta_time, struct.pack("<d", self.dt))
        if self.img.a_current_time:
            self.m.write(self.img.a_current_time, struct.pack("<d", self.now))

    def dispatch(self, f):
        g, o = self.g, self.o
        keys = tuple(f["keys"])
        jump = "SpaceBar" in keys and "SpaceBar" not in self.keys
        g.press(*keys)
        if jump:
            self.m.setbit(g.pc + o.pressed_jump[0], o.pressed_jump[1], True)
        self.keys = keys
        self.inputs[self.counter] = {"keys": list(keys), "dt": self.dt, "jump": jump}

    def tick_start(self, f):
        g, o = self.g, self.o
        dt = core.f32(self.dt)
        self.rts = core.f32(self.rts + dt)
        self.ds = core.f32(min(max(core.f32(dt * self.dilation), 0.0005), 0.4))
        self.ts = core.f32(self.ts + self.ds)
        self.m.f(g.wi + o.real_time_seconds, self.rts)
        self.m.f(g.wi + o.delta_seconds, self.ds)
        self.m.f(g.wi + o.time_seconds, self.ts)

    def actors_a(self, f):
        g, o = self.g, self.o
        if "W" in self.keys:
            self.x = core.f32(self.x + self.SPEED * self.ds)
        self.m.f(g.pawn + o.location, self.x)
        self.m.setbit(g.pc + o.pressed_jump[0], o.pressed_jump[1], False)
        grappling = "LeftMouseButton" in self.keys
        self.m.setbit(g.gun + o.gun_grappling[0], o.gun_grappling[1], grappling)
        self.m.write(g.pawn + o.physics, bytes([4 if grappling else 1]))
        if grappling:
            # The gun's own distance to its anchor (vGrappleLocation, which
            # FakeGame.set_state put at x = 500), measured after the pawn moved.
            d = ((500.0 - self.x) ** 2 + 3.5 ** 2 + 45.05 ** 2) ** 0.5
            self.m.f(g.gun + o.gun_distance, core.f32(d))

    def actors_b(self, f):
        g, o = self.g, self.o
        fwd = "W" in self.keys
        self.v = self.SPEED if fwd else 0.0
        self.m.f(g.pawn + o.velocity, self.v)
        self.yaw += f.get("turn", 16)
        self.m.i(g.pc + o.rotation + 4, self.yaw)
        self.m.f(g.inp + o.a_base_y, 1.0 if fwd else 0.0)

    def frame_end(self, f):
        self.truth[self.counter] = {
            "x": self.x, "v": self.v, "yaw": self.yaw, "rts": self.rts, "ts": self.ts, "ds": self.ds,
            "physics": 4 if "LeftMouseButton" in self.keys else 1,
        }
        self.counter += 1
        self.m.write(self.img.sym[self.o.sym_frame_counter], struct.pack("<Q", self.counter))
        self.index += 1


class VirtualGame:
    """Drives an Engine one step per memory read: the reader's own reads are
    the clock, so a test places every event exactly."""

    DEFAULTS = {"window": 120, "d_update": 2, "d_dispatch": 2, "d_a": 12, "d_b": 6, "d_c": 6}
    STEP = 1e-5  # seconds of virtual time per read

    def __init__(self, engine, frames=None):
        self.e = engine
        self.vtime = 0
        self.wait = 0
        self.frames_left = frames
        self.dead = False
        self.script = self._script()

    def _script(self):
        e = self.e
        while self.frames_left is None or self.frames_left > 0:
            f = dict(self.DEFAULTS)
            f.update(e.spec(e.index))
            yield f["window"]
            e.time_update(f)
            yield f["d_update"]
            e.dispatch(f)
            yield f["d_dispatch"]
            e.tick_start(f)
            yield f["d_a"]
            e.actors_a(f)
            yield f["d_b"]
            e.actors_b(f)
            yield f["d_c"]
            e.frame_end(f)
            if self.frames_left is not None:
                self.frames_left -= 1

    def step(self, n=1):
        self.vtime += n
        self.wait -= n
        while self.wait <= 0 and self.script is not None:
            try:
                self.wait += next(self.script)
            except StopIteration:
                self.script = None

    def read(self, addr, size):
        self.step()
        if self.dead:
            return None
        return self.e.m.read(addr, size)

    def clock(self):
        return self.vtime * self.STEP

    def sleep(self, seconds):
        self.step(max(1, int(round(seconds / self.STEP))))


# ------------------------------------------------------------- test helpers


class Dirs:
    """A temporary 'install' and a separate working folder."""

    def __enter__(self):
        self.root = tempfile.mkdtemp(prefix="asamu-win-")
        self.install = os.path.join(self.root, "steamapps", "common", "A Story About My Uncle")
        self.exe = os.path.join(self.install, "Binaries", "Win32", win.EXE_NAME)
        os.makedirs(os.path.dirname(self.exe))
        self.out = os.path.join(self.root, "work", "traces")
        self.ctl = os.path.join(self.out, "ctl")
        return self

    def __exit__(self, *exc):
        shutil.rmtree(self.root, ignore_errors=True)


def start_opts(*args):
    return win.parser().parse_args(["start"] + list(args))


def make_session(vg, img, exe=None, **kw):
    target = win.MemoryTarget(vg.read, img.base, exe_path=exe, pid=4242, alive=lambda: not vg.dead)
    return win.Session(target, img.L, LAYOUT_PATH, **kw)


def run_recording(d, vg, img, *args, **kw):
    """Records from the virtual game; returns (Recording, header, records)."""
    sess = kw.pop("session", None) or make_session(vg, img, d.exe)
    os.makedirs(d.ctl, exist_ok=True)
    rec = win.Recording(sess, start_opts(*args), d.out, d.ctl, clock=vg.clock, sleep=vg.sleep)
    for name, value in kw.items():
        setattr(rec.poller, name, value)
    rec.open()
    rec.run()
    if rec.path is None:
        return rec, None, []
    header, records = core.read_raw(rec.path)
    return rec, header, records


def check_against_truth(e, records, what=""):
    """Every record is exactly one finished frame's state plus the input of
    the frame that followed: nothing torn, nothing shifted."""
    for r in records:
        c = r["frame"]
        p, w = r["player"], r["world"]
        t = e.truth[c - 1]
        got = (p["location"][0], p["velocity"][0], p["view_rotation"][1], p["physics"],
               w["real_time_seconds"], w["time_seconds"], w["delta_seconds"])
        want = (t["x"], t["v"], t["yaw"], t["physics"], t["rts"], t["ts"], t["ds"])
        ok(got == want, "%s frame %d: state %r is not the end of frame %d %r" % (what, c, got, c - 1, want))
        if r["dt_arg"] is not None:
            i = e.inputs[c]
            ok(p["keys"] == i["keys"], "%s frame %d: keys %r, dispatched %r" % (what, c, p["keys"], i["keys"]))
            ok(r["dt_arg"] == core.f32(i["dt"]), "%s frame %d: dt_arg %r" % (what, c, r["dt_arg"]))
            ok(p["pressed_jump"] == i["jump"], "%s frame %d: pressed_jump" % (what, c))


def frames_of(records):
    return [r["frame"] for r in records]


# -------------------------------------------------------------------- tests


def test_image_and_layout():
    img = WinImage()
    vg = VirtualGame(Engine(img))
    sess = make_session(vg, img)
    L = img.L
    ok(L.pointer_size == 4 and L.id.startswith(win.PLATFORM), L.id)
    for name, spec in L.data["symbols"].items():
        if "rva" in spec:
            ok(sess.addrs[spec["mangled"]] == BASE + int(spec["rva"], 16), name)
    ok(sess.image == (L.data["image"]["time_date_stamp"], L.data["image"]["size_of_image"]))
    ok(sess.sampler.names.name(0, 0) == "None")
    ok(sess.sampler.names.name(img.g.ni["Wide\u00e9Name"], 0) == "Wide\u00e9Name", "UTF-16 name entry")
    ok(sess.late_marker() == (img.a_delta_time, "GDeltaTime"))
    rec, why = sess.sampler.sample(1000)
    ok(why is None and rec["world"]["map"] == "AG-Workshop", why)
    ok(rec["player"]["controller_class"] == "ASAMUPlayerController" and rec["player"]["pawn_class"] == "ASAMUPawn")
    ok(len(sess.sampler.bindings) == 6 and sess.sampler.bindings[1] == {"name": "W", "command": "GBA_MoveForward"})
    ok(rec["player"]["gun"]["max_grapples"] == 2 and rec["player"]["boots"]["enabled"] is True)
    # Another build (or another program) at that address is refused.
    other = WinImage()
    other.mem.write(BASE + 0x108, struct.pack("<I", 12345))
    try:
        make_session(VirtualGame(Engine(other)), other)
        ok(False, "a different time stamp was accepted")
    except win.BuildMismatch as e:
        ok("12345" in str(e), str(e))
    other = WinImage()
    other.mem.write(BASE, b"ZM")
    try:
        make_session(VirtualGame(Engine(other)), other)
        ok(False, "a missing header was accepted")
    except win.BuildMismatch:
        ok(True)
    # A layout with 8-byte pointers (the Mac one) is not for this front end.
    try:
        win.Session(win.MemoryTarget(vg.read, BASE), core.Layout(dict(L.data, pointer_size=8)), None)
        ok(False, "a 64-bit layout was accepted")
    except core.LayoutError:
        ok(True)
    # Fixed step: GDeltaTime never changes, so the late marker is GCurrentTime.
    img = WinImage()
    img.set_benchmark(1.0 / 60.0)
    sess = make_session(VirtualGame(Engine(img)), img)
    ok(sess.late_marker() == (img.a_current_time, "GCurrentTime") and img.a_current_time is not None)
    sess.a_current_time = None
    ok(sess.late_marker() == (None, None))


def test_snapshot():
    ok(win.merge_ranges([(100, 4), (104, 8), (5000, 4), (90, 4), (100, 4)]) ==
       [(90, 112, [(90, 4), (100, 4), (104, 8)]), (5000, 5004, [(5000, 4)])])
    ok(len(win.merge_ranges([(0, 4), (win.MERGE_GAP + 5, 4)])) == 2)
    ok(len(win.merge_ranges([(0, 4), (2000, 4), (4000, 4), (6000, 4), (8000, 4), (10000, 4),
                             (12000, 4), (14000, 4), (16000, 4), (18000, 4)])) == 2, "block size limit")
    m = SparseMemory(0x1000, 0x10000)
    a = m.alloc(64)
    m.write(a, bytes(range(64)))
    b = a + 0x3000
    m.write(b, b"\x07" * 16)
    reads = []

    def read(addr, size):
        reads.append((addr, size))
        return m.read(addr, size)

    s = win.Snapshot(read)
    ok(s.read(a, 4) == bytes(range(4)) and s.misses == 0, "outside a sample every read is live")
    s.capture()
    ok(s.read(a + 8, 4) == bytes(range(8, 12)) and s.read(a + 40, 2) == bytes([40, 41]) and s.read(b, 8) == b"\x07" * 8)
    ok(s.misses == 3)
    s.commit()
    ok(s.plan == [(a + 8, a + 42, [(a + 8, 4), (a + 40, 2)]), (b, b + 8, [(b, 8)])], s.plan)
    del reads[:]
    ok(s.capture() == 2 and reads == [(a + 8, 34), (b, 8)], reads)
    m.write(a + 8, b"\xff\xff\xff\xff")  # the game moves on after the burst
    ok(s.read(a + 8, 4) == bytes(range(8, 12)), "a sample reads the burst, not the live memory")
    ok(s.read(a + 40, 2) == bytes([40, 41]) and s.misses == 0 and len(reads) == 2)
    ok(s.read(a + 44, 4) == bytes(range(44, 48)) and s.misses == 1, "an unplanned range is read live")
    s.release()
    ok(s.read(a + 8, 4) == b"\xff\xff\xff\xff")
    # A merged span over unreadable memory falls back to its parts.
    c = m.alloc(16) + 0x8000
    m.write(c, b"\x01" * 8)
    m.write(c + 0x3000, b"\x02" * 8)
    ok(m.read(c, 0x3008) is None, "the pages in between are not mapped")
    s = win.Snapshot(m.read)
    s.plan = [(c, c + 0x3008, [(c, 8), (c + 0x3000, 8)])]
    s.capture()
    ok(s.failed == 1 and s.read(c, 8) == b"\x01" * 8 and s.read(c + 0x3000, 8) == b"\x02" * 8 and s.misses == 0)


def test_steady_recording(keep=None):
    with Dirs() as d:
        img = WinImage()
        e = Engine(img)
        vg = VirtualGame(e)
        rec, header, records = run_recording(d, vg, img, "--scenario", "T1", "--frames", "60",
                                             "--launch-options=-WINDOWED", "--note", "unit test")
        st = rec.stats
        ok(rec.done == "frame limit reached" and not rec.failed and rec.state == "finished", rec.done)
        ok(len(records) == 60 and rec.gaps == 0, (len(records), rec.gaps))
        f = frames_of(records)
        ok(f == list(range(f[0], f[0] + 60)), "one record per engine frame, none missing")
        ok(st.torn == 0 and st.missed == 0 and st.late == 0 and st.errors == 0 and st.no_input == 0, st.as_dict())
        ok(1 <= st.unconfirmed <= 3, "only the start-up frames are unconfirmed: %d" % st.unconfirmed)
        check_against_truth(e, records, "steady")
        ok(all(r["dt_arg"] is not None for r in records))
        # Variable frame lengths come through exactly.
        ds = [r["world"]["delta_seconds"] for r in records]
        ok(len(set(ds)) == 7 and ds == [e.truth[r["frame"] - 1]["ds"] for r in records], "seven different lengths")
        # After warm-up a frame costs one burst and no further read.
        ok(st.plan_reads.recent[-1] <= 24 and st.live_reads < 400, (st.plan_reads.recent[-1], st.live_reads))
        ok(header["format"] == "asamu-trace-raw" and header["version"] == 1)
        ok(header["recorder"] == win.RECORDER and header["sample_point"] == win.SAMPLE_POINT)
        ok(header["layout"] == img.L.id and header["game_build"] == img.L.game_build)
        ok(header["scenario"] == "T1" and header["launch_options"] == "-WINDOWED")
        ok(header["benchmarking"] is False and len(header["bindings"]) == 6)
        ok("platform: win-x86" in header["notes"] and "unit test" in header["notes"], header["notes"])
        # The file: whole lines only, no addresses, name component safe.
        with open(rec.path, "rb") as fh:
            data = fh.read()
        ok(data.endswith(b"\n") and b"\r" not in data and len(data.splitlines()) == 61)
        ok(os.path.basename(rec.path).endswith("-T1.raw.jsonl") and os.path.dirname(rec.path) == os.path.abspath(d.out))
        for word in ("0x", "%d" % img.g.pawn, "%d" % BASE):
            ok(word.encode() not in data, "an address in the raw file: %s" % word)
        stats = json.load(open(rec.path[: -len(".raw.jsonl")] + ".stats.json", encoding="utf-8"))
        ok(stats["state"] == "finished" and stats["records"] == 60 and stats["stats"]["torn"] == 0)
        ok(stats["file"] == os.path.basename(rec.path) and "exe" not in json.dumps(stats))
        status = json.load(open(os.path.join(d.ctl, "status.json"), encoding="utf-8"))
        ok(status["state"] == "finished" and status["message"] == "frame limit reached" and status["game_pid"] == 4242)
        # Conversion (the Python mirror of asamu-trace convert).
        traces = core.convert_raw(header, records)
        ok(len(traces) == 1)
        meta, samples = traces[0]
        ok(meta["tick_rate"] is None and meta["source"] == "original" and len(samples) == 60)
        ok(any("asamu_win" in n and "poll after the GFrameCounter" in n for n in meta["notes"]), meta["notes"])
        t = 0.0
        for k in range(1, 60):
            t += float(records[k]["world"]["delta_seconds"])
            ok(samples[k]["time"] == t, "sample time is the sum of the frame lengths")
        # Input timing: the first sample with move_forward is the first one that moves.
        moved = [k for k in range(1, 60) if samples[k]["position"][0] != samples[k - 1]["position"][0]]
        fwd = [k for k in range(60) if samples[k]["input"]["move_forward"] == 1.0]
        ok(fwd and moved == fwd and len(fwd) == 18, (fwd, moved))
        ok(records[fwd[0] - 1]["frame"] == 1000 + 12, "W was dispatched for frame 1012")
        jumps = [k for k in range(60) if samples[k]["input"]["jump_pressed"]]
        ok(len(jumps) == 1 and records[jumps[0] - 1]["frame"] == 1000 + 20, jumps)
        attached = [k for k in range(60) if samples[k]["grapple_state"] == "attached"]
        held = [k for k in range(60) if samples[k]["input"]["grapple_held"]]
        ok(attached == held and len(held) == 5, (attached, held))
        ok(not any(n.startswith("check:") for n in meta["notes"]), meta["notes"])
        if keep:
            os.makedirs(keep, exist_ok=True)
            shutil.copy(rec.path, os.path.join(keep, "win-glue-steady.raw.jsonl"))


def test_fixed_step():
    with Dirs() as d:
        img = WinImage()
        img.set_benchmark(1.0 / 60.0)
        e = Engine(img, spec=lambda i: {"dt": 1.0 / 60.0, "keys": ("W",) if i >= 8 else ()})
        vg = VirtualGame(e)
        rec, header, records = run_recording(d, vg, img, "--frames", "30")
        ok(header["benchmarking"] is True and abs(header["fixed_delta_time"] - 1.0 / 60.0) < 1e-12)
        ok(rec.poller.marker_name == "GCurrentTime" and rec.stats.late == 0 and rec.stats.torn == 0)
        check_against_truth(e, records, "fixed")
        meta, samples = core.convert_raw(header, records)[0]
        ok(meta["tick_rate"] == 60.0, meta["tick_rate"])
        ok(os.path.basename(rec.path).endswith("-free.raw.jsonl"))


def torn_spec(frame, window, same_dt, d_a=12):
    def spec(i):
        f = {"dt": 1.0 / 60.0 if same_dt else 1.0 / 60.0 + (i % 5) * 0.0004, "keys": ("W",) if i % 9 < 5 else ()}
        if i == frame:
            f.update({"window": window, "d_update": 0, "d_dispatch": 0, "d_a": d_a})
        return f

    return spec


def test_torn_frames():
    """A tick that starts while a sample is read: the sample is dropped,
    whatever the timing; no written record ever mixes two frames."""
    dropped = {}
    single = {}
    for same_dt in (True, False):
        for d_a in (1, 12):
            for window in range(0, 64):
                with Dirs() as d:
                    img = WinImage()
                    e = Engine(img, spec=torn_spec(20, window, same_dt, d_a))
                    vg = VirtualGame(e)
                    rec, header, records = run_recording(d, vg, img, "--seconds", "0.09")
                    st = rec.stats
                    check_against_truth(e, records, "window %d" % window)
                    lost = sorted(set(range(records[0]["frame"], records[-1]["frame"] + 1)) - set(frames_of(records)))
                    key = (same_dt, d_a)
                    if st.torn or st.late:
                        # One torn (or late) frame costs one record; two when
                        # the whole tick was shorter than the read (d_a 1).
                        ok(lost in ([1020], [1020, 1021]) and st.torn + st.late == 1, "window %d: lost %r" % (window, lost))
                        ok(rec.gaps == 1 and st.missed == 0 and st.unconfirmed == 1 + len(lost), st.as_dict())
                        dropped.setdefault(key, []).append(window)
                        single[key] = single.get(key, 0) + (len(lost) == 1)
                    else:
                        ok(not lost and rec.gaps == 0, "window %d: lost %r without a torn frame" % (window, lost))
                    # A frame is lost, the recording goes on and stays exact.
                    ok(records[-1]["frame"] >= 1030 and st.errors == 0, records[-1]["frame"])
                    if same_dt:
                        ok(st.late == 0, "equal frame lengths: nothing marks a late sample")
    for key, windows in sorted(dropped.items()):
        # Short windows are torn or late, long ones are fine, and the two
        # ranges do not interleave.
        ok(windows == list(range(0, len(windows))) and 15 <= len(windows) < 40, (key, windows))
    ok(len(dropped) == 4)
    ok(all(single[(s, 12)] >= 10 for s in (True, False)), single)
    # With equal frame lengths the drops are all "torn" (the tick started);
    # with different lengths the earliest sign, the time update, wins.
    with Dirs() as d:
        img = WinImage()
        e = Engine(img, spec=torn_spec(20, 4, True))
        rec, header, records = run_recording(d, VirtualGame(e), img, "--seconds", "0.09")
        ok(rec.stats.torn == 1 and rec.stats.late == 0, rec.stats.as_dict())
        traces = core.convert_raw(header, records)
        ok(len(traces) == 2, "the dropped frame splits the recording into two runs")
        ok(any(n.startswith("frames ") and "segment 1 of 2" in n for n in traces[0][0]["notes"]))


def test_late_samples():
    """The next frame's time update ran before the sample was complete, but
    its tick had not started: dropped by default, kept on request."""

    def spec(i):
        f = {"dt": 1.0 / 60.0 + (i % 5) * 0.0004, "keys": ()}
        if i == 15:
            f.update({"window": 3, "d_update": 150})  # update early, dispatch and tick much later
        return f

    for keep in (False, True):
        with Dirs() as d:
            img = WinImage()
            e = Engine(img, spec=spec)
            args = ["--seconds", "0.07"] + (["--keep-late"] if keep else [])
            rec, header, records = run_recording(d, VirtualGame(e), img, *args)
            st = rec.stats
            ok(st.late == 1 and st.torn == 0 and st.late_kept == (1 if keep else 0), st.as_dict())
            ok((1015 in frames_of(records)) == keep and rec.gaps == (0 if keep else 1))
            check_against_truth(e, records, "late")
            ok(("late samples" in " ".join(header["notes"])) and (("kept" if keep else "dropped") in " ".join(header["notes"])))
    # Without a marker (fixed step and no GCurrentTime address) nothing can
    # be called late; the tick rule still holds.
    with Dirs() as d:
        img = WinImage()
        img.set_benchmark(1.0 / 60.0)
        e = Engine(img, spec=lambda i: dict(spec(i), dt=1.0 / 60.0))
        vg = VirtualGame(e)
        sess = make_session(vg, img, d.exe)
        sess.a_current_time = None
        rec, header, records = run_recording(d, vg, img, "--seconds", "0.07", session=sess)
        ok(rec.poller.marker_name is None and rec.stats.late == 0 and rec.gaps == 0)
        ok("none available" in " ".join(header["notes"]))
        check_against_truth(e, records, "no marker")


def test_missed_frames():
    """Two frames end between two polls (the reader was not scheduled): the
    jump is counted, nothing is guessed, the recording recovers."""

    def spec(i):
        f = {"dt": 1.0 / 60.0 + (i % 3) * 0.0005, "keys": ("W",)}
        if i == 18:
            f.update({"window": 0, "d_update": 0, "d_dispatch": 0, "d_a": 0, "d_b": 0, "d_c": 0})
        return f

    with Dirs() as d:
        img = WinImage()
        e = Engine(img, spec=spec)
        rec, header, records = run_recording(d, VirtualGame(e), img, "--seconds", "0.09")
        st = rec.stats
        ok(st.missed == 1 and st.torn == 0 and st.errors == 0, st.as_dict())
        f = frames_of(records)
        lost = sorted(set(range(f[0], f[-1] + 1)) - set(f))
        # Frame 1018 ran between two polls: its window was never seen, and
        # the sample after it cannot be confirmed (its tick was not seen).
        ok(lost == [1018, 1019], lost)
        ok(rec.gaps == 1 and f[-1] >= 1030 and st.unconfirmed == 3)
        check_against_truth(e, records, "missed")
        by_frame = {r["frame"]: r for r in records}
        ok(by_frame[1017]["dt_arg"] is not None and by_frame[1020]["dt_arg"] is not None)
    # The reader sleeps through whole frames now and then (cpu-saver gone wrong).
    with Dirs() as d:
        img = WinImage()
        e = Engine(img)
        vg = VirtualGame(e)
        sess = make_session(vg, img, d.exe)
        naps = [0]
        real_poll = win.Poller.poll

        def poll(self):
            naps[0] += 1
            if naps[0] % 97 == 0:
                vg.step(400)  # more than two frames
            return real_poll(self)

        win.Poller.poll = poll
        try:
            rec, header, records = run_recording(d, vg, img, "--frames", "80", session=sess)
        finally:
            win.Poller.poll = real_poll
        ok(rec.stats.missed >= 5 and len(records) == 80 and rec.gaps >= 5, rec.stats.as_dict())
        check_against_truth(e, records, "naps")


def test_world_changes():
    """Menu, a new pawn, a pause, and a new map at a new address."""
    with Dirs() as d:
        img = WinImage()
        g, o, m = img.g, img.o, img.mem
        e = Engine(img, spec=lambda i: {"dt": 1.0 / 60.0, "keys": ()})
        vg = VirtualGame(e)
        engine = struct.unpack("<I", m.read(img.sym[o.sym_engine], 4))[0]
        players = m.read(engine + o.game_players, 8)
        events = {}
        real_end = e.frame_end

        def frame_end(f):
            real_end(f)
            fn = events.get(e.counter)
            if fn:
                fn()

        e.frame_end = frame_end
        # Menu: no local player until frame 1010.
        m.i(engine + o.game_players + o.arr_count, 0)
        events[1010] = lambda: m.write(engine + o.game_players, players)

        # Frame 1030: the pawn is replaced by a new object (a respawn).
        def respawn():
            size = 0xC00
            new = m.alloc(size)
            m.write(new, m.read(g.pawn, size))
            m.p(g.pc + o.controller_pawn, new)
            g.pawn = new

        events[1030] = respawn

        # Frames 1050..1059: paused.
        events[1050] = lambda: m.p(g.wi + o.pauser, g.pc)
        events[1060] = lambda: m.p(g.wi + o.pauser, 0)

        # Frame 1080: a new map, its WorldInfo at a new address.
        def travel():
            size = 0xB00
            new = m.alloc(size)
            m.write(new, m.read(g.wi, size))
            pkg = g.obj(0x80, 0, "Camera")  # any name stands in for the next map
            world = g.obj(0x80, 0, "TheWorld", outer=pkg)
            m.p(new + o.outer, g.obj(0x80, 0, "PersistentLevel", outer=world))
            m.p(g.pc + o.world_info, new)
            g.wi = new

        events[1080] = travel
        sleeps = [0]
        real_sleep = vg.sleep

        def sleep(s):
            sleeps[0] += 1
            real_sleep(s)

        vg.sleep = sleep
        rec, header, records = run_recording(d, vg, img, "--frames", "75")
        st = rec.stats
        ok(sleeps[0] >= 3, "in the menu the recorder sleeps between polls")
        ok(st.skipped.get("no-player", 0) >= 3 and st.skipped.get("paused", 0) >= 8, st.skipped)
        ok(st.errors == 0 and st.torn == 0 and st.missed == 0, st.as_dict())
        f = frames_of(records)
        ok(f[0] >= 1011 and not any(1050 <= x <= 1059 for x in f) and 1060 in f, f)
        ok(sorted(set(r["player"]["pawn_id"] for r in records)) == [0, 1])
        ok(min(x for x, r in zip(f, records) if r["player"]["pawn_id"] == 1) in (1030, 1031))
        maps = sorted(set(r["world"]["map"] for r in records))
        ok(maps == ["AG-Workshop", "Camera"], maps)
        check_against_truth(e, records, "world changes")
        traces = core.convert_raw(header, records)
        ok(len(traces) >= 4, "pawn change, pause and map change each start a new run: %d" % len(traces))


def hook_after(engine, event, counter, fn):
    """Runs ``fn`` right after ``event`` of the frame with this counter."""
    real = getattr(engine, event)

    def hooked(f):
        at = engine.counter
        real(f)
        if at == counter:
            fn()

    setattr(engine, event, hooked)


def test_world_reload():
    """A level load that leaves the new world's WorldInfo (and pawn) at the
    old addresses: RealTimeSeconds starts again from zero. Whenever in the
    frame it happens, the run ends there, nothing from the two worlds is
    joined, and the recording goes on with the next frames (a poller that
    takes the reset for a tick start falls one frame behind for good)."""
    for when in ("tick_start", "dispatch", "actors_a", "actors_b", "frame_end"):
        for d_after in (0, 1, 3, 40):
            with Dirs() as d:
                img = WinImage()
                g, o, m = img.g, img.o, img.mem

                def spec(i, d_after=d_after, when=when):
                    f = {"dt": 1.0 / 60.0 + (i % 5) * 0.0003, "keys": ("W",) if i % 9 < 5 else ()}
                    if i == 20:  # how long the load takes before the frame goes on
                        f[{"dispatch": "d_dispatch", "tick_start": "d_a", "actors_a": "d_b",
                           "actors_b": "d_c", "frame_end": "window"}[when]] = d_after + (120 if when == "frame_end" else 0)
                    return f

                e = Engine(img, spec=spec)
                vg = VirtualGame(e)

                def reload_world(e=e, g=g, o=o, m=m):
                    e.rts = core.f32(0.0)
                    e.ts = core.f32(0.0)
                    e.x = 0.0
                    m.f(g.wi + o.real_time_seconds, e.rts)
                    m.f(g.wi + o.time_seconds, e.ts)
                    m.f(g.pawn + o.location, e.x)

                if when == "frame_end":
                    # Just before the counter moves: the reset is never seen
                    # on its own, only as a value that does not fit.
                    real_end = e.frame_end

                    def frame_end(f, e=e, real_end=real_end, reload_world=reload_world):
                        if e.counter == 1020:
                            reload_world()
                        real_end(f)

                    e.frame_end = frame_end
                else:
                    hook_after(e, when, 1020, reload_world)
                rec, header, records = run_recording(d, vg, img, "--seconds", "0.12", "--timeout", "3")
                st = rec.stats
                what = "reload at %s +%d" % (when, d_after)
                f = frames_of(records)
                lost = sorted(set(range(f[0], f[-1] + 1)) - set(f))
                ok(f[-1] >= e.counter - 2 and f[-1] >= 1070, "%s: the recording stopped following at %d of %d" % (what, f[-1], e.counter))
                ok(lost and set(lost) <= {1020, 1021, 1022} and rec.gaps == 1, "%s: lost %r" % (what, lost))
                ok(st.torn + st.resyncs >= 1 and st.torn <= 2 and st.resyncs <= 1 and st.unconfirmed <= 4, "%s: %r" % (what, st.as_dict()))
                ok(st.errors == 0 and st.missed == 0 and st.late <= 1, what)
                check_against_truth(e, records, what)
                # A record without its frame's input is the last of its run.
                for a, b in zip(records, records[1:]):
                    ok(a["dt_arg"] is not None or b["frame"] != a["frame"] + 1, "%s: frame %d" % (what, a["frame"]))
                runs = core.split_runs(records)
                ok(len(runs) == 2, "%s: %d runs" % (what, len(runs)))
                for run in runs:
                    times = [r["world"]["time_seconds"] for r in run]
                    ok(times == sorted(times), "%s: a run crosses the reload" % what)
                ok(runs[0][-1]["frame"] <= 1021 and runs[1][0]["frame"] >= 1021, what)


def test_input_read_failures():
    """The frame's keys cannot be read when its tick starts."""
    # For a moment only: read again while the tick lasts; nothing is lost.
    with Dirs() as d:
        img = WinImage()
        g, o, m = img.g, img.o, img.mem
        e = Engine(img, spec=lambda i: {"dt": 1.0 / 60.0, "keys": ("W", "SpaceBar") if 10 <= i < 20 else ("W",)})
        a_count = g.inp + o.pressed_keys + o.arr_count
        hook_after(e, "tick_start", 1015, lambda: m.i(a_count, 9999))
        hook_after(e, "actors_a", 1015, lambda: m.i(a_count, len(e.keys)))
        rec, header, records = run_recording(d, VirtualGame(e), img, "--frames", "40")
        st = rec.stats
        ok(st.input_retries >= 1 and st.no_input == 0 and rec.gaps == 0 and st.torn == 0, st.as_dict())
        ok(all(r["dt_arg"] is not None for r in records) and "input read" in st.last_error)
        check_against_truth(e, records, "keys unreadable for a moment")
    # For the whole tick: the record is written without input and ends its
    # run; it never sits inside a run with the previous frame's keys.
    with Dirs() as d:
        img = WinImage()
        g, o, m = img.g, img.o, img.mem
        e = Engine(img, spec=lambda i: {"dt": 1.0 / 60.0, "keys": ("W",) if i < 15 else ()})
        a_count = g.inp + o.pressed_keys + o.arr_count
        hook_after(e, "tick_start", 1015, lambda: m.i(a_count, -3))
        real_end = e.frame_end

        def frame_end(f):
            if e.counter == 1015:
                m.i(a_count, len(e.keys))  # readable again as the frame ends
            real_end(f)

        e.frame_end = frame_end
        rec, header, records = run_recording(d, VirtualGame(e), img, "--frames", "40")
        st = rec.stats
        by_frame = {r["frame"]: r for r in records}
        ok(st.no_input == 1 and st.input_retries >= 2 and rec.gaps == 1, st.as_dict())
        ok(by_frame[1015]["dt_arg"] is None and 1016 not in by_frame and 1017 in by_frame, sorted(by_frame))
        ok(all(r["dt_arg"] is not None for r in records if r["frame"] != 1015))
        check_against_truth(e, records, "keys unreadable for a tick")
        runs = core.split_runs(records)
        ok(len(runs) == 2 and runs[0][-1]["frame"] == 1015, [len(r) for r in runs])
    # A key whose name does not resolve is a failed read too, not "no key".
    img = WinImage()
    vg = VirtualGame(Engine(img))
    sess = make_session(vg, img)
    img.g.press("W")
    ok(sess.sampler.pressed_keys(img.g.inp, strict=True) == ["W"])
    img.mem.i(img.g.keys, 999999)
    ok(sess.sampler.pressed_keys(img.g.inp) == [])
    try:
        sess.sampler.pressed_keys(img.g.inp, strict=True)
        ok(False, "a key without a name was accepted")
    except core.ReadError:
        ok(True)


def test_bindings_from_a_window():
    """The key bindings in a recording's header are the ones read with a
    sample taken in a window, not whatever the first look happened to see."""
    with Dirs() as d:
        img = WinImage()
        g, o, m = img.g, img.o, img.mem
        a_count = g.inp + o.bindings + o.arr_count
        m.i(a_count, 2)  # the table as a first look in the middle of its set-up would see it
        e = Engine(img)
        vg = VirtualGame(e)
        sess = make_session(vg, img, d.exe)
        rec0, why = sess.sampler.sample(1000)
        ok(why is None and len(sess.sampler.bindings) == 2)
        m.i(a_count, 6)
        rec, header, records = run_recording(d, vg, img, "--frames", "30", session=sess)
        ok(len(header["bindings"]) == 6 and rec.poller.bindings_seen, header["bindings"])
        check_against_truth(e, records, "bindings")
        meta, samples = core.convert_raw(header, records)[0]
        ok(any(s["input"]["move_forward"] == 1.0 for s in samples), "W is bound in the header's table")
        # Once read in a window they are not read again: the burst stays small.
        ok(rec.stats.plan_reads.recent[-1] <= 24 and rec.stats.live_reads < 400, rec.stats.live_reads)
        ok(rec.poller.bindings_tries == 1 and not any("key bindings were not read" in n for n in header["notes"]))
        ok(rec.status()["bindings"] == 6 and rec.status()["bindings_confirmed"] is True and not rec.failed)
    # No window is ever long enough for them (here: none is tried): the
    # first look's table is kept, the samples are unaffected, the header says so.
    saved = win.BINDINGS_TRIES
    win.BINDINGS_TRIES = 0
    try:
        with Dirs() as d:
            img = WinImage()
            e = Engine(img)
            rec, header, records = run_recording(d, VirtualGame(e), img, "--frames", "20")
            ok(len(records) == 20 and len(header["bindings"]) == 6 and not rec.poller.bindings_seen)
            ok(any(n.startswith("warning: the key bindings were not read in a confirmed window") for n in header["notes"]))
            check_against_truth(e, records, "bindings unconfirmed")
    finally:
        win.BINDINGS_TRIES = saved
    # The window closes while they are read (the next tick starts): what was
    # read is not kept, the sample is, and the next window is tried.
    with Dirs() as d:
        img = WinImage()
        g, o, m = img.g, img.o, img.mem
        e = Engine(img)
        vg = VirtualGame(e)
        sess = make_session(vg, img, d.exe)
        real = sess.sampler.read_bindings
        calls = [0]

        def read_bindings(inp):
            calls[0] += 1
            if calls[0] == 2:  # 1 = with the first sample; 2 = the first try in a window
                m.i(g.inp + o.bindings + o.arr_count, 3)  # a table caught half-way ...
                out = real(inp)
                vg.step(200)  # ... by a read that takes longer than the window
                m.i(g.inp + o.bindings + o.arr_count, 6)
                return out
            return real(inp)

        sess.sampler.read_bindings = read_bindings
        rec, header, records = run_recording(d, vg, img, "--frames", "20", session=sess)
        ok(rec.poller.bindings_seen and rec.poller.bindings_tries == 2 and calls[0] == 3, (rec.poller.bindings_tries, calls))
        ok(len(header["bindings"]) == 6 and rec.gaps <= 1, (header["bindings"], rec.gaps))
        check_against_truth(e, records, "bindings later")
        # The header went out before the second try (with the first look's
        # table and the warning); the confirmed table is the same, so the
        # recording stands.
        st = rec.status()
        ok(not rec.failed and st["bindings"] == 6 and st["bindings_confirmed"] is False, st)
        ok(any(n.startswith("warning: the key bindings were not read") for n in header["notes"]))
        # Had the first look seen another table than the window did, the
        # recording says so and counts as failed (the raw file is kept).
        rec.header_bindings = rec.header_bindings[:2]
        rec.failed = False
        rec.fh = None
        rec.finish()
        ok(rec.failed and rec.state == "failed" and "key bindings in the header differ" in rec.done, rec.done)


def test_failures():
    saved = win.MAX_ERRORS
    win.MAX_ERRORS = 25
    try:
        # The controller's pawn pointer leads into unreadable memory:
        # counted, never raised, and in the end the recording stops by
        # itself with what it has.
        with Dirs() as d:
            img = WinImage()
            e = Engine(img)
            vg = VirtualGame(e)
            real_end = e.frame_end

            def frame_end(f):
                real_end(f)
                if e.counter == 1015:
                    img.mem.p(img.g.pc + img.o.controller_pawn, 0x7F000000)

            e.frame_end = frame_end
            rec, header, records = run_recording(d, vg, img, "--frames", "500")
            ok(rec.failed and rec.done == "too many failed samples in a row" and rec.state == "failed", rec.done)
            ok(rec.stats.errors == 25 and rec.stats.skipped.get("error") == 25, rec.stats.as_dict())
            ok(records and records[-1]["frame"] <= 1015 and "ReadError" in rec.stats.last_error)
            check_against_truth(e, records, "failures")
        # The game exits.
        with Dirs() as d:
            img = WinImage()
            e = Engine(img)
            vg = VirtualGame(e)
            real_end = e.frame_end

            def frame_end(f):
                real_end(f)
                if e.counter == 1020:
                    vg.dead = True

            e.frame_end = frame_end
            rec, header, records = run_recording(d, vg, img, "--frames", "500")
            ok(rec.done == "the game exited" and not rec.failed and rec.state == "finished", rec.done)
            ok(len(records) >= 15)
        # A sentinel that does not hold its class default: the layout is
        # wrong for this object, nothing is recorded from it.
        with Dirs() as d:
            img = WinImage()
            img.mem.f(img.g.gun + img.o.gun_max_distance, 1234.0)
            rec, header, records = run_recording(d, VirtualGame(Engine(img)), img, "--frames", "50")
            ok(rec.failed and "layout check failed" in rec.done and "fMaxDistance" in rec.done, rec.done)
            ok(rec.path is None and not records, "no empty file is left")
            ok([n for n in os.listdir(d.out) if n.endswith(".jsonl")] == [])
        # A failed check on a sample that was not read in a window (here:
        # the start-up frames, and a value that settles) proves nothing; the
        # object is checked again and the recording goes on.
        with Dirs() as d:
            img = WinImage()
            a_sentinel = img.g.gun + img.o.gun_max_distance
            img.mem.f(a_sentinel, 1234.0)
            e = Engine(img)
            vg = VirtualGame(e)
            real_read = vg.read

            def read(addr, size):
                data = real_read(addr, size)
                if addr == a_sentinel:  # seen once with the wrong value, by the first (start-up) sample
                    img.mem.f(a_sentinel, 5000.0)
                return data

            vg.read = read
            rec, header, records = run_recording(d, vg, img, "--frames", "20")
            ok(not rec.failed and len(records) == 20 and rec.stats.skipped == {"sentinel-unconfirmed": 1}, rec.stats.skipped)
            ok(rec.sess.sampler.sentinel_failures == [])
            check_against_truth(e, records, "sentinel retry")
        # Errors that come and go never add up to the limit.
        with Dirs() as d:
            img = WinImage()
            e = Engine(img)
            real_end = e.frame_end

            def frame_end(f):
                real_end(f)
                bad = 1010 <= e.counter < 1100 and e.counter % 3 == 0
                img.mem.p(img.g.pc + img.o.controller_pawn, 0x7F000000 if bad else img.g.pawn)

            e.frame_end = frame_end
            rec, header, records = run_recording(d, VirtualGame(e), img, "--frames", "70")
            ok(not rec.failed and rec.stats.errors == 30 and rec.stats.error_run == 0 and len(records) == 70, rec.stats.as_dict())
            check_against_truth(e, records, "sporadic errors")
        with Dirs() as d:
            img = WinImage()
            img.mem.f(img.g.gun + img.o.gun_max_distance, 1234.0)
            vg = VirtualGame(Engine(img))
            sess = make_session(vg, img, d.exe, ignore_sentinels=True)
            rec, header, records = run_recording(d, vg, img, "--frames", "20", session=sess)
            ok(not rec.failed and len(records) == 20)
        # Anything unexpected inside the loop ends the recording, keeps the
        # file and says what happened.
        with Dirs() as d:
            img = WinImage()
            e = Engine(img)
            vg = VirtualGame(e)
            sess = make_session(vg, img, d.exe)
            os.makedirs(d.ctl, exist_ok=True)
            rec = win.Recording(sess, start_opts("--frames", "100"), d.out, d.ctl, clock=vg.clock, sleep=vg.sleep)
            real_emit = rec.poller._emit

            def emit(r, with_input):
                if rec.records == 10:
                    raise RuntimeError("boom")
                real_emit(r, with_input)

            rec.poller._emit = emit
            rec.open()
            rec.run()
            ok(rec.failed and rec.done == "internal error: RuntimeError: boom" and rec.state == "failed", rec.done)
            header, records = core.read_raw(rec.path)
            ok(len(records) == 10)
            check_against_truth(e, records, "internal error")
    finally:
        win.MAX_ERRORS = saved


def test_limits_and_stop():
    # The time limit counts from the first recorded frame.
    with Dirs() as d:
        img = WinImage()
        e = Engine(img)
        vg = VirtualGame(e)
        rec, header, records = run_recording(d, vg, img, "--seconds", "0.05")
        ok(rec.done == "time limit reached" and 25 <= len(records) <= 40, (rec.done, len(records)))
        # A sample still waiting for its frame's input when the limit comes
        # is kept (it ends the run; its keys are never used).
        ok(rec.stats.no_input == (1 if records[-1]["dt_arg"] is None else 0) and rec.stats.no_input <= 1)
        ok(all(r["dt_arg"] is not None for r in records[:-1]))
        check_against_truth(e, records, "seconds")
    # The overall timeout ends a recording that never sees a player.
    with Dirs() as d:
        img = WinImage()
        g, o, m = img.g, img.o, img.mem
        engine = struct.unpack("<I", m.read(img.sym[o.sym_engine], 4))[0]
        m.i(engine + o.game_players + o.arr_count, 0)
        rec, header, records = run_recording(d, VirtualGame(Engine(img)), img, "--frames", "10", "--timeout", "0.5")
        ok(rec.done.startswith("timeout") and rec.path is None and rec.state == "finished", rec.done)
        ok(rec.status()["state"] == "finished" and rec.stats.skipped["no-player"] > 10)
    # A game that stands still (no frame ends) still honours the timeout.
    with Dirs() as d:
        img = WinImage()
        rec, header, records = run_recording(d, VirtualGame(Engine(img), frames=12), img, "--timeout", "0.4")
        ok(rec.done.startswith("timeout") and 8 <= len(records) <= 11, (rec.done, len(records)))
    # The STOP file (found by the helper thread, in real time).
    with Dirs() as d:
        img = WinImage()
        e = Engine(img)
        vg = VirtualGame(e)
        os.makedirs(d.ctl)
        timer = threading.Timer(0.4, lambda: open(os.path.join(d.ctl, "STOP"), "w").close())
        timer.start()
        try:
            rec, header, records = run_recording(d, vg, img, "--scenario", "../../x y")
        finally:
            timer.cancel()
        ok(rec.done == "STOP file found" and len(records) > 50, (rec.done, len(records)))
        ok(os.path.basename(rec.path).endswith("-_.._x_y.raw.jsonl") and os.path.dirname(rec.path) == os.path.abspath(d.out))
        check_against_truth(e, records[:50], "stop")  # (how many frames ran depends on the machine)
        # A second recording in the same second gets its own file.
        os.remove(os.path.join(d.ctl, "STOP"))
        sess = make_session(vg, img, d.exe)
        r1 = win.Recording(sess, start_opts(), d.out, d.ctl, clock=vg.clock, sleep=vg.sleep)
        r2 = win.Recording(sess, start_opts(), d.out, d.ctl, clock=vg.clock, sleep=vg.sleep)
        r1.open()
        r2.open()
        ok(r1.path != r2.path)
        r1.fh.close()
        r2.fh.close()


class FakeOpen:
    """Stands in for open_game: the 'running game' is the virtual one."""

    def __init__(self, vg, img, exe):
        self.vg, self.img, self.exe = vg, img, exe
        self.calls = 0

    def __call__(self, pid=None, base=None, name=win.EXE_NAME):
        self.calls += 1
        if self.vg is None:
            return None
        return win.MemoryTarget(self.vg.read, self.img.base, exe_path=self.exe, pid=4242, alive=lambda: not self.vg.dead)


def run_cli(*argv):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        code = win.main(list(argv))
    return code, out.getvalue()


def test_commands():
    saved = win.open_game, win.raise_own_priority, time.sleep
    win.raise_own_priority = lambda level: None
    try:
        with Dirs() as d:
            img = WinImage()
            e = Engine(img, spec=lambda i: {"dt": 1.0 / 60.0 + (i % 4) * 0.0002, "keys": ("W",)})
            vg = VirtualGame(e)
            win.open_game = FakeOpen(vg, img, d.exe)
            # check: read-only report plus a sampling probe; writes nothing.
            real_probe = win.probe
            win.probe = lambda s, frames, seconds, keep_late=False: real_probe(
                s, frames, seconds, keep_late=keep_late, clock=vg.clock, sleep=vg.sleep)
            try:
                code, text = run_cli("check", "--probe-frames", "40", "--probe-seconds", "1")
            finally:
                win.probe = real_probe
            ok(code == 0, text)
            for want in ("FName::Names[0]='None'", "map AG-Workshop; controller ASAMUPlayerController; pawn ASAMUPawn",
                         "layout sentinels: ok (gun, input, pawn checked of gun, input, pawn)", "bindings read: 6",
                         "module base 0x00D50000 (preferred 0x00400000)", "GIsBenchmarking=False",
                         "0 torn", "0 missed", "0 resyncs", "late marker GDeltaTime", "result: ok", "40 written",
                         "dt_arg read on 40, 0 equal to the frame before"):
                ok(want in text, "check output lacks %r:\n%s" % (want, text))
            ok(not os.path.exists(d.out), "check wrote something")
            # start in the foreground: describes, records, summarises.
            code, text = run_cli("start", "--scenario", "A1", "--frames", "30", "--out", d.out, "--priority", "normal")
            ok(code == 0 and "recording to" in text and "frame limit reached" in text and "30 records" in text, text)
            ok("variable frame lengths" in text and "asamu-win: raw recording" in text)
            raws = [n for n in os.listdir(d.out) if n.endswith(".raw.jsonl")]
            ok(len(raws) == 1 and raws[0].endswith("-A1.raw.jsonl"))
            header, records = core.read_raw(os.path.join(d.out, raws[0]))
            ok(len(records) == 30)
            check_against_truth(e, records, "cli")
            # status and stop without a running recorder.
            code, text = run_cli("status", "--out", d.out)
            ok(code == 0 and "finished (frame limit reached); 30 records, 0 gaps" in text, text)
            code, text = run_cli("status", "--out", d.out, "--json")
            ok(json.loads(text)["records"] == 30)
            code, text = run_cli("stop", "--out", d.out)
            ok(code == 0 and "no recording is running" in text, text)
            code, text = run_cli("status", "--out", os.path.join(d.root, "nothing"))
            ok(code == 0 and "no recording" in text)
            # A status from before any recording existed (the game was not there) reads as one short line.
            ok(win.summary_line({"state": "failed", "message": "x is not running", "failed": True})
               == "asamu-win: failed (x is not running)")
            # --convert writes the canonical trace next to the raw file.
            code, text = run_cli("start", "--frames", "12", "--out", d.out, "--convert")
            ok(code == 0 and "asamu-win: trace " in text, text)
            ok(len([n for n in os.listdir(d.out) if n.endswith(".trace.jsonl")]) == 1)
            # Nothing is ever written inside the game install, and no folder
            # is created there.
            for inside in (os.path.join(d.install, "traces"), os.path.join(d.install, "Binaries", "Win32", "x")):
                code, text = run_cli("start", "--frames", "5", "--out", inside)
                ok(code == 1 and "inside the game install" in text and not os.path.exists(inside), text)
            code, text = run_cli("start", "--frames", "5", "--out", d.out, "--ctl", os.path.join(d.install, "ctl"))
            ok(code == 1 and "inside the game install" in text and not os.path.exists(os.path.join(d.install, "ctl")))
            win.open_game = FakeOpen(vg, img, None)
            code, text = run_cli("start", "--frames", "5", "--out", d.out)
            ok(code == 1 and "cannot tell where the game is installed" in text, text)
            # Another copy of the game on disk, under any folder name, is
            # recognised by its files: with the game running elsewhere, and
            # with no game running at all (a detached start that waits).
            copy = os.path.join(d.root, "backup", "game copy")
            os.makedirs(os.path.join(copy, "Binaries", "Win32"))
            open(os.path.join(copy, "Binaries", "Win32", win.EXE_NAME), "wb").close()  # an empty stand-in
            ok(win.enclosing_install(os.path.join(copy, "a", "b")) == os.path.realpath(copy))
            ok(win.enclosing_install(os.path.join(copy, "Binaries", "Win32")) == os.path.realpath(copy))
            ok(win.enclosing_install(d.out) is None and win.enclosing_install(os.path.join(d.root, "backup")) is None)
            for opener in (FakeOpen(vg, img, d.exe), FakeOpen(None, None, None)):
                win.open_game = opener
                for args in (["--out", os.path.join(copy, "traces")],
                             ["--out", d.out, "--ctl", os.path.join(copy, "Binaries", "ctl")]):
                    for detach in ([], ["--detach"]):
                        if not detach and opener.vg is None:
                            continue  # in the foreground "not running" comes first; nothing is created either
                        code, text = run_cli("start", "--frames", "5", *(args + detach))
                        ok(code == 1 and "start refused" in text and "inside the game install" in text, text)
                ok(sorted(os.listdir(copy)) == ["Binaries"] and os.listdir(os.path.join(copy, "Binaries")) == ["Win32"])
            win.open_game = FakeOpen(None, None, None)
            named = os.path.join(d.root, "x", "A Story About My Uncle", "traces")
            code, text = run_cli("start", "--detach", "--frames", "5", "--out", named)
            ok(code == 1 and "looks like the game install" in text and not os.path.exists(os.path.join(d.root, "x")), text)
            code, text = run_cli("start", "--frames", "5", "--out", os.path.join(copy, "traces"), "--wait-game", "0")
            ok(code == 2 and not os.path.exists(os.path.join(copy, "traces")), text)
            # A running recorder blocks a second start.
            win.open_game = FakeOpen(vg, img, d.exe)
            st = json.load(open(os.path.join(d.ctl, "status.json"), encoding="utf-8"))
            st.update({"state": "recording", "recorder_pid": os.getpid(), "session": "other"})
            win.write_json_atomic(os.path.join(d.ctl, "status.json"), st)
            code, text = run_cli("start", "--frames", "5", "--out", d.out)
            ok(code == 1 and "a recording is running" in text, text)
            st["recorder_pid"] = 0
            win.write_json_atomic(os.path.join(d.ctl, "status.json"), st)
            code, text = run_cli("status", "--out", d.out)
            ok("stale" in text, text)
            # The game is not running.
            win.open_game = FakeOpen(None, None, None)
            code, text = run_cli("check")
            ok(code == 2 and "is not running" in text, text)
            time.sleep = lambda s: None
            code, text = run_cli("start", "--frames", "5", "--out", d.out, "--wait-game", "0")
            ok(code == 2 and "is not running" in text, text)
        ok(win.win_install_root(os.path.join(os.sep, "g", "Binaries", "Win32", "x.exe")) == os.path.realpath(os.path.join(os.sep, "g")))
        ok(win.win_install_root(os.path.join(os.sep, "g", "bin", "x.exe")) == os.path.realpath(os.path.join(os.sep, "g", "bin")))
    finally:
        win.open_game, win.raise_own_priority, time.sleep = saved


def test_source_rules():
    with open(os.path.join(HERE, "asamu_win.py"), "r", encoding="utf-8") as fh:
        src = fh.read()
    # Read-only by construction: the only access asked for on the game is
    # read + query, and none of the calls that change a process is named.
    ok(win.GAME_ACCESS == 0x0410 and win.GAME_ACCESS_GRANTABLE == 0x1410)
    for word in ("WriteProcessMemory", "VirtualAllocEx", "VirtualProtectEx", "CreateRemoteThread", "SuspendThread",
                 "NtSuspendProcess", "DebugActiveProcess", "SetThreadContext", "PROCESS_ALL_ACCESS", "PROCESS_VM_WRITE",
                 "QueueUserAPC", "SetWindowsHookEx", "TerminateProcess", "DuplicateHandle", "AdjustTokenPrivileges",
                 "SeDebugPrivilege", "NtSetInformationProcess", "WriteVirtualMemory", "CreateThread", "OpenThread",
                 "DebugBreakProcess", "MiniDumpWriteDump"):
        ok(word not in src, "asamu_win.py names %s" % word)
    # One handle to the game (read + query, not inheritable); the second
    # OpenProcess is the recorder asking whether a recorder process lives.
    ok(src.count("OpenProcess(") == 3 and src.count("k.OpenProcess(") == 2
       and "k.OpenProcess(GAME_ACCESS, 0, pid)" in src
       and "k.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, int(pid))" in src)
    # Nothing takes a handle of the system's own to the game: the only
    # Toolhelp snapshot is the process list (no module or heap snapshot,
    # which the system takes with a handle whose rights we would not see).
    ok(src.count("CreateToolhelp32Snapshot(") == 1 and "k.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)" in src)
    for word in ("SNAPMODULE", "SNAPHEAPLIST", "SNAPALL", "Module32", "Heap32"):
        ok(word not in src, "asamu_win.py names %s" % word)
    # The process-changing calls it does make are on itself only.
    ok(src.count("SetPriorityClass(") == 1 and "k.SetPriorityClass(me, " in src and "me = k.GetCurrentProcess()" in src)
    ok(src.count("SetProcessInformation") == 1 and "fn(me, PROCESS_POWER_THROTTLING" in src)
    # Every ReadProcessMemory call site reads through a handle it was given.
    ok(src.count("ReadProcessMemory(") == 1 and src.count("self._rpm(self._h, ") == 3)
    # No personal path, and the Windows API only behind the platform check.
    for word in ("Users" + "\\", "/" + "Users/", "Program" + " Files"):
        ok(word not in src, "asamu_win.py contains %r" % word)
    ok(not win.is_windows() or hasattr(win.kernel32(), "ReadProcessMemory"))
    if not win.is_windows():
        try:
            win.open_game()
            ok(False, "open_game worked off Windows")
        except win.WinError:
            ok(True)
    # Every offset comes from the layout through the core's checked lookups.
    for needle in ('.off("', '.bit("', '.st("', "0x4B4", "0x1D0", "0x428"):
        ok(needle not in src, "asamu_win.py has its own layout literal %s" % needle)


TESTS = [
    test_image_and_layout,
    test_snapshot,
    test_steady_recording,
    test_fixed_step,
    test_torn_frames,
    test_late_samples,
    test_missed_frames,
    test_world_changes,
    test_world_reload,
    test_input_read_failures,
    test_bindings_from_a_window,
    test_failures,
    test_limits_and_stop,
    test_commands,
    test_source_rules,
]


# ------------------------------------------------- live stand-in (Windows)

LIVE_PERIOD = 1.0 / 62.0  # the engine's default smoothed frame rate
LIVE_TICK = (0.0012, 0.0004, 0.0003)  # busy time before each actor step and the frame end


def serve_fake(truth_path, seconds, uncapped=False):
    """The stand-in process: the fake image at real 32-bit addresses in this
    process, the engine running in real time until stdin closes."""
    k = ctypes_kernel32()
    L = core.Layout.load(LAYOUT_PATH)
    size = L.data["image"]["size_of_image"] + 0x400000
    base = None
    for want in (0x20000000, 0x30000000, 0x40000000, 0x50000000, 0x60000000, 0x10000000):
        got = k.VirtualAlloc(want, size, 0x3000, 0x04)  # MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE (own process)
        if got:
            base = got
            break
    if base is None:
        print(json.dumps({"error": "no free low address range"}))
        return 1
    import ctypes

    view = memoryview((ctypes.c_char * size).from_address(base)).cast("B")
    mem = SpreadMemory(base=base, size=size, ptr_size=4, buf=view)
    mem.top = base + L.data["image"]["size_of_image"] + 0x10000
    img = WinImage(base=base, memory=mem, layout=L)

    def spec(i):
        keys = []
        if (i // 40) % 2 == 1:
            keys.append("W")
        if i % 97 == 50:
            keys.append("SpaceBar")
        if (i // 150) % 3 == 2:
            keys.append("LeftMouseButton")
        return {"keys": tuple(keys)}

    e = Engine(img, spec=spec)
    stop = threading.Event()
    threading.Thread(target=lambda: (sys.stdin.read(), stop.set()), daemon=True).start()
    print(json.dumps({"pid": os.getpid(), "base": base}))
    sys.stdout.flush()
    clock = time.perf_counter
    t_next = clock()
    last = t_next - LIVE_PERIOD
    end = t_next + seconds

    def busy(s):
        t = clock() + s
        while clock() < t:
            pass

    while not stop.is_set() and clock() < end:
        f = spec(e.index)
        if not uncapped:
            t_next += LIVE_PERIOD
            wait = t_next - clock() - 0.0015
            if wait > 0:
                time.sleep(wait)
            while clock() < t_next:
                pass
        now = clock()
        f["dt"] = now - last
        last = now
        e.time_update(f)
        e.dispatch(f)
        e.tick_start(f)
        busy(LIVE_TICK[0])
        e.actors_a(f)
        busy(LIVE_TICK[1])
        e.actors_b(f)
        busy(LIVE_TICK[2])
        e.frame_end(f)
    with open(truth_path, "w", encoding="utf-8") as fh:
        json.dump({"truth": e.truth, "inputs": e.inputs}, fh)
    return 0


class SpreadMemory(core.FakeMemory):
    """Objects 0x3000 bytes apart, as on a real heap: each one costs the
    recorder a read of its own, so the burst is as long as with the game."""

    def alloc(self, size, align=16):
        a = core.FakeMemory.alloc(self, size, align)
        self.top += 0x3000
        return a


def ctypes_kernel32():
    import ctypes

    k = ctypes.WinDLL("kernel32", use_last_error=True)
    k.VirtualAlloc.restype = ctypes.c_void_p
    k.VirtualAlloc.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32]
    return k


class LiveTruth:
    def __init__(self, path):
        data = json.load(open(path, encoding="utf-8"))
        self.truth = {int(k): v for k, v in data["truth"].items()}
        self.inputs = {int(k): v for k, v in data["inputs"].items()}


def live_fake(seconds=6.0, frames=300):
    """Windows: the real API path against the stand-in process."""
    if not win.is_windows():
        print("live test skipped: needs Windows")
        return 0
    k = win.kernel32()
    # 1. The module search in a 32-bit process, from this (64-bit) Python.
    sys32 = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "SysWOW64", "cmd.exe")
    if os.path.exists(sys32):
        p = subprocess.Popen([sys32, "/c", "ping -n 4 127.0.0.1 >nul"], stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.5)
            t = win.open_game(pid=p.pid, name="cmd.exe")
            hdr = t.read(t.base, 0x200)
            pe = struct.unpack_from("<I", hdr, 0x3C)[0]
            machine = struct.unpack_from("<H", hdr, pe + 4)[0]
            ok(hdr[:2] == b"MZ" and machine == 0x014C and t.base < 2**32, "32-bit module base %#x" % t.base)
            ok(t.exe_path.lower().endswith("syswow64\\cmd.exe") and t.alive(), t.exe_path)
            ok(win.pid_alive(p.pid) and win.find_pids("cmd.exe"))
            # Both ways to the base, each through the recorder's own handle,
            # agree; and that handle has read + query access and no more.
            peb, psapi = win.peb_image_base(t._h), win.psapi_module_base(t._h, "cmd.exe")
            ok(peb == t.base and psapi == t.base, "PEB %r, PSAPI %r, used %#x" % (peb, psapi, t.base))
            ok(t.access is not None and t.access & ~win.GAME_ACCESS_GRANTABLE == 0
               and t.access & win.GAME_ACCESS == win.GAME_ACCESS, "handle access %r" % t.access)
            ok(win.psapi_module_base(t._h, "asamu-no-such-module.dll") is None)
            print("live: 32-bit module search ok (cmd.exe from SysWOW64 at 0x%08X, machine 0x%04X; "
                  "environment block and PSAPI agree; handle access 0x%04X)" % (t.base, machine, t.access))
            t.close()
        finally:
            p.kill()
            p.wait()
        ok(not win.pid_alive(p.pid))
    ok(win.find_pids("asamu-no-such-process.exe") == [])
    results = {}
    for mode in ("capped", "uncapped"):
        tmp = tempfile.mkdtemp(prefix="asamu-live-")
        truth_path = os.path.join(tmp, "truth.json")
        child = subprocess.Popen(
            [sys.executable, "-I", "-B", os.path.abspath(__file__), "--serve-fake", truth_path, str(seconds + 20.0)]
            + (["--uncapped"] if mode == "uncapped" else []),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            info = json.loads(child.stdout.readline().decode())
            ok("pid" in info, info)
            if mode == "capped":
                # The stand-in is a process of this Python's own width: the
                # same search finds its executable there too.
                t = win.open_game(pid=info["pid"], name=os.path.basename(sys.executable))
                ok(t.read(t.base, 2) == b"MZ" and t.access & ~win.GAME_ACCESS_GRANTABLE == 0, "%#x" % t.base)
                t.close()
            out = os.path.join(tmp, "traces")
            common = ["--pid", str(info["pid"]), "--base", hex(info["base"])]
            if mode == "capped":
                code, text = run_cli("check", "--probe-frames", "120", *common)
                print(text.rstrip())
                ok(code == 0 and "result: ok" in text and "layout sentinels: ok" in text, text)
                # Detached start, status while running, stop.
                code, text = run_cli("start", "--detach", "--scenario", "live-detached", "--out", out, *common)
                print(text.rstrip())
                ok(code == 0 and "recorder started detached" in text, text)
                time.sleep(1.5)
                code, text = run_cli("status", "--out", out)
                ok("recording" in text, text)
                code, text = run_cli("start", "--frames", "5", "--out", out, *common)
                ok(code == 1 and "a recording is running" in text, text)
                code, text = run_cli("stop", "--out", out)
                print(text.rstrip())
                ok(code == 0 and "finished (STOP file found)" in text, text)
                raws = [n for n in os.listdir(out) if n.endswith(".raw.jsonl")]
                ok(len(raws) == 1)
                h, recs = core.read_raw(os.path.join(out, raws[0]))
                ok(len(recs) > 40, len(recs))
                os.remove(os.path.join(out, raws[0]))
            # Foreground recording through the real API.
            code, text = run_cli("start", "--scenario", "live-" + mode, "--frames", str(frames),
                                 "--seconds", str(seconds), "--out", out, *common)
            print(text.rstrip())
            ok(code == 0, text)
            child.stdin.close()
            child.wait(30)
            raws = [n for n in os.listdir(out) if n.endswith(".raw.jsonl")]
            if not raws:  # possible without a frame limit: no window long enough
                ok(mode == "uncapped", "nothing recorded")
                results[mode] = (0, json.load(open(os.path.join(out, "ctl", "status.json"), encoding="utf-8")))
                continue
            header, records = core.read_raw(os.path.join(out, raws[0]))
            truth = LiveTruth(truth_path)
            check_against_truth(truth, records, "live " + mode)
            stats = json.load(open(os.path.join(out, raws[0][: -len(".raw.jsonl")] + ".stats.json"), encoding="utf-8"))
            results[mode] = (len(records), stats)
            if mode == "capped":
                s = stats["stats"]
                ok(len(records) >= frames * 0.9, "only %d records" % len(records))
                ok(s["torn"] + s["late"] + s["missed"] <= max(3, frames // 50), s)
                ok(header["recorder"] == win.RECORDER and core.convert_raw(header, records))
        finally:
            if child.poll() is None:
                child.kill()
            child.stdout.close()
            shutil.rmtree(tmp, ignore_errors=True)
    for mode, (n, stats) in results.items():
        s = stats["stats"]
        print("live %-8s: %d records, %d gaps; torn %d, unconfirmed %d, late %d, missed %d; fps %s; burst %s us; "
              "window %s ms; tick %s ms; recorder CPU %s%%"
              % (mode, n, stats["gaps"], s["torn"], s["unconfirmed"], s["late"], s["missed"], s["fps"],
                 (s["capture_us"] or {}).get("mean"), (s["window_ms"] or {}).get("mean"),
                 (s["tick_ms"] or {}).get("mean"), stats["recorder_cpu_percent"]))
    print("win live test ok (%d checks)" % CHECKS[0])
    return 0


def main(argv):
    if argv and argv[0] == "--serve-fake":
        return serve_fake(argv[1], float(argv[2]), uncapped="--uncapped" in argv)
    if argv and argv[0] == "--live-fake":
        return live_fake()
    keep = argv[1] if len(argv) == 2 and argv[0] == "--keep" else None
    for t in TESTS:
        before = CHECKS[0]
        if t is test_steady_recording:
            t(keep)
        else:
            t()
        if "-v" in argv:
            print("%-28s %d checks" % (t.__name__, CHECKS[0] - before))
    print("win glue test ok (%d checks, %d-bit fake image at 0x%08X)" % (CHECKS[0], 32, BASE))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
