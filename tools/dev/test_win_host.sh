#!/usr/bin/env bash
# Self-test of tools/dev/win_host.sh that needs no Windows machine.
#
# A fake `ssh` and a fake `scp` are put first in PATH. They record their
# arguments and what they are given on standard input, and answer the way the
# test asks (success, an exit code at the n-th call, "host unreachable"). The
# test then checks what win_host.sh would have sent: the command lines, the
# PowerShell text, and the tar stream of `sync`, for normal use and for hostile
# input (work tree names such as `..`, file names with spaces, quotes, leading
# dashes and non-ASCII characters, symbolic links, an empty checkout).
#
# Every call the fakes saw, in all of these cases, is checked once more at the
# end against the few forms a command line or script of win_host.sh may have
# (the "audit"): no input, however hostile, may add a form of its own.
#
# What it cannot check is the PowerShell half (win_host_remote.ps1) and the
# real SSH server: tools/dev/test_win_host_remote.ps1 tests that half on a
# Windows machine, and docs/BUILDING.md lists what was run on a real one.
#
# Needs bash, git, python3 and the usual POSIX tools; runs on Linux and macOS
# (CI runs it on both).
#
#   tools/dev/test_win_host.sh          exit code 0 when every check passes
#   KEEP=1 tools/dev/test_win_host.sh   keep the scratch folder and print where it is
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for tool in git python3; do
  command -v "$tool" > /dev/null 2>&1 || {
    echo "test_win_host: $tool is needed" >&2
    exit 2
  }
done

T="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/win_host_test.XXXXXX")" && pwd -P)"
cleanup() {
  if [ "${KEEP:-0}" = 1 ]; then echo "scratch folder kept: $T"; else rm -rf -- "$T"; fi
}
trap cleanup EXIT

LOG="$T/log"
BIN="$T/bin"
AUDIT="$T/audit"
mkdir -p "$LOG" "$BIN" "$AUDIT"
# The caller's settings must not leak into the script under test.
unset ASAMU_WIN_HOST ASAMU_WIN_ROOT ASAMU_WIN_ENV GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
export FAKE_LOG="$LOG"

# --- the fakes ---------------------------------------------------------------

cat > "$BIN/ssh" << 'FAKE'
#!/usr/bin/env bash
# Fake ssh: records the call, then behaves as FAKE_MODE / FAKE_FAIL_AT ask.
n=$(($(cat "$FAKE_LOG/ssh.count" 2> /dev/null || echo 0) + 1))
echo "$n" > "$FAKE_LOG/ssh.count"
printf '%s\0' "$@" > "$FAKE_LOG/ssh.$n.args"
cat > "$FAKE_LOG/ssh.$n.stdin"
remote="${*: -1}"
if [ "${FAKE_MODE:-ok}" = unreachable ]; then
  if [ -n "${FAKE_REASON-unset}" ]; then
    echo "debug1: this line is not the reason" >&2
    printf '%s\r\n\n' "${FAKE_REASON-ssh: connect to host winbox port 22: Operation timed out}" >&2
  fi
  exit 255
fi
if [ "$remote" = 'echo asamu-win-host %OS%' ]; then
  if [ "${FAKE_MODE:-ok}" = notwindows ]; then printf 'asamu-win-host %%OS%%\n'; else printf 'asamu-win-host Windows_NT\r\n'; fi
  exit 0
fi
if [ "$n" = "${FAKE_OUTPUT_AT:-}" ]; then
  cat "$FAKE_LOG/../fake.stdout"
  cat "$FAKE_LOG/../fake.stderr" >&2
fi
if [ "$n" = "${FAKE_FAIL_AT:-}" ]; then exit "${FAKE_FAIL_RC:-1}"; fi
exit 0
FAKE

cat > "$BIN/scp" << 'FAKE'
#!/usr/bin/env bash
# Fake scp: records the call and "copies" a file to the last argument.
n=$(($(cat "$FAKE_LOG/scp.count" 2> /dev/null || echo 0) + 1))
echo "$n" > "$FAKE_LOG/scp.count"
printf '%s\0' "$@" > "$FAKE_LOG/scp.$n.args"
if [ -n "${FAKE_SCP_RC:-}" ]; then exit "$FAKE_SCP_RC"; fi
echo fetched > "${*: -1}"
FAKE
chmod +x "$BIN/ssh" "$BIN/scp"
PATH="$BIN:$PATH"
export PATH

# --- what the test reads the recordings with ----------------------------------

cat > "$T/inspect.py" << 'PY'
"""Reads what the fake ssh recorded. Usage: inspect.py COMMAND LOG N [...]"""
import base64
import hashlib
import io
import os
import re
import sys
import tarfile

PS = "powershell -NoProfile -NonInteractive -EncodedCommand "


def args(log, n, tool="ssh"):
    with open(os.path.join(log, "%s.%s.args" % (tool, n)), "rb") as handle:
        return [a.decode("utf-8") for a in handle.read().split(b"\0")[:-1]]


def stdin(log, n):
    with open(os.path.join(log, "ssh.%s.stdin" % n), "rb") as handle:
        return handle.read()


def bootstrap(log, n):
    remote = args(log, n)[-1]
    assert remote.startswith(PS), "not a PowerShell call: %r" % remote
    encoded = remote[len(PS):]
    assert re.fullmatch(r"[A-Za-z0-9+/=]+", encoded), "the encoded command has other characters"
    assert len(remote) < 8000, "the command line is %d characters; cmd.exe stops at 8191" % len(remote)
    return base64.b64decode(encoded, validate=True).decode("utf-16-le")


def script(log, n):
    bootstrap(log, n)
    lines = stdin(log, n).decode("ascii").split("\n")
    assert lines[-1] == "" and lines[-2] == ".", "the script must end with a line that holds one dot"
    body = lines[1:-2]
    assert all(0 < len(line) <= 76 for line in body), "base64 lines must be 1..76 characters"
    text = "".join(body)
    assert int(lines[0]) == len(text), "the first line must be the number of base64 characters"
    return base64.b64decode(text, validate=True).decode("utf-8")


def command(log, n, lib, root):
    """The part of the script after the root line and the shared functions."""
    text = script(log, n)
    with open(lib, "rb") as handle:
        head = "$root = '%s'\n" % root + handle.read().decode("utf-8") + "\n"
    assert text.startswith(head), "the script must start with the root line and win_host_remote.ps1"
    rest = text[len(head):]
    assert rest.endswith("\n")
    return rest[:-1]


TEXT = re.compile(r"\(WhText '([A-Za-z0-9+/=]*)'\)")

# --- the audit: the only forms a call of ssh or scp may have -------------------

NAME = r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}"
ROOT = r"[A-Za-z]:(?:\\[A-Za-z0-9._-]+)+"
HOST = r"[A-Za-z0-9_][A-Za-z0-9._@-]*"
OPTIONS = ["-o", "BatchMode=yes", "-o", "RequestTTY=no", "-o", "ConnectTimeout=10",
           "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=8"]
