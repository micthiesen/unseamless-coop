---
name: windows-test
description: Validate the overlay's DX12 present-hook on a REAL Windows loader (not vkd3d/Proton) using the local Windows 11 VM, to chase the native-Windows overlay crash (docs/OVERLAY-RENDERING.md). Drives crates/dx12-harness (a minimal D3D12 app + the same hudhook hook) via scripts/win.sh — build here, copy into the VM, run, pull the log. Use when verifying an imgui/DX12 overlay-injection fix, reproducing the native-Windows present-hook crash, or anything "test on Windows / in the VM / on a real Windows box". TRIGGER on "windows vm", "native windows", "dx12 harness", "win.sh", "overlay crash on windows", "test the overlay on windows", "imgui injection crash".
---

# Windows overlay-injection testing (the local Win11 VM)

The in-game overlay (hudhook DX12 present-hook + Dear ImGui, `coop/overlay.rs`) renders fine on our
Linux rig (vkd3d/Proton) but **crashes on native Windows NVIDIA** at the first hooked `Present` (first
friend test, RTX 3080 — full anatomy in [`docs/OVERLAY-RENDERING.md`](../../docs/OVERLAY-RENDERING.md)
> "Native-Windows Crash"). That was investigated *blind* because there was no Windows box. This skill
is the Windows box stand-in: it runs the crashing machinery on a **real Windows loader** in the
existing quickemu Win11 VM, with **no ELDEN RING**.

## What it is

`crates/dx12-harness` is a minimal D3D12 app (window + clear-color present loop) that, after a warmup,
injects the **same** hudhook DX12 present-hook + the **same** imgui font bake the overlay uses into its
own live swapchain, mid-flight, from a side thread — exactly how `coop/overlay.rs::install` does it
in-game. `scripts/win.sh` builds it here, copies it into the running VM (SSH/SCP), runs it, and pulls
its log back. The harness log mirrors the overlay's breadcrumb lines and forces per-record flushing, so
it diffs directly against the rig baseline in the doc and a crash can't eat the decisive tail line.

