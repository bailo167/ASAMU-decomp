#!/usr/bin/env python3
"""Desk runner: start programs of this project in the desktop session of a
second machine, on request of the development tools.

A program started over SSH on Windows runs in a session without a desktop: it
can render offscreen, but it cannot put a window on the screen. To test and
play the recreation on that machine, a process has to run in the owner's own
desktop session. This is that process. The owner starts it by hand (a
double-click on `<root>\\start-runner.cmd`, once per sign-in); it shows a
console window, watches a folder for job files and runs them.

It is deliberately small and strict (Python standard library only):

* A job names ONE program with an ARGUMENT LIST. There is no command string
  and no shell anywhere. The program must be a regular file whose resolved
  path (symbolic links and junctions followed) is inside the root folder
  (`D:\\asamu` on the owner's laptop); on Windows it must be an `.exe`, and
  shells, script hosts and interpreters are refused by name. The working
  directory must resolve inside the root too.
* On Windows the start itself is checked as well, because a folder on the
  way can be swapped for a junction after the check: the working directory
  is held in place while the process is created, and the process stays
  suspended until the system says which file it was made from.
* Every job has a time limit. When it passes, the whole process tree of the
  job is ended (a Windows job object; a process group elsewhere). Nothing a
  job started outlives the job or the runner. The loop that enforces the
  limits never waits for the console window, and does a bounded amount of
  work per round however full the inbox is.
* At most a few jobs run at once; a few more wait in a queue.
* Job files that are too large, malformed or that try to leave the root are
  refused, and the refusal is shown and written down. No job file, however
  odd, ends the runner.
* The runner opens no network port, and writes only inside `<root>/runner`,
  never through a link: a link where one of its files goes is removed or
  refused, and on Windows its folders are held in place while it runs.
* It writes a heartbeat file, stops when `<root>/runner/stop` appears or when
  its window is closed, and refuses to start twice.

What it is NOT: a sandbox. Whoever can put job files in the inbox can also
put a program under the root. The rules above keep the tooling honest (no
"cmd /c ...", nothing outside the project's folder, nothing left running);
they do not defend against the account that owns the folder.

Protocol (version 1), all below `<root>/runner`:

  inbox/<id>.json          a job; written as <id>.json.tmp and then renamed.
                           {"version": 1, "program": "<absolute path>",
                            "args": ["..."], "cwd": "<absolute path>",
                            "timeout_seconds": 900, "env": {"ASAMU_X": "1"}}
  inbox/<id>.cancel        ends job <id> (running or waiting; one that comes
                           before its job is remembered for two minutes)
  outbox/<id>.status.json  state (queued, running, finished, timed_out,
                           cancelled, rejected, failed_to_start, output_limit,
                           runner_stopped), exit code, reason, start and end
  outbox/<id>.stdout.txt   what the program wrote
  outbox/<id>.stderr.txt
  heartbeat.json           rewritten every two seconds while the runner lives
  runner.lock              held (locked) by the running runner
  stop                     makes the runner end its jobs and exit
  runner.log, rejected.log what the console showed; every refusal

Commands:

  run --root ROOT                      the runner itself (the owner starts it)
  submit --root ROOT ... -- ARGS       enqueue a job, wait, stream its output,
                                       exit with its exit code (124 timed out,
                                       125 runner problem or refusal, 126 could
                                       not start, 130 cancelled); interrupting
                                       the wait cancels the job
  status / stop / cancel ID / install  see `--help`
  pack ... -- ARGS                     encode a job for `submit --packed`
                                       (used by tools/dev/win_host.sh)

Needs Python 3.10 or later. Tests: tools/dev/test_desk_runner.py.
"""

from __future__ import annotations

import argparse
import base64
import ctypes
import hashlib
import json
import math
import os
import re
import secrets
import signal
import stat
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from typing import Any, BinaryIO, Callable, TextIO

PROTOCOL_VERSION = 1

MAX_JOB_FILE_BYTES = 64 * 1024
MAX_PATH_CHARS = 1024
MAX_ARGS = 256
MAX_ARG_CHARS = 8192
MAX_COMMAND_CHARS = 30000
MAX_ENV_VARS = 32
MAX_ENV_VALUE_CHARS = 4096
DEFAULT_TIMEOUT_S = 900.0
MAX_TIMEOUT_S = 8 * 3600.0
DEFAULT_MAX_JOBS = 2
HARD_MAX_JOBS = 4
MAX_QUEUED = 16
MAX_OUTPUT_BYTES = 64 * 1024 * 1024
MAX_PROCESSES_PER_JOB = 64
MAX_INBOX_ENTRIES_PER_SCAN = 64
MAX_INBOX_ENTRIES_EXAMINED = 4096
MAX_REMEMBERED_NAMES = 4096
EARLY_CANCEL_KEEP_S = 120.0
NOT_TAKEN_AFTER_S = 60.0
MAX_EARLY_CANCELS = 256
CONSOLE_BACKLOG_LINES = 2000
HEARTBEAT_PERIOD_S = 2.0
HEARTBEAT_STALE_S = 15.0
POLL_PERIOD_S = 0.25
KILL_GRACE_S = 5.0
RESULT_KEEP_S = 14 * 24 * 3600.0
LOG_ROTATE_BYTES = 4 * 1024 * 1024

EXIT_TIMED_OUT = 124
EXIT_RUNNER = 125
EXIT_NOT_STARTED = 126
EXIT_CANCELLED = 130

ID_RE = re.compile(r"\A[A-Za-z0-9][A-Za-z0-9_-]{0,63}\Z")
ENV_NAME_RE = re.compile(r"\A(ASAMU_[A-Z0-9_]{1,56}|WGPU_[A-Z0-9_]{1,56}|BEVY_[A-Z0-9_]{1,56}|RUST_BACKTRACE|RUST_LOG)\Z")
JOB_KEYS = frozenset({"version", "id", "program", "args", "cwd", "timeout_seconds", "env"})
REQUIRED_JOB_KEYS = ("version", "program", "args", "cwd", "timeout_seconds")
TERMINAL_STATES = frozenset(
    {"finished", "timed_out", "cancelled", "rejected", "failed_to_start", "output_limit", "runner_stopped"}
)

# Programs refused by name even when a copy sits under the root: shells, script
# hosts, interpreters, system tools that change the machine, the original game.
# A guard against mistakes (a renamed copy passes), not a sandbox.
DENIED_PROGRAM_STEMS = frozenset(
    """
    cmd command powershell powershell_ise pwsh conhost openconsole wt windowsterminal
    sh bash zsh dash ash ksh csh tcsh fish busybox env wsl wslhost
    py pyw python pythonw perl ruby node deno bun php lua luajit java javaw
    wscript cscript mshta rundll32 regsvr32 msiexec reg regedit regedt32 schtasks at sc
    net net1 netsh taskkill tskill shutdown logoff bcdedit powercfg diskpart format
    runas psexec psexec64 explorer start osascript open xdg-open
    ssh scp sftp curl wget bitsadmin certutil ftp tftp
    steam steamservice steamwebhelper asamu-win32-shipping udk udklift
    """.split()
)
_PYTHON_STEM_RE = re.compile(r"\Apython[0-9.]*w?\Z")
_WIN_RESERVED_NAMES = frozenset(
    ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
    + [f"COM{i}" for i in range(1, 10)]
    + [f"LPT{i}" for i in range(1, 10)]
)


class JobRejected(Exception):
    """A job that will not run; the text is the reason shown to both sides."""


def is_job_id(text: object) -> bool:
    """1 to 64 letters, digits, '-' or '_', and not the name of a device: on
    Windows a file called `nul.status.json` or `com1.stdout.txt` can be the
    device and not a file, whatever folder it is in."""
    return isinstance(text, str) and ID_RE.match(text) is not None and text.upper() not in _WIN_RESERVED_NAMES


BAD_JOB_ID = "job id must be 1 to 64 letters, digits, '-' or '_', and not a device name such as NUL or COM1"


class LaunchError(Exception):
    """The program passed the checks but could not be started or confined."""


class RunnerError(Exception):
    """The runner itself cannot start or go on."""


# --------------------------------------------------------------------------
# Text for people


def printable(value: object, limit: int = 240) -> str:
    """ASCII-only, single-line form of untrusted text for the console and logs."""
    raw = str(value)
    cut = raw[: limit + 1]
    text = cut.encode("ascii", "backslashreplace").decode("ascii")
    text = "".join(ch if 32 <= ord(ch) < 127 else f"\\x{ord(ch):02x}" for ch in text)
    if len(raw) > limit or len(text) > limit:
        text = text[:limit] + "..."
    return text


def show_command(argv: list[str], limit: int = 400) -> str:
    """An argument list for display only (it is never parsed again)."""
    parts = []
    for arg in argv:
        shown = printable(arg, 160)
        parts.append(f'"{shown}"' if shown == "" or " " in shown else shown)
    return printable(" ".join(parts), limit)


def utc_text(unix_seconds: float) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(unix_seconds))


# --------------------------------------------------------------------------
# Validation


@dataclass(frozen=True)
class Policy:
    """Which path rules apply: those of Windows, or those of Linux and macOS."""

    windows: bool

    @classmethod
    def host(cls) -> "Policy":
        return cls(windows=os.name == "nt")


def check_path_text(text: object, what: str, policy: Policy) -> str:
    """Refuse a path by its spelling alone, before the file system is asked.

    Only a plain absolute path on a local drive passes: no network (UNC) or
    device path, no `.` or `..` component, no alternate data stream, no
    reserved device name. Returns the path with the platform's separators.
    """
    if not isinstance(text, str):
        raise JobRejected(f"{what} must be a text string")
    if not text:
        raise JobRejected(f"{what} is empty")
    if len(text) > MAX_PATH_CHARS:
        raise JobRejected(f"{what} is longer than {MAX_PATH_CHARS} characters")
    if any(ord(ch) < 32 or ord(ch) == 127 for ch in text):
        raise JobRejected(f"{what} contains control characters")
    if policy.windows:
        norm = text.replace("/", "\\")
        if norm.startswith("\\\\"):
            raise JobRejected(f"{what} is a network (UNC) or device path; only a path on a local drive is allowed")
        if not re.match(r"^[A-Za-z]:\\", norm):
            raise JobRejected(f"{what} must be an absolute path with a drive letter")
        rest = norm[3:]
        if ":" in rest:
            raise JobRejected(f"{what} names an alternate data stream (':' after the drive letter)")
        for part in rest.split("\\"):
            if part == "":
                continue
            if part in (".", ".."):
                raise JobRejected(f"{what} contains a '{part}' component")
            if part[-1] in " .":
                raise JobRejected(f"{what} has a component that ends in a space or a dot")
            if any(ch in '<>"|?*' for ch in part):
                raise JobRejected(f"{what} contains a character that is not allowed in a file name")
            if part.split(".")[0].upper() in _WIN_RESERVED_NAMES:
                raise JobRejected(f"{what} names a reserved device")
        return norm
    if not text.startswith("/"):
        raise JobRejected(f"{what} must be an absolute path")
    if text.startswith("//"):
        raise JobRejected(f"{what} starts with two slashes (a network-style path)")
    for part in text.split("/"):
        if part in (".", ".."):
            raise JobRejected(f"{what} contains a '{part}' component")
    return text


def is_inside(path: str, folder: str, allow_equal: bool = False) -> bool:
    """Whether the resolved `path` is below the resolved `folder`."""
    p = os.path.normcase(path)
    f = os.path.normcase(folder)
    if p == f:
        return allow_equal
    try:
        return os.path.commonpath([p, f]) == f
    except ValueError:
        return False


