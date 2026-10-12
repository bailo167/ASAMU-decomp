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
#                            (cargo test --workspace --locked --no-fail-fast), build (cargo build
#                            --workspace --locked). One summary line per step, the whole output of
#                            each step in <root>\logs\NAME\<step>.log, exit code 1 if a step
#                            failed. All steps run even after a failure; naming steps runs only those.
#   ps [NAME]                run a PowerShell script read from standard input (in the work tree,
#                            as $work, when NAME is given; $root is the root folder)
#   fetch REMOTE LOCAL       copy one file back (REMOTE is relative to <root>, with / separators)
#   clean NAME               remove <root>\work\NAME and <root>\target-NAME, nothing else (the logs
#                            stay), and say how much that freed
#   disk                     free space, and the size of every work tree and target folder
#   info                     toolchain, free space and the work trees that exist
#
# NAME is 1 to 64 letters, digits, dot, dash or underscore, starting with a
# letter or digit. sync, gate and clean write and delete nothing on the other
# machine outside <root>\work\NAME, <root>\target-NAME and <root>\logs\NAME
# (what cargo, exec and ps do there is up to what you run).
#
# On that machine's screen (a program started over SSH cannot show a window;
# these go through tools/dev/desk_runner.py, which the owner of the machine
# starts by double-clicking <root>\start-runner.cmd, once per sign-in):
#   desk-install           put the desk runner and start-runner.cmd in <root>
#   desk NAME PROG ARGS... run PROG in the desktop session, in work tree NAME: waits, streams the
#                          output, the exit code is the program's (124 time limit, 125 runner not
#                          running or job refused, 126 could not start, 130 cancelled; a code above
#                          255 arrives as its low eight bits, and never as 0). Ctrl+C, or ending
#                          this command in another way, ends the program there. PROG is
#                          <root>\target-NAME\release\PROG.exe, or a path below <root>\target-NAME,
#                          or an absolute path below <root>; the runner starts nothing else
#   desk-status            is the desk runner alive (heartbeat age, jobs)
#   desk-stop              stop the desk runner; its jobs end with it
#   desk-cancel ID         end one desk job
#
# Environment:
#   ASAMU_WIN_HOST   SSH host name or alias (required; key authentication, no prompts)
#   ASAMU_WIN_ROOT   folder there for work trees and build output (default D:\asamu when the
#                    variable is not set; an empty value is refused, like a drive's top folder)
#   ASAMU_WIN_ENV    extra environment for cargo/exec/gate/ps NAME, as PowerShell text,
#                    e.g. '$env:ASAMU_CONVERTED_DIR="D:\asamu\converted"'
#   ASAMU_DESK_TIMEOUT  seconds after which a desk job and everything it started is ended (default 900)
#   ASAMU_DESK_ENV   environment for a desk job: NAME=VALUE pairs separated by ';' (only ASAMU_*, WGPU_*,
#                    BEVY_*, RUST_BACKTRACE, RUST_LOG), e.g. 'ASAMU_CONVERTED_DIR=D:\asamu\converted'
#
# Exit codes: 0 success; for cargo, exec and ps the code of what was run (a
# program's code outside 0..254 is reported as 254); 1 a gate step failed, or
# a ps script stopped with an error; 2 bad usage, name, root or host; 3 the
# checkout cannot be mirrored as it is (symbolic links, names Windows cannot
# hold, nothing to send): nothing was changed; 4 the other machine refused or
# failed (no such work tree, incomplete sync, folder in use, not a Windows
# machine); 127 exec: program not found there; 255 the machine could not be
# reached over SSH (one line with ssh's reason), or the connection was lost
# (what was running there may still be running). Only a `ps` script that
# itself ends with `exit 255` can be mistaken for a lost connection.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
HOST="${ASAMU_WIN_HOST:-}"
# An empty ASAMU_WIN_ROOT is not "the default": a variable that was meant to
# hold another root and came out empty must not send `clean` to D:\asamu.
ROOT="${ASAMU_WIN_ROOT-D:\\asamu}"
REMOTE_LIB="$HERE/win_host_remote.ps1"
PACKER="$HERE/win_host_pack.py"
# No prompts, no terminal; a dead connection is noticed after two minutes.
# Options in the -o form only, so that the list serves scp as well.
SSH_OPTS=(-o BatchMode=yes -o RequestTTY=no -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=8)

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
  # The comment block at the top of this file, whatever its length.
  awk 'NR > 1 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
  exit "${1:-$EX_USAGE}"
}