T = r"\(WhText '[A-Za-z0-9+/=]*'\)"
STEP = r"'(?:fmt|clippy|test|build)'"
PLAIN = [
    r"WhInfo",
    r"WhDisk",
    r"WhClean '%s'" % NAME,
    r"WhSyncPrepare '%s'" % NAME,
    r"WhSyncVerify '%s' [0-9]+ [0-9]+ '[0-9a-f]{64}'" % NAME,
    r"\. \(\[scriptblock\]::Create\(%s\)\)" % T,
]
ENTERED = [
    r"WhRun 'cargo' @\(%s(?:, %s)*\)" % (T, T),
    r"WhRun %s @\((?:%s(?:, %s)*)?\)" % (T, T, T),
    r"WhGate '%s' @\((?:%s(?:,%s)*)?\)" % (NAME, STEP, STEP),
    r"\. \(\[scriptblock\]::Create\(%s\)\)" % T,
]


def audit_call(folder, n, lib, env):
    """Why call N of ssh in this folder is not of an expected form, or None."""
    words = args(folder, n)
    if words[:-2] != OPTIONS or not re.fullmatch(HOST, words[-2]):
        return "options or host %r" % (words[:-1],)
    remote = words[-1]
    if remote == "echo asamu-win-host %OS%":
        return None
    if re.fullmatch(r"%%SystemRoot%%\\System32\\tar\.exe -xf - -C %s\\work\\%s" % (ROOT, NAME), remote):
        return None
    if not remote.startswith(PS):
        return "command line %r" % remote
    text = script(folder, n)
    first, _, rest = text.partition("\n")
    if not re.fullmatch(r"\$root = '%s'" % ROOT, first):
        return "root line %r" % first
    if not rest.startswith(lib + "\n") or not rest.endswith("\n"):
        return "the shared functions are not what follows the root line"
    command = rest[len(lib) + 1:-1]
    if any(re.fullmatch(form, command) for form in PLAIN):
        return None
    head = re.match(r"\$work = WhEnter '%s'\n" % NAME, command)
    # After the caller's environment comes the line that keeps the run silent.
    mute = "$env:ASAMU_MUTE = '1'\n"
    if head and command[head.end():].startswith(env + "\n" + mute):
        command = command[head.end() + len(env) + 1 + len(mute):]
        if any(re.fullmatch(form, command) for form in ENTERED):
            return None
    return "script command %r" % command


def audit(top, lib_path):
    """Checks every recorded call below TOP; prints one line per call that is not as expected."""
    with open(lib_path, "rb") as handle:
        lib = handle.read().decode("utf-8")
    seen = 0
    for run in sorted(os.listdir(top)):
        folder = os.path.join(top, run)
        with open(os.path.join(folder, "env"), "rb") as handle:
            env = handle.read().decode("utf-8")
        for entry in sorted(os.listdir(folder)):
            tool, _, tail = entry.partition(".")
            if not tail.endswith(".args"):
                continue
            n = tail[:-len(".args")]
            seen += 1
            try:
                if tool == "ssh":
                    why = audit_call(folder, n, lib, env)
                else:
                    words = args(folder, n, "scp")
                    remote = r"%s:%s(?:/[A-Za-z0-9_][A-Za-z0-9._-]*)+" % (HOST, ROOT.replace("\\\\", "/"))
                    good = (words[:-2] == ["-q"] + OPTIONS and re.fullmatch(remote, words[-2])
                            and "/../" not in words[-2] and words[-1].startswith(("/", "./")))
                    why = None if good else "scp %r" % (words,)
            except (AssertionError, ValueError, UnicodeDecodeError) as error:
                why = "cannot be read back: %s" % error
            if why:
                sys.stdout.write("%s call %s of run %s: %s\n" % (tool, n, run, why))
    sys.stderr.write("%d\n" % seen)


def main(argv):
    what, log, n = argv[1], argv[2], argv[3]
    out = sys.stdout
    if what == "audit":
        audit(log, n)
    elif what == "args":
        out.write("".join(a + "\n" for a in args(log, n, argv[4] if len(argv) > 4 else "ssh")))
    elif what == "bootstrap":
        out.write(bootstrap(log, n) + "\n")
    elif what == "command":
        # The command with every encoded text replaced by <T>; the texts
        # themselves, NUL-separated, go to the file named last.
        text = command(log, n, argv[4], argv[5])
        with open(argv[6], "wb") as handle:
            for encoded in TEXT.findall(text):
                handle.write(base64.b64decode(encoded, validate=True) + b"\0")
        out.write(TEXT.sub("<T>", text) + "\n")
    elif what == "tar":
        # The tar stream of call N must hold exactly the files named in NAMES
        # (one per line): first the list of names, in the order of their
        # bytes, as .win_host_incomplete/manifest; then file number i of that
        # list as the plain member .win_host_incomplete/<i>, with the bytes and
        # the time of the file in REPO, and nothing of the sender's.
        # Prints what the verify command must say: files, bytes, SHA-256.
        repo, names_file = argv[4], argv[5]
        with open(names_file, "rb") as handle:
            expected = sorted(handle.read().decode("utf-8").split("\n")[:-1], key=lambda s: s.encode("utf-8"))
        archive = tarfile.open(fileobj=io.BytesIO(stdin(log, n)), mode="r:")
        members = archive.getmembers()
        wanted = [".win_host_incomplete/manifest"] + [".win_host_incomplete/%06d" % (i + 1) for i in range(len(expected))]
        assert [m.name for m in members] == wanted, "members %r, expected %r" % ([m.name for m in members], wanted)
        for member in members:
            assert member.isreg(), "%s is not a plain file in the archive" % member.name
            assert "path" not in member.pax_headers and "linkpath" not in member.pax_headers
            assert member.uname == "" and member.gname == "" and member.uid == 0 and member.gid == 0
        manifest = archive.extractfile(members[0]).read()
        assert manifest == "".join(name + "\n" for name in expected).encode("utf-8"), "manifest %r" % manifest
        for member, name in zip(members[1:], expected):
            path = os.path.join(repo, name)
            with open(path, "rb") as handle:
                assert archive.extractfile(member).read() == handle.read(), "content of %s differs" % name
            info = os.stat(path)
            assert member.pax_headers.get("mtime") == "%d.%09d" % divmod(info.st_mtime_ns, 10**9), "time of %s" % name
        out.write("%d %d '%s'\n" % (len(expected), sum(m.size for m in members[1:]), hashlib.sha256(manifest).hexdigest()))
    else:
        raise SystemExit("unknown command " + what)


if __name__ == "__main__":
    try:
        main(sys.argv)
    except AssertionError as error:
        sys.stderr.write("inspect: %s\n" % error)
        sys.exit(1)
PY

