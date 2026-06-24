#!/usr/bin/env bash
# Build (if needed) and install the mod into the game folder: the cdylib as the game's dinput8.dll
# proxy, and our launcher as start_protected_game.exe (backing up the original once). This runs on
# the Linux + Proton rig that can actually launch the game, NOT the macOS dev host (which can build
# but can't run Elden Ring). See CLAUDE.md > "Where things run".
set -euo pipefail

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
