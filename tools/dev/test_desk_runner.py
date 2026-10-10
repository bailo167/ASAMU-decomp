#!/usr/bin/env python3
"""Tests of tools/dev/desk_runner.py (standard library only).

    python3 tools/dev/test_desk_runner.py            # all of them
    python3 tools/dev/test_desk_runner.py -k hostile # a selection

Three kinds:

* the checks a job has to pass, including hostile job files;
* the life cycle of a job with a fake clock and a fake launcher (no real
  process, no waiting);
* a few tests with real processes: the argument list arrives unchanged, the
  exit code comes back, and a job that spawns children and ignores every
  polite signal is ended as a whole tree.

The real-process tests need a native program inside the test's root folder
(the runner starts nothing else). They use a renamed copy of a small launcher
that the system already has (`/usr/bin/env`; the `py` launcher on Windows)
to start this Python; that also shows what the module comment says: the
refusal of shells and interpreters by name is a guard against mistakes, not
a sandbox. Tests that cannot run on a platform are skipped with the reason.
"""

from __future__ import annotations

import ast
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

sys.dont_write_bytecode = True  # leave no __pycache__ in the source tree
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import desk_runner as dr  # noqa: E402

WINDOWS = os.name == "nt"
WIN = dr.Policy(windows=True)
POSIX = dr.Policy(windows=False)
HOST = dr.Policy.host()


class FakeClock:
    """Time that moves only when a test says so."""

    def __init__(self) -> None:
        self.now = 1_800_000_000.0
        self.mono = 5_000.0
        self.slept = 0.0

    def time(self) -> float:
        return self.now

    def monotonic(self) -> float:
        return self.mono

    def advance(self, seconds: float) -> None:
        self.now += seconds
        self.mono += seconds

    def sleep(self, seconds: float) -> None:
        self.slept += seconds
        self.advance(seconds)


class FakeProcess:
    def __init__(self, pid: int, dies_when_killed: bool = True) -> None:
        self.pid = pid
        self.exit_code: int | None = None
        self.kills = 0
        self.closed = False
        self.dies_when_killed = dies_when_killed

    def poll(self) -> int | None:
        return self.exit_code

    def kill_tree(self) -> None:
        self.kills += 1
        if self.dies_when_killed and self.exit_code is None:
            self.exit_code = -9

    def wait(self, seconds: float) -> int | None:
        return self.exit_code

    def close(self) -> None:
        self.closed = True


class FakeLauncher:
    """Records what would have been started."""

    def __init__(self) -> None:
        self.calls: list[dict] = []
        self.procs: list[FakeProcess] = []
        self.fail_with: Exception | None = None
        self.dies_when_killed = True

    def start(self, argv, cwd, env, stdout_path, stderr_path):
        if self.fail_with is not None:
            raise self.fail_with
        for path in (stdout_path, stderr_path):
            with open(path, "wb"):
                pass
        self.calls.append({"argv": list(argv), "cwd": cwd, "env": dict(env), "stdout": stdout_path})
        proc = FakeProcess(4000 + len(self.procs), self.dies_when_killed)
        self.procs.append(proc)
        return proc


def make_program(folder: str, name: str = "tool") -> str:
    """A file that passes for a native program (it is never started)."""
    os.makedirs(folder, exist_ok=True)
    path = os.path.join(folder, name + (".exe" if WINDOWS else ""))
    with open(path, "wb") as handle:
        handle.write(b"MZ\x90\x00" if WINDOWS else b"\x7fELF\x02\x01\x01\x00")
    os.chmod(path, 0o755)
    return path


def tree_snapshot(root: str) -> dict[str, tuple[int, int]]:
    seen = {}
    for folder, dirs, files in os.walk(root):
        for name in dirs + files:
            path = os.path.join(folder, name)
            info = os.lstat(path)
            seen[os.path.relpath(path, root)] = (info.st_size if stat.S_ISREG(info.st_mode) else 0, info.st_mtime_ns)
    return seen


def try_symlink(target: str, link: str, is_dir: bool = False) -> bool:
    try:
        os.symlink(target, link, target_is_directory=is_dir)
    except (OSError, NotImplementedError):
        return False
    return True


class RootCase(unittest.TestCase):
    """A temporary root folder with one program, and a second folder outside it."""

    def setUp(self) -> None:
        self.base = os.path.realpath(tempfile.mkdtemp(prefix="desk-runner-test-"))
        self.addCleanup(shutil.rmtree, self.base, True)
        self.root = os.path.join(self.base, "root")
        self.outside = os.path.join(self.base, "outside")
        os.makedirs(self.root)
        os.makedirs(self.outside)
        self.work = os.path.join(self.root, "work")
        os.makedirs(self.work)
        self.program = make_program(os.path.join(self.root, "bin"))
        self.outside_program = make_program(self.outside, "elsewhere")
        self.layout = dr.Layout(self.root)

    def job(self, **changes) -> dict:
        obj = {
            "version": 1,
            "program": self.program,
            "args": ["--level", "graybox"],
            "cwd": self.work,
            "timeout_seconds": 60,
        }
        obj.update(changes)
        return obj

    def check(self, obj: dict, job_id: str = "job-1") -> dr.Job:
        return dr.validate_job(obj, job_id, self.root, HOST)

    def refused(self, obj: dict, part: str, job_id: str = "job-1") -> None:
        with self.assertRaises(dr.JobRejected) as caught:
            self.check(obj, job_id)
        self.assertIn(part, str(caught.exception))


# --------------------------------------------------------------------------


class PathSpelling(unittest.TestCase):
    """Refusals that need no file system, so the Windows rules run everywhere."""

    def refuse(self, text, policy, part):
        with self.assertRaises(dr.JobRejected) as caught:
            dr.check_path_text(text, "program", policy)
        self.assertIn(part, str(caught.exception))

    def test_windows_plain_paths_pass(self):
        self.assertEqual(
            dr.check_path_text(r"D:\asamu\target-h4\release\asamu.exe", "program", WIN),
            r"D:\asamu\target-h4\release\asamu.exe",
        )
        self.assertEqual(dr.check_path_text("D:/asamu/work/h4", "cwd", WIN), r"D:\asamu\work\h4")

    def test_windows_unc_and_device_paths(self):
        for text in (
            r"\\server\share\asamu.exe",
            "//server/share/asamu.exe",
            r"\\?\D:\asamu\asamu.exe",
            r"\\?\UNC\server\share\asamu.exe",
            r"\\.\pipe\thing",
            r"\\127.0.0.1\D$\asamu\asamu.exe",
        ):
            self.refuse(text, WIN, "network (UNC) or device path")

    def test_windows_relative_paths(self):
        for text in ("asamu.exe", r"release\asamu.exe", r"D:asamu.exe", r"\asamu\asamu.exe", r"..\asamu.exe", "cmd /c dir"):
            self.refuse(text, WIN, "absolute path with a drive letter")

    def test_windows_traversal(self):
        self.refuse(r"D:\asamu\..\Windows\System32\cmd.exe", WIN, "'..' component")
        self.refuse(r"D:\asamu\work\..\..\Windows\notepad.exe", WIN, "'..' component")
        self.refuse("D:/asamu/../Windows/System32/cmd.exe", WIN, "'..' component")
        self.refuse(r"D:\asamu\.\asamu.exe", WIN, "'.' component")

    def test_windows_streams_devices_and_odd_names(self):
        self.refuse(r"D:\asamu\asamu.exe:hidden.exe", WIN, "alternate data stream")
        self.refuse(r"D:\asamu\C:\Windows\cmd.exe", WIN, "alternate data stream")
        self.refuse(r"D:\asamu\NUL", WIN, "reserved device")
        self.refuse(r"D:\asamu\con.exe", WIN, "reserved device")
        self.refuse(r"D:\asamu\COM1.txt.exe", WIN, "reserved device")
        self.refuse("D:\\asamu\\asamu.exe.", WIN, "space or a dot")
        self.refuse("D:\\asamu\\asamu.exe ", WIN, "space or a dot")
        self.refuse("D:\\asamu\\a*.exe", WIN, "not allowed in a file name")
        self.refuse('D:\\asamu\\a".exe', WIN, "not allowed in a file name")

    def test_control_characters_length_and_types(self):
        for policy, good in ((WIN, "D:\\asamu\\"), (POSIX, "/srv/asamu/")):
            self.refuse(good + "a\nb", policy, "control characters")
            self.refuse(good + "a\x00b", policy, "control characters")
            self.refuse(good + "a\x1b[2Jb", policy, "control characters")
            self.refuse(good + "x" * 2000, policy, "longer than")
            self.refuse("", policy, "empty")
            for bad in (None, 7, ["D:\\asamu\\a.exe"], {"path": "x"}, b"/srv/asamu/a"):
                self.refuse(bad, policy, "must be a text string")

    def test_posix_rules(self):
        self.assertEqual(dr.check_path_text("/srv/asamu/bin/tool", "program", POSIX), "/srv/asamu/bin/tool")
        self.refuse("bin/tool", POSIX, "absolute path")
        self.refuse(r"\\server\share\tool", POSIX, "absolute path")
        self.refuse("//server/share/tool", POSIX, "two slashes")
        self.refuse("/srv/asamu/../../bin/sh", POSIX, "'..' component")
        self.refuse("/srv/asamu/./tool", POSIX, "'.' component")


