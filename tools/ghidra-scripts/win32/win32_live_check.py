#!/usr/bin/env python3
"""Read-only live check of docs/reverse-engineering/data/win32/*.json.

Run on the Windows machine while the original game (ASAMU-Win32-Shipping.exe)
is running. It opens the process with query + read rights only, reads memory
with ReadProcessMemory and prints which of the statically derived addresses
hold what they should. It never writes to the process, injects nothing and
attaches no debugger.

    python win32_live_check.py                 (JSON files next to the script or in data\\win32)
    python win32_live_check.py --data DIR --seconds 2
    python win32_live_check.py --selftest      (no game needed; any OS)

Stock CPython 3.9+ (ctypes only). The module imports on macOS and Linux so the
self-test can run there; the Windows API is touched only when a process is
opened.

Exit code: 0 all checks passed, 1 a check failed, 2 the game is not running.
"""

import argparse
import ctypes
import json
import os
import struct
import sys
import time

EXE_NAME = "ASAMU-Win32-Shipping.exe"
DATA_FILES = ("image.json", "globals.json", "class_sizes.json", "vtables.json", "native_evidence.json")
MAX_NAME_CHARS = 1024


class ReadError(Exception):
    pass


# ------------------------------------------------------------ data files


def find_data_dir(explicit=None):
    here = os.path.dirname(os.path.abspath(__file__))
    candidates = [explicit] if explicit else []
    candidates += [
        here,
        os.path.join(here, "data", "win32"),
        os.path.join(here, "..", "..", "..", "docs", "reverse-engineering", "data", "win32"),
    ]
    for c in candidates:
        if c and all(os.path.isfile(os.path.join(c, f)) for f in DATA_FILES):
            return c
    raise SystemExit("data files not found (%s); pass --data DIR" % ", ".join(DATA_FILES))


def load_data(directory):
    out = {}
    for f in DATA_FILES:
        with open(os.path.join(directory, f), "r", encoding="utf-8") as fh:
            out[f[:-5]] = json.load(fh)
    return out


class Model:
    """The few numbers the checks need, taken from the JSON files."""

    def __init__(self, data):
        self.image = data["image"]
        self.globals = {g["name"]: g for g in data["globals"]["globals"]}
        cols = data["class_sizes"]["columns"]
        self.classes = [dict(zip(cols, row)) for row in data["class_sizes"]["classes"]]
        self.by_cpp = {c["cpp_name"]: c for c in self.classes}
        self.script_classes = data["vtables"]["script_classes"]
        shows = {}
        for e in data["native_evidence"]["evidence"]:
            shows.update(e["shows"])
        self.outer = shows["Core.Object.Outer"]
        self.name = self.outer + 4  # Outer, Name (8 bytes), Class, ObjectArchetype
        self.cls = self.outer + 12
        self.game_players = shows["Engine.Engine.GamePlayers"]
        self.player_actor = shows["Engine.Player.Actor"]
        self.entry_index = shows["FNameEntry.index"]
        self.entry_chars = shows["FNameEntry.chars"]
        self.wide_size = shows["FNameEntry.wide_char_size"]
        self.name_log = shows["hardcoded_name_index_Log"]
        self.time = {k.split(".")[-1]: v for k, v in shows.items() if k.startswith("Engine.WorldInfo.")}
        self.uclass_size = data["native_evidence"]["struct_sizes"]["UClass"]

    def rva(self, name):
        return self.globals[name]["rva"]


# -------------------------------------------------------------- memory


class Memory:
    """read(addr, size) -> bytes over a process handle or a fake image."""

    def __init__(self, read, base):
        self._read = read
        self.base = base

    def raw(self, addr, size):
        data = self._read(addr, size)
        if data is None or len(data) != size:
            raise ReadError("cannot read %d bytes at %#x" % (size, addr))
        return data

    def u32(self, addr):
        return struct.unpack("<I", self.raw(addr, 4))[0]

    def i32(self, addr):
        return struct.unpack("<i", self.raw(addr, 4))[0]

    def u64(self, addr):
        return struct.unpack("<Q", self.raw(addr, 8))[0]

    def f32(self, addr):
        return struct.unpack("<f", self.raw(addr, 4))[0]

    def f64(self, addr):
        return struct.unpack("<d", self.raw(addr, 8))[0]

    def try_raw(self, addr, size):
        try:
            return self.raw(addr, size)
        except ReadError:
            return None


