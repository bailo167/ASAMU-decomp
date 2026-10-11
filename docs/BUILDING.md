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

## Building on a second machine

`tools/dev/win_host.sh` builds, tests and runs a checkout on a second machine that runs Windows, over SSH. Use
it when the development machine is short of disk (one debug target folder of this workspace is about 20 GB) or
when a change has to be proven on Windows. The checkout stays where it is; nothing is installed on the other machine
by the script, and its PowerShell half (`tools/dev/win_host_remote.ps1`) travels with every command.

```bash
export ASAMU_WIN_HOST=<ssh host>            # required; ASAMU_WIN_ROOT (default D:\asamu) is the work root there
tools/dev/win_host.sh sync NAME             # mirror this checkout to <root>\work\NAME
tools/dev/win_host.sh gate NAME             # fmt, clippy, test, build: one line per step, logs kept there
tools/dev/win_host.sh cargo NAME test -p asamu-ue3 --locked      # any cargo command, output streamed
tools/dev/win_host.sh exec NAME 'D:\asamu\target-NAME\debug\asamu-locate.exe'   # any program, exact arguments
tools/dev/win_host.sh fetch logs/NAME/test.log /tmp/test.log     # copy one file back
tools/dev/win_host.sh disk                  # free space, size of every work tree and target folder
tools/dev/win_host.sh clean NAME            # remove the work tree and its target folder, report what was freed
tools/dev/win_host.sh --help                # all commands, the environment variables and the exit codes
```

| Command | What it does there |
|---|---|
| `sync [--dry-run] [NAME]` | Replaces `<root>\work\NAME` with the files of this checkout that git does not ignore (tracked or not, as they are on disk, modification times kept to 100 ns, so cargo rebuilds only what changed). A file deleted here disappears there, and a file is sent under the upper and lower case its name has on disk (after a rename that changes only case, git on macOS goes on listing the old name; `sync` says so when it differs). The tree is usable only after the other side has compared every name and the total size with what was sent; until then `cargo`, `exec`, `gate` and `ps NAME` refuse it. `--dry-run` lists what would be sent and contacts nothing. |
| `cargo NAME ARGS...` | `cargo ARGS` in the work tree with `CARGO_TARGET_DIR=<root>\target-NAME`; the exit code is cargo's. |
| `exec NAME PROGRAM [ARGS...]` | Runs a program in the work tree. Arguments arrive exactly as given (spaces, quotes, empty arguments, non-ASCII text); there is no shell syntax. |
| `gate NAME [STEP...]` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked --no-fail-fast`, `cargo build --workspace --locked`. Every step runs even after a failure, and every test program runs even after one failed. One summary line per step (result, seconds, log file), the whole output of each step in `<root>\logs\NAME\{fmt,clippy,test,build}.log`, the last 30 lines of a failed step on standard error (for the test step also the names of the tests that failed), exit code 1 if any step failed. The summary is also in `<root>\logs\NAME\gate.txt`, rewritten after every step (its last line is `still running` until the end), so `fetch logs/NAME/gate.txt` shows how far a gate has come, also after a lost connection. Naming steps (`gate NAME clippy test`) runs only those. |
| `ps [NAME]` | Runs a PowerShell script read from standard input (`$root` is the work root; with NAME it starts in the work tree, `$work`). For scripts up to about 90 KB. |
| `fetch REMOTE LOCAL` | Copies one file back; REMOTE is relative to the root, with `/`. |
| `clean NAME` | Removes `<root>\work\NAME` and `<root>\target-NAME`, nothing else (the logs stay), and says how much that freed. |
| `disk`, `info` | Free space with the size of every work tree and target folder; toolchain versions and the work trees that exist. |

Exit codes: the program's own for `cargo`, `exec` and `ps` (a code outside 0..254 is reported as 254); 1 a gate
step failed, or a `ps` script stopped with an error; 2 bad usage, name, root or host (nothing was contacted);
3 the checkout cannot be mirrored as it is (nothing was changed on the other machine); 4 the other machine
refused or failed (no such work tree, unfinished sync, folder in use, a link in the way, not a Windows machine);
127 `exec` did not find the program; 255 the machine could not be reached, which is one line with ssh's own
reason (`win_host: cannot reach '<host>' over SSH: ...`), or the connection was lost. Nothing else gives 255,
except a `ps` script that itself ends with `exit 255`. A program that was running when the connection was lost
keeps running there.

What the script refuses, by design:

- A NAME that is not 1 to 64 letters, digits, `.`, `_`, `-` starting with a letter or digit (so `..`, paths,
  quotes and Windows device names never reach the other machine), and a root that is not a plain folder on a
  drive: `D:\` or `C:\` (a drive's top folder), an empty `ASAMU_WIN_ROOT` (unset it for the default; an empty
  value is not read as `D:\asamu`), a relative or network path, a root with `..`, a space, a quote or a device
  name such as `NUL` in it. `sync`, `gate` and `clean` write and delete only `<root>\work\NAME`,
  `<root>\target-NAME` and `<root>\logs\NAME`. The PowerShell side checks the name and the root again, because
  it is the side that deletes, and never deletes through a junction or symbolic link: not when the work tree is
  one, not when `<root>\work` or the root itself is one. A link inside a work tree is removed as a link; what
  it points to is left alone.
- A file name is never part of a command line. The files travel numbered in a tar stream with the list of
  their names as one more file, so a name such as `& calc.exe &.txt`, `$(calc).txt` or `%SystemRoot%.txt` is
  data on the whole way; a list that names a place outside the work tree (`..`, an absolute path, `\`) is
  refused by the other side, which then leaves the tree marked unfinished.
- A checkout that cannot be mirrored exactly: a symbolic link (Windows needs a privilege to create one, and
  following it could send files from outside the checkout), a file name Windows cannot hold (`< > : " | ? * \`,
  control characters, a trailing dot or space, a device name such as `NUL.txt`), two names that differ only in
  case, or no files at all. It says which files and sends nothing. A repository of its own inside the checkout
  (another worktree, a submodule) is left out and reported. A file that git still lists and that is no longer
  on disk is not sent; that includes a file whose place a folder took, and the other way round.