class ProgramKind(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = os.path.realpath(tempfile.mkdtemp(prefix="desk-runner-kind-"))
        self.addCleanup(shutil.rmtree, self.dir, True)

    def file(self, name: str, data: bytes = b"MZ", mode: int = 0o755) -> str:
        path = os.path.join(self.dir, name)
        with open(path, "wb") as handle:
            handle.write(data)
        os.chmod(path, mode)
        return path

    def refuse(self, path: str, policy: dr.Policy, part: str) -> None:
        with self.assertRaises(dr.JobRejected) as caught:
            dr.check_program_file(path, policy)
        self.assertIn(part, str(caught.exception))

    def test_windows_runs_only_exe_files(self):
        dr.check_program_file(self.file("asamu.exe"), WIN)
        dr.check_program_file(self.file("asamu-import.EXE"), WIN)
        for name in ("build.cmd", "build.bat", "run.ps1", "tool.py", "tool.vbs", "tool.js", "tool.lnk", "tool.com", "tool.msi", "tool"):
            self.refuse(self.file(name), WIN, "not an .exe file")

    def test_shells_and_interpreters_are_refused_by_name(self):
        for name in ("cmd.exe", "CMD.EXE", "powershell.exe", "pwsh.exe", "python.exe", "pythonw.exe", "python3.13.exe",
                     "py.exe", "wscript.exe", "rundll32.exe", "reg.exe", "schtasks.exe", "shutdown.exe", "bash.exe",
                     "Steam.exe", "ASAMU-Win32-Shipping.exe", "PsExec64.exe"):
            self.refuse(self.file(name), WIN, "those are not run")
        if not WINDOWS:
            for name in ("sh", "bash", "zsh", "env", "python3", "python3.14", "perl", "osascript"):
                self.refuse(self.file(name, b"\x7fELF"), POSIX, "those are not run")

    def test_directories_are_not_programs(self):
        folder = os.path.join(self.dir, "folder.exe")
        os.makedirs(folder)
        self.refuse(folder, WIN, "not a regular file")

    @unittest.skipIf(WINDOWS, "execute permission bits and '#!' scripts are POSIX notions")
    def test_posix_refuses_scripts_and_files_without_execute_permission(self):
        dr.check_program_file(self.file("tool", b"\x7fELF"), POSIX)
        self.refuse(self.file("script", b"#!/bin/sh\necho hi\n"), POSIX, "is a script")
        self.refuse(self.file("data", b"\x7fELF", 0o644), POSIX, "not executable")


class JobChecks(RootCase):
    def test_a_good_job_passes_with_resolved_paths(self):
        job = self.check(self.job(env={"ASAMU_CONVERTED_DIR": "x", "RUST_BACKTRACE": "1"}))
        self.assertEqual(job.program, self.program)
        self.assertEqual(job.cwd, self.work)
        self.assertEqual(job.argv, [self.program, "--level", "graybox"])
        self.assertEqual(job.timeout_seconds, 60.0)
        self.assertEqual(dict(job.env), {"ASAMU_CONVERTED_DIR": "x", "RUST_BACKTRACE": "1"})

    def test_the_root_itself_may_be_the_working_directory_but_not_the_program(self):
        self.assertEqual(self.check(self.job(cwd=self.root)).cwd, self.root)
        self.refused(self.job(program=self.root), "outside")

    def test_hostile_program_outside_the_root(self):
        self.refused(self.job(program=self.outside_program), "outside")
        self.refused(self.job(program=os.path.realpath(sys.executable)), "outside")

    def test_hostile_path_traversal(self):
        sneaky = os.path.join(self.root, "bin", "..", "..", "outside", os.path.basename(self.outside_program))
        self.assertTrue(os.path.exists(sneaky))
        self.refused(self.job(program=sneaky), "'..' component")
        self.refused(self.job(cwd=os.path.join(self.root, "work", "..", "..", "outside")), "'..' component")

    def test_hostile_unc_path(self):
        if WINDOWS:
            self.refused(self.job(program=r"\\localhost\C$\Windows\System32\cmd.exe"), "network (UNC) or device path")
            self.refused(self.job(cwd=r"\\?\C:\Windows"), "network (UNC) or device path")
        else:
            self.refused(self.job(program="//localhost/share/tool"), "two slashes")
            self.refused(self.job(program=r"\\localhost\share\tool"), "absolute path")

    def test_hostile_symlink_to_a_program_outside(self):
        link = os.path.join(self.root, "bin", "linked" + (".exe" if WINDOWS else ""))
        if not try_symlink(self.outside_program, link):
            self.skipTest("this account may not create symbolic links")
        self.refused(self.job(program=link), "outside")

    def test_hostile_symlinked_folder_out_of_the_root(self):
        link = os.path.join(self.root, "escape")
        if not try_symlink(self.outside, link, is_dir=True):
            self.skipTest("this account may not create symbolic links")
        self.refused(self.job(program=os.path.join(link, os.path.basename(self.outside_program))), "outside")
        self.refused(self.job(cwd=link), "outside")

    @unittest.skipUnless(WINDOWS, "junctions exist only on Windows")
    def test_hostile_junction_out_of_the_root(self):
        link = os.path.join(self.root, "junction")
        made = subprocess.run(["cmd", "/c", "mklink", "/J", link, self.outside], capture_output=True)
        self.assertEqual(made.returncode, 0, made.stderr)
        self.addCleanup(os.rmdir, link)
        self.assertTrue(os.path.isfile(os.path.join(link, os.path.basename(self.outside_program))))
        self.refused(self.job(program=os.path.join(link, os.path.basename(self.outside_program))), "outside")
        self.refused(self.job(cwd=link), "outside")

    def test_symlink_that_stays_inside_the_root_is_fine(self):
        link = os.path.join(self.root, "inner")
        if not try_symlink(os.path.join(self.root, "bin"), link, is_dir=True):
            self.skipTest("this account may not create symbolic links")
        job = self.check(self.job(program=os.path.join(link, os.path.basename(self.program))))
        self.assertEqual(job.program, self.program)

    def test_missing_program_folder_as_program_file_as_cwd(self):
        self.refused(self.job(program=os.path.join(self.root, "bin", "absent" + (".exe" if WINDOWS else ""))), "does not exist")
        self.refused(self.job(program=self.work), "not a regular file")
        self.refused(self.job(cwd=self.program), "not a folder")
        self.refused(self.job(cwd=self.outside), "outside")

    def test_nothing_runs_from_the_runners_own_folder(self):
        inside = make_program(os.path.join(self.root, "runner", "inbox"), "planted")
        self.refused(self.job(program=inside), "runner's own folder")
        self.refused(self.job(cwd=os.path.join(self.root, "runner")), "runner's own folder")

    def test_a_command_string_is_never_accepted(self):
        self.refused(self.job(args="--level graybox & del /q *"), "never one command string")
        self.refused(self.job(args=None), "must be a list")
        self.refused(self.job(args={"0": "x"}), "must be a list")
        self.refused(self.job(args=["ok", 7]), "every argument must be a text string")
        self.refused(self.job(args=["ok", ["nested"]]), "every argument must be a text string")
        self.refused(self.job(args=["a\x00b"]), "NUL")
        self.refused(self.job(program=[self.program, "--x"]), "arguments go in 'args'")
        self.refused(self.job(program=self.program + " --extra"), "does not exist")

    def test_limits_on_arguments(self):
        self.refused(self.job(args=["x"] * (dr.MAX_ARGS + 1)), "more than")
        self.refused(self.job(args=["x" * (dr.MAX_ARG_CHARS + 1)]), "longer than")
        self.refused(self.job(args=["x" * 8000] * 5), "together are longer")
        self.check(self.job(args=["x"] * dr.MAX_ARGS))

    def test_time_limit_is_required_and_bounded(self):
        for bad in (0, -5, 0.5, dr.MAX_TIMEOUT_S + 1, 1e12):
            self.refused(self.job(timeout_seconds=bad), "must be between")
        for bad in (True, "60", None, [60], float("nan"), float("inf")):
            self.refused(self.job(timeout_seconds=bad), "must be a number")
        obj = self.job()
        del obj["timeout_seconds"]
        self.refused(obj, "missing field 'timeout_seconds'")
        self.assertEqual(self.check(self.job(timeout_seconds=dr.MAX_TIMEOUT_S)).timeout_seconds, dr.MAX_TIMEOUT_S)

    def test_environment_is_an_allow_list(self):
        for name in ("PATH", "Path", "PYTHONPATH", "COMSPEC", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "asamu_x", "ASAMU_", "X=Y", ""):
            self.refused(self.job(env={name: "1"}), "is not allowed")
        self.refused(self.job(env={"ASAMU_X": 1}), "needs a text value")
        self.refused(self.job(env={"ASAMU_X": "a\x00b"}), "needs a text value")
        self.refused(self.job(env=["ASAMU_X=1"]), "must be an object")
        self.refused(self.job(env={f"ASAMU_V{i}": "1" for i in range(dr.MAX_ENV_VARS + 1)}), "more than")

    def test_shape_of_the_job(self):
        self.refused(self.job(shell=True), "unknown field(s): shell")
        self.refused(self.job(command="cmd /c dir"), "unknown field(s): command")
        self.refused(self.job(version=2), "protocol version")
        self.refused(self.job(version=True), "protocol version")
        self.refused(self.job(version="1"), "protocol version")
        self.refused(self.job(id="another"), "does not match the file name")
        for key in dr.REQUIRED_JOB_KEYS:
            obj = self.job()
            del obj[key]
            self.refused(obj, f"missing field '{key}'")
        for bad_id in ("", "a b", "../x", "x" * 65, "-x", "x.y", "x/y", "x\\y"):
            self.refused(self.job(), "job id must be", job_id=bad_id)

    def test_job_file_bytes(self):
        def refuse(data: bytes, part: str) -> None:
            with self.assertRaises(dr.JobRejected) as caught:
                dr.parse_job_bytes(data)
            self.assertIn(part, str(caught.exception))

        self.assertEqual(dr.parse_job_bytes(b'{"version": 1}'), {"version": 1})
        refuse(b"\xff\xfe{\x00}\x00", "not UTF-8")
        refuse(b"", "not valid JSON")
        refuse(b"{", "not valid JSON")
        refuse(b'{"version": 1} trailing', "not valid JSON")
        refuse(b"[1, 2]", "one JSON object")
        refuse(b'"cmd /c dir"', "one JSON object")
        refuse(b'{"program": "a", "program": "b"}', "not valid JSON")
        refuse(b'{"timeout_seconds": NaN}', "not valid JSON")
        refuse(b'{"timeout_seconds": Infinity}', "not valid JSON")
        # Deep nesting: refused as too deep, or (where the JSON reader copes
        # with the depth) as not being an object. Never an uncaught error.
        refuse(b"[" * 60000, "")
        refuse(b"[" * 30000 + b"]" * 30000, "")
        refuse(b" " * (dr.MAX_JOB_FILE_BYTES + 1), "larger than")

    def test_root_must_be_a_real_project_folder(self):
        anchor = os.path.splitdrive(self.root)[0] + os.sep
        for bad in (anchor, os.path.join(self.root, "absent"), self.program):
            with self.assertRaises(dr.RunnerError):
                dr.resolve_root(bad)
        self.assertEqual(dr.resolve_root(self.root + os.sep + "."), self.root)


# --------------------------------------------------------------------------


class RunnerCase(RootCase):
    def setUp(self) -> None:
        super().setUp()
        self.clock = FakeClock()
        self.launcher = FakeLauncher()
        self.console = io.StringIO()
        self.runner = self.make_runner()

    def make_runner(self, **options) -> dr.Runner:
        runner = dr.Runner(
            self.root,
            clock=options.pop("clock", self.clock),
            launcher=options.pop("launcher", self.launcher),
            out=options.pop("out", self.console),
            **options,
        )
        self.addCleanup(runner.shutdown, "test over")
        return runner

    def start(self) -> None:
        self.runner.startup()

    def drop(self, job_id: str, obj: object = None, raw: bytes | None = None) -> str:
        """Put a job file in the inbox the way a client does (write, rename)."""
        path = os.path.join(self.layout.inbox, job_id + ".json")
        data = raw if raw is not None else json.dumps(obj if obj is not None else self.job()).encode("utf-8")
        with open(path + ".tmp", "wb") as handle:
            handle.write(data)
        os.replace(path + ".tmp", path)
        return path

    def status(self, job_id: str) -> dict:
        status = dr.read_json(self.layout.status(job_id))
        self.assertIsNotNone(status, f"no status for {job_id}")
        assert status is not None
        return status

    def state(self, job_id: str) -> str:
        return self.status(job_id)["state"]


class LifeCycle(RunnerCase):
    def test_banner_says_what_it_is_and_what_it_will_not_run(self):
        self.start()
        text = self.console.getvalue()
        for part in ("ASAMU-decomp desk runner", "WHAT IT RUNS", "WHAT IT NEVER RUNS", self.root,
                     "not the original game, not Steam", "no network port", "Close this window", "waiting for jobs"):
            self.assertIn(part, text)

    def test_heartbeat_is_written_and_refreshed(self):
        self.start()
        first = dr.read_json(self.layout.heartbeat)
        assert first is not None
        self.assertEqual(first["state"], "running")
        self.assertEqual(first["pid"], os.getpid())
        self.assertEqual(first["root"], self.root)
        self.assertEqual(first["beat_unix"], self.clock.now)
        self.clock.advance(dr.HEARTBEAT_PERIOD_S / 2)
        self.runner.step()
        self.assertEqual(dr.read_json(self.layout.heartbeat)["beat_unix"], first["beat_unix"])
        self.clock.advance(dr.HEARTBEAT_PERIOD_S)
        self.runner.step()
        self.assertEqual(dr.read_json(self.layout.heartbeat)["beat_unix"], self.clock.now)

    def test_refuses_to_start_twice(self):
        self.start()
        second = self.make_runner(out=io.StringIO())
        with self.assertRaises(dr.RunnerError) as caught:
            second.startup()
        self.assertIn("already running", str(caught.exception))
        self.assertEqual(second.run(), 3)
        self.assertTrue(dr.InstanceLock.held_by_someone(self.layout.lock))
        self.runner.shutdown("test")
        self.assertFalse(dr.InstanceLock.held_by_someone(self.layout.lock))
        third = self.make_runner(out=io.StringIO())
        third.startup()
        third.shutdown("test")

    def test_job_runs_and_finishes(self):
        self.start()
        nasty = ["--name", "two words", 'quote"inside', "a&b|c>d", "%PATH%", "$(id)", "`id`", "back\\slash\\", ""]
        path = self.drop("job-1", self.job(args=nasty, env={"ASAMU_CONVERTED_DIR": "D:\\asamu\\converted"}))
        self.runner.step()
        self.assertFalse(os.path.exists(path), "the job file is taken out of the inbox")
        self.assertEqual(len(self.launcher.calls), 1)
        call = self.launcher.calls[0]
        self.assertEqual(call["argv"], [self.program] + nasty, "the argument list arrives unchanged")
        self.assertEqual(call["cwd"], self.work)
        self.assertEqual(call["env"]["ASAMU_CONVERTED_DIR"], "D:\\asamu\\converted")
        self.assertEqual(call["stdout"], self.layout.stdout("job-1"))
        running = self.status("job-1")
        self.assertEqual(running["state"], "running")
        self.assertEqual(running["pid"], 4000)
        self.assertEqual(running["started_unix"], self.clock.now)
        self.assertIn("START job job-1", self.console.getvalue())
        self.assertEqual(dr.read_json(self.layout.heartbeat)["running"][0]["id"], "job-1")

        self.clock.advance(12.5)
        self.launcher.procs[0].exit_code = 7
        self.runner.step()
        done = self.status("job-1")
        self.assertEqual(done["state"], "finished")
        self.assertEqual(done["exit_code"], 7)
        self.assertEqual(done["duration_seconds"], 12.5)
        self.assertEqual(done["ended_unix"] - done["started_unix"], 12.5)
        self.assertEqual(done["args"], nasty)
        self.assertEqual(self.launcher.procs[0].kills, 1, "children a finished program left behind are ended")
        self.assertTrue(self.launcher.procs[0].closed)
        self.assertIn("END   job job-1: exit code 7", self.console.getvalue())
        self.assertEqual(dr.read_json(self.layout.heartbeat)["running"], [])

    def test_time_limit_ends_the_process_tree(self):
        self.start()
        self.drop("slow", self.job(timeout_seconds=30))
        self.runner.step()
        proc = self.launcher.procs[0]
        self.clock.advance(29.9)
        self.runner.step()
        self.assertEqual(self.state("slow"), "running")
        self.assertEqual(proc.kills, 0)
        self.clock.advance(0.2)
        self.runner.step()
        status = self.status("slow")
        self.assertEqual(status["state"], "timed_out")
        self.assertIsNone(status["exit_code"])
        self.assertTrue(status["process_tree_ended"])
        self.assertIn("limit of 30 s", status["reason"])
        self.assertEqual(proc.kills, 1)
        self.assertTrue(proc.closed)
        self.assertIn("ENDED job slow", self.console.getvalue())

    def test_a_process_that_survives_the_kill_is_reported(self):
        self.launcher.dies_when_killed = False
        self.start()
        self.drop("stubborn", self.job(timeout_seconds=5))
        self.runner.step()
        self.clock.advance(6)
        self.runner.step()
        status = self.status("stubborn")
        self.assertEqual(status["state"], "timed_out")
        self.assertFalse(status["process_tree_ended"])
        self.assertIn("did NOT end", self.console.getvalue())

    def test_at_most_a_few_jobs_at_a_time_the_rest_wait_in_order(self):
        self.start()
        for index in range(4):
            self.drop(f"job-{index}")
            self.clock.advance(0.01)
            os.utime(os.path.join(self.layout.inbox, f"job-{index}.json"), (self.clock.now, self.clock.now))
        self.runner.step()
        self.assertEqual(len(self.launcher.calls), dr.DEFAULT_MAX_JOBS)
        self.assertEqual([self.state(f"job-{i}") for i in range(4)], ["running", "running", "queued", "queued"])
        self.assertEqual(dr.read_json(self.layout.heartbeat)["queued"], ["job-2", "job-3"])
        self.launcher.procs[1].exit_code = 0
        self.runner.step()
        self.assertEqual([self.state(f"job-{i}") for i in range(4)], ["running", "finished", "running", "queued"])
        self.assertEqual(len(self.launcher.calls), 3)

    def test_max_jobs_is_bounded(self):
        for bad in (0, dr.HARD_MAX_JOBS + 1):
            with self.assertRaises(dr.RunnerError):
                dr.Runner(self.root, clock=self.clock, launcher=self.launcher, out=self.console, max_jobs=bad)

    def test_queue_is_bounded(self):
        self.start()
        total = dr.DEFAULT_MAX_JOBS + dr.MAX_QUEUED + 1
        for index in range(total):
            self.drop(f"job-{index:02d}")
            stamp = self.clock.now + index
            os.utime(os.path.join(self.layout.inbox, f"job-{index:02d}.json"), (stamp, stamp))
        self.runner.step()
        last = self.status(f"job-{total - 1:02d}")
        self.assertEqual(last["state"], "rejected")
        self.assertIn("too many jobs are waiting", last["reason"])
        self.assertEqual(len(self.launcher.calls), dr.DEFAULT_MAX_JOBS)

    def test_cancel_a_running_and_a_waiting_job(self):
        self.start()
        for index in range(3):
            self.drop(f"job-{index}")
            stamp = self.clock.now + index
            os.utime(os.path.join(self.layout.inbox, f"job-{index}.json"), (stamp, stamp))
        self.runner.step()
        self.assertEqual(dr.command_cancel(self.root, "job-2", io.StringIO()), 0)
        self.assertEqual(dr.command_cancel(self.root, "job-0", io.StringIO()), 0)
        self.assertEqual(dr.command_cancel(self.root, "../x", io.StringIO()), 2)
        self.runner.step()
        self.assertEqual(self.state("job-2"), "cancelled")
        self.assertEqual(self.state("job-0"), "cancelled")
        self.assertEqual(self.launcher.procs[0].kills, 1)
        self.assertEqual(self.state("job-1"), "running")
        self.assertEqual(len(self.launcher.calls), 2, "a cancelled waiting job never starts")
        self.assertEqual(os.listdir(self.layout.inbox), [])

    def test_output_limit(self):
        self.start()
        self.drop("noisy")
        self.runner.step()
        with mock.patch.object(dr, "MAX_OUTPUT_BYTES", 1000):
            with open(self.layout.stdout("noisy"), "wb") as handle:
                handle.write(b"x" * 1000)
            self.runner.step()
            self.assertEqual(self.state("noisy"), "running")
            with open(self.layout.stderr("noisy"), "wb") as handle:
                handle.write(b"x" * 1001)
            self.runner.step()
        self.assertEqual(self.state("noisy"), "output_limit")
        self.assertEqual(self.launcher.procs[0].kills, 1)

    def test_a_program_that_cannot_be_started(self):
        self.launcher.fail_with = dr.LaunchError("AssignProcessToJobObject failed (Windows error 5)")
        self.start()
        self.drop("broken")
        self.runner.step()
        status = self.status("broken")
        self.assertEqual(status["state"], "failed_to_start")
        self.assertIn("AssignProcessToJobObject", status["reason"])
        self.assertEqual(dr.exit_code_for(status), dr.EXIT_NOT_STARTED)

    def test_stop_file_ends_the_jobs_and_the_runner(self):
        self.start()
        for index in range(3):
            self.drop(f"job-{index}")
            stamp = self.clock.now + index
            os.utime(os.path.join(self.layout.inbox, f"job-{index}.json"), (stamp, stamp))
        self.assertTrue(self.runner.step())
        with open(self.layout.stop, "w") as handle:
            handle.write("stop\n")
        self.assertFalse(self.runner.step())
        self.assertEqual([self.state(f"job-{i}") for i in range(3)], ["runner_stopped"] * 3)
        self.assertEqual([p.kills for p in self.launcher.procs], [1, 1])
        self.assertEqual(dr.read_json(self.layout.heartbeat)["state"], "stopped")
        self.assertFalse(os.path.exists(self.layout.stop))
        self.assertFalse(dr.InstanceLock.held_by_someone(self.layout.lock))
        self.assertFalse(self.runner.step())
        self.assertEqual(self.runner.stop_reason, "stop file")

    def test_run_loop_ends_on_the_stop_file(self):
        stop_path = self.layout.stop

        class StopsLater(FakeClock):
            def sleep(self, seconds: float) -> None:
                super().sleep(seconds)
                if self.slept >= 3.0 and not os.path.exists(stop_path):
                    with open(stop_path, "w") as handle:
                        handle.write("stop\n")

        clock = StopsLater()
        runner = self.make_runner(clock=clock)
        self.assertEqual(runner.run(), 0)
        self.assertEqual(runner.stop_reason, "stop file")
        self.assertGreaterEqual(clock.slept, 3.0)
        self.assertLess(clock.slept, 4.0)
        self.assertIn("stopping: stop file", self.console.getvalue())
        self.assertFalse(dr.InstanceLock.held_by_someone(self.layout.lock))

    def test_closing_the_window_ends_the_jobs(self):
        self.start()
        self.drop("job-1")
        self.runner.step()
        self.runner.stop_requested.set()
        self.assertFalse(self.runner.step())
        self.assertEqual(self.state("job-1"), "runner_stopped")
        self.assertIn("window was closed", self.status("job-1")["reason"])
        self.assertEqual(self.launcher.procs[0].kills, 1)

    def test_a_leftover_stop_file_does_not_stop_a_new_runner(self):
        os.makedirs(self.layout.base)
        with open(self.layout.stop, "w") as handle:
            handle.write("stop\n")
        self.start()
        self.assertTrue(self.runner.step())

    def test_jobs_waiting_before_the_runner_started_do_not_run(self):
        os.makedirs(self.layout.inbox)
        self.drop("early")
        self.start()
        self.runner.step()
        self.assertEqual(self.launcher.calls, [])
        self.assertEqual(self.state("early"), "rejected")
        self.assertIn("submit it again", self.status("early")["reason"])
        self.assertEqual(os.listdir(self.layout.inbox), [])

    def test_old_results_are_pruned_at_start_and_only_those(self):
        os.makedirs(self.layout.outbox)
        old = self.layout.status("old-job")
        new = self.layout.status("new-job")
        other = os.path.join(self.layout.outbox, "notes of the owner.txt")
        for path in (old, new, other):
            with open(path, "w") as handle:
                handle.write("{}")
        ancient = self.clock.now - dr.RESULT_KEEP_S - 60
        os.utime(old, (ancient, ancient))
        os.utime(other, (ancient, ancient))
        os.utime(new, (self.clock.now, self.clock.now))
        self.start()
        self.assertFalse(os.path.exists(old))
        self.assertTrue(os.path.exists(new))
        self.assertTrue(os.path.exists(other))

    def test_writes_only_inside_its_own_folder(self):
        before = tree_snapshot(self.root)
        outside_before = tree_snapshot(self.outside)
        self.start()
        self.drop("job-1")
        self.drop("bad", raw=b"{")
        self.runner.step()
        self.clock.advance(100)
        self.runner.step()
        self.launcher.procs[0].exit_code = 0
        self.runner.step()
        self.runner.shutdown("test")
        after = tree_snapshot(self.root)
        changed = sorted(path for path in after if before.get(path) != after[path])
        self.assertTrue(changed)
        for path in changed:
            self.assertTrue(path == "runner" or path.startswith("runner" + os.sep), path)
        self.assertEqual(sorted(set(before) - set(after)), [])
        self.assertEqual(tree_snapshot(self.outside), outside_before)

    def test_console_shows_untrusted_text_harmlessly(self):
        self.assertEqual(dr.printable("a\x1b[2J\r\nb\u202e"), "a\\x1b[2J\\x0d\\x0ab\\u202e")
        self.assertEqual(len(dr.printable("x" * 1000, 50)), 53)
        self.assertEqual(dr.show_command(["a.exe", "two words", ""]), 'a.exe "two words" ""')
        self.start()
        self.drop("esc", self.job(program=self.program + "\x1b[2J"))
        self.runner.step()
        self.assertNotIn("\x1b", self.console.getvalue())
        with open(self.layout.log, encoding="utf-8") as handle:
            self.assertNotIn("\x1b", handle.read())


class HostileInbox(RunnerCase):
    def nothing_ran(self) -> None:
        self.assertEqual(self.launcher.calls, [])

    def reason(self, job_id: str) -> str:
        status = self.status(job_id)
        self.assertEqual(status["state"], "rejected")
        return status["reason"]

    def test_hostile_fifty_megabyte_job_file_is_refused_unread(self):
        self.start()
        path = os.path.join(self.layout.inbox, "huge.json")
        with open(path, "wb") as handle:
            handle.truncate(50 * 1024 * 1024)
        with mock.patch.object(self.runner, "_read_job_file", side_effect=AssertionError("must not be read")):
            self.runner.step()
        self.assertIn("larger than", self.reason("huge"))
        self.assertIn("52428800 bytes", self.reason("huge"))
        self.assertFalse(os.path.exists(path))
        self.nothing_ran()
        self.assertIn("REFUSED job 'huge'", self.console.getvalue())

    def test_hostile_file_that_grows_after_the_size_check(self):
        self.start()
        self.drop("grown", raw=b'{"version": 1, "pad": "' + b"x" * (dr.MAX_JOB_FILE_BYTES * 3) + b'"}')
        real_scandir = os.scandir

        class SmallStat:
            def __init__(self, entry):
                self.entry, self.name, self.path = entry, entry.name, entry.path

            def stat(self, follow_symlinks=True):
                info = self.entry.stat(follow_symlinks=follow_symlinks)
                return os.stat_result((info.st_mode, 0, 0, 1, 0, 0, 10, info.st_atime, info.st_mtime, info.st_ctime))

        with mock.patch.object(os, "scandir", lambda path: [SmallStat(e) for e in real_scandir(path)]):
            self.runner.step()
        self.assertIn("larger than", self.reason("grown"))
        self.nothing_ran()

    def test_hostile_malformed_job_files(self):
        self.start()
        cases = {
            "not-json": b"cmd /c del /s /q C:\\*",
            "empty": b"",
            "binary": bytes(range(256)),
            "array": json.dumps([self.program, "--x"]).encode("utf-8"),
            "deep": b"[" * 50000,
            "dup": json.dumps(self.job()).encode()[:-1] + b', "program": "x"}',
            "utf16": json.dumps(self.job()).encode("utf-16"),
        }
        for job_id, raw in cases.items():
            self.drop(job_id, raw=raw)
        self.runner.step()
        for job_id in cases:
            self.assertEqual(self.state(job_id), "rejected", job_id)
        self.nothing_ran()
        self.assertEqual(os.listdir(self.layout.inbox), [])
        with open(self.layout.rejected_log, encoding="utf-8") as handle:
            logged = [json.loads(line)["name"] for line in handle]
        self.assertEqual(sorted(logged), sorted(cases))
        self.runner._beat(force=True)
        self.assertEqual(dr.read_json(self.layout.heartbeat)["jobs_refused"], len(cases))

    def test_hostile_jobs_that_try_to_escape(self):
        self.start()
        cases = {
            "outside": (self.job(program=self.outside_program), "outside"),
            "traversal": (self.job(program=os.path.join(self.root, "bin", "..", "..", "outside", os.path.basename(self.outside_program))), "'..' component"),
            "unc": (self.job(program="\\\\server\\share\\tool.exe"), "UNC" if WINDOWS else "absolute path"),
            "unc2": (self.job(program="//server/share/tool.exe"), "UNC" if WINDOWS else "two slashes"),
            "cwd-out": (self.job(cwd=self.outside), "outside"),
            "string-args": (self.job(args="/c dir"), "never one command string"),
            "shell-key": (self.job(shell=True), "unknown field"),
            "no-limit": ({k: v for k, v in self.job().items() if k != "timeout_seconds"}, "missing field"),
            "env-path": (self.job(env={"PATH": self.outside}), "is not allowed"),
            "relative": (self.job(program="tool"), "absolute path"),
        }
        for job_id, (obj, _part) in cases.items():
            self.drop(job_id, obj)
        self.runner.step()
        for job_id, (_obj, part) in cases.items():
            self.assertIn(part, self.reason(job_id), job_id)
        self.nothing_ran()
        text = self.console.getvalue()
        self.assertEqual(text.count("REFUSED job"), len(cases))

    def test_hostile_symlink_and_junction_jobs(self):
        self.start()
        link = os.path.join(self.root, "escape")
        if not try_symlink(self.outside, link, is_dir=True):
            if not WINDOWS:
                self.skipTest("this account may not create symbolic links")
            made = subprocess.run(["cmd", "/c", "mklink", "/J", link, self.outside], capture_output=True)
            self.assertEqual(made.returncode, 0, made.stderr)
        self.addCleanup(lambda: os.path.lexists(link) and (os.rmdir(link) if WINDOWS else os.unlink(link)))
        self.drop("via-link", self.job(program=os.path.join(link, os.path.basename(self.outside_program))))
        self.drop("cwd-link", self.job(cwd=link))
        self.runner.step()
        self.assertIn("outside", self.reason("via-link"))
        self.assertIn("outside", self.reason("cwd-link"))
        self.nothing_ran()

    def test_hostile_program_swapped_for_a_link_while_the_job_waits(self):
        if not try_symlink(self.outside_program, os.path.join(self.root, "probe")):
            self.skipTest("this account may not create symbolic links")
        os.unlink(os.path.join(self.root, "probe"))
        self.start()
        second = make_program(os.path.join(self.root, "bin"), "second")
        self.drop("a")
        self.drop("b")
        self.drop("waits", self.job(program=second))
        for index, name in enumerate(("a", "b", "waits")):
            stamp = self.clock.now + index
            os.utime(os.path.join(self.layout.inbox, name + ".json"), (stamp, stamp))
        self.runner.step()
        self.assertEqual(self.state("waits"), "queued")
        os.unlink(second)
        os.symlink(self.outside_program, second)
        self.launcher.procs[0].exit_code = 0
        self.runner.step()
        self.assertEqual(len(self.launcher.calls), 2)
        self.assertIn("outside", self.reason("waits"))
        self.assertIn("REFUSED job 'waits'", self.console.getvalue())
        self.assertEqual(dr.read_json(self.layout.heartbeat)["queued"], [])

    def test_hostile_inbox_entries_that_are_not_plain_files(self):
        self.start()
        os.makedirs(os.path.join(self.layout.inbox, "folder.json"))
        secret = os.path.join(self.outside, "real-job.json")
        with open(secret, "w") as handle:
            json.dump(self.job(), handle)
        linked = try_symlink(secret, os.path.join(self.layout.inbox, "linked.json"))
        self.runner.step()
        self.runner.step()
        self.nothing_ran()
        text = self.console.getvalue()
        self.assertEqual(text.count("IGNORED inbox entry 'folder.json'"), 1, "reported once, not every round")
        self.assertTrue(os.path.isdir(os.path.join(self.layout.inbox, "folder.json")), "never deleted, never entered")
        if linked:
            self.assertEqual(text.count("IGNORED inbox entry 'linked.json'"), 1)
            self.assertTrue(os.path.exists(secret), "the target of a link is never touched")

    def test_hostile_file_names(self):
        self.start()
        for name in ("bad name!.json", "-dash.json", "x" * 80 + ".json", "notes.txt", "a.b.json"):
            with open(os.path.join(self.layout.inbox, name), "w") as handle:
                json.dump(self.job(), handle)
        with open(os.path.join(self.layout.inbox, "half-written.json.tmp"), "w") as handle:
            handle.write("{")
        self.runner.step()
        self.nothing_ran()
        self.assertEqual(os.listdir(self.layout.inbox), ["half-written.json.tmp"])
        self.assertEqual(os.listdir(self.layout.outbox), [], "no status file for a name that is not a job id")
        self.assertEqual(self.console.getvalue().count("bad job file name"), 4)
        self.assertIn("not a job file name", self.console.getvalue())

    def test_hostile_reuse_of_a_job_id_keeps_the_first_result(self):
        self.start()
        self.drop("same")
        self.runner.step()
        self.drop("same", self.job(args=["second"]))
        self.runner.step()
        self.assertEqual(len(self.launcher.calls), 1)
        self.assertEqual(self.state("same"), "running")
        self.launcher.procs[0].exit_code = 0
        self.runner.step()
        self.drop("same", self.job(args=["third"]))
        self.runner.step()
        self.assertEqual(len(self.launcher.calls), 1)
        self.assertEqual(self.status("same")["args"], ["--level", "graybox"])

    def test_hostile_runner_folder_replaced_by_a_link(self):
        os.makedirs(self.layout.base)
        if not try_symlink(self.outside, self.layout.inbox, is_dir=True):
            self.skipTest("this account may not create symbolic links")
        with self.assertRaises(dr.RunnerError) as caught:
            self.runner.startup()
        self.assertIn("is a link", str(caught.exception))
        self.assertEqual(sorted(os.listdir(self.outside)), [os.path.basename(self.outside_program)])

    @unittest.skipIf(WINDOWS, "on Windows the runner holds its folders in place while it runs, so this swap cannot "
                              "be made at all: see test_hostile_swap_of_a_runner_folder_cannot_even_be_made")
    def test_hostile_inbox_swapped_for_a_link_while_running(self):
        self.start()
        os.rmdir(self.layout.inbox)
        if not try_symlink(self.outside, self.layout.inbox, is_dir=True):
            self.skipTest("this account may not create symbolic links")
        with open(os.path.join(self.outside, "planted.json"), "w") as handle:
            json.dump(self.job(), handle)
        with self.assertRaises(dr.RunnerError):
            self.runner.step()
        self.nothing_ran()
        self.assertTrue(os.path.exists(os.path.join(self.outside, "planted.json")))

    @unittest.skipIf(WINDOWS, "on Windows the runner holds its folders in place while it runs, so this swap cannot "
                              "be made at all: see test_hostile_swap_of_a_runner_folder_cannot_even_be_made")
    def test_hostile_outbox_swapped_for_a_link_while_a_job_runs(self):
        if not try_symlink(self.outside, os.path.join(self.root, "probe"), is_dir=True):
            self.skipTest("this account may not create symbolic links")
        os.unlink(os.path.join(self.root, "probe"))
        outside_before = tree_snapshot(self.outside)
        seen: dict[str, dict] = {}
        test = self

        def own_files() -> dict:
            return {k: v for k, v in tree_snapshot(test.layout.base).items() if not k.startswith("outbox")}

        class Scripted(FakeClock):
            """The loop's pauses are where the outside world acts."""

            def sleep(self, seconds: float) -> None:
                super().sleep(seconds)
                if self.slept == dr.POLL_PERIOD_S:
                    test.drop("job-1")
                elif self.slept == 3 * dr.POLL_PERIOD_S:
                    for name in os.listdir(test.layout.outbox):
                        os.unlink(os.path.join(test.layout.outbox, name))
                    os.rmdir(test.layout.outbox)
                    os.symlink(test.outside, test.layout.outbox, target_is_directory=True)
                    seen["before"] = own_files()
                elif self.slept > 100:
                    raise AssertionError("the loop did not stop")

        runner = self.make_runner(clock=Scripted())
        self.assertEqual(runner.run(), 1, "the loop stops with an error")
        self.assertEqual([p.kills for p in self.launcher.procs], [1], "its job is ended")
        self.assertEqual(tree_snapshot(self.outside), outside_before, "nothing is written through the link")
        self.assertEqual(own_files(), seen["before"], "and nothing anywhere else either")
        self.assertFalse(dr.InstanceLock.held_by_someone(self.layout.lock))
        self.assertIn("is a link", self.console.getvalue())


# --------------------------------------------------------------------------


class Source(unittest.TestCase):
    """Properties of the runner that are read off its source."""

    def setUp(self) -> None:
        with open(dr.__file__, encoding="utf-8") as handle:
            self.tree = ast.parse(handle.read())

    def test_standard_library_only_and_no_network(self):
        imported = set()
        for node in ast.walk(self.tree):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom):
                imported.add((node.module or "").split(".")[0])
        allowed = {"__future__", "argparse", "base64", "ctypes", "dataclasses", "fcntl", "hashlib", "json", "math",
                   "msvcrt", "os", "re", "secrets", "signal", "stat", "subprocess", "sys", "threading",
                   "time", "typing"}
        self.assertEqual(imported - allowed, set())
        for network in ("socket", "http", "urllib", "asyncio", "ssl", "socketserver", "xmlrpc", "multiprocessing"):
            self.assertNotIn(network, imported)

    def test_no_shell_anywhere(self):
        popen_calls = 0
        for node in ast.walk(self.tree):
            if not isinstance(node, ast.Call):
                continue
            name = ast.unparse(node.func)
            self.assertNotIn(name, ("os.system", "os.popen", "subprocess.run", "subprocess.call",
                                    "subprocess.check_output", "subprocess.check_call", "subprocess.getoutput",
                                    "os.startfile", "eval", "exec"))
            self.assertFalse(name.startswith("os.spawn") or name.startswith("os.exec"), name)
            if name == "subprocess.Popen":
                popen_calls += 1
                keywords = {kw.arg: kw.value for kw in node.keywords}
                self.assertIsInstance(keywords["shell"], ast.Constant)
                self.assertIs(keywords["shell"].value, False)
                self.assertEqual(ast.unparse(keywords["stdin"]), "subprocess.DEVNULL")
                self.assertEqual(ast.unparse(node.args[0]), "argv")
        self.assertEqual(popen_calls, 2)


