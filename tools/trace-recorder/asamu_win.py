#!/usr/bin/env python3
"""Windows front end of the ASAMU trace recorder (original Win32 build).

Records the player's state once per engine frame from the running game
without a debugger: it opens the process with query + read rights only and
polls it with ``ReadProcessMemory``. It never writes to the process, never
suspends a thread, injects nothing and installs nothing. That one handle is
the only handle to the game: the module base and the executable's path are
asked through it as well, its granted access is checked after opening, and
no system snapshot of the game's modules or heaps is taken. Stock CPython
3.8+ with ``ctypes``; the module also imports on macOS and Linux (the
Windows API is touched only when a process is opened), so its logic is
tested there with a fake memory image (``test_win_glue.py``).

    python asamu_win.py check
    python asamu_win.py start --scenario T1 --frames 600 [--seconds 20] [--detach]
    python asamu_win.py mark NAME              (a named marker in the running recording's status and log)
    python asamu_win.py status
    python asamu_win.py stop
    python asamu_win.py v1view FILE.raw.jsonl  (any OS: a copy without the optional fields)

Files: ``layout_win_x86.json`` and ``asamu_recorder_core.py`` next to this
script, and optionally ``data/win32/recorder_optional_win32.json`` (the
optional fields, docs/TRACE_CAPTURE.md 6.10). Output:
``<out>/<UTC time>-<scenario>.raw.jsonl`` (format ``asamu-trace-raw`` v1, the
same as the Mac recorder writes; records carry the optional fields as extra
members unless ``--raw-v1`` is given, and ``v1view`` writes a copy without
them for a reader of plain version-1 files) plus a ``.stats.json`` with the
sampling counters, every dropped frame with its reason, and the markers.
Control folder (``--ctl``, default ``<out>/ctl``): ``status.json`` (rewritten
about once a second), ``STOP`` (create it to end a recording), ``MARK-*``
(marker requests of ``mark``), ``recorder.log``.

How a frame is sampled
----------------------
The frame order of this build (docs/reverse-engineering/WINDOWS_BINARY.md 6,
docs/TRACE_CAPTURE.md 6.2)::

    FEngineLoop::Tick
      appUpdateTimeAndHandleMaxTickRate   waits for the frame limit, stores GDeltaTime
      UGameEngine::Tick
        Client->Tick                      applies the deferred key/button messages
        UWorld::Tick                      first stores WorldInfo.RealTimeSeconds,
                                          then ticks every actor
      GFrameCounter += 1
      ... end-of-frame work, message pump (key messages are only queued) ...

Every actor tick lies between the ``RealTimeSeconds`` store and the counter
increment, so from the increment to the next store ("the window") the world
holds the finished frame's state. Per frame the recorder

1. spins on ``GFrameCounter``; when it changes, reads every object span in
   one burst (the spans the previous frame's sample used), then reads the
   counter and ``RealTimeSeconds`` again. The sample is kept only if the
   counter advanced by exactly one and ``RealTimeSeconds`` still has the
   value seen while the finished frame was ticking: then no later tick had
   started when the last byte was read. Otherwise it is dropped and counted
   (``torn``: a tick started during the read; ``unconfirmed``: the tick was
   not seen, as after a missed frame). A sample is also dropped (``late``)
   when ``GDeltaTime`` changed, because the next frame's input dispatch may
   then have run already.
2. keeps polling ``RealTimeSeconds``; when it changes, the next
   ``UWorld::Tick`` has started and the frame's input is in place: it reads
   ``PressedKeys``, ``bPressedJump`` and ``GDeltaTime`` (the tick's
   ``DeltaSeconds`` argument), checks that the counter is unchanged, and
   writes the record.

So record ``R`` holds what the Mac recorder's record holds: the state at the
end of frame ``R.frame - 1``, the keys of frame ``R.frame`` and
``WorldInfo.DeltaSeconds`` = the length of frame ``R.frame - 1``. One
difference: the Mac sample is taken after the input dispatch, so it also
shows what a key command changed at once; here that appears one record later.

What is never believed
----------------------
* A tick start is one forward step of ``RealTimeSeconds`` per frame. When
  it goes back, or changes twice within one frame (a level load that puts the
  new world at the old address), the frame is given up (``resyncs``): its
  sample is not confirmed and no record joins the two worlds.
* A frame's keys that cannot be read, or do not look like a key list, are
  read again while the tick lasts; a record that never gets its keys is
  written without them and ends its run.
* The key bindings of a recording's header are read again in a confirmed
  window (the header says so if no window was long enough), and a failed
  layout check counts only on a sample read in one.
"""

import argparse
import collections
import ctypes
import gc
import json
import os
import struct
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True  # keep the folder free of __pycache__
import asamu_recorder_core as core  # noqa: E402

RECORDER = "asamu_win 0.2.0"
PLATFORM = "win-x86"
SAMPLE_POINT = (
    "poll after the GFrameCounter increment (end of the frame, before the next frame's input "
    "dispatch); keys, bPressedJump and dt_arg read during the next UWorld::Tick"
)
LAYOUT_FILE = "layout_win_x86.json"
EXE_NAME = "ASAMU-Win32-Shipping.exe"
GLOBALS_FILE = "globals.json"
OPTIONAL_FILE = "recorder_optional_win32.json"

# Poll results.
IDLE, FRAME, TICK, LOST = "idle", "frame", "tick", "lost"

MERGE_GAP = 2048  # two reads of one object closer than this become one read
MAX_BLOCK = 16384
MAX_ERRORS = 600  # failed samples in a row before a recording gives up
BINDINGS_TRIES = 60  # windows in which the key bindings are read again before the first look's table is kept
ACTIVE_STATES = ("starting", "waiting-for-game", "waiting-for-player", "recording")
MAX_DROPS = 2000  # entries of the dropped-frame log (runs of one reason share an entry)
MAX_MARKERS = 500
MARK_PREFIX = "MARK-"  # marker request files in the control folder
MAX_MARKER_NAME = 64


class WinError(Exception):
    """A Windows API call failed, or the platform is not Windows."""


class BuildMismatch(Exception):
    """The running executable is not the build the layout describes."""


# ---------------------------------------------------------------- windows api

TH32CS_SNAPPROCESS = 0x00000002
PROCESS_VM_READ = 0x0010
PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
# The only access the recorder ever asks for on the game process, and the
# only handle to it that exists on the recorder's behalf: every query and
# every read below goes through this one handle (see ``module_base``).
GAME_ACCESS = PROCESS_VM_READ | PROCESS_QUERY_INFORMATION
# What the system may grant for that request (the limited query right comes
# with the full one). Anything beyond it and the handle is closed unused.
GAME_ACCESS_GRANTABLE = GAME_ACCESS | PROCESS_QUERY_LIMITED_INFORMATION
STILL_ACTIVE = 259
LIST_MODULES_32BIT = 0x01
LIST_MODULES_ALL = 0x03
PROCESS_BASIC_INFORMATION_CLASS = 0  # NtQueryInformationProcess: ProcessBasicInformation
PROCESS_WOW64_INFORMATION_CLASS = 26  # ... ProcessWow64Information (the 32-bit PEB of a WOW64 process)
ABOVE_NORMAL_PRIORITY_CLASS = 0x00008000
HIGH_PRIORITY_CLASS = 0x00000080
PROCESS_POWER_THROTTLING = 4  # PROCESS_INFORMATION_CLASS::ProcessPowerThrottling
DETACHED_PROCESS = 0x00000008
CREATE_NEW_PROCESS_GROUP = 0x00000200
CREATE_BREAKAWAY_FROM_JOB = 0x01000000
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


class PROCESS_BASIC_INFORMATION(ctypes.Structure):
    _fields_ = [
        ("ExitStatus", ctypes.c_void_p),
        ("PebBaseAddress", ctypes.c_void_p),
        ("AffinityMask", ctypes.c_void_p),
        ("BasePriority", ctypes.c_void_p),
        ("UniqueProcessId", ctypes.c_void_p),
        ("InheritedFromUniqueProcessId", ctypes.c_void_p),
    ]


class POWER_THROTTLING_STATE(ctypes.Structure):
    _fields_ = [("Version", ctypes.c_uint32), ("ControlMask", ctypes.c_uint32), ("StateMask", ctypes.c_uint32)]


_K32 = None


def is_windows():
    return hasattr(ctypes, "WinDLL")


def kernel32():
    """kernel32 with the prototypes of the calls this recorder makes."""
    global _K32
    if _K32 is not None:
        return _K32
    if not is_windows():
        raise WinError("reading the game needs Windows (this module only imports elsewhere, for tests)")
    k = ctypes.WinDLL("kernel32", use_last_error=True)
    vp, u32, i32 = ctypes.c_void_p, ctypes.c_uint32, ctypes.c_int
    k.CreateToolhelp32Snapshot.restype = vp
    k.CreateToolhelp32Snapshot.argtypes = [u32, u32]
    for fn in ("Process32FirstW", "Process32NextW"):
        getattr(k, fn).restype = i32
        getattr(k, fn).argtypes = [vp, vp]
    k.OpenProcess.restype = vp
    k.OpenProcess.argtypes = [u32, i32, u32]
    k.CloseHandle.restype = i32
    k.CloseHandle.argtypes = [vp]
    k.ReadProcessMemory.restype = i32
    k.ReadProcessMemory.argtypes = [vp, vp, vp, ctypes.c_size_t, vp]
    k.GetExitCodeProcess.restype = i32
    k.GetExitCodeProcess.argtypes = [vp, vp]
    k.QueryFullProcessImageNameW.restype = i32
    k.QueryFullProcessImageNameW.argtypes = [vp, u32, vp, vp]
    k.GetCurrentProcess.restype = vp
    k.GetCurrentProcess.argtypes = []
    k.SetPriorityClass.restype = i32
    k.SetPriorityClass.argtypes = [vp, u32]
    k.K32EnumProcessModulesEx.restype = i32
    k.K32EnumProcessModulesEx.argtypes = [vp, vp, u32, vp, u32]
    k.K32GetModuleBaseNameW.restype = u32
    k.K32GetModuleBaseNameW.argtypes = [vp, vp, vp, u32]
    _K32 = k
    return k


_NTDLL = None


