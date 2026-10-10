#!/usr/bin/env bash
# Build, test and run this checkout on a second machine that runs Windows,
# over SSH. For when the development machine is short of disk or is not the
# platform under test. docs/BUILDING.md ("Building on a second machine") has
# the setup and the rules; tools/dev/test_win_host.sh tests this script
# without a Windows machine.
#
#   ASAMU_WIN_HOST=<ssh host> tools/dev/win_host.sh <command> [args]
#
#   sync [--dry-run] [NAME]  mirror this checkout to <root>\work\NAME (default NAME: this folder's
#                            name): the files git does not ignore, tracked or not, as they are on
#                            disk, with their modification times. The tree there is replaced as a
#                            whole, so a file deleted here disappears there, and it is usable only
#                            after names and sizes were compared. A checkout with a symbolic link
#                            or a file name Windows cannot hold is refused as a whole. --dry-run
#                            lists what would be sent and contacts nothing. Needs git and python3.
#   cargo NAME ARGS...       run `cargo ARGS` in that work tree, target folder <root>\target-NAME;
#                            output is streamed, the exit code is cargo's
#   exec NAME PROGRAM [ARGS...]  run a program in that work tree (arguments are passed exactly:
#                            no shell syntax)
#   gate NAME [STEP...]      the project's gate there: fmt (cargo fmt --all --check), clippy
#                            (cargo clippy --workspace --all-targets --locked -- -D warnings), test
#                            (cargo test --workspace --locked), build (cargo build --workspace
#                            --locked). One summary line per step, the whole output of each step in
#                            <root>\logs\NAME\<step>.log, exit code 1 if a step failed. All steps
#                            run even after a failure; naming steps runs only those.
#   ps [NAME]                run a PowerShell script read from standard input (in the work tree,
#                            as $work, when NAME is given; $root is the root folder)
#   fetch REMOTE LOCAL       copy one file back (REMOTE is relative to <root>, with / separators)
#   clean NAME               remove <root>\work\NAME and <root>\target-NAME, nothing else, and say
#                            how much that freed
#   disk                     free space, and the size of every work tree and target folder
#   info                     toolchain, free space and the work trees that exist
#
# NAME is 1 to 64 letters, digits, dot, dash or underscore, starting with a
# letter or digit. sync, gate and clean write and delete nothing on the other
# machine outside <root>\work\NAME, <root>\target-NAME and <root>\logs\NAME
# (what cargo, exec and ps do there is up to what you run).
#
# Environment:
#   ASAMU_WIN_HOST   SSH host name or alias (required; key authentication, no prompts)
#   ASAMU_WIN_ROOT   folder there for work trees and build output (default D:\asamu)
#   ASAMU_WIN_ENV    extra environment for cargo/exec/gate/ps NAME, as PowerShell text,
#                    e.g. '$env:ASAMU_CONVERTED_DIR="D:\asamu\converted"'
#
# Exit codes: 0 success; for cargo, exec and ps the code of what was run (a
# program's code outside 0..254 is reported as 254); 1 a gate step failed, or
# a ps script stopped with an error; 2 bad usage, name or root; 3 the checkout
# cannot be mirrored as it is (symbolic links, names Windows cannot hold,
# nothing to send): nothing was changed; 4 the other machine refused or failed
# (no such work tree, incomplete sync, folder in use); 127 exec: program not
# found there; 255 the machine could not be reached over SSH, or the
# connection was lost (what was running there may still be running).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
HOST="${ASAMU_WIN_HOST:-}"
ROOT="${ASAMU_WIN_ROOT:-D:\\asamu}"
REMOTE_LIB="$HERE/win_host_remote.ps1"
PACKER="$HERE/win_host_pack.py"
# No prompts, no terminal; a dead connection is noticed after two minutes.
SSH_OPTS=(-T -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=8)

EX_USAGE=2
EX_REFUSED=3
EX_REMOTE=4
EX_UNREACHABLE=255

LETTERS='abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ'
DIGITS='0123456789'
NL=$'\n'

TMP=""
cleanup() {
  if [ -n "$TMP" ]; then rm -rf -- "$TMP"; fi
}
trap cleanup EXIT

usage() {
  awk 'NR > 1 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
  exit "${1:-$EX_USAGE}"
}

# die CODE MESSAGE
die() {
  echo "win_host: $2" >&2
  exit "$1"
}