class ClientSide(RootCase):
    def test_not_running_is_one_line_with_the_exact_remedy(self):
        err = io.StringIO()
        code = dr.command_submit(self.root, self.job(), err=err, out=io.BytesIO(), clock=FakeClock())
        self.assertEqual(code, dr.EXIT_RUNNER)
        self.assertEqual(err.getvalue().count("\n"), 1)
        self.assertIn("is not installed", err.getvalue())
        self.assertIn("win_host.sh desk-install", err.getvalue())

        self.assertEqual(dr.command_install(self.root, io.StringIO()), 0)
        err = io.StringIO()
        code = dr.command_submit(self.root, self.job(), err=err, out=io.BytesIO(), clock=FakeClock())
        self.assertEqual(code, dr.EXIT_RUNNER)
        self.assertEqual(err.getvalue().count("\n"), 1)
        self.assertIn("is not running", err.getvalue())
        self.assertIn("double-click " + os.path.join(self.root, "start-runner.cmd"), err.getvalue())
        self.assertEqual(os.listdir(self.layout.base), ["desk_runner.py"], "nothing is queued for a runner that is not there")

        out = io.StringIO()
        self.assertEqual(dr.command_status(self.root, False, out, FakeClock()), 1)
        self.assertEqual(out.getvalue().count("\n"), 1)
        self.assertIn("double-click", out.getvalue())
        out = io.StringIO()
        self.assertEqual(dr.command_stop(self.root, out, FakeClock()), 0)
        self.assertIn("nothing to stop", out.getvalue())

    def test_status_of_a_running_and_of_a_stuck_runner(self):
        clock = FakeClock()
        runner = dr.Runner(self.root, clock=clock, launcher=FakeLauncher(), out=io.StringIO())
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        out = io.StringIO()
        self.assertEqual(dr.command_status(self.root, False, out, clock), 0)
        self.assertIn("runner is running", out.getvalue())
        self.assertIn("heartbeat 0.0 s ago", out.getvalue())
        state = dr.runner_state(self.root, clock)
        self.assertTrue(state["alive"])
        clock.advance(dr.HEARTBEAT_STALE_S + 1)
        out = io.StringIO()
        self.assertEqual(dr.command_status(self.root, False, out, clock), 1)
        self.assertIn("not responding", out.getvalue())
        self.assertEqual(out.getvalue().count("\n"), 1)
        runner.step()
        self.assertEqual(dr.command_status(self.root, False, io.StringIO(), clock), 0)
        runner.shutdown("test")
        self.assertFalse(dr.runner_state(self.root, clock)["alive"])

    def test_submit_refuses_a_bad_job_before_sending_it(self):
        clock = FakeClock()
        runner = dr.Runner(self.root, clock=clock, launcher=FakeLauncher(), out=io.StringIO())
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        err = io.StringIO()
        code = dr.command_submit(self.root, self.job(program=self.outside_program), err=err, out=io.BytesIO(), clock=clock)
        self.assertEqual(code, dr.EXIT_RUNNER)
        self.assertIn("refused before sending", err.getvalue())
        self.assertEqual(os.listdir(self.layout.inbox), [])

    def test_interrupting_the_wait_cancels_the_job(self):
        clock = FakeClock()
        launcher = FakeLauncher()
        runner = dr.Runner(self.root, clock=clock, launcher=launcher, out=io.StringIO())
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        fields = {k: v for k, v in self.job().items() if k != "version"}
        fields.update(env={}, id="chosen-id")
        err = io.StringIO()
        with mock.patch.object(clock, "sleep", side_effect=KeyboardInterrupt):
            code = dr.command_submit(self.root, fields, out=io.BytesIO(), err=err, clock=clock)
        self.assertEqual(code, dr.EXIT_CANCELLED)
        self.assertIn("interrupted: asked the runner to end job chosen-id", err.getvalue())
        self.assertEqual(sorted(os.listdir(self.layout.inbox)), ["chosen-id.cancel", "chosen-id.json"])
        for name in os.listdir(self.layout.inbox):
            os.utime(os.path.join(self.layout.inbox, name), (clock.now, clock.now))
        runner.step()
        status = dr.read_json(self.layout.status("chosen-id"))
        assert status is not None
        self.assertEqual(status["state"], "cancelled", "the cancellation does not overtake its job")
        self.assertEqual([p.kills for p in launcher.procs], [1])
        self.assertEqual(os.listdir(self.layout.inbox), [])

        fields["id"] = "../chosen"
        err = io.StringIO()
        self.assertEqual(dr.command_submit(self.root, fields, out=io.BytesIO(), err=err, clock=clock), dr.EXIT_RUNNER)
        self.assertIn("job id must be", err.getvalue())
        self.assertEqual(os.listdir(self.layout.inbox), [])

    def test_packed_job_survives_any_argument(self):
        args = ["two words", 'quote"inside', "trailing\\", "a&b|c", "%PATH%", "$HOME", "", "naïve ünïcode", "--x=y"]
        token = dr.pack_job("D:\\asamu\\a.exe", args, "D:\\asamu\\work", 30.0, {"ASAMU_X": "a b"})
        self.assertRegex(token, r"^[A-Za-z0-9_=-]+$")
        obj = dr.unpack_job(token)
        self.assertEqual(obj["args"], args)
        self.assertEqual(obj["program"], "D:\\asamu\\a.exe")
        self.assertEqual(obj["env"], {"ASAMU_X": "a b"})
        self.assertNotIn("id", obj)
        with_id = dr.unpack_job(dr.pack_job("D:\\asamu\\a.exe", [], "D:\\asamu", 30.0, {}, "my-id"))
        self.assertEqual(with_id["id"], "my-id")
        for bad in ("", "not base64 !!", "e30=", "WzFd", "eyJpZCI6ICJ4In0="):
            with self.assertRaises(dr.JobRejected):
                dr.unpack_job(bad)

    def test_pack_command_line(self):
        out = io.StringIO()
        with mock.patch.object(sys, "stdout", out):
            code = dr.main(["pack", "--id", "20261010-1", "--program", "D:\\asamu\\a.exe", "--cwd", "D:\\asamu", "--timeout", "5",
                            "--env", "ASAMU_A=1", "--env-list", "ASAMU_B=2;RUST_BACKTRACE=1", "--", "--help", "-x", "--"])
        self.assertEqual(code, 0)
        obj = dr.unpack_job(out.getvalue().strip())
        self.assertEqual(obj["args"], ["--help", "-x", "--"])
        self.assertEqual(obj["timeout_seconds"], 5.0)
        self.assertEqual(obj["id"], "20261010-1")
        self.assertEqual(obj["env"], {"ASAMU_A": "1", "ASAMU_B": "2", "RUST_BACKTRACE": "1"})

    def test_exit_codes(self):
        self.assertEqual(dr.exit_code_for({"state": "finished", "exit_code": 0}), 0)
        self.assertEqual(dr.exit_code_for({"state": "finished", "exit_code": 101}), 101)
        self.assertEqual(dr.exit_code_for({"state": "finished", "exit_code": -9}), 137)
        self.assertEqual(dr.exit_code_for({"state": "finished", "exit_code": 3221225477}), 3221225477)
        self.assertEqual(dr.exit_code_for({"state": "finished", "exit_code": None}), dr.EXIT_RUNNER)
        self.assertEqual(dr.exit_code_for({"state": "timed_out"}), 124)
        self.assertEqual(dr.exit_code_for({"state": "failed_to_start"}), 126)
        self.assertEqual(dr.exit_code_for({"state": "cancelled"}), 130)
        for state in ("rejected", "output_limit", "runner_stopped", "nonsense", None):
            self.assertEqual(dr.exit_code_for({"state": state}), 125)

    def test_start_cmd_only_starts_the_runner(self):
        text = dr.start_cmd_text(r"C:\Program Files (x86)\Python313\python.exe")
        self.assertIn('if not exist "C:\\Program Files (x86)\\Python313\\python.exe" goto launcher', text)
        self.assertTrue(text.endswith("\r\n"))
        self.assertNotIn("\n", text.replace("\r\n", ""))
        text.encode("ascii")
        run_lines = [line for line in text.split("\r\n") if "desk_runner.py" in line and not line.startswith("rem")]
        self.assertEqual(len(run_lines), 2)
        for line in run_lines:
            self.assertTrue(line.endswith(' -I -X utf8 "%~dp0runner\\desk_runner.py" run --root "%~dp0."'), line)
        for word in ("cmd /c", "powershell", "schtasks", "reg ", "start ", "setx", "runas", "http"):
            self.assertNotIn(word, text.lower())
        for odd in (None, 'C:\\a"b\\python.exe', "C:\\a%PATH%\\python.exe", "C:\\a&b\\python.exe", "python.exe", "\\\\srv\\s\\python.exe"):
            fallback = dr.start_cmd_text(odd)
            self.assertEqual(fallback.count("desk_runner.py\" run"), 1, odd)
            self.assertIn('"%ASAMU_DESK_PY%" -3 -I', fallback)
            for line in fallback.split("\r\n"):
                # A bare `py` or `where` is looked for in the current folder first (the root, on a double-click).
                self.assertFalse(line.lower().startswith(("py ", "where ")), line)

    def test_install_writes_the_script_and_the_batch_file(self):
        out = io.StringIO()
        self.assertEqual(dr.command_install(self.root, out), 0)
        with open(self.layout.script, "rb") as installed, open(dr.__file__, "rb") as source:
            self.assertEqual(installed.read(), source.read())
        with open(self.layout.start_cmd, "rb") as handle:
            self.assertEqual(handle.read().decode("ascii"), dr.start_cmd_text(sys.executable if WINDOWS else None))
        self.assertEqual(sorted(os.listdir(self.root)), ["bin", "runner", "start-runner.cmd", "work"])
        lines = [line for line in out.getvalue().splitlines() if line.startswith("desk:   ")]
        self.assertEqual(len(lines), 3, "the instruction for the owner has three lines")
        self.assertIn("double-click start-runner.cmd", lines[0])
        self.assertEqual(dr.command_install(self.root, io.StringIO()), 0)
        self.assertEqual(dr.command_install(os.path.join(self.root, "absent"), io.StringIO()), dr.EXIT_RUNNER)


