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
- **Explicit, on-demand connection.** Co-op is no longer auto-started at launch; the overlay Actions
  menu drives it: **Open World** (host) / **Join world** (joiner) / **Leave world**. The role is the
  user's **choice** (`steam::LobbyIntent`), not derived, so only the host creates a lobby (no
  both-create race, no owner-id tiebreak). A **Steam-readiness gate** (`crate::steam_ready`:
  Connecting/Ready/Failed) holds Open/Join disabled (behind a "Connecting to Steam..." banner)
  until the SteamID + networking + lobby interfaces resolve and the player is in-game. **Leave** tears
  the session down via a generation counter; the lobby is left on every driver-thread exit (RAII).
  See [COOP-CONNECTION.md](COOP-CONNECTION.md).
- **Peer authentication on the side-channel.** The rung-2 handshake now authenticates the peer with a
  password-keyed proof before linking: `Hello` carries a per-session 16-byte nonce, a new `Auth`
  message carries a domain-separated SHA-256 proof, and a peer is **not linked** (no `ConfigSync` /
  session action / forwarded log honored) until its proof verifies; a wrong password raises a
  plain-voice auth banner and never links. Wire format `VERSION` 5→6; `MIN_PASSWORD_LEN` 5→8 (the
  proof is a fast hash, so a short password is offline-brute-forceable). The two password-keyed hashes
  (auth proof + lobby discovery token, distinct domain tags) live together in
  `unseamless-core/crypto.rs`.
- **Actions-menu redesign.** Paired verbs collapse into one stateful row (Lock⇄Unlock; PvP / PvP teams
  / Friendly fire show on/off and emit a single `Toggle*`), and inapplicable rows are **hidden**, not
  greyed (solo → Open/Join; in-session host → Leave + the four toggles; joiner → Leave). The model is
  `unseamless_core::menu::action_rows`. See [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md).
- **Overlay/debug polish.** The debug report is cached per publish-version (no per-frame deep clone);
  the Debug tab's detail panes render independently of the summary panel (`report_wanted`); the ailment
  display is fixed and **rig-confirmed** (gauges are resistance *remaining*, so buildup = `gauge_max -
  gauge`); and rendered banner/toast strings are **ASCII-only** (the imgui overlay font has no glyph for
  the em dash or ellipsis, so they render as `?`). Banners are now capped (`MAX_BANNERS`) like toasts.

## Wave 2 — in progress

The out-of-band connection stack (rungs 1, 2, 4) is shipped and **now CONFIRMED live across two real
machines** in the first two-player friend test (2026-06-27): the joiner-finds-host leg of rung 4 and the
rung-2 side-channel link both work (`coop: linked … versions match`; the `coop_connect` report showed
`lobby_created`, `host_id resolved`, `handshake reached`, `version match`, and sent 2674 / received 2011
messages bidirectionally). So rungs 1, 2, 4 are done *and verified peer-to-peer*. **Rung 3, driving the
game's own session so players see each other in-world, is the headline-next** (see below). Two findings
from that session: the in-game multiplayer items are **greyed out offline** (outside EAC), so the rung-3
FSM can't be triggered the normal way. We first hunted the item-grey gate to re-enable them, but **three
static candidate families were rig-eliminated** (`is_offline()`, `IsEnableOnlineMode()`, the cached
online-available chain — see [OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)), so that hunt is
**parked**. The approach pivoted (2026-06-28) to **driving `CSSessionManager` directly** — chart and call
the create/join initiation function, no item needed (the multiplayer items become removable harness). The
**overlay crashes on native Windows** (hudhook DX12), a pre-release blocker. **Narrowed to
NVIDIA-driver-specific (2026-06-29):** a local Windows-VM harness (`crates/dx12-harness` + the
[`/windows-test`] skill) ran the same hook + imgui font-bake clean on a real native-Windows D3D12
loader (WARP), ruling out the hardware-independent MinHook mechanism and the font upload; the crash
needs real NVIDIA. A `crashdump` handler (`coop/crashdump.rs`, in every build, verified) now logs the
**faulting module** on the crash, and a crash-trace friend bundle is one command
(`rig.sh package --trace && rig.sh share`). What's left is one run on any real NVIDIA box (a *light*
solo friend ask, or VFIO passthrough); mitigation meanwhile is `[debug] overlay = false`. See
[OVERLAY-RENDERING.md](OVERLAY-RENDERING.md) > "Native-Windows Crash" and
[FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md) > Part C.

