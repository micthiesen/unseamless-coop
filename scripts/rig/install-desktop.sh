#!/usr/bin/env bash
# install-desktop.sh — generate KDE/XDG .desktop launchers for the common rig.sh operations,
# so the apply/launch/restore loop is one click from the app menu on the gaming PC. Re-runnable
# and machine-local: writes to ~/.local/share/applications with absolute paths into THIS checkout.
#
#   bash scripts/rig/install-desktop.sh            # install/refresh the launchers
#   bash scripts/rig/install-desktop.sh --remove   # remove them
#
# Each launcher opens a terminal (Alacritty) running rig.sh; the terminal closes when the command
# finishes (use the Log launcher to follow output). Icon reuses Steam's Elden Ring art.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
RIG="$HERE/../rig.sh"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
PREFIX="unseamless-rig-"
TERMINAL="${RIG_DESKTOP_TERMINAL:-alacritty}"
ICON="${RIG_DESKTOP_ICON:-steam_icon_1245620}"

# id | Menu name | rig.sh args
ACTIONS=(
  "cycle|unseamless: Apply + Launch|cycle"
  "apply|unseamless: Apply (no launch)|apply"
  "launch|unseamless: Launch|launch --wait"
  "restore|unseamless: Restore original stack|restore"
  "status|unseamless: Status|status"
  "log|unseamless: Log (follow)|log -f"
  "kill|unseamless: Kill game|kill"
)

if [[ "${1:-}" == "--remove" ]]; then
  rm -f "$APPS_DIR/$PREFIX"*.desktop
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" 2>/dev/null || true
  echo "removed $PREFIX*.desktop from $APPS_DIR"
  exit 0
fi

command -v "$TERMINAL" >/dev/null 2>&1 || { echo "ERROR: terminal '$TERMINAL' not found (set RIG_DESKTOP_TERMINAL=...)" >&2; exit 1; }
mkdir -p "$APPS_DIR"

for entry in "${ACTIONS[@]}"; do
  IFS='|' read -r id name args <<<"$entry"
  file="$APPS_DIR/$PREFIX$id.desktop"
  cat > "$file" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=$name
Comment=rig.sh $args (unseamless-coop test rig)
Exec=$TERMINAL --title "$name" -e "$RIG" $args
Icon=$ICON
Terminal=false
Categories=Game;
StartupNotify=true
EOF
  echo "wrote $file"
done

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" 2>/dev/null || true
echo "done — search '${PREFIX%-}' or 'unseamless' in your app launcher."