# --- test helpers -------------------------------------------------------------

passed=0
failed=0
ok() {
  passed=$((passed + 1))
  echo "ok    $1"
}
bad() {
  failed=$((failed + 1))
  echo "FAIL  $1"
  if [ $# -ge 2 ]; then printf '%s\n' "$2" | sed 's/^/        /'; fi
}
# check DESCRIPTION COMMAND...: passes when the command succeeds.
check() {
  local what="$1" output
  shift
  if output="$("$@" 2>&1)"; then ok "$what"; else bad "$what" "$output"; fi
}
is() { # is DESCRIPTION ACTUAL EXPECTED
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "got:      $2${NL}expected: $3"; fi
}
has() { # has DESCRIPTION FILE TEXT
  if grep -qF -- "$3" "$2"; then ok "$1"; else bad "$1" "no '$3' in:${NL}$(cat "$2")"; fi
}
hasnt() { # hasnt DESCRIPTION FILE TEXT
  if grep -qF -- "$3" "$2"; then bad "$1" "'$3' found in:${NL}$(cat "$2")"; else ok "$1"; fi
}
NL=$'\n'

calls() { cat "$LOG/${1:-ssh}.count" 2> /dev/null || echo 0; }
inspect() { python3 "$T/inspect.py" "$@"; }
lines() { wc -l < "$1" | tr -d ' '; }

# wh REPO ARGS...: run REPO's win_host.sh with the fakes; RC, $T/out, $T/err.
RC=0
STDIN=/dev/null
wh() {
  local repo="$1"
  shift
  rm -rf "$LOG"
  mkdir -p "$LOG"
  "$repo/tools/dev/win_host.sh" "$@" > "$T/out" 2> "$T/err" < "$STDIN"
  RC=$?
  # For the audit at the end: what the fakes saw, and the ASAMU_WIN_ENV of this run.
  RUNS=$((RUNS + 1))
  if [ "$(calls)" != 0 ] || [ "$(calls scp)" != 0 ]; then
    cp -R "$LOG" "$AUDIT/$RUNS"
    printf '%s' "${ASAMU_WIN_ENV:-}" > "$AUDIT/$RUNS/env"
  fi
}
RUNS=0

# A scratch checkout holding a copy of the script under test.
make_repo() {
  mkdir -p "$1/tools/dev"
  cp "$HERE/win_host.sh" "$HERE/win_host_remote.ps1" "$HERE/win_host_pack.py" "$1/tools/dev/"
  git -c init.defaultBranch=main init -q "$1"
}

# The command part of call N of the last run (texts replaced by <T>, the texts in $T/texts).
command_of() { inspect command "$LOG" "$1" "$HERE/win_host_remote.ps1" "${2:-D:\\asamu}" "$T/texts"; }

SSH_OPTIONS="-o${NL}BatchMode=yes${NL}-o${NL}RequestTTY=no${NL}-o${NL}ConnectTimeout=10${NL}-o${NL}ServerAliveInterval=15${NL}-o${NL}ServerAliveCountMax=8"
SSH_HEAD="$SSH_OPTIONS${NL}winbox"
PROBE='echo asamu-win-host %OS%'
export ASAMU_WIN_HOST=winbox

# --- 1. usage, names, roots, hosts ---------------------------------------------

R="$T/repo"
make_repo "$R"
echo "hello" > "$R/hello.txt"
git -C "$R" add -A

wh "$R" --help
is "help: exit code 0" "$RC" 0
has "help: lists sync" "$T/out" "sync [--dry-run] [NAME]"
has "help: lists gate" "$T/out" "gate NAME [STEP...]"
has "help: lists clean" "$T/out" "clean NAME"
has "help: lists disk" "$T/out" "disk "
is "help: nothing contacted" "$(calls)" 0
wh "$R"
is "no command: exit code 2" "$RC" 2
wh "$R" frobnicate
is "unknown command: exit code 2" "$RC" 2
is "unknown command: nothing contacted" "$(calls)" 0

LONG64="$(printf 'a%.0s' 1 2 3 4 5 6 7 8)"
LONG64="$LONG64$LONG64$LONG64$LONG64$LONG64$LONG64$LONG64$LONG64"
# An empty NAME argument is a bad name too, not "use the default".
# Names that would leave <root>\work, name another folder there, or be read as
# syntax by cmd.exe or PowerShell on the way.
for name in '..' '.' '...' 'a..b' 'a/b' 'a\b' '../x' '..\x' 'h1\..\..\x' 'h1/../../x' '\x' '/x' 'C:' 'C:x' 'C:\x' 'h1:stream' \
  '-x' '--help' '-' 'a b' ' h1' 'h1 ' 'a;b' 'a&b' 'a|b' 'a>b' 'a<b' 'a^b' 'a,b' 'a=b' 'a(b)' 'a!b' 'a*' 'a?' 'a~1' 'a+b' 'a@b' 'a#b' \
  '$(id)' '`id`' '$x' '${x}' "a'b" "'" "h1'; Remove-Item -Recurse D:\\asamu; '" 'a"b' 'a%b' '%SystemRoot%' '%CD%' \
  'h1.' '.hidden' '_x' 'con' 'NUL' 'aux.txt' 'com1' 'LPT9' 'Con.h1' 'prn.a.b' \
  "$(printf 'n\303\244me')" "$(printf '\357\274\216\357\274\216')" "$(printf 'a\nb')" "$(printf 'h1\r')" "$(printf 'h1\tx')" "$(printf 'h1\nWhClean h2')" '' "a$LONG64"; do
  shown="$(printf '%q' "$name")"
  for sub in sync cargo exec gate clean ps; do
    case "$sub" in
      cargo) wh "$R" cargo "$name" build ;;
      exec) wh "$R" exec "$name" prog ;;
      *) wh "$R" "$sub" "$name" ;;
    esac
    if [ "$RC" = 2 ] && [ "$(calls)" = 0 ] && [ "$(calls scp)" = 0 ]; then ok "bad name $shown refused by $sub"; else bad "bad name $shown refused by $sub" "exit $RC, $(calls) ssh calls: $(cat "$T/err")"; fi
  done
done
for name in h1 H1 a 0 a.b a-b a_b wf_fddd8ce3-3cf-1 "$LONG64"; do
  wh "$R" clean "$name"
  is "good name $name accepted" "$RC" 0
done