def ntdll():
    """ntdll with the one query this recorder makes (process information
    through its own read-only handle)."""
    global _NTDLL
    if _NTDLL is not None:
        return _NTDLL
    if not is_windows():
        raise WinError("reading the game needs Windows (this module only imports elsewhere, for tests)")
    n = ctypes.WinDLL("ntdll")
    n.NtQueryInformationProcess.restype = ctypes.c_long
    n.NtQueryInformationProcess.argtypes = [
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
    n.NtQueryObject.restype = ctypes.c_long
    n.NtQueryObject.argtypes = [
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
    _NTDLL = n
    return n


def handle_access(handle):
    """The access mask the system granted on one of this process's own
    handles (``ObjectBasicInformation``), or None when it cannot be asked."""
    info = (ctypes.c_uint32 * 14)()  # Attributes, GrantedAccess, HandleCount, PointerCount, Reserved[10]
    if ntdll().NtQueryObject(handle, 0, info, ctypes.sizeof(info), None) != 0:
        return None
    return int(info[1])


def find_pids(name=EXE_NAME):
    """Process ids of every running process with this executable name (from
    the system's process list: no process is opened for it)."""
    k = kernel32()
    snap = k.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if not snap or snap == INVALID_HANDLE:
        raise WinError("CreateToolhelp32Snapshot failed (error %d)" % ctypes.get_last_error())
    out = []
    try:
        e = PROCESSENTRY32W()
        e.dwSize = ctypes.sizeof(e)
        ok = k.Process32FirstW(snap, ctypes.byref(e))
        while ok:
            if e.szExeFile.lower() == name.lower():
                out.append(int(e.th32ProcessID))
            ok = k.Process32NextW(snap, ctypes.byref(e))
    finally:
        k.CloseHandle(snap)
    return out


def _read_exact(handle, addr, size):
    """``size`` bytes of the process behind ``handle``, or None."""
    k = kernel32()
    buf = ctypes.create_string_buffer(size)
    got = ctypes.c_size_t(0)
    if not k.ReadProcessMemory(handle, addr, buf, size, ctypes.byref(got)) or got.value != size:
        return None
    return ctypes.string_at(buf, size)


def peb_image_base(handle):
    """Load address of the process's own executable, from its environment
    block (``PEB.ImageBaseAddress``): one information query and one memory
    read through ``handle``. For a 32-bit process under a 64-bit Windows the
    32-bit block is used. None when it cannot be read (e.g. the process is
    still being set up)."""
    n = ntdll()
    wow = ctypes.c_size_t(0)
    status = n.NtQueryInformationProcess(
        handle, PROCESS_WOW64_INFORMATION_CLASS, ctypes.byref(wow), ctypes.sizeof(wow), None)
    if status == 0 and wow.value:
        peb, width = int(wow.value), 4
    else:
        info = PROCESS_BASIC_INFORMATION()
        status = n.NtQueryInformationProcess(
            handle, PROCESS_BASIC_INFORMATION_CLASS, ctypes.byref(info), ctypes.sizeof(info), None)
        if status != 0 or not info.PebBaseAddress:
            return None
        peb, width = int(info.PebBaseAddress), ctypes.sizeof(ctypes.c_void_p)
    # PEB: four flag bytes (padded to a pointer), Mutant, ImageBaseAddress.
    data = _read_exact(handle, peb + 2 * width, width)
    if data is None:
        return None
    return int.from_bytes(data, "little") or None


def psapi_module_base(handle, name):
    """Load address of the module ``name``, from the module list PSAPI reads
    out of the process through ``handle`` (the 32-bit list first: that is
    where a 32-bit game's executable is, seen from a 64-bit Python)."""
    k = kernel32()
    mods = (ctypes.c_void_p * 1024)()
    needed = ctypes.c_uint32(0)
    buf = ctypes.create_unicode_buffer(260)
    for which in (LIST_MODULES_32BIT, LIST_MODULES_ALL):
        if not k.K32EnumProcessModulesEx(handle, mods, ctypes.sizeof(mods), ctypes.byref(needed), which):
            continue
        for i in range(min(needed.value // ctypes.sizeof(ctypes.c_void_p), len(mods))):
            if mods[i] and k.K32GetModuleBaseNameW(handle, mods[i], buf, 260) and buf.value.lower() == name.lower():
                return int(mods[i])
    return None


def module_base(pid, handle, name=EXE_NAME):
    """Load address of the game's executable in the (32-bit, WOW64) process.

    Found through the recorder's own read-only handle and nothing else: the
    process environment block first, PSAPI's module list as the fallback. A
    Toolhelp module snapshot is deliberately not used: the system takes it
    with a second handle of its own, with access rights this recorder would
    not control. A candidate counts only if an executable header is readable
    there; the build check (``check_image``) follows in any case.
    """
    del pid  # everything goes through the handle
    for _attempt in range(20):
        for find in (peb_image_base, lambda h: psapi_module_base(h, name)):
            base = find(handle)
            if base and _read_exact(handle, base, 2) == b"MZ":
                return base
        time.sleep(0.05)  # the process is still starting: its module list is not set up yet
    return None


class ProcessTarget:
    """Read-only view of the game process."""

    def __init__(self, pid, handle, base, exe_path, access=None):
        k = kernel32()
        self.pid = pid
        self.base = base
        self.exe_path = exe_path
        self.access = access  # the granted access mask of the handle
        self._k = k
        self._h = handle
        self._rpm = k.ReadProcessMemory
        self._q = ctypes.c_uint64(0)
        self._qp = ctypes.byref(self._q)
        self._d = ctypes.c_uint32(0)
        self._dp = ctypes.byref(self._d)
        self._buf = ctypes.create_string_buffer(1 << 16)
        self._n = ctypes.c_size_t(0)
        self._np = ctypes.byref(self._n)
        self._code = ctypes.c_uint32(0)

    def read(self, addr, size):
        buf = self._buf if size <= len(self._buf) else ctypes.create_string_buffer(size)
        if not self._rpm(self._h, addr, buf, size, self._np) or self._n.value != size:
            return None
        return ctypes.string_at(buf, size)

    def u64(self, addr):
        return self._q.value if self._rpm(self._h, addr, self._qp, 8, None) else None

    def u32(self, addr):
        return self._d.value if self._rpm(self._h, addr, self._dp, 4, None) else None

    def alive(self):
        if not self._k.GetExitCodeProcess(self._h, ctypes.byref(self._code)):
            return False
        return self._code.value == STILL_ACTIVE

    def close(self):
        if self._h:
            self._k.CloseHandle(self._h)
            self._h = None


class MemoryTarget:
    """The same interface over any ``read(addr, size)`` (tests, fake images)."""

    def __init__(self, read, base, exe_path=None, pid=0, alive=None):
        self._read = read
        self.base = base
        self.exe_path = exe_path
        self.pid = pid
        self.access = None
        self._alive = alive

    def read(self, addr, size):
        data = self._read(addr, size)
        return data if data is not None and len(data) == size else None

    def u64(self, addr):
        data = self.read(addr, 8)
        return None if data is None else struct.unpack("<Q", data)[0]

    def u32(self, addr):
        data = self.read(addr, 4)
        return None if data is None else struct.unpack("<I", data)[0]

    def alive(self):
        return True if self._alive is None else bool(self._alive())

    def close(self):
        pass


def open_game(pid=None, base=None, name=EXE_NAME):
    """The running game as a ProcessTarget, or None when it is not running.

    The handle has PROCESS_VM_READ | PROCESS_QUERY_INFORMATION and nothing
    else, it is not inheritable, and it is the only handle to the game: the
    module base and the executable's path are asked through it too.
    ``pid``/``base`` override the search (tests against a stand-in).
    """
    k = kernel32()
    if pid is None:
        pids = find_pids(name)
        if not pids:
            return None
        pid = pids[0]
    handle = k.OpenProcess(GAME_ACCESS, 0, pid)
    if not handle:
        raise WinError("OpenProcess(pid %d, read-only) failed (error %d)" % (pid, ctypes.get_last_error()))
    try:
        access = handle_access(handle)
        if access is not None and access & ~GAME_ACCESS_GRANTABLE:
            raise WinError("the handle to pid %d has more than read access (0x%X); not used" % (pid, access))
        if base is None:
            base = module_base(pid, handle, name)
            if not base:
                raise WinError("module %s not found in pid %d" % (name, pid))
        buf = ctypes.create_unicode_buffer(1024)
        size = ctypes.c_uint32(1024)
        exe = buf.value if k.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size)) else None
    except Exception:
        k.CloseHandle(handle)
        raise
    return ProcessTarget(pid, handle, base, exe, access)


def raise_own_priority(level):
    """Scheduling of THIS process only (never the game's): ``above``/``high``
    raise its priority class and opt it out of background power throttling,
    so the polling loop is not parked on a slow core. Best effort."""
    if level == "normal" or not is_windows():
        return
    k = kernel32()
    me = k.GetCurrentProcess()
    k.SetPriorityClass(me, HIGH_PRIORITY_CLASS if level == "high" else ABOVE_NORMAL_PRIORITY_CLASS)
    try:
        fn = k.SetProcessInformation
        fn.restype = ctypes.c_int
        fn.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        state = POWER_THROTTLING_STATE(1, 1, 0)  # control execution speed: not throttled
        fn(me, PROCESS_POWER_THROTTLING, ctypes.byref(state), ctypes.sizeof(state))
    except (AttributeError, OSError):
        pass


def pid_alive(pid):
    if not pid:
        return False
    if is_windows():
        k = kernel32()
        h = k.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, int(pid))
        if not h:
            return False
        try:
            code = ctypes.c_uint32(0)
            return bool(k.GetExitCodeProcess(h, ctypes.byref(code))) and code.value == STILL_ACTIVE
        finally:
            k.CloseHandle(h)
    try:
        os.kill(int(pid), 0)
    except OSError:
        return False
    return True


# ------------------------------------------------------------------- snapshot


def merge_ranges(ranges, gap=MERGE_GAP, limit=MAX_BLOCK):
    """Sorted ``(start, end, parts)`` covering every ``(addr, size)``: ranges
    closer than ``gap`` are read together (one system call instead of two)."""
    out = []
    for addr, size in sorted(set(ranges)):
        if size <= 0:
            continue
        end = addr + size
        if out and addr <= out[-1][1] + gap and max(end, out[-1][1]) - out[-1][0] <= limit:
            start, old_end, parts = out[-1]
            out[-1] = (start, max(old_end, end), parts + [(addr, size)])
        else:
            out.append((addr, end, [(addr, size)]))
    return out


class Snapshot:
    """A ``read(addr, size)`` that serves a sample from blocks captured in one
    burst.

    ``capture`` reads the spans the previous sample used (addresses change
    rarely from one frame to the next); ``read`` answers from those blocks
    and falls back to a live read, counted in ``misses``, for anything else
    (a new pawn, a new name); ``commit`` turns the reads of the sample just
    taken into the next plan. Outside a sample (after ``commit`` or
    ``release``) every read is live.
    """

    def __init__(self, read):
        self._read = read
        self.plan = []
        self.blocks = []
        self.trace = None
        self.misses = 0
        self.failed = 0
        self._last = None

    def capture(self):
        blocks = []
        read = self._read
        for start, end, parts in self.plan:
            data = read(start, end - start)
            if data is not None:
                blocks.append((start, end, data))
                continue
            self.failed += 1
            for addr, size in parts:  # the merged span crosses unreadable memory
                data = read(addr, size)
                if data is not None:
                    blocks.append((addr, addr + size, data))
        self.blocks = blocks
        self.trace = []
        self.misses = 0
        return len(self.plan)

    def read(self, addr, size):
        if self.trace is not None:
            self.trace.append((addr, size))
            for start, end, data in self.blocks:
                if start <= addr and addr + size <= end:
                    i = addr - start
                    return data[i : i + size]
            self.misses += 1
        return self._read(addr, size)

    def commit(self):
        trace = self.trace
        if trace is not None and trace != self._last:
            self.plan = merge_ranges(trace)
            self._last = trace
        self.release()

    def release(self):
        self.blocks = []
        self.trace = None


# -------------------------------------------------------------------- session


def data_files(layout_path, file_name):
    """Where a data file of the build may lie: ``data/win32`` next to the
    layout (the working folder on the game machine), the layout's own
    folder, or the repository's data folder."""
    d = os.path.dirname(os.path.abspath(layout_path or os.path.join(HERE, LAYOUT_FILE)))
    return (
        os.path.join(d, "data", "win32", file_name),
        os.path.join(d, file_name),
        os.path.join(d, "..", "..", "docs", "reverse-engineering", "data", "win32", file_name),
    )


def load_optional(layout, layout_path=None, explicit=None):
    """Adds the optional fields to ``layout`` from ``explicit`` or from the
    first ``recorder_optional_win32.json`` found (``data_files``). Returns a
    line for the report; a missing file is not an error (the recording then
    has the optional fields that need no offsets)."""
    candidates = (explicit,) if explicit else data_files(layout_path, OPTIONAL_FILE)
    for cand in candidates:
        try:
            with open(cand, "r", encoding="utf-8") as fh:
                data = json.load(fh)
        except OSError:
            continue
        except ValueError as e:
            raise core.LayoutError("%s is not JSON (%s)" % (os.path.basename(cand), e))
        try:
            layout.extend(data)
        except (KeyError, TypeError, ValueError) as e:
            raise core.LayoutError("%s is malformed (%s: %s)" % (os.path.basename(cand), type(e).__name__, e))
        return "optional fields: layout %s (%d fields)" % (layout.optional_id, len(data["fields"]))
    if explicit:
        raise core.LayoutError("optional layout %s cannot be read" % explicit)
    return "optional fields: no %s found; only the fields that need no offsets are recorded" % OPTIONAL_FILE


def find_extra_rva(layout, layout_path, name):
    """RVA of a global the layout file does not list (``GCurrentTime``), from
    the layout itself or from ``globals.json`` of the same build; None when
    unknown. Used only to tell late samples apart in fixed-step mode."""
    spec = layout.data.get("symbols", {}).get(name)
    if spec and "rva" in spec:
        return int(spec["rva"], 16)
    for cand in data_files(layout_path, GLOBALS_FILE):
        try:
            with open(cand, "r", encoding="utf-8") as fh:
                data = json.load(fh)
        except (OSError, ValueError):
            continue
        if data.get("game_build") not in (None, layout.game_build):
            continue
        for g in data.get("globals", []):
            if g.get("name") == name and isinstance(g.get("rva"), (int, str)):
                return g["rva"] if isinstance(g["rva"], int) else int(g["rva"], 16)
    return None


def check_image(target, layout):
    """Compares the module header at the base with the layout's ``image``
    block; returns (time_date_stamp, size_of_image) or raises BuildMismatch."""
    hdr = target.read(target.base, 0x400)
    if hdr is None or hdr[:2] != b"MZ":
        raise BuildMismatch("no executable header at the module base %#x" % target.base)
    pe = struct.unpack_from("<I", hdr, 0x3C)[0]
    if pe > 0x400 - 0x58 or hdr[pe : pe + 4] != b"PE\0\0":
        raise BuildMismatch("no PE header at the module base %#x" % target.base)
    stamp = struct.unpack_from("<I", hdr, pe + 8)[0]
    size = struct.unpack_from("<I", hdr, pe + 0x50)[0]
    image = layout.data.get("image") or {}
    want_stamp, want_size = image.get("time_date_stamp"), image.get("size_of_image")
    if (want_stamp is not None and stamp != want_stamp) or (want_size is not None and size != want_size):
        raise BuildMismatch(
            "the running executable is not build %s (time stamp %d, image size %d; the layout expects %s, %s)"
            % (layout.game_build, stamp, size, want_stamp, want_size)
        )
    return stamp, size


class Session:
    """One attached game: layout, addresses, snapshot reader and sampler."""

    def __init__(self, target, layout, layout_path=None, ignore_sentinels=False, optional=None):
        """``optional``: the optional groups to record (``core.Sampler``):
        None for plain version-1 records, True for all the layout has."""
        if layout.pointer_size != 4:
            raise core.LayoutError("layout %s is not a 32-bit layout" % layout.id)
        self.target = target
        self.layout = layout
        self.image = check_image(target, layout)
        base = target.base
        self.addrs = {}
        for spec in layout.data["symbols"].values():
            if "rva" in spec:
                self.addrs[spec["mangled"]] = base + int(spec["rva"], 16)
        self.snap = Snapshot(target.read)
        self.sampler = core.Sampler(
            layout, self.snap.read, self.addrs, ignore_sentinels=ignore_sentinels, optional=optional)
        o = self.sampler.o
        self.a_counter = self.addrs[o.sym_frame_counter]
        self.a_delta_time = self.addrs.get(o.sym_delta_time)
        rva = find_extra_rva(layout, layout_path, "GCurrentTime")
        self.a_current_time = None if rva is None else base + rva
        self.timing = self.sampler.timing()

    def late_marker(self):
        """(address, name) of the global whose change shows that the next
        frame's time update has run (the input dispatch follows it).

        Variable step: ``GDeltaTime`` is stored after the frame-limit wait,
        last thing before the engine tick. Fixed step: it never changes, so
        ``GCurrentTime`` (advanced once per frame) is used when its address
        is known."""
        if self.timing.get("benchmarking") or self.timing.get("fixed_step"):
            if self.a_current_time is not None:
                return self.a_current_time, "GCurrentTime"
            return None, None
        if self.a_delta_time is not None:
            return self.a_delta_time, "GDeltaTime"
        return None, None


def forget_sentinel_failures(sampler):
    """Drops failed layout checks so the objects are checked again (a check
    made while the game was running through a tick proves nothing)."""
    for key in [k for k, good in sampler.checked.items() if not good]:
        del sampler.checked[key]
    del sampler.sentinel_failures[:]


def f32_bits(x):
    return struct.unpack("<I", struct.pack("<f", x))[0]


def bits_f32(b):
    return struct.unpack("<f", struct.pack("<I", b))[0]


def rts_advanced(old_bits, new_bits):
    """True when ``RealTimeSeconds`` went from ``old`` to a later time, as
    the start of a world tick makes it do. A value that went back (a world
    that was reset or replaced at the same address) or is not a number is
    not a tick start."""
    if old_bits is None or new_bits is None:
        return False
    a, b = bits_f32(old_bits), bits_f32(new_bits)
    return a == a and b == b and abs(a) != float("inf") and abs(b) != float("inf") and b > a


def bits_f64(b):
    return struct.unpack("<d", struct.pack("<Q", b))[0]


# ---------------------------------------------------------------------- stats


class Acc:
    """Count, mean, min, max, and percentiles of the last 512 values (few
    enough that a summary on the helper thread never holds up the polling)."""

    def __init__(self):
        self.n = 0
        self.total = 0.0
        self.lo = None
        self.hi = None
        self.recent = collections.deque(maxlen=512)

    def add(self, x):
        self.n += 1
        self.total += x
        if self.lo is None or x < self.lo:
            self.lo = x
        if self.hi is None or x > self.hi:
            self.hi = x
        self.recent.append(x)

    def mean(self):
        return self.total / self.n if self.n else None

    def summary(self, digits=3):
        if not self.n:
            return None
        s = sorted(self.recent)

        def pct(p):
            return round(s[min(len(s) - 1, int(p * len(s)))], digits)

        return {
            "n": self.n,
            "mean": round(self.total / self.n, digits),
            "min": round(self.lo, digits),
            "p50": pct(0.50),
            "p99": pct(0.99),
            "max": round(self.hi, digits),
        }


class Stats:
    def __init__(self):
        self.frames = 0  # frame ends seen (counter changes)
        self.sampled = 0  # consistent post-tick samples
        self.written = 0  # records handed to the sink
        self.torn = 0  # a tick started while the sample was read
        self.unconfirmed = 0  # the finished frame's tick was not seen
        self.late = 0  # the next frame's time update had run
        self.late_kept = 0
        self.missed = 0  # frames whose end was never seen (counter jumped)
        self.no_input = 0  # records written without their frame's input
        self.resyncs = 0  # frames in which RealTimeSeconds did not behave like one tick start
        self.input_retries = 0  # failed reads of a frame's input (read again while the tick lasts)
        self.retries = 0
        self.errors = 0
        self.error_run = 0  # failed samples in a row
        self.skipped = {}
        self.live_reads = 0  # reads outside the burst, inside samples
        self.capture_us = Acc()  # counter change seen -> burst and its check done
        self.window_ms = Acc()  # counter change seen -> next tick seen
        self.tick_ms = Acc()  # tick seen -> counter change seen
        self.period_ms = Acc()  # counter change -> counter change
        self.plan_reads = Acc()
        self.last_error = None
        self.drops = []  # every frame without a record (or without its input), with the reason
        self.drops_omitted = 0  # frames not listed because the log was full

    def skip(self, why):
        self.skipped[why] = self.skipped.get(why, 0) + 1

    def drop(self, frame, reason, detail=None, count=1):
        """Logs ``count`` frames from ``frame`` on that got no record (reason
        ``missed``, ``skipped``, ``unconfirmed``, ``torn``, ``late``) or a
        record without its input (``no-input``). ``frame`` is the number the
        record has or would have had. Consecutive frames of one reason share
        an entry, and so does a stretch without a player (a menu), whose
        frames are not all seen."""
        last = self.drops[-1] if self.drops else None
        if last is not None and last["reason"] == reason and last["detail"] == detail and (
                frame == last["last"] + 1 or (reason == "skipped" and frame > last["last"])):
            last["last"] = frame + count - 1
            last["count"] += count
            return
        if len(self.drops) >= MAX_DROPS:
            self.drops_omitted += count
            return
        self.drops.append({"frame": frame, "last": frame + count - 1, "count": count, "reason": reason, "detail": detail})

    def as_dict(self):
        period = self.period_ms.mean()
        return {
            "frames_seen": self.frames,
            "sampled": self.sampled,
            "written": self.written,
            "torn": self.torn,
            "unconfirmed": self.unconfirmed,
            "late": self.late,
            "late_kept": self.late_kept,
            "missed": self.missed,
            "no_input": self.no_input,
            "resyncs": self.resyncs,
            "input_retries": self.input_retries,
            "retries": self.retries,
            "errors": self.errors,
            "skipped": dict(sorted(self.skipped.items())),
            "live_reads": self.live_reads,
            "fps": round(1000.0 / period, 2) if period else None,
            "capture_us": self.capture_us.summary(1),
            "window_ms": self.window_ms.summary(),
            "tick_ms": self.tick_ms.summary(),
            "period_ms": self.period_ms.summary(),
            "reads_per_burst": self.plan_reads.summary(1),
            "last_error": self.last_error,
            "drops": [dict(d) for d in list(self.drops)],
            "drops_omitted": self.drops_omitted,
        }

    def line(self):
        d = self.as_dict()

        def acc(key, unit):
            a = d[key]
            return "n/a" if not a else "%s %s mean, %s p99, %s max" % (a["mean"], unit, a["p99"], a["max"])

        return (
            "%d frame ends seen: %d sampled, %d written (%d without input), %d torn, %d unconfirmed, "
            "%d late, %d missed, %d resyncs, %d errors, skipped %r; fps %s; burst %s (%s reads); "
            "window %s; tick %s"
            % (
                d["frames_seen"], d["sampled"], d["written"], d["no_input"], d["torn"], d["unconfirmed"],
                d["late"], d["missed"], d["resyncs"], d["errors"], d["skipped"], d["fps"],
                acc("capture_us", "us"), d["reads_per_burst"]["mean"] if d["reads_per_burst"] else "n/a",
                acc("window_ms", "ms"), acc("tick_ms", "ms"),
            )
        )


# --------------------------------------------------------------------- poller


class Poller:
    """The per-frame state machine (see the module docstring).

    ``poll`` makes one or two small reads and returns IDLE, or handles a
    frame end (FRAME) or a tick start (TICK), or returns LOST when the
    counter cannot be read. Finished records go to ``sink(record)``.
    """

    def __init__(self, session, sink, keep_late=False, clock=time.perf_counter):
        self.sess = session
        self.t = session.target
        self.s = session.sampler
        self.o = session.sampler.o
        self.snap = session.snap
        self.sink = sink
        self.keep_late = keep_late
        self.clock = clock
        self.stats = Stats()
        self.a_counter = session.a_counter
        self.a_dt = session.a_delta_time
        self.a_marker, self.marker_name = session.late_marker()
        self.cur = None  # GFrameCounter as last seen
        self.wi = None  # WorldInfo address of the last sample (its RealTimeSeconds marks tick starts)
        self.pc = None
        self.inp = None
        self.win_rts = None  # RealTimeSeconds bits after the last frame end
        self.tick_rts = None  # RealTimeSeconds bits seen while frame `cur` was ticking
        self.tick_marker = None
        self.pending = None  # record of frame `cur` waiting for its input
        self.spoiled = False  # frame `cur`: RealTimeSeconds did not behave like one tick start
        self.spoil_why = None  # ... and how (for the dropped-frame log)
        self.no_tick_why = None  # why frame `cur`'s tick start was not taken, when it was seen
        self.bindings_seen = False  # the key bindings were read in a confirmed window
        self.bindings_tries = 0
        self.t_end = None
        self.t_tick = None
        self.fatal = None
        self.has_player = False

    # -- small reads
    def _markers(self, wi):
        t = self.t
        c = t.u64(self.a_counter)
        r = t.u32(wi + self.o.real_time_seconds) if wi else None
        m = t.u64(self.a_marker) if self.a_marker else None
        return c, r, m

    def poll(self):
        c = self.t.u64(self.a_counter)
        if c is None:
            return LOST
        if self.cur is None:
            self.cur = c
            return IDLE
        if c != self.cur:
            self._frame_end(c)
            return FRAME
        if self.wi:
            # RealTimeSeconds as last seen during this frame: the window's
            # value until the tick is seen, the tick's value after that. One
            # frame changes it once, forwards (the store at the top of the
            # world tick); anything else is not a tick start.
            ref = self.win_rts if self.tick_rts is None else self.tick_rts
            if ref is not None:
                r = self.t.u32(self.wi + self.o.real_time_seconds)
                if r is not None and r != ref:
                    c2 = self.t.u64(self.a_counter)
                    if c2 is not None and c2 != self.cur:
                        # The frame ended between the two reads above (and,
                        # with a very short window, the next tick has begun
                        # already): an ordinary frame end, nothing unusual.
                        self._frame_end(c2)
                        return FRAME
                    if self.tick_rts is None and not self.spoiled and rts_advanced(ref, r):
                        self._tick_start(r)
                    else:
                        self._resync(r, ref)
                    return TICK
        return IDLE

    def _resync(self, r, ref=None):
        """``RealTimeSeconds`` went back, or changed a second time within one
        frame: the world was reset or replaced at this address (a level
        load), or what was taken for this frame's tick start was not it.
        Nothing read in this frame is attributed to a tick: the frame's
        sample will not be confirmed, a record still waiting for its input
        ends its run without it, and the next frame starts clean."""
        if not self.spoiled:
            self.stats.resyncs += 1
            if ref is None:
                self.spoil_why = "RealTimeSeconds did not behave like one tick start"
            else:
                how = "changed a second time in one frame" if self.tick_rts is not None else "did not move forward"
                self.spoil_why = "RealTimeSeconds %s (%.4f to %.4f)" % (how, bits_f32(ref), bits_f32(r))
        self.spoiled = True
        self.tick_rts = None
        self.tick_marker = None
        self.win_rts = r

    # -- frame end: the state sample
    def _frame_end(self, c):
        now = self.clock()
        st = self.stats
        st.frames += 1
        steps = c - self.cur
        tick_rts, tick_marker, old_wi = self.tick_rts, self.tick_marker, self.wi
        # Why this frame's sample cannot be confirmed, if it cannot.
        cause = self.no_tick_why if tick_rts is None else None
        if steps != 1:
            if steps > 1 and self.has_player:  # in a menu nothing is lost
                st.missed += steps - 1
                st.drop(self.cur + 1, "missed", "the counter advanced by %d between two polls" % steps, steps - 1)
            tick_rts = None
            cause = "the counter advanced by %d between two polls" % steps
        if self.spoiled:
            tick_rts = None
            cause = "resync: %s" % self.spoil_why
            self.spoiled = False
        if self.pending is not None:
            # Its tick was never seen, so the next sample cannot be confirmed
            # either: this record ends its run and its keys are not used.
            self._emit(self.pending, False, cause or "the tick start of its frame was not seen")
            self.pending = None
        self.spoil_why = None
        self.no_tick_why = None
        self.cur = c
        self.tick_rts = None
        self.tick_marker = None
        self.win_rts = None
        if self.t_tick is not None:
            st.tick_ms.add((now - self.t_tick) * 1e3)
        if self.t_end is not None:
            st.period_ms.add((now - self.t_end) * 1e3)
        self.t_end = now
        self.t_tick = None

        snap = self.snap
        rec = why = None
        m1 = m2 = (None, None, None)
        t_burst = now
        for attempt in range(2):
            st.plan_reads.add(snap.capture())
            m1 = self._markers(old_wi)
            t_burst = self.clock()
            try:
                rec, why = self.s.sample(c, None)
                failed = None
            except (core.ReadError, core.LayoutError, struct.error, ValueError, UnicodeError) as e:
                rec, why, failed = None, "error", "%s: %s" % (type(e).__name__, e)
            wi = self.s.objects.get("world_info")
            st.live_reads += snap.misses
            m2 = self._markers(wi) if (snap.misses or wi != old_wi or failed) else m1
            if failed is None:
                snap.commit()
                break
            snap.release()
            st.last_error = failed
            in_window = tick_rts is not None and m2[0] == c and m2[1] == tick_rts
            if attempt == 0 and in_window:
                st.retries += 1  # nothing has moved yet: read the frame again
                continue
            st.errors += 1
            st.error_run += 1
            break
        if failed is None:
            st.error_run = 0

        objs = self.s.objects
        wi = objs.get("world_info")
        self.wi, self.pc, self.inp = wi, objs.get("controller"), objs.get("input")
        self.has_player = bool(objs)
        if m2[0] == c and wi:
            self.win_rts = m2[1]
        if rec is None:
            if why == "sentinel-mismatch":
                in_window = (
                    tick_rts is not None and wi == old_wi
                    and m1[0] == c and m2[0] == c and m1[1] == tick_rts and m2[1] == tick_rts
                )
                if in_window:
                    self.fatal = "layout check failed, nothing recorded from this object: " + "; ".join(
                        self.s.sentinel_failures[:4]
                    )
                else:
                    # Read while the world was (or may have been) changing:
                    # not evidence about the layout. Check the object again.
                    why = "sentinel-unconfirmed"
                    forget_sentinel_failures(self.s)
            st.skip(why)
            st.drop(c, "skipped", why)
            return
        if tick_rts is None or wi != old_wi:
            st.unconfirmed += 1
            if cause is None:
                if old_wi is None:
                    cause = "no WorldInfo was known while the frame ran (the first sample of a level)"
                elif wi != old_wi:
                    cause = "the WorldInfo changed (a level was loaded)"
                else:
                    cause = "the tick start of the frame was not seen"
            st.drop(c, "unconfirmed", cause)
            self.s.optional_unconfirmed()
            return
        rts_rec = f32_bits(rec["world"]["real_time_seconds"])
        if not (m1[0] == c and m2[0] == c and m1[1] == tick_rts and m2[1] == tick_rts and rts_rec == tick_rts):
            st.torn += 1
            if m1[0] != c or m2[0] != c:
                detail = "the counter moved while the sample was read"
            elif m1[1] != tick_rts or m2[1] != tick_rts:
                detail = "RealTimeSeconds moved while the sample was read (the next tick had started)"
            else:
                detail = "the sample's RealTimeSeconds is not the one seen during the tick"
            st.drop(c, "torn", detail)
            self.s.optional_unconfirmed()
            if m2[0] == c and m2[1] != tick_rts and rts_advanced(tick_rts, m2[1]):
                # The counter is unchanged and RealTimeSeconds has moved on:
                # frame c is ticking right now. Taking its input here keeps
                # the next sample confirmable, so one torn frame costs one
                # record, not two. (Should this not be the tick after all,
                # `poll` sees RealTimeSeconds change once more in this frame
                # and resynchronises.)
                self._tick_start(m2[1])
            return
        if tick_marker is not None and (m1[2] != tick_marker or m2[2] != tick_marker):
            st.late += 1
            if not self.keep_late:
                st.drop(c, "late", "%s changed before the sample was complete (the next frame's time update had run)"
                        % self.marker_name)
                self.s.optional_unconfirmed()
                return
            st.late_kept += 1
        st.sampled += 1
        st.capture_us.add((t_burst - now) * 1e6)
        self.pending = rec
        if not self.bindings_seen and self.bindings_tries < BINDINGS_TRIES:
            self._confirm_bindings(c, tick_rts, tick_marker)

    def _confirm_bindings(self, c, tick_rts, tick_marker):
        """Reads the key bindings once more, in the window the sample just
        accepted was read in, and keeps them if the window was still open
        afterwards. The table read by the first look (at an arbitrary
        moment, possibly while a level was being set up) is replaced by one
        read while the world stood still. Separate from the sample's own
        reads, so a short window costs the bindings, never a sample."""
        self.bindings_tries += 1
        if not self.inp:
            return
        try:
            fresh = self.s.read_bindings(self.inp)
        except (core.ReadError, core.LayoutError, struct.error, ValueError, UnicodeError):
            return
        m = self._markers(self.wi)
        if m[0] == c and m[1] == tick_rts and (tick_marker is None or m[2] == tick_marker):
            self.s.bindings = fresh
            self.bindings_seen = True

    # -- tick start: the frame's input
    def _tick_start(self, r):
        now = self.clock()
        t = self.t
        st = self.stats
        keys = jump = dt_bits = None
        try:
            if self.inp:
                keys = self.s.pressed_keys(self.inp, strict=True)
            if self.pc:
                jump = self.s.pressed_jump(self.pc)
        except (core.ReadError, struct.error) as e:
            # Not taken as this frame's tick start: `poll` comes back here
            # while the tick lasts. If the input never reads, the waiting
            # record is written without it at the frame end and ends its run
            # (the next sample is then not confirmed), so no run ever carries
            # a record with another frame's keys in its middle.
            st.last_error = "input read: %s" % e
            st.input_retries += 1
            self.no_tick_why = "the frame's input could not be read at its tick start"
            return
        if self.a_dt:
            dt_bits = t.u64(self.a_dt)
        if self.a_marker == self.a_dt:
            marker = dt_bits
        else:
            marker = t.u64(self.a_marker) if self.a_marker else None
        if t.u64(self.a_counter) != self.cur:
            self.no_tick_why = "the frame ended while its input was read"
            return  # the frame ended while its input was read: not confirmed
        self.no_tick_why = None
        self.tick_rts = r
        self.tick_marker = marker
        self.t_tick = now
        if self.t_end is not None:
            st.window_ms.add((now - self.t_end) * 1e3)
        rec = self.pending
        if rec is None:
            return
        self.pending = None
        if keys is not None:
            rec["player"]["keys"] = keys
        if jump is not None:
            rec["player"]["pressed_jump"] = jump
        if dt_bits is not None:
            dt = core.f32(bits_f64(dt_bits)) if abs(bits_f64(dt_bits)) < 3.0e38 else None
            if dt is not None and dt == dt:
                rec["dt_arg"] = dt
        self._emit(rec, True)

    def _emit(self, rec, with_input, why=None):
        self.stats.written += 1
        if not with_input:
            self.stats.no_input += 1
            self.stats.drop(rec["frame"], "no-input", "record written without its input: %s" % why)
        self.sink(rec)

    def flush(self):
        """Hands over a record still waiting for its input (end of a recording)."""
        if self.pending is not None:
            self._emit(self.pending, False, "the recording ended first")
            self.pending = None


# ------------------------------------------------------------------ recording


def win_install_root(exe_path):
    """The install folder of an executable at ``<install>/Binaries/Win32/``;
    otherwise the executable's own folder."""
    d = os.path.dirname(os.path.realpath(exe_path))
    up = os.path.dirname(d)
    if os.path.basename(d).lower() == "win32" and os.path.basename(up).lower() == "binaries":
        return os.path.dirname(up)
    return d


def default_out_dir():
    """``research/local/traces/win`` inside a repository checkout, else
    ``traces`` next to this script (the working folder on the game machine)."""
    root = os.path.abspath(os.path.join(HERE, "..", ".."))
    if os.path.isfile(os.path.join(root, "Cargo.toml")) and os.path.isdir(os.path.join(root, "tools")):
        return os.path.join(root, "research", "local", "traces", "win")
    return os.path.join(HERE, "traces")


def utc_stamp():
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())


