#!/usr/bin/env bash
# Build (if needed) and copy the DLL into the game's Elden Mod Loader mods/ folder.
# This runs on the Linux + Proton rig that can actually launch the game, NOT the macOS dev
# host (which can build but can't run Elden Ring). See CLAUDE.md > "Where things run".
set -euo pipefail

GAME="/mnt/games/SteamLibrary/steamapps/common/ELDEN RING/Game"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DLL="$ROOT/target/x86_64-pc-windows-gnu/release/unseamless_coop.dll"

if [[ ! -f "$DLL" ]]; then
  echo "DLL not built; run: cargo build --release --target x86_64-pc-windows-gnu" >&2
  exit 1
fi

cp -v "$DLL" "$GAME/mods/unseamless_coop.dll"
echo
echo "Deployed to Elden Mod Loader. After launching, the log appears at:"
echo "  $GAME/unseamless_coop.log"
echo "(If it's not there, the game's cwd differs — search the Proton prefix drive_c.)"