# A work-tree name becomes a folder name on the other machine and part of
# command lines there. The character lists are spelled out (no ranges) so that
# the check does not depend on the locale.
check_name() {
  local n="$1" lower
  if [ -z "$n" ] || [ "${#n}" -gt 64 ] || [ -n "${n//[$LETTERS$DIGITS._-]/}" ]; then
    die "$EX_USAGE" "bad work tree name $(printf '%q' "$n"): use 1 to 64 letters, digits, . _ -"
  fi
  case "$n" in
    [._-]* | *. | *..*) die "$EX_USAGE" "bad work tree name '$n': it must start with a letter or digit, not end with a dot and not contain '..'" ;;
  esac
  lower="$(printf '%s' "$n" | tr 'ABCDEFGHIJKLMNOPQRSTUVWXYZ' 'abcdefghijklmnopqrstuvwxyz')"
  case "${lower%%.*}" in
    con | prn | aux | nul | com[0123456789] | lpt[0123456789]) die "$EX_USAGE" "bad work tree name '$n': Windows reserves that name for a device" ;;
  esac
}

# The root folder: a drive letter and one or more folder names of safe
# characters, e.g. D:\asamu. It is put into command lines on the other machine
# unquoted, and everything this script deletes there is below it.
check_root() {
  local drive="${ROOT%%:*}" rest="${ROOT#?:}"
  if [ "${#ROOT}" -gt 100 ] || [ "${#drive}" -ne 1 ] || [ -n "${drive//[$LETTERS]/}" ] ||
    [ "$rest" = "$ROOT" ] || [ -n "${rest//[$LETTERS$DIGITS._\\-]/}" ]; then
    die "$EX_USAGE" "bad ASAMU_WIN_ROOT $(printf '%q' "$ROOT"): give a folder on a drive, like D:\\asamu, made of letters, digits, . _ -"
  fi
  case "$rest" in
    '\'*) ;;
    *) die "$EX_USAGE" "bad ASAMU_WIN_ROOT '$ROOT': give a folder on a drive, like D:\\asamu" ;;
  esac
  case "$rest\\" in
    *'\\'* | *'.\'*) die "$EX_USAGE" "bad ASAMU_WIN_ROOT '$ROOT': it must name a folder (not a drive), without empty names or names that end with a dot" ;;
  esac
}

# The host is an argument of ssh and scp: it must not look like an option or a path.
check_host() {
  [ -n "$HOST" ] || die "$EX_USAGE" "set ASAMU_WIN_HOST to the SSH host of the Windows machine"
  if [ -n "${HOST//[$LETTERS$DIGITS._@-]/}" ]; then
    die "$EX_USAGE" "bad ASAMU_WIN_HOST $(printf '%q' "$HOST"): use an SSH host name or alias (letters, digits, . _ - @)"
  fi
  case "$HOST" in
    [.@-]*) die "$EX_USAGE" "bad ASAMU_WIN_HOST '$HOST': it must start with a letter, digit or underscore" ;;
  esac
}

need() {
  command -v "$1" > /dev/null 2>&1 || die "$EX_USAGE" "$1 is needed on this machine for \`$2\` and was not found"
}

lost() {
  die "$EX_UNREACHABLE" "the SSH connection to '$HOST' failed or was lost"
}

# One round trip that proves the machine answers and is what this script
# expects: Windows, with cmd.exe as the SSH default shell (which expands %OS%).
# An unreachable machine is reported in one line, with ssh's own reason.
probe() {
  local out rc=0 reason
  out="$(ssh "${SSH_OPTS[@]}" "$HOST" 'echo asamu-win-host %OS%' 2>&1 < /dev/null)" || rc=$?
  if [ "$rc" -eq 255 ]; then
    reason="$(printf '%s\n' "$out" | tr -d '\r' | awk 'NF { last = $0 } END { print last }')"
    die "$EX_UNREACHABLE" "cannot reach '$HOST' over SSH: ${reason:-ssh gave no reason}"
  fi
  case "$out" in
    *'asamu-win-host Windows_NT'*) ;;
    *) die "$EX_REMOTE" "'$HOST' does not answer like a Windows machine whose SSH default shell is cmd.exe (ssh exit $rc)" ;;
  esac
}

# PowerShell shows a script's own warnings and non-terminating errors on
# standard error as "CLIXML" when it is started the way ps_run starts it.
# Turn those records back into plain lines; pass every other line through.
#
# swap FROM TO replaces every FROM in S by TO, both taken literally (bash
# versions differ in how ${S//FROM/TO} reads quotes and & in TO).
S=""
swap() {
  local from="$1" to="$2" out=""
  while [ "$S" != "${S#*"$from"}" ]; do
    out+="${S%%"$from"*}$to"
    S="${S#*"$from"}"
  done
  S="$out$S"
}

