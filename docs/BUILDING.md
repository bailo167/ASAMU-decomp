# Building

Requirements: a stable Rust toolchain (see `rust-version` in `Cargo.toml`; Bevy 0.20 needs ≥ 1.97).

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo run -p progress-gen -- --check
cargo run -p repo-hygiene
cargo run -p asamu           # Bevy runtime (pre-alpha)
```

`default-members` excludes the Bevy app so `cargo test` on the default set stays fast; `--workspace` includes it.

## Platform prerequisites

- **Linux:** `sudo apt-get install -y pkg-config libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev`
- **Windows:** MSVC build tools.
- **macOS:** Xcode command line tools. Both `aarch64-apple-darwin` and `x86_64-apple-darwin` are targets:
  `rustup target add x86_64-apple-darwin && cargo check --workspace --target x86_64-apple-darwin`.

## Using your original install

The tools find the game through Steam (App ID 278360). To point them elsewhere:

```bash
export ASAMU_ORIGINAL_DIR="/path/to/A Story About My Uncle"
cargo run -p asamu-locate
```

Tests that need original data skip themselves when it is unavailable (`ASAMU_ORIGINAL_DIR` for the install,
`ASAMU_CONVERTED_DIR` for converted data).

## Converting and playing from source

```bash
cargo run --release -p asamu-import -- all                     # into the default user data folder
cargo run --release -p asamu-import -- --out research/local/converted all   # or a git-ignored folder
cargo run --release -p asamu -- --converted research/local/converted --level AG-Workshop
```

`asamu-import all` locates the install, checks its input files against the committed inventory
(`docs/reverse-engineering/data/inventory/*.json`, embedded at build time), runs every conversion and writes
`asamu-import-run.json` (deterministic: importer build, install identity, verification, per-stage status,
fingerprint and output digest) plus `asamu-import-state.json` (hash cache and timings). Reruns skip stages whose
fingerprint, key file and output file count and size are unchanged. The output folder must be new, empty or an
earlier import: `all` replaces whole stage folders, so it refuses a folder holding anything else. The module documentation in `tools/asamu-import/src/all.rs` describes the
resumability rules; [PLAYING.md](PLAYING.md) is the player-facing guide. Never write converted data into the
repository outside the git-ignored `research/local/` (the importer refuses), and never commit it.

Reference measurements (Apple-silicon Mac, release build, Mac build 1822049 of the game, warm file cache). The
machine was shared with other builds, so times vary by about ±30%; the package-cache comparison was repeated.

| Run | Time |
|---|---|
| First import of all nine stages into an empty folder | 32.1 s (hashing the 45 input files, 1.1 GB: 2.5 s; output digests: 2.2 s) |
| First import of all twelve stages (2026-10-10, development-profile build, machine in use) | 44.4 s (hashing 1.5 s); 13,091 files, 2,282,441,193 bytes |
| Seven stages (without kismet and lightmaps) on a quiet machine | 17.7 s |
| Rerun with nothing to do | 0.05–0.1 s |
| `--package-cache` on the six package-only stages | 3.0–3.6 s faster per forced run (building the cache: 1.6–1.7 s once, 1.5 GB) |
| Independent re-run (verification pass, machine heavily loaded by parallel builds) | 111 s first import, 0.14 s no-op rerun (wall clock incl. process start); peak footprint 2.5 GB |

Per stage (first import of the nine-stage pipeline): textures 4.9 s, meshes 2.2 s, materials 3.1 s, levels 2.4 s,
audio 1.5 s, matinee 1.3 s, skeletal 1.9 s, kismet 3.0 s, lightmaps 7.0 s. Output then: 13,108 files, 2.2 GB
(textures 1.29 GB, lightmaps 0.62 GB). The three later stages add particles 5.0 s, decals 2.1 s and
localization 0.2 s; the determinism and resume measurements below were made on the nine-stage pipeline and have
not been repeated for the twelve.
Peak resident memory of the whole run: 2.7 GB. The run manifest, including every stage's output digest, was byte
for byte identical across a first run, a no-op rerun, a forced rerun and forced reruns with the package cache, so
the conversions are deterministic and the cache does not change their output. The verification pass reproduced
this independently: same file counts and byte totals per stage (13,108 files, 2,201,376,317 bytes) and identical
manifests after a no-op rerun and after a forced lightmaps run that was killed and then resumed; a forced run of
the six package-only stages through the package cache gave the same output digests.

## Release builds and packages

`.github/workflows/release.yml` runs on a `v*` tag (or by hand with *Run workflow*). It builds `asamu` and
`asamu-import` in release mode for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` (on Ubuntu 22.04 for an
older glibc baseline), `aarch64-apple-darwin` and `x86_64-apple-darwin` (cross-compiled on the arm64 runner,
`MACOSX_DEPLOYMENT_TARGET=11.0`), checks the architecture of both macOS builds with `lipo`, smoke-tests the
native ones with `--help`, and packages one zip per target containing exactly the two executables, `LICENSE` and
`docs/PLAYING.md`. The package job requires exactly those four files in every zip and refuses any file that
starts with the magic bytes of game data or converted data (UE3 package/texture cache, Scaleform, Bink, DDS, Ogg,
WAV, glTF, PNG), writes `SHA256SUMS.txt`, uploads everything as workflow artifacts and, for a tag, attaches it to
a **draft** GitHub release that a maintainer reviews and publishes. A manual run with the `tag` input checks out
and packages that tag; a release that is already published is never modified. The workflow has not run on
GitHub yet; its scripts were exercised locally on stand-in files (including every rejection case).

Before publishing a release, check the licence notices of the statically linked dependencies (Bevy and the
rest of the dependency tree, mostly MIT/Apache-2.0): MIT requires their copyright notices in binary
distributions, and the zips currently carry only this project's `LICENSE`.

The same build locally:

```bash
cargo build --locked --release -p asamu -p asamu-import                          # host target
cargo build --locked --release --target x86_64-apple-darwin -p asamu -p asamu-import   # Intel Mac from an arm64 Mac
mkdir -p target/dist/asamu-decomp-dev    # target/ is git-ignored
cp target/release/asamu target/release/asamu-import LICENSE docs/PLAYING.md target/dist/asamu-decomp-dev/
(cd target/dist && zip -r asamu-decomp-dev.zip asamu-decomp-dev)
```

Packages must never contain game files or converted data: players convert their own copy with
`asamu-import all`.

## On the second machine's screen: the desk runner

`tools/dev/win_host.sh` builds, tests and runs a checkout on a second machine (Windows) over SSH. What a
program started that way can and cannot do there was measured on 2026-10-10, and again in an independent
review on 2026-10-11 (Windows 11, OpenSSH server, a laptop with an NVIDIA RTX 5070 Ti; release build of this
repository):

- **Works over SSH alone:** building, the test suite, the importer, and **offscreen rendering on the GPU**.
  `asamu.exe --hidden-window --screenshot <root>\scratch\x.png --screenshot-delay 2 --exit-after 15` (the
  graybox, no game data) created the adapter (`NVIDIA GeForce RTX 5070 Ti Laptop GPU`, backend Vulkan), wrote a
  1600 × 900 image that is a correct render of the graybox, and exited with code 0. Screenshot checks of
  converted levels therefore need nobody at that machine.
- **Does not work over SSH:** a window anyone can see. The SSH server runs its sessions in session 0, on a
  window station of its own (`Service-0x0-...$`), not on the desktop of the person signed in (`WinSta0` in
  session 1). The app's window is created there and is invisible. Playing needs a process inside the desktop
  session, and only the person at that machine may put one there.

That process is `tools/dev/desk_runner.py` (Python standard library only). Nothing is installed and no setting,
service, scheduled task or registry key is involved: the owner of the machine starts it by hand, and it is gone
when its window is closed.

```bash
export ASAMU_WIN_HOST=<ssh host>
tools/dev/win_host.sh desk-install        # copies the runner to <root>\runner and writes <root>\start-runner.cmd
```

Then, at that machine (this is the whole instruction for its owner):

1. At that machine's own screen, signed in: open `<root>` (default `D:\asamu`) in File Explorer and double-click
   `start-runner.cmd`.
2. A console window titled "ASAMU-decomp desk runner" opens and says what it will and will not run. Leave it
   open (minimise it) while testing or playing.
3. To stop it, close that window: everything it started ends with it. Start it again after each sign-in or
   restart.

From the development machine:

```bash
tools/dev/win_host.sh desk-status                       # alive? heartbeat age, jobs; says what to do if not
tools/dev/win_host.sh sync NAME && tools/dev/win_host.sh cargo NAME build --release -p asamu
tools/dev/win_host.sh desk NAME asamu                   # the graybox, in a window on that machine's screen
ASAMU_DESK_TIMEOUT=3600 ASAMU_DESK_ENV='ASAMU_CONVERTED_DIR=D:\asamu\converted' \
  tools/dev/win_host.sh desk NAME asamu --level AG-Workshop
tools/dev/win_host.sh desk-stop                         # ends the runner and its jobs
```

`desk` waits for the program, streams its output and returns its exit code (124: the time limit passed and
the process tree was ended; 125: the runner is not running or refused the job; 126: the program could not be
started; 130: cancelled). Of a Windows exit code only the low eight bits come back through SSH, so a code
above 255 is returned as those bits, or as 1 when they are 0: a program that failed never reads as 0 (the
line `desk: job ... finished: exit code N` has the full code). Ctrl+C on the development machine ends the
program on the other one, and so does ending the `desk` command in any other way that lets it act (a time
limit of the caller, a closed terminal); if it is killed outright, the job's own time limit is what ends the
program. A new window may open behind others (Windows limits which programs may take the focus); if so, click
it in the taskbar.

What the runner does, and refuses (the module comment in `desk_runner.py` has the protocol):

- A job is one program with an **argument list**, a working directory and a time limit. There is no shell and no
  command string anywhere on the way: the job crosses SSH as one base64 token, and the program receives the
  list as it was (checked with arguments holding spaces, quotes, `&`, `|`, `%PATH%` and a trailing backslash).
- The program must be a regular `.exe` whose resolved path (symbolic links and junctions followed) is inside
  `<root>`; the working directory likewise. "Inside" is judged twice: by the spelling of the resolved path, and
  by which folder it really is (a folder `ROOT` beside `root` is another folder where the file system tells
  the two apart, which Windows can switch on per folder). Network (UNC) and device paths, paths on a network
  drive, `..`, alternate data streams, reserved device names, batch files and scripts, and shells, interpreters
  and system tools by name (`cmd`, `powershell`, `python`, `rundll32`, `reg`, `schtasks`, ..., also Steam and the
  original game's executable) are refused. Only `ASAMU_*`, `WGPU_*`, `BEVY_*`, `RUST_BACKTRACE` and `RUST_LOG`
  may be set in the environment.
- The start itself is checked too, because a folder on the way can be swapped for a junction after the check:
  the working directory is held in place while the process is created, and the new process stays suspended
  until Windows confirms which file it was made from. A program from anywhere else is ended before it has run.
- Each job runs in a Windows job object: at the time limit, on cancellation, when the program has ended, when
  the runner stops and when the runner dies, every process the job started is ended. A process cannot leave
  it (a start with "break away from job" is refused; detached children and children in a new process group
  stay inside), and one job can have 64 processes at most. At most two jobs run at
  once and sixteen wait; a job file larger than 64 KiB is refused unread; 64 MiB of output ends a job (the
  size is looked at four times a second, so a fast writer gets some way past it: 81 MB arrived in one test).
- It opens no network port, writes only inside `<root>\runner` (inbox, outbox, heartbeat, log), refuses to
  start twice (a lock the operating system drops with the process), rejects jobs that were waiting before it
  started, and shows every start, end and refusal in its window and in `runner.log` / `rejected.log`.
- None of its files is written through a link (a link planted where an output, a status or a log goes is
  removed or refused, never followed), and while it runs it holds its folders in place, so that they cannot be
  renamed away and replaced by a junction. The client commands (`desk`, `desk-cancel`, `desk-stop`) keep to
  the same rule, also when no runner is there to hold the folders. Its window never holds up the job loop: text
  being selected in a console blocks the program that writes to it, so the window is written from a thread of
  its own.
- It is not a sandbox: whoever can write job files can also put a program under `<root>` (or replace the runner
  script itself), and that account can already run anything over SSH. The rules keep the tooling honest; they do
  not defend against its own account, nor against any other account that may write to `<root>` (look at the
  folder's permissions: on a drive with Windows' default permissions every signed-in account may). Known
  limits: the arguments of a job are not judged (our own program can be told to write a screenshot anywhere it
  is allowed to); a copy of any program under another name passes, and so does a second name (hard link) for
  a file elsewhere on the same drive, which cannot be told from a copy (cargo's own `target\release\x.exe` is
  one); a process that some other program starts on a job's behalf (COM, WMI, the task scheduler) is not in its
  job object; a symbolic link inside `<root>` that leads to a server which does not answer holds up one round
  of the loop until Windows gives up on the server; and on Linux and macOS, where the runner only runs for its
  tests, the moment between check and start stays open and a killed runner does not take its jobs with it.

Tests: `python3 tools/dev/test_desk_runner.py` (144 tests; run on macOS with Python 3.14 and on Windows with
Python 3.13; no CI job runs them yet): the checks with hostile job files (path traversal, UNC, a program
outside the root, links and junctions out of the root, a 50 MB job file, command strings), the life cycle with
a fake clock, and real processes (a tree of five processes that ignores every signal is ended at the limit;
killing the runner ends its jobs; a console event stops it in an orderly way). The tests work in the system's
temporary folder; to keep them inside `<root>` on the second machine:

```bash
ASAMU_WIN_ENV='$env:TEMP="D:\asamu\scratch\NAME\tmp"; $env:TMP=$env:TEMP' \
  tools/dev/win_host.sh exec NAME <python.exe> -B tools\dev\test_desk_runner.py
```

A second pass attacked the runner and added each attack as a test; those that got through and were fixed: a
time limit of hundreds of digits ended the runner; links planted in its folder sent its writes to other files;
a program folder swapped for a junction between check and start ran a program from outside the root; a busy
console window stopped the time limits; a deleted lock file allowed a second runner (not on Windows); a reused
job id was answered with the old result; `start-runner.cmd` ran a `py.cmd` planted in the root.

A third pass (an independent review, 2026-10-11) attacked it again, on the Windows machine itself as well.
Got through and fixed, each with a test: a program in a folder whose name differs from the root's only in case
was taken for one inside the root (Windows, with case-sensitive names switched on for the folder above);
`desk-cancel` wrote its file through a junction put where the inbox belongs when no runner was running; a
status or heartbeat file nested a hundred thousand levels deep ended the client with a traceback; an exit code
of 256 came back through SSH as 0; a `desk` command ended by its caller (not by Ctrl+C) left the program
running until the job's own time limit; a runner that was killed and started again at once left the client
waiting for ever; on macOS a program in the runner's own folder passed when the folder was spelled `RUNNER`;
a folder called `stop` stopped every new runner without a word; an interrupt in the middle of a start could
leave a suspended process behind. Tried and held: mixed separators, trailing dots and spaces, alternate data
streams, `\\?\` and `\\.\` and loopback UNC spellings, 8.3 short names (on a drive that has them), paths
longer than 260 characters, environment variables in paths, links of every kind out of the root (junction,
symbolic link to a file, relative, with a `\\?\` target, a loop), a program replaced by a symbolic link at
the last moment, `args` as one string, `PATH` and `COMSPEC` in a job's environment, a batch file renamed to
`.exe`, 30 MB and deeply nested job files, leaving the job object, a job that starts processes without end
(stopped at 63 children), a second start through six other spellings of the root.

The protocol was exercised end to end over SSH on 2026-10-11, with the runner started from `start-runner.cmd`
inside an SSH session on a temporary root (so its jobs ran without a screen): arguments with spaces, quotes,
`&`, `|`, `%PATH%` and a trailing backslash arrived as they were; exit codes 7, 256 (as 1) and 0xC0000005 (as
5) came back; a job with three children was ended at its limit of 6 s and none of the four processes was
left; 20,969,600 bytes of output arrived with the SHA-256 the program computed; of 42 hostile job files
dropped straight into the inbox the runner refused 41, Windows itself refused to start the 42nd (batch text
in a file named `.exe`), nothing any of them named ran, and the runner went on; the app wrote its offscreen
screenshot as a job; Ctrl+C, SIGTERM and SIGHUP on the development machine each ended the program within two seconds; a
second start was refused; `desk-stop` ended a running job and the runner. **Not yet observed:** the runner
started by double-click in a real desktop session, and a visible window opened through it; that needs the
owner at the machine.
