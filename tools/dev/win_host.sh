#!/usr/bin/env bash
# Build, test and run this checkout on a second machine that runs Windows,
# over SSH (Windows OpenSSH server with key authentication; Rust and the MSVC
# build tools installed there; `tar` and PowerShell come with Windows).
#
# The checkout stays where it is: `sync` mirrors its files (tracked and
# untracked, not git-ignored: no build output, no local research data) into a
# work tree on the other machine, and `cargo` runs there with its own target
# directory. Nothing else on that machine is touched.
#
#   ASAMU_WIN_HOST=<ssh host> tools/dev/win_host.sh <command> [args]
#
#   sync [NAME]            mirror this checkout to <root>\work\NAME (default NAME: this folder's name)
#   cargo NAME ARGS...     run `cargo ARGS` in that work tree; output is streamed, the exit code is cargo's
#   exec NAME ARGS...      run a program with arguments in that work tree (no shell syntax)
#   ps [NAME]              run a PowerShell script read from stdin (in the work tree when NAME is given)
#   fetch REMOTE LOCAL     copy one file back (REMOTE is relative to <root>)
#   info                   toolchain, free disk and the work trees that exist
#
# Environment:
#   ASAMU_WIN_HOST   SSH host name or alias (required)
#   ASAMU_WIN_ROOT   folder there for work trees and build output (default D:\asamu)
#   ASAMU_WIN_ENV    extra environment for cargo/exec, as PowerShell assignments,
#                    e.g. '$env:ASAMU_CONVERTED_DIR="D:\asamu\converted"'
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
HOST="${ASAMU_WIN_HOST:-}"
ROOT="${ASAMU_WIN_ROOT:-D:\\asamu}"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)

usage() {
  sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

die() {
  echo "win_host: $*" >&2
  exit 1
}

# A work-tree name: letters, digits, dot, dash, underscore (it becomes a folder name).
check_name() {
  [[ "$1" =~ ^[A-Za-z0-9._-]+$ ]] || die "bad work tree name '$1' (letters, digits, . _ - only)"
}

# One argument as a PowerShell single-quoted string.
psq() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

# Run PowerShell text on the other machine. PowerShell's own progress records are dropped.
ps_run() {
  local script b64
  script="\$ProgressPreference='SilentlyContinue'; \$ErrorActionPreference='Stop'; \$env:Path = (Join-Path \$env:USERPROFILE '.cargo\\bin') + ';' + \$env:Path; \$env:CARGO_TERM_COLOR='never'; \$root = $(psq "$ROOT"); $1"
  b64=$(printf '%s' "$script" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\n')
  ssh "${SSH_OPTS[@]}" "$HOST" "powershell -NoProfile -NonInteractive -EncodedCommand $b64" \
    2> >(grep -v -e '^#< CLIXML' -e '^<Objs ' >&2)
}

# PowerShell that enters the work tree NAME and sets the build environment.
enter() {
  printf '%s' "\$work = Join-Path \$root (Join-Path 'work' $(psq "$1")); if (-not (Test-Path \$work)) { throw \"no work tree $1: run sync first\" }; Set-Location \$work; \$env:CARGO_TARGET_DIR = Join-Path \$root $(psq "target-$1"); ${ASAMU_WIN_ENV:-}; "
}

# Arguments as PowerShell literals, space separated.
ps_args() {
  local out="" a
  for a in "$@"; do out+=" $(psq "$a")"; done
  printf '%s' "$out"
}

[ $# -ge 1 ] || usage
cmd="$1"
shift
case "$cmd" in
  -h | --help | help) usage 0 ;;
esac
[ -n "$HOST" ] || die "set ASAMU_WIN_HOST to the SSH host of the Windows machine"

case "$cmd" in
  sync)
    name="${1:-$(basename "$REPO")}"
    check_name "$name"
    work="$ROOT\\work\\$name"
    # Tracked and untracked files that git does not ignore, as they are on disk.
    # The tree there is replaced as a whole; tar keeps modification times, so
    # cargo rebuilds only what changed.
    count=$(cd "$REPO" && git ls-files -co --exclude-standard | while IFS= read -r f; do [ -e "$f" ] && echo "$f"; done | wc -l | tr -d ' ')
    (cd "$REPO" && git ls-files -co --exclude-standard -z |
      while IFS= read -r -d '' f; do [ -e "$f" ] && printf '%s\0' "$f"; done |
      COPYFILE_DISABLE=1 tar --null -T - -cf -) |
      ssh "${SSH_OPTS[@]}" "$HOST" "cmd /c \"(if exist \"$work\" rmdir /s /q \"$work\") & mkdir \"$work\" & tar -xf - -C \"$work\"\""
    echo "win_host: synced $count files to $work"
    ;;
  cargo)
    [ $# -ge 2 ] || die "usage: cargo NAME ARGS..."
    name="$1"
    shift
    check_name "$name"
    ps_run "$(enter "$name") & cargo$(ps_args "$@"); exit \$LASTEXITCODE"
    ;;
  exec)
    [ $# -ge 2 ] || die "usage: exec NAME PROGRAM [ARGS...]"
    name="$1"
    shift
    check_name "$name"
    ps_run "$(enter "$name") &$(ps_args "$@"); exit \$LASTEXITCODE"
    ;;
  ps)
    script="$(cat)"
    if [ $# -ge 1 ]; then
      check_name "$1"
      ps_run "$(enter "$1") $script"
    else
      ps_run "$script"
    fi
    ;;
  fetch)
    [ $# -eq 2 ] || die "usage: fetch REMOTE LOCAL"
    case "$1" in
      *..* | /* | \\* | *:*) die "REMOTE must be a path below $ROOT" ;;
    esac
    remote="$(printf '%s' "$ROOT/$1" | tr '\\' '/')"
    scp "${SSH_OPTS[@]}" -q "$HOST:$remote" "$2"
    ;;
  info)
    ps_run "rustc --version; cargo --version; Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | ForEach-Object { '{0} {1:N0} GB free' -f \$_.DeviceID, (\$_.FreeSpace/1GB) }; if (Test-Path (Join-Path \$root 'work')) { Get-ChildItem (Join-Path \$root 'work') -Directory | ForEach-Object { 'work tree ' + \$_.Name } }"
    ;;
  *)
    usage
    ;;
esac