### Setting up the second machine

Windows 10 or 11 with the OpenSSH server enabled, key authentication for your user, and `cmd.exe` as the SSH
default shell (the Windows default); Rust with the MSVC build tools; a folder for the work root. PowerShell 5.1
and `tar` ship with Windows. On the development machine: bash, `ssh`/`scp`, `git`, `python3` (for `sync`),
`iconv` and `base64`. To run tests that need the game or converted data there, point them at it with
`ASAMU_WIN_ENV`, e.g. `ASAMU_WIN_ENV='$env:ASAMU_CONVERTED_DIR="D:\asamu\converted"'`.

Rules for a machine that is somebody's own computer:

- Write only below the work root. One work tree and one target folder per NAME; do not touch another
  person's or agent's NAME.
- An original game install on that machine is read-only, as everywhere: never modify, move or delete anything
  in it, and never start the game or Steam over SSH. Converted game data produced there stays there; it is
  never copied into a checkout or uploaded.
- Install nothing and change no setting (system, power, registry, services, firewall, antivirus); do not
  reboot, log off or lock the machine, and do not stop processes you did not start. A program started over SSH
  cannot put a window on that machine's screen.

### What was measured (Windows 11 build 26200, OpenSSH for Windows 9.5, PowerShell 5.1; 2026-10-10 and -11)

These explain the less obvious parts of the script. Each was observed on the real machine; the scripts that
showed them are not kept.

| Observation | Consequence in the script |
|---|---|
| Windows' `tar` (bsdtar 3.8.8) cannot create a file whose name has characters outside the machine's legacy code page: from a pax archive with UTF-8 names it wrote `über-äö café.txt` but skipped `日本語/日本語.txt` and an emoji name ("empty or unreadable filename"), with or without `chcp 65001`, `LC_ALL`, `LANG` or `--options hdrcharset=UTF-8`. | `sync` sends the files numbered (`.win_host_incomplete\000001`, ...) with the list of names as a file; PowerShell moves each to its name. That folder is also the mark of an unfinished sync. |
| Standard input for PowerShell stops arriving after 0.27 to 2.3 MB (10 of 10 runs with 4 MB or more, with three different ways of reading; 350 KB and 1.4 MB arrived), and `ReadToEnd` on 40 KB waited forever in 9 of 18 runs. Native programs had no such trouble: Python read 16 MB in under a second (8 of 8 runs), and `tar` unpacked a 42 MB checkout 30 times in a row. | The PowerShell text of a command (about 30 KB as sent) ends with a line holding a dot and is checked against its announced length; nothing waits for the end of input. Scripts over about 90 KB are refused. File contents never pass through PowerShell's standard input. |
| A nested `cmd /c` in an SSH command keeps the closing quote (`ssh host 'cmd /c echo hi'` prints `hi"`). | No nested `cmd`: PowerShell text travels as `-EncodedCommand`, and the only other command line (`tar -xf - -C <root>\work\NAME`) needs no quoting because of the NAME and root rules. |
| PowerShell 5.1 drops empty arguments and splits arguments that contain quotes when it starts a program. | `cargo` and `exec` build the command line themselves (Microsoft C runtime rules); 23 awkward arguments, among them `a b\`, `sp q"uo te`, an empty one, `$env:PATH`, a line break and non-ASCII text, arrived unchanged. |
| PowerShell reports its own errors as CLIXML on standard error when started this way. | Errors are caught and printed as one plain line; warnings are converted back to text. |
| Killing the SSH client leaves the PowerShell process (and what it started) running on the other machine. | Exit code 255 says so; look with `ps` before starting the same build again. |
| When the connection is lost while a command's PowerShell text is still on its way, the session there waits for the rest for ever (one was found idle four minutes after `ssh` had given up with "server not responding"). | A second thread in the session ends it when the text has not arrived within 120 seconds. A text that arrived is not affected however long it runs. |
| `[IO.Path]::GetFullPath` leaves `D:\asamu\COM1` as it is on this Windows 11 (older versions turn it into the device), so comparing a path with its normal form does not catch a device name. | Device names in the root and in NAME are refused by name, on both sides. |

