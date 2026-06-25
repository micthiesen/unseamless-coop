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
  local timeout="$1" before="${2:-}" deadline=$((SECONDS + timeout)) log=""
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
  say "Launching ELDEN RING via Steam (appid $APPID; uses your gamescope launch options)"
  steam -applaunch "$APPID" >/dev/null 2>&1 &
  ok "handed off to Steam. Our launcher sets UNSEAMLESS_LAUNCH and starts the game outside EAC."
  [[ $do_wait -eq 1 ]] && wait_for_framework 150 "$before"
}

latest_log() { ls -1t "$LOG_DIR"/unseamless_coop-*.log 2>/dev/null | head -1; }

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
  cmd_apply "$@"
  cmd_launch --wait \
    && say "Game is running. Drive your test, then: scripts/rig.sh log -f  /  scripts/rig.sh kill"
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
  launch [--wait]        steam -applaunch (uses your gamescope launch options). --wait blocks until
                         the framework comes up and prints the install lines.
  log [-f]               Print (or -f follow) the latest run log.
  kill                   Stop the game (pkill eldenring.exe).
  cycle [apply-opts]     apply -> launch -> wait for the install/heartbeat lines (solo smoke test).

Env overrides: GAME_DIR, BACKUP_DIR, APPID.
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
  cycle)   cmd_cycle "$@" ;;
  ""|-h|--help|help) usage ;;
  *) die "unknown command '$cmd' (try: rig.sh help)" ;;
esac
