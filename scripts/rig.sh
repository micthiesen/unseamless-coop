#!/usr/bin/env bash
# rig.sh — drive the local Linux + Proton rig (this gaming PC) for unseamless-coop testing.
#
# This machine already runs the *real* mod stack: Elden Mod Loader (dinput8.dll) + Seamless Co-op
# (SeamlessCoop/ersc.dll, launched via an ersc-launcher copy at start_protected_game.exe) + the
# user's own DLL mods in mods/ (er_crit_coop, cosmetics). Testing unseamless-coop means temporarily
# standing in for that stack: our cdylib becomes dinput8.dll and our launcher becomes
# start_protected_game.exe.
#
# The safety model (per the user's workflow):
#   - `backup`  snapshots the ORIGINAL stack exactly once, to a dir OUTSIDE the game folder. Guarded
#               so it can never capture our own mod as "the original".
#   - `apply`   installs our mod over it. Safe to run as many times as you like; never auto-restores.
#   - `restore` is EXPLICIT only — it puts the original stack back. Nothing else rolls back for you.
#
# So the loop is: backup once -> apply/launch/log/kill freely -> restore when the user says so.
#
# Everything is overridable by env: GAME_DIR, BACKUP_DIR, APPID.
set -euo pipefail

# Sentinel: marks every child process as "running under the rig orchestrator". Guarded primitives
# (currently scripts/deploy.sh) check for this so they refuse to run standalone, where there's no
# backup safety. Exported so it reaches anything rig.sh execs.
export UNSEAMLESS_RIG_DRIVER=1

# ---- paths -------------------------------------------------------------------------------------
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GAME_DIR="${GAME_DIR:-/mnt/games/SteamLibrary/steamapps/common/ELDEN RING/Game}"
BACKUP_DIR="${BACKUP_DIR:-$HOME/.local/share/unseamless-coop/rig-backup}"
APPID="${APPID:-1245620}"
# Elden Ring's save folder (…/EldenRing/<SteamID64>), used by `seed-save`. Empty = derive it from the
# Steam library that holds GAME_DIR (…/steamapps/common/… -> …/steamapps/compatdata/APPID/pfx/…); set
# it explicitly if your prefix lives elsewhere.
SAVE_DIR="${SAVE_DIR:-}"
TRIPLE="x86_64-pc-windows-gnu"
SEED_CONFIG="$ROOT/scripts/rig/seed-config.toml"
# Where/how big the game runs during a rig launch. The point: keep your Steam launch options set to
# your *gaming* config and still rig in a small, low-res window. Two cooperating pieces:
#   - render resolution: scripts/rig/gamescope-wrapper.sh (set as your launch options) reads RIG_GS_FLAG
#     at launch and runs gamescope at the rig size via -w/-h (a real lower render res). cmd_launch
#     writes that flag; the wrapper consumes + deletes it, so manual launches stay fullscreen.
#   - window placement: reposition_window then nudges that window to the top-left.
# Override any of these via env.
WINDOW_MARGIN="${WINDOW_MARGIN:-24}"
# Rig render size: 1440x900 (16:10). A compact window that leaves room on the 3440x1440 panel for
# the terminal/logs alongside the game; the gamescope wrapper renders at this real resolution.
RIG_WINDOW_WIDTH="${RIG_WINDOW_WIDTH:-1440}"
RIG_WINDOW_HEIGHT="${RIG_WINDOW_HEIGHT:-900}"
# One-shot flag handed to gamescope-wrapper.sh. MUST match the wrapper's FLAG (same env, same default).
RIG_GS_FLAG="${UNSEAMLESS_RIG_GAMESCOPE_FLAG:-${XDG_RUNTIME_DIR:-/tmp}/unseamless-rig-gamescope}"
# Minimize the game window the moment it appears and keep it hidden until reposition_window places it,
# so you don't see the centered/loading window before it snaps to the corner. Set 0 to disable (e.g.
# if a minimized window ever throttles the load). The reveal always runs, so this can't strand it.
RIG_HIDE_UNTIL_PLACED="${RIG_HIDE_UNTIL_PLACED:-1}"
# Re-blank the display after a launch that woke it. If the screen was DPMS-off (blanked via the
# Blank Screen tool) when a launch started, starting the game forces the monitor back on; this puts
# it back to sleep RIG_REBLANK_DELAY seconds later so a remote/headless rig run doesn't leave the
# panel lit. Set RIG_REBLANK=0 to disable. Only fires when the screen was already blanked.
RIG_REBLANK="${RIG_REBLANK:-1}"
RIG_REBLANK_DELAY="${RIG_REBLANK_DELAY:-20}"
# Auto-dismiss the startup popups (the offline-mode / connection-error dialogs we can't suppress in
# code — they're Arxan-hardened, see docs/OFFLINE-TITLE-SCREEN.md) by injecting key presses via
# ydotool. `cycle` does this by default so a solo smoke test lands at the menu unattended; opt out
# with `cycle --no-dismiss`, or trigger it yourself with `rig.sh dismiss`.
RIG_YDOTOOL_SOCKET="${YDOTOOL_SOCKET:-${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/.ydotool_socket}"
# How fast `dismiss` clicks through the startup popups, and how long the whole sequence runs. The
# popups are MODAL (persist until confirmed), so the loop only needs to (a) press fast enough that
# each one dies the moment it appears, and (b) keep going long enough to outlast the slowest popup's
# *appearance* — the "offline mode" popup (#3) fires after a connection attempt times out (~10-15s),
# well after the first two. Total span ≈ PRESSES × INTERVAL (+ a focus settle every REFOCUS_EVERY
# presses). Defaults: 22 × 0.4s ≈ 11s span (measured ~0.52s/press incl. the periodic re-focus),
# long enough to outlast popup #3's ~10-15s timeout. The popups are *in-engine* dialogs, not OS
# windows, so they can't steal X focus between presses — re-focusing every single press was wasted
# time, hence REFOCUS_EVERY (focus once, then only re-assert occasionally). Dial these in live with
# the per-press desktop toasts (RIG_DISMISS_NOTIFY=1): watch when each press fires vs. when a popup
# actually shows, then raise PRESSES/INTERVAL if popup #3 still slips through, or drop them to go
# shorter. Set RIG_DISMISS_NOTIFY=0 to silence the toasts once dialed in.
RIG_DISMISS_PRESSES="${RIG_DISMISS_PRESSES:-22}"
RIG_DISMISS_INTERVAL="${RIG_DISMISS_INTERVAL:-0.4}"
RIG_DISMISS_REFOCUS_EVERY="${RIG_DISMISS_REFOCUS_EVERY:-4}"   # re-focus the game window every Nth press (1 = every press, old behavior)
RIG_DISMISS_FOCUS_SETTLE="${RIG_DISMISS_FOCUS_SETTLE:-0.25}"  # pause after activating the window before injecting (was a hardcoded 0.5)
RIG_DISMISS_NOTIFY="${RIG_DISMISS_NOTIFY:-1}"                 # per-press desktop toast for visually dialing in the timing

