#!/usr/bin/env bash
# deck.sh — drive a REMOTE rig (a Steam Deck / second machine) over SSH for two-player networking tests.
#
# The local PC rig (scripts/rig.sh) is player 1; this drives player 2 on another box (a Steam Deck on a
# throwaway Steam account, per the plan). We build the DLL HERE (the toolchain lives here), rsync the
# artifacts + config + save to the Deck, and drive it through the on-Deck helper (scripts/deck/deck-remote.sh)
# over SSH. The Deck stays nearly stateless: it holds only that helper + the applied mod/config/save — no
# git clone, no build tools. No backup/restore (it's a throwaway account; nothing to protect) — but
# `setup` marks the host as a throwaway rig and `apply` refuses without that mark, so a mispointed
# DECK_HOST can't clobber a real install.
#
# Auth is SSH key-based (set up out of band). Everything is env-overridable, which is also how the file/
# apply/config/save/log plumbing is validated against a plain Linux box before any real Deck exists:
#   DECK_HOST=michael@10.10.1.100 DECK_GAME_DIR=/home/michael/working/er/Game \
#   DECK_HELPER_DIR=/home/michael/working/unseamless-deck scripts/deck.sh setup
#
# Verbs:  setup | apply | seed-save | launch | dismiss | kill | cycle | log | pull-logs | status | paths | check | shell
set -euo pipefail

# ---- config (env-overridable) -------------------------------------------------------------------
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DECK_HOST="${DECK_HOST:-}"                 # user@host (key auth). REQUIRED.
DECK_PORT="${DECK_PORT:-22}"               # SSH port (the Deck on this network listens on 2222).
DECK_APPID="${DECK_APPID:-1245620}"
TRIPLE="x86_64-pc-windows-gnu"
SEED_CONFIG="${SEED_CONFIG:-$ROOT/scripts/rig/seed-config.toml}"
REMOTE_HELPER_SRC="$ROOT/scripts/deck/deck-remote.sh"
# Remote paths (defaults derive off the resolved remote $HOME in resolve_paths — see below — so they're
# correct for any Deck user, not just `deck`; override for an SD-card install or non-standard layout).
DECK_HELPER_DIR="${DECK_HELPER_DIR:-}"     # default: <remote-home>/.local/share/unseamless-deck
DECK_STAGING="${DECK_STAGING:-}"           # default: $DECK_HELPER_DIR/staging
DECK_STEAM_ROOT="${DECK_STEAM_ROOT:-}"     # default: <remote-home>/.local/share/Steam
DECK_GAME_DIR="${DECK_GAME_DIR:-}"         # default: $DECK_STEAM_ROOT/steamapps/common/ELDEN RING/Game
# DECK_SAVE_ROOT (remote) is normally left unset and derived from DECK_STEAM_ROOT by the on-Deck helper;
# set it for an SD-card prefix. DECK_STEAM_ID64 selects/creates the save subdir on a fresh account.
# Local save SOURCE for `seed-save` (a file path); empty = try to auto-resolve the local rig's seeded save.
DECK_SAVE_SRC="${DECK_SAVE_SRC:-}"
# Extra ssh flags. NOTE: individual option VALUES must not contain spaces — they're also passed to
# rsync's `-e`, which word-splits them (the flattened `${SSH_OPTS[*]}` form below).
# shellcheck disable=SC2206  # DECK_SSH_OPTS is meant to word-split into extra ssh flags
SSH_OPTS=(-p "$DECK_PORT" -o BatchMode=yes -o ConnectTimeout=10 ${DECK_SSH_OPTS:-})

# ---- output helpers -----------------------------------------------------------------------------
say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  !\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

need_host() { [[ -n "$DECK_HOST" ]] || die "set DECK_HOST=user@host (SSH key-authenticated)"; }

ssh_base() { ssh "${SSH_OPTS[@]}" "$DECK_HOST" "$@"; }

# Single-quote a value for the REMOTE shell, escaping any embedded single quote (the standard '\'' trick)
# so paths/values with spaces OR quotes survive intact when interpolated into an ssh command string.
qsh() { printf "'%s'" "${1//\'/\'\\\'\'}"; }

