#!/usr/bin/env bash
# win.sh — drive the native-Windows DX12 present-hook validation harness in the local Windows 11 VM.
#
# The overlay (hudhook DX12 + imgui, coop/overlay.rs) renders on our Linux rig (vkd3d/Proton) but
# CRASHES on native Windows NVIDIA at the first hooked Present (docs/OVERLAY-RENDERING.md). We were
# debugging that blind with no Windows box. This drives `crates/dx12-harness` (a minimal D3D12 app +
# the same hook) in the existing quickemu Win11 VM, so the crash path runs on a real Windows loader
# without ELDEN RING. See the `/windows-test` skill for the workflow and the fidelity ladder.
#
# Everything is MANUAL except apply/run: YOU boot the VM (`cd ~/VMs && quickemu --vm windows-11.conf`)
# and watch the window; this script builds the exe HERE, copies it in, runs it, and pulls the log back.
# Copy path is SSH/SCP over the port quickemu already forwards (host :22220 -> guest :22). One-time
# guest setup (enable OpenSSH Server, trust an SSH key) is `win.sh setup-help`.
#
# Verbs:  build [--diag] | push | run | pull-log | apply | cycle | status | shell | setup-help | paths
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TRIPLE="x86_64-pc-windows-gnu"

# ---- config (env-overridable) -------------------------------------------------------------------
WIN_HOST="${WIN_HOST:-localhost}"          # the VM, reachable on the forwarded port below
WIN_USER="${WIN_USER:-quickemu}"           # the existing Win11 VM's account (quickemu's default user)
WIN_PORT="${WIN_PORT:-22220}"             # quickemu's default host->guest:22 forward (check boot output)
WIN_REMOTE_DIR="${WIN_REMOTE_DIR:-dx12-harness}"   # staging dir under the guest user's home
WIN_REMOTE_LOG="${WIN_REMOTE_LOG:-dx12-harness.log}" # log filename, written inside WIN_REMOTE_DIR
LOCAL_LOG="${LOCAL_LOG:-$ROOT/target/win-test/dx12-harness.log}"  # where pull-log drops it locally
# Seconds to wait for the interactive run task to finish. Must exceed the harness's own runtime:
# DX12_HARNESS_FRAMES frames at the vsync cadence (~25s for the 1500-frame / vsync=1 default). Raise it
# if you bump FRAMES or hit slow WARP; the run reports a TIMEOUT line (not a hang) if it's too low.
WIN_RUN_TIMEOUT="${WIN_RUN_TIMEOUT:-180}"
PROFILE="release"                          # flipped to "diag" by `build --diag`
# Extra ssh/scp flags via WIN_SSH_OPTS (e.g. a custom IdentityFile / ProxyJump); values must not
# contain spaces. ssh takes -p, scp takes -P, so SCP_OPTS mirrors SSH_OPTS with the uppercase port
# flag — both pick up WIN_SSH_OPTS so push/run/pull honour the same overrides as ssh/status.
# shellcheck disable=SC2206
SSH_OPTS=(-p "$WIN_PORT" -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new ${WIN_SSH_OPTS:-})
# shellcheck disable=SC2206
SCP_OPTS=(-P "$WIN_PORT" -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new ${WIN_SSH_OPTS:-})
SSH_TGT="${WIN_USER}@${WIN_HOST}"

