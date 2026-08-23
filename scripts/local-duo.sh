#!/usr/bin/env bash
# Drive two independent ELDEN RING + Proton processes on one Linux desktop. This is displayless,
# not simulation-free: both clients render, run Steam networking, and own distinct identities,
# prefixes, saves, installs, logs, and gamescope displays.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DUO_CONFIG=${UNSEAMLESS_DUO_CONFIG:-"$HOME/.config/unseamless-coop/local-duo.env"}
if [[ -f "$DUO_CONFIG" ]]; then
  # This is a user-owned shell fragment containing path overrides only. Keeping it outside the repo
  # lets the one-time desktop setup survive checkouts without committing machine-specific paths.
  # shellcheck disable=SC1090
  source "$DUO_CONFIG"
fi
DUO_ROOT=${UNSEAMLESS_DUO_ROOT:-"$HOME/.local/share/unseamless-duo"}
P1_HOME=${UNSEAMLESS_DUO_P1_HOME:-"$HOME"}
P2_HOME=${UNSEAMLESS_DUO_P2_HOME:-"$DUO_ROOT/p2-home"}
P1_STEAM_ROOT=${UNSEAMLESS_DUO_P1_STEAM_ROOT:-"$P1_HOME/.local/share/Steam"}
P2_STEAM_ROOT=${UNSEAMLESS_DUO_P2_STEAM_ROOT:-"$P2_HOME/.local/share/Steam"}
P1_LIBRARY=${UNSEAMLESS_DUO_P1_LIBRARY:-"$P1_STEAM_ROOT"}
P2_LIBRARY=${UNSEAMLESS_DUO_P2_LIBRARY:-"$DUO_ROOT/p2-library"}
P1_GAME_DIR="$P1_LIBRARY/steamapps/common/ELDEN RING/Game"
P2_GAME_DIR="$P2_LIBRARY/steamapps/common/ELDEN RING/Game"
P1_BACKUP="$DUO_ROOT/backups/p1"
P2_BACKUP="$DUO_ROOT/backups/p2"
APPID=1245620
P2_STEAM_IPC=${UNSEAMLESS_DUO_P2_STEAM_IPC:-unseamless_p2}
WRAPPER="$ROOT/scripts/local-duo/gamescope-instance.sh"

die() { printf 'local-duo: %s\n' "$*" >&2; exit 1; }
note() { printf 'local-duo: %s\n' "$*"; }

require_linux() { [[ $(uname -s) == Linux ]] || die 'this command requires the CachyOS/Linux desktop'; }

resolved() { realpath -m -- "$1"; }

assert_distinct() {
  local label=$1 left=$2 right=$3 left_real right_real
  left_real=$(resolved "$left")
  right_real=$(resolved "$right")
  [[ "$left_real" != "$right_real" ]] || die "$label resolve to the same path: $left_real"
}

assert_isolated_paths() {
  command -v realpath >/dev/null || die 'realpath is required'
  assert_distinct 'P1/P2 homes' "$P1_HOME" "$P2_HOME"
  assert_distinct 'P1/P2 Steam roots' "$P1_STEAM_ROOT" "$P2_STEAM_ROOT"
  assert_distinct 'P1/P2 libraries' "$P1_LIBRARY" "$P2_LIBRARY"
  assert_distinct 'P1/P2 game directories' "$P1_GAME_DIR" "$P2_GAME_DIR"
  assert_distinct 'P1/P2 Proton prefixes' \
    "$P1_LIBRARY/steamapps/compatdata/$APPID" "$P2_LIBRARY/steamapps/compatdata/$APPID"
}

assert_no_game_processes() {
  local pids
  pids=$(pgrep -f 'eldenring.exe|start_protected_game.exe' || true)
  [[ -z "$pids" ]] || die "an ELDEN RING process is still running ($pids); refusing to overwrite its install"
}