Checked on the real machine with a scratch checkout on 2026-10-10: 20 files, among them names with spaces, an
apostrophe, `-rf`, `--help`, `%OS%`, `$HOME`, `&`, `^`, a backtick, Japanese and an emoji, arrived with identical
names, sizes and SHA-256 and a modification time exact to 100 ns; a file held open by another program made
`sync` fail with exit code 4 and left the tree marked unfinished (`cargo` and `gate` refused it) until the next
successful sync; a junction inside the work tree was removed by `sync` without touching the folder it pointed
to; a work tree that was itself a junction was refused by `sync` and `clean`; `clean` with a file of the work
tree held open removed the target folder, reported the work tree as in use (exit code 4) and finished on the
next call. Timings then: `sync` of a 42 MB scratch checkout 4 s, 30 times in a row; `clean` of a 19.5 GB target
folder (19,970 files) 3.7 s. The gate from an empty target folder while four other builds were running on the
machine (24 cores): fmt 2.2 s, clippy 254.0 s, test 941.2 s, build 100.9 s.

Checked again on 2026-10-11, with the script as it is now:

- **`sync` is an exact mirror.** A scratch checkout outside the repository (40 files, 6.4 MB: a 6 MB file of
  random bytes, a file with all 256 byte values, a text file with CR LF and lone CR line ends below a
  `.gitattributes` that asks for conversion, an empty file, a 190-character path, and names such as
  `& calc.exe &.txt`, `$(calc).txt`, `'; Remove-Item -Recurse x; '.txt`, `%SystemRoot%/%CD%.txt`, names with
  spaces, apostrophes, backticks, Japanese and an emoji) was synced, and the list of every file's name,
  SHA-256, size and modification time (in 100 ns) was computed on both sides: identical. Then, in one step, a
  file was changed without changing its size, one grew, two were deleted, one renamed, one moved into a new
  folder, a tracked file and two tracked folders renamed in case only, a tracked file replaced by a folder and
  a tracked folder by a file, and new files added; after the next `sync` the two lists were identical again.
  This is where two faults were found and fixed: the case-only renames arrived under their old names (git on
  macOS lists those), and the file and folder that swapped places made `sync` refuse the checkout. A file with a
  double quote in its name and a symbolic link out of the checkout were each refused with exit code 3, and the
  tree on the other machine was what it was before, byte for byte. `sync` of this checkout (506 files, 16 MB):
  3.2 to 3.5 s.
- **A transfer that is cut off** (a wrapper around `ssh` that passes on only the first 300 KB): exit code 4;
  `cargo`, `gate` and `exec` refused the tree with exit code 4; the next `sync` repaired it.
