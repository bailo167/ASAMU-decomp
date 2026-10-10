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


def optional_layout():
    """The Windows layout with its optional fields, or None when
    ``recorder_optional_win32.json`` is not there (the optional tests then
    say so and are skipped)."""
    L = core.Layout.load(LAYOUT_PATH)
    win.load_optional(L, LAYOUT_PATH)
    return L if L.optional_id else None


class OptionalGame(core.FakeGame):
    """The fake game with the names the optional fields need."""

    NAMES = core.FakeGame.NAMES + [
        "CylinderComponent", "State", "Active", "WeaponFiring", "RefireCheckTimer", "InstantReleaseTimer",
        "freds_place", "Package",
    ]


class WinImage:
    """The objects the recorder walks, laid out as in the 32-bit build: the
    executable's header at the module base, the globals at base + RVA, the
    objects on a heap behind the image. ``optional``: also what the optional
    fields lead to (the layout must have its optional part)."""

    def __init__(self, base=BASE, memory=None, layout=None, optional=False):
        L = self.L = layout or (optional_layout() if optional else core.Layout.load(LAYOUT_PATH))
        image = L.data["image"]
        self.base = base
        self.mem = m = memory or SparseMemory(base, base + image["size_of_image"] + 0x10000)
        self.sym = {
            spec["mangled"]: base + int(spec["rva"], 16) for spec in L.data["symbols"].values() if "rva" in spec
        }
        self.g = g = (OptionalGame if optional else core.FakeGame)(L, memory=m, symbols_at=self.sym)
        self.opt = None
        if optional:
            self.build_optional()
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

    def build_optional(self):
        """The camera's field-of-view lock and cached view, the floor normal,
        the view bob, the collision cylinder, the gun's state frame with two
        states and its timers, and a second actor with the floor actor's name
        in another level: every optional field has something to read, and
        every sentinel holds."""
        g, m, L = self.g, self.mem, self.L
        o = g.o
        q = self.opt = core.Optional(L).groups
        ptr = lambda a: struct.unpack("<I", m.read(a, 4))[0]
        meta = ptr(ptr(g.pawn + o.cls) + o.cls)
        self.cam = cam = ptr(g.pc + o.player_camera)
        for group, target in (("fov", cam), ("camera_pov", cam), ("bob", g.pawn)):
            for off, expected, _what in L.optional_sentinels(group):
                m.f(target + off, expected)
        m.f(cam + q["fov"]["default"], 90.0)
        m.write(g.pawn + q["floor"]["normal"], struct.pack("<3f", 0.0, 0.0, 1.0))
        m.f(g.pawn + q["eye"]["base"], 38.0)
        m.f(g.pawn + q["bob"]["bob"], 0.01)
        # The collision cylinder (a component: an object of its own).
        self.cylinder = cyl = g.obj(0x200, g.obj(0x100, meta, "CylinderComponent"), "CylinderComponent")
        m.f(cyl + q["cylinder"]["radius"], 21.0)
        m.f(cyl + q["cylinder"]["height"], 44.0)
        m.p(g.pawn + q["cylinder"]["component"], cyl)
        m.p(g.pawn + q["cylinder"]["collision"], cyl)
        # The gun's state frame and its two states.
        state_cls = g.obj(0x100, meta, "State")
        self.states = {n: g.obj(0x100, state_cls, n) for n in ("Active", "WeaponFiring")}
        self.frame = frame = m.alloc(0x60)
        m.p(frame + q["weapon_state"]["node"], self.states["Active"])
        m.p(g.gun + q["weapon_state"]["frame"], frame)
        # Two timers: the refire check (running while the button is held)
        # and a paused, looping one.
        t = q["timers"]
        self.timers = timers = m.alloc(t["size"] * 4)
        for i, (name, rate, loop, paused) in enumerate(
                (("RefireCheckTimer", 0.0, False, False), ("InstantReleaseTimer", 0.05, True, True))):
            at = timers + i * t["size"]
            m.write(at + t["flags"], struct.pack("<I", (int(loop) << t["loop"]) | (int(paused) << t["paused"])))
            m.i(at + t["name"], g.ni[name])
            m.f(at + t["rate"], rate)
        m.p(g.gun + t["array"] + o.arr_data, timers)
        m.i(g.gun + t["array"] + o.arr_count, 2)
        # The floor actor lies in the persistent level of AG-Workshop; an
        # actor of the same name lies in a streamed level's package.
        self.floors = []
        self.floor_a = ptr(g.pawn + o.base)
        self.floor_b = g.obj(0x300, ptr(self.floor_a + o.cls), "StaticMeshActor", number=13)
        for actor, package in ((self.floor_a, "AG-Workshop"), (self.floor_b, "freds_place")):
            world = g.obj(0x80, 0, "TheWorld", outer=g.obj(0x80, 0, package))
            m.p(actor + o.outer, g.obj(0x80, 0, "PersistentLevel", outer=world))


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
        self.refire = 0.0  # the refire timer's count (optional fields)
        self.optional = None  # what the optional members of a record should be
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
        if self.img.opt:
            self.optional_step(grappling)

    def optional_step(self, grappling):
        """What the optional fields read changes with the frame: the field of
        view is locked and the gun fires while the button is held (its refire
        timer runs), the floor tilts and the view bobs while walking, and
        every tenth frame the pawn stands on the same-named actor of the
        other level."""
        img, g, o, m = self.img, self.g, self.o, self.m
        q, f32 = img.opt, core.f32
        walking = "W" in self.keys
        cam = img.cam
        m.setbit(cam + q["fov"]["locked"][0], q["fov"]["locked"][1], grappling)
        lock = 50.0 if grappling else 0.0
        m.f(cam + q["fov"]["lock"], lock)
        pov = {"location": [self.x, -3.5, f32(45.05 + 38.0)], "rotation": [-100, self.yaw, 0]}
        m.write(cam + q["camera_pov"]["location"], struct.pack("<3f", *pov["location"]))
        m.write(cam + q["camera_pov"]["rotation"], struct.pack("<3i", *pov["rotation"]))
        floor = [f32(0.6), 0.0, f32(0.8)] if walking else [0.0, 0.0, 1.0]
        m.write(g.pawn + q["floor"]["normal"], struct.pack("<3f", *floor))
        b = q["bob"]
        bob = {"bob": f32(0.01), "land": 0.0, "jump": 0.0, "applied": f32(self.x * 0.001), "time": self.ts,
               "just_landed": False, "land_recovery": walking}
        walk = [0.0, f32(self.x * 0.002), f32(self.x * 0.01)]
        for k in ("applied", "time"):
            m.f(g.pawn + b[k], bob[k])
        m.setbit(g.pawn + b["land_recovery"][0], b["land_recovery"][1], walking)
        m.write(g.pawn + b["walk"], struct.pack("<3f", *walk))
        state = "WeaponFiring" if grappling else "Active"
        m.p(img.frame + q["weapon_state"]["node"], img.states[state])
        self.refire = f32(self.refire + self.ds) if grappling else 0.0
        t = q["timers"]
        m.f(img.timers + t["rate"], f32(0.1) if grappling else 0.0)
        m.f(img.timers + t["count"], self.refire)
        other = (self.index // 10) % 2 == 1
        m.p(g.pawn + o.base, img.floor_b if other else img.floor_a)
        self.optional = {
            "base_level": "freds_place" if other else "AG-Workshop",
            "fov_default": 90.0, "fov_locked": grappling, "fov_lock": lock,
            "camera_pov": pov, "floor": floor, "base_eye_height": 38.0, "walk_bob": walk, "bob": bob,
            "cylinder": {"radius": 21.0, "half_height": 44.0, "translation": [0.0, 0.0, 0.0], "collision_component": True},
            "gun.state": state,
            "gun.timers": [
                {"name": "RefireCheckTimer", "rate": f32(0.1) if grappling else 0.0, "count": self.refire,
                 "loop": False, "paused": False},
                {"name": "InstantReleaseTimer", "rate": f32(0.05), "count": 0.0, "loop": True, "paused": True},
            ],
        }

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
        if self.optional is not None:
            self.truth[self.counter]["optional"] = json.loads(json.dumps(self.optional))
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


def check_against_truth(e, records, what="", optional=()):
    """Every record is exactly one finished frame's state plus the input of
    the frame that followed: nothing torn, nothing shifted. ``optional``:
    the optional members every record must have, with that frame's values."""
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
        if optional:
            # The optional members are the same finished frame's, each one.
            want = t["optional"]
            for member in optional:
                name = member[len("player."):]
                got = p["gun"].get(name[4:], "absent") if name.startswith("gun.") else p.get(name, "absent")
                ok(got == want[name], "%s frame %d: %s is %r, the frame ended with %r" % (what, c, name, got, want[name]))


def frames_of(records):
    return [r["frame"] for r in records]


def check_drops(rec, records, what=""):
    """The dropped-frame log accounts for the recording: every frame between
    the first and the last record that has no record is in the log with a
    reason, no logged frame has a record, the counters equal the log, and a
    record without its input is listed as such."""
    st = rec.stats
    drops = st.as_dict()["drops"]
    ok(st.drops_omitted == 0, "%s: the log overflowed" % what)
    have = set(frames_of(records))
    logged = set()
    count = {}
    for d in drops:
        ok(d["last"] >= d["frame"] and d["count"] >= 1 and d["reason"] and d["detail"], "%s: %r" % (what, d))
        count[d["reason"]] = count.get(d["reason"], 0) + d["count"]
        if d["reason"] == "no-input":
            ok(d["frame"] in have, "%s: a record without input that is not in the file: %r" % (what, d))
            continue
        span = set(range(d["frame"], d["last"] + 1))
        ok(not span & have, "%s: logged as dropped but recorded: %r" % (what, d))
        logged |= span
    if records:
        missing = set(range(records[0]["frame"], records[-1]["frame"] + 1)) - have
        ok(missing <= logged, "%s: frames %r have no record and no entry in the log %r" % (what, sorted(missing - logged), drops))
    for reason, n in (("torn", st.torn), ("missed", st.missed), ("unconfirmed", st.unconfirmed),
                      ("late", st.late - st.late_kept), ("no-input", st.no_input)):
        ok(count.get(reason, 0) == n, "%s: %d %s frames counted, %d logged" % (what, n, reason, count.get(reason, 0)))
    return drops


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
                    check_drops(rec, records, "window %d" % window)
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
            drops = check_drops(rec, records, "late")
            late = [x for x in drops if x["reason"] == "late"]
            ok(len(late) == (0 if keep else 1) and all(x["frame"] == 1015 and "GDeltaTime" in x["detail"] for x in late), drops)
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
        drops = check_drops(rec, records, "missed")
        # The log says which frame was never seen and why the one after it
        # has no record either.
        by = {x["frame"]: x for x in drops}
        ok(by[1018]["reason"] == "missed" and "advanced by 2" in by[1018]["detail"], drops)
        ok(by[1019]["reason"] == "unconfirmed" and "advanced by 2" in by[1019]["detail"], drops)
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
        check_drops(rec, records, "naps")


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
        drops = check_drops(rec, records, "world changes")
        # The menu is one entry however many of its frames were seen; the
        # pause is one run of ten frames; the new map's first sample says why
        # it could not be confirmed.
        menu = [x for x in drops if x["detail"] == "no-player"]
        ok(len(menu) == 1 and menu[0]["reason"] == "skipped" and menu[0]["last"] <= 1010, drops)
        paused = [x for x in drops if x["detail"] == "paused"]
        ok(len(paused) == 1 and (paused[0]["frame"], paused[0]["last"]) == (1050, 1059), paused)
        ok(any(x["reason"] == "unconfirmed" and "WorldInfo changed" in x["detail"] for x in drops), drops)


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
                drops = check_drops(rec, records, what)
                # A reset the poller saw says what RealTimeSeconds did.
                if st.resyncs:
                    ok(any("resync: RealTimeSeconds" in x["detail"] for x in drops), "%s: %r" % (what, drops))
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
        by = {x["frame"]: x for x in check_drops(rec, records, "keys unreadable for a tick")}
        ok(by[1015]["reason"] == "no-input" and "input could not be read" in by[1015]["detail"], by)
        ok(by[1016]["reason"] == "unconfirmed" and "input could not be read" in by[1016]["detail"], by)
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


OPTIONAL_MEMBERS = sorted(m for members in core.Optional.MEMBERS.values() for m in members)


def test_optional_fields(keep=None):
    """The optional fields: read in the same burst as the rest, each one the
    finished frame's value; announced in the header; removable without a
    trace; absent from a recording made without them."""
    if optional_layout() is None:
        print("optional-field tests skipped: no %s next to the layout" % win.OPTIONAL_FILE)
        return
    spec = lambda i: dict(default_spec(i), keys=default_spec(i)["keys"] + (("LeftMouseButton",) if 45 <= i < 52 else ()))
    with Dirs() as d:
        img = WinImage(optional=True)
        e = Engine(img, spec=spec)
        vg = VirtualGame(e)
        sess = make_session(vg, img, d.exe, optional=True)
        ok(sorted(sess.sampler.opt_on) == sorted(core.Optional.MEMBERS) and not sess.sampler.opt_missing)
        rec, header, records = run_recording(d, vg, img, "--scenario", "OPT", "--frames", "70", session=sess)
        st = rec.stats
        ok(len(records) == 70 and rec.gaps == 0 and st.torn == 0 and st.errors == 0 and st.late == 0, st.as_dict())
        ok(header["version"] == 1 and header["optional_fields"] == OPTIONAL_MEMBERS, header.get("optional_fields"))
        ok(header["optional_layout"] == img.L.optional_id and img.L.optional_id.endswith("-optional"))
        ok(any(n.startswith("optional fields (members beyond raw version 1") for n in header["notes"]), header["notes"])
        check_against_truth(e, records, "optional", optional=OPTIONAL_MEMBERS)
        check_drops(rec, records, "optional")
        # The values really vary (a constant would pass a broken read).
        p = [r["player"] for r in records]
        ok({x["fov_locked"] for x in p} == {False, True} and {x["fov_lock"] for x in p} == {0.0, 50.0})
        ok({x["base_level"] for x in p} == {"AG-Workshop", "freds_place"} and {x["base"] for x in p} == {"StaticMeshActor_12"},
           "two actors of one name are told apart by their level")
        ok({x["gun"]["state"] for x in p} == {"Active", "WeaponFiring"})
        ok(len({x["gun"]["timers"][0]["count"] for x in p}) >= 6 and len({tuple(x["floor"]) for x in p}) == 2)
        ok(len({tuple(x["walk_bob"]) for x in p}) > 10 and len({tuple(x["camera_pov"]["rotation"]) for x in p}) > 30)
        # Still one burst per frame: the optional reads are planned with the rest.
        ok(st.plan_reads.recent[-1] <= 36 and st.live_reads < 700, (st.plan_reads.recent[-1], st.live_reads))
        status = rec.status()
        ok(status["optional"]["fields"] == OPTIONAL_MEMBERS and not status["optional"]["off"]
           and not status["optional"]["left_out"] and status["optional_in_header"] == OPTIONAL_MEMBERS, status["optional"])
        with open(rec.path, "rb") as fh:
            data = fh.read()
        for word in ("0x", "%d" % img.g.pawn, "%d" % img.cam, "%d" % img.cylinder, "%d" % img.frame, "%d" % BASE):
            ok(word.encode() not in data, "an address in the raw file: %s" % word)
        # The version-1 view: exactly the members of a version-1 file, and
        # the same canonical trace.
        v1_path = os.path.join(d.root, "v1", os.path.basename(rec.path))
        os.makedirs(os.path.dirname(v1_path))
        ok(core.write_v1_view(rec.path, v1_path) == 70)
        h1, r1 = core.read_raw(v1_path)
        ok(set(h1) == set(core.V1_HEADER) and "version-1 view: %d optional fields were removed" % len(OPTIONAL_MEMBERS) in h1["notes"][-1])
        for a in r1:
            ok(set(a) == set(core.V1_RECORD) and set(a["player"]) == set(core.V1_PLAYER) and set(a["player"]["gun"]) == set(core.V1_GUN))
        full, plain = core.convert_raw(header, records), core.convert_raw(h1, r1)
        ok(len(full) == len(plain) == 1 and full[0][1] == plain[0][1], "the optional fields change no sample")
        try:
            core.write_v1_view(rec.path, v1_path)
            ok(False, "the view was written over a file")
        except FileExistsError:
            ok(True)
        if keep:
            os.makedirs(os.path.join(keep, "v1"), exist_ok=True)
            shutil.copy(rec.path, os.path.join(keep, "win-glue-optional.raw.jsonl"))
            shutil.copy(v1_path, os.path.join(keep, "v1", "win-glue-optional.raw.jsonl"))
        # The same game recorded without them (--raw-v1): the records are the
        # view's, member for member and value for value.
        img2 = WinImage(optional=True)
        e2 = Engine(img2, spec=spec)
        vg2 = VirtualGame(e2)
        rec2, header2, records2 = run_recording(d, vg2, img2, "--scenario", "V1", "--frames", "70",
                                                session=make_session(vg2, img2, d.exe))
        ok("optional_fields" not in header2 and set(header2) == set(core.V1_HEADER))
        # (The reader's own reads are the virtual game's clock, so the two
        # recordings need not start on the same frame.)
        view = {r["frame"]: r for r in r1}
        common = [r for r in records2 if r["frame"] in view]
        ok(len(common) >= 60 and all(r == view[r["frame"]] for r in common), "a version-1 recording equals the view")
        ok(rec2.status()["optional"] is None and rec2.stats.plan_reads.recent[-1] < st.plan_reads.recent[-1])
    # An older layout (no optional part) still loads and records: only the
    # member that needs no offset is there, and the header says what is not.
    with Dirs() as d:
        img = WinImage()
        ok(img.L.optional_id is None and core.Optional(img.L).groups == {"base_level": {}})
        e = Engine(img)
        vg = VirtualGame(e)
        sess = make_session(vg, img, d.exe, optional=True)
        rec, header, records = run_recording(d, vg, img, "--frames", "20", session=sess)
        ok(header["optional_fields"] == ["player.base_level"] and header["optional_layout"] is None, header)
        ok(len(sess.sampler.opt_missing) == len(core.Optional.MEMBERS) - 1)
        ok(sum(1 for n in header["notes"] if "not in the layout, not recorded" in n) == len(core.Optional.MEMBERS) - 1)
        ok(all(set(r["player"]) == set(core.V1_PLAYER) | {"base_level"} for r in records))
        ok(all(r["player"]["base_level"] == "StaticMeshActor_12" for r in records), "an actor without an outer names itself")
        check_against_truth(e, records, "old layout")
    # Asked for by name; an unknown group is refused.
    img = WinImage(optional=True)
    vg = VirtualGame(Engine(img))
    sess = make_session(vg, img, optional=("fov", "floor"))
    r, why = sess.sampler.sample(1000)
    ok(why is None and set(r["player"]) == set(core.V1_PLAYER) | {"fov_default", "fov_lock", "fov_locked", "floor"})
    ok(sess.sampler.optional_fields() == ["player.floor", "player.fov_default", "player.fov_lock", "player.fov_locked"])
    try:
        make_session(vg, img, optional=("fov", "nonsense"))
        ok(False, "an unknown optional group was accepted")
    except core.LayoutError:
        ok(True)
    # The Mac layout (8-byte pointers, no optional part; on the game machine,
    # where it is not deployed, the Windows layout without its optional
    # part) and the default sampler: plain version-1 records, member for
    # member.
    mac = os.path.join(HERE, core.DEFAULT_LAYOUT)
    g = core.FakeGame(core.Layout.load(mac if os.path.isfile(mac) else LAYOUT_PATH))
    plain = core.Sampler(g.L, g.m.read, g.symbols)
    g.set_state(1000, ("W",), 1.0, 0, 1, False)
    r, why = plain.sample(1000)
    ok(why is None and set(r) == set(core.V1_RECORD) and set(r["player"]) == set(core.V1_PLAYER)
       and set(r["player"]["gun"]) == set(core.V1_GUN), "the version-1 member lists are the sampler's")
    ok(set(core.make_header(g.L, plain.bindings)) == set(core.V1_HEADER) and plain.optional_fields() == [])
    r, why = core.Sampler(g.L, g.m.read, g.symbols, optional=True).sample(1000)
    ok(why is None and set(r["player"]) == set(core.V1_PLAYER) | {"base_level"} and r["player"]["base_level"] == "StaticMeshActor_12")


def test_optional_checks():
    """What keeps a wrong optional offset out of a recording: a sentinel or
    class check that fails switches the group off for that object, a value
    that cannot be right is left out of that record, and neither costs a
    record or any other field."""
    if optional_layout() is None:
        return
    v1 = set(core.V1_PLAYER)

    def session(img):
        # The image stands still (no engine runs on the reads), so what a
        # test writes into it stays.
        Engine(img)
        return win.Session(win.MemoryTarget(img.mem.read, img.base), img.L, LAYOUT_PATH, optional=True)

    def members(r):
        return set(r["player"]) - v1, set(r["player"]["gun"]) - set(core.V1_GUN)

    everything = ({m[7:] for m in OPTIONAL_MEMBERS if not m.startswith("player.gun.")}, {"state", "timers"})
    img = WinImage(optional=True)
    q, g, m = img.opt, img.g, img.mem
    r, why = session(img).sampler.sample(1000)
    ok(why is None and members(r) == everything, members(r))
    # A sentinel that does not hold: the group is off, with the reason; the
    # other groups and every version-1 field are untouched.
    for group, target, gone in (("fov", "cam", {"fov_default", "fov_lock", "fov_locked"}),
                                ("camera_pov", "cam", {"camera_pov"}), ("bob", "pawn", {"bob", "walk_bob"})):
        img = WinImage(optional=True)
        obj = img.cam if target == "cam" else img.g.pawn
        off, expected, what = img.L.optional_sentinels(group)[0]
        img.mem.f(obj + off, expected + 1.0)
        s = session(img).sampler
        r, why = s.sample(1000)
        ok(why is None and members(r) == (everything[0] - gone, everything[1]), (group, members(r)))
        ok(list(s.opt_off) == [group] and what in s.opt_off[group] and not s.sentinel_failures, s.opt_off)
        ok(all(not x.startswith("player." + n) for n in gone for x in s.optional_fields()))
        # Read while the world was changing (the front end says so): the
        # check is forgotten and made again on the next sample.
        s.optional_unconfirmed()
        ok(not s.opt_off and not s.opt_checked)
        img.mem.f(obj + off, expected)
        r, why = s.sample(1001)
        ok(why is None and members(r) == everything and not s.opt_off)
        # ... and a failure that was confirmed stays: the object is not read again.
        img.mem.f(obj + off, expected + 1.0)
        r, why = s.sample(1002)
        ok(members(r) == everything, "a checked object is not checked on every frame")
    # The cylinder: no component, a component of another class, a size that
    # is none.
    img = WinImage(optional=True)
    q, g, m = img.opt, img.g, img.mem
    s = session(img).sampler
    m.p(g.pawn + q["cylinder"]["component"], 0)
    r, why = s.sample(1000)
    ok(why is None and "cylinder" not in r["player"] and s.opt_rejected["cylinder"] == [1, "the pawn has no cylinder component"])
    m.p(g.pawn + q["cylinder"]["component"], g.gun)
    r, why = s.sample(1001)
    ok("cylinder" not in r["player"] and "found a GrappleGun where a CylinderComponent should be" in s.opt_off["cylinder"], s.opt_off)
    m.p(g.pawn + q["cylinder"]["component"], img.cylinder)
    m.f(img.cylinder + q["cylinder"]["radius"], -1.0)
    r, why = s.sample(1002)
    ok("cylinder" not in r["player"] and s.opt_rejected["cylinder"][0] == 2 and "not a cylinder" in s.opt_rejected["cylinder"][1])
    m.f(img.cylinder + q["cylinder"]["radius"], 21.0)
    m.p(g.pawn + q["cylinder"]["collision"], g.gun)
    r, why = s.sample(1003)
    ok(r["player"]["cylinder"] == {"radius": 21.0, "half_height": 44.0, "translation": [0.0, 0.0, 0.0], "collision_component": False})
    # The floor normal: a unit vector, or none yet (a pawn that has not walked).
    for normal, kept in (((0.0, 0.0, 0.0), True), ((0.0, 0.6, 0.8), True), ((0.0, 0.0, 2.0), False), ((0.5, 0.5, 0.5), False),
                         ((float("nan"), 0.0, 1.0), False), ((0.0, float("inf"), 0.0), False)):
        m.write(g.pawn + q["floor"]["normal"], struct.pack("<3f", *normal))
        r, why = s.sample(1004)
        ok(why is None and ("floor" in r["player"]) == kept, normal)
        ok(not kept or r["player"]["floor"] == list(struct.unpack("<3f", struct.pack("<3f", *normal))))
        core.dumps(r)
    ok(s.opt_rejected["floor"][0] == 4)
    # Numbers that are none never reach a record (the file stays JSON).
    for group, obj, key in (("fov", img.cam, "lock"), ("eye", g.pawn, "base"), ("bob", g.pawn, "time")):
        m.f(obj + q[group][key], float("nan"))
        r, why = s.sample(1005)
        gone = {m[len("player."):] for m in core.Optional.MEMBERS[group]}
        ok(why is None and group in s.opt_rejected and not gone & set(r["player"]), group)
        core.dumps(r)
        m.f(obj + q[group][key], 1.0)
    m.write(img.cam + q["camera_pov"]["location"], struct.pack("<f", float("-inf")))
    r, why = s.sample(1006)
    ok("camera_pov" not in r["player"] and s.opt_rejected["camera_pov"][0] == 1)
    # The gun's state: no frame or no state is "no state"; a node that is no
    # state switches the group off.
    img = WinImage(optional=True)
    q, g, m = img.opt, img.g, img.mem
    s = session(img).sampler
    m.p(img.frame + q["weapon_state"]["node"], 0)
    ok(s.sample(1000)[0]["player"]["gun"]["state"] is None)
    m.p(g.gun + q["weapon_state"]["frame"], 0)
    ok(s.sample(1001)[0]["player"]["gun"]["state"] is None)
    m.p(g.gun + q["weapon_state"]["frame"], img.frame)
    m.p(img.frame + q["weapon_state"]["node"], img.cylinder)
    r, why = s.sample(1002)
    ok("state" not in r["player"]["gun"] and "where a State should be" in s.opt_off["weapon_state"], s.opt_off)
    m.p(img.frame + q["weapon_state"]["node"], img.states["WeaponFiring"])
    ok(s.sample(1003)[0]["player"]["gun"]["state"] == "WeaponFiring" and "weapon_state" not in s.opt_off)
    m.p(g.gun + q["weapon_state"]["frame"], 0x7FFF0000)  # unmapped
    r, why = s.sample(1004)
    ok(why is None and "state" not in r["player"]["gun"] and s.opt_rejected["weapon_state"] == [1, "a read failed"])
    # The timers: an empty list, a list that is none, an entry without a
    # name, data that cannot be read.
    t = q["timers"]
    a_count = g.gun + t["array"] + img.o.arr_count
    m.i(a_count, 0)
    ok(s.sample(1005)[0]["player"]["gun"]["timers"] == [])
    for count in (-1, core.MAX_TIMERS + 1, 10 ** 6):
        m.i(a_count, count)
        r, why = s.sample(1006)
        ok(why is None and "timers" not in r["player"]["gun"] and "not a timer list" in s.opt_rejected["timers"][1])
    m.i(a_count, 2)
    m.i(img.timers + t["size"] + t["name"], 999999)
    r, why = s.sample(1007)
    ok("timers" not in r["player"]["gun"] and s.opt_rejected["timers"][1] == "timer 1 is not a timer")
    m.i(img.timers + t["size"] + t["name"], g.ni["InstantReleaseTimer"])
    m.f(img.timers + t["count"], float("nan"))
    ok("timers" not in s.sample(1008)[0]["player"]["gun"] and s.opt_rejected["timers"][1] == "timer 0 is not a timer")
    m.f(img.timers + t["count"], 0.25)
    m.p(g.gun + t["array"] + img.o.arr_data, 0x7FFF0000)
    ok("timers" not in s.sample(1009)[0]["player"]["gun"] and s.opt_rejected["timers"][1] == "a read failed")
    m.p(g.gun + t["array"] + img.o.arr_data, img.timers)
    r, why = s.sample(1010)
    ok([x["name"] for x in r["player"]["gun"]["timers"]] == ["RefireCheckTimer", "InstantReleaseTimer"]
       and r["player"]["gun"]["timers"][0]["count"] == 0.25 and r["player"]["gun"]["timers"][1]["paused"] is True)
    # A pawn of another class has no bob, a weapon that is no grapple gun no
    # state and no timers; no base, no level.
    m.i(struct.unpack("<I", m.read(g.pawn + img.o.cls, 4))[0] + img.o.name, g.ni["Camera"])
    m.p(g.pawn + img.o.base, 0)
    s = session(img).sampler
    r, why = s.sample(1011)
    ok(why is None and r["player"]["pawn_class"] == "Camera" and "bob" not in r["player"] and r["player"]["base_level"] is None)
    # In a recording: a group that fails its check is named in the header
    # and absent from every record; the rest of the recording is as exact.
    with Dirs() as d:
        img = WinImage(optional=True)
        off, expected, what = img.L.optional_sentinels("bob")[0]
        img.mem.f(img.g.pawn + off, 44.0)
        e = Engine(img)
        vg = VirtualGame(e)
        rec, header, records = run_recording(d, vg, img, "--frames", "30", session=make_session(vg, img, d.exe, optional=True))
        kept = [x for x in OPTIONAL_MEMBERS if x not in ("player.bob", "player.walk_bob")]
        ok(len(records) == 30 and header["optional_fields"] == kept, header["optional_fields"])
        ok(any(n.startswith("optional group bob: check failed, not recorded") and "DoubleJumpEyeHeight" in n for n in header["notes"]))
        check_against_truth(e, records, "bob off", optional=kept)
        ok(all("bob" not in r["player"] and "walk_bob" not in r["player"] for r in records))
        ok("bob" in rec.status()["optional"]["off"] and "off: bob" in win.summary_line(rec.status()))


def test_random_interleavings():
    """Interleavings nobody scripted: per frame a random window, random
    delays between the engine's six events, random keys, and a reader that
    is not scheduled for a while now and then (seeded, so every run is the
    same on every Python). Whatever the timing: each written record is one
    finished frame's state with the next frame's keys, each optional member
    it announces is that frame's value, and every frame without a record is
    in the dropped-frame log with a reason (and no logged frame has one).

    The space bar is left out on purpose: ``bPressedJump`` is read once at
    the tick start and nothing confirms that read (the fake clears the flag
    in the first actor step, the game in the controller's tick), so a
    reader that is late there records the press as not pressed. That is a
    limit of the sampling scheme (docs/TRACE_CAPTURE.md 6.3), not something
    this test may hide by timing: it is kept out of the random keys and the
    scripted tests cover the flag."""
    import random

    def run(seed, optional):
        rnd = random.Random(seed)
        below = lambda n: int(rnd.random() * n)  # 0 .. n-1; random() is the same on every Python
        table = {}

        def spec(i):
            if i not in table:
                f = {"dt": 1.0 / 60.0 + (below(4) if below(2) else 0) * 0.00031}
                f["keys"] = tuple(k for k, p in (("W", 0.4), ("LeftMouseButton", 0.2)) if rnd.random() < p)
                if rnd.random() < 0.25:
                    f["window"] = below(71)  # down to none: torn and late samples
                if rnd.random() < 0.10:
                    f["d_update"] = below(41)
                if rnd.random() < 0.10:
                    f["d_dispatch"] = below(41)
                if rnd.random() < 0.15:
                    f["d_a"] = below(15)
                if rnd.random() < 0.10:
                    f["d_b"], f["d_c"] = below(31), below(31)
                if rnd.random() < 0.05:  # a frame shorter than one burst: missed
                    f.update({"window": below(3), "d_update": 0, "d_dispatch": 0, "d_a": below(2), "d_b": 0, "d_c": 0})
                table[i] = f
            return dict(table[i])

        with Dirs() as d:
            img = WinImage(optional=optional)
            e = Engine(img, spec=spec)
            vg = VirtualGame(e)
            sess = make_session(vg, img, d.exe, optional=True if optional else None)
            every = (0, 0, 53, 97, 211)[below(5)]
            nap = (30, 150, 400)[below(3)]
            polls = [0]
            real_poll = win.Poller.poll

            def poll(self):
                polls[0] += 1
                if every and polls[0] % every == 0:
                    vg.step(nap)  # the reader was not scheduled
                return real_poll(self)

            win.Poller.poll = poll
            try:
                rec, header, records = run_recording(d, vg, img, "--frames", "80", "--seconds", "4", session=sess)
            finally:
                win.Poller.poll = real_poll
            st = rec.stats
            what = "random interleaving %d" % seed
            ok(len(records) == 80 and st.errors == 0, "%s: %r" % (what, st.as_dict()))
            members = header.get("optional_fields") or []
            ok(members == (OPTIONAL_MEMBERS if optional else []), "%s: %r" % (what, members))
            check_against_truth(e, records, what, optional=members)
            check_drops(rec, records, what)
            frames = frames_of(records)
            ok(frames == sorted(set(frames)), "%s: frames out of order" % what)
            if optional:
                ok(not sess.sampler.opt_off and not sess.sampler.opt_rejected, "%s: %r %r" % (what, sess.sampler.opt_off, sess.sampler.opt_rejected))
            return st, rec.gaps

    seen = {"torn": 0, "late": 0, "missed": 0, "unconfirmed": 0, "no_input": 0, "gaps": 0}
    for seed in range(16):
        st, gaps = run(seed, optional=optional_layout() is not None and seed % 4 != 3)
        for k in ("torn", "late", "missed", "unconfirmed", "no_input"):
            seen[k] += getattr(st, k)
        seen["gaps"] += gaps
    # The seeds really exercise every way a frame is lost.
    ok(seen["torn"] >= 20 and seen["late"] >= 3 and seen["missed"] >= 20 and seen["no_input"] >= 20 and seen["gaps"] >= 40, seen)


def test_optional_layout_file():
    """The optional layout: what the core looks up is in the file and the
    file has nothing else; its rows are the rule-derived layout's; a field
    called native_code has its instruction in the recorder layout; a file
    for another layout, or a broken one, is refused."""
    path = next((p for p in win.data_files(LAYOUT_PATH, win.OPTIONAL_FILE) if os.path.isfile(p)), None)
    if path is None:
        return
    with open(path, "r", encoding="utf-8") as fh:
        data = json.load(fh)
    with open(os.path.join(HERE, "asamu_recorder_core.py"), "r", encoding="utf-8") as fh:
        src = fh.read()
    import re

    used = set(re.findall(r'\.opt(?:_bit)?\(\s*"([^"]+)",\s*"([^"]+)"\s*\)', src))
    structs = set(re.findall(r'\.opt_st\(\s*"([^"]+)",\s*"([^"]+)"', src))
    ok(len(used) >= 20 and len(structs) >= 8, (len(used), len(structs)))
    fields = {(f["class"], f["name"]): f for f in data["fields"]}
    sentinels = {(x["class"], x["name"]) for x in data["sentinels"]}
    ok(len(fields) == len(data["fields"]) and used <= set(fields), sorted(used - set(fields)))
    ok(set(fields) - used == sentinels, "a field of the file that nothing reads: %r" % sorted(set(fields) - used - sentinels))
    for name, member in structs:
        st = data["structs"][name]
        ok(member in st.get("members", {}) or member in st.get("bits", {}) or member in st, (name, member))
    groups = {f["group"] for f in data["fields"]}
    ok(groups == set(core.Optional.MEMBERS) - {"base_level"} and {x["group"] for x in data["sentinels"]} <= groups, groups)
    L = core.Layout.load(LAYOUT_PATH)
    ok(data["extends"] == L.id and data["game_build"] == L.game_build and data["pointer_size"] == 4)
    for f in data["fields"]:
        ok(f["evidence"] in ("native_code", "native_layout", "layout_rule") and f["size"] > 0, f)
        ok((f["kind"] == "Bool") == ("bit" in f), f)
        ok((f["class"], f["name"]) not in L._fields, "%s.%s is in both layouts" % (f["class"], f["name"]))
    # Against the rule-derived layout and the recorder layout's instructions,
    # in a repository checkout only: `deploy` does not copy that file, so a
    # copy in the working folder of the game machine is whatever was left
    # there.
    native_path = win.data_files(LAYOUT_PATH, "native_layout_win32.json")[-1]
    if os.path.isfile(native_path):
        with open(native_path, "r", encoding="utf-8") as fh:
            native = json.load(fh)

        def row(container, name):
            for section in ("classes", "structs", "other_fields"):
                for r in native.get(section, {}).get(container, {}).get("fields", []):
                    if r[1] == name:
                        return r
            return None

        for f in data["fields"]:
            ident = "%s.%s" % (f["class"], f["name"])
            if "." in f["name"]:
                path_ = native["member_paths"][ident]
                total = 0
                for container, member, at in path_["steps"]:
                    r = row(container, member)
                    ok(r is not None and r[0] == at, (ident, container, member))
                    total += int(at, 16)
                ok(int(f["offset"], 16) == total == int(path_["offset"], 16) and (r[2], r[3]) == (f["kind"], f["size"]), ident)
            else:
                r = row(f["class"], f["name"])
                ok(r is not None and r[:5] == [f["offset"], f["name"], f["kind"], f["size"], f.get("bit")], (ident, r))
        timer = native["structs"]["Engine.Actor.TimerData"]
        rows = {r[1]: r for r in timer["fields"]}
        st = data["structs"]["TimerData"]
        ok(timer["size"] == st["size"] and int(rows["bLoop"][0], 16) == st["members"]["flags"]
           and all(int(rows[k][0], 16) == st["members"][k] for k in ("FuncName", "Rate", "Count", "TimerObj"))
           and (rows["bLoop"][4], rows["bPaused"][4]) == (st["bits"]["bLoop"], st["bits"]["bPaused"]), st)
        evidence = L.data["native_evidence"]
        shown = {(x["field"], x["offset"]) for x in evidence if "field" in x}
        for f in data["fields"]:
            if f["evidence"] == "native_code":
                ok(("%s.%s" % (f["class"], f["name"]), f["offset"]) in shown, "no instruction for %s.%s" % (f["class"], f["name"]))
        for name in data["structs"]:
            ok(any(x.get("struct") == name for x in evidence), "no instruction for the structure %s" % name)
        node = [x for x in evidence if x.get("struct") == "FStateFrame"]
        ok(len(node) == 1 and "%02x" % data["structs"]["FStateFrame"]["members"]["StateNode"] in node[0]["bytes"])
    # Refused: another layout's file, a wrong schema or pointer size, a field
    # twice, a bool without a bit, a sentinel that is no field, a field the
    # layout has elsewhere; and a file that cannot be read when one is named.
    def refused(change, what):
        bad = json.loads(json.dumps(data))
        change(bad)
        try:
            core.Layout.load(LAYOUT_PATH).extend(bad)
            ok(False, "accepted: %s" % what)
        except core.LayoutError:
            ok(True)

    refused(lambda b: b.update(extends="mac-x86_64-steam-1822049"), "another layout")
    refused(lambda b: b.update(schema="asamu-decomp/recorder-layout/v1"), "the schema of a main layout")
    refused(lambda b: b.update(pointer_size=8), "8-byte pointers")
    refused(lambda b: b["fields"].append(dict(b["fields"][0])), "a field twice")
    refused(lambda b: [f.pop("bit") for f in b["fields"] if f["kind"] == "Bool"], "a bool without a bit")
    refused(lambda b: b["sentinels"].append(dict(b["sentinels"][0], name="NoSuchField")), "a sentinel that is no field")
    refused(lambda b: b["fields"].append({"group": "floor", "class": "Engine.Pawn", "name": "EyeHeight", "offset": "0x2CC",
                                          "kind": "Float", "size": 4, "evidence": "layout_rule"}), "a contradiction")
    try:
        core.Layout.load(os.path.join(HERE, core.DEFAULT_LAYOUT)).extend(data)
        ok(False, "the Mac layout took the Windows optional fields")
    except (core.LayoutError, OSError):
        ok(True)
    with Dirs() as d:
        os.makedirs(d.out)
        broken = os.path.join(d.out, "broken.json")
        for text in ("{not json", json.dumps({"schema": core.OPTIONAL_SCHEMA, "extends": L.id, "pointer_size": 4}),
                     json.dumps(dict(data, fields=[{"class": "A.B"}]))):
            with open(broken, "w", encoding="utf-8") as fh:
                fh.write(text)
            try:
                win.load_optional(core.Layout.load(LAYOUT_PATH), LAYOUT_PATH, broken)
                ok(False, "a broken optional layout was accepted")
            except core.LayoutError:
                ok(True)
        try:
            win.load_optional(core.Layout.load(LAYOUT_PATH), LAYOUT_PATH, os.path.join(d.out, "missing.json"))
            ok(False, "a named optional layout that is not there was ignored")
        except core.LayoutError:
            ok(True)
        # Not named and not found: no error, a line that says so.
        lone = os.path.join(d.out, win.LAYOUT_FILE)
        shutil.copy(LAYOUT_PATH, lone)
        L2 = core.Layout.load(lone)
        ok("no %s found" % win.OPTIONAL_FILE in win.load_optional(L2, lone) and L2.optional_id is None)
        os.makedirs(os.path.join(d.out, "data", "win32"))
        shutil.copy(path, os.path.join(d.out, "data", "win32", win.OPTIONAL_FILE))
        ok("layout %s" % data["id"] in win.load_optional(L2, lone) and L2.optional_id == data["id"], "found next to the layout")


def test_markers():
    """`mark` sets a named marker in the running recording: in its status,
    its stats file and its log, with the frame it was set at. It writes a
    request file into the control folder and nothing else."""
    saved = win.open_game
    with Dirs() as d:
        img = WinImage()
        e = Engine(img)
        vg = VirtualGame(e)
        os.makedirs(d.ctl)
        results = []

        def mark(*name):
            # (Printed into the recording's captured output below: the
            # command's own line and the recorder's.)
            results.append(win.main(["mark", "--out", d.out] + list(name)))

        timers = [threading.Timer(0.5, mark, ("zoom", "hold 1")), threading.Timer(1.1, mark, ("wall\x07 45",)),
                  threading.Timer(1.9, lambda: open(os.path.join(d.ctl, "STOP"), "w").close())]
        out = io.StringIO()
        try:
            for t in timers:
                t.start()
            with contextlib.redirect_stdout(out):
                rec, header, records = run_recording(d, vg, img, "--scenario", "KM1", "--note", "input: keyboard and mouse")
        finally:
            for t in timers:
                t.cancel()
                t.join()
        ok(rec.done == "STOP file found" and len(records) > 50, rec.done)
        ok(results == [0, 0], results)
        ok([m["name"] for m in rec.markers] == ["zoom hold 1", "wall  45"], rec.markers)
        frames = frames_of(records)
        text = out.getvalue()
        for m in rec.markers:
            ok(frames[0] <= m["frame"] <= frames[-1] and m["state"] == "recording" and m["utc"].endswith("Z"), m)
            ok(0 < m["records"] <= len(records) and records[m["records"] - 1]["frame"] <= m["frame"], m)
            line = "asamu-win: marker %r at frame %d (%d records so far" % (m["name"], m["frame"], m["records"])
            ok(line + ")" in text, "the recorder's own log line: %s" % text)
            ok(line + ", file " in text, "the command's answer: %s" % text)
        ok(rec.markers[0]["frame"] < rec.markers[1]["frame"])
        stats = json.load(open(rec.path[: -len(".raw.jsonl")] + ".stats.json", encoding="utf-8"))
        ok([m["name"] for m in stats["markers"]] == ["zoom hold 1", "wall  45"] and stats["markers_omitted"] == 0)
        ok("2 markers, last 'wall  45'" in win.summary_line(stats), win.summary_line(stats))
        code, text = run_cli("status", "--out", d.out)
        ok(code == 0 and "  marker 'zoom hold 1': frame %d, record %d, " % (rec.markers[0]["frame"], rec.markers[0]["records"]) in text
           and text.count("  marker ") == 2, text)
        ok("input: keyboard and mouse" in header["notes"])
        ok(not [n for n in os.listdir(d.ctl) if n.startswith(win.MARK_PREFIX)], "a request file was left behind")
        # The raw file is untouched by markers (they are not records).
        ok(len(records) == rec.records and all("marker" not in json.dumps(r) for r in records[:5]))
        # Without a running recorder nothing is set and no file is written.
        before = sorted(os.listdir(d.ctl))
        code, text = run_cli("mark", "--out", d.out, "late")
        ok(code == 1 and "no recording is running" in text and sorted(os.listdir(d.ctl)) == before, text)
        code, text = run_cli("mark", "--out", os.path.join(d.root, "nowhere"), "x")
        ok(code == 1 and not os.path.exists(os.path.join(d.root, "nowhere")))
        # A recorder that does not take the marker (it is not this one's
        # control folder that is being served): said so, after the wait.
        st = json.load(open(os.path.join(d.ctl, "status.json"), encoding="utf-8"))
        st.update({"state": "recording", "recorder_pid": os.getpid()})
        win.write_json_atomic(os.path.join(d.ctl, "status.json"), st)
        code, text = run_cli("mark", "--out", d.out, "--wait", "0.2", "x" * 200)
        ok(code == 1 and "did not take the marker" in text, text)
        left = [n for n in os.listdir(d.ctl) if n.startswith(win.MARK_PREFIX)]
        ok(len(left) == 1 and json.load(open(os.path.join(d.ctl, left[0]), encoding="utf-8"))["name"] == "x" * win.MAX_MARKER_NAME)
        # A request that is no JSON object is dropped, not fatal; the list is bounded.
        with open(os.path.join(d.ctl, win.MARK_PREFIX + "junk.json"), "w", encoding="utf-8") as fh:
            fh.write("[1, 2")
        sess = make_session(vg, img, d.exe)
        r2 = win.Recording(sess, start_opts(), d.out, d.ctl, clock=vg.clock, sleep=vg.sleep)
        with contextlib.redirect_stdout(io.StringIO()):
            r2.take_marks()
            ok([m["name"] for m in r2.markers] == ["x" * win.MAX_MARKER_NAME] and r2.markers[0]["frame"] is None)
            ok(not [n for n in os.listdir(d.ctl) if n.startswith(win.MARK_PREFIX)])
            r2.markers = [dict(r2.markers[0])] * win.MAX_MARKERS
            win.write_json_atomic(os.path.join(d.ctl, win.MARK_PREFIX + "more.json"), {"name": "one too many"})
            r2.take_marks()
        ok(len(r2.markers) == win.MAX_MARKERS and r2.markers_omitted == 1)
        ok(win.marker_name("  a\tb\n") == "a b" and win.marker_name("") == "marker" and win.marker_name(None) == "None")
    win.open_game = saved
    # `mark` never opens the game: it is file work in the control folder.
    import inspect

    body = inspect.getsource(win.cmd_mark) + inspect.getsource(win.Recording.take_marks)
    for word in ("open_game", "attach(", "OpenProcess", "ReadProcessMemory", "target"):
        ok(word not in body, "mark uses %s" % word)


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
            # The commands load the layout themselves, with the optional
            # fields when their file is there: the game has what they read.
            optional = optional_layout() is not None
            img = WinImage(optional=optional)
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
            if optional:
                # The optional fields are part of the report: their values,
                # and that every group passed its check.
                for want in ("optional fields: layout %s (" % img.L.optional_id, "base level 'AG-Workshop'; floor [",
                             "cylinder {'radius': 21.0, 'half_height': 44.0", "fov default 90.0 locked False lock 0.0",
                             "base eye height 38.0; walk bob [", "gun state 'Active'; timers [{'name': 'RefireCheckTimer'",
                             "optional fields on: %s; not in the layout: none; off: none; left out of this sample: none"
                             % ", ".join(OPTIONAL_MEMBERS)):
                    ok(want in text, "check output lacks %r:\n%s" % (want, text))
                code, text = run_cli("check", "--raw-v1", "--probe-frames", "5", "--probe-seconds", "1")
                ok(code == 0 and "optional fields: none (plain version-1 records)" in text and "gun state" not in text, text)
            ok(not os.path.exists(d.out), "check wrote something")
            # start in the foreground: describes, records, summarises.
            code, text = run_cli("start", "--scenario", "A1", "--frames", "30", "--out", d.out, "--priority", "normal")
            ok(code == 0 and "recording to" in text and "frame limit reached" in text and "30 records" in text, text)
            ok("variable frame lengths" in text and "asamu-win: raw recording" in text)
            raws = [n for n in os.listdir(d.out) if n.endswith(".raw.jsonl")]
            ok(len(raws) == 1 and raws[0].endswith("-A1.raw.jsonl"))
            header, records = core.read_raw(os.path.join(d.out, raws[0]))
            ok(len(records) == 30)
            check_against_truth(e, records, "cli", optional=OPTIONAL_MEMBERS if optional else ())
            ok(header.get("optional_fields") == (OPTIONAL_MEMBERS if optional else ["player.base_level"]), header.get("optional_fields"))
            # v1view: a copy without the optional fields, in a folder of its
            # own, never over a file; --raw-v1 records that way to begin with.
            code, text = run_cli("v1view", os.path.join(d.out, raws[0]))
            view = os.path.join(d.out, "v1", raws[0])
            ok(code == 0 and os.path.isfile(view) and "(30 records)" in text, text)
            h1, r1 = core.read_raw(view)
            ok(set(h1) == set(core.V1_HEADER) and all(set(r["player"]) == set(core.V1_PLAYER) for r in r1))
            ok(r1 == [core.v1_record(r) for r in records] and core.read_raw(os.path.join(d.out, raws[0]))[1] == records)
            code, text = run_cli("v1view", os.path.join(d.out, raws[0]), os.path.join(d.out, "no-such.raw.jsonl"))
            ok(code == 1 and "exists, left as it is" in text and "v1view failed" in text, text)
            shutil.rmtree(os.path.join(d.out, "v1"))
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
            code, text = run_cli("start", "--scenario", "plain", "--frames", "8", "--out", d.out, "--raw-v1")
            ok(code == 0, text)
            plain = [n for n in os.listdir(d.out) if n.endswith("-plain.raw.jsonl")]
            h2, r2 = core.read_raw(os.path.join(d.out, plain[0]))
            ok(set(h2) == set(core.V1_HEADER) and all(set(r["player"]) == set(core.V1_PLAYER) for r in r2) and len(r2) == 8)
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
    source_allow_list(src)
    # The allow-list notices what the list of names above would not: a call
    # it has not judged, a third library, another access right, a suspend
    # through ntdll, a third place that opens the game.
    for extra in ("\ndef x(h):\n    k = kernel32()\n    k.VirtualFreeEx(h, 0, 0, 0x8000)\n",
                  '\nU = ctypes.WinDLL("user32")\n',
                  "\nPROCESS_VM_OPERATION = 0x0008\n",
                  "\ndef x(h):\n    n = ntdll()\n    n.NtSuspendThread(h, None)\n",
                  '\ndef x(k):\n    return getattr(kernel32(), "DebugSetProcessKillOnExit")\n',
                  "\ndef x():\n    return open_game(pid=1)\n"):
        try:
            source_allow_list(src + extra)
        except AssertionError:
            ok(True)
        else:
            ok(False, "the allow-list did not notice %r" % extra)


def source_allow_list(src):
    """The read-only rule once more, as an allow-list (the list of forbidden
    names above only knows the calls it names): which system libraries are
    loaded, which functions are taken from them, which process access rights
    the file names, where the game is opened, and what the shared core may
    touch. A new call of any kind fails here until it is judged and listed."""
    import inspect
    import re

    # Two libraries, loaded by name in one place each; nothing that sends
    # input or messages to a window, starts a shell or signals a process.
    ok(re.findall(r'WinDLL\(\s*"([^"]+)"', src) == ["kernel32", "ntdll"] and src.count("WinDLL") == 3,
       "libraries loaded: %r" % re.findall(r'WinDLL\(\s*"([^"]+)"', src))
    for word in ("windll", "oledll", "CDLL", "cdll", "LoadLibrary", "user32", "advapi32", "dbghelp", "SendInput",
                 "PostMessage", "SendMessage", "PostThreadMessage", "keybd_event", "mouse_event", "NtResumeProcess",
                 "NtWrite", "ZwWrite", "os.system", "os.popen", "shell=True", "taskkill", "signal."):
        ok(word not in src, "asamu_win.py names %s" % word)
    # Every function taken from the two libraries: the process list, one
    # open, reads, queries, closing a handle, and two calls on the recorder's
    # own process (pinned to `me` above).
    allowed = {
        "CreateToolhelp32Snapshot", "Process32FirstW", "Process32NextW", "OpenProcess", "CloseHandle",
        "ReadProcessMemory", "GetExitCodeProcess", "QueryFullProcessImageNameW", "K32EnumProcessModulesEx",
        "K32GetModuleBaseNameW", "GetCurrentProcess", "SetPriorityClass", "SetProcessInformation",
        "NtQueryInformationProcess", "NtQueryObject",
    }
    taken = set(re.findall(r"\b(?:k|n|self\._k|kernel32\(\)|ntdll\(\))\.([A-Z][A-Za-z0-9]+)", src))
    ok(taken <= allowed and {"OpenProcess", "ReadProcessMemory"} <= taken, "library functions not judged: %r" % sorted(taken - allowed))
    # By name through getattr: the two process-list steps and nothing else.
    by_name = re.findall(r"getattr\(\s*(k|n|kernel32\(\)|ntdll\(\)|self\._k)\s*,", src)
    ok(by_name == ["k", "k"] and 'for fn in ("Process32FirstW", "Process32NextW"):' in src, by_name)
    # The access rights: read and the two query rights; every other name
    # that starts like one is an information class or a structure.
    rights = set(re.findall(r"\bPROCESS_[A-Z0-9_]+\b", src))
    ok(rights == {"PROCESS_VM_READ", "PROCESS_QUERY_INFORMATION", "PROCESS_QUERY_LIMITED_INFORMATION",
                  "PROCESS_BASIC_INFORMATION", "PROCESS_BASIC_INFORMATION_CLASS", "PROCESS_WOW64_INFORMATION_CLASS",
                  "PROCESS_INFORMATION_CLASS", "PROCESS_POWER_THROTTLING"}, sorted(rights))
    ok((win.PROCESS_VM_READ, win.PROCESS_QUERY_INFORMATION, win.PROCESS_QUERY_LIMITED_INFORMATION) == (0x10, 0x400, 0x1000))
    # The game is opened in two places (attaching, and the folder guard of a
    # detached start, which closes it again); the commands and helpers
    # recorder 0.2.0 added open nothing and read nothing.
    ok(src.count("open_game(") == 3 and src.count("= open_game(pid=opts.pid, base=opts.base)") == 2)
    for fn in (win.cmd_mark, win.cmd_v1view, win.cmd_status, win.cmd_stop, win.Recording.take_marks, win.load_optional,
               win.optional_report, win.describe_optional, win.marker_name, win.data_files, win.Stats.drop):
        body = inspect.getsource(fn)
        for word in ("open_game", "attach(", "kernel32", "ntdll", "OpenProcess", "ReadProcessMemory", "ctypes", "subprocess"):
            ok(word not in body, "%s uses %s" % (fn.__name__, word))
    # The only process it starts is itself, and the only signal it sends is
    # the "does it exist" probe of a recorder's pid, off Windows only: there
    # os.kill would end the process, so the Windows branch has to have
    # returned before that line on every path.
    ok(src.count("subprocess.Popen(") == 3 and "subprocess.Popen" not in src.replace(inspect.getsource(win.spawn_detached), ""))
    ok('cmd = [sys.executable, "-I", "-B", os.path.abspath(__file__)] + list(argv)' in inspect.getsource(win.spawn_detached))
    body = inspect.getsource(win.pid_alive)
    head, found, _tail = body.partition("    try:\n        os.kill(int(pid), 0)\n")
    ok(found and src.count("os.kill(") == 1 and "    if is_windows():\n" in head and "os.kill" not in head
       and head.endswith("        finally:\n            k.CloseHandle(h)\n"), "pid_alive: %r" % head[-120:])
    # The shared core reads memory through the callable it is handed and
    # never touches a process or the system itself.
    with open(os.path.join(HERE, "asamu_recorder_core.py"), "r", encoding="utf-8") as fh:
        core_src = fh.read()
    for word in ("ctypes", "WinDLL", "subprocess", "Popen", "socket", "os.system", "os.kill", "os.remove", "os.unlink",
                 "shutil", "OpenProcess", "ProcessMemory"):
        ok(word not in core_src, "asamu_recorder_core.py names %s" % word)
    ok(sorted(set(re.findall(r"^(?:import|from) (\w+)", core_src, re.M))) == ["json", "math", "os", "re", "struct", "sys"])


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
    test_optional_fields,
    test_optional_checks,
    test_random_interleavings,
    test_optional_layout_file,
    test_markers,
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
    L = optional_layout() or core.Layout.load(LAYOUT_PATH)
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
    img = WinImage(base=base, memory=mem, layout=L, optional=L.optional_id is not None)

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
                # A marker, set from this process, taken by the detached one.
                code, text = run_cli("mark", "--out", out, "live", "marker")
                print(text.rstrip())
                ok(code == 0 and "marker 'live marker' at frame " in text, text)
                code, text = run_cli("start", "--frames", "5", "--out", out, *common)
                ok(code == 1 and "a recording is running" in text, text)
                code, text = run_cli("stop", "--out", out)
                print(text.rstrip())
                ok(code == 0 and "finished (STOP file found)" in text, text)
                raws = [n for n in os.listdir(out) if n.endswith(".raw.jsonl")]
                ok(len(raws) == 1)
                h, recs = core.read_raw(os.path.join(out, raws[0]))
                ok(len(recs) > 40, len(recs))
                st = json.load(open(os.path.join(out, raws[0][: -len(".raw.jsonl")] + ".stats.json"), encoding="utf-8"))
                ok([m["name"] for m in st["markers"]] == ["live marker"]
                   and recs[0]["frame"] <= st["markers"][0]["frame"] <= recs[-1]["frame"], st["markers"])
                with open(os.path.join(out, "ctl", "recorder.log"), "r", encoding="utf-8", errors="replace") as fh:
                    ok("marker 'live marker' at frame %d" % st["markers"][0]["frame"] in fh.read(), "the marker is in the log")
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
            # The optional fields (when their layout file is deployed here):
            # read in the same burst, through the real API, each one exact.
            optional = header.get("optional_fields") or []
            staged = all("optional" in t for t in truth.truth.values())
            check_against_truth(truth, records, "live " + mode, optional=optional if staged else ())
            stats = json.load(open(os.path.join(out, raws[0][: -len(".raw.jsonl")] + ".stats.json"), encoding="utf-8"))
            results[mode] = (len(records), stats)
            if mode == "capped":
                s = stats["stats"]
                ok(len(records) >= frames * 0.9, "only %d records" % len(records))
                ok(s["torn"] + s["late"] + s["missed"] <= max(3, frames // 50), s)
                ok(header["recorder"] == win.RECORDER and core.convert_raw(header, records))
                ok(optional == (OPTIONAL_MEMBERS if staged else ["player.base_level"]), optional)
                ok(not stats["optional"]["off"] and not stats["optional"]["left_out"], stats["optional"])
                missing = set(range(records[0]["frame"], records[-1]["frame"] + 1)) - set(frames_of(records))
                logged = set()
                for x in s["drops"]:
                    if x["reason"] != "no-input":
                        logged |= set(range(x["frame"], x["last"] + 1))
                ok(missing <= logged, "frames %r are missing without an entry in the drop log" % sorted(missing - logged))
        finally:
            if child.poll() is None:
                child.kill()
            child.stdout.close()
            shutil.rmtree(tmp, ignore_errors=True)
    for mode, (n, stats) in results.items():
        s = stats["stats"]
        print("live %-8s: %d records, %d gaps; torn %d, unconfirmed %d, late %d, missed %d; fps %s; burst %s us "
              "(%s reads); window %s ms; tick %s ms; recorder CPU %s%%; optional fields %d"
              % (mode, n, stats["gaps"], s["torn"], s["unconfirmed"], s["late"], s["missed"], s["fps"],
                 (s["capture_us"] or {}).get("mean"), (s["reads_per_burst"] or {}).get("mean"),
                 (s["window_ms"] or {}).get("mean"),
                 (s["tick_ms"] or {}).get("mean"), stats["recorder_cpu_percent"],
                 len((stats.get("optional") or {}).get("fields") or [])))
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
        if t in (test_steady_recording, test_optional_fields):
            t(keep)
        else:
            t()
        if "-v" in argv:
            print("%-28s %d checks" % (t.__name__, CHECKS[0] - before))
    print("win glue test ok (%d checks, %d-bit fake image at 0x%08X)" % (CHECKS[0], 32, BASE))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