# Roots that are a drive's top folder, not a folder on a drive, or not plain:
# everything `clean` and `sync` delete is below the root. An empty value is
# not "the default".
for root in '' 'D:\' 'C:\' 'c:\' 'D:\\' 'D:' 'C:' 'D:.' 'D:..' 'D:asamu' 'asamu' '\' '\asamu' '/' '.' '..' '\\server\share\x' '\\?\D:\asamu' '\\.\D:\asamu' \
  'D:\asamu\' 'D:\asamu\..' 'D:\asamu\..\x' 'D:\..' 'D:\.' 'D:\.\asamu' 'D:\asamu.' 'D:\asamu.\x' 'D:\a b' 'D:\asamu ' ' D:\asamu' 'D:\a"b' "D:\\a'b" \
  'D:\a&b' 'D:\a;b' 'D:\a%b' 'D:\a^b' 'D:\a$b' 'D:\a,b' 'D:\a(b)' 'D:/asamu' 'D:\asamu/x' 'DD:\asamu' '1:\asamu' 'D:\asamu\\x' 'D:\asamu:x' 'D::\asamu' \
  'D:\nul' 'D:\asamu\COM1' 'D:\aux.d\x' 'D:\con\asamu' "$(printf 'D:\\asamu\nx')" "$(printf 'D:\\asamu\r')" "$(printf 'D:\\\303\244')" "D:\\$LONG64$LONG64"; do
  for sub in info disk "clean h1" "sync h1" "cargo h1 build" "gate h1" "fetch logs/x never-written.tmp"; do
    # shellcheck disable=SC2086
    ASAMU_WIN_ROOT="$root" wh "$R" $sub
    if [ "$RC" = 2 ] && [ "$(calls)" = 0 ]; then ok "bad root $(printf '%q' "$root") refused by ${sub%% *}"; else bad "bad root $(printf '%q' "$root") refused by ${sub%% *}" "exit $RC, $(calls) ssh calls"; fi
  done
done
ASAMU_WIN_ROOT='' wh "$R" clean h1
has "empty root: says so" "$T/err" "ASAMU_WIN_ROOT is set and empty"
ASAMU_WIN_ROOT='D:\' wh "$R" clean h1
has "a drive as root: says so" "$T/err" "it must name a folder (not a drive)"
ASAMU_WIN_ROOT='E:\build\asamu-2' wh "$R" clean h1
is "another root accepted" "$RC" 0
check "another root: it is the root of the script sent" test "$(command_of 2 'E:\build\asamu-2')" = "WhClean 'h1'"
for root in 'c:\x' 'Z:\a\b\c.d\e_f\g-h' 'D:\.cache' 'D:\console' 'D:\com10'; do
  ASAMU_WIN_ROOT="$root" wh "$R" clean h1
  is "root $root accepted" "$RC/$(command_of 2 "$root" 2>&1)" "0/WhClean 'h1'"
done
wh "$R" clean h1
is "no ASAMU_WIN_ROOT: the default root" "$RC/$(command_of 2 'D:\asamu' 2>&1)" "0/WhClean 'h1'"

for host in '-oProxyCommand=touch x' '-oProxyCommand=x' '-v' '-' 'a b' 'a;b' 'host:path' '.x' '@x' "$(printf 'h\303\266st')" 'a$b' 'a/b' 'a\b' "a'b" 'a"b' '$(id)' 'a,b' 'a%b' "$(printf 'a\nb')"; do
  for sub in info disk "clean h1" "sync h1" "cargo h1 build" "exec h1 prog" "gate h1" ps "fetch logs/x never-written.tmp"; do
    # shellcheck disable=SC2086
    ASAMU_WIN_HOST="$host" wh "$R" $sub
    if [ "$RC" = 2 ] && [ "$(calls)" = 0 ] && [ "$(calls scp)" = 0 ]; then ok "bad host $(printf '%q' "$host") refused by ${sub%% *}"; else bad "bad host $(printf '%q' "$host") refused by ${sub%% *}" "exit $RC, $(calls) ssh calls"; fi
  done
done
for sub in info disk "clean h1" "sync h1" "cargo h1 build" "exec h1 prog" "gate h1" ps "fetch logs/x never-written.tmp"; do
  # shellcheck disable=SC2086
  ASAMU_WIN_HOST='' wh "$R" $sub
  is "no host, ${sub%% *}: exit code 2, nothing contacted" "$RC/$(calls)/$(calls scp)" "2/0/0"
  has "no host, ${sub%% *}: says what to set" "$T/err" "set ASAMU_WIN_HOST"
done
ASAMU_WIN_HOST='user@win-box_2.example' wh "$R" info
is "user@host accepted" "$RC" 0

# --- 2. the commands that send PowerShell ---------------------------------------

wh "$R" info
is "info: exit code" "$RC" 0
is "info: two connections" "$(calls)" 2
is "info: the first is the probe" "$(inspect args "$LOG" 1)" "$SSH_HEAD$NL$PROBE"
is "info: ssh options and host of the second" "$(inspect args "$LOG" 2 | sed '$d')" "$SSH_HEAD"
check "info: bootstrap decodes, fits a command line" inspect bootstrap "$LOG" 2
inspect bootstrap "$LOG" 2 > "$T/boot" 2> /dev/null
has "info: the bootstrap reads up to the dot line, not to the end of input" "$T/boot" '$l -eq "."'
hasnt "info: the bootstrap does not wait for the end of input" "$T/boot" 'ReadToEnd'
has "info: the bootstrap ends a session whose script does not arrive" "$T/boot" 'Start-Sleep -Seconds 120; if (-not `$g.arrived) {'
has "info: the bootstrap ends a session whose script does not arrive (the exit)" "$T/boot" '[Environment]::Exit(4)'
has "info: the bootstrap notes that the script arrived before running it" "$T/boot" '$g.arrived = $true; $function:WinHost = '
is "info: command" "$(command_of 2)" "WhInfo"
wh "$R" disk
is "disk: command" "$(command_of 2)" "WhDisk"
wh "$R" disk extra
is "disk: no arguments" "$RC" 2
wh "$R" clean h1
is "clean: command" "$(command_of 2)" "WhClean 'h1'"
is "clean: two connections" "$(calls)" 2
wh "$R" clean
is "clean: NAME is required" "$RC" 2
wh "$R" clean h1 h2
is "clean: one NAME only" "$RC" 2
is "clean: one NAME only, nothing contacted" "$(calls)" 0

wh "$R" gate h1
is "gate: command" "$(command_of 2)" "\$work = WhEnter 'h1'${NL}${NL}\$env:ASAMU_MUTE = '1'${NL}WhGate 'h1' @()"
wh "$R" gate h1 clippy test
is "gate: chosen steps" "$(command_of 2)" "\$work = WhEnter 'h1'${NL}${NL}\$env:ASAMU_MUTE = '1'${NL}WhGate 'h1' @('clippy','test')"
wh "$R" gate h1 bogus
is "gate: unknown step refused" "$RC" 2
is "gate: unknown step, nothing contacted" "$(calls)" 0
wh "$R" gate h1 "fmt'); Remove-Item x; ('"
is "gate: hostile step refused" "$RC" 2
FAKE_FAIL_AT=2 FAKE_FAIL_RC=1 wh "$R" gate h1
is "gate: a failed step is exit code 1" "$RC" 1