steam_env() {
  local instance=$1
  shift
  if [[ "$instance" == p1 ]]; then
    env HOME="$P1_HOME" STEAM_COMPAT_CLIENT_INSTALL_PATH="$P1_STEAM_ROOT" \
      UNSEAMLESS_DUO_ROOT="$DUO_ROOT" "$@"
  else
    mkdir -p "$P2_HOME/.config" "$P2_HOME/.cache" "$P2_HOME/.local/share"
    env HOME="$P2_HOME" XDG_CONFIG_HOME="$P2_HOME/.config" XDG_CACHE_HOME="$P2_HOME/.cache" \
      XDG_DATA_HOME="$P2_HOME/.local/share" STEAM_COMPAT_CLIENT_INSTALL_PATH="$P2_STEAM_ROOT" \
      UNSEAMLESS_DUO_ROOT="$DUO_ROOT" "$@"
  fi
}

instance_pids() {
  local instance=$1 pid
  while read -r pid; do
    [[ -r "/proc/$pid/environ" ]] || continue
    tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -qx "UNSEAMLESS_DUO_INSTANCE=$instance" && printf '%s\n' "$pid"
  done < <(pgrep -f 'eldenring.exe|start_protected_game.exe' || true)
}

latest_log() {
  local instance=$1 game_dir newest baseline baseline_path baseline_sum current_sum
  [[ "$instance" == p1 ]] && game_dir=$P1_GAME_DIR || game_dir=$P2_GAME_DIR
  newest=$(find "$game_dir/unseamless-coop/logs" -maxdepth 1 -type f -name 'unseamless_coop-*.log' -print 2>/dev/null \
    | sort | tail -1)
  [[ -n "$newest" ]] || return 0
  baseline="$DUO_ROOT/runtime/$instance/previous-log"
  if [[ -f "$baseline" ]]; then
    IFS=$'\t' read -r baseline_sum baseline_path < "$baseline" || true
    if [[ "$newest" == "$baseline_path" ]]; then
      current_sum=$(cksum < "$newest")
      [[ "$current_sum" != "$baseline_sum" ]] || return 0
    fi
  fi
  printf '%s\n' "$newest"
}

record_log_baseline() {
  local instance=$1 game_dir newest runtime
  [[ "$instance" == p1 ]] && game_dir=$P1_GAME_DIR || game_dir=$P2_GAME_DIR
  runtime="$DUO_ROOT/runtime/$instance"
  mkdir -p "$runtime"
  newest=$(find "$game_dir/unseamless-coop/logs" -maxdepth 1 -type f -name 'unseamless_coop-*.log' -print 2>/dev/null \
    | sort | tail -1)
  if [[ -n "$newest" ]]; then
    printf '%s\t%s\n' "$(cksum < "$newest")" "$newest" > "$runtime/previous-log"
  else
    : > "$runtime/previous-log"
  fi
}

wait_log() {
  local instance=$1 pattern=$2 label=$3 log
  for _ in $(seq 1 120); do
    log=$(latest_log "$instance")
    if [[ -n "$log" ]] && grep -q "$pattern" "$log"; then note "$instance: $label"; return; fi
    sleep 1
  done
  die "$instance timed out waiting for $label"
}

display_for() {
  local instance=$1 pid display
  pid=$(instance_pids "$instance" | head -1)
  [[ -n "$pid" ]] || die "$instance game process is not running"
  display=$(tr '\0' '\n' < "/proc/$pid/environ" | sed -n 's/^DISPLAY=//p' | head -1)
  [[ -n "$display" ]] || die "$instance has no nested X display"
  printf '%s\n' "$display"
}

dismiss_to_world() {
  local instance=$1 display elapsed log start
  display=$(display_for "$instance")
  note "$instance: settling for startup dialogs"
  sleep 10
  for _ in $(seq 1 22); do
    "$ROOT/scripts/rig/xtest-key" --display "$display" Return
    sleep 0.4
  done
  log=$(latest_log "$instance")
  [[ -n "$log" ]] || die "$instance has no current run log"
  start=$SECONDS
  while ! grep -qE 'in_gameplay +=.*\btrue\b' "$log"; do
    elapsed=$((SECONDS - start))
    (( elapsed < 180 )) || die "$instance timed out entering the world"
    "$ROOT/scripts/rig/xtest-key" --display "$display" e
    sleep 18
  done
  note "$instance: in world"
}