# ---- output helpers -----------------------------------------------------------------------------
say()  { printf '\033[1;35m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  !\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

exe_path() { echo "$ROOT/target/$TRIPLE/$PROFILE/dx12-harness.exe"; }

# Run a PowerShell command in the guest. Pass it via -EncodedCommand (base64 UTF-16LE): the guest's
# default OpenSSH shell is cmd.exe, which would otherwise parse any `|`/`>`/`&`/quotes in the PowerShell
# text before powershell.exe ever sees it (e.g. `... | Out-Null` made cmd try to run Out-Null). Encoding
# makes the whole script one cmd-safe token, so quoting/pipes inside the command are immune.
win_ps() {
  local b64
  b64="$(printf '%s' "$1" | iconv -f UTF-8 -t UTF-16LE | base64 -w0)"
  ssh "${SSH_OPTS[@]}" "$SSH_TGT" "powershell -NoProfile -EncodedCommand $b64"
}

# Emit the per-run harness knobs as KEY=VALUE lines for knobs.txt (read by run.ps1 inside the task —
# the scheduled task runs in a fresh session and does NOT inherit this SSH session's env, so knobs must
# travel as a file, not `$env:` in our command). Forwards any DX12_HARNESS_* the caller set, then fills
# two VM-specific defaults:
#   - WARP=1 : the VM's virtio GPU exposes no D3D12, so the default adapter fails with
#              DXGI_ERROR_NOT_CURRENTLY_AVAILABLE (0x887A0022). WARP (software D3D12) is required here.
#              On a GPU-passthrough or native target, pass DX12_HARNESS_WARP=0 to use the real GPU.
#   - FRAMES=1500 : so a HEALTHY (non-crashing) run self-terminates instead of presenting forever.
# DX12_HARNESS_LOG is intentionally omitted — run.ps1 sets it to the staging dir itself.
knobs_lines() {
  local name val has_warp=0 has_frames=0 out=""
  while IFS='=' read -r name val; do
    [[ $name == DX12_HARNESS_* ]] || continue
    [[ $name == DX12_HARNESS_LOG ]] && continue
    out+="${name}=${val}"$'\n'
    [[ $name == DX12_HARNESS_WARP ]] && has_warp=1
    [[ $name == DX12_HARNESS_FRAMES ]] && has_frames=1
  done < <(env)
  [[ $has_warp == 1 ]]   || out+="DX12_HARNESS_WARP=1"$'\n'
  [[ $has_frames == 1 ]] || out+="DX12_HARNESS_FRAMES=1500"$'\n'
  printf '%s' "$out"
}

# ---- verbs --------------------------------------------------------------------------------------
cmd_build() {
  local extra=()
  [[ "${1:-}" == "--diag" ]] && { PROFILE="diag"; extra=(--profile diag); }
  [[ "${1:-}" == "--diag" ]] || extra=(--release)
  say "Building dx12-harness ($PROFILE) for $TRIPLE"
  ( cd "$ROOT" && cargo build -p dx12-harness "${extra[@]}" )
  ok "built $(exe_path)"
}

cmd_push() {
  local exe; exe="$(exe_path)"
  [[ -f "$exe" ]] || die "not built: $exe  (run: win.sh build)"
  say "Staging $WIN_REMOTE_DIR/ in the guest and copying the exe + run.ps1"
  win_ps "New-Item -ItemType Directory -Force -Path \$HOME\\$WIN_REMOTE_DIR | Out-Null" \
    || die "ssh to $SSH_TGT:$WIN_PORT failed — is the VM booted with OpenSSH Server? (win.sh setup-help)"
  scp "${SCP_OPTS[@]}" "$exe" "$SSH_TGT:$WIN_REMOTE_DIR/dx12-harness.exe"
  # run.ps1 is the interactive-session wrapper the scheduled task executes (see cmd_run / the .ps1 head).
  scp "${SCP_OPTS[@]}" "$ROOT/scripts/win/run.ps1" "$SSH_TGT:$WIN_REMOTE_DIR/run.ps1"
  ok "pushed dx12-harness.exe + run.ps1 -> $SSH_TGT:~/$WIN_REMOTE_DIR/"
}

cmd_run() {
  # The harness creates a DXGI swapchain, which needs a real window station / desktop. An SSH session
  # has none, so running the exe directly over SSH fails with DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
  # (0x887A0022). So we drive it via an Interactive-principal SCHEDULED TASK that lands in the
  # logged-on desktop session, executing run.ps1 there. REQUIRES a user logged into the VM's desktop.
  local knobs; knobs="$(knobs_lines)"
  say "Running the harness in the guest (interactive task). knobs:"
  printf '%s' "$knobs" | sed 's/^/    /'

  # 1) Ship knobs.txt (scp avoids any quoting of the KEY=VALUE lines).
  local tmp; tmp="$(mktemp)"; printf '%s' "$knobs" > "$tmp"
  scp "${SCP_OPTS[@]}" "$tmp" "$SSH_TGT:$WIN_REMOTE_DIR/knobs.txt" \
    || { rm -f "$tmp"; die "couldn't write knobs.txt — is the exe pushed? (win.sh push)"; }
  rm -f "$tmp"

  # 2) Reap any lingering prior run (a hung/long harness keeps the swapchain + would make
  #    Start-ScheduledTask a no-op under the default IgnoreNew policy, so a new run would silently watch
  #    the OLD instance). Then clear stale artifacts, register + start the interactive task, and poll for
  #    the COMPLETION SENTINEL — run.ps1 writes exitcode.txt as its last line, so its (re)appearance is a
  #    definite "this run finished", immune to the scheduled-task LastTaskResult lead/trailing-edge race.
  win_ps "
\$dir = Join-Path \$env:USERPROFILE '$WIN_REMOTE_DIR'
Get-ScheduledTask -TaskName dx12h -ErrorAction SilentlyContinue | Stop-ScheduledTask -ErrorAction SilentlyContinue
Get-Process dx12-harness -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path \$dir 'exitcode.txt'),(Join-Path \$dir 'run-out.txt'),(Join-Path \$dir '$WIN_REMOTE_LOG') -ErrorAction SilentlyContinue
\$arg = '-NoProfile -ExecutionPolicy Bypass -File \"' + (Join-Path \$dir 'run.ps1') + '\"'
\$a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument \$arg
\$p = New-ScheduledTaskPrincipal -UserId \$env:USERNAME -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName dx12h -Action \$a -Principal \$p -Force | Out-Null
Start-ScheduledTask -TaskName dx12h
\$ec = Join-Path \$dir 'exitcode.txt'
\$n=0; while (-not (Test-Path \$ec) -and \$n -lt $WIN_RUN_TIMEOUT) { Start-Sleep -Seconds 1; \$n++ }
\$st = (Get-ScheduledTaskInfo -TaskName dx12h).State
if (Test-Path \$ec) { ('  run completed in {0}s (task state {1})' -f \$n,\$st) }
else { ('  TIMEOUT after {0}s with no exitcode.txt (task state {1}) — desktop session logged in? raise WIN_RUN_TIMEOUT?' -f \$n,\$st) }
" || warn "task driver returned non-zero"

  # 3) Pull artifacts: the harness log + the task's captured stdout/err + exit code.
  local dir; dir="$(dirname "$LOCAL_LOG")"; mkdir -p "$dir"
  local f
  for f in "$WIN_REMOTE_LOG" run-out.txt exitcode.txt; do
    scp "${SCP_OPTS[@]}" \
      "$SSH_TGT:$WIN_REMOTE_DIR/$f" "$dir/$f" 2>/dev/null || warn "no $f produced (run may not have started — is a desktop session logged in?)"
  done
  [[ -f "$dir/$WIN_REMOTE_LOG" && "$dir/$WIN_REMOTE_LOG" != "$LOCAL_LOG" ]] && cp -f "$dir/$WIN_REMOTE_LOG" "$LOCAL_LOG"
  echo "----- exitcode -----"; cat "$dir/exitcode.txt" 2>/dev/null || echo "(none)"
  echo "----- harness log tail -----"; tail -n 30 "$LOCAL_LOG" 2>/dev/null || echo "(no log)"
  ok "run finished (full log: $LOCAL_LOG; task stdout/err: $dir/run-out.txt)"
}