# ---- friend-bundle packaging (rig.sh package / share) ------------------------------------------
# `package` builds the mod and assembles a self-contained zip a friend installs on Windows
# (dist/unseamless-coop-<build_id>.zip): our two binaries, a shared seed config (their password + an
# isolated save extension + debug logging), a MANIFEST with sha256s, and the PowerShell installer
# from scripts/dist/. `share` uploads it to a GitHub prerelease. See scripts/dist/README-FRIENDS.txt.
DIST_DIR="${DIST_DIR:-$ROOT/dist}"
DIST_SRC="$ROOT/scripts/dist"
# Save extension baked into the friend config. Distinct from vanilla (.sl2) AND ERSC (.co2) so a
# tester's real saves are never touched. Matches the rig seed's isolation choice.
FRIEND_SAVE_EXT="${FRIEND_SAVE_EXT:-uco}"
# Where the shared co-op password comes from (precedence: --password flag, this env, then this
# gitignored file). Must be >= 5 chars (the mod's startup guard rejects shorter).
SHARED_PASSWORD_FILE="${SHARED_PASSWORD_FILE:-$DIST_SRC/.shared-password}"
# GitHub prerelease tag `share` uploads to (a rolling bucket of test builds, not a version release).
SHARE_TAG="${SHARE_TAG:-friends-test}"
# Password the friend bundle zip is encrypted with. This is NOT a secret (it's published in the
# release notes); its only job is to keep browsers/AV from scanning the .exe inside and throwing
# "unsafe download" false positives. Windows Explorer extracts a ZipCrypto zip with a password
# prompt, so friends still don't need any extra tool. Surfaced in the share notes by `cmd_share`.
FRIEND_ZIP_PASSWORD="${FRIEND_ZIP_PASSWORD:-test}"

# Files in the game folder our apply overwrites — i.e. the surface that must be snapshotted to
# restore the original stack. SeamlessCoop/ is deliberately NOT here: apply never touches it (ERSC
# just stays dormant because we swap its launcher), so its ersc_settings.ini / password are safe.
MANAGED_FILES=(dinput8.dll start_protected_game.exe mod_loader_config.ini)
# mods/ is snapshotted as a whole tree (handled separately from the flat files above).

# Our install marker, written by `apply`, removed by `restore`. Its presence means "our mod is
# currently installed" — the backup guard reads it so it never snapshots our mod as the original.
MARKER="$GAME_DIR/unseamless-coop/.rig-applied"
CONFIG_DST="$GAME_DIR/unseamless-coop/unseamless_coop.toml"
LOG_DIR="$GAME_DIR/unseamless-coop/logs"

# ---- output helpers ----------------------------------------------------------------------------
say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  !\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# Desktop toast (best-effort, KDE/libnotify). The synchronous hint makes a run of calls replace each
# other in place, so the dismiss loop reads as one live counter instead of stacking N toasts. No-op
# if notify-send is missing or RIG_DISMISS_NOTIFY=0. $1=summary $2=body $3=timeout-ms (default 1200).
notify() {
  [[ "${RIG_DISMISS_NOTIFY:-1}" == 1 ]] || return 0
  command -v notify-send >/dev/null 2>&1 || return 0
  notify-send -a unseamless-rig -u low -t "${3:-1200}" \
    -h string:x-canonical-private-synchronous:rig-dismiss \
    "$1" "${2:-}" 2>/dev/null || true
}

# Copy $1 to the system clipboard, best-effort. Wayland-native wl-copy first, then X fallbacks
# (xclip/xsel), then pbcopy. Returns 1 if no tool is found. NOTE: detects real binaries — an
# interactive `pbcopy` *alias* (e.g. xsel) isn't visible in this non-interactive script. wl-copy forks
# to hold the selection, so the value survives after rig.sh exits.
clip_copy() {  # $1 = text
  local text="$1" tool
  for tool in wl-copy xclip xsel pbcopy; do
    command -v "$tool" >/dev/null 2>&1 || continue
    case "$tool" in
      wl-copy) printf '%s' "$text" | wl-copy >/dev/null 2>&1 && return 0 ;;
      xclip)   printf '%s' "$text" | xclip -selection clipboard >/dev/null 2>&1 && return 0 ;;
      xsel)    printf '%s' "$text" | xsel --clipboard --input >/dev/null 2>&1 && return 0 ;;
      pbcopy)  printf '%s' "$text" | pbcopy >/dev/null 2>&1 && return 0 ;;
    esac
  done
  return 1
}

need_game_dir() { [[ -d "$GAME_DIR" ]] || die "game folder not found: $GAME_DIR (set GAME_DIR=...)"; }
backup_exists() { [[ -f "$BACKUP_DIR/MANIFEST" ]]; }
applied()       { [[ -f "$MARKER" ]]; }

# ---- backup --------------------------------------------------------------------------------------
cmd_backup() {
  need_game_dir
  if backup_exists; then
    ok "snapshot already exists at $BACKUP_DIR (taken $(sed -n 's/^date: //p' "$BACKUP_DIR/MANIFEST"))"
    say "Nothing to do. To roll back use 'rig.sh restore'; to force a new snapshot delete that dir first."
    return 0
  fi
  # Refuse to snapshot if our mod is already installed and there's no prior snapshot: we'd capture
  # our own dinput8.dll/launcher as "the original" and lose the real stack forever.
  if applied; then
    die "our mod appears to be installed (marker $MARKER) but there is no snapshot.
       Refusing to snapshot — it would record OUR mod as the original.
       If you're sure the game folder is back to its original ERSC+EML state, remove the marker:
         rm '$MARKER'
       then re-run 'rig.sh backup'."
  fi

  say "Snapshotting the original install -> $BACKUP_DIR"
  mkdir -p "$BACKUP_DIR"
  # Build the MANIFEST under a .partial name and only `mv` it into place AFTER every copy succeeds.
  # backup_exists() keys on the final name, so a mid-snapshot failure (under set -e) leaves no
  # MANIFEST -> the next backup retries instead of treating an incomplete snapshot as complete (which
  # would let `apply` overwrite originals we never captured). The Windows installer does the same.
  local mani="$BACKUP_DIR/MANIFEST" partial="$BACKUP_DIR/.MANIFEST.partial"
  {
    echo "# unseamless-coop rig backup — the original ERSC + Elden Mod Loader stack."
    echo "date: $(date -Is)"
    echo "source: $GAME_DIR"
    echo "files:"
  } > "$partial"

  for f in "${MANAGED_FILES[@]}"; do
    if [[ -f "$GAME_DIR/$f" ]]; then
      cp -p "$GAME_DIR/$f" "$BACKUP_DIR/$f"
      printf '  %s  %s\n' "$(sha256sum "$GAME_DIR/$f" | cut -d' ' -f1)" "$f" >> "$partial"
      ok "saved $f"
    else
      warn "no $f in game folder (skipping)"
    fi
  done

  if [[ -d "$GAME_DIR/mods" ]]; then
    rsync -a --delete "$GAME_DIR/mods/" "$BACKUP_DIR/mods/"
    echo "  mods/ ($(find "$BACKUP_DIR/mods" -maxdepth 1 -name '*.dll' | wc -l | tr -d ' ') dll(s))" >> "$partial"
    ok "saved mods/ ($(find "$BACKUP_DIR/mods" -maxdepth 1 -name '*.dll' -printf '%f ' 2>/dev/null))"
  fi
  mv "$partial" "$mani"   # commit the snapshot: now backup_exists() sees a complete MANIFEST
  say "Snapshot complete. This is your rollback point; 'apply' will never touch it."
}

# ---- restore (explicit only) ---------------------------------------------------------------------
cmd_restore() {
  need_game_dir
  backup_exists || die "no snapshot at $BACKUP_DIR — nothing to restore."
  say "Restoring the original ERSC + Elden Mod Loader stack from $BACKUP_DIR"
  for f in "${MANAGED_FILES[@]}"; do
    if [[ -f "$BACKUP_DIR/$f" ]]; then
      cp -p "$BACKUP_DIR/$f" "$GAME_DIR/$f"
      ok "restored $f"
    fi
  done
  if [[ -d "$BACKUP_DIR/mods" ]]; then
    rsync -a --delete "$BACKUP_DIR/mods/" "$GAME_DIR/mods/"
    ok "restored mods/ (exact original set)"
  fi
  rm -f "$MARKER"
  ok "removed our install marker"
  say "Original stack is back. (Our unseamless-coop/ folder with logs is left in place; harmless — "
  say "  delete it manually if you want it gone.) Launch via Steam to play your normal co-op setup."
}