### Solo / host-doable (no 2nd player needed)

- **Rung 4 — Steam lobby discovery (password-keyed).** *Shipped — the live connection path.* It
  **replaces** the manual SteamID copy-paste: co-op is triggered on demand from the overlay menu (Open
  World hosts, Join world joins), both players share the same password, and the resolved peer + chosen
  role seed the rung-2 side-channel. The role is the user's choice, not derived — only the host creates a
  lobby (no both-create race). *Independent of rung 3* — it links the side-channels, it doesn't put
  players in one another's world (that's still rung 3). Status against the build order (full spec in
  [COOP-CONNECTION.md](COOP-CONNECTION.md) > rung 4):
  - ✅ **Rig probe (done 2026-06-26).** The one hard unknown is answered: ELDEN RING pumps Steam via
    legacy `RunCallbacks` (its imports carry `RunCallbacks` + `RegisterCallResult`, no `ManualDispatch`),
    and `CreateLobby` **succeeds in-process** (EResult OK, real lobby id). Key lesson: do **not** register
    a call-result *and* poll the same handle — ER's pump consumes it first and the poll sees
    `InvalidHandle`. The path is **poll-based** (`ISteamUtils` `IsAPICallCompleted` + `GetAPICallResult`,
    accessor `SteamAPI_SteamUtils_v010`), matching rung 1/2's poll-not-pump model.
  - ✅ **Harness prototype (done).** The [`harness`](../crates/harness) crate (a normal native exe that
    *can* take `steamworks-rs`) proved `CreateLobby` → `SetLobbyData("usc_pw", hash(password))` + version
    tag → `AddRequestLobbyListStringFilter` → `RequestLobbyList` → `JoinLobby` → read host SteamID, on
    Spacewar (appid 480), validating the password-keyed scheme off-rig.
  - ✅ **DLL hand-bind (shipped).** The **poll-based** `ISteamUtils`/`ISteamMatchmaking` path is bound in
    `coop/steam.rs` (the register-based `CCallbackBase` machinery is gone), driven on demand by the Open
    World / Join world actions and seeding the rung-2 side-channel from the resolved host SteamID + chosen
    role. Solo `CreateLobby` is rig-proven, and the **joiner-finds-host leg is now CONFIRMED** in the
    2026-06-27 friend test (the host resolved the joiner's lobby and linked) — rung 4 is fully verified
    end-to-end (see [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md)).
- **Rung-3 RE prep (diagnostic DLL).** *Scaffold shipped* (`coop/session_probe`, gated by
  `[debug.probes] session_probe`): the FSM rising-edge logger works solo; the create/join entry hooks
  are in place but **inert until the initiation-function AOBs are charted on the rig** (a precise TODO).
  Accelerates the co-op core below. See [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md),
  [COOP-CONNECTION.md](COOP-CONNECTION.md), the [`/reverse-engineer`] skill.
- **Overhead nameplates** — **shipped: a native per-player colored dot, on by default**
  (`[nameplates] enabled`), drawn by the game's own `CSEzDraw` renderer (world-space, depth-tested, no
  present-hook) over each player and your own head — so it's verifiable solo. The earlier imgui
  projected-label nameplates (and their projection/text-content core modules) were removed; the dot is
  the one nameplate surface. The **one** remaining follow-up is **color-by-SteamID** (the dot color is
  keyed off the phantom pointer today) — rung-3-gated, since it needs the session core to map a
  phantom→identity. Full design in [NAMEPLATES.md](NAMEPLATES.md).

