#!/usr/bin/env bash
# Bare install primitive: copy the cdylib in as the game's dinput8.dll proxy and our launcher as
# start_protected_game.exe. **No backup safety** — it overwrites those files in place. On a machine
# that runs the real ERSC + Elden Mod Loader stack (the gaming PC) that clobbers it irrecoverably, so
# `scripts/rig.sh apply` (which snapshots the original stack first; `rig.sh restore` reverts) is the
# entrypoint everywhere and supersedes this script. See CLAUDE.md > "Where things run" and the
# /test-loop skill (layer 4).
set -euo pipefail

# Guard: refuse to run standalone. Passes only when invoked under rig.sh (UNSEAMLESS_RIG_DRIVER) or
# with an explicit acknowledgement for the one legit case — a clean rig with no real stack to protect.
if [[ -z "${UNSEAMLESS_RIG_DRIVER:-}" && -z "${UNSEAMLESS_DEPLOY_STANDALONE:-}" ]]; then
  cat >&2 <<'EOF'
deploy.sh: refusing to run standalone — it installs into the game folder with NO backup safety,
which clobbers the real ERSC / Elden Mod Loader stack on a machine that has one.

  Use instead:   scripts/rig.sh apply      (snapshots the original stack first; rig.sh restore reverts)

  deploy.sh is the bare install primitive, superseded by rig.sh. If you really mean to install on a
  clean rig with nothing to protect, re-run with:   UNSEAMLESS_DEPLOY_STANDALONE=1 scripts/deploy.sh
EOF
  exit 1
fi

GAME="/mnt/games/SteamLibrary/steamapps/common/ELDEN RING/Game"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/target/x86_64-pc-windows-gnu/release"
DLL="$OUT/unseamless_coop.dll"
LAUNCHER="$OUT/start_protected_game.exe"

if [[ ! -f "$DLL" || ! -f "$LAUNCHER" ]]; then
  echo "Not built; run: cargo build --release --target x86_64-pc-windows-gnu" >&2
  exit 1
fi

# The mod is the game's dinput8.dll proxy (auto-loaded; no separate mod loader).
cp -v "$DLL" "$GAME/dinput8.dll"
mkdir -p "$GAME/mods"   # other DLL mods we load go here

# Our launcher replaces start_protected_game.exe. Back up the original EAC bootstrapper once so it
# can be restored without a Steam "verify integrity".
if [[ -f "$GAME/start_protected_game.exe" && ! -f "$GAME/start_protected_game_eac.exe" ]]; then
  cp -v "$GAME/start_protected_game.exe" "$GAME/start_protected_game_eac.exe"
fi
cp -v "$LAUNCHER" "$GAME/start_protected_game.exe"

echo
echo "Installed. Launch via Steam 'Play' (runs our start_protected_game.exe, which starts the game"
echo "directly with the UNSEAMLESS_LAUNCH marker set). The run log appears at:"
echo "  $GAME/unseamless-coop/logs/"
echo "(To restore vanilla/EAC: Steam > Verify integrity of game files, then delete dinput8.dll.)"