# ---- build ---------------------------------------------------------------------------------------
# Default build profile is `diag` (symbols + debug-assertions -> readable panic backtraces), which
# is what you want while chasing rig behavior. `--release` switches to the shipping profile.
build() {
  local profile="$1"
  say "Building ($profile) for $TRIPLE"
  if [[ "$profile" == release ]]; then
    ( cd "$ROOT" && cargo build --release )
  else
    # diag is the rig build: include the dev side-channel bridge (the overlay is always-on now).
    ( cd "$ROOT" && cargo build --profile "$profile" --features unseamless-coop/bridge )
  fi
}

artifact_dir() { echo "$ROOT/target/$TRIPLE/$1"; }

# ---- apply (safe, repeatable) --------------------------------------------------------------------
cmd_apply() {
  local profile=diag do_build=1 keep_config=0 with_mods=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --release)     profile=release ;;
      --diag)        profile=diag ;;
      --no-build)    do_build=0 ;;
      --keep-config) keep_config=1 ;;
      --with-mods)   with_mods="${2:-}"; shift ;;
      --with-mods=*) with_mods="${1#*=}" ;;
      *) die "apply: unknown option '$1'" ;;
    esac
    shift
  done
  need_game_dir

  # Invariant: never apply without a snapshot of the original first. This auto-runs the (guarded)
  # backup — which is a no-op if a snapshot already exists, and aborts if it'd capture our own mod.
  if ! backup_exists; then
    say "No snapshot yet — taking one before the first apply."
    cmd_backup
  fi

  # Validate --with-mods names against the snapshot up front, BEFORE we touch the game folder, so a
  # typo can't leave a half-applied install. Build the list of names to copy here.
  local mod_names=()
  if [[ -n "$with_mods" ]]; then
    IFS=',' read -ra mod_names <<< "$with_mods"
    for name in "${mod_names[@]}"; do
      name="${name%.dll}"
      [[ -f "$BACKUP_DIR/mods/$name.dll" ]] || die "--with-mods: '$name.dll' not in snapshot ($BACKUP_DIR/mods/). Available: $(find "$BACKUP_DIR/mods" -maxdepth 1 -name '*.dll' -printf '%f ' 2>/dev/null)"
    done
  fi

  [[ $do_build -eq 1 ]] && build "$profile"

  local out; out="$(artifact_dir "$profile")"
  local dll="$out/unseamless_coop.dll" launcher="$out/start_protected_game.exe"
  [[ -f "$dll" ]]      || die "missing $dll — build first (drop --no-build)."
  [[ -f "$launcher" ]] || die "missing $launcher — build first (drop --no-build)."

  say "Installing our mod ($profile) into the game folder"
  cp -v "$dll" "$GAME_DIR/dinput8.dll"
  cp -v "$launcher" "$GAME_DIR/start_protected_game.exe"

  # mods/: rebuild from scratch each apply so the set is exactly what we asked for. Default = empty
  # (clean observation; the loader logs "no extra mods"). --with-mods pulls named mods out of the
  # snapshot (the .dll plus any companion subdir).
  rm -rf "$GAME_DIR/mods"
  mkdir -p "$GAME_DIR/mods"
  if [[ ${#mod_names[@]} -gt 0 ]]; then
    for name in "${mod_names[@]}"; do
      name="${name%.dll}"
      cp "$BACKUP_DIR/mods/$name.dll" "$GAME_DIR/mods/"
      [[ -d "$BACKUP_DIR/mods/$name" ]] && cp -r "$BACKUP_DIR/mods/$name" "$GAME_DIR/mods/"
      ok "test mod: $name.dll"
    done
  else
    ok "mods/ left empty (only our cdylib loads)"
  fi

  # Seed config: a known starting point with debug logging on. Don't clobber an edited one if asked.
  mkdir -p "$(dirname "$CONFIG_DST")"
  if [[ $keep_config -eq 1 && -f "$CONFIG_DST" ]]; then
    ok "kept existing config ($CONFIG_DST)"
  else
    cp "$SEED_CONFIG" "$CONFIG_DST"
    ok "wrote seed config ($CONFIG_DST) — [debug] enabled, password 'coop-test'"
  fi

  # Record what we applied so `status` is honest and `backup` knows our mod is installed.
  local sha; sha="$(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  { echo "profile: $profile"; echo "git: $sha"; echo "date: $(date -Is)"; echo "mods: ${with_mods:-<none>}"; } > "$MARKER"

  say "Applied. Launch with: scripts/rig.sh launch   (then: scripts/rig.sh log -f)"
  warn "This replaced your live ERSC + Elden Mod Loader setup. Run 'rig.sh restore' to get it back."
}

# ---- run helpers ---------------------------------------------------------------------------------
# Wait up to `$1` seconds for a NEW run log (newer than `$2`) whose framework has come up, then print
# the install/heartbeat lines. Returns 0 on success. Used by `launch --wait` and `cycle`, so the poll
# loop lives in one place instead of being hand-rolled each time.
wait_for_framework() {
  # Separate declarations: in a single `local`, $((SECONDS + timeout)) is expanded before the
  # `timeout=` assignment takes effect, so under `set -u` it would read timeout as unbound.
  local timeout="$1" before="${2:-}" log=""
  local deadline=$((SECONDS + timeout))
  say "Waiting up to ${timeout}s for the framework to come up (title screen is enough)…"
  while (( SECONDS < deadline )); do
    log="$(latest_log || true)"
    if [[ -n "$log" && "$log" != "$before" ]] \
       && grep -q "registered feature 'session-observer'" "$log" 2>/dev/null; then
      ok "framework is up — $log"
      grep -E "loaded config|wrote default config|extra mod|loaded mod|registered feature|override set|bridge listening|overlay:|observer" \
        "$log" | sed 's/^/      /'
      return 0
    fi
    sleep 3
  done
  warn "didn't see the framework come up within ${timeout}s."
  [[ -n "$log" ]] && say "  latest log: $log" || say "  (no new log written — did the game launch?)"
  return 1
}

# ---- re-blank the display after a launch wakes it -----------------------------------------------
# Is any screen currently DPMS-off (blanked)? Captured BEFORE a launch so we only re-blank a screen
# the user had already turned off — never one they're actively using. `kscreen-doctor --dpms show`
# prints "dpms mode for screen <out>: on|off" per output; treat any `off` as blanked. Best-effort:
# "can't tell" (no kscreen-doctor) reads as not-blanked, so we default to leaving the display alone.
display_is_blanked() {
  command -v kscreen-doctor >/dev/null 2>&1 || return 1
  kscreen-doctor --dpms show 2>/dev/null | grep -qiw off
}

# The screen was blanked before this launch and starting the game forced it back on. Put it back to
# sleep after RIG_REBLANK_DELAY seconds. Detached via setsid so it survives this script exiting (the
# no-wait `launch` path returns immediately); notifies first so a watcher knows why the panel is about
# to go dark, then `kscreen-doctor --dpms off` (the same call the Blank Screen tool makes).
schedule_reblank() {
  [[ "$RIG_REBLANK" == 1 ]] || return 0
  command -v kscreen-doctor >/dev/null 2>&1 || return 0
  setsid -f bash -c '
    sleep "$1"
    if command -v notify-send >/dev/null 2>&1; then
      notify-send -a unseamless-rig -u low -t 4000 \
        "rig: re-blanking display" "Screen was off before launch; the game woke it. Turning it back off." 2>/dev/null || true
    fi
    sleep 1                                 # let the toast render before the panel powers off
    kscreen-doctor --dpms off >/dev/null 2>&1 || true
  ' _ "$RIG_REBLANK_DELAY" >/dev/null 2>&1 || true
  say "display was blanked before launch — re-blanking in ${RIG_REBLANK_DELAY}s (RIG_REBLANK=0 to disable)"
}

cmd_launch() {
  local do_wait=0; [[ "${1:-}" == "--wait" ]] && do_wait=1
  need_game_dir
  applied || warn "our mod isn't applied (no marker) — launching whatever is currently installed."
  # Capture display-blank state BEFORE handing off to Steam: the launch wakes the panel, so we must
  # sample it now to know whether to put it back to sleep afterwards (schedule_reblank at the end).
  local was_blanked=0; display_is_blanked && was_blanked=1
  local before; before="$(latest_log || true)"  # capture BEFORE launch so --wait spots the new run
  # Tell gamescope-wrapper.sh (if it's your launch options) to render at the rig size this launch. The
  # wrapper consumes + deletes the flag; if you haven't switched to the wrapper yet it's just ignored.
  printf '%s %s\n' "$RIG_WINDOW_WIDTH" "$RIG_WINDOW_HEIGHT" > "$RIG_GS_FLAG"
  # Hide the window as it appears so the centered/loading window isn't seen before we place it. Only
  # when --wait (reposition below reveals it); reveal always runs, so it can't get stranded minimized.
  [[ $do_wait -eq 1 && "$RIG_HIDE_UNTIL_PLACED" == 1 ]] && hide_window_until_placed
  say "Launching ELDEN RING via Steam (appid $APPID; rig render size ${RIG_WINDOW_WIDTH}x${RIG_WINDOW_HEIGHT} via wrapper)"
  steam -applaunch "$APPID" >/dev/null 2>&1 &
  ok "handed off to Steam. Our launcher sets UNSEAMLESS_LAUNCH and starts the game outside EAC."
  if [[ $do_wait -eq 1 ]]; then
    wait_for_framework 150 "$before" || true  # reveal/place regardless, so the window is never stuck hidden
    rm -f "$RIG_GS_FLAG"                       # no-op if the wrapper already consumed it
    sleep 1                                    # let gamescope finish mapping its window
    reposition_window                          # reveal (unminimize) + move to top-left
  fi
  # If the screen was blanked before this launch, put it back to sleep (the game woke it). Anchored
  # here at the end so the timer starts after the window is up (--wait) and well clear of cycle's
  # post-launch popup-dismiss key injection, which would otherwise re-wake the panel.
  [[ $was_blanked -eq 1 ]] && schedule_reblank
}

latest_log() { ls -1t "$LOG_DIR"/unseamless_coop-*.log 2>/dev/null | head -1; }

# ---- window placement (KDE Plasma Wayland) ------------------------------------------------------
# KWin has no CLI to move/minimize windows; you load a tiny JS snippet over D-Bus and it runs inside
# the compositor. kwin_run loads + starts a script file (leaves it loaded); kwin_unload tears it down
# (also dropping any signal it connected). Best-effort — a no-op off KDE so the rig still runs elsewhere.
kwin_available() { command -v gdbus >/dev/null 2>&1; }
kwin_run() {  # $1 = .js file, $2 = plugin name
  kwin_available || return 1
  gdbus call --session --dest org.kde.KWin --object-path /Scripting \
    --method org.kde.kwin.Scripting.loadScript "$1" "$2" >/dev/null 2>&1 || true
  gdbus call --session --dest org.kde.KWin --object-path /Scripting \
    --method org.kde.kwin.Scripting.start >/dev/null 2>&1 || true
}
kwin_unload() {  # $1 = plugin name
  kwin_available || return 0
  gdbus call --session --dest org.kde.KWin --object-path /Scripting \
    --method org.kde.kwin.Scripting.unloadScript "$1" >/dev/null 2>&1 || true
}

# Plugin name of the hide watcher, so launch can load it and reposition_window can tear it down.
HIDE_PLUGIN="unseamless-rig-hide"

# Minimize the game window the instant it appears (and any already up), and keep watching so it stays
# hidden until reposition_window reveals + places it — no centered/loading window flashing first.
# Leaves the watcher loaded (it holds the windowAdded connection); reposition_window unloads it.
hide_window_until_placed() {
  kwin_available || return 0
  local js; js="$(mktemp /tmp/er-win-hide.XXXXXX.js)"
  cat > "$js" <<'EOF'
function hide(w) { if (w && w.resourceClass == "gamescope" && !w.minimized) w.minimized = true; }
const ws = (typeof workspace.windowList === "function") ? workspace.windowList() : workspace.clientList();
for (const w of ws) hide(w);
workspace.windowAdded.connect(hide);
EOF
  kwin_run "$js" "$HIDE_PLUGIN"
  rm -f "$js"
}

# Move the game window to the top-left and reveal it (if hide_window_until_placed minimized it). We
# only MOVE, never resize: gamescope already renders at the right size (gamescope-wrapper.sh sets it),
# and forcing a KWin resize makes gamescope scale its buffer into the new frame -> blurry text. gamescope
# centers its window and has no position flag / honors no KWin rule, so we set the position directly.
# Best-effort: a no-op note if gdbus/KWin aren't around. KDE Plasma 6 (Wayland).
reposition_window() {
  kwin_available || { warn "gdbus not found — skipping window reposition"; return 0; }
  local plugin="er-reposition-$$-$SECONDS" js stamp
  js="$(mktemp /tmp/er-win-move.XXXXXX.js)"
  cat > "$js" <<EOF
const ws = (typeof workspace.windowList === "function") ? workspace.windowList() : workspace.clientList();
for (const w of ws) {
  if (w.resourceClass == "gamescope") {
    const g = w.frameGeometry;
    w.minimized = false;  // reveal if the hide watcher had it minimized
    w.frameGeometry = { x: ${WINDOW_MARGIN}, y: ${WINDOW_MARGIN}, width: g.width, height: g.height };  // move only — keep gamescope's native size (no scaling/blur)
    print("ERMOVE | gamescope " + g.x + "," + g.y + " -> ${WINDOW_MARGIN},${WINDOW_MARGIN} (" + g.width + "x" + g.height + ")");
  }
}
EOF
  stamp="$(date '+%Y-%m-%d %H:%M:%S')"
  kwin_run "$js" "$plugin"
  sleep 0.6
  kwin_unload "$plugin"
  kwin_unload "$HIDE_PLUGIN"  # stop holding the window minimized now that it's placed + revealed
  rm -f "$js"
  # KWin script print() lands in the journal; use it to report whether a gamescope window was found.
  if journalctl --user -t kwin_wayland --since "$stamp" --no-pager 2>/dev/null | grep -q "ERMOVE"; then
    ok "window placed top-left (margin ${WINDOW_MARGIN}px)"
  else
    warn "no gamescope window found yet (give it a moment, then: rig.sh reposition)"
  fi
}

# ---- startup-popup auto-dismiss (ydotool) -------------------------------------------------------
# The offline-mode + connection-error popups can't be killed in code (Arxan-hardened path, parked —
# docs/OFFLINE-TITLE-SCREEN.md), so for unattended solo runs we just click through them: inject the
# menu-confirm key into the focused game window. ydotool injects at the uinput level (a virtual
# input device), so the gamescope window must be focused — focus_game_window handles that. Linux
# input event codes: Enter = 28, E = 18 (ER's keyboard menu-accept), so we send both to cover
# whichever a given dialog wants.
ydo() { YDOTOOL_SOCKET="$RIG_YDOTOOL_SOCKET" ydotool "$@"; }

# Raise + focus the gamescope window so injected keys land in the game, not the terminal. Returns 0
# if a gamescope window was found and activated, 1 if not — so the caller can refuse to inject keys
# into whatever else has focus. Off KDE (no gdbus) we can't enumerate windows, so return 0 and let the
# caller proceed (best-effort, same posture as reposition_window). Same KWin-over-D-Bus mechanism, and
# we detect the match via the script's print() landing in the journal (like reposition_window).
focus_game_window() {
  kwin_available || return 0
  local js plugin="er-focus-$$-$SECONDS" stamp
  js="$(mktemp /tmp/er-win-focus.XXXXXX.js)"
  cat > "$js" <<'EOF'
const ws = (typeof workspace.windowList === "function") ? workspace.windowList() : workspace.clientList();
for (const w of ws) if (w.resourceClass == "gamescope") { w.minimized = false; workspace.activeWindow = w; print("ERFOCUS | gamescope"); }
EOF
  stamp="$(date '+%Y-%m-%d %H:%M:%S')"
  kwin_run "$js" "$plugin"; sleep "$RIG_DISMISS_FOCUS_SETTLE"; kwin_unload "$plugin"; rm -f "$js"
  journalctl --user -t kwin_wayland --since "$stamp" --no-pager 2>/dev/null | grep -q "ERFOCUS"
}

cmd_dismiss() {
  local presses="${1:-$RIG_DISMISS_PRESSES}"
  # Degrade (warn + return), never `die`: cmd_cycle calls us with `|| warn` to keep going, and `die`
  # exits the whole script (can't be trapped by `||`), which would abort cycle after the game launched.
  command -v ydotool >/dev/null 2>&1 || { warn "ydotool not installed (pacman -S ydotool; enable ydotoold) — skipping auto-dismiss"; return 1; }
  [[ "$presses" =~ ^[0-9]+$ ]] || { warn "dismiss: press count must be a number, got '$presses'"; return 1; }
  [[ -S "$RIG_YDOTOOL_SOCKET" ]] || warn "ydotool socket $RIG_YDOTOOL_SOCKET missing — is the ydotoold user service running?"
  pgrep -f '[e]ldenring.exe' >/dev/null || warn "eldenring.exe doesn't look like it's running yet"
  local refocus="$RIG_DISMISS_REFOCUS_EVERY"
  [[ "$refocus" =~ ^[0-9]+$ && "$refocus" -gt 0 ]] || refocus=1
  say "Dismissing startup popups: $presses confirm presses (Enter, ${RIG_DISMISS_INTERVAL}s apart, re-focusing every ${refocus})…"
  local i focused=0 start now elapsed
  start="${EPOCHREALTIME/,/.}"
  for ((i = 0; i < presses; i++)); do
    # The popups are *in-engine* dialogs, not separate OS windows, so focus can't be stolen between
    # presses — we re-assert it on the first press and every Nth after (insurance against KWin
    # reshuffling), not every single press. Only inject once we actually hold the game window.
    if (( i % refocus == 0 )); then
      if focus_game_window; then
        focused=1
      elif (( focused == 0 )); then
        warn "couldn't focus the gamescope window — skipping this press"
        notify "rig: dismissing popups" "press $((i + 1))/$presses skipped — no game-window focus" 1500
        sleep "$RIG_DISMISS_INTERVAL"
        continue
      fi
    fi
    (( focused == 0 )) && { sleep "$RIG_DISMISS_INTERVAL"; continue; }
    ydo key 28:1 28:0 || { warn "ydotool failed — is ydotoold running and YDOTOOL_SOCKET correct?"; return 1; }
    now="${EPOCHREALTIME/,/.}"
    elapsed="$(LC_ALL=C awk -v a="$start" -v b="$now" 'BEGIN { printf "%.1f", b - a }')"
    notify "rig: dismissing popups" "press $((i + 1))/$presses · ${elapsed}s elapsed"
    sleep "$RIG_DISMISS_INTERVAL"
  done
  if [[ $focused -eq 0 ]]; then
    warn "never managed to focus the game window — nothing dismissed; clear popups manually or: scripts/rig.sh dismiss"
    notify "rig: dismiss failed" "never got game-window focus — nothing sent" 3000
    return 1
  fi
  focus_game_window || true     # one more focus before the fallback key
  ydo key 18:1 18:0 || true     # one E too, in case a dialog wants the menu-accept key over Enter
  now="${EPOCHREALTIME/,/.}"
  elapsed="$(LC_ALL=C awk -v a="$start" -v b="$now" 'BEGIN { printf "%.1f", b - a }')"
  notify "rig: popups dismissed" "$presses presses over ${elapsed}s — re-run dismiss if one lingers" 2500
  ok "sent ($presses presses, ${elapsed}s). If a popup is still up, run: scripts/rig.sh dismiss"
}

# ---- seed the test save from a real save (rig.sh seed-save) ------------------------------------
# Resolve the Elden Ring save directory (…/EldenRing/<SteamID64>). Honors SAVE_DIR; otherwise derives
# it from the Steam library holding GAME_DIR. Prints the path on success, returns 1 if not found.
resolve_save_dir() {
  if [[ -n "$SAVE_DIR" ]]; then printf '%s\n' "$SAVE_DIR"; return 0; fi
  local steamapps root sub
  steamapps="$(cd "$GAME_DIR/../../.." && pwd)" || return 1   # …/common/ELDEN RING/Game -> …/steamapps
  root="$steamapps/compatdata/$APPID/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing"
  [[ -d "$root" ]] || return 1
  # One numeric SteamID64 subdir holds the saves; take the first (typically the only one).
  sub="$(find "$root" -mindepth 1 -maxdepth 1 -type d -regex '.*/[0-9]+' 2>/dev/null | head -n1)"
  [[ -n "$sub" ]] || return 1
  printf '%s\n' "$sub"
}

# The save extension the installed config redirects to (what a rig run reads/writes) — falls back to
# the seed config, then "uco".
configured_save_ext() {
  local f ext
  for f in "$CONFIG_DST" "$SEED_CONFIG"; do
    [[ -f "$f" ]] || continue
    ext="$(sed -nE 's/^[[:space:]]*file_extension[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$f" | head -n1)"
    [[ -n "$ext" ]] && { printf '%s\n' "$ext"; return 0; }
  done
  printf 'uco\n'
}

# Copy a real save into the rig's isolated test extension so you can test on a real character. Source
# extension is the arg (default `co2`, this machine's real ERSC co-op save); destination is whatever
# the installed config redirects to (e.g. `uco`). Backs up the existing test save first, and never
# touches the source or the vanilla `.sl2`. Game must be closed.
cmd_seed_save() {
  local src_ext="${1:-co2}"
  local dst_ext; dst_ext="$(configured_save_ext)"
  [[ "$dst_ext" == "sl2" ]] && die "configured save ext is 'sl2' (vanilla) — refusing to overwrite your single-player save"
  [[ "$src_ext" == "sl2" ]] && die "refusing to copy *from* the vanilla 'sl2' — pass a co-op extension (e.g. co2)"
  [[ "$src_ext" == "$dst_ext" ]] && die "source and destination extension are both '$src_ext' — nothing to do"
  pgrep -f '[e]ldenring.exe' >/dev/null && die "Elden Ring is running — close it before seeding the save"
  local dir; dir="$(resolve_save_dir)" || die "couldn't find the Elden Ring save folder (set SAVE_DIR=…)"
  local src="$dir/ER0000.$src_ext"
  [[ -f "$src" ]] || die "source save not found: $src"
  local stamp; stamp="$(date +%Y%m%d-%H%M%S)"
  say "Seeding test save in $dir"
  say "  ER0000.$src_ext  ->  ER0000.$dst_ext   (+ .bak)"
  # Back up the existing destination (the previous test save) before overwriting; never touch src/.sl2.
  local f
  for f in "ER0000.$dst_ext" "ER0000.$dst_ext.bak"; do
    [[ -f "$dir/$f" ]] && { cp -p "$dir/$f" "$dir/$f.seedbak-$stamp"; ok "backed up $f -> $f.seedbak-$stamp"; }
  done
  cp -p "$src" "$dir/ER0000.$dst_ext"; ok "copied ER0000.$src_ext -> ER0000.$dst_ext"
  if [[ -f "$src.bak" ]]; then
    cp -p "$src.bak" "$dir/ER0000.$dst_ext.bak"; ok "copied ER0000.$src_ext.bak -> ER0000.$dst_ext.bak"
  fi
  ok "done. The rig build (file_extension=\"$dst_ext\") will now load this character."
}

cmd_log() {
  local follow=0; [[ "${1:-}" == "-f" ]] && follow=1
  local log; log="$(latest_log || true)"
  [[ -n "$log" ]] || die "no run log yet under $LOG_DIR (launch the game first)."
  say "Latest log: $log"
  if [[ $follow -eq 1 ]]; then tail -f "$log"; else cat "$log"; fi
}

cmd_kill() {
  # The game + our launcher run under Wine/Proton, where SIGTERM is routinely ignored — so escalate
  # to SIGKILL and verify, instead of leaving stragglers that the next launch trips over. Kills both
  # the game and the launcher. Bracket trick so pkill doesn't match its own command line.
  rm -f "$RIG_GS_FLAG"  # clear any stale rig-size flag so the next manual launch is fullscreen
  local procs=('[e]ldenring.exe' '[s]tart_protected_game')
  if ! pgrep -f '[e]ldenring.exe' >/dev/null && ! pgrep -f '[s]tart_protected_game' >/dev/null; then
    warn "game not running"
    return 0
  fi
  for p in "${procs[@]}"; do pkill -f "$p" 2>/dev/null || true; done          # SIGTERM
  for _ in 1 2 3; do pgrep -f '[e]ldenring.exe' >/dev/null || break; sleep 1; done
  for p in "${procs[@]}"; do pkill -9 -f "$p" 2>/dev/null || true; done        # SIGKILL stragglers
  sleep 1
  if pgrep -f '[e]ldenring.exe' >/dev/null; then
    warn "eldenring.exe STILL running after SIGKILL — investigate"
  else
    ok "game stopped"
  fi
}

# ---- status --------------------------------------------------------------------------------------
cmd_status() {
  need_game_dir
  say "Game folder: $GAME_DIR"
  if backup_exists; then
    ok "snapshot: present ($(sed -n 's/^date: //p' "$BACKUP_DIR/MANIFEST")) at $BACKUP_DIR"
  else
    warn "snapshot: NONE — first 'apply' (or 'backup') will create it"
  fi
  if applied; then
    ok "installed: unseamless-coop"
    sed 's/^/      /' "$MARKER"
  else
    ok "installed: original stack (our mod not applied)"
  fi
  printf '      dinput8.dll: %s\n' "$(stat -c '%s bytes, %y' "$GAME_DIR/dinput8.dll" 2>/dev/null || echo missing)"
  printf '      mods/: %s\n' "$(find "$GAME_DIR/mods" -maxdepth 1 -name '*.dll' -printf '%f ' 2>/dev/null || echo '(none)')"
  local log; log="$(latest_log || true)"
  [[ -n "$log" ]] && printf '      latest log: %s (%s)\n' "$log" "$(stat -c '%y' "$log")" || printf '      latest log: none\n'
}

# ---- cycle: solo smoke test ----------------------------------------------------------------------
# apply -> launch -> wait for the framework's install/heartbeat lines -> show them. This is the
# layer-4 "does the DLL load, register, and tick" check from the test-loop skill; it does NOT need a
# save (FrameBegin ticks at the title screen). Leaves the game running so you can drive an
# observation run; 'rig.sh kill' when done.
# Launch with --wait (block until the framework's install/heartbeat lines appear), then unless
# dismiss=0 clear the offline-mode / connection-error startup popups so the game lands ready to play
# instead of stuck behind intros. Shared by `cycle` and `friend-test`.
launch_and_dismiss() {
  local dismiss="${1:-1}"
  if cmd_launch --wait; then
    if [[ $dismiss -eq 1 ]]; then
      sleep 2          # give the offline/connection popups a moment to appear after the title
      cmd_dismiss || warn "auto-dismiss failed; clear the popups manually or: scripts/rig.sh dismiss"
    fi
    say "Game is running. Drive your test, then: scripts/rig.sh log -f  /  scripts/rig.sh kill"
  fi
}

cmd_cycle() {
  # Pull our own flags out of the arg list before forwarding the rest to apply (which would `die` on
  # an unknown option). `--no-dismiss` leaves the startup popups for you to clear manually.
  local dismiss=1 args=()
  for a in "$@"; do
    case "$a" in
      --no-dismiss) dismiss=0 ;;
      *) args+=("$a") ;;
    esac
  done
  cmd_apply ${args[@]+"${args[@]}"}
  launch_and_dismiss "$dismiss"
}