cmd_setup() {
  require_linux
  assert_isolated_paths
  mkdir -p "$DUO_ROOT/backups" "$DUO_ROOT/runtime" "$P2_HOME" "$P2_LIBRARY/steamapps/common" "$(dirname "$DUO_CONFIG")"
  if [[ ! -e "$DUO_CONFIG" ]]; then
    umask 077
    printf '# Local paths for scripts/local-duo.sh. Uncomment overrides when your main library is elsewhere.\n# export UNSEAMLESS_DUO_P1_LIBRARY=/mnt/games/SteamLibrary\n# export UNSEAMLESS_DUO_P2_LIBRARY=%q\n' "$P2_LIBRARY" > "$DUO_CONFIG"
    note "created machine-local config: $DUO_CONFIG"
  fi
  note "P1 Steam launch options: $WRAPPER p1 -- %command%"
  note "P2 Steam launch options: $WRAPPER p2 -- %command%"
  note "P2 Steam home: $P2_HOME"
  note "Run 'scripts/local-duo.sh steam-p2', sign into the second Steam account once, add the P2 library, and set its launch options."
  note "Then run 'scripts/local-duo.sh clone-p2' (or install ELDEN RING into the P2 library normally)."
}

cmd_steam_p2() {
  require_linux
  assert_isolated_paths
  command -v steam >/dev/null || die 'steam is required'
  note "starting isolated Steam client under $P2_HOME"
  steam_env p2 steam -master_ipc_name_override "$P2_STEAM_IPC" -userchooser -nochatui -nofriendsui
}

cmd_clone_p2() {
  require_linux
  assert_isolated_paths
  [[ -f "$P1_GAME_DIR/eldenring.exe" ]] || die "P1 game is missing: $P1_GAME_DIR"
  [[ ! -e "$P2_GAME_DIR" ]] || die "P2 game already exists; refusing to overwrite: $P2_GAME_DIR"
  command -v cp >/dev/null || die 'cp is required'
  mkdir -p "$P2_LIBRARY/steamapps/common"
  note 'reflink-cloning ELDEN RING for an independent, copy-on-write P2 install'
  cp -a --reflink=always "$P1_LIBRARY/steamapps/common/ELDEN RING" "$P2_LIBRARY/steamapps/common/"
  if [[ -f "$P1_LIBRARY/steamapps/appmanifest_1245620.acf" ]]; then
    cp -a --reflink=always "$P1_LIBRARY/steamapps/appmanifest_1245620.acf" "$P2_LIBRARY/steamapps/"
  fi
  note 'clone complete; add the P2 library in the isolated Steam client and verify installed files once'
}

cmd_check() {
  require_linux
  local failed=0 path
  for path in steam gamescope python3 realpath; do
    if command -v "$path" >/dev/null; then note "ok: $path"; else note "missing command: $path"; failed=1; fi
  done
  assert_isolated_paths
  if python3 -c 'import Xlib' >/dev/null 2>&1; then note 'ok: python-xlib'; else note 'missing Python module: Xlib'; failed=1; fi
  [[ "$P1_HOME" != "$P2_HOME" ]] || { note 'invalid: Steam homes are identical'; failed=1; }
  [[ "$P1_GAME_DIR" != "$P2_GAME_DIR" ]] || { note 'invalid: game directories are identical'; failed=1; }
  for path in "$P1_GAME_DIR/eldenring.exe" "$P2_GAME_DIR/eldenring.exe"; do
    if [[ -f "$path" ]]; then note "ok: $path"; else note "missing: $path"; failed=1; fi
  done
  for path in "$P1_LIBRARY/steamapps/compatdata/$APPID" "$P2_LIBRARY/steamapps/compatdata/$APPID"; do
    if [[ -d "$path" ]]; then note "ok: $path"; else note "missing Proton prefix: $path"; failed=1; fi
  done
  [[ -x "$WRAPPER" ]] || { note "not executable: $WRAPPER"; failed=1; }
  note "P1 prefix: $P1_LIBRARY/steamapps/compatdata/$APPID"
  note "P2 prefix: $P2_LIBRARY/steamapps/compatdata/$APPID"
  return "$failed"
}