stderr_filter() {
  local line kind rest open='<S S="'
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"
    case "$line" in
      '#< CLIXML') ;;
      '<Objs '*)
        S="${line#*>}"
        S="${S%</Objs>}"
        while [ "$S" != "${S#*"$open"}" ]; do
          rest="${S#*"$open"}"
          kind="${rest%%\"*}"
          rest="${rest#*>}"
          if [ "$kind" = warning ]; then rest="WARNING: $rest"; fi
          S="${S%%"$open"*}$rest"
        done
        swap '</S>' "$NL"
        swap '_x000D__x000A_' ''
        swap '_x000A_' ''
        swap '&lt;' '<'
        swap '&gt;' '>'
        swap '&quot;' '"'
        swap '&apos;' "'"
        swap '&amp;' '&'
        printf '%s' "$S" >&2
        ;;
      *) printf '%s\n' "$line" >&2 ;;
    esac
  done
}

# What starts PowerShell on the other machine. The text is fixed and short
# (a command line there is limited to about 8000 characters) and travels as
# -EncodedCommand, so no shell on the way can change it. It reads the real
# script from standard input, runs it, and turns an error into one plain line
# and exit code 1.
#
# The script arrives as: a line with the number of base64 characters, the
# base64 text in lines, and a line with a single dot. The dot matters:
# PowerShell behind the Windows SSH server often never sees the end of its
# standard input (measured: ReadToEnd on 40 KB waited forever in 9 of 18 runs;
# a native program always saw the end), so nothing here waits for the end of
# input. The count makes sure a script that arrived cut short is never run.
BOOTSTRAP='$ProgressPreference = "SilentlyContinue"; $ErrorActionPreference = "Stop"; try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false } catch { }; $n = [Console]::In.ReadLine(); $b = New-Object System.Text.StringBuilder; while ($true) { $l = [Console]::In.ReadLine(); if ($null -eq $l -or $l -eq ".") { break }; [void]$b.Append($l) }; if ($l -ne "." -or [string]$b.Length -ne $n) { [Console]::Error.WriteLine("win_host: the script arrived incomplete on the other machine (" + $b.Length + " of " + $n + " characters): nothing was run"); exit 4 }; $function:WinHost = [scriptblock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b.ToString()))); try { WinHost } catch { [Console]::Error.WriteLine("win_host: error on the other machine: " + $_.Exception.Message); if ($_.InvocationInfo) { [Console]::Error.WriteLine($_.InvocationInfo.PositionMessage) }; exit 1 }; exit 0'

# Run PowerShell text ($1) on the other machine after the shared functions of
# win_host_remote.ps1. Standard output and error are streamed; the exit code is
# left in RC.
RC=0
ps_run() {
  local boot text payload
  [ -r "$REMOTE_LIB" ] || die "$EX_USAGE" "$REMOTE_LIB is missing (it belongs next to this script)"
  boot="$(printf '%s' "$BOOTSTRAP" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\r\n')"
  text="$({
    printf "\$root = '%s'\n" "$ROOT"
    cat "$REMOTE_LIB"
    printf '\n%s\n' "$1"
  } | base64 | tr -d '\r\n')"
  # Measured on Windows 11's SSH server: input for PowerShell stops arriving
  # somewhere after the first 270 KB (10 of 10 runs with 4 MB or more; never
  # before 270 KB), while a native program such as tar reads 40 MB without
  # trouble. What this script sends itself is about 30 KB; only a `ps` script
  # can be larger.
  [ "${#text}" -le 150000 ] || die "$EX_USAGE" "the PowerShell script is too large to send reliably ($((${#text} * 3 / 4)) bytes with the shared functions; the limit is about 90 KB): put it in the checkout and run it there"
  payload="${#text}$NL$(printf '%s\n' "$text" | fold -w 76)$NL."
  set +e
  {
    ssh "${SSH_OPTS[@]}" "$HOST" "powershell -NoProfile -NonInteractive -EncodedCommand $boot" 2>&1 1>&3 3>&- <<< "$payload" | stderr_filter 3>&-
    RC="${PIPESTATUS[0]}"
  } 3>&1
  set -e
  [ "$RC" -ne "$EX_UNREACHABLE" ] || lost
}

# Text as a PowerShell expression that cannot be misread: base64 of its UTF-8
# bytes, decoded there by WhText.
ps_text() {
  printf "(WhText '%s')" "$(printf '%s' "$1" | base64 | tr -d '\r\n')"
}