# ---- friend-bundle packaging ---------------------------------------------------------------------
# Re-derive the same build id build.rs bakes into the DLL (short HEAD sha + `-dirty` when the tree
# has uncommitted changes), so the zip name and MANIFEST match the binary that's in it.
compute_build_id() {
  local sha dirty
  sha="$(cd "$ROOT" && git rev-parse --short=7 HEAD 2>/dev/null || echo nogit)"
  dirty="$(cd "$ROOT" && git status --porcelain 2>/dev/null)"
  [[ -n "$dirty" ]] && sha="${sha}-dirty"
  printf '%s' "$sha"
}

# Workspace version (the first `version = "..."` in the root Cargo.toml — the [workspace.package] one).
workspace_version() { sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1; }

# Resolve the shared co-op password (arg > env > file), validating the mod's 5-char minimum.
resolve_password() {
  local pw="${1:-}"
  [[ -z "$pw" ]] && pw="${UNSEAMLESS_SHARED_PASSWORD:-}"
  [[ -z "$pw" && -f "$SHARED_PASSWORD_FILE" ]] && pw="$(tr -d '[:space:]' < "$SHARED_PASSWORD_FILE")"
  [[ -n "$pw" ]] || die "no shared password. Pass --password X, set UNSEAMLESS_SHARED_PASSWORD, or write one to $SHARED_PASSWORD_FILE"
  # 5 is unseamless_core::config::MIN_PASSWORD_LEN — a fail-fast so we don't ship a config the mod
  # rejects at startup on a friend's machine; the runtime guard (password_is_valid) stays authoritative.
  [[ ${#pw} -ge 5 ]] || die "shared password must be >= 5 characters (the mod rejects shorter)."
  # Restrict to a safe charset: the seed config is written via an UNquoted heredoc (so $build_id et al.
  # expand) and as a TOML basic string, so a `$`, backtick, or `\` would shell-expand/execute and a `"`
  # would break the TOML. The mod's own generated passwords are [A-Z0-9], so this never rejects a real one.
  [[ "$pw" =~ ^[A-Za-z0-9._~!@#%^*()+=-]+$ ]] \
    || die "shared password has an unsupported character. Use letters, digits, and . _ ~ ! @ # % ^ * ( ) + = - (no spaces, quotes, backslash, backtick, or \$)."
  printf '%s' "$pw"
}

cmd_package() {
  local profile=diag do_apply=0 pw_arg="" guide_arg="" probe=0 auto_arg="" no_overlay=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --release)    profile=release ;;
      --diag)       profile=diag ;;
      --apply)      do_apply=1 ;;
      --password)   pw_arg="${2:-}"; shift ;;
      --password=*) pw_arg="${1#*=}" ;;
      --guide)      guide_arg="${2:-}"; shift ;;   # bake a rig-guide name into the bundle config
      --guide=*)    guide_arg="${1#*=}" ;;
      --session-probe) probe=1 ;;                  # bake [debug.probes] session_probe = true
      --auto-session)   auto_arg="${2:-}"; shift ;;  # bake [debug] auto_session = host|join
      --auto-session=*) auto_arg="${1#*=}" ;;
      --no-overlay) no_overlay=1 ;;                # bake [debug] overlay = false (headless machine)
      *) die "package: unknown option '$1'" ;;
    esac
    shift
  done
  command -v zip >/dev/null 2>&1 || die "zip not installed (pacman -S zip)."
  local password version; password="$(resolve_password "$pw_arg")"; version="$(workspace_version)"

  # Refresh the baked build_id's dirty flag — build.rs only re-runs on a commit move otherwise — then
  # build WITHOUT the bridge feature (that loopback debug listener is dev-host-only, not for friends).
  touch "$ROOT/crates/unseamless-coop/build.rs"
  say "Building friend bundle ($profile, no bridge) for $TRIPLE"
  ( cd "$ROOT" && cargo build --profile "$profile" )

  local build_id out dll launcher
  build_id="$(compute_build_id)"
  out="$(artifact_dir "$profile")"
  dll="$out/unseamless_coop.dll"; launcher="$out/start_protected_game.exe"
  [[ -f "$dll" && -f "$launcher" ]] || die "build artifacts missing under $out"

  local stage="$DIST_DIR/staging"
  rm -rf "$stage"; mkdir -p "$stage"
  cp "$dll" "$stage/dinput8.dll"
  cp "$launcher" "$stage/start_protected_game.exe"
  for f in Install.cmd Install.ps1 Uninstall.cmd Uninstall.ps1 _lib.ps1 README-FRIENDS.txt; do
    cp "$DIST_SRC/$f" "$stage/$f"
  done

  # Shared seed config: only what friends must have set — the password and an isolated save extension
  # — plus debug logging + forwarding so their logs come back to the host. Everything else stays at
  # the mod's defaults; the host's ConfigSync pushes the authoritative settings live once connected.
  cat > "$stage/unseamless_coop.toml" <<EOF