# die CODE MESSAGE, or die MESSAGE for a usage error.
die() {
  if [ $# -eq 1 ]; then set -- "$EX_USAGE" "$1"; fi
  printf 'win_host: %s\n' "$2" >&2
  exit "$1"
}

# A Windows device name (CON, NUL, COM1, ...), with or without an extension,
# in any case: such a file or folder name opens the device instead.
is_device_name() {
  local lower
  lower="$(printf '%s' "$1" | tr 'ABCDEFGHIJKLMNOPQRSTUVWXYZ' 'abcdefghijklmnopqrstuvwxyz')"
  case "${lower%%.*}" in
    con | prn | aux | nul | com[0123456789] | lpt[0123456789]) return 0 ;;
  esac
  return 1
}

# A work-tree name becomes a folder name on the other machine and part of
# command lines there. The character lists are spelled out (no ranges) so that
# the check does not depend on the locale.
check_name() {
  local n="$1"
  if [ -z "$n" ] || [ "${#n}" -gt 64 ] || [ -n "${n//[$LETTERS$DIGITS._-]/}" ]; then
    die "$EX_USAGE" "bad work tree name $(printf '%q' "$n"): use 1 to 64 letters, digits, . _ -"
  fi
  case "$n" in
    [._-]* | *. | *..*) die "$EX_USAGE" "bad work tree name '$n': it must start with a letter or digit, not end with a dot and not contain '..'" ;;
  esac
  if is_device_name "$n"; then
    die "$EX_USAGE" "bad work tree name '$n': Windows reserves that name for a device"
  fi
}

# The root folder: a drive letter and one or more folder names of safe
# characters, e.g. D:\asamu. It is put into command lines on the other machine
# unquoted, and everything this script deletes there is below it. A drive's
# top folder (D:\) is not a root: this script's folders do not belong there.
check_root() {
  local drive="${ROOT%%:*}" rest="${ROOT#?:}" part parts
  [ -n "$ROOT" ] || die "$EX_USAGE" "ASAMU_WIN_ROOT is set and empty: unset it for the default (D:\\asamu) or give a folder on a drive"
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
  parts="${rest#?}"
  while [ -n "$parts" ]; do
    part="${parts%%\\*}"
    if is_device_name "$part"; then
      die "$EX_USAGE" "bad ASAMU_WIN_ROOT '$ROOT': Windows reserves the name '$part' for a device"
    fi
    case "$parts" in
      *'\'*) parts="${parts#*\\}" ;;
      *) parts="" ;;
    esac
  done
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
#
# When the connection is lost while the script is on its way, nothing tells
# PowerShell either: it would wait for the next line for ever (seen: such a
# session was still there, idle, minutes after ssh had given up). So a second
# thread ends the session when the script has not arrived within two minutes.
BOOTSTRAP='$ProgressPreference = "SilentlyContinue"; $ErrorActionPreference = "Stop"; try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false } catch { }; $g = [hashtable]::Synchronized(@{}); $w = [PowerShell]::Create(); [void]$w.AddScript("param(`$g) Start-Sleep -Seconds 120; if (-not `$g.arrived) { [Console]::Error.WriteLine(`"win_host: the script did not arrive within 120 seconds on the other machine: nothing was run`"); [Environment]::Exit(4) }").AddArgument($g); [void]$w.BeginInvoke(); $n = [Console]::In.ReadLine(); $b = New-Object System.Text.StringBuilder; while ($true) { $l = [Console]::In.ReadLine(); if ($null -eq $l -or $l -eq ".") { break }; [void]$b.Append($l) }; if ($l -ne "." -or [string]$b.Length -ne $n) { [Console]::Error.WriteLine("win_host: the script arrived incomplete on the other machine (" + $b.Length + " of " + $n + " characters): nothing was run"); exit 4 }; $g.arrived = $true; $function:WinHost = [scriptblock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b.ToString()))); try { WinHost } catch { [Console]::Error.WriteLine("win_host: error on the other machine: " + $_.Exception.Message); if ($_.InvocationInfo) { [Console]::Error.WriteLine($_.InvocationInfo.PositionMessage) }; exit 1 }; exit 0'