cmd_apply() {
  require_linux
  local build=${1:-build}
  cmd_check
  assert_no_game_processes
  if [[ "$build" == build ]]; then
    (cd "$ROOT" && cargo build --profile diag)
  elif [[ "$build" != no-build ]]; then
    die 'apply accepts only --no-build'
  fi
  GAME_DIR="$P1_GAME_DIR" BACKUP_DIR="$P1_BACKUP" "$ROOT/scripts/rig.sh" apply --no-build --auto-session host
  GAME_DIR="$P2_GAME_DIR" BACKUP_DIR="$P2_BACKUP" "$ROOT/scripts/rig.sh" apply --no-build --auto-session join
}

cmd_restore() {
  require_linux
  cmd_kill all
  assert_no_game_processes
  GAME_DIR="$P1_GAME_DIR" BACKUP_DIR="$P1_BACKUP" "$ROOT/scripts/rig.sh" restore
  GAME_DIR="$P2_GAME_DIR" BACKUP_DIR="$P2_BACKUP" "$ROOT/scripts/rig.sh" restore
}

cmd_launch_one() {
  local instance=$1
  record_log_baseline "$instance"
  note "launching $instance through its isolated Steam client"
  if [[ "$instance" == p2 ]]; then
    steam_env p2 steam -master_ipc_name_override "$P2_STEAM_IPC" -silent -applaunch "$APPID" >/dev/null 2>&1 &
  else
    steam_env p1 steam -silent -applaunch "$APPID" >/dev/null 2>&1 &
  fi
}

cmd_kill() {
  require_linux
  local target=${1:-all} instance pid wrapper_pid
  [[ "$target" == all || "$target" == p1 || "$target" == p2 ]] || die 'kill target must be p1, p2, or all'
  for instance in p1 p2; do
    [[ "$target" == all || "$target" == "$instance" ]] || continue
    while read -r pid; do
      [[ -n "$pid" ]] || continue
      note "stopping $instance pid $pid"
      kill "$pid" 2>/dev/null || true
    done < <(instance_pids "$instance")
  done
  for instance in p1 p2; do
    [[ "$target" == all || "$target" == "$instance" ]] || continue
    for _ in $(seq 1 10); do
      [[ -z $(instance_pids "$instance") ]] && break
      sleep 1
    done
    while read -r pid; do [[ -n "$pid" ]] && kill -9 "$pid" 2>/dev/null || true; done < <(instance_pids "$instance")
    if [[ -f "$DUO_ROOT/runtime/$instance/wrapper.pid" ]]; then
      wrapper_pid=$(tr -d '\r\n' < "$DUO_ROOT/runtime/$instance/wrapper.pid")
      if [[ "$wrapper_pid" =~ ^[0-9]+$ && -r "/proc/$wrapper_pid/environ" ]] \
        && tr '\0' '\n' < "/proc/$wrapper_pid/environ" | grep -qx "UNSEAMLESS_DUO_INSTANCE=$instance" \
        && grep -qa gamescope "/proc/$wrapper_pid/cmdline"; then
        note "stopping $instance gamescope pid $wrapper_pid"
        kill "$wrapper_pid" 2>/dev/null || true
        for _ in $(seq 1 5); do [[ ! -d "/proc/$wrapper_pid" ]] && break; sleep 1; done
        [[ -d "/proc/$wrapper_pid" ]] && kill -9 "$wrapper_pid" 2>/dev/null || true
      fi
    fi
  done
}

cmd_cycle() {
  require_linux
  cmd_check
  cmd_kill all
  cmd_apply "${1:-build}"
  cmd_launch_one p1
  wait_log p1 'unseamless-coop installed' 'framework installed'
  dismiss_to_world p1
  wait_log p1 'auto-session: opening world' 'auto host fired'
  cmd_launch_one p2
  wait_log p2 'unseamless-coop installed' 'framework installed'
  dismiss_to_world p2
  wait_log p2 'auto-session: joining world' 'auto join fired'
  cmd_verify
}