class Names:
    def __init__(self, mem, model):
        self.mem = mem
        self.m = model
        self.addr = mem.base + model.rva("FName::Names")
        self.cache = {}

    def header(self):
        data, count, cap = struct.unpack("<Iii", self.mem.raw(self.addr, 12))
        return data, count, cap

    def get(self, index):
        if index in self.cache:
            return self.cache[index]
        data, count, _cap = self.header()
        if not data or index < 0 or index >= count:
            return None
        entry = self.mem.u32(data + 4 * index)
        if not entry:
            return None
        wide = bool(self.mem.u32(entry + self.m.entry_index) & 1)
        unit = self.m.wide_size if wide else 1
        out = []
        pos = entry + self.m.entry_chars
        while len(out) < MAX_NAME_CHARS:
            buf = self.mem.try_raw(pos, 64 * unit) or self.mem.raw(pos, unit)
            done = False
            for i in range(0, len(buf), unit):
                c = int.from_bytes(buf[i : i + unit], "little")
                if c == 0:
                    done = True
                    break
                out.append(chr(c) if c < 0xD800 or 0xE000 <= c < 0x110000 else "�")
            if done:
                break
            pos += len(buf)
        text = "".join(out)
        self.cache[index] = text
        return text

    def of_object(self, obj):
        index, number = struct.unpack("<ii", self.mem.raw(obj + self.m.name, 8))
        base = self.get(index)
        if base is None:
            return None
        return "%s_%d" % (base, number - 1) if number > 0 else base


# -------------------------------------------------------------- checks


class Report:
    def __init__(self, out=sys.stdout):
        self.out = out
        self.passed = 0
        self.failed = 0

    def check(self, ok, text):
        if ok:
            self.passed += 1
        else:
            self.failed += 1
        self.out.write("%s %s\n" % ("ok  " if ok else "FAIL", text))
        return ok

    def info(self, text):
        self.out.write("     %s\n" % text)


def best_offset(tally, total):
    if not tally:
        return None, 0
    off, n = max(tally.items(), key=lambda kv: (kv[1], -kv[0]))
    return off, n


