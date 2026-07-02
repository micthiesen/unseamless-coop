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
# Gaming (fullscreen) resolution. gamescope's -W/-H default to 1280x720 when omitted, so a bare
# `gamescope -f` fullscreens a blurry 720p buffer — the size must be passed explicitly.
GAMING_W="${UNSEAMLESS_GAMING_WIDTH:-3440}"
GAMING_H="${UNSEAMLESS_GAMING_HEIGHT:-1440}"

if [[ -f "$FLAG" ]]; then
  W=1440 H=900                     # defaults match RIG_WINDOW_WIDTH/HEIGHT in scripts/rig.sh
  read -r W H < "$FLAG" || true   # "WIDTH HEIGHT"; fall back to defaults if malformed/empty
  rm -f "$FLAG"                    # one-shot: consume it so manual launches stay fullscreen
  : "${W:=1440}" "${H:=900}"
  exec gamescope -w "$W" -h "$H" -W "$W" -H "$H" "${COMMON[@]}" "$@"
fi

# Gaming default: true fullscreen at the display's native resolution (must be explicit — see above).
# First undo any resolution clamp a rig run left in the game's saved GraphicsConfig.xml (the game
# persists the small rig display as WINDOW mode, which would upscale blurrily here). Best-effort:
# never block the launch over it.
"$(dirname "${BASH_SOURCE[0]}")/normalize-graphics-config.py" || true
exec gamescope -w "$GAMING_W" -h "$GAMING_H" -W "$GAMING_W" -H "$GAMING_H" -f "${COMMON[@]}" "$@"
