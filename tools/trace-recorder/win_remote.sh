#!/usr/bin/env bash
# Mac-side helper for the Windows trace recorder (docs/TRACE_CAPTURE.md, section 6).
#
# Drives tools/trace-recorder/asamu_win.py on the Windows machine that runs the
# original game, over SSH (Windows OpenSSH server, key authentication). The
# game itself is started and played by its owner through Steam; nothing here
# starts, changes or installs anything on that machine beyond copying our own
# scripts into one working folder and running them with the Python already
# there. Remote commands go through `powershell -EncodedCommand`, so no
# argument ever meets cmd.exe quoting.
#
#   ASAMU_WIN_HOST=<ssh host> tools/trace-recorder/win_remote.sh <command> [args]
#
#   deploy                      copy the recorder into the working folder and compare SHA-256
#   selftest [--live]           run the recorder's tests there (--live: real API path on a stand-in process)
#   game                        is the game running? (exit 0 yes, 2 no)
#   check [args]                asamu_win.py check: live layout check and a sampling probe; records nothing
#   start <scenario> [frames] [args]   start a detached recording (args go to asamu_win.py start)
#   status [--json]             counters of the current or last recording
#   stop                        end the recording (STOP file) and print its summary
#   fetch [--validate]          copy finished recordings to research/local/traces/win
#   run <args...>               any asamu_win.py command line
#
# Environment:
#   ASAMU_WIN_HOST     SSH host name or alias of the game machine (required)
#   ASAMU_WIN_DIR      working folder there, relative to %USERPROFILE% (default asamu-trace)
#   ASAMU_WIN_PYTHON   Python there: "py" (the launcher, default) or a path; %VARS% are expanded there
#   ASAMU_WIN_TRACES   local folder for fetched recordings (default research/local/traces/win)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
HOST="${ASAMU_WIN_HOST:-}"
REMOTE_DIR="${ASAMU_WIN_DIR:-asamu-trace}"
PYTHON="${ASAMU_WIN_PYTHON:-py}"
LOCAL_TRACES="${ASAMU_WIN_TRACES:-$REPO/research/local/traces/win}"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)
DEPLOY_FILES=(asamu_win.py asamu_recorder_core.py layout_win_x86.json test_win_glue.py)
GLOBALS="$REPO/docs/reverse-engineering/data/win32/globals.json"