def run_checks(mem, model, report, seconds=1.0, sleep=time.sleep):
    m = model
    base = mem.base
    names = Names(mem, m)

    # image
    hdr = mem.raw(base, 0x400)
    e_lfanew = struct.unpack_from("<I", hdr, 0x3C)[0]
    stamp = struct.unpack_from("<I", hdr, e_lfanew + 8)[0] if hdr[:2] == b"MZ" and e_lfanew < 0x3F0 else None
    report.check(stamp == m.image["time_date_stamp"], "module header at the base has the analysed build's time stamp")

    # names
    data, count, cap = names.header()
    report.check(data != 0 and 1000 < count <= cap < 5000000, "FName::Names is a plausible array (%d names)" % count)
    report.check(names.get(0) == "None", "FName::Names[0] is 'None' (got %r)" % names.get(0))
    report.check(names.get(m.name_log) == "Log", "FName::Names[%d] is 'Log' (got %r)" % (m.name_log, names.get(m.name_log)))

    # timing globals
    f0 = mem.u64(base + m.rva("GFrameCounter"))
    fixed = mem.f64(base + m.rva("GFixedDeltaTime"))
    delta = mem.f64(base + m.rva("GDeltaTime"))
    bench = mem.u32(base + m.rva("GIsBenchmarking"))
    usefixed = mem.u32(base + m.rva("GUseFixedTimeStep"))
    report.check(0.0 < fixed <= 1.0, "GFixedDeltaTime is a plausible step (%.9g)" % fixed)
    report.check(0.0 <= delta <= 1.0, "GDeltaTime is a plausible frame time (%.9g)" % delta)
    report.check(bench in (0, 1) and usefixed in (0, 1), "GIsBenchmarking=%d GUseFixedTimeStep=%d are booleans" % (bench, usefixed))
    if bench:
        report.check(abs(delta - fixed) < 1e-12, "benchmarking: GDeltaTime equals GFixedDeltaTime")

    # classes: PrivateStaticClass -> UClass object named like the class
    cls_vt = base + m.by_cpp["UClass"]["vtable_rva"]
    registered = named = 0
    size_tally = {}
    cdo_tally = {}
    size_hits = {}
    cdo_hits = {}
    for c in m.classes:
        ptr = mem.u32(base + c["private_static_class_rva"])
        if not ptr:
            continue
        registered += 1
        blob = mem.try_raw(ptr, m.uclass_size)
        if blob is None or struct.unpack_from("<I", blob, 0)[0] != cls_vt:
            continue
        if names.of_object(ptr) != c["name"]:
            continue
        named += 1
        want_vt = base + c["vtable_rva"]
        for off in range(m.cls + 8, m.uclass_size - 3, 4):
            v = struct.unpack_from("<I", blob, off)[0]
            if v == c["size"]:
                size_tally[off] = size_tally.get(off, 0) + 1
                size_hits.setdefault(c["cpp_name"], set()).add(off)
            elif v > 0x10000:
                head = mem.try_raw(v, m.cls + 4)
                if head and struct.unpack_from("<I", head, 0)[0] == want_vt and struct.unpack_from("<I", head, m.cls)[0] == ptr:
                    cdo_tally[off] = cdo_tally.get(off, 0) + 1
                    cdo_hits.setdefault(c["cpp_name"], set()).add(off)
        size_hits.setdefault(c["cpp_name"], set())
        cdo_hits.setdefault(c["cpp_name"], set())
    report.check(registered > 1000, "%d of %d PrivateStaticClass pointers are set" % (registered, len(m.classes)))
    report.check(registered and named == registered, "%d of them point at a UClass whose name is the class name" % named)
    for what, tally, hits in (
        ("sizeof equals the class object's size field", size_tally, size_hits),
        ("the default object carries the listed vtable", cdo_tally, cdo_hits),
    ):
        off, n = best_offset(tally, named)
        report.check(named and n >= named * 0.98, "%s for %d of %d classes (UClass+0x%X)" % (what, n, named, off or 0))
        missing = sorted(k for k, v in hits.items() if off not in v)
        if missing:
            report.info("not matching at that offset: %s%s" % (", ".join(missing[:12]), " ..." if len(missing) > 12 else ""))

    # engine and world
    engine = mem.u32(base + m.rva("GEngine"))
    report.check(engine != 0, "GEngine is set")
    if engine:
        ok = mem.u32(engine) == base + m.by_cpp["UGameEngine"]["vtable_rva"]
        report.check(ok, "GEngine carries UGameEngine's vtable (class %r)" % names.of_object(mem.u32(engine + m.cls)))
    world = mem.u32(base + m.rva("GWorld"))
    if world:
        ok = mem.u32(world) == base + m.by_cpp["UWorld"]["vtable_rva"]
        report.check(ok, "GWorld carries UWorld's vtable (outermost %r)" % outermost(mem, m, names, world))
    else:
        report.info("GWorld is null (no world loaded yet)")

    # objects
    odata, ocount, ocap = struct.unpack("<Iii", mem.raw(base + m.rva("UObject::GObjObjects"), 12))
    report.check(odata != 0 and 1000 < ocount <= ocap, "UObject::GObjObjects is a plausible array (%d slots)" % ocount)
    world_infos = []
    if odata and 0 < ocount < 5000000:
        table = mem.raw(odata, 4 * ocount)
        wi_vt = base + m.by_cpp["AWorldInfo"]["vtable_rva"]
        index_ok = sampled = 0
        for i in range(0, ocount):
            obj = struct.unpack_from("<I", table, 4 * i)[0]
            if not obj:
                continue
            if sampled < 2000:
                head = mem.try_raw(obj, 0x24)
                if head:
                    sampled += 1
                    index_ok += struct.unpack_from("<i", head, 0x20)[0] == i
            vt = mem.try_raw(obj, 4)
            if vt and struct.unpack("<I", vt)[0] == wi_vt:
                world_infos.append(obj)
        report.check(sampled and index_ok == sampled, "object.Index (+0x20) equals its slot for %d sampled objects" % index_ok)

    # player chain
    controller = None
    if engine:
        pdata, pcount = struct.unpack("<Ii", mem.raw(engine + m.game_players, 8))
        if pdata and 0 < pcount <= 8:
            player = mem.u32(pdata)
            controller = mem.u32(player + m.player_actor) if player else 0
        report.info("Engine.GamePlayers: %d player(s)" % pcount)
    if controller:
        want = {s["script_class"]: s for s in m.script_classes}
        pc_vt = base + want["asamu.ASAMUPlayerController"]["vtable_rva"]
        cname = names.of_object(mem.u32(controller + m.cls))
        report.check(mem.u32(controller) == pc_vt, "GamePlayers[0].Actor carries AUDKPlayerController's vtable (class %r)" % cname)
        pawn_vt = base + want["asamu.ASAMUPawn"]["vtable_rva"]
        blob = mem.raw(controller, m.by_cpp["AUDKPlayerController"]["size"])
        hits = []
        for off in range(m.cls + 4, len(blob) - 3, 4):
            v = struct.unpack_from("<I", blob, off)[0]
            if v > 0x10000:
                head = mem.try_raw(v, 4)
                if head and struct.unpack("<I", head)[0] == pawn_vt:
                    hits.append((off, names.of_object(mem.u32(v + m.cls))))
        report.info("controller fields pointing at an AUDKPawn-vtable object: %s" % (", ".join("+0x%X (%s)" % h for h in hits) or "none"))
    else:
        report.info("no local player controller yet (main menu or loading): player checks skipped")

    # world time and frame counter over an interval
    live = [w for w in world_infos if names.of_object(w) and not (names.of_object(w) or "").startswith("Default__")]
    if live:
        wi = live[0]
        t = m.time
        rts0 = mem.f32(wi + t["RealTimeSeconds"])
        report.info(
            "WorldInfo %r: TimeSeconds %.3f RealTimeSeconds %.3f DeltaSeconds %.6f TimeDilation %.3f"
            % (outermost(mem, m, names, wi), mem.f32(wi + t["TimeSeconds"]), rts0, mem.f32(wi + t["DeltaSeconds"]), mem.f32(wi + t["TimeDilation"]))
        )
    sleep(seconds)
    f1 = mem.u64(base + m.rva("GFrameCounter"))
    report.check(f1 > f0, "GFrameCounter advanced by %d in %.1f s" % (f1 - f0, seconds))
    if live:
        rts1 = mem.f32(live[0] + m.time["RealTimeSeconds"])
        report.check(rts1 > rts0, "WorldInfo.RealTimeSeconds advanced by %.3f s" % (rts1 - rts0))
        dil = mem.f32(live[0] + m.time["TimeDilation"])
        report.check(0.0 < dil <= 10.0, "WorldInfo.TimeDilation is plausible (%.3f)" % dil)
    return report.failed == 0