def resolve_inside(text: object, root_real: str, what: str, policy: Policy, allow_root: bool) -> str:
    """Spelling check, then the real path (links and junctions followed), then
    the containment check against the root and the runner's own folder."""
    spelled = check_path_text(text, what, policy)
    try:
        real = os.path.realpath(spelled, strict=True)
    except (OSError, ValueError) as err:
        raise JobRejected(f"{what} does not exist or cannot be resolved ({type(err).__name__})") from None
    if not is_inside(real, root_real, allow_equal=allow_root):
        raise JobRejected(f"{what} is outside {root_real} once links are followed")
    if is_inside(real, os.path.join(root_real, "runner"), allow_equal=True):
        raise JobRejected(f"{what} is inside the runner's own folder")
    return real


def check_program_file(real: str, policy: Policy) -> None:
    """The resolved program must be a plain program file, and not a shell."""
    try:
        info = os.stat(real)
    except OSError as err:
        raise JobRejected(f"program cannot be examined ({type(err).__name__})") from None
    if not stat.S_ISREG(info.st_mode):
        raise JobRejected("program is not a regular file")
    name = os.path.basename(real).lower()
    stem = name
    if policy.windows:
        if not name.endswith(".exe"):
            raise JobRejected("program is not an .exe file (scripts and batch files are not run)")
        stem = name[: -len(".exe")]
    else:
        if not os.access(real, os.X_OK):
            raise JobRejected("program is not executable")
        try:
            with open(real, "rb") as handle:
                magic = handle.read(2)
        except OSError as err:
            raise JobRejected(f"program cannot be read ({type(err).__name__})") from None
        if magic == b"#!":
            raise JobRejected("program is a script (scripts are not run)")
    if stem in DENIED_PROGRAM_STEMS or _PYTHON_STEM_RE.match(stem):
        raise JobRejected(f"program '{printable(name, 60)}' is a shell, an interpreter or a system tool; those are not run")


@dataclass(frozen=True)
class Job:
    """A job that passed every check. `program` and `cwd` are resolved paths."""

    job_id: str
    program: str
    args: tuple[str, ...]
    cwd: str
    timeout_seconds: float
    env: tuple[tuple[str, str], ...] = ()

    @property
    def argv(self) -> list[str]:
        return [self.program, *self.args]


def _no_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for key, value in pairs:
        if key in out:
            raise ValueError(f"duplicate key {key!r}")
        out[key] = value
    return out


def _no_constants(name: str) -> Any:
    raise ValueError(f"{name} is not allowed")


def parse_job_bytes(data: bytes) -> dict[str, Any]:
    """Job file bytes to a dictionary, or `JobRejected`."""
    if len(data) > MAX_JOB_FILE_BYTES:
        raise JobRejected(f"job file is larger than {MAX_JOB_FILE_BYTES} bytes")
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        raise JobRejected("job file is not UTF-8 text") from None
    try:
        obj = json.loads(text, object_pairs_hook=_no_duplicate_keys, parse_constant=_no_constants)
    except RecursionError:
        raise JobRejected("job file is nested too deeply") from None
    except ValueError as err:
        raise JobRejected(f"job file is not valid JSON ({printable(err, 80)})") from None
    if not isinstance(obj, dict):
        raise JobRejected("job file must hold one JSON object")
    return obj


def validate_job(obj: dict[str, Any], job_id: str, root_real: str, policy: Policy) -> Job:
    """Every rule a job has to meet. `root_real` is the resolved root folder."""
    if not is_job_id(job_id):
        raise JobRejected(BAD_JOB_ID)
    unknown = sorted(str(key) for key in obj if key not in JOB_KEYS)
    if unknown:
        raise JobRejected(f"unknown field(s): {printable(', '.join(unknown), 80)}")
    for key in REQUIRED_JOB_KEYS:
        if key not in obj:
            raise JobRejected(f"missing field '{key}'")
    version = obj["version"]
    if isinstance(version, bool) or version != PROTOCOL_VERSION:
        raise JobRejected(f"protocol version must be {PROTOCOL_VERSION}")
    if "id" in obj and obj["id"] != job_id:
        raise JobRejected("the 'id' field does not match the file name")

    args = obj["args"]
    if isinstance(args, str):
        raise JobRejected("'args' must be a list of strings, never one command string")
    if not isinstance(args, list):
        raise JobRejected("'args' must be a list of strings")
    if len(args) > MAX_ARGS:
        raise JobRejected(f"more than {MAX_ARGS} arguments")
    for arg in args:
        if not isinstance(arg, str):
            raise JobRejected("every argument must be a text string")
        if len(arg) > MAX_ARG_CHARS:
            raise JobRejected(f"an argument is longer than {MAX_ARG_CHARS} characters")
        if "\x00" in arg:
            raise JobRejected("an argument contains a NUL character")

    timeout = obj["timeout_seconds"]
    # An integer of hundreds of digits is valid JSON and cannot become a float:
    # only a float is asked whether it is finite.
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float)):
        raise JobRejected("'timeout_seconds' must be a number")
    if isinstance(timeout, float) and not math.isfinite(timeout):
        raise JobRejected("'timeout_seconds' must be a number")
    if not 1 <= timeout <= MAX_TIMEOUT_S:
        raise JobRejected(f"'timeout_seconds' must be between 1 and {int(MAX_TIMEOUT_S)}")

    env_obj = obj.get("env", {})
    if not isinstance(env_obj, dict):
        raise JobRejected("'env' must be an object of text values")
    if len(env_obj) > MAX_ENV_VARS:
        raise JobRejected(f"more than {MAX_ENV_VARS} environment variables")
    env: list[tuple[str, str]] = []
    for name, value in sorted(env_obj.items()):
        if not isinstance(name, str) or not ENV_NAME_RE.match(name):
            raise JobRejected(
                f"environment variable '{printable(name, 60)}' is not allowed (only ASAMU_*, WGPU_*, BEVY_*, RUST_BACKTRACE, RUST_LOG)"
            )
        if not isinstance(value, str) or len(value) > MAX_ENV_VALUE_CHARS or "\x00" in value:
            raise JobRejected(f"environment variable '{name}' needs a text value of at most {MAX_ENV_VALUE_CHARS} characters")
        env.append((name, value))

    if isinstance(obj["program"], list):
        raise JobRejected("'program' must be one path; arguments go in 'args'")
    program = resolve_inside(obj["program"], root_real, "program", policy, allow_root=False)
    check_program_file(program, policy)
    cwd = resolve_inside(obj["cwd"], root_real, "working directory", policy, allow_root=True)
    if not os.path.isdir(cwd):
        raise JobRejected("working directory is not a folder")
    if len(program) + sum(len(arg) + 3 for arg in args) > MAX_COMMAND_CHARS:
        raise JobRejected(f"program and arguments together are longer than {MAX_COMMAND_CHARS} characters")
    return Job(job_id, program, tuple(args), cwd, float(timeout), tuple(env))


def resolve_root(root: str) -> str:
    """The root folder, resolved; it must exist and must not be a whole drive."""
    try:
        real = os.path.realpath(root, strict=True)
    except (OSError, ValueError) as err:
        raise RunnerError(f"root folder {printable(root)} does not exist ({type(err).__name__})") from None
    if not os.path.isdir(real):
        raise RunnerError(f"root {printable(real)} is not a folder")
    if os.path.dirname(real) == real:
        raise RunnerError(f"root {printable(real)} is a whole drive or file system; give a project folder")
    return real


# --------------------------------------------------------------------------
# Time


class RealClock:
    """Wall time for records, monotonic time for limits. Tests use a fake."""

    def time(self) -> float:
        return time.time()

    def monotonic(self) -> float:
        return time.monotonic()

    def sleep(self, seconds: float) -> None:
        time.sleep(seconds)


# --------------------------------------------------------------------------
# Starting and ending processes