- **`clean` removes its two folders and nothing else.** A work tree (40 files) and a target folder (3 MB, one
  file read-only), each with a junction to a folder elsewhere below the root: both gone in 1.5 s; the names in
  the root, in `work`, in `logs` and in `scratch` before and after differed by exactly those two; the file
  behind the junctions and the log folder were still there; a second `clean` found nothing and said so. With
  the names `..`, `NAME\..\other`, `NAME & echo x`, `%CD%`, `nul` and an empty one, and with the roots `D:\`,
  `C:\`, an empty one, `D:\asamu\..`, a network path and `D:\nul`, `clean` and `sync` ended with exit code 2
  before anything was contacted.
- **A machine that cannot be reached** is one line and exit code 255, with nothing on standard output: an
  address that does not answer (after the 10 s connect timeout), a host name that does not resolve, and a
  machine without an SSH server.
- **Arguments**: `exec` passed `a b`, `q"uo te`, an empty argument, `it's`, `back\`, `a\\"b`, `$env:PATH`,
  `%OS%`, `&calc`, `^`, `x;y` and `café` to a program unchanged; exit code 7 came back as 7, 300 as 254 with
  the real code on standard error, a missing program as 127.
- Any command costs 1.2 to 1.3 s before its own work starts; `disk` took 21 s for 264 GB in 13 work trees and
  14 target folders.
- **The gate** on this workspace with a target folder that held a build of an older state: fmt 2.6 s, clippy
  25.1 s, test 178.2 s, build 73.9 s, 279.8 s in all (again right after it, with nothing to rebuild: 2.1 s,
  4.1 s, 55.9 s and 1.4 s). The test step ran 137 test programs and doc-test sets (2,236
  tests passed, 5 ignored) and failed, which the gate reported with exit code 1, the end of the log and the
  name of the one test that failed: `asamu-locate`'s `windows_backslash_paths_in_vdf_are_unescaped`, which
  finds the real Steam folder of that machine where it expects none. The game installed there was hidden from
  the other tests with `ASAMU_WIN_ENV='$env:ASAMU_STEAM_ROOT="D:\asamu\scratch\no-steam"'`; without that,
  `asamu-inventory --test real_install` fails as well (it compares the install it finds with the Mac
  inventory). Both are tests that do not yet expect a Windows machine with the game on it.

### Testing the script

Two tests, both run by CI (`.github/workflows/ci.yml`: the first in the hygiene job on Linux and in the macOS
build job, the second in the Windows build job).

```bash
tools/dev/test_win_host.sh                                    # no Windows machine needed
tools/dev/win_host.sh ps < tools/dev/test_win_host_remote.ps1 # the PowerShell half, on the Windows machine
```

`test_win_host.sh` puts a fake `ssh` and `scp` first in `PATH` that record their arguments and standard
input, and checks what `win_host.sh` would have sent: the command lines, the decoded PowerShell text and the
tar stream of `sync` (names, bytes, times), for normal use and for hostile input (67 work tree names such as
`..`, `h1\..\..\x`, `a;b`, `$(id)` or `%SystemRoot%`, 54 roots such as `D:\`, `C:\`, an empty one or
`D:\asamu\..`, 19 hosts that look like options or carry syntax, each with every command; file names that are
syntax to `cmd.exe` or PowerShell, symbolic links, case-only renames, an unreadable file, an empty checkout, an
unreachable machine, a connection lost half way). At its end it checks every call the fakes saw in the whole
run once more against the few forms a command line or script may have, so that no input can add a form of its
own. It needs bash, git and python3. Run on 2026-10-11: macOS (bash 3.2; Python 3.9 and 3.14; UTF-8 and C
locale) 1,189 checks, two skipped because the file system ignores case and refuses names that are not UTF-8;
Linux (Ubuntu 26.04, bash 5.3, Python 3.14; UTF-8 and C locale) 1,194 checks, one skipped. Each of 35 faults
put into the script and the packer on purpose made it fail.

`test_win_host_remote.ps1` tests the side that deletes, on a Windows machine with Windows PowerShell 5.1, in
a scratch folder that it removes at the end (`<root>\scratch\win_host_remote_test`, or `WIN_HOST_TEST_DIR`;
on the machine itself: `powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\test_win_host_remote.ps1`).
It checks that the function every path comes from gives out nothing but `<root>\work\NAME`,
`<root>\target-NAME` and `<root>\logs\NAME` for 67 hostile names and 54 hostile roots; that `clean` leaves
15 neighbours with similar names (`work\ab`, `target-a.b`, `targets-a`, `logs\ab`, ...) and its own log folder exactly as they were, with
a read-only file, paths longer than 260 characters and junctions in the tree it removes; that a work tree, a
`<root>\work` or a root that is a link is refused by `clean` and `sync` and nothing behind the link changes;
that a file in use is reported and the tree stays marked unfinished; that a `sync` whose list of names holds
`../escape.txt`, an absolute path, a name with a trailing dot or 27 other such names is refused and writes
nothing outside the work tree; and that 35 awkward arguments reach a program unchanged. 628 checks in 4 to 5
seconds, run both ways on the real machine. Of 17 faults put into the PowerShell half on purpose 12 made it
fail; the other 5 removed one of two fences that each refuse the same input (for example the form of a name
is checked, and the path made from it is compared with its normal form), so the outcome did not change.
What neither test covers is the SSH server itself and Windows' `tar`: that is what the measurements above are
for.

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