def outermost(mem, model, names, obj):
    cur = obj
    for _ in range(16):
        nxt = mem.u32(cur + model.outer)
        if not nxt:
            break
        cur = nxt
    return names.of_object(cur)


# ------------------------------------------------------------- windows

TH32CS_SNAPPROCESS = 0x00000002
TH32CS_SNAPMODULE = 0x00000008
TH32CS_SNAPMODULE32 = 0x00000010
PROCESS_VM_READ = 0x0010
PROCESS_QUERY_INFORMATION = 0x0400
INVALID_HANDLE = ctypes.c_void_p(-1).value


class PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", ctypes.c_uint32),
        ("cntUsage", ctypes.c_uint32),
        ("th32ProcessID", ctypes.c_uint32),
        ("th32DefaultHeapID", ctypes.c_size_t),
        ("th32ModuleID", ctypes.c_uint32),
        ("cntThreads", ctypes.c_uint32),
        ("th32ParentProcessID", ctypes.c_uint32),
        ("pcPriClassBase", ctypes.c_int32),
        ("dwFlags", ctypes.c_uint32),
        ("szExeFile", ctypes.c_wchar * 260),
    ]


class MODULEENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", ctypes.c_uint32),
        ("th32ModuleID", ctypes.c_uint32),
        ("th32ProcessID", ctypes.c_uint32),
        ("GlblcntUsage", ctypes.c_uint32),
        ("ProccntUsage", ctypes.c_uint32),
        ("modBaseAddr", ctypes.c_void_p),
        ("modBaseSize", ctypes.c_uint32),
        ("hModule", ctypes.c_void_p),
        ("szModule", ctypes.c_wchar * 256),
        ("szExePath", ctypes.c_wchar * 260),
    ]