# cargo: every argument arrives as an encoded text, none as PowerShell syntax.
set -- test --workspace -- --exact "it's a 'test'" 'a b' '' '$(touch x)' '`id`' '; Remove-Item -Recurse C:\' '-x' "$(printf 'caf\303\251 \342\234\223')" "$(printf '\342\200\231curly\342\200\231')" "$(printf 'two\nlines')" '"quoted"' 'back\slash\' '%OS%' '@args' '#x'
wh "$R" cargo h1 "$@"
is "cargo: exit code" "$RC" 0
is "cargo: command" "$(command_of 2)" "\$work = WhEnter 'h1'${NL}${NL}\$env:ASAMU_MUTE = '1'${NL}WhRun 'cargo' @($(printf '<T>, %.0s' "$@" | sed 's/, $//'))"
printf '%s\0' "$@" > "$T/expected"
check "cargo: the arguments, byte for byte" cmp "$T/expected" "$T/texts"
inspect command "$LOG" 2 "$HERE/win_host_remote.ps1" 'D:\asamu' "$T/texts" > "$T/cmd"
hasnt "cargo: no argument text outside the encoded form" "$T/cmd" "Remove-Item"
FAKE_FAIL_AT=2 FAKE_FAIL_RC=101 wh "$R" cargo h1 test
is "cargo: the exit code is cargo's" "$RC" 101
wh "$R" cargo h1
is "cargo: arguments are required" "$RC" 2
printf 'out line\n\000binary\377\n' > "$T/fake.stdout"
printf 'err line\r\n#< CLIXML\r\n<Objs Version="1.1.0.1" xmlns="http://schemas.microsoft.com/powershell/2004/04"><S S="warning">careful &lt;now&gt; &amp; "then"</S><S S="Error">first_x000D__x000A_</S><S S="Error">second_x000D__x000A_</S></Objs>' > "$T/fake.stderr"
FAKE_OUTPUT_AT=2 wh "$R" cargo h1 build
check "cargo: standard output passes through untouched" cmp "$T/out" "$T/fake.stdout"
is "cargo: PowerShell's CLIXML on standard error becomes plain lines" "$(cat "$T/err")" "err line${NL}WARNING: careful <now> & \"then\"${NL}first${NL}second"

ASAMU_WIN_ENV='$env:ASAMU_CONVERTED_DIR="D:\asamu\converted"' wh "$R" cargo h1 test
is "cargo: ASAMU_WIN_ENV on its own line" "$(command_of 2)" "\$work = WhEnter 'h1'${NL}\$env:ASAMU_CONVERTED_DIR=\"D:\\asamu\\converted\"${NL}\$env:ASAMU_MUTE = '1'${NL}WhRun 'cargo' @(<T>)"

wh "$R" exec h1 'C:\Program Files\some tool\x.exe'
is "exec: program without arguments" "$(command_of 2)" "\$work = WhEnter 'h1'${NL}${NL}\$env:ASAMU_MUTE = '1'${NL}WhRun <T> @()"
printf '%s\0' 'C:\Program Files\some tool\x.exe' > "$T/expected"
check "exec: the program name, byte for byte" cmp "$T/expected" "$T/texts"
wh "$R" exec h1 prog --flag 'a b'
is "exec: program with arguments" "$(command_of 2)" "\$work = WhEnter 'h1'${NL}${NL}\$env:ASAMU_MUTE = '1'${NL}WhRun <T> @(<T>, <T>)"
wh "$R" exec h1 ''
is "exec: empty program refused" "$RC" 2
FAKE_FAIL_AT=2 FAKE_FAIL_RC=37 wh "$R" exec h1 prog
is "exec: the exit code is the program's" "$RC" 37

printf '"quoted" $x = 1\nthrow '"'"'it'"'"'\n' > "$T/script.ps1"
STDIN="$T/script.ps1" wh "$R" ps
is "ps: command" "$(command_of 2)" ". ([scriptblock]::Create(<T>))"
{
  printf '"quoted" $x = 1\nthrow '"'"'it'"'"'\nif (-not $?) { exit 1 }'
  printf '\0'
} > "$T/expected"
check "ps: the script, byte for byte, plus the exit rule" cmp "$T/expected" "$T/texts"
STDIN="$T/script.ps1" wh "$R" ps h1
is "ps NAME: enters the work tree first" "$(command_of 2)" "\$work = WhEnter 'h1'${NL}${NL}\$env:ASAMU_MUTE = '1'${NL}. ([scriptblock]::Create(<T>))"
STDIN="$T/script.ps1" FAKE_FAIL_AT=2 FAKE_FAIL_RC=12 wh "$R" ps
is "ps: the exit code is the script's" "$RC" 12
wh "$R" ps h1 h2
is "ps: one NAME at most" "$RC" 2
python3 -c 'print("# " + "x" * 120000)' > "$T/large.ps1"
STDIN="$T/large.ps1" wh "$R" ps
is "ps: a script too large to arrive reliably is refused, not sent" "$RC/$(calls)" "2/1"
has "ps: a script too large to arrive reliably: says so" "$T/err" "too large to send reliably"

wh "$R" fetch logs/h1/test.log "$T/got.log"
is "fetch: exit code" "$RC" 0
is "fetch: scp arguments" "$(inspect args "$LOG" 1 scp)" "-q${NL}$SSH_OPTIONS${NL}winbox:D:/asamu/logs/h1/test.log${NL}$T/got.log"
(cd "$T" && wh "$R" fetch logs/h1/test.log -oProxyCommand=x)
is "fetch: a relative LOCAL can be neither an option nor host:path" "$(inspect args "$LOG" 1 scp | tail -n 1)" "./-oProxyCommand=x"
rm -f "$T/-oProxyCommand=x"
for remote in '../x' 'logs/../../x' '/abs' 'C:/Windows/x' 'a b' 'a;b' 'logs/-x' 'a//b' './x' '' 'a\b' "a'b" 'a$b'; do
  wh "$R" fetch "$remote" "$T/got.log"
  if [ "$RC" = 2 ] && [ "$(calls)" = 0 ] && [ "$(calls scp)" = 0 ]; then ok "fetch: REMOTE $(printf '%q' "$remote") refused"; else bad "fetch: REMOTE $(printf '%q' "$remote") refused" "exit $RC"; fi
done
FAKE_SCP_RC=1 wh "$R" fetch logs/h1/none.log "$T/got.log"
is "fetch: a failed copy is exit code 4" "$RC" 4

# --- 3. the machine cannot be reached -------------------------------------------

for sub in info disk "clean h1" "sync h1" "cargo h1 build" "exec h1 prog" "gate h1" ps "fetch logs/x never-written.tmp"; do
  # shellcheck disable=SC2086
  FAKE_MODE=unreachable wh "$R" $sub
  is "unreachable, ${sub%% *}: exit code 255" "$RC" 255
  is "unreachable, ${sub%% *}: one line, with ssh's reason" "$(cat "$T/err")" "win_host: cannot reach 'winbox' over SSH: ssh: connect to host winbox port 22: Operation timed out"
  is "unreachable, ${sub%% *}: gave up after the probe" "$(calls)/$(calls scp)" "1/0"