# unseamless-coop shared test config (build $build_id). Everyone in the party uses this so the
# co-op password matches; the host syncs the rest of the settings to you live once connected.

[session]
password = "$password"

[save]
# Isolated from vanilla (.sl2) and ERSC (.co2) so your real saves are never touched.
file_extension = "$FRIEND_SAVE_EXT"

[debug]
enabled = true          # capture verbose logs for this test build
forward_to_host = true  # send your logs to the host so they land in one place
EOF
  # Optional: bake a rig-guide name (--guide) so every machine runs the same on-screen test flow, and
  # the rung-3 FSM probe (--session-probe) so create/join transitions land in the log. rig_role is left
  # at the default (solo) — two-player-join derives each machine's role from its Open/Join action.
  # [debug] keys first (guide / auto_session / overlay), then the [debug.probes] subsection.
  [[ -n "$guide_arg" ]] && printf 'guide = "%s"\n' "$guide_arg" >> "$stage/unseamless_coop.toml"
  [[ -n "$auto_arg" ]] && printf 'auto_session = "%s"\n' "$auto_arg" >> "$stage/unseamless_coop.toml"
  [[ $no_overlay -eq 1 ]] && printf 'overlay = false\n' >> "$stage/unseamless_coop.toml"
  if [[ $probe -eq 1 ]]; then
    printf '\n[debug.probes]\nsession_probe = true\n' >> "$stage/unseamless_coop.toml"
  fi

  # MANIFEST: build id + sha256 of each binary, for the installer's post-copy verification.
  {
    echo "# unseamless-coop friend bundle"
    echo "build_id: $build_id"
    echo "version: $version"
    echo "created: $(date -Is)"
    echo "save_extension: $FRIEND_SAVE_EXT"
    echo "files:"
    printf '  %s  %s\n' "$(sha256sum "$stage/dinput8.dll" | cut -d' ' -f1)" "dinput8.dll"
    printf '  %s  %s\n' "$(sha256sum "$stage/start_protected_game.exe" | cut -d' ' -f1)" "start_protected_game.exe"
  } > "$stage/MANIFEST.txt"

  local zipf="$DIST_DIR/unseamless-coop-$build_id.zip"
  rm -f "$zipf"
  # Encrypt the bundle (ZipCrypto via `zip -P`) so browsers/AV can't scan the .exe inside and flag a
  # "unsafe download" false positive. Not security — the password is published in the share notes; it
  # just gates the scanner. Windows Explorer extracts it with a password prompt, so friends need no
  # extra tool. (-P puts the password on the command line, fine on this single-user host.)
  ( cd "$stage" && zip -qr -P "$FRIEND_ZIP_PASSWORD" "$zipf" . )
  ok "packaged $zipf (password-protected: '$FRIEND_ZIP_PASSWORD')"
  say "build_id $build_id · v$version · save ext .$FRIEND_SAVE_EXT · coop password ${#password} chars · zip password '$FRIEND_ZIP_PASSWORD'"
  [[ "$build_id" == *-dirty ]] && warn "this is a -dirty build (uncommitted changes) — fine for testing; commit first if you want a clean id to share."

  # --apply: install the SAME built bits locally (host side) with the shared config, so the host runs
  # exactly what friends do. Reuses the guarded apply (auto-snapshots, never restores).
  if [[ $do_apply -eq 1 ]]; then
    say "Installing the same build locally (host) with the shared config"
    mkdir -p "$(dirname "$CONFIG_DST")"
    cp "$stage/unseamless_coop.toml" "$CONFIG_DST"
    local pflag=--diag; [[ "$profile" == release ]] && pflag=--release
    cmd_apply --no-build "$pflag" --keep-config
  fi
  rm -rf "$stage"
}