# Run PowerShell text ($1) on the other machine after the shared functions of
# win_host_remote.ps1. Standard output and error are streamed. The exit code
# there is the return value, and is left in RC: under `set -e` a plain
# `ps_run ...` ends this script with that code when it is not 0.
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
  return "$RC"
}

# One argument as a PowerShell single-quoted string. For names that were
# checked already (the quoting is a second fence); other text travels with
# ps_text.
psq() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
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
# ASAMU_MUTE is set last, so nothing started this way (cargo, exec, gate, ps)
# makes a sound on a machine its owner is sitting at; only `desk`, which puts
# a program on that machine's screen for its owner, runs with sound.
ENTER=""
enter() {
  ENTER="\$work = WhEnter $(psq "$1")$NL${ASAMU_WIN_ENV:-}$NL\$env:ASAMU_MUTE = '1'$NL"
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
  ps_run "WhSyncPrepare $(psq "$name")"
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
  ps_run "WhSyncVerify $(psq "$name") $COUNT $BYTES $(psq "$DIGEST")"
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
  ps_run "${ENTER}WhGate $(psq "$name") @($(
    IFS=,
    printf '%s' "${steps[*]-}"
  ))"
}

[ $# -ge 1 ] || usage
cmd="$1"
shift
case "$cmd" in
  -h | --help | help) usage 0 ;;
esac
check_root
# Every command talks to the other machine, except `sync --dry-run`.
case "$cmd" in
  sync) ;; # checks the host itself, after its options
  *) check_host ;;
esac