class _WinApi:
    """The few kernel32 calls needed to confine a process tree (job objects)."""

    JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 0x0008
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION = 0x0400
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000
    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION = 9
    PROCESS_TERMINATE = 0x0001
    PROCESS_SET_QUOTA = 0x0100
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    FILE_LIST_DIRECTORY = 0x0001
    FILE_SHARE_READ_WRITE = 0x0001 | 0x0002
    OPEN_EXISTING = 3
    FILE_FLAG_BACKUP_SEMANTICS = 0x02000000
    FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000
    FILE_ATTRIBUTE_DIRECTORY = 0x0010
    FILE_ATTRIBUTE_REPARSE_POINT = 0x0400
    THREAD_SUSPEND_RESUME = 0x0002
    TH32CS_SNAPTHREAD = 0x0004
    CREATE_SUSPENDED = 0x0004
    CREATE_NEW_PROCESS_GROUP = 0x0200
    CREATE_NO_WINDOW = 0x08000000

    def __init__(self) -> None:
        u32, vp, c_int = ctypes.c_uint32, ctypes.c_void_p, ctypes.c_int

        class BasicLimit(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_int64),
                ("PerJobUserTimeLimit", ctypes.c_int64),
                ("LimitFlags", u32),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", u32),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", u32),
                ("SchedulingClass", u32),
            ]

        class IoCounters(ctypes.Structure):
            _fields_ = [(f"c{i}", ctypes.c_uint64) for i in range(6)]

        class ExtendedLimit(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BasicLimit),
                ("IoInfo", IoCounters),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        class ThreadEntry(ctypes.Structure):
            _fields_ = [
                ("dwSize", u32),
                ("cntUsage", u32),
                ("th32ThreadID", u32),
                ("th32OwnerProcessID", u32),
                ("tpBasePri", ctypes.c_int32),
                ("tpDeltaPri", ctypes.c_int32),
                ("dwFlags", u32),
            ]

        class FileInformation(ctypes.Structure):
            _fields_ = [
                ("dwFileAttributes", u32),
                ("ftCreationTime", u32 * 2),
                ("ftLastAccessTime", u32 * 2),
                ("ftLastWriteTime", u32 * 2),
                ("dwVolumeSerialNumber", u32),
                ("nFileSizeHigh", u32),
                ("nFileSizeLow", u32),
                ("nNumberOfLinks", u32),
                ("nFileIndexHigh", u32),
                ("nFileIndexLow", u32),
            ]

        self.ExtendedLimit = ExtendedLimit
        self.ThreadEntry = ThreadEntry
        self.FileInformation = FileInformation
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined]
        self.k32 = k32
        for name, restype, argtypes in (
            ("CreateJobObjectW", vp, [vp, ctypes.c_wchar_p]),
            ("SetInformationJobObject", c_int, [vp, c_int, vp, u32]),
            ("AssignProcessToJobObject", c_int, [vp, vp]),
            ("TerminateJobObject", c_int, [vp, u32]),
            ("OpenProcess", vp, [u32, c_int, u32]),
            ("CloseHandle", c_int, [vp]),
            ("CreateToolhelp32Snapshot", vp, [u32, u32]),
            ("Thread32First", c_int, [vp, ctypes.POINTER(ThreadEntry)]),
            ("Thread32Next", c_int, [vp, ctypes.POINTER(ThreadEntry)]),
            ("OpenThread", vp, [u32, c_int, u32]),
            ("ResumeThread", u32, [vp]),
            ("QueryFullProcessImageNameW", c_int, [vp, u32, ctypes.c_wchar_p, ctypes.POINTER(u32)]),
            ("CreateFileW", vp, [ctypes.c_wchar_p, u32, u32, vp, u32, u32, vp]),
            ("GetFileInformationByHandle", c_int, [vp, ctypes.POINTER(FileInformation)]),
            ("GetFinalPathNameByHandleW", u32, [vp, ctypes.c_wchar_p, u32, u32]),
        ):
            func = getattr(k32, name)
            func.restype = restype
            func.argtypes = argtypes

    def fail(self, what: str) -> LaunchError:
        return LaunchError(f"{what} failed (Windows error {ctypes.get_last_error()})")  # type: ignore[attr-defined]

    def create_job(self) -> int:
        job = self.k32.CreateJobObjectW(None, None)
        if not job:
            raise self.fail("CreateJobObject")
        info = self.ExtendedLimit()
        info.BasicLimitInformation.LimitFlags = (
            self.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | self.JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | self.JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
        )
        info.BasicLimitInformation.ActiveProcessLimit = MAX_PROCESSES_PER_JOB
        if not self.k32.SetInformationJobObject(
            job, self.JOB_OBJECT_EXTENDED_LIMIT_INFORMATION, ctypes.byref(info), ctypes.sizeof(info)
        ):
            err = self.fail("SetInformationJobObject")
            self.k32.CloseHandle(job)
            raise err
        return int(job)

    def assign(self, job: int, pid: int) -> None:
        handle = self.k32.OpenProcess(self.PROCESS_SET_QUOTA | self.PROCESS_TERMINATE, 0, pid)
        if not handle:
            raise self.fail("OpenProcess")
        try:
            if not self.k32.AssignProcessToJobObject(job, handle):
                raise self.fail("AssignProcessToJobObject")
        finally:
            self.k32.CloseHandle(handle)

    def image_path(self, pid: int) -> str:
        """The file a process was really made from, links and junctions
        followed by the system itself at the moment it was created."""
        handle = self.k32.OpenProcess(self.PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
        if not handle:
            raise self.fail("OpenProcess")
        try:
            size = ctypes.c_uint32(32768)
            buf = ctypes.create_unicode_buffer(size.value)
            if not self.k32.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size)):
                raise self.fail("QueryFullProcessImageName")
            return buf.value
        finally:
            self.k32.CloseHandle(handle)

    def pin_folder(self, path: str) -> int:
        """Hold a folder in place: while the handle is open, the folder and
        the folders above it can be neither renamed nor removed, so nothing
        can put a link where they are. Files in it can still come and go.
        A link or a junction is refused (it is opened itself, not followed)."""
        handle = self.k32.CreateFileW(
            path,
            self.FILE_LIST_DIRECTORY,
            self.FILE_SHARE_READ_WRITE,
            None,
            self.OPEN_EXISTING,
            self.FILE_FLAG_BACKUP_SEMANTICS | self.FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
        if not handle or handle == ctypes.c_void_p(-1).value:
            raise self.fail("opening the folder")
        info = self.FileInformation()
        if not self.k32.GetFileInformationByHandle(handle, ctypes.byref(info)):
            err = self.fail("GetFileInformationByHandle")
            self.k32.CloseHandle(handle)
            raise err
        attributes = info.dwFileAttributes
        if not attributes & self.FILE_ATTRIBUTE_DIRECTORY or attributes & self.FILE_ATTRIBUTE_REPARSE_POINT:
            self.k32.CloseHandle(handle)
            raise LaunchError("it is a link or not a folder")
        return int(handle)

    def final_path(self, handle: int) -> str:
        """The real path of what an open handle refers to."""
        buf = ctypes.create_unicode_buffer(32768)
        length = self.k32.GetFinalPathNameByHandleW(handle, buf, 32768, 0)
        if length == 0 or length >= 32768:
            raise self.fail("GetFinalPathNameByHandle")
        return buf.value

    def unpin(self, handle: int) -> None:
        self.k32.CloseHandle(handle)

    def resume(self, pid: int) -> None:
        """Resume the threads of a process that was created suspended."""
        snap = self.k32.CreateToolhelp32Snapshot(self.TH32CS_SNAPTHREAD, 0)
        if not snap or snap == ctypes.c_void_p(-1).value:
            raise self.fail("CreateToolhelp32Snapshot")
        resumed = 0
        try:
            entry = self.ThreadEntry()
            entry.dwSize = ctypes.sizeof(entry)
            more = self.k32.Thread32First(snap, ctypes.byref(entry))
            while more:
                if entry.th32OwnerProcessID == pid:
                    thread = self.k32.OpenThread(self.THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID)
                    if thread:
                        if self.k32.ResumeThread(thread) != 0xFFFFFFFF:
                            resumed += 1
                        self.k32.CloseHandle(thread)
                more = self.k32.Thread32Next(snap, ctypes.byref(entry))
        finally:
            self.k32.CloseHandle(snap)
        if resumed == 0:
            raise LaunchError("the new process has no thread that could be resumed")


class RunningProcess:
    """One started job: its main process and the means to end its whole tree."""

    def __init__(self, popen: "subprocess.Popen[bytes]", files: list[BinaryIO], win: _WinApi | None, job: int | None):
        self._popen = popen
        self._files = files
        self._win = win
        self._job = job
        self.pid = popen.pid

    def poll(self) -> int | None:
        return self._popen.poll()

    def kill_tree(self) -> None:
        """End the main process and everything it started. Safe to repeat."""
        if self._win is not None:
            if self._job is not None:
                self._win.k32.TerminateJobObject(self._job, 1)
            return
        try:
            os.killpg(self.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        if self._popen.poll() is None:
            try:
                self._popen.kill()
            except OSError:
                pass

    def wait(self, seconds: float) -> int | None:
        try:
            return self._popen.wait(timeout=seconds)
        except subprocess.TimeoutExpired:
            return None

    def close(self) -> None:
        """Release handles. Closing the job object ends whatever is left in it."""
        if self._win is not None and self._job is not None:
            self._win.k32.CloseHandle(self._job)
            self._job = None
        for handle in self._files:
            try:
                handle.close()
            except OSError:
                pass
        self._files = []


def same_windows_path(one: str, other: str) -> bool:
    """Whether two resolved Windows paths are the same text, ignoring case,
    the kind of slash and the long-path prefix of a path that came from a
    handle."""

    def plain(path: str) -> str:
        path = path.replace("/", "\\")
        if path.startswith("\\\\?\\") and not path.upper().startswith("\\\\?\\UNC\\"):
            path = path[4:]
        return path.lower()

    return plain(one) == plain(other)


class ProcessLauncher:
    """Starts an argument list without a shell, confined so that its whole
    process tree can be ended: a job object on Windows (the tree also ends if
    the runner dies), a new session and process group elsewhere (a child that
    starts its own session escapes the group; Windows has no such gap).

    `argv[0]` and `cwd` are the resolved paths that passed the checks. On
    Windows the start itself is checked against them, because a folder on the
    way can be swapped for a junction after the check: the working directory
    is held in place while the process is created, and the new process stays
    suspended until the system confirms which file it was made from. A
    program from anywhere else is ended before it has run one instruction.
    Elsewhere (Linux, macOS: used for the tests) that last gap stays open."""

    def __init__(self) -> None:
        self._win = _WinApi() if os.name == "nt" else None

    def start(
        self, argv: list[str], cwd: str, env: dict[str, str], stdout_path: str, stderr_path: str
    ) -> RunningProcess:
        files: list[BinaryIO] = []
        job: int | None = None
        try:
            files.append(os.fdopen(open_own_file(stdout_path, "new"), "wb"))
            files.append(os.fdopen(open_own_file(stderr_path, "new"), "wb"))
            if self._win is not None:
                job = self._win.create_job()
                try:
                    pin = self._win.pin_folder(cwd)
                except LaunchError as err:
                    raise LaunchError(f"the working directory cannot be held in place: {err}") from None
                try:
                    if not same_windows_path(self._win.final_path(pin), cwd):
                        raise LaunchError("the working directory is not the folder that was checked (a link was put in its place)")
                    popen = subprocess.Popen(
                        argv,
                        executable=argv[0],
                        cwd=cwd,
                        env=env,
                        stdin=subprocess.DEVNULL,
                        stdout=files[0],
                        stderr=files[1],
                        shell=False,
                        close_fds=True,
                        creationflags=self._win.CREATE_SUSPENDED
                        | self._win.CREATE_NEW_PROCESS_GROUP
                        | self._win.CREATE_NO_WINDOW,
                    )
                finally:
                    self._win.unpin(pin)
                try:
                    self._win.assign(job, popen.pid)
                    image = self._win.image_path(popen.pid)
                    if not same_windows_path(image, argv[0]):
                        raise LaunchError(
                            f"the program that was about to run is {printable(image, 200)}, not the file that was "
                            "checked (a link was put in its place); it was ended before it ran"
                        )
                    self._win.resume(popen.pid)
                except LaunchError:
                    popen.kill()
                    popen.wait(timeout=KILL_GRACE_S)
                    raise
            else:
                popen = subprocess.Popen(
                    argv,
                    executable=argv[0],
                    cwd=cwd,
                    env=env,
                    stdin=subprocess.DEVNULL,
                    stdout=files[0],
                    stderr=files[1],
                    shell=False,
                    close_fds=True,
                    start_new_session=True,
                )
        except (OSError, ValueError, LaunchError, subprocess.SubprocessError) as err:
            if job is not None and self._win is not None:
                self._win.k32.CloseHandle(job)
            for handle in files:
                handle.close()
            if isinstance(err, LaunchError):
                raise
            raise LaunchError(f"{type(err).__name__}: {printable(err, 160)}") from None
        return RunningProcess(popen, files, self._win, job)


# --------------------------------------------------------------------------
# Files the two sides share


def _is_link(info: os.stat_result) -> bool:
    """A symbolic link, or on Windows any reparse point (junctions included)."""
    return stat.S_ISLNK(info.st_mode) or bool((getattr(info, "st_file_attributes", 0) or 0) & 0x400)


_WIN_FILE_API: list[Any] = []


def _win_open_no_follow(path: str, access: int, disposition: int, flags: int) -> int:
    """CreateFileW with FILE_FLAG_OPEN_REPARSE_POINT, as a file descriptor.
    The C runtime's own open() follows a symbolic link, even a dangling one
    and even when asked to create exclusively."""
    import msvcrt

    if not _WIN_FILE_API:
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined]
        k32.CreateFileW.restype = ctypes.c_void_p
        k32.CreateFileW.argtypes = [
            ctypes.c_wchar_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
        ]
        k32.CloseHandle.argtypes = [ctypes.c_void_p]
        _WIN_FILE_API.append(k32)
    k32 = _WIN_FILE_API[0]
    # Share reading and writing (as the C runtime does), normal attributes,
    # and open a link itself instead of what it points to.
    handle = k32.CreateFileW(path, access, 0x1 | 0x2, None, disposition, 0x80 | 0x00200000, None)
    if not handle or handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined]
    try:
        return msvcrt.open_osfhandle(handle, flags | os.O_BINARY | os.O_NOINHERIT)  # type: ignore[attr-defined]
    except OSError:
        k32.CloseHandle(handle)
        raise


def open_own_file(path: str, mode: str) -> int:
    """Open one of the files the runner or its client writes, never through a
    link, and return the file descriptor.

    "new"     whatever has that name is removed first (a link itself, never
              what it points to), then the file is created;
    "append"  opened for appending, created when missing (the logs);
    "lock"    opened for reading and writing, created when missing.

    A planted link would otherwise send the write to any file the account can
    reach. What was opened is checked as well: it must be a regular file, no
    link, with this one name only."""
    if mode == "new":
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
    if os.name == "nt":
        access, disposition, flags = {
            "new": (0x40000000, 1, os.O_WRONLY),  # GENERIC_WRITE, CREATE_NEW
            "append": (0x0004 | 0x0080 | 0x00100000, 4, os.O_WRONLY | os.O_APPEND),  # FILE_APPEND_DATA, OPEN_ALWAYS
            "lock": (0x80000000 | 0x40000000, 4, os.O_RDWR),
        }[mode]
        fd = _win_open_no_follow(path, access, disposition, flags)
    else:
        flags = {
            "new": os.O_WRONLY | os.O_CREAT | os.O_EXCL,
            "append": os.O_WRONLY | os.O_APPEND | os.O_CREAT,
            "lock": os.O_RDWR | os.O_CREAT,
        }[mode]
        # O_NONBLOCK: a named pipe put in the file's place must not hold up the open.
        fd = os.open(path, flags | os.O_NOFOLLOW | os.O_NONBLOCK, 0o644)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or _is_link(info) or info.st_nlink != 1:
            raise OSError(f"{path} is a link, or not a plain file with one name")
    except OSError:
        os.close(fd)
        raise
    return fd