def _kernel32():
    if not hasattr(ctypes, "windll"):
        raise SystemExit("the live check needs Windows; use --selftest elsewhere")
    k = ctypes.windll.kernel32
    k.CreateToolhelp32Snapshot.restype = ctypes.c_void_p
    k.CreateToolhelp32Snapshot.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    k.OpenProcess.restype = ctypes.c_void_p
    k.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
    k.CloseHandle.argtypes = [ctypes.c_void_p]
    k.ReadProcessMemory.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
    k.ReadProcessMemory.restype = ctypes.c_int
    for fn in ("Process32FirstW", "Process32NextW", "Module32FirstW", "Module32NextW"):
        getattr(k, fn).argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        getattr(k, fn).restype = ctypes.c_int
    return k


def find_pid(k, name=EXE_NAME):
    snap = k.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if not snap or snap == INVALID_HANDLE:
        return None
    try:
        e = PROCESSENTRY32W()
        e.dwSize = ctypes.sizeof(e)
        ok = k.Process32FirstW(snap, ctypes.byref(e))
        while ok:
            if e.szExeFile.lower() == name.lower():
                return e.th32ProcessID
            ok = k.Process32NextW(snap, ctypes.byref(e))
    finally:
        k.CloseHandle(snap)
    return None


def module_base(k, pid, name=EXE_NAME):
    """Load address of the game's main module (32-bit modules of a WOW64 process included)."""
    for _attempt in range(10):
        snap = k.CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)
        if snap and snap != INVALID_HANDLE:
            break
        time.sleep(0.1)  # ERROR_BAD_LENGTH while the module list changes: retry
    else:
        return None
    try:
        e = MODULEENTRY32W()
        e.dwSize = ctypes.sizeof(e)
        ok = k.Module32FirstW(snap, ctypes.byref(e))
        while ok:
            if e.szModule.lower() == name.lower():
                return e.modBaseAddr
            ok = k.Module32NextW(snap, ctypes.byref(e))
    finally:
        k.CloseHandle(snap)
    return None


def open_game(pid=None):
    """(Memory, close) for the running game, or (None, None) when it is not running."""
    k = _kernel32()
    pid = pid or find_pid(k)
    if not pid:
        return None, None
    handle = k.OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid)
    if not handle:
        raise SystemExit("OpenProcess failed for pid %d (error %d)" % (pid, ctypes.GetLastError()))
    base = module_base(k, pid)
    if not base:
        k.CloseHandle(handle)
        raise SystemExit("module %s not found in pid %d" % (EXE_NAME, pid))

    def read(addr, size):
        buf = ctypes.create_string_buffer(size)
        got = ctypes.c_size_t(0)
        if not k.ReadProcessMemory(handle, ctypes.c_void_p(addr), buf, size, ctypes.byref(got)):
            return None
        return buf.raw[: got.value]

    return Memory(read, base), lambda: k.CloseHandle(handle)


# ------------------------------------------------------------- selftest