case "$cmd" in
  sync)
    cmd_sync "$@"
    ;;
  cargo)
    [ $# -ge 2 ] || die "$EX_USAGE" "usage: cargo NAME ARGS..."
    name="$1"
    shift
    check_name "$name"
    probe
    enter "$name"
    ps_run "${ENTER}WhRun 'cargo' $(ps_array "$@")"
    ;;
  exec)
    [ $# -ge 2 ] || die "$EX_USAGE" "usage: exec NAME PROGRAM [ARGS...]"
    name="$1"
    program="$2"
    shift 2
    check_name "$name"
    [ -n "$program" ] || die "$EX_USAGE" "exec: the program name is empty"
    probe
    enter "$name"
    ps_run "${ENTER}WhRun $(ps_text "$program") $(ps_array "$@")"
    ;;
  gate)
    cmd_gate "$@"
    ;;
  ps)
    [ $# -le 1 ] || die "$EX_USAGE" "usage: ps [NAME]  (the script is read from standard input)"
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
    ;;
  fetch)
    [ $# -eq 2 ] || die "$EX_USAGE" "usage: fetch REMOTE LOCAL"
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
    scp -q "${SSH_OPTS[@]}" "$HOST:$(printf '%s' "$ROOT" | tr '\\' '/')/$remote" "$local_file" || rc=$?
    [ "$rc" -eq 0 ] || die "$EX_REMOTE" "could not fetch $ROOT\\$(printf '%s' "$remote" | tr '/' '\\') from $HOST (scp exit $rc)"
    ;;
  clean)
    [ $# -eq 1 ] || die "$EX_USAGE" "usage: clean NAME"
    check_name "$1"
    probe
    ps_run "WhClean $(psq "$1")"
    ;;
  disk)
    [ $# -eq 0 ] || die "$EX_USAGE" "usage: disk"
    probe
    ps_run "WhDisk"
    ;;
  info)
    [ $# -eq 0 ] || die "$EX_USAGE" "usage: info"
    probe
    ps_run "WhInfo"
    ;;
  # ---- desk runner: this project's programs in the desktop session ---------
  # (tools/dev/desk_runner.py; one self-contained block, keep it last)
  desk | desk-install | desk-status | desk-stop | desk-cancel)
    # PowerShell that runs the installed runner script there with the arguments $1.
    desk_ps() {
      printf '%s' "\$desk = Join-Path (Join-Path \$root 'runner') 'desk_runner.py'; if (-not (Test-Path \$desk)) { [Console]::Error.WriteLine('desk: the desk runner is not installed on that machine: run \"tools/dev/win_host.sh desk-install\" first'); exit 125 }; \$py = Get-Command py -ErrorAction SilentlyContinue; \$pre = @('-3'); if (-not \$py) { \$py = Get-Command python -ErrorAction SilentlyContinue; \$pre = @() }; if (-not \$py) { [Console]::Error.WriteLine('desk: Python 3 was not found on that machine'); exit 125 }; & \$py.Source @pre -B -I -X utf8 \$desk $1; exit \$LASTEXITCODE"
    }
    case "$cmd" in
      desk-install)
        [ $# -eq 0 ] || die "usage: desk-install"
        ps_run "New-Item -ItemType Directory -Force -Path (Join-Path \$root 'runner') | Out-Null"
        scp "${SSH_OPTS[@]}" -q "$HERE/desk_runner.py" "$HOST:$(printf '%s' "$ROOT/runner/desk_runner.py" | tr '\\' '/')"
        ps_run "$(desk_ps "install --root \$root")"
        ;;
      desk-status)
        ps_run "$(desk_ps "status --root \$root")"
        ;;
      desk-stop)
        ps_run "$(desk_ps "stop --root \$root")"
        ;;
      desk-cancel)
        [ $# -eq 1 ] || die "usage: desk-cancel ID"
        # Checked here as well: the id becomes part of a command line on the way.
        [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$ ]] || die "desk-cancel: '$1' is not a job id (see desk-status)"
        ps_run "$(desk_ps "cancel --root \$root $(psq "$1")")"
        ;;
      desk)
        [ $# -ge 2 ] || die "usage: desk NAME PROGRAM [ARGS...]"
        name="$1"
        prog="$2"
        shift 2
        check_name "$name"
        case "$prog" in
          [A-Za-z]:[\\/]*) ;;
          *[\\/]*) prog="$ROOT\\target-$name\\$prog" ;;
          *.*) prog="$ROOT\\target-$name\\release\\$prog" ;;
          *) prog="$ROOT\\target-$name\\release\\$prog.exe" ;;
        esac
        # The job travels as one token, so no argument can be mangled by a
        # command line on the way: the runner gets the list as it is here.
        job_id="$(date -u +%Y%m%d-%H%M%S)-$$-$RANDOM"
        packed="$(python3 "$HERE/desk_runner.py" pack --id "$job_id" --program "$prog" --cwd "$ROOT\\work\\$name" \
          --timeout "${ASAMU_DESK_TIMEOUT:-900}" --env-list "${ASAMU_DESK_ENV:-}" -- "$@")"
        # Interrupting or ending this command must not leave the program
        # running on the other machine's screen: ask the runner to end the job.
        # The wait for the job runs in the background, because bash acts on a
        # signal only when the command it waits for in the foreground is over:
        # ended by a signal of its own (a time limit of the caller, a closed
        # terminal), it would otherwise go on waiting until the job's limit.
        desk_wait=""
        desk_interrupted() {
          trap '' INT TERM HUP
          ps_run "$(desk_ps "cancel --root \$root $(psq "$job_id")")" >&2 || true
          # The wait ends by itself once the runner has ended the job.
          for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "$desk_wait" 2> /dev/null || break
            sleep 1
          done
          kill "$desk_wait" 2> /dev/null || true
          exit 130
        }
        trap desk_interrupted INT TERM HUP
        ps_run "$(desk_ps "submit --root \$root --packed $(psq "$packed")")" &
        desk_wait=$!
        desk_code=0
        wait "$desk_wait" || desk_code=$?
        exit "$desk_code"
        ;;
    esac
    ;;
  # ---- end of the desk runner block ----------------------------------------
  *)
    echo "win_host: unknown command $(printf '%q' "$cmd")" >&2
    usage
    ;;
esac