def write_json_atomic(path: str, obj: object) -> None:
    """Write next to the target, then rename over it, so a reader never sees
    half a file. A reader on Windows can hold the old file for a moment."""
    tmp = path + ".tmp"
    with os.fdopen(open_own_file(tmp, "new"), "w", encoding="utf-8", newline="\n") as handle:
        json.dump(obj, handle, indent=1, sort_keys=True)
        handle.write("\n")
    for attempt in range(20):
        try:
            os.replace(tmp, path)
            return
        except PermissionError:
            if attempt == 19:
                raise
            time.sleep(0.02)


def read_json(path: str) -> dict[str, Any] | None:
    try:
        with open(path, "r", encoding="utf-8") as handle:
            obj = json.loads(handle.read(4 * 1024 * 1024))
    except (OSError, ValueError):
        return None
    return obj if isinstance(obj, dict) else None


class InstanceLock:
    """A lock file held for the life of the runner. The operating system drops
    the lock when the process ends, however it ends, so it is never stale."""

    def __init__(self, path: str):
        self.path = path
        self._fd: int | None = None

    @staticmethod
    def _try_lock(fd: int) -> bool:
        try:
            if os.name == "nt":
                import msvcrt

                os.lseek(fd, 0, os.SEEK_SET)
                msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)  # type: ignore[attr-defined]
            else:
                import fcntl

                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return False
        return True

    def acquire(self) -> bool:
        """True when this process now holds the lock. Raises OSError when the
        lock file is a link or cannot be opened."""
        fd = open_own_file(self.path, "lock")
        if self._try_lock(fd) and self._is_the_named_file(fd):
            self._fd = fd
            return True
        os.close(fd)
        return False

    def _is_the_named_file(self, fd: int) -> bool:
        try:
            return os.path.samestat(os.fstat(fd), os.lstat(self.path))
        except OSError:
            return False

    def still_in_place(self) -> bool:
        """Whether the lock file's name still leads to the file this process
        locked. Where a file that is open can be deleted (Linux, macOS), a
        deleted lock file would let a second runner start; the first one has
        to notice and stop."""
        return self._fd is not None and self._is_the_named_file(self._fd)

    def release(self) -> None:
        if self._fd is not None:
            os.close(self._fd)
            self._fd = None

    @classmethod
    def held_by_someone(cls, path: str) -> bool:
        """Whether a live process holds the lock. Creates nothing."""
        try:
            fd = os.open(path, os.O_RDWR)
        except OSError:
            return False
        try:
            return not cls._try_lock(fd)
        finally:
            os.close(fd)


@dataclass(frozen=True)
class Layout:
    """Where everything is, below the resolved root."""

    root: str

    @property
    def base(self) -> str:
        return os.path.join(self.root, "runner")

    @property
    def inbox(self) -> str:
        return os.path.join(self.base, "inbox")

    @property
    def outbox(self) -> str:
        return os.path.join(self.base, "outbox")

    @property
    def heartbeat(self) -> str:
        return os.path.join(self.base, "heartbeat.json")

    @property
    def lock(self) -> str:
        return os.path.join(self.base, "runner.lock")

    @property
    def stop(self) -> str:
        return os.path.join(self.base, "stop")

    @property
    def log(self) -> str:
        return os.path.join(self.base, "runner.log")

    @property
    def rejected_log(self) -> str:
        return os.path.join(self.base, "rejected.log")

    @property
    def script(self) -> str:
        return os.path.join(self.base, "desk_runner.py")

    @property
    def start_cmd(self) -> str:
        return os.path.join(self.root, "start-runner.cmd")

    def status(self, job_id: str) -> str:
        return os.path.join(self.outbox, job_id + ".status.json")

    def stdout(self, job_id: str) -> str:
        return os.path.join(self.outbox, job_id + ".stdout.txt")

    def stderr(self, job_id: str) -> str:
        return os.path.join(self.outbox, job_id + ".stderr.txt")


def own_digest() -> str:
    """SHA-256 of this file, so both sides can tell which runner is running."""
    try:
        with open(os.path.abspath(__file__), "rb") as handle:
            return hashlib.sha256(handle.read()).hexdigest()
    except OSError:
        return "unknown"


def session_info() -> dict[str, Any]:
    """Where this process runs. On Windows only a process on the interactive
    window station (`WinSta0`) of a session other than 0 can show a window."""
    info: dict[str, Any] = {"interactive": None, "window_station": None, "session_id": None, "elevated": None}
    if os.name != "nt":
        return info
    try:
        user32 = ctypes.WinDLL("user32", use_last_error=True)  # type: ignore[attr-defined]
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined]
        user32.GetProcessWindowStation.restype = ctypes.c_void_p
        user32.GetUserObjectInformationW.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_uint32),
        ]
        station = user32.GetProcessWindowStation()
        name = ""
        if station:
            buf = ctypes.create_unicode_buffer(256)
            needed = ctypes.c_uint32(0)
            if user32.GetUserObjectInformationW(station, 2, buf, ctypes.sizeof(buf), ctypes.byref(needed)):
                name = buf.value
        sid = ctypes.c_uint32(0)
        if not k32.ProcessIdToSessionId(k32.GetCurrentProcessId(), ctypes.byref(sid)):
            sid = ctypes.c_uint32(0xFFFFFFFF)
        info["window_station"] = name
        info["session_id"] = int(sid.value)
        info["interactive"] = name.lower() == "winsta0" and sid.value not in (0, 0xFFFFFFFF)
        info["elevated"] = bool(ctypes.WinDLL("shell32").IsUserAnAdmin())  # type: ignore[attr-defined]
    except (OSError, AttributeError):
        pass
    return info


# --------------------------------------------------------------------------
# The runner


@dataclass
class _Active:
    job: Job
    proc: Any
    started_mono: float
    started_unix: float
    cancel: bool = False


@dataclass
class _Queued:
    job_id: str
    obj: dict[str, Any]
    queued_unix: float