# Resolve remote home once (also the connectivity check), then derive helper/steam/game paths so every
# subsequent path is absolute and consistent between rsync targets and the on-Deck helper.
DECK_HOME=""
resolve_paths() {
  need_host
  [[ -n "$DECK_HOME" ]] && return 0
  DECK_HOME="$(ssh_base 'printf %s "$HOME"')" || die "cannot SSH to $DECK_HOST (key auth set up? host reachable?)"
  [[ -n "$DECK_HOME" ]] || die "could not resolve remote \$HOME on $DECK_HOST"
  DECK_HELPER_DIR="${DECK_HELPER_DIR:-$DECK_HOME/.local/share/unseamless-deck}"
  DECK_STAGING="${DECK_STAGING:-$DECK_HELPER_DIR/staging}"
  DECK_STEAM_ROOT="${DECK_STEAM_ROOT:-$DECK_HOME/.local/share/Steam}"
  DECK_GAME_DIR="${DECK_GAME_DIR:-$DECK_STEAM_ROOT/steamapps/common/ELDEN RING/Game}"
  REMOTE_HELPER="$DECK_HELPER_DIR/deck-remote.sh"
}

# Build the env prefix forwarding the DECK_* knobs the on-Deck helper reads (only those actually set,
# plus the resolved paths), each properly quoted for the remote shell.
remote_env() {
  local out="" v val
  for v in DECK_APPID DECK_STEAM_ROOT DECK_GAME_DIR DECK_SAVE_ROOT DECK_HELPER_DIR DECK_STAGING \
           DECK_STEAM_ID64 DECK_DISMISS_PRESSES DECK_DISMISS_INTERVAL DECK_DISMISS_KEY DECK_YDOTOOL_SOCKET; do
    val="${!v:-}"
    [[ -n "$val" ]] && out+="$v=$(qsh "$val") "
  done
  printf '%s' "$out"
}

# Invoke a remote-helper verb over SSH with the env prefix. Args are quoted for the remote shell, so
# they may contain spaces (e.g. a baseline log path under ".../ELDEN RING/Game"). Guards on the helper
# existing so a missing one gives a clear "run setup" hint, not a raw bash error.
deck_remote() {
  resolve_paths
  local q="" a
  for a in "$@"; do q+=" $(qsh "$a")"; done
  ssh_base "test -f $(qsh "$REMOTE_HELPER") || { echo 'deck-remote.sh not on the Deck — run: scripts/deck.sh setup' >&2; exit 127; }
            $(remote_env)bash $(qsh "$REMOTE_HELPER")$q"
}

push_helper() {
  resolve_paths
  [[ -f "$REMOTE_HELPER_SRC" ]] || die "missing $REMOTE_HELPER_SRC"
  ssh_base "mkdir -p $(qsh "$DECK_HELPER_DIR") $(qsh "$DECK_STAGING")"
  # -s (--protect-args): send the remote path verbatim so spaces survive without remote-shell splitting.
  rsync -azs -e "ssh ${SSH_OPTS[*]}" "$REMOTE_HELPER_SRC" "$DECK_HOST:$DECK_HELPER_DIR/deck-remote.sh"
  ssh_base "chmod +x $(qsh "$REMOTE_HELPER")"
}

rsync_to() {  # $1 = local file, $2 = remote absolute path
  rsync -azs -e "ssh ${SSH_OPTS[*]}" "$1" "$DECK_HOST:$2"
}

# ---- build (local; mirrors rig.sh) --------------------------------------------------------------
build() {
  local profile="$1"
  say "Building ($profile) for $TRIPLE"
  # No `--features bridge` here (unlike rig.sh's diag build): the bridge is the LOCAL-host dev loopback
  # listener, not wanted on the remote player. No `| tail` either, so a compile error shows in full.
  ( cd "$ROOT" && cargo build --profile "$profile" )
}
artifact_dir() { echo "$ROOT/target/$TRIPLE/$1"; }