cmd_verify() {
  require_linux
  local timeout=${UNSEAMLESS_DUO_VERIFY_TIMEOUT:-180} p1log p2log evidence="$DUO_ROOT/evidence"
  mkdir -p "$evidence"
  for _ in $(seq 1 "$timeout"); do
    p1log=$(latest_log p1); p2log=$(latest_log p2)
    if [[ -n "$p1log" && -n "$p2log" ]] \
      && grep -Eq 'presence-probe: roster .*phantoms=[1-9].*active=[1-9].*remote=[1-9]' "$p1log" \
      && grep -Eq 'presence-probe: roster .*phantoms=[1-9].*active=[1-9].*remote=[1-9]' "$p2log"; then
      cp "$p1log" "$evidence/p1.log"
      cp "$p2log" "$evidence/p2.log"
      note "PASS: both clients independently observed a two-player roster; evidence: $evidence"
      return
    fi
    sleep 1
  done
  p1log=$(latest_log p1); p2log=$(latest_log p2)
  [[ -n "$p1log" ]] && cp "$p1log" "$evidence/p1.log"
  [[ -n "$p2log" ]] && cp "$p2log" "$evidence/p2.log"
  if [[ -n "$p1log" && -n "$p2log" ]] \
    && grep -q 'players=2' "$p1log" && grep -q 'players=2' "$p2log"; then
    note "FAIL: both game rosters reached players=2, but neither client observed an active remote ChrIns within ${timeout}s"
  else
    note "FAIL: the pair did not reach a symmetric players=2 roster within ${timeout}s"
  fi
  note "evidence: $evidence"
  return 1
}

cmd_status() {
  local instance log
  for instance in p1 p2; do
    note "$instance pids: $(instance_pids "$instance" | tr '\n' ' ')"
    log=$(latest_log "$instance")
    if [[ -n "$log" ]]; then note "$instance log: $log"; else note "$instance log: none"; fi
  done
}

cmd_logs() {
  local instance=${1:-p1} log
  [[ "$instance" == p1 || "$instance" == p2 ]] || die 'logs target must be p1 or p2'
  log=$(latest_log "$instance")
  [[ -n "$log" ]] || die "no $instance log"
  if [[ ${2:-} == -f ]]; then tail -f "$log"; else cat "$log"; fi
}

usage() {
  cat <<'EOF'
Usage: scripts/local-duo.sh COMMAND

  setup             create the P2 layout and print the two one-time Steam launch options
  steam-p2          start the isolated P2 Steam client for login/configuration
  clone-p2          reflink-clone P1's game into an independent P2 library
  check              validate dependencies and instance isolation
  apply [--no-build] install host/join configs into both independent game directories
  restore           stop both clients and restore each original mod stack
  cycle [--no-build] kill, apply, launch, enter world, and assert both two-player views
  verify             assert the already-running pair and save both logs as evidence
  status             show instance-scoped processes and logs
  logs p1|p2 [-f]    print or follow one instance's latest log
  kill [p1|p2|all]  stop only the selected local-duo processes
EOF
}

case ${1:-help} in
  setup) cmd_setup ;;
  steam-p2) cmd_steam_p2 ;;
  clone-p2) cmd_clone_p2 ;;
  check) cmd_check ;;
  apply) shift; if [[ ${1:-} == --no-build ]]; then cmd_apply no-build; else cmd_apply build; fi ;;
  restore) cmd_restore ;;
  cycle) shift; if [[ ${1:-} == --no-build ]]; then cmd_cycle no-build; else cmd_cycle build; fi ;;
  verify) cmd_verify ;;
  status) cmd_status ;;
  logs) shift; cmd_logs "$@" ;;
  kill) shift; cmd_kill "${1:-all}" ;;
  help|-h|--help) usage ;;
  *) usage >&2; exit 2 ;;
esac