# --------------------------------------------------------------------------
# Real processes


def pid_alive(pid: int) -> bool:
    if WINDOWS:
        import ctypes

        k32 = ctypes.WinDLL("kernel32")  # type: ignore[attr-defined]
        k32.OpenProcess.restype = ctypes.c_void_p
        k32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        k32.CloseHandle.argtypes = [ctypes.c_void_p]
        handle = k32.OpenProcess(0x00100000 | 0x1000, 0, pid)
        if not handle:
            return False
        try:
            return k32.WaitForSingleObject(handle, 0) == 258
        finally:
            k32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:
        # A process that has ended but was not collected yet still has an id.
        state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    except OSError:
        return True
    return bool(state) and not state.startswith("Z")


HOSTILE_SCRIPT = r"""
import os, signal, subprocess, sys, time
for name in ("SIGTERM", "SIGINT", "SIGHUP", "SIGBREAK", "SIGQUIT"):
    if hasattr(signal, name):
        signal.signal(getattr(signal, name), signal.SIG_IGN)
pid_dir, depth, mode = sys.argv[1], int(sys.argv[2]), sys.argv[3]
if depth > 0:
    for _ in range(2 if depth == 2 else 1):
        subprocess.Popen([sys.executable, __file__, pid_dir, str(depth - 1), "stay"])
os.makedirs(pid_dir, exist_ok=True)
with open(os.path.join(pid_dir, str(os.getpid())), "w"):
    pass  # one file per process: appends to one shared file can overwrite each other
print("started", os.getpid(), flush=True)
if mode == "leave":
    time.sleep(1.0)
    sys.exit(0)
while True:
    time.sleep(0.2)
"""