class FakeGame:
    """A tiny memory image laid out from the JSON files (no game data)."""

    def __init__(self, model, base=0x00D50000):
        self.m = model
        self.base = base
        self.mem = {}
        self.next = 0x10000000
        self.frame_reads = 0
        m = model
        hdr = bytearray(0x400)
        hdr[:2] = b"MZ"
        struct.pack_into("<I", hdr, 0x3C, 0x100)
        hdr[0x100:0x104] = b"PE\0\0"
        struct.pack_into("<I", hdr, 0x108, m.image["time_date_stamp"])
        self.write(base, bytes(hdr))
        self.names = {}
        self.name_list = []
        for i in range(m.name_log + 1):
            self.name_list.append(None)
        self.name_list[0] = "None"
        self.name_list[m.name_log] = "Log"
        some = [c for c in m.classes if c["cpp_name"] in ("UObject", "UClass", "UWorld", "UGameEngine", "AWorldInfo", "AUDKPlayerController", "AUDKPawn", "ULocalPlayer", "UPackage")]
        filler = [c for c in m.classes if c not in some][:1200]
        self.objects = []
        self.class_ptr = {}
        for c in m.classes:
            self.p32(base + c["private_static_class_rva"], 0)
        for c in some + filler:
            self.class_ptr[c["cpp_name"]] = self.alloc(m.uclass_size)
        cls_class = self.class_ptr["UClass"]
        for c in some + filler:
            ptr = self.class_ptr[c["cpp_name"]]
            self.init_object(ptr, base + m.by_cpp["UClass"]["vtable_rva"], cls_class, c["name"])
            self.p32(ptr + 0x60, c["size"])
            cdo = self.new_object(c["cpp_name"], "Default__" + c["name"])
            self.p32(ptr + 0x64, cdo)
            self.p32(base + c["private_static_class_rva"], ptr)
        # world, engine, player
        package = self.new_object("UPackage", "AG-Test")
        world = self.new_object("UWorld", "TheWorld", package)
        info = self.new_object("AWorldInfo", "WorldInfo_0", package)
        self.info = info
        self.f32(info + m.time["TimeDilation"], 1.0)
        self.f32(info + m.time["RealTimeSeconds"], 12.5)
        self.f32(info + m.time["TimeSeconds"], 12.5)
        self.f32(info + m.time["DeltaSeconds"], 1 / 60)
        engine = self.new_object("UGameEngine", "GameEngine_0")
        player = self.new_object("ULocalPlayer", "LocalPlayer_0")
        pc = self.new_object("AUDKPlayerController", "ASAMUPlayerController_0", package)
        pawn = self.new_object("AUDKPawn", "ASAMUPawn_0", package)
        self.p32(pc + 0x1F8, pawn)
        self.p32(player + m.player_actor, pc)
        arr = self.alloc(4)
        self.p32(arr, player)
        self.write(engine + m.game_players, struct.pack("<Iii", arr, 1, 1))
        self.p32(base + m.rva("GEngine"), engine)
        self.p32(base + m.rva("GWorld"), world)
        self.write(base + m.rva("GFixedDeltaTime"), struct.pack("<d", 1 / 60))
        self.write(base + m.rva("GDeltaTime"), struct.pack("<d", 1 / 60))
        self.p32(base + m.rva("GIsBenchmarking"), 1)
        self.p32(base + m.rva("GUseFixedTimeStep"), 0)
        self.write(base + m.rva("GFrameCounter"), struct.pack("<Q", 1000))
        table = self.alloc(4 * len(self.objects))
        for i, o in enumerate(self.objects):
            self.p32(table + 4 * i, o)
            self.p32(o + 0x20, i)
        self.write(base + m.rva("UObject::GObjObjects"), struct.pack("<Iii", table, len(self.objects), len(self.objects)))
        # names table
        entries = self.alloc(4 * len(self.name_list))
        for i, n in enumerate(self.name_list):
            if n is None:
                continue
            wide = i % 2 == 1
            raw = n.encode("utf-16-le") + b"\0\0" if wide else n.encode("ascii") + b"\0"
            e = self.alloc(m.entry_chars + len(raw))
            self.p32(e + m.entry_index, (i << 1) | (1 if wide else 0))
            self.write(e + m.entry_chars, raw)
            self.p32(entries + 4 * i, e)
        self.write(base + m.rva("FName::Names"), struct.pack("<Iii", entries, len(self.name_list), len(self.name_list) + 8))

    def alloc(self, size):
        a = self.next
        self.next += (size + 15) & ~15
        self.write(a, bytes(size))
        return a

    def write(self, addr, data):
        for i, b in enumerate(data):
            self.mem[addr + i] = b

    def p32(self, addr, v):
        self.write(addr, struct.pack("<I", v))

    def f32(self, addr, v):
        self.write(addr, struct.pack("<f", v))

    def name_index(self, text):
        if text not in self.names:
            self.names[text] = len(self.name_list)
            self.name_list.append(text)
        return self.names[text]

    def init_object(self, ptr, vtable, cls, name, outer=0):
        self.p32(ptr, vtable)
        self.p32(ptr + self.m.outer, outer)
        self.write(ptr + self.m.name, struct.pack("<ii", self.name_index(name), 0))
        self.p32(ptr + self.m.cls, cls)
        self.objects.append(ptr)

    def new_object(self, cpp, name, outer=0):
        c = self.m.by_cpp[cpp]
        ptr = self.alloc(c["size"])
        self.init_object(ptr, self.base + c["vtable_rva"], self.class_ptr[cpp], name, outer)
        return ptr

    def tick(self, _seconds):
        m = self.m
        addr = self.base + m.rva("GFrameCounter")
        v = struct.unpack("<Q", self.read(addr, 8))[0]
        self.write(addr, struct.pack("<Q", v + 60))
        self.f32(self.info + m.time["RealTimeSeconds"], 13.5)

    def read(self, addr, size):
        try:
            return bytes(self.mem[addr + i] for i in range(size))
        except KeyError:
            return None


