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

# ---- paths -------------------------------------------------------------------------------------
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GAME_DIR="${GAME_DIR:-/mnt/games/SteamLibrary/steamapps/common/ELDEN RING/Game}"
BACKUP_DIR="${BACKUP_DIR:-$HOME/.local/share/unseamless-coop/rig-backup}"
APPID="${APPID:-1245620}"
TRIPLE="x86_64-pc-windows-gnu"
SEED_CONFIG="$ROOT/scripts/rig/seed-config.toml"
# Where/how big the game runs during a rig launch. The point: keep your Steam launch options set to
# your *gaming* config and still rig in a small, low-res window. Two cooperating pieces:
#   - render resolution: scripts/rig/gamescope-wrapper.sh (set as your launch options) reads RIG_GS_FLAG
#     at launch and runs gamescope at the rig size via -w/-h (a real lower render res). cmd_launch
#     writes that flag; the wrapper consumes + deletes it, so manual launches stay fullscreen.
#   - window placement: reposition_window then nudges that window to the top-left.
# 1720x720 keeps the 3440x1440 (21:9) aspect. Override any of these via env.
WINDOW_MARGIN="${WINDOW_MARGIN:-24}"
RIG_WINDOW_WIDTH="${RIG_WINDOW_WIDTH:-1720}"
RIG_WINDOW_HEIGHT="${RIG_WINDOW_HEIGHT:-720}"
# One-shot flag handed to gamescope-wrapper.sh. MUST match the wrapper's FLAG (same env, same default).
RIG_GS_FLAG="${UNSEAMLESS_RIG_GAMESCOPE_FLAG:-${XDG_RUNTIME_DIR:-/tmp}/unseamless-rig-gamescope}"
# Minimize the game window the moment it appears and keep it hidden until reposition_window places it,
# so you don't see the centered/loading window before it snaps to the corner. Set 0 to disable (e.g.
# if a minimized window ever throttles the load). The reveal always runs, so this can't strand it.
RIG_HIDE_UNTIL_PLACED="${RIG_HIDE_UNTIL_PLACED:-1}"
# Auto-dismiss the startup popups (the offline-mode / connection-error dialogs we can't suppress in
# code — they're Arxan-hardened, see docs/OFFLINE-TITLE-SCREEN.md) by injecting key presses via
# ydotool. `cycle` does this by default so a solo smoke test lands at the menu unattended; opt out
# with `cycle --no-dismiss`, or trigger it yourself with `rig.sh dismiss`.
RIG_YDOTOOL_SOCKET="${YDOTOOL_SOCKET:-${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/.ydotool_socket}"
# How many confirm presses `dismiss` sends (one per popup + 'press any button' + a little slack).
RIG_DISMISS_PRESSES="${RIG_DISMISS_PRESSES:-6}"

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
  : > "$BACKUP_DIR/MANIFEST"
  {
    echo "# unseamless-coop rig backup — the original ERSC + Elden Mod Loader stack."
    echo "date: $(date -Is)"
    echo "source: $GAME_DIR"
    echo "files:"
  } >> "$BACKUP_DIR/MANIFEST"

  for f in "${MANAGED_FILES[@]}"; do
    if [[ -f "$GAME_DIR/$f" ]]; then
      cp -p "$GAME_DIR/$f" "$BACKUP_DIR/$f"
      printf '  %s  %s\n' "$(sha256sum "$GAME_DIR/$f" | cut -d' ' -f1)" "$f" >> "$BACKUP_DIR/MANIFEST"
      ok "saved $f"
    else
      warn "no $f in game folder (skipping)"
    fi
  done

  if [[ -d "$GAME_DIR/mods" ]]; then
    rsync -a --delete "$GAME_DIR/mods/" "$BACKUP_DIR/mods/"
    echo "  mods/ ($(find "$BACKUP_DIR/mods" -maxdepth 1 -name '*.dll' | wc -l | tr -d ' ') dll(s))" >> "$BACKUP_DIR/MANIFEST"
    ok "saved mods/ ($(find "$BACKUP_DIR/mods" -maxdepth 1 -name '*.dll' -printf '%f ' 2>/dev/null))"
  fi
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

cmd_launch() {
  local do_wait=0; [[ "${1:-}" == "--wait" ]] && do_wait=1
  need_game_dir
  applied || warn "our mod isn't applied (no marker) — launching whatever is currently installed."
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
  kwin_run "$js" "$plugin"; sleep 0.5; kwin_unload "$plugin"; rm -f "$js"
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
  # Only inject if we actually focused the game window — otherwise the keypresses would land in the
  # terminal or whatever else has focus (e.g. if the launch never came up).
  if ! focus_game_window; then
    warn "no gamescope window found to focus — skipping injection so keys don't go to the wrong window"
    return 1
  fi
  say "Dismissing startup popups: $presses confirm presses (Enter) into the game window…"
  local i
  for ((i = 0; i < presses; i++)); do
    ydo key 28:1 28:0 || { warn "ydotool failed — is ydotoold running and YDOTOOL_SOCKET correct?"; return 1; }
    sleep 1.2
  done
  ydo key 18:1 18:0 || true   # one E too, in case a dialog wants the menu-accept key over Enter
  ok "sent. If a popup is still up, run: scripts/rig.sh dismiss"
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
  if cmd_launch --wait; then
    if [[ $dismiss -eq 1 ]]; then
      sleep 2          # give the offline/connection popups a moment to appear after the title
      cmd_dismiss || warn "auto-dismiss failed; clear the popups manually or: scripts/rig.sh dismiss"
    fi
    say "Game is running. Drive your test, then: scripts/rig.sh log -f  /  scripts/rig.sh kill"
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
  dismiss [N]            Click through the startup popups (offline-mode / connection-error) by
                         injecting N confirm presses (default 6) into the focused game window via
                         ydotool. Run if a popup is still up after launch.
  cycle [apply-opts]     apply -> launch -> wait for the install/heartbeat lines (solo smoke test).
                         Auto-dismisses the startup popups; pass --no-dismiss to skip that.

Env overrides: GAME_DIR, BACKUP_DIR, APPID, WINDOW_MARGIN, RIG_WINDOW_WIDTH, RIG_WINDOW_HEIGHT,
               RIG_HIDE_UNTIL_PLACED, RIG_YDOTOOL_SOCKET, RIG_DISMISS_PRESSES.
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
  dismiss) cmd_dismiss "$@" ;;
  cycle)   cmd_cycle "$@" ;;
  ""|-h|--help|help) usage ;;
  *) die "unknown command '$cmd' (try: rig.sh help)" ;;
esac