# Arguments as a PowerShell array of such expressions.
ps_array() {
  local out="@(" sep="" a
  for a in "$@"; do
    out+="$sep$(ps_text "$a")"
    sep=", "
  done
  printf '%s)' "$out"
}

# PowerShell that enters the work tree NAME and sets the build environment,
# in ENTER (a variable, because $(...) would drop the line break at its end).
ENTER=""
enter() {
  ENTER="\$work = WhEnter '$1'$NL${ASAMU_WIN_ENV:-}$NL"
}

# The files of this checkout that sync sends: names in $TMP/manifest, and
# COUNT, BYTES and DIGEST (the SHA-256 of the manifest). Exits with 3 when the
# checkout cannot be mirrored as it is.
COUNT=0
BYTES=0
DIGEST=""
collect() {
  local prefix summary
  need git sync
  need python3 sync
  [ -r "$PACKER" ] || die "$EX_USAGE" "$PACKER is missing (it belongs next to this script)"
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/win_host.XXXXXX")"
  # The checkout this script is in, whatever git environment the caller has.
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
  if ! prefix="$(git -C "$REPO" rev-parse --show-prefix 2> "$TMP/git.err")"; then
    die "$EX_REFUSED" "$REPO is not a git checkout: $(head -n 1 "$TMP/git.err")"
  fi
  [ -z "$prefix" ] || die "$EX_REFUSED" "$REPO is not the top folder of a git checkout (this script belongs in <checkout>/tools/dev)"
  if ! git -C "$REPO" ls-files -co --exclude-standard -z > "$TMP/list" 2> "$TMP/git.err"; then
    die "$EX_REFUSED" "cannot list the files of $REPO: $(head -n 1 "$TMP/git.err")"
  fi
  summary="$(python3 "$PACKER" check "$REPO" "$TMP/list" "$TMP/manifest")" || exit $?
  read -r COUNT BYTES DIGEST <<< "$summary"
  case "$COUNT$BYTES" in
    '' | *[!0123456789]*) die "$EX_USAGE" "internal error: unexpected answer from $PACKER: $summary" ;;
  esac
  if [ "${#DIGEST}" -ne 64 ] || [ -n "${DIGEST//[0123456789abcdef]/}" ]; then
    die "$EX_USAGE" "internal error: unexpected answer from $PACKER: $summary"
  fi
}

cmd_sync() {
  local dry=0 name="" named=0 status
  while [ $# -gt 0 ]; do
    case "$1" in
      -n | --dry-run) dry=1 ;;
      -?*) die "$EX_USAGE" "sync: unknown option $(printf '%q' "$1")" ;;
      *)
        [ "$named" -eq 0 ] || die "$EX_USAGE" "usage: sync [--dry-run] [NAME]"
        named=1
        name="$1"
        ;;
    esac
    shift
  done
  [ "$named" -eq 1 ] || name="$(basename "$REPO")"
  check_name "$name"
  if [ "$dry" -eq 1 ]; then
    collect
    cat "$TMP/manifest"
    echo "win_host: would send $COUNT files ($BYTES bytes) to $ROOT\\work\\$name${HOST:+ on $HOST}; nothing was contacted" >&2
    return 0
  fi
  check_host
  collect
  probe
  ps_run "WhSyncPrepare '$name'"
  [ "$RC" -eq 0 ] || exit "$RC"
  # Windows' own tar (bsdtar); the path needs no quoting, see check_root and check_name.
  set +e
  python3 "$PACKER" tar "$REPO" "$TMP/manifest" |
    ssh "${SSH_OPTS[@]}" "$HOST" "%SystemRoot%\\System32\\tar.exe -xf - -C $ROOT\\work\\$name"
  status=("${PIPESTATUS[@]}")
  set -e
  [ "${status[1]}" -ne "$EX_UNREACHABLE" ] || lost
  if [ "${status[0]}" -ne 0 ] || [ "${status[1]}" -ne 0 ]; then
    die "$EX_REMOTE" "sending the files failed (packing: exit ${status[0]}, unpacking on $HOST: exit ${status[1]}); work tree $name is incomplete, run sync again"
  fi
  ps_run "WhSyncVerify '$name' $COUNT $BYTES '$DIGEST'"
  [ "$RC" -eq 0 ] || exit "$RC"
}