@dataclass
class Runner:
    """The job loop. `step()` does one round; `run()` repeats it.

    The clock and the launcher are parameters so that the tests can drive the
    life cycle of a job without real processes and without waiting."""

    root: str
    clock: Any = field(default_factory=RealClock)
    launcher: Any = None
    policy: Policy = field(default_factory=Policy.host)
    out: TextIO | None = None
    max_jobs: int = DEFAULT_MAX_JOBS
    session: dict[str, Any] = field(default_factory=session_info)
    stop_requested: threading.Event = field(default_factory=threading.Event)

    def __post_init__(self) -> None:
        self.root = resolve_root(self.root)
        if not 1 <= self.max_jobs <= HARD_MAX_JOBS:
            raise RunnerError(f"--max-jobs must be between 1 and {HARD_MAX_JOBS}")
        if self.launcher is None:
            self.launcher = ProcessLauncher()
        if self.out is None:
            self.out = sys.stdout
        self.layout = Layout(self.root)
        self._lock = InstanceLock(self.layout.lock)
        self._active: dict[str, _Active] = {}
        self._queue: list[_Queued] = []
        self._ignored: set[str] = set()
        self._said_once: set[str] = set()
        self._undeletable: dict[str, int] = {}
        self._early_cancels: dict[str, float] = {}
        self._pins: list[int] = []
        self._winapi = _WinApi() if os.name == "nt" else None
        self._last_beat: float | None = None
        self._started_unix = 0.0
        self._digest = own_digest()
        self._running = False
        self.stop_reason = ""
        self.stop_request_reason = "its window was closed"
        self._no_writes = False
        self.jobs_started = 0
        self.jobs_refused = 0

    # -- output ------------------------------------------------------------

    def _print(self, line: str) -> None:
        out = self.out if self.out is not None else sys.stdout
        try:
            out.write(line + "\n")
            out.flush()
        except (OSError, ValueError):
            pass

    def say(self, text: str) -> None:
        """One line on the console (local time) and in the log (UTC)."""
        self._print(f"[{time.strftime('%H:%M:%S', time.localtime(self.clock.time()))}] {text}")
        self._append(self.layout.log, f"{utc_text(self.clock.time())} {text}")

    def _append(self, path: str, line: str) -> None:
        if self._no_writes:
            return
        try:
            if os.path.getsize(path) > LOG_ROTATE_BYTES:
                os.replace(path, path + ".1")
        except OSError:
            pass
        try:
            with os.fdopen(open_own_file(path, "append"), "a", encoding="utf-8", newline="\n") as handle:
                handle.write(line + "\n")
        except OSError:
            pass

    def _say_once(self, key: str, text: str) -> None:
        if key not in self._said_once:
            self._said_once.add(key)
            self.say(text)

    def banner(self) -> list[str]:
        """What the owner reads in the console window."""
        root = printable(self.root)
        lines = [
            "=" * 72,
            f" ASAMU-decomp desk runner   (protocol {PROTOCOL_VERSION}, process {os.getpid()})",
            "=" * 72,
            " WHAT THIS IS",
            "   A helper for developing the ASAMU-decomp game recreation. It lets the",
            "   project's tools on the other computer start OUR programs on this",
            "   computer's screen, so the recreation can be tested and played here.",
            " WHAT IT RUNS",
            f"   Only program files that are stored inside {root}",
            "   each one with a fixed list of arguments and a time limit, after which",
            f"   it is ended together with everything it started. At most {self.max_jobs} at a time.",
            " WHAT IT NEVER RUNS",
            f"   Anything outside {root} (not the original game, not Steam),",
            "   no cmd, PowerShell, scripts, batch files or interpreters, and no",
            "   command lines: a request that tries is refused and shown below.",
            " WHAT ELSE IT DOES",
            "   Nothing. It opens no network port, changes no setting, and writes only",
            f"   inside {printable(self.layout.base)}",
            " TO STOP IT",
            "   Close this window. Programs it started are ended with it.",
            "-" * 72,
        ]
        if self.session.get("interactive") is False:
            lines.append(
                f" NOTE: this runner is NOT in a desktop session (window station "
                f"'{printable(self.session.get('window_station'), 40)}', session {self.session.get('session_id')}):"
            )
            lines.append("       programs it starts cannot show a window. Start it by double-click.")
        if self.session.get("elevated") and self.session.get("interactive"):
            lines.append(" NOTE: this window has administrator rights, and so will the programs it")
            lines.append("       starts. Start it by double-click, not with 'Run as administrator'.")
        return lines

    # -- start and stop ----------------------------------------------------

    def _check_own_folders(self) -> None:
        """The runner's folders must be real folders in place: a link there
        would send its writes somewhere else."""
        for folder in (self.layout.base, self.layout.inbox, self.layout.outbox):
            self._check_own_folder(folder)

    @staticmethod
    def _check_own_folder(folder: str) -> None:
        try:
            info = os.lstat(folder)
            real = os.path.realpath(folder, strict=True)
        except OSError as err:
            raise RunnerError(f"folder {printable(folder)} is missing ({type(err).__name__})") from None
        if not stat.S_ISDIR(info.st_mode) or _is_link(info) or os.path.normcase(real) != os.path.normcase(folder):
            raise RunnerError(f"folder {printable(folder)} is a link or not a folder; refusing to use it")

    def _check_own_files(self) -> None:
        """The files the runner keeps writing must be plain files of its own
        (or not exist yet): through a link its writes would land elsewhere."""
        for path in (self.layout.lock, self.layout.log, self.layout.rejected_log, self.layout.heartbeat):
            try:
                info = os.lstat(path)
            except FileNotFoundError:
                continue
            except OSError as err:
                raise RunnerError(f"file {printable(path)} cannot be examined ({type(err).__name__})") from None
            if not stat.S_ISREG(info.st_mode) or _is_link(info) or info.st_nlink != 1:
                raise RunnerError(f"file {printable(path)} is a link or not a plain file; refusing to use it")

    def _pin_own_folders(self) -> None:
        """Windows: hold the runner's folders in place for as long as it runs,
        so that none of them (nor the root above them) can be renamed away and
        replaced by a link between one check and the next write. Elsewhere
        the check at the start of every round has to do."""
        if self._winapi is None:
            return
        for folder in (self.layout.base, self.layout.inbox, self.layout.outbox):
            try:
                self._pins.append(self._winapi.pin_folder(folder))
            except LaunchError as err:
                raise RunnerError(f"folder {printable(folder)} cannot be held in place: {err}") from None

    def _unpin_own_folders(self) -> None:
        while self._pins:
            handle = self._pins.pop()
            if self._winapi is not None:
                self._winapi.unpin(handle)

    def startup(self) -> None:
        try:
            self._startup()
        except RunnerError:
            self._unpin_own_folders()
            raise

    def _startup(self) -> None:
        # One level at a time, each checked before the next is made: nothing
        # is created through a link that was put where a folder belongs.
        for folder in (self.layout.base, self.layout.inbox, self.layout.outbox):
            try:
                os.mkdir(folder)
            except FileExistsError:
                pass
            except OSError as err:
                raise RunnerError(f"folder {printable(folder)} cannot be made ({type(err).__name__})") from None
            self._check_own_folder(folder)
        self._pin_own_folders()
        self._check_own_folders()
        self._check_own_files()
        for _ in range(5):
            try:
                if self._lock.acquire():
                    break
            except OSError as err:
                raise RunnerError(
                    f"the lock file {printable(self.layout.lock)} cannot be used ({type(err).__name__}: {printable(err, 120)})"
                ) from None
            time.sleep(0.2)
        else:
            other = read_json(self.layout.heartbeat) or {}
            raise RunnerError(
                f"a desk runner is already running for {printable(self.root)} "
                f"(process {printable(other.get('pid', '?'), 12)}); not starting a second one"
            )
        self._running = True
        self._started_unix = self.clock.time()
        try:
            os.unlink(self.layout.stop)
        except OSError:
            pass
        for line in self.banner():
            self._print(line)
        self.say(f"started; root {printable(self.root)}")
        self._prune_outbox()
        self._clear_inbox_at_start()
        self._beat(force=True)
        self.say(f"waiting for jobs in {printable(self.layout.inbox)}")

    def _clear_inbox_at_start(self) -> None:
        """Nothing runs that was not submitted while the runner was up."""
        try:
            entries = sorted(os.scandir(self.layout.inbox), key=lambda e: e.name)
        except OSError:
            return
        for entry in entries:
            try:
                if not stat.S_ISREG(entry.stat(follow_symlinks=False).st_mode):
                    continue
                os.unlink(entry.path)
            except OSError:
                continue
            if entry.name.endswith(".json"):
                self._refuse(entry.name[: -len(".json")], "was already waiting when the runner started; submit it again")

    def _prune_outbox(self) -> None:
        pattern = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.(status\.json|stdout\.txt|stderr\.txt)(\.tmp)?$")
        now = self.clock.time()
        try:
            entries = list(os.scandir(self.layout.outbox))
        except OSError:
            return
        for entry in entries:
            try:
                info = entry.stat(follow_symlinks=False)
                if pattern.match(entry.name) and stat.S_ISREG(info.st_mode) and now - info.st_mtime > RESULT_KEEP_S:
                    os.unlink(entry.path)
            except OSError:
                continue

    def shutdown(self, reason: str) -> None:
        """End every job, say why, drop the lock. Safe to call twice."""
        if not self._running:
            return
        self._running = False
        self.stop_reason = reason
        self.say(f"stopping: {reason}")
        for active in list(self._active.values()):
            self._end(active, "runner_stopped", f"the runner stopped ({reason})")
        for queued in self._queue:
            self._write_status(queued.job_id, {"state": "runner_stopped", "reason": f"the runner stopped ({reason})"})
        self._queue = []
        self._beat(force=True, state="stopped")
        if not self._no_writes:
            try:
                os.unlink(self.layout.stop)
            except OSError:
                pass
        self._lock.release()
        self.say("stopped")
        self._unpin_own_folders()

    def run(self) -> int:
        try:
            self.startup()
        except RunnerError as err:
            self._print(f"desk runner: {err}")
            return 3
        try:
            while self.step():
                self.clock.sleep(POLL_PERIOD_S)
        except KeyboardInterrupt:
            self.shutdown("Ctrl+C")
        except RunnerError as err:
            self.shutdown(f"error: {err}")
            return 1
        finally:
            self.shutdown("the runner ended unexpectedly")
        return 0

    # -- one round ---------------------------------------------------------

    def step(self) -> bool:
        """One round of the loop. False once the runner has stopped."""
        if not self._running:
            return False
        if self.stop_requested.is_set():
            self.shutdown(self.stop_request_reason)
            return False
        if os.path.lexists(self.layout.stop):
            self.shutdown("stop file")
            return False
        try:
            self._check_own_folders()
        except RunnerError:
            # One of the runner's folders now leads somewhere else: from here
            # on it writes nothing at all, ends its jobs and stops.
            self._no_writes = True
            raise
        if not self._lock.still_in_place():
            raise RunnerError(
                "the lock file was removed or replaced, so a second runner could start; stopping so that two never work at once"
            )
        self._scan_inbox()
        self._poll_active()
        self._start_queued()
        self._beat()
        return True

    def _beat(self, force: bool = False, state: str = "running") -> None:
        now = self.clock.monotonic()
        if self._no_writes:
            return
        if not force and self._last_beat is not None and now - self._last_beat < HEARTBEAT_PERIOD_S:
            return
        self._last_beat = now
        beat = {
            "protocol": PROTOCOL_VERSION,
            "state": state,
            "pid": os.getpid(),
            "root": self.root,
            "started_unix": self._started_unix,
            "beat_unix": self.clock.time(),
            "beat_utc": utc_text(self.clock.time()),
            "max_jobs": self.max_jobs,
            "script_sha256": self._digest,
            "running": [
                {
                    "id": a.job.job_id,
                    "program": a.job.program,
                    "pid": getattr(a.proc, "pid", None),
                    "started_unix": a.started_unix,
                    "timeout_seconds": a.job.timeout_seconds,
                }
                for a in self._active.values()
            ],
            "queued": [q.job_id for q in self._queue],
            "jobs_started": self.jobs_started,
            "jobs_refused": self.jobs_refused,
            **self.session,
        }
        try:
            write_json_atomic(self.layout.heartbeat, beat)
        except OSError:
            pass

    # -- intake ------------------------------------------------------------

    def _report_ignored(self, name: str, why: str) -> None:
        if name in self._ignored:
            return
        if len(self._ignored) >= MAX_REMEMBERED_NAMES:
            # Not remembered, so it would be reported again every round.
            self._say_once(
                "ignored-full",
                f"more than {MAX_REMEMBERED_NAMES} inbox entries have been ignored; further ones are not reported one by one",
            )
            return
        self._ignored.add(name)
        self.jobs_refused += 1
        self.say(f"IGNORED inbox entry '{printable(name, 80)}': {why}")
        self._append(
            self.layout.rejected_log,
            json.dumps({"time": utc_text(self.clock.time()), "name": printable(name, 120), "reason": why}),
        )

    def _refuse(self, job_id: str, reason: str, program: object = None, replace_status: bool = False) -> None:
        """Refuse a job: console, log and, when the id is usable, its status.
        A status that already exists is the result of an earlier job with the
        same id and is kept, unless it is this job's own "queued" status."""
        self.jobs_refused += 1
        shown = f" ({printable(program, 120)})" if isinstance(program, str) else ""
        self.say(f"REFUSED job '{printable(job_id, 80)}'{shown}: {reason}")
        self._append(
            self.layout.rejected_log,
            json.dumps({"time": utc_text(self.clock.time()), "name": printable(job_id, 120), "reason": reason}),
        )
        if is_job_id(job_id) and (replace_status or not os.path.lexists(self.layout.status(job_id))):
            fields: dict[str, Any] = {"state": "rejected", "reason": reason}
            if isinstance(program, str):
                fields["program"] = printable(program, 300)
            self._write_status(job_id, fields)

    def _write_status(self, job_id: str, fields: dict[str, Any]) -> None:
        if self._no_writes:
            return
        now = self.clock.time()
        status = {"protocol": PROTOCOL_VERSION, "id": job_id, "updated_unix": now, "updated_utc": utc_text(now)}
        status.update(fields)
        try:
            write_json_atomic(self.layout.status(job_id), status)
        except OSError as err:
            self.say(f"could not write the status of job {job_id}: {type(err).__name__}")

    def _scan_inbox(self) -> None:
        try:
            listing = os.scandir(self.layout.inbox)
        except OSError as err:
            raise RunnerError(f"cannot read the inbox ({type(err).__name__})") from None
        found: list[tuple[float, str, int]] = []
        examined = 0
        try:
            for entry in listing:
                # A bound on the work of one round: the time limits of the
                # running jobs are enforced by this same loop.
                examined += 1
                if examined > MAX_INBOX_ENTRIES_EXAMINED:
                    self._say_once(
                        "inbox-full",
                        f"the inbox holds more than {MAX_INBOX_ENTRIES_EXAMINED} entries; only that many are looked at "
                        "each round (remove what does not belong there)",
                    )
                    break
                name = entry.name
                if name in self._ignored:
                    continue
                try:
                    info = entry.stat(follow_symlinks=False)
                except OSError:
                    continue
                if not stat.S_ISREG(info.st_mode) or _is_link(info):
                    self._report_ignored(name, "not a regular file (links and folders are never followed)")
                    continue
                if name.endswith(".json.tmp") or name.endswith(".cancel.tmp"):
                    continue
                found.append((info.st_mtime, name, info.st_size))
        except OSError as err:
            raise RunnerError(f"cannot read the inbox ({type(err).__name__})") from None
        finally:
            close = getattr(listing, "close", None)
            if close is not None:
                close()
        # Oldest first; within one round jobs before cancellations, so that a
        # cancellation never overtakes the job it is meant for.
        found.sort()
        found = found[:MAX_INBOX_ENTRIES_PER_SCAN]
        found.sort(key=lambda item: item[1].endswith(".cancel"))
        withdrawn = {name[: -len(".cancel")] for _mtime, name, _size in found if name.endswith(".cancel")}
        for _mtime, name, size in found:
            path = os.path.join(self.layout.inbox, name)
            if name.endswith(".cancel"):
                if self._remove_inbox_file(name, path):
                    self._cancel(name[: -len(".cancel")])
                continue
            if not name.endswith(".json"):
                if self._remove_inbox_file(name, path):
                    self._report_ignored(name, "not a job file name (removed)")
                    self._ignored.discard(name)
                continue
            job_id = name[: -len(".json")]
            if job_id in withdrawn and is_job_id(job_id) and not os.path.lexists(self.layout.status(job_id)):
                # Its cancellation is already here: it is not started at all.
                if self._remove_inbox_file(name, path):
                    self.jobs_refused += 1
                    self.say(f"CANCELLED job {job_id} (it was withdrawn before it started)")
                    self._write_status(job_id, {"state": "cancelled", "reason": "cancelled before it started"})
                continue
            if size > MAX_JOB_FILE_BYTES:
                if self._remove_inbox_file(name, path):
                    self._refuse(job_id, f"job file is larger than {MAX_JOB_FILE_BYTES} bytes ({size} bytes); not read")
                continue
            data = self._read_job_file(path)
            if data is None:
                self._report_ignored(name, "could not be opened as a regular file")
                continue
            if not self._remove_inbox_file(name, path):
                continue
            self._accept(job_id, data)

    def _read_job_file(self, path: str) -> bytes | None:
        # O_NONBLOCK: a named pipe swapped in after the check must not hold up the open.
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0) | getattr(os, "O_NONBLOCK", 0)
        try:
            fd = os.open(path, flags)
        except OSError:
            return None
        try:
            if not stat.S_ISREG(os.fstat(fd).st_mode):
                return None
            chunks = []
            left = MAX_JOB_FILE_BYTES + 1
            while left > 0:
                chunk = os.read(fd, left)
                if not chunk:
                    break
                chunks.append(chunk)
                left -= len(chunk)
            return b"".join(chunks)
        except OSError:
            return None
        finally:
            os.close(fd)

    def _remove_inbox_file(self, name: str, path: str) -> bool:
        """Take a file out of the inbox. One that cannot be removed (still
        open by its writer) is tried again; it is never acted on twice."""
        try:
            os.unlink(path)
        except OSError:
            tries = self._undeletable.get(name, 0) + 1
            self._undeletable[name] = tries
            if tries >= 40:
                self._undeletable.pop(name, None)
                self._report_ignored(name, "could not be removed from the inbox")
            return False
        self._undeletable.pop(name, None)
        return True

    def _accept(self, job_id: str, data: bytes) -> None:
        if not is_job_id(job_id):
            self._refuse(job_id, "bad job file name (1 to 64 letters, digits, '-' or '_', no device name, then .json)")
            return
        if job_id in self._active or any(q.job_id == job_id for q in self._queue):
            self._refuse(job_id, "a job with this id is already running or waiting")
            return
        if os.path.lexists(self.layout.status(job_id)):
            self.jobs_refused += 1
            self.say(f"REFUSED job '{job_id}': a result with this id already exists (it is kept)")
            return
        cancelled_at = self._early_cancels.pop(job_id, None)
        if cancelled_at is not None and self.clock.monotonic() - cancelled_at <= EARLY_CANCEL_KEEP_S:
            self.jobs_refused += 1
            self.say(f"CANCELLED job {job_id} (its cancellation arrived before it did)")
            self._write_status(job_id, {"state": "cancelled", "reason": "cancelled before it arrived"})
            return
        program: object = None
        try:
            obj = parse_job_bytes(data)
            program = obj.get("program")
            validate_job(obj, job_id, self.root, self.policy)
        except JobRejected as err:
            self._refuse(job_id, str(err), program)
            return
        except Exception as err:  # noqa: BLE001 - no job file, however odd, may end the runner
            self._refuse(job_id, f"could not be checked ({type(err).__name__})", program)
            return
        if len(self._active) < self.max_jobs and not self._queue:
            self._start(job_id, obj)
        elif len(self._queue) < MAX_QUEUED:
            self._queue.append(_Queued(job_id, obj, self.clock.time()))
            self._write_status(job_id, {"state": "queued", "position": len(self._queue)})
            self.say(f"QUEUED job {job_id} ({len(self._active)} running, {len(self._queue)} waiting)")
            self._beat(force=True)
        else:
            self._refuse(job_id, f"too many jobs are waiting ({MAX_QUEUED})", program)

    def _start_queued(self) -> None:
        while self._queue and len(self._active) < self.max_jobs:
            queued = self._queue.pop(0)
            self._start(queued.job_id, queued.obj, was_queued=True)
            self._beat(force=True)

    # -- running -----------------------------------------------------------

    def _start(self, job_id: str, obj: dict[str, Any], was_queued: bool = False) -> None:
        # Checked again at the moment of starting: the files may have changed
        # while the job waited.
        try:
            job = validate_job(obj, job_id, self.root, self.policy)
        except JobRejected as err:
            self._refuse(job_id, str(err), obj.get("program"), replace_status=was_queued)
            return
        except Exception as err:  # noqa: BLE001 - as in _accept
            self._refuse(job_id, f"could not be checked ({type(err).__name__})", obj.get("program"), replace_status=was_queued)
            return
        env = dict(os.environ)
        env.update(dict(job.env))
        started_unix = self.clock.time()
        base = {
            "program": job.program,
            "args": list(job.args),
            "cwd": job.cwd,
            "timeout_seconds": job.timeout_seconds,
            "env": dict(job.env),
            "started_unix": started_unix,
            "started_utc": utc_text(started_unix),
        }
        try:
            proc = self.launcher.start(job.argv, job.cwd, env, self.layout.stdout(job_id), self.layout.stderr(job_id))
        except (LaunchError, OSError) as err:
            reason = f"could not start: {printable(err, 200)}"
            self.say(f"FAILED job {job_id}: {reason}")
            self._write_status(
                job_id,
                {**base, "state": "failed_to_start", "reason": reason, "ended_unix": started_unix, "ended_utc": utc_text(started_unix)},
            )
            return
        self.jobs_started += 1
        active = _Active(job, proc, self.clock.monotonic(), started_unix)
        self._active[job_id] = active
        self._write_status(job_id, {**base, "state": "running", "pid": getattr(proc, "pid", None)})
        self.say(f"START job {job_id}: {show_command(job.argv)}")
        self.say(f"      in {printable(job.cwd)}, limit {job.timeout_seconds:g} s, process {getattr(proc, 'pid', '?')}")
        self._beat(force=True)

    def _cancel(self, job_id: str) -> None:
        if job_id in self._active:
            self._active[job_id].cancel = True
            return
        for index, queued in enumerate(self._queue):
            if queued.job_id == job_id:
                del self._queue[index]
                self._write_status(job_id, {"state": "cancelled", "reason": "cancelled while waiting"})
                self.say(f"CANCELLED job {job_id} (it had not started)")
                self._beat(force=True)
                return
        # A cancellation can arrive before its job (the sender was interrupted
        # while the job file was still on its way): remember it for a while.
        if is_job_id(job_id) and not os.path.lexists(self.layout.status(job_id)):
            now = self.clock.monotonic()
            for old in [key for key, at in self._early_cancels.items() if now - at > EARLY_CANCEL_KEEP_S]:
                del self._early_cancels[old]
            if len(self._early_cancels) < MAX_EARLY_CANCELS:
                self._early_cancels[job_id] = now

    def _output_size(self, job_id: str) -> int:
        total = 0
        for path in (self.layout.stdout(job_id), self.layout.stderr(job_id)):
            try:
                total = max(total, os.path.getsize(path))
            except OSError:
                pass
        return total

    def _poll_active(self) -> None:
        now = self.clock.monotonic()
        for active in list(self._active.values()):
            job = active.job
            code = active.proc.poll()
            if code is not None:
                self._end(active, "finished", None, code)
            elif active.cancel:
                self._end(active, "cancelled", "cancelled on request")
            elif now - active.started_mono >= job.timeout_seconds:
                self._end(active, "timed_out", f"still running after its limit of {job.timeout_seconds:g} s")
            elif self._output_size(job.job_id) > MAX_OUTPUT_BYTES:
                self._end(active, "output_limit", f"wrote more than {MAX_OUTPUT_BYTES} bytes of output")

    def _end(self, active: _Active, state: str, reason: str | None, code: int | None = None) -> None:
        """Record the end of a job. Its process tree is ended in every case:
        a finished program may have left children behind."""
        job = active.job
        active.proc.kill_tree()
        killed = state != "finished"
        confirmed = True
        if killed:
            code = active.proc.wait(KILL_GRACE_S)
            confirmed = code is not None
        active.proc.close()
        self._active.pop(job.job_id, None)
        ended_unix = self.clock.time()
        duration = self.clock.monotonic() - active.started_mono
        fields: dict[str, Any] = {
            "state": state,
            "reason": reason,
            "exit_code": None if killed else code,
            "program": job.program,
            "args": list(job.args),
            "cwd": job.cwd,
            "timeout_seconds": job.timeout_seconds,
            "env": dict(job.env),
            "pid": getattr(active.proc, "pid", None),
            "started_unix": active.started_unix,
            "started_utc": utc_text(active.started_unix),
            "ended_unix": ended_unix,
            "ended_utc": utc_text(ended_unix),
            "duration_seconds": round(duration, 3),
        }
        if killed:
            fields["process_tree_ended"] = confirmed
        self._write_status(job.job_id, fields)
        if state == "finished":
            self.say(f"END   job {job.job_id}: exit code {code} after {duration:.1f} s")
        else:
            gone = "its process tree was ended" if confirmed else "its main process did NOT end in time"
            self.say(f"ENDED job {job.job_id}: {reason}; {gone} ({duration:.1f} s)")
        self._beat(force=True)


