# Roadmap

What's built vs. what's next, grouped by what **gates** each item. Detail lives in the linked design
docs; this is the map. Work proceeds in **waves** (one fleet batch each — see
[ORCHESTRATION.md](ORCHESTRATION.md)).

## Wave 1 — DONE (2026-06)

Shipped to `main`, rig-verified where applicable:

- **Skip intros**, **separate co-op saves** (`.co2`/`.uco`), **offline/non-EAC launch** + EAC guard,
  the **`dinput8.dll` proxy loader**, **config + settings registry**, the **diag/log model**.
- **Boot volume**, **world-time lock** (FrameBegin re-assert; boot-volume re-asserts through the
  saved-options clobber).
- **Scaling** — per-player enemy/boss HP/damage/posture via the multiplayer `SpEffectParam` rate rows
  (rig-verified writes; in-combat effect is 2-player-gated). See [SCALING.md](SCALING.md).
- **Death debuffs** — stacking penalty tiers, cured at a Site of Grace (flag 9000), repurposed clean
  rows, ER-voiced toasts. See [DEATH-DEBUFFS.md](DEATH-DEBUFFS.md).
- **Overlay** (hudhook DX12 + imgui): notifications, session-action menu, settings/log tabs,
  column-major debug panel with live **vitals + status** readout. See [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md).
- Host hardening (narrowed live-config writes, host-tested queues), cdylib hygiene (typed
  `HookError`, FFI annotations).
- **Shipping `panic=unwind` + a "feature disabled" toast.** Every game→us FFI entry point is
  firewalled with `catch_unwind` ([FFI-UNWIND-AUDIT.md](FFI-UNWIND-AUDIT.md)), so release/shipping now
  builds with `panic=unwind` like `diag` — a feature panic is caught, disabled, and toasted (plain
  voice) instead of crashing the player's game.

## Wave 2 — next (not started)

### Solo / host-doable (no 2nd player needed)

- **Rung 4 — Steam lobby discovery (password-keyed).** *Independent of rung 3* — lobbies replace the
  manual SteamID copy-paste that seeds the rung-2 side-channel; they don't put players in one another's
  world (that's still rung 3). So most of this is buildable off-rig now, ahead of any 2-player RE. The
  build order (full spec in [COOP-CONNECTION.md](COOP-CONNECTION.md) > rung 4):
  - **Harness prototype (fully solo, zero rig).** The [`harness`](../crates/harness) crate is a normal
    native exe and *can* take `steamworks-rs`; prove `CreateLobby` → `SetLobbyData("usc_pw",
    hash(password))` + version tag → `AddRequestLobbyListStringFilter` → `RequestLobbyList` →
    `JoinLobby` → read host SteamID, on the host (appid 480/Spacewar, not the game). Biggest de-risk
    for the least cost; validates the password-keyed scheme entirely off-rig.
  - **DLL hand-bind (writable solo, fires on the rig).** Bind the `RegisterCallResult` C++-ABI in
    `coop/steam.rs` (the `CCallbackBase*` vtable + three `extern "C"` thunks) and seed the rung-2
    side-channel (`[coop] peer_steam_id` + `is_host`) from the resolved host SteamID. Authorable +
    `cargo check`/`clippy`-able without the game; only firing needs the rig.
  - **Cheap rig probe (rig, but *single-player*).** The one hard unknown: does ER pump Steam via legacy
    `RunCallbacks` (our registered call-results fire) or `ManualDispatch` (path blocked)? A one-machine
    experiment — register one harmless `CreateLobby` call-result and watch it fire under ER's pump. Gates
    the hand-bind, so run it early; fits "no 2-player" since it's solo.
- **Rung-3 RE prep (diagnostic DLL).** *Scaffold shipped* (`coop/session_probe`, gated by
  `[debug.probes] session_probe`): the FSM rising-edge logger works solo; the create/join entry hooks
  are in place but **inert until the initiation-function AOBs are charted on the rig** (a precise TODO).
  Accelerates the co-op core below. See [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md),
  [COOP-CONNECTION.md](COOP-CONNECTION.md), the [`/reverse-engineer`] skill.
- **Overhead nameplates** — *projection rig-confirmed (2026-06-26)* + base styling (alpha, shadow,
  per-peer palette tint) shipped; the color palette + off-screen edge-clamp math are host-tested
  utilities ready to wire. The rest (stable per-peer colors, distance LOD text→dot, off-screen edge
  indicator, real name/ping/SL/death content) rides on the co-op core's peer feed and needs 2-player to
  verify. Full design in [NAMEPLATES.md](NAMEPLATES.md).

### 2-player-gated (the co-op core + everything riding on it)

- **Rung 2 verification** — confirm the private Steam P2P side-channel links across two machines
  (NAT/auth; whether peers must be Steam friends). Implementation is done + harness-proven.
- **Rung 3 — drive the session FSM** to put a peer in your world (the hard RE: the create/join
  functions, the password-derived AES key). This is what unblocks in-world presence.
- **Riding on the session layer:** session-management actions (open/join/lock/unlock/leave, password,
  evil session), PvP/friendly-fire/team toggles, rune-arc sharing, overhead player display
  (ping/SL/death-count), enemy/boss-rush modes, inbound-action host authorization. See [FEATURES.md](FEATURES.md).
- **2-player live verifications:** scaling's in-combat HP/posture effect + the off-by-one player count,
  the `>4`-player limit, session persistence across area boundaries, death-debuffs `dont_sync`
  (per-player stacks), client→host log forwarding. See [RIG-RUNBOOK.md](RIG-RUNBOOK.md).
- **Event toasts** — player join/leave and similar (the notifications surface Michael wants expanded);
  the side-channel already toasts connect/version/liveness, so this slots in with the session layer.

## Won't-do

- **Offline title-screen popup suppression + FMG watermark** — Arxan-walled / superseded by the
  overlay watermark. RE record kept in [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md); do not bump
  the SDK pin for FMG access.
- **Native `display_status_message` banner fallback** — a degraded notification path via the charted
  `CSMenuManImp::display_status_message` RVA, for when the overlay fails to init. Dropped: not worth
  the added surface for a path the overlay already covers. The RE record (the call is charted/callable)
  stays in [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md) for reference; we just won't ship it as a
  fallback.

[`/reverse-engineer`]: ../.claude/skills/reverse-engineer