done
for reason in 'ssh: Could not resolve hostname winbox: nodename nor servname provided, or not known' 'bailey@winbox: Permission denied (publickey,password,keyboard-interactive).' 'kex_exchange_identification: Connection closed by remote host'; do
  FAKE_MODE=unreachable FAKE_REASON="$reason" wh "$R" clean h1
  is "unreachable, another reason: one line, exit code 255" "$RC/$(cat "$T/err")" "255/win_host: cannot reach 'winbox' over SSH: $reason"
done
FAKE_MODE=unreachable FAKE_REASON='' wh "$R" info
is "unreachable, ssh says nothing: still one line" "$RC/$(cat "$T/err")" "255/win_host: cannot reach 'winbox' over SSH: ssh gave no reason"
FAKE_MODE=notwindows wh "$R" info
is "not Windows with cmd.exe: exit code 4" "$RC" 4
is "not Windows with cmd.exe: gave up after the probe" "$(calls)" 1
FAKE_FAIL_AT=2 FAKE_FAIL_RC=255 wh "$R" cargo h1 build
is "connection lost during the command: exit code 255" "$RC" 255
is "connection lost during the command: one line" "$(cat "$T/err")" "win_host: the SSH connection to 'winbox' failed or was lost"

# --- 4. sync --------------------------------------------------------------------

S="$T/checkout"
make_repo "$S"
: > "$T/names"
mk() { # mk NAME [CONTENT]: a file that sync must send
  mkdir -p "$(dirname "$S/$1")"
  printf '%s' "${2:-content of $1}" > "$S/$1"
  printf '%s\n' "$1" >> "$T/names"
}
for f in tools/dev/win_host.sh tools/dev/win_host_remote.ps1 tools/dev/win_host_pack.py; do printf '%s\n' "$f" >> "$T/names"; done
# Precomposed characters only: git on macOS reports names in that form.
UML="$(printf '\303\274ber-\303\244\303\266 caf\303\251')"
CJK="$(printf '\346\227\245\346\234\254\350\252\236')"
mk "plain.txt"
mk "dir/sub/deep.txt"
mk "with space.txt"
mk "dir with space/file with space.txt"
mk "it's.txt"
mk "-rf"
mk "--help"
mk "-C"
mk "dir/-leading-dash.txt"
mk "$CJK/$CJK.txt"
mk "$UML.txt"
mk "@args.txt"
mk "tab-free name (1) [x] {y} #z %OS% \$HOME !.txt"
# Legal on Windows, and syntax to cmd.exe or PowerShell: they must travel as data only.
mk "& calc.exe &.txt"
mk "\$(calc).txt"
mk "'; Remove-Item -Recurse x; '.txt"
mk "\`whoami\`.txt"
mk "a^b.txt"
mk ";x.txt"
mk "%SystemRoot%/%CD%.txt"
mk "semi;colon,comma=eq+plus~tilde.txt"
mk "@(1,2)/\$env_PATH.txt"
mk "$(printf 'emoji-\360\237\246\200.txt')"
mk "crlf.txt" "$(printf 'one\r\ntwo\r\nlone\rend')"
mk "empty.txt" ""
mk ".gitignore" "ignored.txt${NL}ignored-dir/${NL}"
mk "untracked.txt"
mk "tracked-then-changed.txt"
mk "tracked-then-deleted.txt"
python3 -c 'import sys; sys.stdout.buffer.write(bytes(range(256)) * 40)' > "$S/all-bytes.dat"
printf '%s\n' "all-bytes.dat" >> "$T/names"
python3 -c 'import os, sys; os.utime(sys.argv[1], ns=(1700000000123456789, 1700000000123456789))' "$S/plain.txt"
chmod +x "$S/with space.txt"
git -C "$S" add -A
git -C "$S" rm -q --cached untracked.txt
printf 'changed after git add' > "$S/tracked-then-changed.txt"
rm "$S/tracked-then-deleted.txt"
grep -vxF "tracked-then-deleted.txt" "$T/names" > "$T/names.tmp" && mv "$T/names.tmp" "$T/names"
echo secret > "$S/ignored.txt"
mkdir -p "$S/ignored-dir" && echo secret > "$S/ignored-dir/x.txt"

wh "$S" sync h1
is "sync: exit code" "$RC" 0
is "sync: silent (the last word is the other machine's)" "$(cat "$T/out" "$T/err")" ""
is "sync: four connections (probe, empty the tree, send, verify)" "$(calls)" 4
is "sync: empties the work tree first" "$(command_of 2)" "WhSyncPrepare 'h1'"
is "sync: unpacks with Windows' own tar into the work tree" "$(inspect args "$LOG" 3)" "$SSH_HEAD$NL%SystemRoot%\\System32\\tar.exe -xf - -C D:\\asamu\\work\\h1"
check "sync: the tar stream is exactly the files git does not ignore (names, bytes, times)" inspect tar "$LOG" 3 "$S" "$T/names"
summary="$(inspect tar "$LOG" 3 "$S" "$T/names" 2> /dev/null)"
is "sync: verify is told the number of files, the bytes and the SHA-256 of the list" "$(command_of 4)" "WhSyncVerify 'h1' $summary"
is "sync: the file count is the one expected" "${summary%% *}" "$(lines "$T/names")"

rm "$S/dir/sub/deep.txt" "$S/-rf"
grep -vxF -e "dir/sub/deep.txt" -e "-rf" "$T/names" > "$T/names.tmp" && mv "$T/names.tmp" "$T/names"
wh "$S" sync h1
is "sync after deleting files: exit code" "$RC" 0
is "sync after deleting files: the tree is emptied before the files are sent" "$(command_of 2) / $(inspect args "$LOG" 3 | tail -n 1)" "WhSyncPrepare 'h1' / %SystemRoot%\\System32\\tar.exe -xf - -C D:\\asamu\\work\\h1"
check "sync after deleting files: they are not sent again" inspect tar "$LOG" 3 "$S" "$T/names"

mv "$S" "$T/default-name_1.x"
S="$T/default-name_1.x"
wh "$S" sync
is "sync without NAME: the folder's name" "$(command_of 2)" "WhSyncPrepare 'default-name_1.x'"

wh "$S" sync --dry-run h1
is "dry run: exit code" "$RC" 0
is "dry run: nothing contacted" "$(calls)" 0
is "dry run: lists what would be sent" "$(LC_ALL=C sort "$T/out")" "$(LC_ALL=C sort "$T/names")"
has "dry run: says so" "$T/err" "would send $(lines "$T/names") files"
ASAMU_WIN_HOST='' wh "$S" sync -n
is "dry run: needs no host" "$RC" 0
wh "$S" sync --force h1
is "sync: unknown option refused" "$RC" 2
wh "$S" sync h1 h2
is "sync: one NAME only" "$RC" 2
is "sync: one NAME only, nothing contacted" "$(calls)" 0