# --------------------------------------------------------------------------
# The window of the runner (Windows)


def _prepare_console(runner: Runner, done: threading.Event) -> list[object]:
    """Title the window, stop a click in it from freezing the runner, and end
    the jobs in an orderly way when the window is closed. Returns objects that
    must stay alive. Changes only this console window, nothing system-wide."""
    keep: list[object] = []
    if os.name != "nt":
        return keep
    try:
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined]
        k32.GetStdHandle.restype = ctypes.c_void_p
        k32.GetConsoleMode.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
        k32.SetConsoleMode.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        k32.SetConsoleTitleW("ASAMU-decomp desk runner")
        # QuickEdit: a click in a console window selects text and blocks the
        # program's next write until a key is pressed. That would stop the
        # time limits from being enforced, so it is off for this window.
        stdin = k32.GetStdHandle(ctypes.c_uint32(-10 & 0xFFFFFFFF))
        mode = ctypes.c_uint32(0)
        if stdin and k32.GetConsoleMode(stdin, ctypes.byref(mode)):
            k32.SetConsoleMode(stdin, (mode.value & ~0x0040) | 0x0080)

        handler_type = ctypes.WINFUNCTYPE(ctypes.c_int, ctypes.c_uint32)  # type: ignore[attr-defined]

        reasons = {1: "Ctrl+Break", 2: "its window was closed", 5: "the user signed out", 6: "the machine shut down"}

        def on_event(event: int) -> int:
            if event in reasons:
                runner.stop_request_reason = reasons[event]
                runner.stop_requested.set()
                done.wait(4.0)
                return 1
            return 0

        callback = handler_type(on_event)
        k32.SetConsoleCtrlHandler.argtypes = [handler_type, ctypes.c_int]
        k32.SetConsoleCtrlHandler(callback, 1)
        keep.append(callback)
    except (OSError, AttributeError):
        pass
    return keep


