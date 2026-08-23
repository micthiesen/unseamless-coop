#!/usr/bin/env bash
# Steam launch-options wrapper for one side of the local two-player rig.
set -euo pipefail

INSTANCE=${1:?first argument must be p1 or p2}
shift
[[ "$INSTANCE" == p1 || "$INSTANCE" == p2 ]] || { echo "invalid instance: $INSTANCE" >&2; exit 2; }

DUO_ROOT=${UNSEAMLESS_DUO_ROOT:-"$HOME/.local/share/unseamless-duo"}
RUNTIME_DIR="$DUO_ROOT/runtime/$INSTANCE"
mkdir -p "$RUNTIME_DIR"
printf '%s\n' "$$" > "$RUNTIME_DIR/wrapper.pid"

export UNSEAMLESS_DUO_INSTANCE="$INSTANCE"
export SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT=00000000000000000000000000000000

if [[ "$INSTANCE" == p1 ]]; then
  WIDTH=${UNSEAMLESS_DUO_P1_WIDTH:-960}
  HEIGHT=${UNSEAMLESS_DUO_P1_HEIGHT:-540}
  if [[ ${UNSEAMLESS_DUO_P1_HEADLESS:-0} == 1 ]]; then
    exec gamescope --backend headless -w "$WIDTH" -h "$HEIGHT" -W "$WIDTH" -H "$HEIGHT" -r 30 --immediate-flips "$@"
  fi
  exec gamescope -w "$WIDTH" -h "$HEIGHT" -W "$WIDTH" -H "$HEIGHT" -r 30 --immediate-flips "$@"
fi

WIDTH=${UNSEAMLESS_DUO_P2_WIDTH:-640}
HEIGHT=${UNSEAMLESS_DUO_P2_HEIGHT:-360}
exec gamescope --backend headless -w "$WIDTH" -h "$HEIGHT" -W "$WIDTH" -H "$HEIGHT" -r 30 --immediate-flips "$@"