ECHO_SCRIPT = r"""
import json, os, sys
print(json.dumps({"args": sys.argv[1:], "cwd": os.getcwd(), "env": os.environ.get("ASAMU_TEST_VALUE")}))
sys.stderr.write("to stderr\n")
sys.exit(int(os.environ.get("ASAMU_TEST_EXIT", "0")))
"""


_NATIVE_STARTER: list[tuple[str, list[str]] | None] = []


def native_starter() -> tuple[str, list[str]] | None:
    """A native program that, copied to another folder under another name,
    starts a Python: (file to copy, arguments to put first). Candidates are
    tried for real, because not every copy runs (macOS ends a copied system
    binary; a copied python.exe misses its DLL)."""
    if _NATIVE_STARTER:
        return _NATIVE_STARTER[0]
    candidates: list[tuple[str, list[str]]] = []
    if WINDOWS:
        launcher = shutil.which("py")
        if launcher:
            candidates.append((launcher, ["-3"]))
    else:
        candidates.append(("/usr/bin/env", [sys.executable]))
    candidates.append((os.path.realpath(sys.executable), []))
    found = None
    folder = tempfile.mkdtemp(prefix="desk-runner-starter-")
    try:
        for index, (source, prefix) in enumerate(candidates):
            copy = os.path.join(folder, f"starter{index}" + (".exe" if WINDOWS else ""))
            try:
                shutil.copyfile(source, copy)
                os.chmod(copy, 0o755)
                done = subprocess.run([copy] + prefix + ["-c", "import sys; sys.exit(42)"], capture_output=True, timeout=60)
            except (OSError, subprocess.SubprocessError):
                continue
            if done.returncode == 42:
                found = (source, prefix)
                break
    finally:
        shutil.rmtree(folder, True)
    _NATIVE_STARTER.append(found)
    return found


class RealProcessCase(RootCase):
    """A real launcher, and inside the root a native program that starts a
    Python: a renamed copy of a launcher the system already has."""

    def setUp(self) -> None:
        super().setUp()
        self.native = os.path.join(self.root, "bin", "starter" + (".exe" if WINDOWS else ""))
        choice = native_starter()
        if choice is None:
            self.skipTest("no native program found that still starts this Python when copied into the root")
        source, self.prefix = choice
        shutil.copyfile(source, self.native)
        os.chmod(self.native, 0o755)
        self.scratch = os.path.join(self.root, "scratch")
        os.makedirs(self.scratch)
        self.pids: list[int] = []
        self.addCleanup(self.kill_leftovers)

    def kill_leftovers(self) -> None:
        for pid in self.pids:
            if pid_alive(pid):
                try:
                    os.kill(pid, 9)
                except OSError:
                    pass

    def script(self, name: str, text: str) -> str:
        path = os.path.join(self.scratch, name)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    def read_pids(self, pid_file: str, count: int, seconds: float = 30.0) -> list[int]:
        deadline = time.monotonic() + seconds
        pids: list[int] = []
        while time.monotonic() < deadline:
            try:
                pids = sorted(int(name) for name in os.listdir(pid_file))
            except (OSError, ValueError):
                pids = []
            if len(pids) >= count:
                break
            time.sleep(0.05)
        self.pids = pids
        self.assertEqual(len(pids), count, "the job did not start its whole process tree in time")
        return pids

    def wait_all_gone(self, pids: list[int], seconds: float = 10.0) -> list[int]:
        deadline = time.monotonic() + seconds
        left = list(pids)
        while left and time.monotonic() < deadline:
            left = [pid for pid in left if pid_alive(pid)]
            if left:
                time.sleep(0.05)
        return left


class RealProcesses(RealProcessCase):
    def test_hostile_job_that_spawns_children_and_ignores_the_time_limit(self):
        """Five processes in three generations, all ignoring every signal that
        can be ignored; the runner's clock passes the limit; all five end."""
        clock = FakeClock()
        console = io.StringIO()
        runner = dr.Runner(self.root, clock=clock, out=console)
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        pid_file = os.path.join(self.scratch, "pids")
        script = self.script("hostile.py", HOSTILE_SCRIPT)
        job = self.job(program=self.native, args=self.prefix + [script, pid_file, "2", "stay"], timeout_seconds=30)
        with open(os.path.join(self.layout.inbox, "tree.json"), "w") as handle:
            json.dump(job, handle)
        self.assertTrue(runner.step())
        status = dr.read_json(self.layout.status("tree"))
        assert status is not None
        self.assertEqual(status["state"], "running", status)
        pids = self.read_pids(pid_file, 5)
        self.assertEqual(len(set(pids)), 5)
        self.assertEqual([pid for pid in pids if not pid_alive(pid)], [])

        clock.advance(29.0)
        runner.step()
        self.assertEqual(dr.read_json(self.layout.status("tree"))["state"], "running")
        self.assertEqual([pid for pid in pids if not pid_alive(pid)], [], "nothing is ended before the limit")

        clock.advance(2.0)
        runner.step()
        status = dr.read_json(self.layout.status("tree"))
        assert status is not None
        self.assertEqual(status["state"], "timed_out")
        self.assertTrue(status["process_tree_ended"])
        self.assertEqual(self.wait_all_gone(pids), [], "every process of the job is gone")
        self.assertFalse(pid_alive(status["pid"]))
        with open(self.layout.stdout("tree"), "rb") as handle:
            self.assertEqual(handle.read().count(b"started"), 5)

    def test_children_left_behind_by_a_finished_job_are_ended(self):
        clock = FakeClock()
        runner = dr.Runner(self.root, clock=clock, out=io.StringIO())
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        pid_file = os.path.join(self.scratch, "pids")
        script = self.script("hostile.py", HOSTILE_SCRIPT)
        job = self.job(program=self.native, args=self.prefix + [script, pid_file, "1", "leave"], timeout_seconds=600)
        with open(os.path.join(self.layout.inbox, "leaver.json"), "w") as handle:
            json.dump(job, handle)
        runner.step()
        pids = self.read_pids(pid_file, 2)
        deadline = time.monotonic() + 30
        state = "running"
        while state == "running" and time.monotonic() < deadline:
            time.sleep(0.05)
            runner.step()
            state = dr.read_json(self.layout.status("leaver"))["state"]
        status = dr.read_json(self.layout.status("leaver"))
        assert status is not None
        self.assertEqual(status["state"], "finished")
        self.assertEqual(status["exit_code"], 0)
        self.assertEqual(self.wait_all_gone(pids), [], "the child it left running is ended with the job")

    def test_stopping_the_runner_ends_the_tree(self):
        clock = FakeClock()
        runner = dr.Runner(self.root, clock=clock, out=io.StringIO())
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        pid_file = os.path.join(self.scratch, "pids")
        script = self.script("hostile.py", HOSTILE_SCRIPT)
        job = self.job(program=self.native, args=self.prefix + [script, pid_file, "2", "stay"], timeout_seconds=600)
        with open(os.path.join(self.layout.inbox, "tree.json"), "w") as handle:
            json.dump(job, handle)
        runner.step()
        pids = self.read_pids(pid_file, 5)
        out = io.StringIO()
        stopper = threading.Thread(target=dr.command_stop, args=(self.root, out))
        stopper.start()
        deadline = time.monotonic() + 15
        while runner.step() and time.monotonic() < deadline:
            time.sleep(0.05)
        stopper.join(20)
        self.assertIn("the runner stopped", out.getvalue())
        self.assertEqual(dr.read_json(self.layout.status("tree"))["state"], "runner_stopped")
        self.assertEqual(self.wait_all_gone(pids), [])

    def test_launcher_refuses_what_is_not_a_program(self):
        launcher = dr.ProcessLauncher()
        out, err = os.path.join(self.scratch, "o.txt"), os.path.join(self.scratch, "e.txt")
        with self.assertRaises(dr.LaunchError):
            launcher.start([self.program, "x"], self.work, dict(os.environ), out, err)
        with self.assertRaises(dr.LaunchError):
            launcher.start([os.path.join(self.root, "bin", "absent.exe")], self.work, dict(os.environ), out, err)


class EndToEndCase(RealProcessCase):
    """Runner and client as two parties with the real clock: the runner loop
    in a thread, `submit` as the tools call it."""

    def setUp(self) -> None:
        super().setUp()
        self.console = io.StringIO()
        self.runner = dr.Runner(self.root, out=self.console)
        self.thread = threading.Thread(target=self.runner.run, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_runner)
        deadline = time.monotonic() + 10
        while not dr.runner_state(self.root)["alive"] and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue(dr.runner_state(self.root)["alive"])

    def stop_runner(self) -> None:
        self.runner.stop_requested.set()
        self.thread.join(30)

    def submit(self, **changes):
        out, err = io.BytesIO(), io.TextIOWrapper(io.BytesIO(), encoding="utf-8", write_through=True)
        fields = {k: v for k, v in self.job(**changes).items() if k != "version"}
        fields.setdefault("env", {})
        code = dr.command_submit(self.root, fields, out=out, err=err)
        return code, out.getvalue(), err.buffer.getvalue().decode("utf-8")


class EndToEnd(EndToEndCase):
    def test_arguments_output_and_exit_code_make_the_round_trip(self):
        script = self.script("echo.py", ECHO_SCRIPT)
        nasty = ["two words", 'quote"inside', "a&b|c>d<e", "%PATH%", "$(id)", "`id`", "trailing\\", "x;y", "*", "naïve"]
        code, out, err = self.submit(
            program=self.native,
            args=self.prefix + [script] + nasty,
            env={"ASAMU_TEST_VALUE": "from the job", "ASAMU_TEST_EXIT": "7"},
        )
        self.assertEqual(code, 7, err)
        seen = json.loads(out.decode("utf-8"))
        self.assertEqual(seen["args"], nasty, "no shell touched the arguments")
        self.assertEqual(os.path.realpath(seen["cwd"]), self.work)
        self.assertEqual(seen["env"], "from the job")
        self.assertIn("to stderr", err)
        self.assertIn("finished: exit code 7", err)
        self.assertIn("START job", self.console.getvalue())
        self.assertIn("exit code 7", self.console.getvalue())

    def test_time_limit_seen_from_the_client(self):
        script = self.script("hostile.py", HOSTILE_SCRIPT)
        pid_file = os.path.join(self.scratch, "pids")
        started = time.monotonic()
        code, out, err = self.submit(
            program=self.native, args=self.prefix + [script, pid_file, "2", "stay"], timeout_seconds=6
        )
        took = time.monotonic() - started
        self.assertEqual(code, dr.EXIT_TIMED_OUT, err)
        self.assertIn("timed out", err)
        self.assertGreaterEqual(took, 6.0)
        self.assertLess(took, 25.0)
        pids = self.read_pids(pid_file, 5, seconds=1.0)
        self.assertEqual(self.wait_all_gone(pids), [])
        self.assertEqual(out.count(b"started"), 5)

    def test_refusal_seen_from_the_client(self):
        """The client checks first; here its check is bypassed so that the
        runner's own refusal is what comes back."""
        real = dr.validate_job

        def checked(obj, job_id, root, policy):
            if threading.current_thread() is self.thread:
                return real(obj, job_id, root, policy)
            return dr.Job(job_id, obj["program"], tuple(obj["args"]), obj["cwd"], 5.0)

        with mock.patch.object(dr, "validate_job", checked):
            code, _out, err = self.submit(program=self.outside_program, args=[])
        self.assertEqual(code, dr.EXIT_RUNNER, err)
        self.assertIn("rejected", err)
        self.assertIn("outside", err)
        self.assertIn("REFUSED job", self.console.getvalue())

    def test_stop_from_the_client(self):
        out = io.StringIO()
        self.assertEqual(dr.command_stop(self.root, out), 0)
        self.assertIn("the runner stopped", out.getvalue())
        self.thread.join(10)
        self.assertFalse(self.thread.is_alive())
        self.assertFalse(dr.runner_state(self.root)["alive"])
        code, _out, err = self.submit()
        self.assertEqual(code, dr.EXIT_RUNNER)
        self.assertIn("is not", err)


CTRL_BREAK_DRIVER = r"""
# Runs in a console of its own: starts the runner in that console, waits for
# the 'go' file, sends Ctrl+Break to the runner and reports how it ended.
import os, signal, subprocess, sys, time
go, command = sys.argv[1], sys.argv[2:]
runner = subprocess.Popen(command, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
deadline = time.monotonic() + 120
while not os.path.exists(go) and time.monotonic() < deadline and runner.poll() is None:
    time.sleep(0.05)
if runner.poll() is None:
    os.kill(runner.pid, signal.CTRL_BREAK_EVENT)
try:
    sys.exit(100 + runner.wait(20))
except subprocess.TimeoutExpired:
    runner.kill()
    sys.exit(99)
"""