class _Sink:
    def __init__(self):
        self.lines = []

    def write(self, text):
        self.lines.append(text)


def selftest(model):
    game = FakeGame(model)
    sink = _Sink()
    rep = Report(sink)
    ok = run_checks(Memory(game.read, game.base), model, rep, seconds=1.0, sleep=game.tick)
    if not ok or rep.passed < 15:
        sys.stdout.write("".join(sink.lines))
        raise SystemExit("selftest: the checks fail on the fake image")
    # a wrong address must be noticed
    bad = FakeGame(model)
    bad.write(bad.base + model.rva("GFrameCounter"), struct.pack("<Q", 5))
    rep2 = Report(_Sink())
    run_checks(Memory(bad.read, bad.base), model, rep2, seconds=1.0, sleep=lambda s: None)
    if rep2.failed != 2:  # the frame counter and the world clock stand still
        raise SystemExit("selftest: a stalled counter was not reported (%d failures)" % rep2.failed)
    bad = FakeGame(model)
    bad.p32(bad.base + model.rva("GEngine"), bad.class_ptr["UWorld"])
    rep3 = Report(_Sink())
    try:
        run_checks(Memory(bad.read, bad.base), model, rep3, seconds=1.0, sleep=bad.tick)
    except ReadError:
        rep3.failed += 1
    if rep3.failed == 0:
        raise SystemExit("selftest: a wrong GEngine was not reported")
    print("selftest ok (%d checks on the fake image)" % rep.passed)
    return 0


# ---------------------------------------------------------------- main


def main(argv):
    ap = argparse.ArgumentParser(description="Read-only live check of the Win32 address tables")
    ap.add_argument("--data", help="directory with image.json, globals.json, class_sizes.json, vtables.json, native_evidence.json")
    ap.add_argument("--pid", type=int, help="process id (default: find %s)" % EXE_NAME)
    ap.add_argument("--seconds", type=float, default=1.0, help="interval for the frame counter check")
    ap.add_argument("--selftest", action="store_true", help="run the checks on a fake memory image")
    args = ap.parse_args(argv)
    model = Model(load_data(find_data_dir(args.data)))
    if args.selftest:
        return selftest(model)
    mem, close = open_game(args.pid)
    if mem is None:
        print("%s is not running" % EXE_NAME)
        return 2
    try:
        print("module base 0x%08X (preferred %s)" % (mem.base, model.image["image_base"]))
        rep = Report()
        try:
            run_checks(mem, model, rep, seconds=args.seconds)
        except ReadError as e:
            rep.check(False, "memory read failed: %s" % e)
        print("%d passed, %d failed" % (rep.passed, rep.failed))
        return 1 if rep.failed else 0
    finally:
        close()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