class BackgroundWriter:
    """Console output that never holds up the runner.

    A console window blocks the program at its next write while its text is
    being selected or its output is paused. The job loop enforces the time
    limits, so it must not wait for the window: lines go into a bounded
    backlog that a thread of its own writes out. When the backlog is full,
    lines are dropped and counted (runner.log still has every one)."""

    def __init__(self, target: TextIO, limit: int = CONSOLE_BACKLOG_LINES):
        self._target = target
        self._limit = limit
        self._lines: list[str] = []
        self._dropped = 0
        self._busy = False
        self._closed = False
        self._changed = threading.Condition()
        self._thread = threading.Thread(target=self._drain, name="desk-runner-console", daemon=True)
        self._thread.start()

    def write(self, text: str) -> int:
        with self._changed:
            if len(self._lines) >= self._limit:
                self._dropped += 1
            else:
                self._lines.append(text)
            self._changed.notify_all()
        return len(text)

    def flush(self) -> None:
        """Nothing to do: the thread flushes what it writes."""

    def _drain(self) -> None:
        while True:
            with self._changed:
                while not self._lines and not self._dropped and not self._closed:
                    self._changed.wait()
                if not self._lines and not self._dropped:
                    return
                batch, self._lines = self._lines, []
                dropped, self._dropped = self._dropped, 0
                self._busy = True
            try:
                self._target.write("".join(batch))
                if dropped:
                    self._target.write(f"... {dropped} line(s) were not shown while this window was busy; runner.log has them\n")
                self._target.flush()
            except (OSError, ValueError):
                pass
            with self._changed:
                self._busy = False
                self._changed.notify_all()

    def close(self, seconds: float) -> bool:
        """Write out what is left, waiting at most `seconds`. False when the
        window did not take it in that time."""
        deadline = time.monotonic() + seconds
        with self._changed:
            self._closed = True
            self._changed.notify_all()
            while self._lines or self._dropped or self._busy:
                left = deadline - time.monotonic()
                if left <= 0:
                    return False
                self._changed.wait(left)
        return True


def command_run(root: str, max_jobs: int) -> int:
    try:
        sys.stdout.reconfigure(errors="backslashreplace")  # type: ignore[union-attr]
    except (AttributeError, ValueError):
        pass
    console = BackgroundWriter(sys.stdout)
    try:
        runner = Runner(root, max_jobs=max_jobs, out=console)  # type: ignore[arg-type]
    except RunnerError as err:
        print(f"desk runner: {err}")
        return 2
    done = threading.Event()
    keep = _prepare_console(runner, done)
    try:
        return runner.run()
    finally:
        console.close(2.0)
        done.set()
        del keep


# --------------------------------------------------------------------------
# The other side: submit, status, stop, cancel, install


def runner_state(root_real: str, clock: Any = None) -> dict[str, Any]:
    """Is a runner alive for this root? The lock says whether its process
    exists; the heartbeat says whether it is still going round its loop."""
    clock = clock or RealClock()
    layout = Layout(root_real)
    beat = read_json(layout.heartbeat)
    held = InstanceLock.held_by_someone(layout.lock)
    age = None
    if beat is not None and isinstance(beat.get("beat_unix"), (int, float)):
        age = clock.time() - float(beat["beat_unix"])
    alive = held and age is not None and age < HEARTBEAT_STALE_S and beat is not None and beat.get("state") == "running"
    return {
        "alive": bool(alive),
        "lock_held": held,
        "heartbeat_age_seconds": age,
        "heartbeat": beat,
        "installed": os.path.isfile(layout.start_cmd),
    }


def host_name() -> str:
    """This machine's name, without asking the network."""
    name = os.environ.get("COMPUTERNAME", "")
    if not name and hasattr(os, "uname"):
        name = os.uname().nodename
    return printable(name or "that machine", 40)


def not_running_message(root_real: str, state: dict[str, Any]) -> str:
    """One line: what is wrong and exactly what the owner has to do."""
    layout = Layout(root_real)
    host = host_name()
    if state["lock_held"]:
        age = state["heartbeat_age_seconds"]
        shown = "none" if age is None else f"{age:.0f} s old"
        return (
            f"the desk runner on {host} is not responding (heartbeat {shown}): close its window "
            f"\"ASAMU-decomp desk runner\" and double-click {layout.start_cmd} again"
        )
    if not state["installed"]:
        return f"the desk runner is not installed on {host}: run \"tools/dev/win_host.sh desk-install\" first"
    return (
        f"the desk runner is not running on {host}: at that machine's own screen, double-click "
        f"{layout.start_cmd} (once per sign-in), leave its window open, then run this again"
    )


PACKED_KEYS = frozenset({"program", "args", "cwd", "timeout_seconds", "env"})


def pack_job(
    program: str, args: list[str], cwd: str, timeout: float, env: dict[str, str], job_id: str | None = None
) -> str:
    """A job as one token that survives any command line (URL-safe base64 of
    JSON). With `job_id` the sender chooses the id, so that it can cancel
    the job without having heard back."""
    obj: dict[str, Any] = {
        "program": program,
        "args": list(args),
        "cwd": cwd,
        "timeout_seconds": timeout,
        "env": dict(env),
    }
    if job_id is not None:
        obj["id"] = job_id
    return base64.urlsafe_b64encode(json.dumps(obj).encode("utf-8")).decode("ascii")


def unpack_job(token: str) -> dict[str, Any]:
    try:
        obj = json.loads(base64.urlsafe_b64decode(token.encode("ascii")).decode("utf-8"))
    except (ValueError, UnicodeError) as err:
        raise JobRejected(f"--packed is not a packed job ({type(err).__name__})") from None
    if not isinstance(obj, dict) or set(obj) - {"id"} != PACKED_KEYS:
        raise JobRejected("--packed is not a packed job")
    return obj


def parse_env_pairs(pairs: list[str]) -> dict[str, str]:
    env: dict[str, str] = {}
    for pair in pairs:
        name, sep, value = pair.partition("=")
        if not sep or not name:
            raise JobRejected(f"environment setting '{printable(pair, 60)}' is not NAME=VALUE")
        env[name.strip()] = value
    return env


def new_job_id(clock: Any) -> str:
    return time.strftime("%Y%m%d-%H%M%S", time.gmtime(clock.time())) + "-" + secrets.token_hex(4)


def _pump(path: str, offset: int, sink: BinaryIO) -> int:
    """Copy what was appended to `path` since `offset` to `sink`."""
    try:
        with open(path, "rb") as handle:
            handle.seek(offset)
            while True:
                chunk = handle.read(65536)
                if not chunk:
                    break
                sink.write(chunk)
                offset += len(chunk)
        sink.flush()
    except OSError:
        pass
    return offset


def exit_code_for(status: dict[str, Any]) -> int:
    """The exit code `submit` ends with for a finished job's status."""
    state = status.get("state")
    if state == "finished":
        code = status.get("exit_code")
        if not isinstance(code, int) or isinstance(code, bool):
            return EXIT_RUNNER
        if code < 0:
            return 128 + min(-code, 127)
        return code & 0xFFFFFFFF
    if state == "timed_out":
        return EXIT_TIMED_OUT
    if state == "failed_to_start":
        return EXIT_NOT_STARTED
    if state == "cancelled":
        return EXIT_CANCELLED
    return EXIT_RUNNER


def command_submit(
    root: str,
    job_fields: dict[str, Any],
    wait: bool = True,
    out: BinaryIO | None = None,
    err: TextIO | None = None,
    clock: Any = None,
) -> int:
    """Enqueue one job; by default wait for it, stream its output and return
    its exit code. `job_fields` may carry an "id" chosen by the caller.
    Interrupting the wait (Ctrl+C) asks the runner to end the job."""
    clock = clock or RealClock()
    out = out if out is not None else sys.stdout.buffer
    err = err if err is not None else sys.stderr

    def note(text: str) -> None:
        err.write(f"desk: {text}\n")
        err.flush()

    try:
        root_real = resolve_root(root)
    except RunnerError as problem:
        note(str(problem))
        return EXIT_RUNNER
    layout = Layout(root_real)
    state = runner_state(root_real, clock)
    if not state["alive"]:
        note(not_running_message(root_real, state))
        return EXIT_RUNNER
    job_fields = dict(job_fields)
    job_id = job_fields.pop("id", None) or new_job_id(clock)
    if not is_job_id(job_id):
        note(f"refused before sending: {BAD_JOB_ID}")
        return EXIT_RUNNER
    if os.path.lexists(layout.status(job_id)):
        # The runner keeps the first result of an id and would not run this
        # one; waiting would then hand back the old result as if it were new.
        note(f"refused before sending: a job with the id {job_id} already has a result; use a new id")
        return EXIT_RUNNER
    obj = {"version": PROTOCOL_VERSION, "id": job_id, **job_fields}
    try:
        job = validate_job(obj, job_id, root_real, Policy.host())
    except JobRejected as problem:
        note(f"refused before sending: {problem}")
        return EXIT_RUNNER
    beat = state["heartbeat"] or {}
    if beat.get("interactive") is False:
        note(
            f"note: the runner is not in a desktop session (window station "
            f"'{printable(beat.get('window_station'), 40)}'): no window will be visible"
        )
    target = os.path.join(layout.inbox, job_id + ".json")
    try:
        with os.fdopen(open_own_file(target + ".tmp", "new"), "w", encoding="utf-8", newline="\n") as handle:
            json.dump(obj, handle)
        os.replace(target + ".tmp", target)
    except OSError as problem:
        note(f"could not write the job file ({type(problem).__name__})")
        return EXIT_RUNNER
    note(f"job {job_id} sent: {show_command(job.argv)}")
    if not wait:
        out.write((job_id + "\n").encode("ascii"))
        out.flush()
        return 0

    try:
        return _wait_for_job(root_real, job_id, out, err, note, clock)
    except KeyboardInterrupt:
        try:
            request_cancel(layout, job_id)
            note(f"interrupted: asked the runner to end job {job_id}")
        except OSError:
            note(f"interrupted; job {job_id} goes on until it ends or its time limit passes")
        return EXIT_CANCELLED


def _wait_for_job(
    root_real: str, job_id: str, out: BinaryIO, err: TextIO, note: Callable[[str], None], clock: Any
) -> int:
    """Follow a job to its end: its state changes on `err`, its output as it
    is written, and at the end the exit code for it."""
    layout = Layout(root_real)
    stderr_sink = getattr(err, "buffer", None)
    offsets = [0, 0]
    announced = ""
    sent_at = last_alive_check = clock.monotonic()
    while True:
        status = read_json(layout.status(job_id)) or {}
        current = str(status.get("state", ""))
        if not current and clock.monotonic() - sent_at >= NOT_TAKEN_AFTER_S:
            # A runner that is alive takes a job within a second. Do not wait
            # for ever, and do not leave the job to start later unattended.
            try:
                request_cancel(layout, job_id)
            except OSError:
                pass
            note(
                f"the runner is alive but did not take job {job_id} within {NOT_TAKEN_AFTER_S:g} s (look at its window: "
                "is its inbox full of other files?); the job was withdrawn"
            )
            return EXIT_RUNNER
        if current != announced and current in ("queued", "running"):
            announced = current
            note(f"job {job_id} is {current}" + (f" (process {status.get('pid')})" if current == "running" else ""))
        # The state was read before the output: once the state says the job
        # is over, everything it wrote is in the files.
        offsets[0] = _pump(layout.stdout(job_id), offsets[0], out)
        if stderr_sink is not None:
            offsets[1] = _pump(layout.stderr(job_id), offsets[1], stderr_sink)
        if current in TERMINAL_STATES:
            took = status.get("duration_seconds")
            after = f" after {took} s" if took is not None else ""
            if current == "finished":
                note(f"job {job_id} finished: exit code {status.get('exit_code')}{after}")
            else:
                note(f"job {job_id} {current.replace('_', ' ')}{after}: {printable(status.get('reason'), 300)}")
            return exit_code_for(status)
        if clock.monotonic() - last_alive_check >= HEARTBEAT_PERIOD_S:
            last_alive_check = clock.monotonic()
            now_state = runner_state(root_real, clock)
            if not now_state["alive"]:
                clock.sleep(1.0)
                status = read_json(layout.status(job_id)) or {}
                if status.get("state") in TERMINAL_STATES:
                    continue
                note(f"the runner went away while job {job_id} was {current or 'not yet taken'}")
                note(not_running_message(root_real, now_state))
                return EXIT_RUNNER
        clock.sleep(0.2)