def write_json_atomic(path, data):
    tmp = path + ".tmp"
    text = json.dumps(data, sort_keys=True) + "\n"  # one line: quick to write and to read
    with open(tmp, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)
    for _ in range(20):
        try:
            os.replace(tmp, path)
            return
        except PermissionError:  # a reader has the file open (Windows)
            time.sleep(0.01)
    os.replace(tmp, path)


def read_json(path):
    for _ in range(5):
        try:
            with open(path, "r", encoding="utf-8") as fh:
                return json.load(fh)
        except FileNotFoundError:
            return None
        except (OSError, ValueError):
            time.sleep(0.02)
    return None


def marker_name(text):
    """A marker name as it is stored: printable characters only, at most
    MAX_MARKER_NAME of them."""
    out = "".join(c if c.isprintable() else " " for c in str(text)).strip()
    return out[:MAX_MARKER_NAME] or "marker"


def optional_report(sampler):
    """What the status and stats files say about the optional fields."""
    if not sampler.opt_on and not sampler.opt_missing:
        return None
    return {
        "layout": sampler.L.optional_id,
        "fields": sampler.optional_fields(),
        "not_in_layout": list(sampler.opt_missing),
        "off": dict(sorted(sampler.opt_off.items())),
        "left_out": {k: {"records": v[0], "last_reason": v[1]} for k, v in sorted(sampler.opt_rejected.items())},
    }


