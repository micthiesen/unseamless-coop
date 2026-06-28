#!/usr/bin/env bash
# deck-remote.sh — the ON-DECK half of the remote rig (see scripts/deck.sh for the local driver and
# the `steam-deck` skill for the workflow).
#
# This runs ON the Steam Deck (or any SteamOS/Linux box acting as a second player). The local driver
# (`scripts/deck.sh`) rsyncs this file + the built artifacts to the Deck, then invokes the verbs here
# over SSH. The Deck stays (almost) stateless: it holds only this helper, the applied mod files, the
# config, and the save — no git clone, no toolchain. Re-pushed on every `apply`, so it's never stale.
#
# All Deck paths are env-overridable (the local driver passes them), which is also what lets the whole
# thing be validated against a throwaway dir on a plain Linux box before any real Deck exists.
set -euo pipefail

# ---- config (env-overridable; SteamOS defaults) -------------------------------------------------
APPID="${DECK_APPID:-1245620}"
STEAM_ROOT="${DECK_STEAM_ROOT:-$HOME/.local/share/Steam}"
GAME_DIR="${DECK_GAME_DIR:-$STEAM_ROOT/steamapps/common/ELDEN RING/Game}"
# Elden Ring's Proton-prefix save root (…/EldenRing/<SteamID64>/ER0000.<ext>).
SAVE_ROOT="${DECK_SAVE_ROOT:-$STEAM_ROOT/steamapps/compatdata/$APPID/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing}"
HELPER_DIR="${DECK_HELPER_DIR:-$HOME/.local/share/unseamless-deck}"
STAGING="${DECK_STAGING:-$HELPER_DIR/staging}"
# Optional: the throwaway account's SteamID64, only needed to CREATE a save subdir when the game has
# never run under this account yet (so EldenRing/<id>/ doesn't exist). Otherwise auto-detected.
STEAM_ID64="${DECK_STEAM_ID64:-}"

CONFIG_DST="$GAME_DIR/unseamless-coop/unseamless_coop.toml"
LOG_DIR="$GAME_DIR/unseamless-coop/logs"
MARKER="$GAME_DIR/unseamless-coop/.deck-applied"
# Sentinel written by `deck.sh setup` to mark this host as an explicitly-initialized throwaway rig.
# `apply-staged` refuses without it, so a mispointed DECK_HOST can't clobber a real (non-throwaway)
# install — the one safety guard we keep despite "no backup" (there's nothing to back up, but a wrong
# host is a real foot-gun). See scripts/rig.sh's backup model for the local-rig equivalent.
THROWAWAY_SENTINEL="$HELPER_DIR/.deck-throwaway"

# ---- output helpers -----------------------------------------------------------------------------
say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  !\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ---- popup-dismiss tuning (mirrors rig.sh; the popups are MODAL in-engine dialogs) --------------
# 100 taps x 400ms = a ~40s window — deliberately longer than rig.sh's 30: the Deck runs UNATTENDED and
# ELDEN RING's Proton cold-start can show the popups well after the mod's framework loads, so we spam
# generously to be sure we outlast them (extra taps after reaching gameplay are harmless). Same 400ms
# frequency, so each modal popup still gets cleared the moment it appears.
DISMISS_PRESSES="${DECK_DISMISS_PRESSES:-100}"
DISMISS_INTERVAL_MS="${DECK_DISMISS_INTERVAL_MS:-400}" # 400ms == rig.sh's 0.4s
DISMISS_KEY="${DECK_DISMISS_KEY:-28}"                  # 28 = Enter (the menu-confirm key, as on the local rig)
TAP_BIN="$HELPER_DIR/bin/uinput-tap"                   # the bundled static uinput key-tapper (deck.sh seeds it)

# ---- session-env lift ---------------------------------------------------------------------------
# Lift the graphical-session env (XDG_RUNTIME_DIR / DBUS / WAYLAND_DISPLAY / DISPLAY / XAUTHORITY) out of
# the running Steam process so SSH-launched commands (the game launch) reach the live session —
# over SSH we don't inherit it. Picks the NEWEST steam pid (an older lingering one can carry dead-session
# vars). Returns non-zero if it can't even resolve XDG_RUNTIME_DIR (no live session).
lift_session_env() {
  local spid envf var val
  spid="$(pgrep -x steam | tail -1 || true)"
  [[ -z "$spid" ]] && spid="$(pgrep -f 'steam/ubuntu12_32/steam' | tail -1 || true)"
  [[ -z "$spid" ]] && return 1
  envf="/proc/$spid/environ"
  for var in XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS WAYLAND_DISPLAY DISPLAY XAUTHORITY; do
    val="$(tr '\0' '\n' < "$envf" 2>/dev/null | sed -n "s/^$var=//p" | head -1 || true)"
    [[ -n "$val" ]] && export "$var=$val"
  done
  [[ -n "${XDG_RUNTIME_DIR:-}" ]]
}