cmd_gate() {
  local name step steps=()
  [ $# -ge 1 ] || die "$EX_USAGE" "usage: gate NAME [fmt] [clippy] [test] [build]"
  name="$1"
  shift
  check_name "$name"
  for step in "$@"; do
    case "$step" in
      fmt | clippy | test | build) steps+=("'$step'") ;;
      *) die "$EX_USAGE" "gate: unknown step $(printf '%q' "$step") (the steps are fmt, clippy, test, build)" ;;
    esac
  done
  probe
  enter "$name"
  # ${steps[*]-}: an empty array is "unset" to bash 3.2 under `set -u`.
  ps_run "${ENTER}WhGate '$name' @($(
    IFS=,
    printf '%s' "${steps[*]-}"
  ))"
  exit "$RC"
}

[ $# -ge 1 ] || usage
cmd="$1"
shift
case "$cmd" in
  -h | --help | help) usage 0 ;;
esac
check_root

case "$cmd" in
  sync)
    cmd_sync "$@"
    ;;
  cargo)
    [ $# -ge 2 ] || die "$EX_USAGE" "usage: cargo NAME ARGS..."
    check_host
    name="$1"
    shift
    check_name "$name"
    probe
    enter "$name"
    ps_run "${ENTER}WhRun 'cargo' $(ps_array "$@")"
    exit "$RC"
    ;;
  exec)
    [ $# -ge 2 ] || die "$EX_USAGE" "usage: exec NAME PROGRAM [ARGS...]"
    check_host
    name="$1"
    program="$2"
    shift 2
    check_name "$name"
    [ -n "$program" ] || die "$EX_USAGE" "exec: the program name is empty"
    probe
    enter "$name"
    ps_run "${ENTER}WhRun $(ps_text "$program") $(ps_array "$@")"
    exit "$RC"
    ;;
  gate)
    check_host
    cmd_gate "$@"
    ;;
  ps)
    [ $# -le 1 ] || die "$EX_USAGE" "usage: ps [NAME]  (the script is read from standard input)"
    check_host
    if [ $# -eq 1 ]; then
      check_name "$1"
      enter "$1"
    fi
    script="$(cat)"
    probe
    # The script runs as a block of its own, in the same scope, so that the
    # line numbers in its error messages are its own. Like PowerShell itself:
    # without an `exit`, the code is 1 when the last statement failed.
    ps_run "$ENTER. ([scriptblock]::Create($(ps_text "$script$NL"'if (-not $?) { exit 1 }')))"
    exit "$RC"
    ;;
  fetch)
    [ $# -eq 2 ] || die "$EX_USAGE" "usage: fetch REMOTE LOCAL"
    check_host
    remote="$1"
    local_file="$2"
    flat="${remote//\//}"
    if [ -z "$remote" ] || [ -n "${flat//[$LETTERS$DIGITS._-]/}" ]; then
      die "$EX_USAGE" "fetch: REMOTE must be a path below $ROOT made of letters, digits, . _ - and / (use \`ps\` for other names)"
    fi
    case "/$remote/" in
      *//* | */../* | */./* | */-*) die "$EX_USAGE" "fetch: REMOTE must be a path below $ROOT, like logs/NAME/test.log" ;;
    esac
    [ -n "$local_file" ] || die "$EX_USAGE" "fetch: LOCAL is empty"
    case "$local_file" in
      /*) ;;
      *) local_file="./$local_file" ;; # never an option, never "host:path"
    esac
    need scp fetch
    probe
    rc=0
    scp -q -o BatchMode=yes -o ConnectTimeout=10 "$HOST:$(printf '%s' "$ROOT" | tr '\\' '/')/$remote" "$local_file" || rc=$?
    [ "$rc" -eq 0 ] || die "$EX_REMOTE" "could not fetch $ROOT\\$(printf '%s' "$remote" | tr '/' '\\') from $HOST (scp exit $rc)"
    ;;
  clean)
    [ $# -eq 1 ] || die "$EX_USAGE" "usage: clean NAME"
    check_host
    check_name "$1"
    probe
    ps_run "WhClean '$1'"
    exit "$RC"
    ;;
  disk)
    [ $# -eq 0 ] || die "$EX_USAGE" "usage: disk"
    check_host
    probe
    ps_run "WhDisk"
    exit "$RC"
    ;;
  info)
    [ $# -eq 0 ] || die "$EX_USAGE" "usage: info"
    check_host
    probe
    ps_run "WhInfo"
    exit "$RC"
    ;;
  *)
    echo "win_host: unknown command $(printf '%q' "$cmd")" >&2
    usage
    ;;
esac
