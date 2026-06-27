#!/usr/bin/env bash
# gamescope-wrapper.sh — one Steam launch-options string that serves BOTH normal gaming and
# unseamless-coop rig runs, by choosing the gamescope resolution at launch.
#
# Set your ELDEN RING launch options to (absolute path, then your usual `-- %command%`):
#
#     /home/michael/Code/unseamless-coop/scripts/rig/gamescope-wrapper.sh -- %command%
#
# Behavior:
#   - No rig flag present (you press Play normally): gamescope runs FULLSCREEN at the display's
#     native resolution — same as a plain `gamescope -f … -- %command%`. Gaming is unchanged.
#   - Rig flag present: gamescope renders at the rig size via `-w/-h` (the actual render resolution,
#     so the GPU really does less work) and outputs at the same size, windowed. `scripts/rig.sh`
#     writes the flag right before its own launches; this wrapper consumes and deletes it, so the
#     flag is one-shot and a later manual launch is always fullscreen.
#
# Why a wrapper at all: gamescope's resolution is fixed at launch (no runtime knob), and
# `steam -applaunch` can't override launch options per run. Selecting at launch from a flag is the
# clean way to keep a single launch-options string for both uses.
set -euo pipefail

# Must match RIG_GS_FLAG in scripts/rig.sh (same env override, same default path).
FLAG="${UNSEAMLESS_RIG_GAMESCOPE_FLAG:-${XDG_RUNTIME_DIR:-/tmp}/unseamless-rig-gamescope}"
COMMON=(--immediate-flips)

if [[ -f "$FLAG" ]]; then
  W=2580 H=1080                    # defaults match RIG_WINDOW_WIDTH/HEIGHT in scripts/rig.sh (0.75x of 3440x1440)
  read -r W H < "$FLAG" || true   # "WIDTH HEIGHT"; fall back to defaults if malformed/empty
  rm -f "$FLAG"                    # one-shot: consume it so manual launches stay fullscreen
  : "${W:=2580}" "${H:=1080}"
  exec gamescope -w "$W" -h "$H" -W "$W" -H "$H" "${COMMON[@]}" "$@"
fi

# Gaming default: true fullscreen at the output's native resolution.
exec gamescope -f "${COMMON[@]}" "$@"