cmd_share() {
  local zipf="${1:-}"
  command -v gh >/dev/null 2>&1 || die "gh (GitHub CLI) not installed."
  if [[ -z "$zipf" ]]; then
    zipf="$(ls -1t "$DIST_DIR"/unseamless-coop-*.zip 2>/dev/null | head -1)"
    [[ -n "$zipf" ]] || die "no zip in $DIST_DIR — run 'rig.sh package' first (or pass a path)."
  fi
  [[ -f "$zipf" ]] || die "no such zip: $zipf"
  say "Sharing $(basename "$zipf") to GitHub prerelease '$SHARE_TAG'"
  # Delete-and-recreate the prerelease on every share so it re-pins to the top of GitHub's Releases
  # list (which orders by publish date — reusing the release would freeze its position there while
  # version releases pile up above it, burying the freshest build). Cleaning the tag too makes the
  # recreate point at current main and start asset-free, so no stale bundles or "Source code" archives
  # linger from a prior build.
  if ( cd "$ROOT" && gh release view "$SHARE_TAG" >/dev/null 2>&1 ); then
    say "Replacing existing prerelease '$SHARE_TAG'"
    # Keep gh's stderr (only stdout is silenced): this is the one step that destroys remote state, so a
    # failure cause (auth, rate-limit, partial --cleanup-tag) must reach the terminal, not be swallowed.
    ( cd "$ROOT" && gh release delete "$SHARE_TAG" --cleanup-tag --yes >/dev/null ) \
      || die "couldn't delete existing prerelease '$SHARE_TAG'"
  fi
  say "Creating prerelease '$SHARE_TAG'"
  # If this fails the old release is already gone (deleted just above): say so, and that a rerun recovers
  # it — the source zip is still in dist/, so 'rig.sh share' recreates the release from scratch.
  ( cd "$ROOT" && gh release create "$SHARE_TAG" --prerelease --target main \
      --title "$SHARE_TAG" \
      --notes "Rolling bucket of test builds for co-op testing. Not a real release.

Download the newest asset below, then extract it with the password **\`$FRIEND_ZIP_PASSWORD\`** and follow README-FRIENDS.txt inside. (The zip is password-protected only so browsers and antivirus don't throw a false-positive \"unsafe download\" warning on the .exe; it isn't a secret. Windows Explorer will just prompt you for the password when you extract.)" ) \
    || die "couldn't create prerelease '$SHARE_TAG' (the old one was already deleted; re-run 'rig.sh share' to recreate it — your dist/ zip is intact)"

  ( cd "$ROOT" && gh release upload "$SHARE_TAG" "$zipf" --clobber ) || die "upload failed"
  ok "uploaded $(basename "$zipf")"
  # Share the release *page* URL, not the asset's direct-download link. A direct .zip link trips
  # browser/AV "unsafe download" false-positives; the release page lets friends download from GitHub's
  # own UI (and reads the extract-password note in the body). This is the html_url gh reports for the
  # release, e.g. https://github.com/<owner>/<repo>/releases/tag/$SHARE_TAG.
  local url; url="$(cd "$ROOT" && gh release view "$SHARE_TAG" --json url --jq .url 2>/dev/null || true)"
  if [[ -n "$url" ]]; then
    say "Release page: $url"
    # Copy the page URL to the clipboard by default, so it's ready to paste to friends.
    if clip_copy "$url"; then ok "copied release link to clipboard"; else warn "no clipboard tool (wl-copy/xclip/xsel) — URL not copied"; fi
  else
    warn "couldn't resolve the release page URL (the release exists; open it with: gh release view $SHARE_TAG --web)"
  fi
}

# All-in-one friend-test loop: build + package a debug bundle of the current code, install those same
# bits locally (host) with the shared config, push the zip to the GitHub prerelease (copying the
# release-page link to your clipboard — `share` does that by default), then launch the game and clear the
# startup popups (like `cycle`, so it lands ready to play instead of stuck behind intros). The host
# rig and the friend zip run the same co-op password because `package --apply` resolves it once and
# writes the one seed config to both (cycle's own apply would write a default config and break that,
# which is why this packages-then-applies rather than calling `cycle`). Package opts pass through
# (--release, --password X); --no-launch stops before starting the game; --no-dismiss leaves the
# popups for you.
cmd_friend_test() {
  local do_launch=1 dismiss=1 pkg_args=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --no-launch)  do_launch=0 ;;
      --no-dismiss) dismiss=0 ;;
      *) pkg_args+=("$1") ;;
    esac
    shift
  done
  # Seed a stable default password the first time so the loop is truly zero-config; everything after
  # reuses it (and so do friends' bundles), and you can edit the file to change it. Only kicks in when
  # neither the env var nor the file already supplies one — never overrides an explicit choice.
  if [[ -z "${UNSEAMLESS_SHARED_PASSWORD:-}" && ! -f "$SHARED_PASSWORD_FILE" ]]; then
    mkdir -p "$(dirname "$SHARED_PASSWORD_FILE")"
    printf 'coop-test\n' > "$SHARED_PASSWORD_FILE"
    say "seeded default co-op password 'coop-test' -> $SHARED_PASSWORD_FILE (edit to change)"
  fi
  cmd_package --apply ${pkg_args[@]+"${pkg_args[@]}"}
  cmd_share
  if [[ $do_launch -eq 1 ]]; then
    launch_and_dismiss "$dismiss"   # launch --wait + auto-clear startup popups (like `cycle`)
  else
    say "skipping launch (--no-launch)"
  fi
}

# ---- dispatch ------------------------------------------------------------------------------------
usage() {
  cat <<'EOF'
rig.sh — drive the local Elden Ring rig for unseamless-coop testing.

  backup                 One-time snapshot of the current ERSC + Elden Mod Loader install.
                         Guarded & idempotent; this is your rollback point.
  apply [opts]           Build + install our mod over the original. Safe to repeat; auto-snapshots
                         first if needed. Never restores.
        --release          Build/install the shipping profile (default: diag, with symbols).
        --no-build         Install whatever's already in target/ (skip cargo build).
        --with-mods a,b    Also load these mods (by name) from the snapshot — for loader testing.
                           Default: mods/ is left empty (clean observation run).
        --keep-config      Don't overwrite an existing on-disk config (default: write the seed).
  restore                EXPLICIT rollback to the original stack. The only thing that un-applies.
  status                 Show snapshot state, what's installed, and the latest run log.
  launch [--wait]        steam -applaunch (uses your gamescope launch options). Renders at the rig
                         size if you've set launch options to scripts/rig/gamescope-wrapper.sh.
                         --wait blocks until the framework comes up and prints the install lines.
  log [-f]               Print (or -f follow) the latest run log.
  kill                   Stop the game (pkill eldenring.exe).
  reposition             Move the running game window to the top-left (and reveal it if hidden). Only
                         moves, never resizes (size comes from gamescope-wrapper.sh, so no scaling
                         blur). Auto-run by 'launch --wait' / 'cycle'; run it again here if needed.
  seed-save [src-ext]    Copy a real save into the rig's isolated test extension so you can test on a
                         real character. src-ext defaults to 'co2' (this machine's real ERSC save);
                         destination is whatever the installed config redirects to (e.g. 'uco').
                         Backs up the existing test save first; never touches the source or vanilla
                         '.sl2'. NOT a per-run step — the test save already in place is usually fine,
                         so only run this on initial apply or when you want to reset/refresh it.
                         Game must be closed.
  dismiss [N]            Click through the startup popups (offline-mode / connection-error) by
                         injecting N confirm presses (default 8) into the focused game window via
                         ydotool, re-focusing the window before each. Run if a popup is still up.
  cycle [apply-opts]     apply -> launch -> wait for the install/heartbeat lines (solo smoke test).
                         Auto-dismisses the startup popups; pass --no-dismiss to skip that.
  package [opts]         Build + assemble a Windows friend-install zip in dist/. Bundles our binaries,
                         a shared seed config, a sha256 MANIFEST, and the PowerShell installer. The zip
                         is password-protected ('$FRIEND_ZIP_PASSWORD') to dodge browser/AV false-positives.
        --release          Ship the lean profile (default: diag — symbols + the build id in-overlay).
        --password X       Shared co-op password (else $UNSEAMLESS_SHARED_PASSWORD, else the file
                           scripts/dist/.shared-password). Must be >= 5 chars.
        --apply            Also install the same build locally (host) with the shared config.
  share [zip]            Upload a packaged zip (default: newest in dist/) to the GitHub prerelease
                         '$SHARE_TAG', recreating it fresh each time so it re-pins to the top of the
                         Releases list. Copies the release-PAGE URL to your clipboard (not a direct
                         download link) and publishes the extract password in the release notes.
  friend-test [opts]     All-in-one: package + apply a debug build, share it (link to clipboard), then
                         launch and auto-clear the startup popups (like 'cycle', lands ready to play).
                         Host and bundle share one co-op password (seeds a default 'coop-test' on first
                         run). Passes package opts through (--release, --password X); --no-launch stops
                         before starting the game; --no-dismiss leaves the popups for you.

Env overrides: GAME_DIR, BACKUP_DIR, APPID, SAVE_DIR, WINDOW_MARGIN, RIG_WINDOW_WIDTH,
               RIG_WINDOW_HEIGHT, RIG_HIDE_UNTIL_PLACED, RIG_YDOTOOL_SOCKET, RIG_DISMISS_PRESSES,
               RIG_DISMISS_INTERVAL, DIST_DIR, FRIEND_SAVE_EXT, SHARED_PASSWORD_FILE, SHARE_TAG.
EOF
}

cmd="${1:-}"; shift || true
case "$cmd" in
  backup)  cmd_backup "$@" ;;
  apply)   cmd_apply "$@" ;;
  restore) cmd_restore "$@" ;;
  status)  cmd_status "$@" ;;
  launch)  cmd_launch "$@" ;;
  log)     cmd_log "$@" ;;
  kill)    cmd_kill "$@" ;;
  reposition) reposition_window ;;
  seed-save) cmd_seed_save "$@" ;;
  dismiss) cmd_dismiss "$@" ;;
  cycle)   cmd_cycle "$@" ;;
  package) cmd_package "$@" ;;
  share)   cmd_share "$@" ;;
  friend-test) cmd_friend_test "$@" ;;
  ""|-h|--help|help) usage ;;
  *) die "unknown command '$cmd' (try: rig.sh help)" ;;
esac