### 2-player-gated (the co-op core + everything riding on it)

- **Rung 2 verification — DONE (2026-06-27 friend test).** The private Steam P2P side-channel links
  across two real machines: the NAT/auth/handshake completed and versions matched (`coop: linked`),
  with substantial bidirectional traffic (sent 2674 / received 2011). The peers were Steam friends in
  this run; whether non-friends can link is still untested but didn't block here. No manual peer
  pairing — the side-channel was seeded by rung-4 lobby discovery (one opened a world, the other
  joined), exactly as designed. See [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md).
- **Rung 3: drive the session FSM (the headline-next).** RE the create/join functions that move
  `CSSessionManager` to `Host`/`Client` for a given peer (the password derives the session AES key),
  so players see each other in-world. This is the apply layer the rest of the UI is already waiting on.

  > **State (2026-06-29) — PICK UP HERE in a new session; full detail in
  > [SESSION-DRIVE.md](SESSION-DRIVE.md) + [SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md):** the
  > create/join initiation is **charted** and direct-drive is **rig-PROVEN** — calling the create
  > wrapper `0x140cad4c0` on `[G]` (no item, no peer) moves `lobby_state` off `None`. **Corrected blocker
  > (write-watch run):** the Arxan gate `0x140cb4b50` is **NOT** the wall — with
  > `bypass_session_create_gate` (flips its `jne→jmp`) + `enable_offline_multiplayer` applied, a hardware
  > write-watch on `[G]+0x24` **HIT at `RIP=0x140cb2086`** (the leg-B store `mov [this+0x24],eax` @
  > `0x140cb2083`), so control **reaches the network-create vmethod (leg B)** — which returns `eax=0`
  > offline → `FailedToCreateSession`. (The earlier "gate still rejects, `[G]+0x24=0`" note was a
  > *peek* artifact: peek can't tell never-written from leg-B-wrote-`0`; the write-watch can, and shows
  > leg B ran.) **Leg B now captured + charted:** the create vmethod is **`0x1423f5c00`** (resolved live
  > via `this→*(this+0x60)→+0x710→VT=*()→VT[1]`; CLEAN, not Arxan), and it returns 0 on any of **three
  > early rejects** — `*(NetworkSession+0x10)==0` (@`0x1423f5c4f`), `vtable[0xe8](…)==false` (@`0x1423f5c69`),
  > or `vtable[0x108](…)==null` (@`0x1423f5c87`); see [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Leg B charted".
  > **Reject #1 forced but insufficient → root cause CONFIRMED (2026-06-29 in-world rig):** forcing
  > `NetworkSession+0x10` nonzero did **not** unblock; create passes every static gate (rejects #1/#2/#3 +
  > the 4th gate) then dies in **leg B's tail capacity check** — the session-slot array is **capacity 0**
  > offline (`cmp count,[rbx+0x20]; jae fail`, `rbx=[[NetworkSession+8]+0x48]`, `[rbx+0x20]==0`), because no
  > real match/lobby allocated it, so the finished session has nowhere to be stored. It is **not** OOM, the
  > gate, the 4th gate, or the finalize registry (superseded hypotheses, tombstoned in SESSION-DRIVE.md — incl.
  > the earlier wrong note that `0x1423fa1b0`'s `0x144842d28` read was the item-grey service; it's a hash
  > modulus, no proven link). **NEXT (the real fix): drive create with a live rung-4 lobby + a real peer**
  > (2-player — a real peer is what sizes the slot array) — set `drive_create` + `bypass_session_create_gate`
  > + `force_netsession_ready` on both machines. Keep `bypass_session_create_gate` ON (confirmed prerequisite).
  > Tooling ready:
  > the cdylib drive-probe (`[debug.probes] drive_create`), `scripts/re/watch-write.py` (sudo-free
  > peek + HW write/rw-watch), and `rig.sh cycle` reaches in-game autonomously. The **create** success path,
  > the **join** leg, and the real two-player in-world test all need a friend now (a peer sizes the slot
  > array); solo, only the capacity-0 **failure** mode is confirmable.

  It unblocks:
  - **The in-world session itself.** Open/Join/Leave already drive the connection layer (lobby + the
    rung-2 side-channel), but they don't yet put players in one another's *world*; rung 3 is what makes
    them place a peer in your session.
  - **Wiring the inert toggle actions.** Lock/Unlock/PvP/PvP teams/Friendly fire are surfaced by the
    overlay menu but still inert ("not wired up yet"); rung 3 connects them to real game calls.
  - **Sourcing the menu's state bits.** `SessionContext.{world_locked, pvp_on, pvp_teams_on,
    friendly_fire_on}` are always-`false` placeholders today; rung 3 must source them from the session
    FSM so the collapsed toggle rows show real state.
  - **In-world presence:** the game's own net sync takes over once `Ingame`.
  - **Peer-map pruning on session-leave:** drop a departed peer from the side-channel's linked set when
    the session roster shrinks.
- **Riding on the session layer:** session-management actions (open/join/lock/unlock/leave, password),
  PvP/friendly-fire/team toggles, rune-arc sharing, overhead player display (ping/SL/death-count),
  inbound-action host authorization. See [FEATURES.md](FEATURES.md). (Evil sessions and enemy/boss-rush
  modes are **dropped** — see Won't-do.)
- **2-player live verifications:** the off-by-one player count, the `>4`-player limit, session
  persistence across area boundaries, death-debuffs `dont_sync` (per-player stacks), client→host log
  forwarding. See [RIG-RUNBOOK.md](RIG-RUNBOOK.md). (Scaling's in-combat HP/posture *effect* itself is
  no longer treated as a blocker — see "Pending validations".)
- **Event toasts** — the player join/leave presence feature landing on the session layer (the
  notifications surface Michael wants expanded); the side-channel already toasts
  connect/version/liveness, so this slots in with the session layer. (Distinct from the *read-correctness*
  of effect toasts in normal play — see "Pending validations".)

## Pending validations (not blockers)

Low-risk things we **expect to work** and will confirm by *noticing them in normal play*, not by
gating progress on a dedicated rig run. These are explicitly **not blockers** — if Michael spots one
behaving wrong, it's a quick fix, not a reason to hold the line. (We were blocking on too much that can
just be corrected when noticed.)

- **Crit co-op** — a partner can damage an enemy during the riposte/backstab/guard-counter crit window.
- **Boot master volume** — the configured boot volume is *audibly* applied (the write lifecycle is
  already rig-confirmed; this is the human-ear check).
- **Death debuffs** — a debuff lands on death and then clears at a Site of Grace.
- **Scaling** — the in-combat enemy/boss HP/posture effect (the rate-row writes are already rig-verified).
- **Gameplay toasts** — ER-voiced effect toasts (death debuffs, presence, etc.) read correctly in play.

## Won't-do

- **Original-MP modes & evil sessions** — enemy rush, boss rush, arena waves, custom mod goods
  (`MODGOODS_*`), and "evil" / invasion-style sessions. unseamless-coop is a **co-op-only**
  reimplementation targeting core co-op gameplay; original PvP/invasion modes and bolted-on game
  modes are out of scope. See [FEATURES.md](FEATURES.md) > "Custom content, modes & original-MP
  sessions — WON'T DO". (Rune-arc sharing is *not* dropped.)
- **Offline title-screen popup suppression + FMG watermark** — Arxan-walled / superseded by the
  overlay watermark. RE record kept in [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md); do not bump
  the SDK pin for FMG access.
- **Native `display_status_message` banner fallback** — a degraded notification path via the charted
  `CSMenuManImp::display_status_message` RVA, for when the overlay fails to init. Dropped: not worth
  the added surface for a path the overlay already covers. The RE record (the call is charted/callable)
  stays in [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md) for reference; we just won't ship it as a
  fallback.

[`/reverse-engineer`]: ../.claude/skills/reverse-engineer
[`/windows-test`]: ../.claude/skills/windows-test/SKILL.md