cmd_pull_log() {
  mkdir -p "$(dirname "$LOCAL_LOG")"
  say "Pulling the harness log -> $LOCAL_LOG"
  scp "${SCP_OPTS[@]}" \
    "$SSH_TGT:$WIN_REMOTE_DIR/$WIN_REMOTE_LOG" "$LOCAL_LOG" || die "no remote log at ~/$WIN_REMOTE_DIR/$WIN_REMOTE_LOG yet"
  ok "saved $LOCAL_LOG"
  echo "----- tail -----"
  tail -n 40 "$LOCAL_LOG"
}

cmd_status() {
  say "Probing $SSH_TGT on port $WIN_PORT"
  win_ps 'Write-Host "guest: $env:COMPUTERNAME  user: $env:USERNAME  $([Environment]::OSVersion.VersionString)"' \
    || die "no SSH to the guest — boot the VM and run win.sh setup-help"
  ok "guest reachable"
}

cmd_shell() { exec ssh "${SSH_OPTS[@]}" "$SSH_TGT"; }

cmd_paths() {
  cat <<EOF
host exe : $(exe_path)
ssh      : ssh -p $WIN_PORT $SSH_TGT
guest dir: ~/$WIN_REMOTE_DIR  (exe + $WIN_REMOTE_LOG)
local log: $LOCAL_LOG
EOF
}