**Result so far (2026-06-28):** the VM run was **CLEAN on WARP** — hook injected mid-flight, CQ found at
offset +0x138, `initialize() reached` → `first render frame reached`, 5690 hooked-Present frames, no
crash. That **ruled out** the hardware-independent MinHook mechanism (hyp #1) and the imgui font upload
(hyp #3): the native crash is **NVIDIA-driver-specific**, which WARP can't reproduce. The harness did
its job (narrowed the space); the remaining gate is the friend's machine or GPU passthrough. Re-running
is still useful to re-confirm after a hudhook/imgui change, or to A/B a candidate fix's *mechanism*.

## What it can and can't tell you (read before trusting a result)

The VM has **no GPU passthrough** — Windows in it runs D3D12 via **WARP (Microsoft's software D3D12)**
or virtio-gpu, *not* the NVIDIA driver. So it's a real native Windows DXGI/D3D12 **loader + present
path** (genuinely different from vkd3d), but **not NVIDIA hardware**. The fidelity ladder:

| Reproduces? | Covers | Where |
|---|---|---|
| MinHook detour on a live swapchain vtable, off-thread (hyp #1 mechanism) | ✅ ran clean — ruled out as the cause | this VM |
| imgui DX12 font bake + GPU upload (hyp #3) | ✅ ran clean — ruled out as the cause | this VM |
| ELDEN RING's exact swapchain flags | ⚠️ only if mirrored via env knobs (pin them with a rig probe) | this VM |
| DLSS swapchain interposer (hyp #2) | ❌ | friend's real machine |
| NVIDIA-driver-specific present threading (hyp #1 trigger) | ❌ (no NVIDIA in a VM) | GPU passthrough / friend |

**Decision tree:** if the harness **crashes** in the VM, you have a fast local repro — iterate the fix
here and confirm it stops crashing. If it **doesn't crash even unfixed**, you've *learned* the bug needs
real NVIDIA hardware (narrows it), and the VM can't validate the fix — escalate. A fix is only
"~95% validated" once (a) the harness reproduced and then stopped after the fix, OR (b) you accept the
VM can't trigger it and lean on the friend gate. The **friend's native machine is the single
super-validated final gate** — only spend it when you're already ~95% confident from the lighter loop.

## First-time setup (one-time, manual)

You boot and watch the VM; the script does the rest. One-time guest setup enables SSH:

```bash
scripts/win.sh setup-help     # prints the exact steps
```

Summary: boot the VM (`cd ~/VMs && quickemu --vm windows-11.conf`), enable OpenSSH Server in the guest,
append this box's `~/.ssh/id_ed25519.pub` to the guest's `authorized_keys` (or
`administrators_authorized_keys` for an admin account), then `scripts/win.sh status` to confirm.
`WIN_USER` defaults to **`quickemu`** (this VM's account); `WIN_PORT` defaults to quickemu's `22220`.

Field gotchas that bite on first setup:

- **Inbound SSH silently dropped.** The OpenSSH capability's firewall rule is scoped Private/Domain, but
  the qemu user-mode NIC lands in the **Public** profile, so port 22 is dropped and ssh just times out.
  Fix both: `Set-NetConnectionProfile -InterfaceAlias Ethernet -NetworkCategory Private` **and**
  `Set-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -Profile Any`.
- **No clipboard / WebDAV on the GTK display.** To hand a file in without SSH, build a CD-ROM ISO
  (`genisoimage`) and hot-swap it via the qemu monitor (`change ide2-cd0 <iso>` on
  `~/VMs/windows-11/windows-11-monitor.socket`), then right-click a `.ps1` → Run with PowerShell.

`scripts/win.sh setup-help` prints the exact commands.

### `run` needs more than SSH (the load-bearing gotcha)

A DXGI swapchain needs a real **window station / desktop**, which an SSH (non-interactive) session
lacks — running the exe straight over SSH fails with `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` (`0x887A0022`).
So `win.sh run` launches the harness via an **Interactive-principal scheduled task**
(`scripts/win/run.ps1`) that lands in the logged-on desktop session. Two consequences:

- **A user must be logged into the VM's desktop** when you `run`, or the task can't land and no
  artifacts are produced.
- **WARP is required** (`run` forces `DX12_HARNESS_WARP=1`): the VM's virtio GPU exposes no D3D12, so
  the default adapter fails the same `0x887A0022`. Pass `DX12_HARNESS_WARP=0` only on a
  GPU-passthrough / native target.

## The loop

```bash
# You: boot the VM and LOG INTO its desktop (the interactive task needs a desktop session).
scripts/win.sh cycle          # build + push + run + pull-log (one shot)
# or step by step:
scripts/win.sh build          # cross-compile the exe here (release; --diag for symbols)
scripts/win.sh push           # copy exe + run.ps1 into the VM  (this is the automated "apply")
scripts/win.sh run            # run it in the VM via the interactive task (forces WARP); pulls artifacts
scripts/win.sh pull-log       # re-pull the harness log + tail it
```

`run` writes the env knobs to `knobs.txt`, fires the scheduled task, waits for it to finish
(`WIN_RUN_TIMEOUT`, default 120s), then pulls the harness log + the task's `run-out.txt` + `exitcode.txt`
into `target/win-test/`.

Read the pulled log against the doc's rig baseline. Key lines:
`overlay: DX12 present-hook installed` → `hudhook initialize() reached (baking fonts)` →
`first render frame reached` → hudhook's `Call IDXGISwapChain::Present trampoline` /
`Found command queue pointer …`. If the log ends after `present-hook installed` with no
`initialize() reached`, the hook died before calling us (hyp #1); if it dies after `initialize()` but
before `first render frame`, suspect the imgui font upload (hyp #3).

### Tuning the run (env knobs, all forwarded by `win.sh run`)

`run` already forces `DX12_HARNESS_WARP=1` and a finite `DX12_HARNESS_FRAMES`; override any knob on the
command line:

```bash
DX12_HARNESS_HOOK_THREAD=0 scripts/win.sh run # install the hook inline, not off-thread
DX12_HARNESS_NO_HOOK=1  scripts/win.sh run   # control run: present without ever hooking
DX12_HARNESS_BUFFERS=2 DX12_HARNESS_VSYNC=0 scripts/win.sh run  # mirror ER's swapchain (pin via a rig probe)
DX12_HARNESS_WARMUP=300 DX12_HARNESS_FRAMES=2000 scripts/win.sh run  # present longer before/after hooking
DX12_HARNESS_WARP=0     scripts/win.sh run   # use the real GPU (ONLY on a passthrough / native target)
```

Note `format`, `swap effect`, window size, and swapchain `flags` are currently hardcoded in the harness
(`crates/dx12-harness/src/main.rs`); only buffers + vsync are env-tunable. If a rig probe shows ER uses
e.g. `ALLOW_TEARING` or a waitable swapchain, extend the harness to expose those before mirroring.

To raise VM fidelity toward ER, capture ER's real `DXGI_SWAP_CHAIN_DESC1` (buffer count, format, flags,
swap effect) from inside our present hook on the rig, then set the matching `DX12_HARNESS_*` knobs. That
is an **orchestrator** task (it drives the rig) — as a worker, ask for it rather than running the rig.

## Boundaries

- **You never drive the rig or the game from here** — that's the orchestrator's. This skill is the VM
  only. The rig baseline + any ER-swapchain probe are requested from the orchestrator/Michael.
- **The VM is the cheap filter, not the verdict.** A clean VM run is necessary, not sufficient; the
  friend test is the real gate. Don't report a Windows crash "fixed" off a VM run alone unless the VM
  actually reproduced it first.
- If WARP can't reproduce and you need real NVIDIA, the higher-fidelity options are single-GPU VFIO
  passthrough of the RTX 5080 into the VM (heavy: the host desktop goes dark while the VM holds the
  GPU) or a native Windows dual-boot. Both are Michael's call, not something to start unprompted.

## Files

- `crates/dx12-harness/` — the harness (D3D12 presenter + injected hudhook hook).
- `scripts/win.sh` — build/push/run/pull-log driver for the VM.
- [`docs/OVERLAY-RENDERING.md`](../../docs/OVERLAY-RENDERING.md) — the overlay design + the
  "Native-Windows Crash" anatomy, hypotheses, and the rig baseline to diff against.
- VM: `~/VMs/windows-11.conf` (quickemu); research notes in `~/.research/qemu-windows-vm.md`.