# ---- path resolution ----------------------------------------------------------------------------
# Find the single EldenRing/<SteamID64>/ save subdir. The game creates it on first run under an
# account, so on a fresh throwaway account it may not exist yet (run the game once, or pass
# DECK_STEAM_ID64 to create it). Prints the resolved dir, or empty + a reason on stderr.
resolve_save_dir() {
  if [[ -n "$STEAM_ID64" ]]; then echo "$SAVE_ROOT/$STEAM_ID64"; return 0; fi
  [[ -d "$SAVE_ROOT" ]] || { warn "save root missing ($SAVE_ROOT) — has the game run once on the Deck?"; return 1; }
  local dirs=() d
  for d in "$SAVE_ROOT"/*/; do [[ -d "$d" && "$(basename "$d")" =~ ^[0-9]+$ ]] && dirs+=("${d%/}"); done
  case ${#dirs[@]} in
    1) echo "${dirs[0]}" ;;
    0) warn "no EldenRing/<SteamID64>/ subdir yet — run the game once on the Deck, or set DECK_STEAM_ID64"; return 1 ;;
    *) warn "multiple SteamID64 save subdirs found; set DECK_STEAM_ID64 to pick one: ${dirs[*]}"; return 1 ;;
  esac
}

# ---- verbs --------------------------------------------------------------------------------------
cmd_paths() {
  printf 'appid       %s\n' "$APPID"
  printf 'steam_root  %s\n' "$STEAM_ROOT"
  printf 'game_dir    %s\n' "$GAME_DIR"
  printf 'config      %s\n' "$CONFIG_DST"
  printf 'log_dir     %s\n' "$LOG_DIR"
  printf 'save_root   %s\n' "$SAVE_ROOT"
  printf 'save_dir    %s\n' "$(resolve_save_dir 2>/dev/null || echo '<unresolved>')"
  printf 'helper_dir  %s\n' "$HELPER_DIR"
  printf 'marker      %s\n' "$MARKER"
}

cmd_check() {
  say "deck-remote check on $(uname -n) ($(uname -s))"
  local have_steam have_tap have_game uinput_w
  have_steam="$(command -v steam || echo MISSING)"
  have_tap=missing; [[ -x "$TAP_BIN" ]] && have_tap=present
  uinput_w=no; [[ -w /dev/uinput ]] && uinput_w=yes
  have_game=missing; [[ -f "$GAME_DIR/eldenring.exe" ]] && have_game=present
  printf '  steam        %s\n' "$have_steam"
  printf '  uinput-tap   %s  (dismiss/click-into-gameplay)\n' "$have_tap"
  printf '  /dev/uinput  writable=%s\n' "$uinput_w"
  printf '  game exe     %s  (%s/eldenring.exe)\n' "$have_game" "$GAME_DIR"
  printf '  game dir     %s\n' "$([[ -d "$GAME_DIR" ]] && echo present || echo missing)"
  printf '  throwaway    %s\n' "$([[ -f "$THROWAWAY_SENTINEL" ]] && echo yes || echo 'no (run setup before apply)')"
  printf '  applied      %s\n' "$([[ -f "$MARKER" ]] && echo yes || echo no)"
  [[ "$have_steam" == MISSING ]] && warn "steam not found — launch will fail (is this the Deck, logged into Game Mode?)"
  [[ "$have_tap" == missing ]] && warn "uinput-tap not seeded — 'dismiss' unavailable until 'scripts/deck.sh setup' (or seed-input)"
  [[ "$uinput_w" == no ]] && warn "/dev/uinput not writable — dismiss needs an active Game Mode session"
  return 0   # the trailing `[[ ]] && warn` above must not become this function's (failing) exit status
}

cmd_mark_throwaway() { mkdir -p "$HELPER_DIR"; : > "$THROWAWAY_SENTINEL"; ok "marked $(uname -n) as a throwaway rig ($THROWAWAY_SENTINEL)"; }

# Move staged artifacts into place. The local driver rsyncs unseamless_coop.dll / start_protected_game.exe
# / unseamless_coop.toml into $STAGING first; we place them like rig.sh's apply (dll -> dinput8.dll, our
# launcher -> start_protected_game.exe), seed the config, empty mods/, and write the marker.
cmd_apply_staged() {
  local keep_config=0; [[ "${1:-}" == "--keep-config" ]] && keep_config=1
  [[ -f "$THROWAWAY_SENTINEL" ]] || die "this host isn't initialized as a throwaway rig — run 'scripts/deck.sh setup' first.
       (this guard stops a mispointed DECK_HOST from clobbering a real install; there's no backup.)"
  [[ -d "$GAME_DIR" ]] || die "game dir not found: $GAME_DIR (set DECK_GAME_DIR, or is the game installed?)"
  local dll="$STAGING/unseamless_coop.dll" launcher="$STAGING/start_protected_game.exe"
  [[ -f "$dll" ]] || die "staged DLL missing ($dll) — did the local driver rsync it?"
  [[ -f "$launcher" ]] || die "staged launcher missing ($launcher)"

  say "Installing our mod into $GAME_DIR"
  install -m644 "$dll" "$GAME_DIR/dinput8.dll"
  install -m755 "$launcher" "$GAME_DIR/start_protected_game.exe"
  ok "dinput8.dll + start_protected_game.exe in place"

  # mods/ empty (clean observation; we own the whole loader — no EML, no other mods).
  rm -rf "$GAME_DIR/mods"; mkdir -p "$GAME_DIR/mods"

  mkdir -p "$(dirname "$CONFIG_DST")"
  if [[ $keep_config -eq 1 && -f "$CONFIG_DST" ]]; then
    ok "kept existing config ($CONFIG_DST)"
  elif [[ -f "$STAGING/unseamless_coop.toml" ]]; then
    install -m644 "$STAGING/unseamless_coop.toml" "$CONFIG_DST"
    ok "wrote seed config ($CONFIG_DST)"
  else
    warn "no staged config — left $CONFIG_DST as-is"
  fi

  { echo "applied: $(date -Is)"; echo "host: $(uname -n)"; cat "$STAGING/BUILD_INFO" 2>/dev/null || true; } > "$MARKER"
  ok "wrote marker ($MARKER)"
}

# Place a staged save (staging/save/ER0000.<ext>) into the resolved EldenRing/<id>/ dir, backing up any
# existing test save ONCE (so a re-seed never overwrites the original backup). Never touches a vanilla .sl2.
cmd_seed_save_staged() {
  local ext="${1:?usage: seed-save-staged <ext>}"
  [[ "$ext" == "sl2" ]] && die "refusing to seed a .sl2 save — that's the vanilla single-player save (use the co-op ext)"
  pgrep -f '[e]ldenring.exe' >/dev/null 2>&1 && die "the game is running — close it first (a live game overwrites the seeded save)"
  local src="$STAGING/save/ER0000.$ext"
  [[ -f "$src" ]] || die "staged save missing ($src)"
  local dir; dir="$(resolve_save_dir)" || die "could not resolve the save dir (see warning above)"
  mkdir -p "$dir"
  local dst="$dir/ER0000.$ext"
  # Only back up if we haven't already — else a second seed clobbers the original backup with the test save.
  [[ -f "$dst" && ! -f "$dst.deckbak" ]] && { cp -f "$dst" "$dst.deckbak"; ok "backed up existing $dst -> $dst.deckbak"; }
  install -m644 "$src" "$dst"
  ok "seeded save -> $dst"
}

# Launch the game via the RUNNING Steam (Game Mode). Lifts the session env from the live Steam process
# (we don't inherit it over SSH), then fires the rungameid URL at it; detached (setsid) so the closing
# SSH session can't SIGHUP the in-flight handoff. Our applied start_protected_game.exe is what Steam
# invokes, so this launches outside EAC with the UNSEAMLESS_LAUNCH marker, exactly as on the rig.
cmd_launch() {
  command -v steam >/dev/null 2>&1 || die "steam not found on the Deck (is it logged into Game Mode?)"
  lift_session_env || die "no running Steam session found — start Steam / Game Mode on the Deck first"
  say "Launching appid $APPID via the running Steam session"
  setsid steam "steam://rungameid/$APPID" </dev/null >/dev/null 2>&1 &
  ok "handed off to Steam (steam://rungameid/$APPID). The applied launcher starts the game outside EAC."
}

# Tap Enter through our bundled static uinput key-tapper to clear the modal startup popups and select
# Continue → gameplay. No daemon/socket (unlike ydotool): the tapper writes /dev/uinput directly, which
# the SteamOS session ACL grants the `deck` user while Game Mode is up. One process, the whole sequence.
cmd_dismiss() {
  [[ -x "$TAP_BIN" ]] || { warn "uinput-tap not seeded ($TAP_BIN) — run 'scripts/deck.sh setup' (or 'seed-input')"; return 1; }
  [[ "$DISMISS_PRESSES" =~ ^[0-9]+$ ]] || die "DECK_DISMISS_PRESSES must be a number (got '$DISMISS_PRESSES')"
  [[ -w /dev/uinput ]] || warn "/dev/uinput not writable by $(whoami) — needs an active session (Game Mode up on the Deck)"
  say "Dismissing startup popups: $DISMISS_PRESSES taps of key $DISMISS_KEY every ${DISMISS_INTERVAL_MS}ms (uinput-tap)"
  "$TAP_BIN" "$DISMISS_KEY" "$DISMISS_PRESSES" "$DISMISS_INTERVAL_MS" \
    || { warn "uinput-tap failed — is /dev/uinput accessible? (active Game Mode session?)"; return 1; }
  ok "sent $DISMISS_PRESSES taps"
}

cmd_kill() {
  local any=0
  # Bracket trick so pgrep/pkill don't match their own command line.
  if pgrep -f '[e]ldenring.exe' >/dev/null 2>&1; then any=1; pkill -f '[e]ldenring.exe' || true; fi
  if pgrep -f '[s]tart_protected_game.exe' >/dev/null 2>&1; then any=1; pkill -f '[s]tart_protected_game.exe' || true; fi
  sleep 2
  if pgrep -f '[e]ldenring.exe' >/dev/null 2>&1; then
    warn "eldenring.exe still up after SIGTERM — sending SIGKILL"; pkill -9 -f '[e]ldenring.exe' || true
  fi
  [[ $any -eq 1 ]] && ok "game stopped" || ok "nothing was running"
}

latest_log() { ls -1t "$LOG_DIR"/unseamless_coop-*.log 2>/dev/null | head -1; }

cmd_latest_log() { latest_log || { warn "no log yet in $LOG_DIR"; return 1; }; }

cmd_log() {
  local follow=0; [[ "${1:-}" == "-f" ]] && follow=1
  local f; f="$(latest_log || true)"
  [[ -n "$f" ]] || { warn "no log yet in $LOG_DIR"; return 1; }
  if [[ $follow -eq 1 ]]; then tail -f "$f"; else cat "$f"; fi
}

# Poll the on-Deck log until the framework reports up, so `deck.sh cycle` can wait before dismissing.
# $1 = timeout seconds (default 150). $2 = pre-launch baseline log path: a fresh run writes a NEW
# timestamped file, so we require the latest log to DIFFER from the baseline — otherwise a prior run's
# log (the cdylib keeps old ones) would match instantly and we'd dismiss into a still-loading game.
cmd_wait_framework() {
  local timeout="${1:-150}" baseline="${2:-}" waited=0 f
  while (( waited < timeout )); do
    f="$(latest_log || true)"
    if [[ -n "$f" && "$f" != "$baseline" ]] && grep -q "registered feature 'session-observer'" "$f" 2>/dev/null; then
      ok "framework up ($f)"; return 0
    fi
    sleep 3; waited=$((waited+3))
  done
  warn "framework not up within ${timeout}s"; return 1
}

cmd_status() {
  cmd_check
  echo
  say "applied state"
  if [[ -f "$MARKER" ]]; then sed 's/^/      /' "$MARKER"; else echo "      (not applied)"; fi
  printf '  config       %s\n' "$([[ -f "$CONFIG_DST" ]] && echo present || echo missing)"
  printf '  dinput8.dll  %s\n' "$(stat -c '%s bytes, %y' "$GAME_DIR/dinput8.dll" 2>/dev/null || echo missing)"
  printf '  launcher     %s\n' "$(stat -c '%s bytes, %y' "$GAME_DIR/start_protected_game.exe" 2>/dev/null || echo missing)"
  printf '  latest log   %s\n' "$(latest_log || echo none)"
  local sd; sd="$(resolve_save_dir 2>/dev/null || true)"
  printf '  save dir     %s\n' "${sd:-<unresolved>}"
}

main() {
  local verb="${1:-}"; shift || true
  case "$verb" in
    paths)             cmd_paths "$@" ;;
    check)             cmd_check "$@" ;;
    mark-throwaway)    cmd_mark_throwaway "$@" ;;
    apply-staged)      cmd_apply_staged "$@" ;;
    seed-save-staged)  cmd_seed_save_staged "$@" ;;
    launch)            cmd_launch "$@" ;;
    dismiss)           cmd_dismiss "$@" ;;
    kill)              cmd_kill "$@" ;;
    latest-log)        cmd_latest_log "$@" ;;
    wait-framework)    cmd_wait_framework "$@" ;;
    log)               cmd_log "$@" ;;
    status)            cmd_status "$@" ;;
    *) die "unknown verb '$verb' (paths|check|mark-throwaway|apply-staged|seed-save-staged|launch|dismiss|kill|latest-log|wait-framework|log|status)" ;;
  esac
}

main "$@"