configured_save_ext() {  # read [save] file_extension from the seed config (fallback matches rig.sh)
  local ext; ext="$(sed -nE 's/^[[:space:]]*file_extension[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$SEED_CONFIG" | head -n1)"
  printf '%s' "${ext:-uco}"
}

# ---- verbs --------------------------------------------------------------------------------------
cmd_setup() {
  resolve_paths
  say "Setting up the remote rig on $DECK_HOST (home: $DECK_HOME)"
  push_helper
  ok "pushed helper -> $REMOTE_HELPER"
  deck_remote mark-throwaway        # the apply guard: marks this host as an explicit throwaway rig
  deck_remote check || true
  say "Next: scripts/deck.sh apply   (build + push the mod), then launch / cycle."
}

cmd_apply() {
  local profile=diag do_build=1 keep_config=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --release)     profile=release ;;
      --no-build)    do_build=0 ;;
      --keep-config) keep_config=1 ;;
      *) die "apply: unknown flag '$1'" ;;
    esac; shift
  done
  resolve_paths
  [[ $do_build -eq 1 ]] && build "$profile"
  local out; out="$(artifact_dir "$profile")"
  local dll="$out/unseamless_coop.dll" launcher="$out/start_protected_game.exe"
  [[ -f "$dll" ]]      || die "missing $dll — build first (drop --no-build)."
  [[ -f "$launcher" ]] || die "missing $launcher — build first (drop --no-build)."

  push_helper
  say "Staging artifacts -> $DECK_HOST:$DECK_STAGING"
  ssh_base "mkdir -p $(qsh "$DECK_STAGING")"
  rsync_to "$dll" "$DECK_STAGING/unseamless_coop.dll"
  rsync_to "$launcher" "$DECK_STAGING/start_protected_game.exe"
  [[ $keep_config -eq 0 ]] && rsync_to "$SEED_CONFIG" "$DECK_STAGING/unseamless_coop.toml"
  # Build-info for the marker (so the Deck records what build it's running).
  local sha; sha="$(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  printf 'profile: %s\ngit: %s\nbuilt: %s\n' "$profile" "$sha" "$(date -Is)" \
    | ssh_base "cat > $(qsh "$DECK_STAGING/BUILD_INFO")"
  ok "staged dll + launcher + config + build-info"

  local rargs=(apply-staged); [[ $keep_config -eq 1 ]] && rargs+=(--keep-config)
  deck_remote "${rargs[@]}"
  ok "applied on $DECK_HOST"
}

cmd_seed_save() {
  resolve_paths
  local src="${1:-$DECK_SAVE_SRC}"
  local ext; ext="$(configured_save_ext)"
  if [[ -z "$src" ]]; then
    # Try the local rig's seeded save: <local compatdata>/…/EldenRing/<id>/ER0000.<ext>. Override
    # LOCAL_SAVE_ROOT if your local Steam library isn't the default /mnt/games one.
    local lroot="${LOCAL_SAVE_ROOT:-/mnt/games/SteamLibrary/steamapps/compatdata/$DECK_APPID/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing}"
    local cand; cand="$(ls -1 "$lroot"/*/ER0000."$ext" 2>/dev/null | head -1 || true)"
    [[ -n "$cand" ]] && src="$cand"
  fi
  [[ -n "$src" && -f "$src" ]] || die "no save source. Pass a file: scripts/deck.sh seed-save /path/ER0000.$ext  (or set DECK_SAVE_SRC / LOCAL_SAVE_ROOT)"
  say "Pushing save $src -> Deck (as ER0000.$ext)"
  ssh_base "mkdir -p $(qsh "$DECK_STAGING/save")"
  rsync_to "$src" "$DECK_STAGING/save/ER0000.$ext"
  deck_remote seed-save-staged "$ext"
}

cmd_launch()  { deck_remote launch; }
cmd_dismiss() { deck_remote dismiss; }
cmd_kill()    { deck_remote kill; }
cmd_status()  { deck_remote status; }
cmd_paths()   { deck_remote paths; }
cmd_check()   { deck_remote check; }
cmd_log()     { deck_remote log "$@"; }   # `log -f` streams tail -f over the ssh channel (Ctrl-C to stop)