FAKE_FAIL_AT=2 FAKE_FAIL_RC=4 wh "$S" sync h1
is "sync: stops when the tree cannot be emptied" "$RC/$(calls)" "4/2"
FAKE_FAIL_AT=3 FAKE_FAIL_RC=1 wh "$S" sync h1
is "sync: a failed transfer is exit code 4, and is not verified" "$RC/$(calls)" "4/3"
has "sync: a failed transfer says the tree is incomplete" "$T/err" "work tree h1 is incomplete"
FAKE_FAIL_AT=4 FAKE_FAIL_RC=4 wh "$S" sync h1
is "sync: a failed verification is exit code 4" "$RC" 4
FAKE_FAIL_AT=3 FAKE_FAIL_RC=255 wh "$S" sync h1
is "sync: connection lost while sending is exit code 255" "$RC" 255

# A rename that changes only upper and lower case. Where the file system
# ignores case git goes on listing the old name; the name on disk is what must
# be sent. (Where case matters git lists the new name itself.)
printf 'renamed' > "$S/CaseName.txt"
mkdir -p "$S/CaseDir/Sub" && printf 'in a renamed folder' > "$S/CaseDir/Sub/file.txt"
git -C "$S" add CaseName.txt CaseDir
mv "$S/CaseName.txt" "$S/casename.tmp" && mv "$S/casename.tmp" "$S/CASENAME.txt"
mv "$S/CaseDir" "$S/casedir.tmp" && mv "$S/casedir.tmp" "$S/casedir"
printf '%s\n' "CASENAME.txt" "casedir/Sub/file.txt" >> "$T/names"
wh "$S" sync h1
is "case-only rename: exit code" "$RC" 0
check "case-only rename: the names on disk are sent, the old ones are not" inspect tar "$LOG" 3 "$S" "$T/names"
wh "$S" sync --dry-run h1
is "case-only rename: the dry run lists the names on disk" "$(grep -i -e '^casename' -e '^casedir' "$T/out" | LC_ALL=C sort | tr '\n' ' ')" "CASENAME.txt casedir/Sub/file.txt "
if git -C "$S" ls-files | grep -qxF "CaseName.txt" && [ -e "$S/CaseName.txt" ]; then
  has "case-only rename: says that the name differs from git's" "$T/err" "sending 'CASENAME.txt' under the name it has on disk (git lists it as 'CaseName.txt')"
else
  echo "skip  case-only rename: says that the name differs from git's (this file system is case sensitive: git lists the new name)"
fi

# A tracked file replaced by a folder, and a tracked folder replaced by a file:
# what is on disk is sent, and what git still lists of the old shape is not.
printf 'a file' > "$S/file-then-folder" && mkdir -p "$S/folder-then-file" && printf 'in it' > "$S/folder-then-file/inside.txt"
git -C "$S" add file-then-folder folder-then-file
rm "$S/file-then-folder" && mkdir "$S/file-then-folder" && printf 'now in a folder' > "$S/file-then-folder/new.txt"
rm -r "$S/folder-then-file" && printf 'now a file' > "$S/folder-then-file"
printf '%s\n' "file-then-folder/new.txt" "folder-then-file" >> "$T/names"
wh "$S" sync h1
is "file and folder swapped: exit code" "$RC" 0
check "file and folder swapped: what is on disk is sent" inspect tar "$LOG" 3 "$S" "$T/names"
hasnt "file and folder swapped: nothing is reported as left out" "$T/err" "left out"

# The packer on its own, with lists git would not produce or that this file
# system could not hold: it must refuse all of them (exit code 3), on every system.
pack_check() { # pack_check DESCRIPTION WORD: the list in $T/list must be refused with WORD in the message
  rm -f "$T/manifest.out"
  python3 -B "$S/tools/dev/win_host_pack.py" check "$S" "$T/list" "$T/manifest.out" > "$T/out" 2> "$T/err"
  RC=$?
  if [ "$RC" = 3 ] && [ ! -s "$T/out" ] && [ ! -e "$T/manifest.out" ] && grep -qF -- "$2" "$T/err"; then ok "packer refuses $1"; else bad "packer refuses $1" "exit $RC, output '$(cat "$T/out")', message:${NL}$(cat "$T/err")"; fi
}
printf 'plain.txt\0bad-\377-utf8.txt\0' > "$T/list"
pack_check 'a name that is not UTF-8' 'not valid UTF-8'
for entry in '../outside.txt' 'dir/../../outside.txt' '/etc/hosts' 'dir//deep.txt' '..' 'dir/..'; do
  printf 'plain.txt\0%s\0' "$entry" > "$T/list"
  pack_check "the path $entry" 'not a path inside the checkout'
done
printf 'plain.txt\0' > "$T/list"
python3 -B "$S/tools/dev/win_host_pack.py" check "$S" "$T/list" "$T/manifest.out" > "$T/out" 2> "$T/err"
is "packer accepts a plain list" "$?/$(cut -d' ' -f1 "$T/out")/$(cat "$T/manifest.out")" "0/1/plain.txt"
is "packer: paths that differ only in case are found (files, and folders on the way)" "$(python3 -B -c '
import sys
sys.path.insert(0, sys.argv[1])
import win_host_pack
print(win_host_pack.case_clashes(["a/b.txt", "A/c.txt", "x", "X", "y/z", "y/Z", "q", "src/lib.rs", "src/main.rs"]))' "$S/tools/dev")" "[('a', 'A'), ('x', 'X'), ('y/z', 'y/Z')]"