usage() {
  sed -n '2,29p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

die() {
  echo "win_remote: $*" >&2
  exit 1
}

# One argument as a PowerShell single-quoted string.
psq() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

# Run PowerShell text on the game machine; PowerShell's own progress records are dropped.
ps_run() {
  local script b64 rc
  script="\$ProgressPreference='SilentlyContinue'; \$env:PYTHONIOENCODING='utf-8'; \$d = Join-Path \$env:USERPROFILE $(psq "$REMOTE_DIR"); $1"
  b64=$(printf '%s' "$script" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\n')
  set +e
  ssh "${SSH_OPTS[@]}" "$HOST" "powershell -NoProfile -NonInteractive -EncodedCommand $b64" 2>&1 |
    sed -e '/^#< CLIXML/d' -e '/^<Objs Version=/d' -e 's/\r$//'
  rc=${PIPESTATUS[0]}
  set -e
  return "$rc"
}

# The Python invocation, as PowerShell text.
ps_python() {
  if [ "$PYTHON" = "py" ]; then
    printf '& py -3 -I -B'
  else
    printf '& ([Environment]::ExpandEnvironmentVariables(%s)) -I -B' "$(psq "$PYTHON")"
  fi
}

# Run a script of the working folder there: py_run <script> [args...]
py_run() {
  local script="$1" args="" a
  shift
  for a in "$@"; do
    args="$args $(psq "$a")"
  done
  ps_run "Set-Location \$d; $(ps_python) (Join-Path \$d $(psq "$script"))$args; exit \$LASTEXITCODE"
}

sha() {
  shasum -a 256 "$1" | awk '{print $1}'
}

cmd_deploy() {
  local f name want got bad=0
  for f in "${DEPLOY_FILES[@]}"; do
    [ -f "$HERE/$f" ] || die "missing $HERE/$f"
  done
  ps_run "New-Item -ItemType Directory -Force -Path \$d, (Join-Path \$d 'data\\win32') | Out-Null; 'working folder ' + \$d"
  for f in "${DEPLOY_FILES[@]}"; do
    scp -q "${SSH_OPTS[@]}" "$HERE/$f" "$HOST:$REMOTE_DIR/$f"
  done
  if [ -f "$GLOBALS" ]; then
    # Optional: gives the recorder GCurrentTime (late-sample check in fixed-step mode).
    scp -q "${SSH_OPTS[@]}" "$GLOBALS" "$HOST:$REMOTE_DIR/data/win32/globals.json"
  fi
  got=$(ps_run "Get-ChildItem -Path \$d, (Join-Path \$d 'data\\win32') -File | ForEach-Object { (Get-FileHash -Algorithm SHA256 \$_.FullName).Hash.ToLower() + ' ' + \$_.Name }; exit 0") || die "cannot hash the deployed files: $got"
  for f in "${DEPLOY_FILES[@]}" "data/win32/globals.json"; do
    name=$(basename "$f")
    if [ "$name" = globals.json ]; then
      [ -f "$GLOBALS" ] || continue
      want=$(sha "$GLOBALS")
    else
      want=$(sha "$HERE/$f")
    fi
    if printf '%s\n' "$got" | grep -q "^$want $name\$"; then
      echo "ok   $name  $want"
    else
      echo "BAD  $name  (expected $want)"
      bad=1
    fi
  done
  [ "$bad" = 0 ] || die "deployed files differ from the local ones"
}

cmd_selftest() {
  py_run asamu_recorder_core.py selftest --layout layout_win_x86.json
  py_run test_win_glue.py
  if [ "${1:-}" = "--live" ]; then
    py_run test_win_glue.py --live-fake
  fi
}

cmd_game() {
  local out
  out=$(ps_run "Get-Process -Name 'ASAMU-Win32-Shipping' -ErrorAction SilentlyContinue | ForEach-Object { 'running: pid ' + \$_.Id + ', session ' + \$_.SessionId + ', started ' + \$_.StartTime.ToUniversalTime().ToString('s') + 'Z' }; exit 0") || die "cannot ask the game machine: $out"
  if [ -n "$out" ]; then
    echo "$out"
  else
    echo "not running (the owner starts the game through Steam)"
    return 2
  fi
}

cmd_start() {
  [ $# -ge 1 ] || die "start needs a scenario id, e.g. start T1 600"
  local scenario="$1" frames=""
  shift
  if [ $# -ge 1 ] && printf '%s' "$1" | grep -Eq '^[0-9]+$'; then
    frames="$1"
    shift
  fi
  if [ -n "$frames" ]; then
    py_run asamu_win.py start --detach --scenario "$scenario" --frames "$frames" "$@"
  else
    py_run asamu_win.py start --detach --scenario "$scenario" "$@"
  fi
}

cmd_fetch() {
  local validate=0 names name new=()
  [ "${1:-}" = "--validate" ] && validate=1
  mkdir -p "$LOCAL_TRACES"
  names=$(ps_run "\$t = Join-Path \$d 'traces'; if (Test-Path \$t) { Get-ChildItem \$t -File | Where-Object { \$_.Name -like '*.raw.jsonl' -or \$_.Name -like '*.stats.json' } | ForEach-Object { \$_.Name } }; exit 0") || die "cannot list the recordings: $names"
  for name in $names; do
    # Names come from the other machine: accept plain file names only.
    printf '%s' "$name" | grep -Eq '^[A-Za-z0-9._+-]+\.(raw\.jsonl|stats\.json)$' || { echo "skipped odd name: $name"; continue; }
    if [ -e "$LOCAL_TRACES/$name" ]; then
      continue
    fi
    case "$name" in
      *.raw.jsonl)
        # The recorder writes <stem>.stats.json after it has closed the raw file: a raw file without it
        # is still being written (or its recorder was killed). A copy taken now would be kept as if complete.
        if ! printf '%s\n' "$names" | grep -Fxq "${name%.raw.jsonl}.stats.json"; then
          echo "not fetched (no .stats.json yet: still recording?): $name"
          continue
        fi
        ;;
    esac
    scp -q "${SSH_OPTS[@]}" "$HOST:$REMOTE_DIR/traces/$name" "$LOCAL_TRACES/$name"
    echo "fetched $LOCAL_TRACES/$name"
    case "$name" in *.raw.jsonl) new+=("$LOCAL_TRACES/$name") ;; esac
  done
  if [ ${#new[@]} -eq 0 ]; then
    echo "no new recordings"
    return 0
  fi
  if [ "$validate" = 1 ]; then
    (cd "$REPO" && cargo run -q -p asamu-trace -- validate "${new[@]}")
    for name in "${new[@]}"; do
      (cd "$REPO" && cargo run -q -p asamu-trace -- convert "$name")
    done
  else
    echo "next: cargo run -p asamu-trace -- validate ${new[*]}"
  fi
}

[ $# -ge 1 ] || usage 2
command="$1"
shift
case "$command" in
  -h | --help | help) usage 0 ;;
esac
[ -n "$HOST" ] || die "set ASAMU_WIN_HOST to the SSH host of the game machine"
case "$command" in
  deploy) cmd_deploy ;;
  selftest) cmd_selftest "$@" ;;
  game) cmd_game ;;
  check) py_run asamu_win.py check "$@" ;;
  start) cmd_start "$@" ;;
  status) py_run asamu_win.py status "$@" ;;
  stop) py_run asamu_win.py stop "$@" ;;
  fetch) cmd_fetch "$@" ;;
  run) py_run asamu_win.py "$@" ;;
  *) usage 2 ;;
esac