class RunnerProcess(RealProcessCase):
    """The runner as the owner starts it: a process of its own."""

    def runner_command(self) -> list[str]:
        return [sys.executable, "-I", "-X", "utf8", "-B", os.path.abspath(dr.__file__), "run", "--root", self.root]

    def wait_alive(self, proc: "subprocess.Popen[bytes]") -> None:
        deadline = time.monotonic() + 30
        while not dr.runner_state(self.root)["alive"] and time.monotonic() < deadline and proc.poll() is None:
            time.sleep(0.05)
        self.assertTrue(dr.runner_state(self.root)["alive"], "the runner process did not come up")

    def end(self, proc: "subprocess.Popen[bytes]") -> None:
        if proc.poll() is None:
            proc.kill()
        proc.wait(20)

    def spawn(self) -> "subprocess.Popen[bytes]":
        self.console_path = os.path.join(self.scratch, "runner-console.txt")
        log = open(self.console_path, "wb")
        self.addCleanup(log.close)
        proc = subprocess.Popen(self.runner_command(), stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        self.addCleanup(self.end, proc)
        self.wait_alive(proc)
        return proc

    def submit_tree(self, job_id: str) -> list[int]:
        script = self.script("hostile.py", HOSTILE_SCRIPT)
        pid_dir = os.path.join(self.scratch, "pids")
        fields = {
            "id": job_id,
            "program": self.native,
            "args": self.prefix + [script, pid_dir, "2", "stay"],
            "cwd": self.work,
            "timeout_seconds": 600,
            "env": {},
        }
        err = io.StringIO()
        self.assertEqual(dr.command_submit(self.root, fields, wait=False, out=io.BytesIO(), err=err), 0, err.getvalue())
        return self.read_pids(pid_dir, 5)

    def test_start_refuse_a_second_one_run_a_job_stop(self):
        proc = self.spawn()
        second = subprocess.run(self.runner_command(), stdin=subprocess.DEVNULL, capture_output=True, timeout=60)
        self.assertEqual(second.returncode, 3)
        self.assertIn(b"already running", second.stdout)
        self.assertNotIn(b"WHAT IT RUNS", second.stdout)
        self.assertIsNone(proc.poll(), "the first runner is not disturbed")

        script = self.script("echo.py", ECHO_SCRIPT)
        out, err = io.BytesIO(), io.StringIO()
        fields = {"program": self.native, "args": self.prefix + [script, "a b"], "cwd": self.work,
                  "timeout_seconds": 60, "env": {"ASAMU_TEST_EXIT": "3"}}
        self.assertEqual(dr.command_submit(self.root, fields, out=out, err=err), 3, err.getvalue())
        self.assertEqual(json.loads(out.getvalue().decode("utf-8"))["args"], ["a b"])

        stopped = io.StringIO()
        self.assertEqual(dr.command_stop(self.root, stopped), 0)
        self.assertEqual(proc.wait(20), 0)
        with open(self.console_path, encoding="utf-8") as handle:
            console = handle.read()
        for part in ("ASAMU-decomp desk runner", "WHAT IT NEVER RUNS", "START job", "exit code 3", "stopping: stop file"):
            self.assertIn(part, console)

    @unittest.skipUnless(WINDOWS, "only a Windows job object ends a job when the runner itself is killed; "
                                  "elsewhere a job's process group outlives a killed runner until its own end")
    def test_killing_the_runner_ends_its_jobs(self):
        proc = self.spawn()
        pids = self.submit_tree("orphan")
        self.assertEqual([pid for pid in pids if not pid_alive(pid)], [])
        proc.kill()
        proc.wait(20)
        self.assertEqual(self.wait_all_gone(pids), [], "nothing the runner started outlives it")
        self.assertFalse(dr.runner_state(self.root)["alive"])
        self.assertFalse(dr.InstanceLock.held_by_someone(self.layout.lock), "the lock is never stale")
        again = self.spawn()
        self.assertIsNone(again.poll(), "a new runner starts after the crash")

    @unittest.skipUnless(WINDOWS, "console control events are a Windows notion; elsewhere Ctrl+C is covered by "
                                  "the KeyboardInterrupt path")
    def test_console_event_stops_the_runner_in_an_orderly_way(self):
        """The path taken when the owner closes the window, driven by the
        one console event a program may send (Ctrl+Break), in a real console."""
        go = os.path.join(self.scratch, "go")
        info = subprocess.STARTUPINFO()  # type: ignore[attr-defined]
        info.dwFlags |= subprocess.STARTF_USESHOWWINDOW  # type: ignore[attr-defined]
        info.wShowWindow = 0  # SW_HIDE: no window flashes up when this runs on a desktop
        driver = subprocess.Popen(
            [sys.executable, "-I", "-c", CTRL_BREAK_DRIVER, go] + self.runner_command(),
            creationflags=subprocess.CREATE_NEW_CONSOLE,  # type: ignore[attr-defined]
            startupinfo=info,
        )
        self.addCleanup(self.end, driver)
        self.wait_alive(driver)
        pids = self.submit_tree("windowed")
        with open(go, "w"):
            pass
        self.assertEqual(driver.wait(60), 100, "the runner ended with exit code 0 after the console event")
        status = dr.read_json(self.layout.status("windowed"))
        assert status is not None
        self.assertEqual(status["state"], "runner_stopped")
        self.assertIn("Ctrl+Break", status["reason"])
        self.assertEqual(dr.read_json(self.layout.heartbeat)["state"], "stopped")
        self.assertEqual(self.wait_all_gone(pids), [])
        self.assertFalse(dr.InstanceLock.held_by_someone(self.layout.lock))


# --------------------------------------------------------------------------
# Attacks tried by the verification pass. Each test is one way in which a
# hostile job file, a planted link, a second start or a busy window was used
# against the runner; those marked "got through" did before they were fixed.


def dir_link(target: str, link: str) -> bool:
    """A link to a folder: a symbolic link, or on Windows a junction."""
    if try_symlink(target, link, is_dir=True):
        return True
    if WINDOWS:
        return subprocess.run(["cmd", "/c", "mklink", "/J", link, target], capture_output=True).returncode == 0
    return False


def remove_dir_link(link: str) -> None:
    if os.path.lexists(link):
        try:
            os.unlink(link)
        except OSError:
            os.rmdir(link)


class AttackCase(RunnerCase):
    """A runner with fakes, and outside the root a file that must stay as it is."""

    def setUp(self) -> None:
        super().setUp()
        self.victim = os.path.join(self.outside, "victim.txt")
        with open(self.victim, "w") as handle:
            handle.write("PRECIOUS\n")
        self.absent = os.path.join(self.outside, "not-there.txt")
        self.outside_before = tree_snapshot(self.outside)

    def link(self, link: str, target: str | None = None) -> None:
        if not try_symlink(target if target is not None else self.victim, link):
            self.skipTest("this account may not create symbolic links")

    def outside_untouched(self) -> None:
        self.assertEqual(tree_snapshot(self.outside), self.outside_before, "something outside the root was written")
        with open(self.victim) as handle:
            self.assertEqual(handle.read(), "PRECIOUS\n")

    def reason(self, job_id: str) -> str:
        status = self.status(job_id)
        self.assertEqual(status["state"], "rejected")
        return status["reason"]


class HostileValues(AttackCase):
    def test_hostile_time_limit_of_hundreds_of_digits_does_not_end_the_runner(self):
        """Got through: an integer too large for a float raised OverflowError
        in the check, and the runner ended with a traceback."""
        self.start()
        good = json.dumps(self.job()).encode("utf-8")
        self.assertIn(b'"timeout_seconds": 60', good)
        cases = {
            "big": b"1" + b"0" * 400,
            "negative": b"-1" + b"0" * 400,
            "digits": b"1" + b"0" * 6000,
            "exponent": b"1e999",
            "huge-float": b"1" + b"0" * 400 + b".5",
        }
        for job_id, number in cases.items():
            self.drop(job_id, raw=good.replace(b'"timeout_seconds": 60', b'"timeout_seconds": ' + number))
        self.assertTrue(self.runner.step())
        for job_id in cases:
            self.assertEqual(self.state(job_id), "rejected", job_id)
        self.assertIn("must be between", self.reason("big"))
        self.assertEqual(self.launcher.calls, [])
        self.drop("fine")
        self.assertTrue(self.runner.step(), "the runner goes on")
        self.assertEqual(self.state("fine"), "running")

    def test_hostile_job_that_breaks_a_check_is_refused_and_the_runner_goes_on(self):
        self.start()
        with mock.patch.object(dr, "validate_job", side_effect=RecursionError("too deep")):
            self.drop("odd")
            self.assertTrue(self.runner.step())
        self.assertIn("could not be checked (RecursionError)", self.reason("odd"))
        # The same at the moment a job that waited is started.
        for name in ("a", "b", "waits"):
            self.drop(name)
        self.runner.step()
        self.assertEqual(self.state("waits"), "queued")
        self.launcher.procs[0].exit_code = 0
        with mock.patch.object(dr, "validate_job", side_effect=OverflowError("too large")):
            self.assertTrue(self.runner.step())
        self.assertIn("could not be checked (OverflowError)", self.reason("waits"))
        self.assertEqual(len(self.launcher.calls), 2)
        self.assertTrue(self.runner.step())

    def test_hostile_names_that_end_in_a_line_feed(self):
        """Got through: `$` in a pattern also matches before a final line feed."""
        for name in ("RUST_LOG\n", "ASAMU_X\n", "RUST_BACKTRACE\n", "WGPU_BACKEND\n"):
            self.refused(self.job(env={name: "1"}), "is not allowed")
        self.assertFalse(dr.is_job_id("job-1\n"))
        self.refused(self.job(), "job id must be", job_id="job-1\n")
        self.assertEqual(dr.command_cancel(self.root, "job-1\n", io.StringIO()), 2)
        if WINDOWS:
            return  # no file name there can hold a line feed
        self.start()
        with open(os.path.join(self.layout.inbox, "nl\n.json"), "w") as handle:
            json.dump(self.job(), handle)
        self.runner.step()
        self.assertEqual(self.launcher.calls, [])
        self.assertEqual(os.listdir(self.layout.outbox), [], "no status file under such a name")
        self.assertIn("bad job file name", self.console.getvalue())
        self.assertNotIn("nl\n", self.console.getvalue())

    def test_hostile_job_ids_that_are_device_names(self):
        """Got through: `nul.json` or `com1.json` was taken as a job, and its
        status and output files were opened under the device's name."""
        for name in ("con", "NUL", "aux", "Prn", "com1", "LPT9"):
            self.assertFalse(dr.is_job_id(name), name)
            self.refused(self.job(), "device name", job_id=name)
            self.assertEqual(dr.command_cancel(self.root, name, io.StringIO()), 2)
        for name in ("console", "com10", "nul-1", "lpt", "a"):
            self.assertTrue(dr.is_job_id(name), name)
        self.start()
        dropped = 0
        for name in ("aux", "com1", "nul"):
            try:
                self.drop(name)
                dropped += 1
            except OSError:
                pass  # an older Windows cannot make such a file at all
        self.runner.step()
        self.assertEqual(self.launcher.calls, [])
        self.assertEqual(os.listdir(self.layout.outbox), [])
        self.assertEqual(self.console.getvalue().count("bad job file name"), dropped)
        fields = {k: v for k, v in self.job().items() if k != "version"}
        fields.update(env={}, id="aux")
        err = io.StringIO()
        self.assertEqual(dr.command_submit(self.root, fields, out=io.BytesIO(), err=err, clock=self.clock), dr.EXIT_RUNNER)
        self.assertIn("device name", err.getvalue())
        self.assertEqual(os.listdir(self.layout.inbox), [])

    def test_hostile_job_that_sets_path_or_comspec(self):
        names = ("PATH", "Path", "path", "COMSPEC", "ComSpec", "PATHEXT", "SystemRoot", "windir", "TEMP", "TMP",
                 "USERPROFILE", "APPDATA", "PYTHONSTARTUP", "__COMPAT_LAYER", "VK_ICD_FILENAMES", "VK_LAYER_PATH",
                 "ASAMU_X=1;PATH", "ASAMU_X\x00PATH", " ASAMU_X", "ASAMU_X ", "ASAMU_\u00e9", "RUST_LOG2", "rust_log")
        for name in names:
            self.refused(self.job(env={name: self.outside}), "is not allowed")
        self.start()
        self.drop("path", self.job(env={"PATH": self.outside}))
        self.drop("comspec", self.job(env={"ASAMU_OK": "1", "COMSPEC": self.outside_program}))
        self.drop("good", self.job(env={"ASAMU_OK": "1"}))
        self.runner.step()
        self.assertIn("is not allowed", self.reason("path"))
        self.assertIn("is not allowed", self.reason("comspec"))
        self.assertEqual(len(self.launcher.calls), 1)
        env = self.launcher.calls[0]["env"]
        self.assertEqual(env.get("PATH"), os.environ.get("PATH"), "a job gets the runner's own PATH, never one it asked for")
        self.assertEqual(env.get("COMSPEC"), os.environ.get("COMSPEC"))
        self.assertEqual(env["ASAMU_OK"], "1")

    def test_hostile_environment_variables_in_paths_are_not_expanded(self):
        for text in (r"%SystemRoot%\System32\cmd.exe", r"%ASAMU_ROOT%\tool.exe", r"$env:SystemRoot\cmd.exe", r"~\tool.exe",
                     r"%D%:\asamu\tool.exe"):
            with self.assertRaises(dr.JobRejected):
                dr.check_path_text(text, "program", WIN)
        literal = r"D:\asamu\%SystemRoot%\cmd.exe"
        self.assertEqual(dr.check_path_text(literal, "program", WIN), literal, "kept as the literal name it is")
        for text in ("$HOME/tool", "~/tool", "${ASAMU_ROOT}/tool"):
            with self.assertRaises(dr.JobRejected):
                dr.check_path_text(text, "program", POSIX)
        # On this host, with a variable that would lead to a real program if anything expanded it.
        with mock.patch.dict(os.environ, {"ASAMU_TEST_DIR": "bin"}):
            name = os.path.basename(self.program)
            for folder in ("%ASAMU_TEST_DIR%", "$ASAMU_TEST_DIR", "${ASAMU_TEST_DIR}"):
                self.refused(self.job(program=os.path.join(self.root, folder, name)), "does not exist")
                self.refused(self.job(cwd=os.path.join(self.root, folder)), "does not exist")

    def test_hostile_mixed_separators_and_odd_spellings(self):
        cases = {
            "D:/asamu\\..//Windows\\System32/cmd.exe": "'..' component",
            "D:\\asamu/work\\..\\..\\Windows/notepad.exe": "'..' component",
            "D:/asamu/.\\tool.exe": "'.' component",
            "//?/D:/asamu/tool.exe": "network (UNC) or device path",
            "\\/server/share/tool.exe": "network (UNC) or device path",
            "/\\server\\share\\tool.exe": "network (UNC) or device path",
            "\\\\.\\D:\\asamu\\tool.exe": "network (UNC) or device path",
            "\\\\?\\GLOBALROOT\\Device\\HarddiskVolume3\\asamu\\tool.exe": "network (UNC) or device path",
            "\\??\\D:\\asamu\\tool.exe": "absolute path with a drive letter",
            "D:\\asamu\\tool.exe::$DATA": "alternate data stream",
            "D:\\asamu\\bin:$I30:$INDEX_ALLOCATION\\tool.exe": "alternate data stream",
            "D:\\asamu\\tool.exe . .": "space or a dot",
            "D:\\asamu\\bin.\\tool.exe": "space or a dot",
            "D:\\asamu\\bin \\tool.exe": "space or a dot",
            "D:\\asamu\\...\\tool.exe": "space or a dot",
            "D:\\asamu\\aux\\tool.exe": "reserved device",
            "D:\\asamu\\CONIN$": "reserved device",
            "D:\\asamu\\tool?.exe": "not allowed in a file name",
            "D:\\asamu\\tool.exe|more": "not allowed in a file name",
            "D:\\asamu\\<tool>.exe": "not allowed in a file name",
        }
        for text, part in cases.items():
            with self.assertRaises(dr.JobRejected, msg=text) as caught:
                dr.check_path_text(text, "program", WIN)
            self.assertIn(part, str(caught.exception), text)

    @unittest.skipUnless(WINDOWS, "8.3 short names exist only on Windows")
    def test_hostile_short_name_spelling_is_judged_by_the_real_name(self):
        import ctypes

        def short(path: str) -> str:
            buf = ctypes.create_unicode_buffer(1024)
            ctypes.WinDLL("kernel32").GetShortPathNameW(path, buf, 1024)  # type: ignore[attr-defined]
            return buf.value

        folder = os.path.join(self.root, "a folder with a long name")
        shell = make_program(folder, "powershell")
        good = make_program(folder, "a program with a long name")
        if os.path.normcase(short(good)) == os.path.normcase(good):
            self.skipTest("this volume makes no 8.3 short names")
        self.refused(self.job(program=short(shell)), "is a shell")
        self.assertEqual(self.check(self.job(program=short(good))).program, good)
        self.assertEqual(self.check(self.job(cwd=short(folder))).cwd, folder)


class HostileLinks(AttackCase):
    """Got through, all of them: the runner's folders were checked for links,
    its files were not, and a file opened for writing follows a link."""

    def test_hostile_link_where_a_jobs_output_goes(self):
        self.start()
        out, err = self.layout.stdout("j1"), self.layout.stderr("j1")
        self.link(out)
        self.link(err, self.absent)
        with self.assertRaises(dr.LaunchError):  # the file is no program; its output files are opened first
            dr.ProcessLauncher().start([self.program, "x"], self.work, dict(os.environ), out, err)
        self.outside_untouched()
        for path in (out, err):
            self.assertTrue(stat.S_ISREG(os.lstat(path).st_mode), "the link was replaced by a file of the runner's own")

    def test_hostile_link_where_a_status_or_the_heartbeat_is_written(self):
        self.start()
        self.link(self.layout.status("bad") + ".tmp")
        self.link(self.layout.heartbeat + ".tmp", self.absent)
        self.drop("bad", raw=b"{")
        self.runner.step()
        self.clock.advance(dr.HEARTBEAT_PERIOD_S + 1)
        self.runner.step()
        self.assertEqual(self.state("bad"), "rejected")
        self.assertEqual(dr.read_json(self.layout.heartbeat)["beat_unix"], self.clock.now)
        self.outside_untouched()

    def test_hostile_links_in_place_of_the_runners_own_files(self):
        os.makedirs(self.layout.base)
        for name in ("runner.lock", "runner.log", "rejected.log", "heartbeat.json"):
            for target in (self.victim, self.absent):
                with self.subTest(name=name, dangling=target == self.absent):
                    path = os.path.join(self.layout.base, name)
                    self.link(path, target)
                    runner = self.make_runner(out=io.StringIO())
                    with self.assertRaises(dr.RunnerError) as caught:
                        runner.startup()
                    self.assertIn("is a link or not a plain file", str(caught.exception))
                    self.assertEqual(runner.run(), 3, "and it says so instead of starting")
                    self.outside_untouched()
                    os.unlink(path)
        self.runner.startup()  # with the links gone it starts
        self.outside_untouched()

    def test_hostile_second_name_for_a_log(self):
        """A hard link has no target to follow: the log would simply be the other file."""
        os.makedirs(self.layout.base)
        try:
            os.link(self.victim, self.layout.log)
        except (OSError, NotImplementedError):
            self.skipTest("hard links cannot be made here")
        with self.assertRaises(dr.RunnerError) as caught:
            self.runner.startup()
        self.assertIn("is a link or not a plain file", str(caught.exception))
        with open(self.victim) as handle:
            self.assertEqual(handle.read(), "PRECIOUS\n")

    def test_hostile_link_put_in_place_of_a_log_while_running(self):
        self.start()
        os.unlink(self.layout.log)
        self.link(self.layout.log)
        self.link(self.layout.rejected_log, self.absent)
        self.drop("bad", raw=b"{")
        self.assertTrue(self.runner.step())
        self.assertIn("REFUSED job 'bad'", self.console.getvalue(), "the window still says what happened")
        self.assertEqual(self.state("bad"), "rejected")
        self.outside_untouched()

    def test_hostile_link_in_place_of_the_whole_runner_folder(self):
        """Got through: the inbox and outbox were created through the link
        before the folders were checked."""
        if not dir_link(self.outside, self.layout.base):
            self.skipTest("this account may not create links to folders")
        self.addCleanup(remove_dir_link, self.layout.base)
        with self.assertRaises(dr.RunnerError) as caught:
            self.runner.startup()
        self.assertIn("is a link", str(caught.exception))
        self.outside_untouched()
        self.assertEqual(dr.command_install(self.root, io.StringIO()), dr.EXIT_RUNNER)
        self.outside_untouched()
        self.assertFalse(os.path.lexists(self.layout.start_cmd))

    def test_hostile_links_where_the_client_writes(self):
        self.start()
        self.link(os.path.join(self.layout.inbox, "c1.json.tmp"))
        self.link(os.path.join(self.layout.inbox, "c1.cancel.tmp"), self.absent)
        self.link(self.layout.stop)
        fields = {k: v for k, v in self.job().items() if k != "version"}
        fields.update(env={}, id="c1")
        err = io.StringIO()
        self.assertEqual(dr.command_submit(self.root, fields, wait=False, out=io.BytesIO(), err=err, clock=self.clock), 0, err.getvalue())
        dr.request_cancel(self.layout, "c1")
        self.assertEqual(dr.command_stop(self.root, io.StringIO(), self.clock, wait_seconds=0.0), 1)
        self.outside_untouched()
        for name in ("c1.json", "c1.cancel"):
            self.assertTrue(stat.S_ISREG(os.lstat(os.path.join(self.layout.inbox, name)).st_mode))
        self.assertTrue(stat.S_ISREG(os.lstat(self.layout.stop).st_mode))
        self.assertFalse(self.runner.step(), "the stop file stops the runner")
        self.outside_untouched()

    def test_hostile_links_where_install_writes(self):
        os.makedirs(self.layout.base)
        self.link(self.layout.start_cmd)
        self.link(self.layout.script, self.absent)
        self.assertEqual(dr.command_install(self.root, io.StringIO()), 0)
        self.outside_untouched()
        for path in (self.layout.start_cmd, self.layout.script):
            self.assertTrue(stat.S_ISREG(os.lstat(path).st_mode))
        with open(self.layout.script, "rb") as installed, open(dr.__file__, "rb") as source:
            self.assertEqual(installed.read(), source.read())

    @unittest.skipUnless(hasattr(os, "mkfifo"), "named pipes in the file system are a POSIX notion")
    def test_hostile_named_pipe_where_a_file_is_expected_does_not_hold_up_the_runner(self):
        """Opening a named pipe waits for its other end; nobody comes."""
        self.start()
        os.unlink(self.layout.log)
        os.mkfifo(self.layout.log)
        pipe_job = os.path.join(self.layout.inbox, "pipe.json")
        os.mkfifo(pipe_job)
        self.addCleanup(lambda: [os.path.lexists(p) and os.unlink(p) for p in (self.layout.log, pipe_job)])
        result: list[object] = []
        finished = threading.Event()

        def work() -> None:
            result.append(self.runner._read_job_file(pipe_job))
            self.drop("bad", raw=b"{")
            result.append(self.runner.step())
            finished.set()

        threading.Thread(target=work, daemon=True).start()
        self.assertTrue(finished.wait(15), "the runner hangs on a named pipe")
        self.assertEqual(result, [None, True])
        self.assertEqual(self.state("bad"), "rejected")
        self.assertEqual(self.launcher.calls, [])


class HostileFlood(AttackCase):
    def test_hostile_inbox_full_of_folders_is_not_reported_for_ever(self):
        """Got through: past the number of names it remembers, the runner
        reported every such entry again in every round."""
        self.start()
        with mock.patch.object(dr, "MAX_REMEMBERED_NAMES", 8):
            for index in range(20):
                os.mkdir(os.path.join(self.layout.inbox, f"folder-{index:02}"))
            for _ in range(6):
                self.assertTrue(self.runner.step())
        text = self.console.getvalue()
        self.assertEqual(text.count("IGNORED inbox entry"), 8)
        self.assertEqual(text.count("further ones are not reported"), 1)
        with open(self.layout.rejected_log, encoding="utf-8") as handle:
            self.assertEqual(len(handle.readlines()), 8)
        self.assertEqual(len(os.listdir(self.layout.inbox)), 20, "never deleted, never entered")

    def test_hostile_inbox_with_more_entries_than_a_round_looks_at(self):
        """The loop that reads the inbox also enforces the time limits: the
        work of one round is bounded however full the inbox is."""
        self.start()
        self.drop("slow", self.job(timeout_seconds=30))
        self.runner.step()
        for index in range(60):
            with open(os.path.join(self.layout.inbox, f"junk-{index:02}.txt"), "w"):
                pass
        real_scandir = os.scandir
        taken: list[str] = []

        def counting(path):
            with real_scandir(path) as listing:
                for entry in listing:
                    taken.append(entry.name)
                    yield entry

        with mock.patch.object(dr, "MAX_INBOX_ENTRIES_EXAMINED", 10), mock.patch.object(os, "scandir", counting):
            self.clock.advance(31)
            self.assertTrue(self.runner.step())
            self.assertLessEqual(len(taken), 11)
            self.assertEqual(self.state("slow"), "timed_out", "the time limit is enforced in that same round")
            for _ in range(8):
                self.runner.step()
        self.assertEqual(self.console.getvalue().count("the inbox holds more than 10 entries"), 1)
        self.assertEqual(os.listdir(self.layout.inbox), [], "what does not belong there is cleared round by round")

    def test_hostile_flood_of_cancellations_is_bounded(self):
        self.start()
        for index in range(dr.MAX_EARLY_CANCELS + 40):
            dr.request_cancel(self.layout, f"none-{index}")
        for _ in range(8):
            self.runner.step()
        self.assertEqual(os.listdir(self.layout.inbox), [])
        self.assertLessEqual(len(self.runner._early_cancels), dr.MAX_EARLY_CANCELS)
        self.assertEqual(os.listdir(self.layout.outbox), [])


class BusyWindow(RunnerCase):
    def test_a_window_that_takes_no_output_does_not_stop_the_time_limits(self):
        """Got through: the runner wrote to its console in the job loop. A
        console whose text is being selected blocks the write, and with it
        the loop that ends jobs at their time limit."""
        gate = threading.Event()
        self.addCleanup(gate.set)

        class Stuck(io.StringIO):
            def write(self, text: str) -> int:
                gate.wait(60)
                return super().write(text)

        window = Stuck()
        writer = dr.BackgroundWriter(window)
        runner = self.make_runner(out=writer)
        finished = threading.Event()

        def work() -> None:
            runner.startup()
            self.drop("slow", self.job(timeout_seconds=30))
            runner.step()
            self.clock.advance(31)
            runner.step()
            finished.set()

        threading.Thread(target=work, daemon=True).start()
        self.assertTrue(finished.wait(15), "the job loop waits for the window")
        self.assertEqual(self.state("slow"), "timed_out")
        self.assertEqual(self.launcher.procs[0].kills, 1)
        self.assertEqual(window.getvalue(), "")
        self.assertFalse(writer.close(0.2), "the window has taken nothing yet")
        gate.set()
        self.assertTrue(writer.close(15))
        shown = window.getvalue()
        for part in ("ASAMU-decomp desk runner", "START job slow", "ENDED job slow"):
            self.assertIn(part, shown)
        with open(self.layout.log, encoding="utf-8") as handle:
            self.assertIn("ENDED job slow", handle.read())

    def test_the_backlog_of_a_busy_window_is_bounded(self):
        gate = threading.Event()
        self.addCleanup(gate.set)

        class Stuck(io.StringIO):
            def write(self, text: str) -> int:
                gate.wait(60)
                return super().write(text)

        window = Stuck()
        writer = dr.BackgroundWriter(window, limit=5)
        for index in range(40):
            writer.write(f"line {index}\n")
        with writer._changed:
            self.assertLessEqual(len(writer._lines), 5)
        gate.set()
        self.assertTrue(writer.close(15))
        shown = window.getvalue()
        self.assertIn("line 0\n", shown)
        self.assertRegex(shown, r"\.\.\. 3[45] line\(s\) were not shown")
        self.assertNotIn("line 39", shown)

    def test_a_window_that_is_gone_does_not_end_the_runner(self):
        class Gone(io.StringIO):
            def write(self, text: str) -> int:
                raise OSError("the window is gone")

        writer = dr.BackgroundWriter(Gone())
        runner = self.make_runner(out=writer)
        runner.startup()
        self.drop("job-1")
        self.assertTrue(runner.step())
        self.assertEqual(self.state("job-1"), "running")
        self.assertTrue(writer.close(15))


class StartedTwice(AttackCase):
    def test_hostile_second_start_through_another_spelling_of_the_root(self):
        self.start()
        spellings = [self.root + os.sep, os.path.join(self.root, "work", ".."), os.path.join(self.root, ".")]
        if os.path.exists(self.root.upper()) and os.path.exists(self.root.lower()):
            spellings += [self.root.upper(), self.root.lower()]
        alias = os.path.join(self.base, "alias")
        if dir_link(self.root, alias):
            self.addCleanup(remove_dir_link, alias)
            spellings.append(alias)
        for spelling in spellings:
            second = dr.Runner(spelling, clock=self.clock, launcher=self.launcher, out=io.StringIO())
            with self.assertRaises(dr.RunnerError, msg=spelling) as caught:
                second.startup()
            self.assertIn("already running", str(caught.exception), spelling)
        self.assertTrue(self.runner.step(), "the first runner is not disturbed")

    def test_hostile_removal_of_the_lock_file_does_not_leave_two_runners(self):
        """Got through (not on Windows): with the lock file deleted a second
        runner started beside the first, and both went on."""
        if WINDOWS:
            self.start()
            with self.assertRaises(OSError):
                os.unlink(self.layout.lock)
            with self.assertRaises(OSError):
                os.rename(self.layout.lock, self.layout.lock + ".away")
            self.assertTrue(self.runner.step())
            return
        test = self
        seen: list[object] = []

        class Scripted(FakeClock):
            def sleep(self, seconds: float) -> None:
                super().sleep(seconds)
                if self.slept == dr.POLL_PERIOD_S:
                    test.drop("job-1")
                elif self.slept == 3 * dr.POLL_PERIOD_S:
                    os.unlink(test.layout.lock)
                    second = dr.Runner(test.root, clock=FakeClock(), launcher=FakeLauncher(), out=io.StringIO())
                    test.addCleanup(second.shutdown, "test over")
                    second.startup()  # cannot be prevented where an open file can be deleted
                    seen.append(second)
                elif self.slept > 100:
                    raise AssertionError("the first runner went on beside the second")

        runner = self.make_runner(clock=Scripted())
        self.assertEqual(runner.run(), 1, "the first runner stops with an error")
        self.assertEqual(len(seen), 1)
        self.assertEqual([p.kills for p in self.launcher.procs], [1], "and ends its job")
        self.assertIn("lock file was removed or replaced", self.console.getvalue())
        self.assertEqual(self.state("job-1"), "runner_stopped")
        self.assertTrue(seen[0].step(), "the second one is now the only one")

    def test_hostile_replacement_of_the_lock_file(self):
        if WINDOWS:
            self.skipTest("a lock file that is held cannot be deleted or renamed on Windows (see the test above)")
        self.start()
        os.rename(self.layout.lock, self.layout.lock + ".away")
        with open(self.layout.lock, "w"):
            pass
        with self.assertRaises(dr.RunnerError) as caught:
            self.runner.step()
        self.assertIn("lock file was removed or replaced", str(caught.exception))

    @unittest.skipUnless(WINDOWS, "an open handle holds a folder in place only on Windows; elsewhere a swap is "
                                  "noticed at the start of the next round (HostileInbox)")
    def test_hostile_swap_of_a_runner_folder_cannot_even_be_made(self):
        self.start()
        for folder in (self.layout.inbox, self.layout.outbox, self.layout.base, self.root):
            with self.assertRaises(OSError, msg=folder):
                os.rename(folder, folder + ".away")
        with self.assertRaises(OSError):
            os.rmdir(self.layout.inbox)
        self.drop("job-1")
        self.assertTrue(self.runner.step())
        self.assertEqual(self.state("job-1"), "running", "files in those folders still come and go")
        self.runner.shutdown("test")
        os.rename(self.layout.outbox, self.layout.outbox + ".away")  # free again once it has stopped

    @unittest.skipUnless(WINDOWS, "an open handle holds a folder in place only on Windows")
    def test_a_runner_that_could_not_start_holds_nothing(self):
        self.start()
        second = self.make_runner(out=io.StringIO())
        with self.assertRaises(dr.RunnerError):
            second.startup()
        self.runner.shutdown("test")
        os.rename(self.layout.inbox, self.layout.inbox + ".away")  # neither runner holds it any more


class ClientAttacks(AttackCase):
    def test_a_used_job_id_is_refused_and_no_old_result_is_handed_back(self):
        """Got through: the runner keeps the first result of an id and does
        not run a second job under it; the waiting client then returned the
        old exit code and the old output as if they were new."""
        self.start()
        dr.write_json_atomic(self.layout.status("old"), {"state": "finished", "exit_code": 0})
        with open(self.layout.stdout("old"), "w") as handle:
            handle.write("STALE OUTPUT\n")
        fields = {k: v for k, v in self.job().items() if k != "version"}
        fields.update(env={}, id="old")
        out, err = io.BytesIO(), io.StringIO()
        self.assertEqual(dr.command_submit(self.root, fields, out=out, err=err, clock=self.clock), dr.EXIT_RUNNER)
        self.assertEqual(out.getvalue(), b"")
        self.assertIn("already has a result", err.getvalue())
        self.assertEqual(os.listdir(self.layout.inbox), [])

    def test_hostile_heartbeat_does_not_break_the_status_command(self):
        """Got through: a heartbeat whose "running" is a number ended `status` with a traceback."""
        self.start()
        forged_values = (
            {"running": 5, "queued": 7},
            {"running": "x" * 100, "queued": {"a": 1}},
            {"running": [1, None, {"id": "\x1b[2J", "program": ["x"]}], "queued": None},
            {"pid": "\x1b[2J" * 50, "session_id": ["x"], "window_station": {"x": 1}, "jobs_started": "\x1b[0m"},
            {"running": [{"id": "x", "program": "y"}] * 5000},
        )
        for forged in forged_values:
            beat = dr.read_json(self.layout.heartbeat)
            assert beat is not None
            beat.update(forged)
            dr.write_json_atomic(self.layout.heartbeat, beat)
            out = io.StringIO()
            self.assertEqual(dr.command_status(self.root, False, out, self.clock), 0)
            self.assertNotIn("\x1b", out.getvalue())
            self.assertLess(out.getvalue().count("\n"), 10)
            self.runner._beat(force=True)

    def test_a_cancellation_that_arrives_before_its_job_still_stops_it(self):
        """The sender was interrupted while its job file was still on its way:
        the cancellation must not be lost, or the job runs with nobody waiting."""
        self.start()
        dr.request_cancel(self.layout, "early")
        self.runner.step()
        self.assertEqual(os.listdir(self.layout.outbox), [])
        self.drop("early")
        self.runner.step()
        self.assertEqual(self.state("early"), "cancelled")
        self.assertEqual(self.launcher.calls, [])
        self.assertEqual(dr.exit_code_for(self.status("early")), dr.EXIT_CANCELLED)
        # Only for a while: an old cancellation does not block the id for ever.
        dr.request_cancel(self.layout, "late")
        self.runner.step()
        self.clock.advance(dr.EARLY_CANCEL_KEEP_S + 1)
        self.drop("late")
        self.runner.step()
        self.assertEqual(self.state("late"), "running")
        # And the cancellation of a job that is over is not kept for the next one.
        self.launcher.procs[0].exit_code = 0
        self.runner.step()
        dr.request_cancel(self.layout, "late")
        self.runner.step()
        self.assertEqual(self.runner._early_cancels, {})


@unittest.skipUnless(WINDOWS, "the start is checked against what the system really used only on Windows (a suspended "
                              "process, its image path, a held working directory); elsewhere the moment between "
                              "check and start stays open, as the module says")
class LastMomentSwap(RealProcessCase):
    """An executable or a folder replaced between the check and the start."""

    def setUp(self) -> None:
        super().setUp()
        self.clock = FakeClock()
        self.console = io.StringIO()
        self.runner = dr.Runner(self.root, clock=self.clock, out=self.console)
        self.addCleanup(self.runner.shutdown, "test over")
        self.runner.startup()
        self.marker = os.path.join(self.outside, "ran.txt")

    def wait_state(self, job_id: str, seconds: float = 30.0) -> dict:
        deadline = time.monotonic() + seconds
        status: dict = {}
        while time.monotonic() < deadline:
            self.runner.step()
            status = dr.read_json(self.layout.status(job_id)) or {}
            if status.get("state") in dr.TERMINAL_STATES:
                break
            time.sleep(0.05)
        return status

    def test_hostile_program_folder_swapped_for_a_junction_between_check_and_start(self):
        """Got through: the path that was checked was resolved again by the
        system when the process was created."""
        good_dir = os.path.dirname(self.native)
        evil_dir = os.path.join(self.outside, "bin")
        os.makedirs(evil_dir)
        shutil.copyfile(self.native, os.path.join(evil_dir, os.path.basename(self.native)))
        real_popen = subprocess.Popen
        swapped: list[int] = []

        def swapping(*args, **kwargs):
            if not swapped:
                swapped.append(1)
                os.rename(good_dir, good_dir + ".moved")
                made = subprocess.run(["cmd", "/c", "mklink", "/J", good_dir, evil_dir], capture_output=True)
                swapped.append(made.returncode)
            return real_popen(*args, **kwargs)

        self.addCleanup(lambda: os.path.isdir(good_dir + ".moved") and (remove_dir_link(good_dir), os.rename(good_dir + ".moved", good_dir)))
        code = f"open({self.marker!r}, 'w').close()"
        with open(os.path.join(self.layout.inbox, "swap.json"), "w") as handle:
            json.dump(self.job(program=self.native, args=self.prefix + ["-c", code]), handle)
        with mock.patch.object(subprocess, "Popen", swapping):
            self.runner.step()
        self.assertEqual(swapped, [1, 0], "the swap was made at the last moment")
        status = self.wait_state("swap")
        self.assertEqual(status.get("state"), "failed_to_start", status)
        self.assertIn("not the file that was checked", status["reason"])
        time.sleep(1.0)
        self.assertFalse(os.path.exists(self.marker), "the program from outside the root ran")
        self.assertIn("FAILED job swap", self.console.getvalue())

    def test_hostile_working_directory_swapped_for_a_junction_before_the_start(self):
        real_pin = dr._WinApi.pin_folder
        swapped: list[int] = []
        work = self.work

        def swapping(api, path):
            if not swapped and os.path.normcase(path) == os.path.normcase(work):
                os.rename(work, work + ".moved")
                swapped.append(subprocess.run(["cmd", "/c", "mklink", "/J", work, self.outside], capture_output=True).returncode)
            return real_pin(api, path)

        self.addCleanup(lambda: os.path.isdir(work + ".moved") and (remove_dir_link(work), os.rename(work + ".moved", work)))
        code = f"import os; open({self.marker!r}, 'w').write(os.getcwd())"
        with open(os.path.join(self.layout.inbox, "cwd.json"), "w") as handle:
            json.dump(self.job(program=self.native, args=self.prefix + ["-c", code]), handle)
        with mock.patch.object(dr._WinApi, "pin_folder", swapping):
            self.runner.step()
        self.assertEqual(swapped, [0])
        status = self.wait_state("cwd")
        self.assertEqual(status.get("state"), "failed_to_start", status)
        self.assertIn("working directory", status["reason"])
        time.sleep(1.0)
        self.assertFalse(os.path.exists(self.marker), "the program ran in a folder outside the root")

    def test_the_working_directory_is_held_in_place_while_the_process_is_made(self):
        real_popen = subprocess.Popen
        tried: list[str] = []

        def trying(*args, **kwargs):
            if not tried:
                try:
                    os.rename(self.work, self.work + ".moved")
                    tried.append("renamed")
                except OSError as err:
                    tried.append(type(err).__name__)
            return real_popen(*args, **kwargs)

        script = self.script("echo.py", ECHO_SCRIPT)
        with open(os.path.join(self.layout.inbox, "held.json"), "w") as handle:
            json.dump(self.job(program=self.native, args=self.prefix + [script]), handle)
        with mock.patch.object(subprocess, "Popen", trying):
            self.runner.step()
        self.assertEqual(tried, ["PermissionError"])
        status = self.wait_state("held")
        self.assertEqual(status.get("state"), "finished", status)
        with open(self.layout.stdout("held"), encoding="utf-8") as handle:
            self.assertEqual(os.path.realpath(json.loads(handle.read())["cwd"]), self.work)

    def test_same_path_comparison(self):
        same = dr.same_windows_path
        self.assertTrue(same(r"D:\asamu\target-h4\release\asamu.exe", r"d:/ASAMU/target-h4/release/Asamu.exe"))
        self.assertTrue(same("\\\\?\\D:\\asamu\\work", r"D:\asamu\work"))
        self.assertFalse(same(r"D:\asamu\work", r"D:\asamu\work2"))
        self.assertFalse(same("\\\\?\\UNC\\server\\share\\x", r"server\share\x"))
        self.assertFalse(same(r"C:\Windows\System32\cmd.exe", r"D:\asamu\bin\cmd.exe"))


class RealOutput(RealProcessCase):
    """Real programs whose output goes where a link was planted, or is large."""

    def run_to_end(self, runner: dr.Runner, job_id: str, seconds: float = 60.0) -> dict:
        deadline = time.monotonic() + seconds
        status: dict = {}
        while time.monotonic() < deadline:
            runner.step()
            status = dr.read_json(self.layout.status(job_id)) or {}
            if status.get("state") in dr.TERMINAL_STATES:
                break
            time.sleep(0.05)
        return status

    def test_hostile_links_where_a_real_jobs_output_and_status_go(self):
        victim = os.path.join(self.outside, "victim.txt")
        with open(victim, "w") as handle:
            handle.write("PRECIOUS\n")
        runner = dr.Runner(self.root, clock=FakeClock(), out=io.StringIO())
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        for name in ("real.stdout.txt", "real.stderr.txt", "real.status.json.tmp"):
            if not try_symlink(victim, os.path.join(self.layout.outbox, name)):
                self.skipTest("this account may not create symbolic links")
        before = tree_snapshot(self.outside)
        script = self.script("echo.py", ECHO_SCRIPT)
        with open(os.path.join(self.layout.inbox, "real.json"), "w") as handle:
            json.dump(self.job(program=self.native, args=self.prefix + [script, "hello"]), handle)
        status = self.run_to_end(runner, "real")
        self.assertEqual(status.get("state"), "finished", status)
        with open(self.layout.stdout("real"), encoding="utf-8") as handle:
            self.assertEqual(json.loads(handle.read())["args"], ["hello"])
        with open(self.layout.stderr("real"), encoding="utf-8") as handle:
            self.assertIn("to stderr", handle.read())
        self.assertEqual(tree_snapshot(self.outside), before)
        with open(victim) as handle:
            self.assertEqual(handle.read(), "PRECIOUS\n")

    def test_a_job_that_never_stops_writing_is_ended_at_the_output_limit(self):
        runner = dr.Runner(self.root, clock=FakeClock(), out=io.StringIO())
        self.addCleanup(runner.shutdown, "test over")
        runner.startup()
        pid_file = os.path.join(self.scratch, "pid.txt")
        script = self.script("flood.py", "import os, sys\n"
                             "open(sys.argv[1], 'w').write(str(os.getpid()))\n"
                             "block = b'x' * 65536\n"
                             "while True:\n    sys.stdout.buffer.write(block)\n")
        with open(os.path.join(self.layout.inbox, "flood.json"), "w") as handle:
            json.dump(self.job(program=self.native, args=self.prefix + [script, pid_file], timeout_seconds=600), handle)
        with mock.patch.object(dr, "MAX_OUTPUT_BYTES", 2 * 1024 * 1024):
            status = self.run_to_end(runner, "flood")
        self.assertEqual(status.get("state"), "output_limit", status)
        self.assertTrue(status["process_tree_ended"])
        self.assertEqual(dr.exit_code_for(status), dr.EXIT_RUNNER)
        with open(pid_file) as handle:
            self.pids = [int(handle.read())]
        self.assertEqual(self.wait_all_gone(self.pids), [])
        size = os.path.getsize(self.layout.stdout("flood"))
        time.sleep(0.5)
        self.assertEqual(os.path.getsize(self.layout.stdout("flood")), size, "nothing is written any more")


class LargeOutput(EndToEndCase):
    def test_large_output_arrives_whole_and_in_order(self):
        import hashlib

        script = self.script("large.py", "import hashlib, sys\n"
                             "digest = hashlib.sha256()\n"
                             "for index in range(int(sys.argv[1])):\n"
                             "    block = (b'%08d ' % index) * 7281 + b'\\n'\n"
                             "    digest.update(block)\n"
                             "    sys.stdout.buffer.write(block)\n"
                             "    if index % 64 == 0:\n        sys.stderr.write('progress %d\\n' % index)\n"
                             "sys.stdout.buffer.flush()\n"
                             "sys.stderr.write('sha256 ' + digest.hexdigest() + '\\n')\n"
                             "sys.exit(5)\n")
        blocks = 320  # 65530 bytes each: about 21 MB
        code, out, err = self.submit(program=self.native, args=self.prefix + [script, str(blocks)], timeout_seconds=300)
        self.assertEqual(code, 5, err[-2000:])
        self.assertEqual(len(out), blocks * 65530)
        self.assertIn("sha256 " + hashlib.sha256(out).hexdigest(), err)
        self.assertEqual(err.count("progress "), 5)
        self.assertTrue(out.startswith(b"00000000 ") and out.endswith(b"00000319 \n"))


@unittest.skipUnless(WINDOWS, "start-runner.cmd is a Windows batch file")
class StartCmd(RootCase):
    def test_hostile_py_file_in_the_root_is_not_what_the_batch_file_starts(self):
        """Got through (in the fallback used when the recorded python.exe is
        gone): a double-click runs the batch file with the root as the current
        folder, and a bare `py` is looked for there first."""
        if shutil.which("py") is None:
            self.skipTest("no py launcher on PATH, so the batch file's fallback has nothing to start")
        marker = os.path.join(self.outside, "planted-ran.txt")
        for name in ("py.cmd", "py.bat", "where.cmd"):
            with open(os.path.join(self.root, name), "w", newline="") as handle:
                handle.write(f'@echo planted> "{marker}"\r\n')
        self.assertEqual(dr.command_install(self.root, io.StringIO()), 0)
        with open(self.layout.start_cmd, "w", encoding="ascii", newline="") as handle:
            handle.write(dr.start_cmd_text(None))
        proc = subprocess.Popen(["cmd", "/d", "/c", self.layout.start_cmd], cwd=self.root, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.addCleanup(lambda: proc.poll() is None and proc.kill())
        deadline = time.monotonic() + 60
        while not dr.runner_state(self.root)["alive"] and time.monotonic() < deadline and proc.poll() is None:
            time.sleep(0.1)
        alive = dr.runner_state(self.root)["alive"]
        self.assertEqual(dr.command_stop(self.root, io.StringIO()), 0)
        shown = proc.communicate(timeout=60)[0]
        self.assertFalse(os.path.exists(marker), "a file planted in the root was run")
        self.assertTrue(alive, shown)
        self.assertIn(b"ASAMU-decomp desk runner", shown)
        self.assertIn(b"not running any more", shown)
        self.assertEqual(proc.returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=1)