# Files that cannot be mirrored: refused before anything is contacted.
refuse() { # refuse DESCRIPTION WORD-IN-MESSAGE: the checkout in $S must be refused
  wh "$S" sync h1
  if [ "$RC" = 3 ] && [ "$(calls)" = 0 ] && grep -qF -- "$2" "$T/err"; then ok "sync refuses $1"; else bad "sync refuses $1" "exit $RC, $(calls) ssh calls, message:${NL}$(cat "$T/err")"; fi
  wh "$S" sync --dry-run h1
  is "dry run refuses $1" "$RC" 3
}
try_name() { # try_name DESCRIPTION NAME WORD: a file of that name makes sync refuse
  mkdir -p "$(dirname "$S/$2")"
  if ! (printf x > "$S/$2") 2> /dev/null; then
    echo "skip  sync refuses $1 (this file system cannot hold such a name)"
    return
  fi
  refuse "$1" "$3"
  rm -f "$S/$2"
  rmdir "$(dirname "$S/$2")" 2> /dev/null
}
try_name 'a name with a double quote' 'say "cheese".txt' 'which Windows does not allow'
try_name 'a name with a backslash' 'back\slash.txt' 'which Windows does not allow'
try_name 'a name with a colon' 'a:b.txt' 'which Windows does not allow'
try_name 'a name with < > | ? *' 'what?.txt' 'which Windows does not allow'
try_name 'a name with a line break' "$(printf 'two\nlines.txt')" 'control character'
try_name 'a name with a tab' "$(printf 'tab\there.txt')" 'control character'
try_name 'a name that ends with a dot' 'ends-with-dot.' 'ends with a space or a dot'
try_name 'a folder name that ends with a space' 'folder /x.txt' 'ends with a space or a dot'
try_name 'a device name' 'sub/NUL.txt' 'reserves for a device'
try_name 'a device name without extension' 'aux' 'reserves for a device'
try_name 'the name of the sync folder' '.win_host_incomplete/000001' 'used by win_host.sh itself'
try_name 'the name of the sync folder in other case' '.WIN_HOST_incomplete' 'used by win_host.sh itself'
try_name 'a path longer than 200 characters' "$(printf 'long/%0180d/%030d.txt' 0 0)" 'longer than 200'
if (printf x > "$S/$(printf 'bad-\377-utf8.txt')") 2> /dev/null; then
  refuse 'a name that is not UTF-8' 'not valid UTF-8'
  rm -f "$S/$(printf 'bad-\377-utf8.txt')"
else
  echo "skip  sync refuses a name that is not UTF-8 (this file system cannot hold such a name)"
fi
printf x > "$S/CaseProbe"
if [ -e "$S/caseprobe" ]; then
  echo "skip  sync refuses names that differ only in case (this file system is not case sensitive)"
  rm -f "$S/CaseProbe"
else
  printf y > "$S/caseprobe"
  refuse 'two files that differ only in case' 'differ only in case'
  rm -f "$S/CaseProbe" "$S/caseprobe"
  mkdir -p "$S/Folder" "$S/folder" && printf x > "$S/Folder/a.txt" && printf y > "$S/folder/b.txt"
  refuse 'two folders that differ only in case' 'differ only in case'
  rm -rf "$S/Folder" "$S/folder"
fi

printf 'no permission to read' > "$S/unreadable.txt" && chmod 000 "$S/unreadable.txt"
if [ -r "$S/unreadable.txt" ]; then
  echo "skip  sync refuses a file that cannot be read (this user can read every file)"
else
  refuse 'a file that cannot be read' 'cannot be read (no permission)'
fi
rm -f "$S/unreadable.txt"

echo "outside the checkout" > "$T/outside.txt"
ln -s "$T/outside.txt" "$S/link-to-outside"
refuse 'a symbolic link to a file' 'symbolic link'
hasnt "a refused sync does not name a file as sent" "$T/out" "outside"
rm "$S/link-to-outside"
ln -s "does-not-exist" "$S/dangling-link"
refuse 'a dangling symbolic link' 'symbolic link'
rm "$S/dangling-link"
mkdir "$T/outside-dir" && echo x > "$T/outside-dir/x.txt"
ln -s "$T/outside-dir" "$S/link-dir"
refuse 'a symbolic link to a folder' 'symbolic link'
rm "$S/link-dir"
# A tracked folder replaced by a link: git still lists the files below it.
mkdir "$S/realdir" && echo x > "$S/realdir/x.txt" && git -C "$S" add realdir
mv "$S/realdir" "$T/realdir-moved" && ln -s "$T/realdir-moved" "$S/realdir"
refuse 'files reached through a linked folder' 'symbolic link'
rm "$S/realdir" && git -C "$S" rm -q --cached realdir/x.txt
# The same when git is told to ignore the link itself: only the file below it is listed.
echo "/ignored-link" >> "$S/.git/info/exclude"
mkdir "$S/ignored-link" && echo x > "$S/ignored-link/x.txt" && git -C "$S" add -f ignored-link/x.txt
rm -r "$S/ignored-link" && ln -s "$T/realdir-moved" "$S/ignored-link"
refuse 'tracked files below an ignored link' 'reached through the symbolic link'
rm "$S/ignored-link" && git -C "$S" rm -q --cached ignored-link/x.txt
wh "$S" sync h1
is "sync works again once the offending files are gone" "$RC/$(calls)" "0/4"

# A repository of its own inside the checkout is not part of it.
git -c init.defaultBranch=main init -q "$S/nested" && echo x > "$S/nested/inner.txt"
wh "$S" sync h1
is "nested repository: sync goes on" "$RC" 0
has "nested repository: left out, and said so" "$T/err" "left out 'nested/'"
check "nested repository: its files are not sent" inspect tar "$LOG" 3 "$S" "$T/names"
rm -rf "$S/nested"

# Nothing to send: never replace a work tree with an empty one.
E="$T/empty"
make_repo "$E"
echo '*' > "$E/.gitignore"
wh "$E" sync h1
is "empty checkout: exit code 3" "$RC" 3
is "empty checkout: nothing contacted" "$(calls)" 0
has "empty checkout: says why" "$T/err" "no files to send"
N="$T/not-a-checkout"
mkdir -p "$N/tools/dev" && cp "$HERE/win_host.sh" "$HERE/win_host_remote.ps1" "$HERE/win_host_pack.py" "$N/tools/dev/"
# Above $T there may be a repository (the scratch folder can be inside one); stop git from finding it.
GIT_CEILING_DIRECTORIES="$T" wh "$N" sync h1
is "not a git checkout: exit code 3" "$RC" 3
is "not a git checkout: nothing contacted" "$(calls)" 0
I="$T/outer"
make_repo "$I"
mkdir -p "$I/inner/tools/dev" && cp "$HERE/win_host.sh" "$HERE/win_host_remote.ps1" "$HERE/win_host_pack.py" "$I/inner/tools/dev/"
git -C "$I" add -A
wh "$I/inner" sync h1
is "script not at the top of its checkout: exit code 3" "$RC" 3
is "script not at the top of its checkout: nothing contacted" "$(calls)" 0
mv "$S/tools/dev/win_host_pack.py" "$T/pack.away"
wh "$S" sync h1
is "missing helper file: refused before anything is contacted" "$RC/$(calls)" "2/0"
mv "$T/pack.away" "$S/tools/dev/win_host_pack.py"

# --- 5. the audit ----------------------------------------------------------------

audited="$(inspect audit "$AUDIT" "$HERE/win_host_remote.ps1" 2>&1 > "$T/violations")"
is "audit: every call of ssh and scp in this run had one of the expected forms ($audited calls)" "$(cat "$T/violations")" ""
case "$audited" in
  '' | *[!0123456789]* | ? | ??) bad "audit: looked at the calls" "saw '$audited'" ;;
  *) ok "audit: looked at the calls" ;;
esac

echo
echo "test_win_host: $passed passed, $failed failed"
[ "$failed" -eq 0 ]