cmd_setup_help() {
  cat <<'EOF'
One-time guest (Windows VM) setup so win.sh can copy + run over SSH:

1. Boot the VM:        cd ~/VMs && quickemu --vm windows-11.conf
   Note the SSH line quickemu prints, e.g. "ssh ... -p 22220" — that port is WIN_PORT.

2. In the VM (PowerShell as Administrator), enable OpenSSH Server:
     Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
     Start-Service sshd
     Set-Service -Name sshd -StartupType Automatic

   GOTCHA (silently drops inbound SSH): the capability's firewall rule is scoped Private/Domain, but
   the qemu user-mode NIC lands in the PUBLIC profile, so inbound 22 is dropped and ssh just times out.
   Fix both — mark the network Private AND broaden the rule to all profiles:
     Set-NetConnectionProfile -InterfaceAlias Ethernet -NetworkCategory Private
     Set-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -Profile Any

3. Trust this PC's SSH key (so win.sh runs key-only, no password prompt). On THIS Linux box:
     ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519        # if you don't already have a key
   Then copy your PUBLIC key into the VM. Easiest: in the VM PowerShell, paste your
   ~/.ssh/id_ed25519.pub contents into:
     Add-Content "$env:USERPROFILE\.ssh\authorized_keys" "<paste-the-pub-key-line>"
   For a NON-admin guest account that file is what OpenSSH reads. (If the guest user is an
   admin, OpenSSH instead reads C:\ProgramData\ssh\administrators_authorized_keys — append there.)

4. Confirm from this box:  scripts/win.sh status

Two things `run` REQUIRES (not just setup):
  - A user must be LOGGED INTO the VM's desktop. `run` launches the harness via an Interactive
    scheduled task so the DXGI swapchain gets a real window station; with no desktop session logged
    in, the task can't land and no artifacts are produced.
  - The VM has no hardware D3D12 (virtio GPU), so the harness MUST use WARP — `run` defaults
    DX12_HARNESS_WARP=1. A default-adapter run fails with 0x887A0022. Pass DX12_HARNESS_WARP=0 only on
    a GPU-passthrough / native target.

Notes:
  - WIN_USER defaults to 'quickemu' (this VM's account). Override if yours differs.
  - If port 22220 is wrong, set WIN_PORT to whatever quickemu printed.
  - win.sh sends PowerShell via -EncodedCommand, so the guest's default OpenSSH shell can stay cmd.exe
    (it would otherwise mis-parse pipes/quotes in the command).
  - No WebDAV/clipboard share on the GTK display: to hand a file in WITHOUT SSH, build a CD-ROM ISO
    (genisoimage) and hot-swap it via the qemu monitor (`change ide2-cd0 <iso>` on
    ~/VMs/windows-11/windows-11-monitor.socket), then right-click a .ps1 > Run with PowerShell.
EOF
}

cmd_apply() { cmd_build "$@"; cmd_push; }                 # the automated "apply the mod" step
cmd_cycle() { cmd_build "${1:-}"; cmd_push; cmd_run; cmd_pull_log; }

case "${1:-}" in
  build)       shift; cmd_build "$@";;
  push)        cmd_push;;
  run)         cmd_run;;
  pull-log|log) cmd_pull_log;;
  apply)       shift; cmd_apply "$@";;
  cycle)       shift; cmd_cycle "$@";;
  status)      cmd_status;;
  shell)       cmd_shell;;
  paths)       cmd_paths;;
  setup-help|setup) cmd_setup_help;;
  ""|-h|--help|help)
    cat <<EOF
win.sh — drive the dx12-harness native-Windows overlay test in the Win11 VM.

  build [--diag]   cross-compile the harness exe here (release default)
  push             copy the exe into the running VM (SSH/SCP over :$WIN_PORT)
  run              run it in the VM (forwards DX12_HARNESS_* env knobs)
  pull-log         copy the harness log back here and tail it
  apply [--diag]   build + push (the automated "apply" step)
  cycle [--diag]   build + push + run + pull-log
  status           check the VM is reachable over SSH
  shell            interactive ssh into the VM
  paths            print the resolved paths/targets
  setup-help       one-time guest OpenSSH setup instructions

First time? Boot the VM, then: scripts/win.sh setup-help
Env: WIN_USER (default quickemu), WIN_PORT (quickemu's forward), WIN_HOST, WIN_RUN_TIMEOUT,
     DX12_HARNESS_* knobs (run forces WARP=1; needs a desktop session logged into the VM).
EOF
    ;;
  *) die "unknown verb '$1' (try: win.sh help)";;
esac