class Recording:
    """One raw recording: file, limits, status, markers and STOP handling.

    All file work happens on a helper thread (flush, ``status.json``, the
    STOP check), so the polling thread never waits for the disk.
    """

    def __init__(self, session, opts, out_dir, ctl_dir, clock=time.perf_counter, sleep=time.sleep):
        self.sess = session
        self.opts = opts
        self.out_dir = out_dir
        self.ctl_dir = ctl_dir
        self.clock = clock
        self.sleep = sleep
        self.poller = Poller(session, self._sink, keep_late=opts.keep_late, clock=clock)
        self.stats = self.poller.stats
        self.session_id = opts.session or utc_stamp()
        self.lines = []
        self.lock = threading.Lock()
        self.header_written = False
        self.header_bindings = None  # the table written to the header, and whether a window confirmed it
        self.header_bindings_confirmed = False
        self.records = 0
        self.gaps = 0
        self.last_frame = None
        self.first_frame = None
        self.t_start = clock()
        self.t_first = None
        self.cpu0 = time.process_time()
        self.done = None  # reason the recording ended
        self.failed = False
        self.stop_flag = False
        self.state = "starting"
        self.fh = None
        self.path = None
        self.outputs = []
        self.markers = []  # named markers of `mark`, each with the frame it was set at
        self.markers_omitted = 0
        self.header_optional = None  # the optional fields the header announced
        self._thread = None
        self._thread_stop = threading.Event()
        self.stop_files = [os.path.join(ctl_dir, "STOP"), os.path.join(out_dir, "STOP")]

    # -- output
    def open(self):
        stem = "%s-%s" % (utc_stamp(), core.safe_component(self.opts.scenario or "free"))
        self.fh, self.path = core.create_unique(self.out_dir, stem, ".raw.jsonl")

    def header(self):
        timing = self.sess.timing
        notes = [
            "platform: %s" % PLATFORM,
            "sampling: read-only polling, no debugger; a record is written only when its sample was read "
            "entirely between the GFrameCounter increment and the next RealTimeSeconds store",
            "late samples (next frame's time update already run, marker %s): %s"
            % (self.poller.marker_name or "none available", "kept" if self.opts.keep_late else "dropped"),
            "pressed_jump is read early in the frame's UWorld::Tick and can miss a press the controller "
            "has already consumed",
        ]
        if not self.poller.bindings_seen:
            notes.append(
                "warning: the key bindings were not read in a confirmed window (no window was long enough); "
                "they are the ones the first look at the game found"
            )
        sampler = self.sess.sampler
        optional = sampler.optional_fields()
        if optional:
            notes.append(
                "optional fields (members beyond raw version 1, listed in optional_fields; a record lacks "
                "one whose value could not be read or checked): %s" % ", ".join(optional))
        for name in sampler.opt_missing:
            notes.append("optional group %s: not in the layout, not recorded" % name)
        for name, why in sorted(sampler.opt_off.items()):
            notes.append("optional group %s: check failed, not recorded (%s)" % (name, why))
        notes.extend(self.opts.note or [])
        return core.make_header(
            self.sess.layout,
            sampler.bindings,
            scenario=self.opts.scenario,
            launch_options=self.opts.launch_options,
            timing=timing,
            notes=notes,
            recorder=RECORDER,
            sample_point=SAMPLE_POINT,
            optional_fields=optional,
        )

    def _sink(self, rec):
        if self.done is not None:
            return
        out = []
        if not self.header_written:
            header = self.header()
            out.append(core.dumps(header))
            self.header_written = True
            self.header_optional = header.get("optional_fields", [])
            self.header_bindings = header["bindings"]
            self.header_bindings_confirmed = self.poller.bindings_seen
            self.t_first = self.clock()
            self.first_frame = rec["frame"]
        if self.last_frame is not None and rec["frame"] != self.last_frame + 1:
            self.gaps += 1
        self.last_frame = rec["frame"]
        out.append(core.dumps(rec))
        with self.lock:
            self.lines.extend(out)
        self.records += 1
        if self.opts.frames and self.records >= self.opts.frames:
            self.done = "frame limit reached"

    def _flush(self):
        with self.lock:
            lines, self.lines = self.lines, []
        if lines and self.fh is not None:
            self.fh.write("\n".join(lines) + "\n")
            self.fh.flush()

    # -- status
    def status(self):
        wall = max(self.clock() - self.t_start, 1e-9)
        return {
            "recorder": RECORDER,
            "session": self.session_id,
            "state": self.state,
            "message": self.done,
            "failed": self.failed,
            "recorder_pid": os.getpid(),
            "game_pid": self.sess.target.pid,
            "scenario": self.opts.scenario,
            "file": os.path.basename(self.path) if self.path else None,
            "frames_limit": self.opts.frames or None,
            "seconds_limit": self.opts.seconds or None,
            "records": self.records,
            "gaps": self.gaps,
            "first_frame": self.first_frame,
            "last_frame": self.last_frame,
            "layout": self.sess.layout.id,
            "benchmarking": self.sess.timing.get("benchmarking"),
            "late_marker": self.poller.marker_name,
            "bindings": len(self.header_bindings) if self.header_bindings is not None else None,
            "bindings_confirmed": self.header_bindings_confirmed,
            "elapsed_s": round(wall, 3),
            "recorder_cpu_percent": round(100.0 * (time.process_time() - self.cpu0) / wall, 1),
            "stats": self.stats.as_dict(),
            "optional": optional_report(self.sess.sampler),
            "optional_in_header": self.header_optional,
            "markers": [dict(m) for m in list(self.markers)],
            "markers_omitted": self.markers_omitted,
            "updated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        }

    def write_status(self):
        try:
            write_json_atomic(os.path.join(self.ctl_dir, "status.json"), self.status())
        except OSError:
            pass

    def take_marks(self):
        """Turns the marker requests of ``mark`` (files in the control
        folder) into markers: each gets the number of the last recorded
        frame, so a scenario boundary can be found in the recording later.
        Runs on the helper thread; nothing here touches the game."""
        try:
            names = sorted(n for n in os.listdir(self.ctl_dir) if n.startswith(MARK_PREFIX) and n.endswith(".json"))
        except OSError:
            return
        for n in names:
            path = os.path.join(self.ctl_dir, n)
            req = read_json(path)
            try:
                os.remove(path)
            except OSError:
                continue  # still being written, or gone: the next pass sees it
            if not isinstance(req, dict):
                continue
            mark = {
                "name": marker_name(req.get("name", "")),
                "id": str(req.get("id", ""))[:80],
                "frame": self.last_frame,
                "records": self.records,
                "state": self.state,
                "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            }
            if len(self.markers) >= MAX_MARKERS:
                self.markers_omitted += 1
            else:
                self.markers.append(mark)
            print("asamu-win: marker %r at frame %s (%d records so far)" % (mark["name"], mark["frame"], mark["records"]))
            sys.stdout.flush()
            self.write_status()

    def _helper(self):
        n = 0
        while not self._thread_stop.wait(0.25):
            try:
                self._flush()
                self.take_marks()
                if any(os.path.exists(p) for p in self.stop_files):
                    self.stop_flag = True
            except OSError as e:  # e.g. the disk is full
                self.stats.last_error = "file: %s" % e
                self.failed = True
                self.stop_flag = True
            n += 1
            if n % 4 == 0:
                try:
                    self.write_status()
                except Exception as e:  # a status that cannot be written never ends a recording
                    self.stats.last_error = "status: %s" % e

    # -- main loop
    def run(self):
        opts = self.opts
        poller = self.poller
        t = self.sess.target
        self.state = "waiting-for-player"
        self.write_status()
        self._thread = threading.Thread(target=self._helper, name="asamu-files", daemon=True)
        self._thread.start()
        gc_was = gc.isenabled()
        gc.disable()  # no collector pauses inside the polling loop
        lost = 0
        idle = 0
        try:
            while self.done is None:
                ev = poller.poll()
                if ev == IDLE:
                    idle += 1
                    if not poller.has_player:
                        self.sleep(0.002)  # menu or loading: nothing to time
                        self._limits()
                    elif idle & 0x3FF == 0:
                        self._limits()  # also when the game stands still
                    elif idle == 1 and opts.cpu_saver:
                        self._nap()
                    continue
                idle = 0
                if ev == LOST:
                    lost += 1
                    if not t.alive():
                        self.done = "the game exited"
                        break
                    if lost >= MAX_ERRORS:
                        self.done = "the game's memory cannot be read"
                        self.failed = True
                        break
                    self.sleep(0.005)
                    continue
                lost = 0
                if self.records and self.state != "recording":
                    self.state = "recording"
                self._limits()
        except KeyboardInterrupt:
            self.done = "interrupted"
        except Exception as e:  # keep what was recorded and say what happened
            self.done = "internal error: %s: %s" % (type(e).__name__, e)
            self.failed = True
        finally:
            if gc_was:
                gc.enable()
        if self.done != "frame limit reached":
            saved, self.done = self.done, None
            poller.flush()  # the last sample, without its input (it ends the run)
            self.done = saved
        self.finish()

    def _limits(self):
        """Ends the recording when a limit, the STOP file or an error asks for it."""
        opts = self.opts
        if self.done is not None:
            return
        if self.poller.fatal:
            self.done = self.poller.fatal
            self.failed = True
        elif self.stats.error_run >= MAX_ERRORS:
            self.done = "too many failed samples in a row"
            self.failed = True
        elif self.stop_flag:
            self.done = "file error" if self.failed else "STOP file found"
        elif opts.seconds and self.t_first is not None and self.clock() - self.t_first >= opts.seconds:
            self.done = "time limit reached"
        elif opts.timeout and self.clock() - self.t_start >= opts.timeout:
            self.done = "timeout (%g s) reached" % opts.timeout

    def _nap(self):
        """With --cpu-saver: sleep through most of the wait between a frame
        end and the next tick, when recent frames show a long, steady wait."""
        w = self.stats.window_ms
        if self.poller.tick_rts is not None or w.n < 30 or len(w.recent) < 30:
            return
        recent = list(w.recent)[-30:]
        spare = min(recent) - self.opts.guard_ms - (self.clock() - (self.poller.t_end or 0)) * 1e3
        if spare > 1.0 and self.stats.missed == 0 and self.stats.unconfirmed <= 2:
            self.sleep(spare / 1e3)

    def finish(self):
        self._thread_stop.set()
        if self._thread is not None:
            self._thread.join(5)
        try:
            self._flush()
        except OSError as e:
            self.stats.last_error = "file: %s" % e
            self.failed = True
        if self.fh is not None:
            self.fh.close()
            self.fh = None
        if (
            self.header_written and not self.header_bindings_confirmed and self.poller.bindings_seen
            and list(self.sess.sampler.bindings or []) != self.header_bindings
        ):
            # The header went out with the first look's table, and a window
            # later showed another one: the keys of this file would be
            # mapped through the wrong bindings.
            self.failed = True
            self.done = "the key bindings in the header differ from the ones read later in a window (%s)" % self.done
        self.state = "failed" if self.failed else "finished"
        if self.path and not self.header_written:
            try:
                os.remove(self.path)  # nothing was recorded: leave no empty file
            except OSError:
                pass
            self.path = None
        if self.path:
            write_json_atomic(self.path[: -len(".raw.jsonl")] + ".stats.json", self.status())
            if self.opts.convert and self.records >= 2:
                try:
                    self.outputs = core.convert_file(self.path)
                except Exception as e:  # keep the raw file; convert later
                    self.stats.last_error = "conversion: %s" % e
        self.write_status()


# ------------------------------------------------------------------- commands


def load_layout(path=None):
    path = path or os.path.join(HERE, LAYOUT_FILE)
    return core.Layout.load(path), path


def attach(opts, ignore_sentinels=False):
    """(Session, None) for the running game, or (None, message)."""
    layout, layout_path = load_layout(opts.layout)
    optional = note = None
    if not getattr(opts, "raw_v1", False):
        optional = True
        note = load_optional(layout, layout_path, getattr(opts, "optional_layout", None))
    target = open_game(pid=opts.pid, base=opts.base)
    if target is None:
        return None, "%s is not running" % EXE_NAME
    try:
        session = Session(target, layout, layout_path, ignore_sentinels=ignore_sentinels, optional=optional)
    except Exception:
        target.close()
        raise
    session.optional_note = note
    return session, None


def describe(session, out, keep_failures=True):
    """The read-only report of ``check`` (also the first lines of a start).
    Returns (ok, sample record or None)."""
    target, layout, sampler = session.target, session.layout, session.sampler
    ok = True
    out.append("%s; layout %s (%d symbols)" % (RECORDER, layout.id, len(session.addrs)))
    out.append(
        "game: pid %d, module base 0x%08X (preferred %s); header time stamp %d and image size %d match the layout"
        % (target.pid, target.base, (layout.data.get("image") or {}).get("image_base"), session.image[0], session.image[1])
    )
    if target.access is not None:
        out.append(
            "handle: access 0x%04X granted (asked for 0x%04X: read memory + query information; nothing else)"
            % (target.access, GAME_ACCESS)
        )
    t = session.timing
    out.append(
        "GIsBenchmarking=%s GUseFixedTimeStep=%s GFixedDeltaTime=%r GDeltaTime=%r"
        % (t["benchmarking"], t["fixed_step"], t["fixed_delta_time"], t["delta_time"])
    )
    first = sampler.names.name(0, 0)
    ok &= first == "None"
    out.append("FName::Names[0]=%r (expected 'None'); %d names" % (first, sampler.names.count))
    rec, why = None, None
    for attempt in range(4):
        # The game is running while this first look is taken: a read can
        # catch an object in the middle of a tick, so look again before
        # believing a failed check.
        try:
            rec, why = sampler.sample(sampler.frame_counter())
        except (core.ReadError, struct.error) as e:
            rec, why = None, "memory read failed: %s" % e
        if rec is not None or why in ("no-player", "paused"):
            break
        if attempt < 3:
            forget_sentinel_failures(sampler)
            time.sleep(0.03)
    if rec is None:
        out.append("player: not sampled (%s)" % why)
        ok &= why in ("no-player", "paused")
        for f in sampler.sentinel_failures:
            out.append("  layout check failed: %s" % f)
        if keep_failures is False:
            forget_sentinel_failures(sampler)
    else:
        p = rec["player"]
        w = rec["world"]
        out.append("map %s; controller %s; pawn %s" % (w["map"], p["controller_class"], p["pawn_class"]))
        out.append("location %r velocity %r physics %d base %r" % (p["location"], p["velocity"], p["physics"], p["base"]))
        out.append("view rotation %r; fov camera %r controller %r" % (p["view_rotation"], p["fov_camera"], p["fov_controller"]))
        out.append("keys %r; gun %r" % (p["keys"], p["gun"]))
        out.append(
            "world time %.3f s, real time %.3f s, DeltaSeconds %r, TimeDilation %r"
            % (w["time_seconds"], w["real_time_seconds"], w["delta_seconds"], w["time_dilation"])
        )
        checked = sorted(set(s["object"] for s in layout.sentinels()))
        seen = sorted(role for role, _ptr in sampler.checked)
        out.append(
            "bindings read: %d; layout sentinels: ok (%s checked of %s)"
            % (len(sampler.bindings or []), ", ".join(seen) or "none", ", ".join(checked))
        )
        describe_optional(session, p, out)
    return ok, rec


def describe_optional(session, p, out):
    """The optional fields of one sample (``check``): their values, and which
    groups are on, missing from the layout, or off after a failed check. A
    group that is off does not fail the check: the recording goes on
    without it."""
    sampler = session.sampler
    note = getattr(session, "optional_note", None)
    if note:
        out.append(note)
    if not sampler.opt_on and not sampler.opt_missing:
        out.append("optional fields: none (plain version-1 records)")
        return
    gun = p.get("gun") or {}
    out.append("base level %r; floor %r; cylinder %r" % (p.get("base_level"), p.get("floor"), p.get("cylinder")))
    out.append("fov default %r locked %r lock %r; camera pov %r"
               % (p.get("fov_default"), p.get("fov_locked"), p.get("fov_lock"), p.get("camera_pov")))
    out.append("base eye height %r; walk bob %r; bob %r" % (p.get("base_eye_height"), p.get("walk_bob"), p.get("bob")))
    out.append("gun state %r; timers %r" % (gun.get("state"), gun.get("timers")))
    rep = optional_report(sampler)
    out.append(
        "optional fields on: %s; not in the layout: %s; off: %s; left out of this sample: %s"
        % (", ".join(rep["fields"]) or "none", ", ".join(rep["not_in_layout"]) or "none",
           "; ".join("%s (%s)" % kv for kv in rep["off"].items()) or "none",
           "; ".join("%s (%s)" % (k, v["last_reason"]) for k, v in rep["left_out"].items()) or "none")
    )


def probe(session, frames, seconds, keep_late=False, clock=time.perf_counter, sleep=time.sleep):
    """Runs the sampling loop without writing anything; returns
    (Stats, records, counter advance, wall seconds, cpu seconds)."""
    got = []
    poller = Poller(session, got.append, keep_late=keep_late, clock=clock)
    c0 = session.target.u64(session.a_counter)
    t0 = clock()
    cpu0 = time.process_time()
    gc_was = gc.isenabled()
    gc.disable()
    try:
        while len(got) < frames and clock() - t0 < seconds:
            ev = poller.poll()
            if ev == LOST:
                if not session.target.alive():
                    break
                sleep(0.005)
            elif ev == IDLE and not poller.has_player:
                sleep(0.002)
    finally:
        if gc_was:
            gc.enable()
    wall = clock() - t0
    c1 = session.target.u64(session.a_counter)
    advance = None if c0 is None or c1 is None else c1 - c0
    return poller.stats, got, advance, wall, time.process_time() - cpu0


def cmd_check(opts):
    try:
        session, why = attach(opts)
    except (WinError, BuildMismatch, core.LayoutError, core.ReadError) as e:
        print("check failed: %s" % e)
        return 1
    if session is None:
        print(why)
        return 2
    out = []
    try:
        raise_own_priority(opts.priority)
        ok, rec = describe(session, out)
        stats, got, advance, wall, cpu = probe(session, opts.probe_frames, opts.probe_seconds, keep_late=opts.keep_late)
        if advance is None:
            out.append("GFrameCounter: unreadable")
            ok = False
        else:
            ok &= advance > 0
            out.append(
                "GFrameCounter advanced by %d in %.2f s (%.1f frames per second)%s"
                % (advance, wall, advance / wall if wall > 0 else 0.0, "" if advance > 0 else "  <-- not running?")
            )
        out.append("probe (nothing written): " + stats.line())
        out.append(
            "probe: recorder CPU %.0f%% of one core; late marker %s"
            % (100.0 * cpu / wall if wall > 0 else 0.0, session.late_marker()[1] or "none")
        )
        if len(got) >= 2:
            dts = [r["world"]["delta_seconds"] for r in got[1:]]
            args = [r["dt_arg"] for r in got if r.get("dt_arg") is not None]
            # Frames whose tick argument repeats the previous frame's exactly:
            # there the late marker (GDeltaTime) would not show a time update.
            # (Compared as the float the tick got; an upper bound for the
            # double the marker is.)
            same = sum(1 for a, b in zip(got, got[1:])
                       if b["frame"] == a["frame"] + 1 and a.get("dt_arg") is not None and a.get("dt_arg") == b.get("dt_arg"))
            out.append(
                "probe: %d records, frames %d..%d; DeltaSeconds %.6f..%.6f; dt_arg read on %d, "
                "%d equal to the frame before"
                % (len(got), got[0]["frame"], got[-1]["frame"], min(dts), max(dts), len(args), same)
            )
        if rec is not None and stats.sampled == 0 and stats.frames > 10:
            out.append("probe: no consistent sample in %d frames (see torn/unconfirmed/late above)" % stats.frames)
            ok = False
        out.append("result: %s" % ("ok" if ok else "FAILED"))
    except (core.ReadError, struct.error) as e:
        out.append("check failed: memory read failed: %s" % e)
        ok = False
    finally:
        session.target.close()
    print("\n".join(out))
    return 0 if ok else 1


def resolve_dirs(opts):
    out_dir = os.path.abspath(opts.out or default_out_dir())
    ctl_dir = os.path.abspath(opts.ctl or os.path.join(out_dir, "ctl"))
    return out_dir, ctl_dir


def enclosing_install(path):
    """The game install that ``path`` lies in, judged by the files on disk:
    the nearest folder at or above it that holds ``Binaries/Win32/<the
    executable>``. None when there is none. Needs no running game, and
    finds any copy of the game, whatever its folder is called."""
    p = os.path.realpath(path)
    while True:
        if os.path.isfile(os.path.join(p, "Binaries", "Win32", EXE_NAME)):
            return p
        parent = os.path.dirname(p)
        if parent == p:
            return None
        p = parent


def guard_on_disk(out_dir, ctl_dir):
    """Error text when a folder lies inside a game install found on disk,
    else None."""
    for d in (out_dir, ctl_dir):
        root = enclosing_install(d)
        if root:
            return "folder %s is inside the game install %s; pass --out/--ctl elsewhere" % (d, root)
    return None


def guard_folders(out_dir, ctl_dir):
    """The check that needs no process (the game is not running yet): a
    folder inside an install found on disk, or named like one, is refused."""
    bad = guard_on_disk(out_dir, ctl_dir)
    if bad:
        return bad
    for d in (out_dir, ctl_dir):
        if "a story about my uncle" in d.lower():
            return "folder %s looks like the game install; pass --out/--ctl elsewhere" % d
    return None


def guard_output(out_dir, ctl_dir, exe_path):
    """Error text when a folder lies inside the game install, else None."""
    bad = guard_on_disk(out_dir, ctl_dir)
    if bad:
        return bad
    if not exe_path:
        return "cannot tell where the game is installed; refusing to write anything"
    root = win_install_root(exe_path)
    for d in (out_dir, ctl_dir):
        if core.is_inside(d, root):
            return "folder %s is inside the game install %s; pass --out/--ctl elsewhere" % (d, root)
    return None


def running_recorder(ctl_dir):
    st = read_json(os.path.join(ctl_dir, "status.json"))
    if st and st.get("state") in ACTIVE_STATES and pid_alive(st.get("recorder_pid")):
        return st
    return None


def summary_line(st):
    s = st.get("stats")
    if s is None:  # a status written before any recording existed
        return "asamu-win: %s%s" % (st.get("state"), " (%s)" % st["message"] if st.get("message") else "")
    line = (
        "asamu-win: %s%s; %s records, %s gaps, file %s; torn %s, unconfirmed %s, late %s, missed %s, "
        "resyncs %s, errors %s, skipped %r; fps %s; recorder CPU %s%%"
        % (
            st.get("state"),
            " (%s)" % st["message"] if st.get("message") else "",
            st.get("records"), st.get("gaps"), st.get("file"),
            s.get("torn"), s.get("unconfirmed"), s.get("late"), s.get("missed"), s.get("resyncs"), s.get("errors"),
            s.get("skipped"), s.get("fps"), st.get("recorder_cpu_percent"),
        )
    )
    opt = st.get("optional")
    if opt:
        line += "; optional fields %d" % len(opt.get("fields") or [])
        if opt.get("off") or opt.get("not_in_layout"):
            line += " (off: %s)" % ", ".join(sorted(list(opt.get("off") or {}) + list(opt.get("not_in_layout") or [])))
    marks = st.get("markers")
    if marks:
        line += "; %d markers, last %r at frame %s" % (len(marks), marks[-1].get("name"), marks[-1].get("frame"))
    return line


def spawn_detached(argv, log_path):
    """Starts this script again as a process that outlives the caller's
    session (an SSH session ends its job; the recorder leaves the job)."""
    cmd = [sys.executable, "-I", "-B", os.path.abspath(__file__)] + list(argv)
    with open(log_path, "ab") as log:
        kwargs = dict(stdin=subprocess.DEVNULL, stdout=log, stderr=log, close_fds=True)
        if is_windows():
            flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
            try:
                return subprocess.Popen(cmd, creationflags=flags | CREATE_BREAKAWAY_FROM_JOB, **kwargs)
            except OSError:
                return subprocess.Popen(cmd, creationflags=flags, **kwargs)
        return subprocess.Popen(cmd, start_new_session=True, **kwargs)


def cmd_start(opts, argv):
    out_dir, ctl_dir = resolve_dirs(opts)
    other = running_recorder(ctl_dir)
    if other and other.get("session") != opts.session:
        print("a recording is running (recorder pid %s, %s); stop it first" % (other.get("recorder_pid"), other.get("file")))
        return 1
    if opts.detach:
        return start_detached(opts, argv, out_dir, ctl_dir)
    status_path = os.path.join(ctl_dir, "status.json")

    def early(state, message, failed=False):
        # Before a Recording exists: tell `status` and a detached parent.
        try:
            os.makedirs(ctl_dir, exist_ok=True)
            write_json_atomic(status_path, {
                "recorder": RECORDER, "session": opts.session, "state": state, "message": message,
                "failed": failed, "recorder_pid": os.getpid(), "scenario": opts.scenario,
                "updated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            })
        except OSError:
            pass

    session = None
    deadline = time.perf_counter() + opts.wait_game
    try:
        while True:
            try:
                session, why = attach(opts, ignore_sentinels=opts.ignore_sentinels)
            except (core.ReadError, WinError) as e:
                if time.perf_counter() >= deadline:
                    raise
                session, why = None, "the game is starting (%s)" % e  # e.g. no module list yet
            if session is not None:
                break
            if time.perf_counter() >= deadline:
                print(why)
                if opts.session:
                    early("failed", why, True)
                return 2
            # Never create a folder before the install location is known.
            if opts.session:
                early("waiting-for-game", why)
            time.sleep(1.0)
    except (WinError, BuildMismatch, core.LayoutError, core.ReadError) as e:
        print("start failed: %s" % e)
        if opts.session:
            early("failed", str(e), True)
        return 1
    try:
        bad = guard_output(out_dir, ctl_dir, session.target.exe_path)
        if bad:
            print("start refused: %s" % bad)
            return 1
        os.makedirs(out_dir, exist_ok=True)
        os.makedirs(ctl_dir, exist_ok=True)
        for p in (os.path.join(ctl_dir, "STOP"), os.path.join(out_dir, "STOP")):
            if os.path.exists(p):
                os.remove(p)
        raise_own_priority(opts.priority)
        lines = []
        try:
            describe(session, lines, keep_failures=False)  # the recording checks again, in a window
        except (core.ReadError, struct.error) as e:
            lines.append("first look failed (%s); the recording will retry" % e)
        print("\n".join(lines))
        rec = Recording(session, opts, out_dir, ctl_dir)
        rec.open()
        msg = "asamu-win: recording to %s" % rec.path
        if session.timing.get("benchmarking"):
            msg += " (fixed step %r s)" % session.timing.get("fixed_delta_time")
        else:
            msg += " (variable frame lengths: tick_rate null)"
        print(msg + "; create %s to stop" % rec.stop_files[0])
        sys.stdout.flush()
        rec.run()
        st = rec.status()
        print(summary_line(st))
        print("asamu-win: " + rec.stats.line())
        if rec.path:
            print("asamu-win: raw recording %s" % rec.path)
        else:
            print("asamu-win: nothing recorded, no file written")
        for p in rec.outputs:
            print("asamu-win: trace %s" % p)
        return 1 if rec.failed else 0
    finally:
        session.target.close()


def start_detached(opts, argv, out_dir, ctl_dir):
    # The child repeats every check; here only the folders are needed, and
    # they must not be created inside the install either.
    try:
        target = open_game(pid=opts.pid, base=opts.base)
    except WinError as e:
        print("start failed: %s" % e)
        return 1
    if target is not None:
        bad = guard_output(out_dir, ctl_dir, target.exe_path)
        target.close()
    else:
        # The game is not running yet (--wait-game): judge the folders by
        # what is on disk. The child checks again against the running game.
        bad = guard_folders(out_dir, ctl_dir)
    if bad:
        print("start refused: %s" % bad)
        return 1
    os.makedirs(ctl_dir, exist_ok=True)
    session_id = "%s-%d" % (utc_stamp(), os.getpid())
    child_argv = [a for a in argv if a != "--detach"] + ["--session", session_id]
    log_path = os.path.join(ctl_dir, "recorder.log")
    proc = spawn_detached(child_argv, log_path)
    status_path = os.path.join(ctl_dir, "status.json")
    deadline = time.time() + 20.0
    st = None
    while time.time() < deadline:
        st = read_json(status_path)
        if st and st.get("session") == session_id and st.get("state") != "starting":
            break
        if proc.poll() is not None:
            st = read_json(status_path)
            break
        time.sleep(0.1)
    if st and st.get("session") == session_id:
        print("asamu-win: recorder started detached (pid %d, session %s)" % (proc.pid, session_id))
        print(summary_line(st))
        return 1 if st.get("failed") else 0
    print("asamu-win: the detached recorder did not report (exit code %r); see %s" % (proc.poll(), log_path))
    return 1


def cmd_status(opts):
    _out_dir, ctl_dir = resolve_dirs(opts)
    st = read_json(os.path.join(ctl_dir, "status.json"))
    if st is None:
        print("asamu-win: no recording (no status in %s)" % ctl_dir)
        return 0
    if st.get("state") in ACTIVE_STATES and not pid_alive(st.get("recorder_pid")):
        st["state"] = "stale (the recorder process is gone; last state %s)" % st.get("state")
    if opts.json:
        print(json.dumps(st, indent=1, sort_keys=True))
    else:
        print(summary_line(st))
        for m in st.get("markers") or []:
            print("  marker %r: frame %s, record %s, %s" % (m.get("name"), m.get("frame"), m.get("records"), m.get("utc")))
    return 0


def cmd_stop(opts):
    out_dir, ctl_dir = resolve_dirs(opts)
    st = running_recorder(ctl_dir)
    if st is None:
        print("asamu-win: no recording is running")
        return cmd_status(opts)
    stop = os.path.join(ctl_dir, "STOP")
    with open(stop, "w", encoding="utf-8") as fh:
        fh.write("stop\n")
    deadline = time.time() + opts.wait
    while time.time() < deadline:
        if running_recorder(ctl_dir) is None:
            break
        time.sleep(0.1)
    else:
        print("asamu-win: the recorder (pid %s) did not stop within %g s" % (st.get("recorder_pid"), opts.wait))
        return 1
    try:
        os.remove(stop)
    except OSError:
        pass
    return cmd_status(opts)


def cmd_mark(opts):
    """Sets a named marker in the running recording: a request file in the
    control folder, which the recorder turns into an entry of its status and
    stats files and a line of its log. Nothing is sent to the game."""
    out_dir, ctl_dir = resolve_dirs(opts)
    st = running_recorder(ctl_dir)
    if st is None:
        print("asamu-win: no recording is running; no marker set")
        return 1
    bad = guard_folders(out_dir, ctl_dir)
    if bad:
        print("mark refused: %s" % bad)
        return 1
    name = marker_name(" ".join(opts.name))
    ident = "%s-%d-%09d" % (utc_stamp(), os.getpid(), time.time_ns() % 10 ** 9)
    path = os.path.join(ctl_dir, "%s%s.json" % (MARK_PREFIX, ident))
    write_json_atomic(path, {"name": name, "id": ident})
    deadline = time.time() + opts.wait
    while time.time() < deadline:
        st = read_json(os.path.join(ctl_dir, "status.json")) or {}
        for m in st.get("markers") or []:
            if m.get("id") == ident:
                print("asamu-win: marker %r at frame %s (%s records so far, file %s)"
                      % (m.get("name"), m.get("frame"), m.get("records"), st.get("file")))
                return 0
        time.sleep(0.05)
    print("asamu-win: the recorder did not take the marker %r within %g s" % (name, opts.wait))
    return 1


def cmd_v1view(opts):
    """Writes a copy of each raw recording without the optional fields (for
    a reader of plain version-1 files) into ``--out-dir``, default a folder
    ``v1`` next to the recording; the copy keeps the file name. Works on
    any machine."""
    status = 0
    for raw in opts.raw:
        out_dir = os.path.abspath(opts.out_dir or os.path.join(os.path.dirname(os.path.abspath(raw)), "v1"))
        out = os.path.join(out_dir, os.path.basename(raw))
        try:
            os.makedirs(out_dir, exist_ok=True)
            n = core.write_v1_view(raw, out)
        except FileExistsError:
            print("exists, left as it is: %s" % out)
            continue
        except (OSError, ValueError, KeyError) as e:
            print("v1view failed for %s: %s: %s" % (raw, type(e).__name__, e))
            status = 1
            continue
        print("%s (%d records)" % (out, n))
    return status


def _int(text):
    return int(text, 0)


def parser():
    ap = argparse.ArgumentParser(prog="asamu_win.py", description="Read-only trace recorder for the Win32 build")
    sub = ap.add_subparsers(dest="cmd")

    def common(p):
        p.add_argument("--layout", default=None, help="layout file (default: %s next to this script)" % LAYOUT_FILE)
        p.add_argument("--pid", type=int, default=None, help="process id (default: find %s)" % EXE_NAME)
        p.add_argument("--base", type=_int, default=None, help="module base override (tests against a stand-in)")
        p.add_argument("--keep-late", action="store_true",
                       help="keep samples read after the next frame's time update (default: drop and count them)")
        p.add_argument("--priority", choices=("normal", "above", "high"), default="above",
                       help="scheduling of the recorder's own process (default above normal)")
        p.add_argument("--raw-v1", action="store_true",
                       help="plain version-1 records: read and write no optional field")
        p.add_argument("--optional-layout", default=None,
                       help="optional-field layout (default: data/win32/%s next to the layout, if there)" % OPTIONAL_FILE)

    def dirs(p):
        p.add_argument("--out", default=None, help="output folder (default: traces next to this script)")
        p.add_argument("--ctl", default=None, help="control folder (default: <out>/ctl)")

    c = sub.add_parser("check", help="attach read-only, check the layout on the live objects, probe the sampling; records nothing")
    common(c)
    c.add_argument("--probe-frames", type=int, default=120, help="frames of the sampling probe (default 120)")
    c.add_argument("--probe-seconds", type=float, default=3.0, help="longest probe (default 3 s)")

    s = sub.add_parser("start", help="record a raw recording until a limit, the STOP file or the game's exit")
    common(s)
    dirs(s)
    s.add_argument("--scenario", default=None, help="scenario id, e.g. T1 or A4-0.2")
    s.add_argument("--frames", type=int, default=0, help="stop after this many recorded frames")
    s.add_argument("--seconds", type=float, default=0.0, help="stop this long after the first recorded frame")
    s.add_argument("--timeout", type=float, default=1800.0,
                   help="give up this long after the start, recorded or not (default 1800 s; 0 = never)")
    s.add_argument("--wait-game", type=float, default=0.0, help="wait this long for the game to be started")
    s.add_argument("--launch-options", default=None, help="the Steam launch options in use (recorded only)")
    s.add_argument("--note", action="append", default=[], help="free-form note (repeatable)")
    s.add_argument("--ignore-sentinels", action="store_true", help="record even if the layout check fails")
    s.add_argument("--convert", action="store_true", help="also write canonical trace(s) with the Python converter")
    s.add_argument("--cpu-saver", action="store_true",
                   help="sleep through the wait between frames instead of spinning (may lose frames)")
    s.add_argument("--guard-ms", type=float, default=3.0, help="with --cpu-saver: wake this long before the next tick")
    s.add_argument("--detach", action="store_true", help="run the recorder as a detached process and return")
    s.add_argument("--session", default=None, help=argparse.SUPPRESS)

    for name, text in (("status", "print the last status"), ("stop", "end the running recording")):
        p = sub.add_parser(name, help=text)
        dirs(p)
        p.add_argument("--json", action="store_true", help="print the whole status as JSON")
        if name == "stop":
            p.add_argument("--wait", type=float, default=15.0, help="seconds to wait for the recorder to finish")

    m = sub.add_parser("mark", help="set a named marker in the running recording (status, stats and log; not in the game)")
    dirs(m)
    m.add_argument("name", nargs="+", help="marker name, e.g. zoom-hold-1")
    m.add_argument("--wait", type=float, default=5.0, help="seconds to wait for the recorder to take it")

    v = sub.add_parser("v1view", help="copy raw recordings without their optional fields (any OS)")
    v.add_argument("raw", nargs="+", help="raw recording(s)")
    v.add_argument("--out-dir", default=None, help="folder for the copies (default: v1 next to each recording)")
    return ap


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    ap = parser()
    opts = ap.parse_args(argv)
    if opts.cmd == "check":
        return cmd_check(opts)
    if opts.cmd == "start":
        return cmd_start(opts, argv)
    if opts.cmd == "status":
        return cmd_status(opts)
    if opts.cmd == "stop":
        return cmd_stop(opts)
    if opts.cmd == "mark":
        return cmd_mark(opts)
    if opts.cmd == "v1view":
        return cmd_v1view(opts)
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