def request_cancel(layout: Layout, job_id: str) -> None:
    """Ask the runner to end a job (written like a job file: whole or not at all)."""
    target = os.path.join(layout.inbox, job_id + ".cancel")
    with os.fdopen(open_own_file(target + ".tmp", "new"), "w", encoding="ascii") as handle:
        handle.write("cancel\n")
    os.replace(target + ".tmp", target)


def command_status(root: str, as_json: bool, out: TextIO | None = None, clock: Any = None) -> int:
    out = out or sys.stdout
    try:
        root_real = resolve_root(root)
    except RunnerError as problem:
        out.write(f"desk: {problem}\n")
        return EXIT_RUNNER
    state = runner_state(root_real, clock)
    if as_json:
        out.write(json.dumps(state, indent=1, sort_keys=True) + "\n")
        return 0 if state["alive"] else 1
    if not state["alive"]:
        out.write(f"desk: {not_running_message(root_real, state)}\n")
        return 1
    beat = state["heartbeat"] or {}
    where = "in a desktop session" if beat.get("interactive") else "NOT in a desktop session (no window will be visible)"
    if beat.get("interactive") is None:
        where = "session kind unknown on this platform"
    # The heartbeat is a file anyone with the folder can write: nothing in it
    # is trusted to have the right type, and its text is escaped.
    running = beat.get("running") if isinstance(beat.get("running"), list) else []
    queued = beat.get("queued") if isinstance(beat.get("queued"), list) else []
    out.write(
        f"desk: runner is running (process {printable(beat.get('pid'), 12)}, heartbeat {state['heartbeat_age_seconds']:.1f} s ago, "
        f"{where}, window station '{printable(beat.get('window_station'), 40)}', session {printable(beat.get('session_id'), 12)}); "
        f"{len(running)} job(s) running, {len(queued)} waiting, "
        f"{printable(beat.get('jobs_started'), 12)} started and {printable(beat.get('jobs_refused'), 12)} refused since it began\n"
    )
    for entry in running[:HARD_MAX_JOBS]:
        if isinstance(entry, dict):
            out.write(f"desk:   job {printable(entry.get('id'), 70)}: {printable(entry.get('program'), 200)}\n")
    if beat.get("script_sha256") != own_digest():
        out.write(
            "desk: note: the running runner is a different version of desk_runner.py than this one; "
            "close its window and start it again to use the installed version\n"
        )
    return 0


def command_stop(root: str, out: TextIO | None = None, clock: Any = None, wait_seconds: float = 20.0) -> int:
    out = out or sys.stdout
    clock = clock or RealClock()
    try:
        root_real = resolve_root(root)
    except RunnerError as problem:
        out.write(f"desk: {problem}\n")
        return EXIT_RUNNER
    layout = Layout(root_real)
    if not InstanceLock.held_by_someone(layout.lock):
        out.write("desk: no runner is running; nothing to stop\n")
        return 0
    try:
        with os.fdopen(open_own_file(layout.stop, "new"), "w", encoding="ascii") as handle:
            handle.write("stop\n")
    except OSError as problem:
        out.write(f"desk: could not write the stop file ({type(problem).__name__})\n")
        return EXIT_RUNNER
    deadline = clock.monotonic() + wait_seconds
    while clock.monotonic() < deadline:
        if not InstanceLock.held_by_someone(layout.lock):
            out.write("desk: the runner stopped (its jobs were ended); its window can be closed\n")
            return 0
        clock.sleep(0.2)
    out.write(f"desk: the runner did not stop within {wait_seconds:g} s; close its window\n")
    return 1


def command_cancel(root: str, job_id: str, out: TextIO | None = None) -> int:
    out = out or sys.stdout
    if not is_job_id(job_id):
        out.write("desk: that is not a job id\n")
        return 2
    try:
        request_cancel(Layout(resolve_root(root)), job_id)
    except (RunnerError, OSError) as problem:
        out.write(f"desk: could not ask for the cancellation ({printable(problem)})\n")
        return EXIT_RUNNER
    out.write(f"desk: asked the runner to end job {job_id}\n")
    return 0


OWNER_INSTRUCTIONS = (
    "1. At that machine's own screen, signed in: open {root} in File Explorer and double-click start-runner.cmd.",
    '2. A console window titled "ASAMU-decomp desk runner" opens and says what it will and will not run. Leave it open (minimise it) while testing or playing.',
    "3. To stop it, close that window: everything it started ends with it. Start it again after each sign-in or restart.",
)


def start_cmd_text(python_exe: str | None) -> str:
    """The small batch file the owner double-clicks. It only starts the runner
    with the folder it lies in as the root, and keeps the window open at the end."""
    lines = [
        "@echo off",
        "rem Starts the ASAMU-decomp desk runner (tools/dev/desk_runner.py) for this folder.",
        "rem Written by 'desk_runner.py install'. It changes nothing on this computer;",
        "rem delete this file and the 'runner' folder to be rid of it.",
        "title ASAMU-decomp desk runner",
        "setlocal",
        'set "ASAMU_DESK_PY="',
    ]
    safe = python_exe is not None and re.fullmatch(r"[A-Za-z]:\\[A-Za-z0-9 _.()\\-]+\.exe", python_exe) is not None
    if safe:
        lines += [
            f'if not exist "{python_exe}" goto launcher',
            f'"{python_exe}" -I -X utf8 "%~dp0runner\\desk_runner.py" run --root "%~dp0."',
            "goto done",
            ":launcher",
        ]
    # `%%~$PATH:P` looks in the folders of PATH only. A bare `py` would be
    # looked for in the current folder first, which on a double-click is the
    # root: a file called py.cmd or py.exe there would be run instead.
    lines += [
        'for %%P in (py.exe) do if not "%%~$PATH:P"=="" set "ASAMU_DESK_PY=%%~$PATH:P"',
        "if not defined ASAMU_DESK_PY goto nopython",
        '"%ASAMU_DESK_PY%" -3 -I -X utf8 "%~dp0runner\\desk_runner.py" run --root "%~dp0."',
        "goto done",
        ":nopython",
        "echo Python 3 was not found on this computer, so the desk runner cannot start.",
        ":done",
        "echo.",
        "echo The desk runner is not running any more. Press a key to close this window.",
        "pause >nul",
    ]
    return "\r\n".join(lines) + "\r\n"


def command_install(root: str, out: TextIO | None = None) -> int:
    """Put this script at <root>/runner/desk_runner.py and write the batch file."""
    out = out or sys.stdout
    try:
        root_real = resolve_root(root)
        layout = Layout(root_real)
        try:
            os.mkdir(layout.base)
        except FileExistsError:
            pass
        Runner._check_own_folder(layout.base)
        source = os.path.abspath(__file__)
        if os.path.normcase(os.path.realpath(source)) != os.path.normcase(os.path.realpath(layout.script)):
            with open(source, "rb") as src:
                data = src.read()
            with os.fdopen(open_own_file(layout.script + ".tmp", "new"), "wb") as dst:
                dst.write(data)
            os.replace(layout.script + ".tmp", layout.script)
        python_exe = sys.executable if os.name == "nt" else None
        with os.fdopen(open_own_file(layout.start_cmd + ".tmp", "new"), "w", encoding="ascii", errors="replace", newline="") as handle:
            handle.write(start_cmd_text(python_exe))
        os.replace(layout.start_cmd + ".tmp", layout.start_cmd)
    except (RunnerError, OSError) as problem:
        out.write(f"desk: install failed: {printable(problem)}\n")
        return EXIT_RUNNER
    out.write(f"desk: installed {layout.script} and {layout.start_cmd}\n")
    out.write("desk: the owner of that machine starts the runner; it cannot be started from here:\n")
    for line in OWNER_INSTRUCTIONS:
        out.write("desk:   " + line.format(root=root_real) + "\n")
    return 0


# --------------------------------------------------------------------------


def _job_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--program", help="absolute path of the program (inside the root)")
    parser.add_argument("--cwd", help="working directory (inside the root)")
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S, help="seconds before the job is ended")
    parser.add_argument("--env", action="append", default=[], metavar="NAME=VALUE", help="ASAMU_*, WGPU_*, BEVY_*, RUST_BACKTRACE, RUST_LOG")
    parser.add_argument("--env-list", default="", metavar="A=1;B=2", help="several --env settings separated by ';'")
    parser.add_argument("--id", help="job id (default: made from the time and a random part)")
    parser.add_argument("args", nargs=argparse.REMAINDER, help="-- then the program's arguments")


def _job_fields(ns: argparse.Namespace) -> dict[str, Any]:
    if not ns.program or not ns.cwd:
        raise JobRejected("--program and --cwd are required")
    args = list(ns.args)
    if args and args[0] == "--":
        args = args[1:]
    pairs = list(ns.env) + [p for p in ns.env_list.split(";") if p.strip()]
    fields = {
        "program": ns.program,
        "args": args,
        "cwd": ns.cwd,
        "timeout_seconds": ns.timeout,
        "env": parse_env_pairs(pairs),
    }
    if ns.id:
        fields["id"] = ns.id
    return fields


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="desk_runner.py",
        description="Run this project's programs in the desktop session of a second machine. "
        "See the comment at the top of this file for the rules and the protocol.",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("run", help="the runner itself; the owner of the machine starts it")
    p.add_argument("--root", required=True)
    p.add_argument("--max-jobs", type=int, default=DEFAULT_MAX_JOBS)
    p = sub.add_parser("submit", help="enqueue a job, wait, stream its output, exit with its exit code")
    p.add_argument("--root", required=True)
    p.add_argument("--packed", help="a job made by 'pack' (instead of --program, --cwd and the rest)")
    p.add_argument("--no-wait", action="store_true", help="print the job id and return at once")
    _job_options(p)
    p = sub.add_parser("pack", help="encode a job as one token for 'submit --packed'")
    _job_options(p)
    p = sub.add_parser("status", help="is the runner alive (exit code 0) or not (1)")
    p.add_argument("--root", required=True)
    p.add_argument("--json", action="store_true")
    p = sub.add_parser("stop", help="make the runner end its jobs and exit")
    p.add_argument("--root", required=True)
    p = sub.add_parser("cancel", help="end one job")
    p.add_argument("--root", required=True)
    p.add_argument("job_id")
    p = sub.add_parser("install", help="copy this script to ROOT/runner and write ROOT/start-runner.cmd")
    p.add_argument("--root", required=True)
    ns = parser.parse_args(argv)

    if ns.command == "run":
        return command_run(ns.root, ns.max_jobs)
    if ns.command == "status":
        return command_status(ns.root, ns.json)
    if ns.command == "stop":
        return command_stop(ns.root)
    if ns.command == "cancel":
        return command_cancel(ns.root, ns.job_id)
    if ns.command == "install":
        return command_install(ns.root)
    try:
        if ns.command == "pack":
            fields = _job_fields(ns)
            print(
                pack_job(
                    fields["program"],
                    fields["args"],
                    fields["cwd"],
                    fields["timeout_seconds"],
                    fields["env"],
                    fields.get("id"),
                )
            )
            return 0
        fields = unpack_job(ns.packed) if ns.packed else _job_fields(ns)
    except JobRejected as problem:
        sys.stderr.write(f"desk: {problem}\n")
        return 2
    return command_submit(ns.root, fields, wait=not ns.no_wait)


if __name__ == "__main__":
    sys.exit(main())