cmd_pull_logs() {
  resolve_paths
  local dest="${1:-$ROOT/.deck-logs}"
  mkdir -p "$dest"
  local remote_logs="$DECK_GAME_DIR/unseamless-coop/logs"
  say "Pulling logs $DECK_HOST:$remote_logs -> $dest"
  # -s (--protect-args) so the space in ".../ELDEN RING/Game" survives without manual escaping.
  local rc=0
  rsync -azs -e "ssh ${SSH_OPTS[*]}" "$DECK_HOST:$remote_logs/" "$dest/" || rc=$?
  if [[ $rc -eq 0 ]]; then ok "logs in $dest"
  elif [[ $rc -eq 23 ]]; then warn "no logs pulled — the logs dir doesn't exist yet (has the Deck run?)"
  else die "rsync failed (exit $rc) pulling logs — connectivity/permissions?"; fi
}

cmd_cycle() {
  cmd_apply "$@"
  say "Stopping any running game on the Deck before relaunch…"
  cmd_kill || true
  # Baseline: the latest log BEFORE launch, so wait-framework waits for a genuinely NEW run's log (the
  # cdylib keeps old logs, which already contain the framework-up line).
  local before; before="$(deck_remote latest-log 2>/dev/null || true)"
  cmd_launch
  say "Waiting for the framework to come up on the Deck…"
  deck_remote wait-framework 150 "$before" || warn "framework didn't report up in time — check 'deck.sh log'"
  say "Settling, then dismissing startup popups…"
  sleep "${DECK_DISMISS_PRESETTLE:-10}"
  cmd_dismiss || warn "dismiss failed (ydotool not set up on the Deck?) — dismiss manually or see the skill"
}

cmd_shell() { need_host; exec ssh "${SSH_OPTS[@]}" "$DECK_HOST"; }

usage() {
  cat <<EOF
deck.sh — remote rig over SSH (player 2). Set DECK_HOST=user@host first.

  setup                 push the on-Deck helper, mark the host a throwaway rig, report deps (run once per Deck)
  apply [--release] [--no-build] [--keep-config]
                        build the DLL here, rsync it + the launcher + seed config, install on the Deck
  seed-save [file]      push a save (default: the local rig's seeded ER0000.<ext>) into the Deck's prefix
  launch                start the game on the Deck via the running Steam
  dismiss               ydotool the startup popups away (click into gameplay) — needs ydotool on the Deck
  kill                  stop the game + launcher on the Deck
  cycle [apply-opts]    apply -> kill -> launch -> wait -> dismiss (the solo-on-Deck smoke test)
  log [-f]              print/follow the latest Deck log
  pull-logs [dest]      rsync the Deck's logs back here (default .deck-logs/)
  status | paths | check
                        report applied state / resolved paths / remote deps
  shell                 open an SSH shell on the Deck

Key env: DECK_HOST, DECK_GAME_DIR, DECK_APPID, DECK_HELPER_DIR, DECK_SAVE_SRC, DECK_STEAM_ID64.
EOF
}

main() {
  local verb="${1:-}"; shift || true
  case "$verb" in
    setup)      cmd_setup "$@" ;;
    apply)      cmd_apply "$@" ;;
    seed-save)  cmd_seed_save "$@" ;;
    launch)     cmd_launch "$@" ;;
    dismiss)    cmd_dismiss "$@" ;;
    kill)       cmd_kill "$@" ;;
    cycle)      cmd_cycle "$@" ;;
    log)        cmd_log "$@" ;;
    pull-logs)  cmd_pull_logs "$@" ;;
    status)     cmd_status "$@" ;;
    paths)      cmd_paths "$@" ;;
    check)      cmd_check "$@" ;;
    shell)      cmd_shell "$@" ;;
    ""|-h|--help|help) usage ;;
    *) usage; die "unknown verb '$verb'" ;;
  esac
}

main "$@"
