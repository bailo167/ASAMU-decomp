#!/bin/bash
# Launch the ORIGINAL Mac build (read-only use of the user's Steam install) with the
# options the trace recorder needs, optionally straight into a map.
#
#   tools/trace-recorder/launch_original.sh [MAP] [extra args...]
#
# Fullscreen at the main display's resolution by default; ASAMU_WINDOWED=1 gives a
# 1280x720 window instead. ASAMU_RES=WxH overrides the resolution.
#
# -ONETHREAD   required on Apple silicon/modern macOS: the threaded renderer has no
#              current GL context on its thread and crashes in glClear (see
#              docs/TRACE_CAPTURE.md "Launching on modern macOS").
# -BENCHMARK -FPS=60  fixed 1/60 s game step (E5) so traces replay at 60 Hz.
# SDL_GAMECONTROLLERCONFIG adds a DualSense mapping the bundled 2014 SDL2 lacks
#              (GUID as that SDL computes it: vendor 054c / product 0ce6).
set -euo pipefail
ROOT="${ASAMU_ORIGINAL_DIR:-$HOME/Library/Application Support/Steam/steamapps/common/A Story About My Uncle}"
APP="$ROOT/A Story About My Uncle.app"
[ -d "$APP" ] || { echo "original app not found: $APP" >&2; exit 1; }
MAP_ARG=()
if [ $# -gt 0 ] && [[ "$1" != -* ]]; then MAP_ARG=("$1"); shift; fi
if [ "${ASAMU_WINDOWED:-0}" = 1 ]; then
  MODE=(-WINDOWED -ResX=1280 -ResY=720)
else
  RES="${ASAMU_RES:-$(system_profiler SPDisplaysDataType 2>/dev/null | awk '/Resolution:/{r=$2"x"$4} /Main Display: Yes/{print r; exit}')}"
  RES="${RES:-1920x1080}"
  MODE=(-FULLSCREEN -ResX="${RES%x*}" -ResY="${RES#*x}")
fi
DUALSENSE='4c05000000000000e60c000000000000,PS5 Controller,a:b1,b:b2,back:b8,dpdown:h0.4,dpleft:h0.8,dpright:h0.2,dpup:h0.1,guide:b12,leftshoulder:b4,leftstick:b10,lefttrigger:a3,leftx:a0,lefty:a1,rightshoulder:b5,rightstick:b11,righttrigger:a4,rightx:a2,righty:a5,start:b9,x:b0,y:b3,'
HERE="$(cd "$(dirname "$0")" && pwd)"
LOGDIR="${ASAMU_TRACE_DIR:-$HERE/../../research/local/traces}"; mkdir -p "$LOGDIR"
open --env "ALSOFT_CONF=${ALSOFT_CONF:-$HERE/alsoft.conf}" \
     --env "ALSOFT_LOGLEVEL=${ALSOFT_LOGLEVEL:-2}" --env "ALSOFT_LOGFILE=$LOGDIR/openal.log" \
     --env "SDL_GAMECONTROLLERCONFIG=${SDL_GAMECONTROLLERCONFIG:-$DUALSENSE}" \
     --env "SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS=1" \
     -a "$APP" --args "${MAP_ARG[@]}" -ONETHREAD -NOSPLASH -BENCHMARK -FPS=60 "${MODE[@]}" "$@"
for _ in $(seq 1 30); do P=$(pgrep -x ASAMU || true); [ -n "$P" ] && { echo "$P"; exit 0; }; sleep 0.5; done
echo "game did not start" >&2; exit 1
