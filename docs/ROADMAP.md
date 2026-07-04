# Roadmap

What's built vs. what's next, grouped by what **gates** each item. Detail lives in the linked design
docs; this is the map. Work proceeds in **waves** (one fleet batch each — see
[ORCHESTRATION.md](ORCHESTRATION.md)).

> **Scope & legitimacy.** Everything here is interop work on a game we own, on the developer's own
> machine, to reimplement a co-op mod — co-op-only, *outside* anti-cheat by construction, no
> DRM-cracking or reaching other players' systems. Where an item mentions "bypassing a gate," it
> means flipping a check in our own in-memory copy so our own private co-op path can proceed offline,
> never defeating anti-cheat to touch the official servers. See CLAUDE.md > Safety / legitimacy +
> Clean-room hygiene.

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

The out-of-band connection stack (rungs 1, 2, 4) is shipped and **CONFIRMED live across two real
machines** (2026-06-27 friend test: `coop: linked … versions match`, bidirectional traffic). **Rung 3,
driving the game's own session so players see each other in-world, is the headline-next** — and as of
**2026-07-04 its hardest unknown, the transport, is SOLVED**: we stand up the game's own DLNW3D transport
ourselves offline and have rig-proven its legacy P2P works two-machine (rig + Deck). Only the "seam" to
the session FSM remains. Full state in the rung-3 callout below. *(The earlier item-grey-gate hunt —
three static candidate families rig-eliminated, [OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md) —
is now **moot for the connection**: the transport-leg path sidesteps the multiplayer items entirely.)* The
**overlay crashes on native Windows**, a pre-release blocker — **ROOT-CAUSED (mechanism) 2026-07-01:
not DX12/NVIDIA at all.** The friend trace run showed the present hook, imgui init, and rendering
all healthy on native NVIDIA; his WER record then pinned the death at **`XINPUT1_4.dll+0x9a65` =
`XInputGetState+5`**: an **inline-hook collision** between our ilhook patch on `XInputGetState`
(the overlay's controller capture) and a second 5-byte hooker (likely Steam's gameoverlayrenderer)
whose trampoline jumps back to `entry+5`, mid-our-patch. **FIX APPLIED + FULLY VALIDATED (install
rig-verified, collision solo-validated on real Windows):** the XInput capture is now an **IAT hook** on
`eldenring.exe`'s import (no function-body patching, collision-immune by construction), and
`crashdump.rs` re-asserts its exception filter every 3s + names any displacer (our filter had been
silently bypassed). Both are on `main`. The rig re-verify (2026-07-03) caught that the first IAT version
never installed — the game's XINPUT `FirstThunk` is 4-byte-aligned and pelite's `iat()` rejected it —
now fixed (match off the INT, index the IAT manually; commit `0fde906`) and confirmed installing on the
rig. The vkd3d rig can't reproduce the *collision*, so it was validated **solo on a real Windows loader**
instead (2026-07-03): the `dx12-harness` XInput phases in the quickemu Win11 VM show the inline-hook
collision AV at `XInputGetState+5` (the exact WER address) and the IAT hook surviving the same setup
(`scripts/win.sh xinput repro|iat`). A friend run is now optional confirmation on his exact hooker stack,
not a blocker; the pre-release overlay blocker is effectively retired (`[debug] overlay = false` no
longer needed as a mitigation). See [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md) > "WER Verdict" and
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

  > **State (2026-07-04) — the transport is SOLVED; only the "seam" to the session FSM remains. PICK UP
  > at thread 2 below. Full detail in [COOP-CONNECTION.md](COOP-CONNECTION.md) > rung 3 "THE PLAN" +
  > [SESSION-DRIVE.md](SESSION-DRIVE.md) + [FROMNET-LINK-FINDINGS.md](FROMNET-LINK-FINDINGS.md).**
  >
  > **SOLVED — create initiation + real session state.** Driving create (`0x140cad4c0`) moves
  > `lobby_state None→TryToCreateSession`; driving the container's real session-established handler
  > (`ManagerImplSteam@DLNR3D::0x1423f4870`) populates genuine session state (veto bit + real SteamID).
  > Every create gate is charted + satisfiable. (The old "leg-B slot-array capacity-0" blocker is
  > **superseded** — it was downstream of the real wall below.)
  >
  > **THE REAL WALL WAS THE TRANSPORT — now cracked.** After `TryToCreateSession` the session dispatches
  > vmethods on the connection at `[container+0x708]`, null offline; fabrication is a proven dead end.
  > That connection is a **`SteamConnection@DLNW3D`** on a lower transport namespace (**DLNW3D**) riding
  > **legacy `ISteamNetworking006`** P2P. A live scan proved the whole transport is **dormant offline**
  > (0 objects). Three workers charted it end-to-end; the key fact: legacy P2P is **addressed by CSteamID
  > alone**, so the un-forgeable server broker-blob (`join_data`) is **unneeded** — we only need the
  > rung-4 peer SteamID64. **Decision: inject at the transport leg (path C) — stand the transport up
  > ourselves and feed it the peer; don't intercept FromNet, don't crack the item-grey signal (moot).**
  >
  > **RIG-PROVEN (2026-07-03/04):**
  > - We **build the entire DLNW3D transport ourselves offline** — `SteamServiceImpl` +
  >   `SteamConnectionManager` + `SteamConnection`, all constructed off the real game heap
  >   (`0x144842d38`), game alive, `scan-vtable.py` confirms each (`[debug.probes] stand_up_transport`).
  > - **✅✅ TWO-MACHINE (rig + Steam Deck): the game's legacy P2P works offline.** Both machines drove the
  >   game's own `ISteamNetworking006` (`Accept` + `Send`/`Read`) at each other's SteamID64 and exchanged
  >   packets **bidirectionally + sustained, no matchmaking** (`game-p2p — RECV "USC-GAMEP2P#N"` on both).
  >   ERSC's premise — skip the matchmaker, feed the peer SteamID64 — is validated end-to-end.
  >   (`[debug.probes] p2p_test_peer_a/_b` feed both SteamIDs so no manual Open/Join link is needed.)
  >
  > **REMAINING — thread 2, the "seam" (the last piece to `Host`):** wire the proven transport into the
  > game's session. `[container+0x708]` wants a **refcounted DLNR3D-level connection** (create refcounts
  > `[+0x708+8]`, so it's *not* a raw `SteamConnection` — it's a wrapper on the DLNW3D transport). Chart
  > that object's type + how it's populated (the container's connection-event callbacks
  > `0x1423f44d0`/`0x1423f4560` alloc per-connection objects on events), build/wire it like the DLNW3D
  > objects with `+0x8`=iface / `+0x128`=peer, land it at `+0x708`, then drive create → `Host`.

  **Seamlessness (independent, additive) — one armed gate, charted.** All game-driven co-op disconnects
  (boss defeat, area transition, death, host migration, remote-leave) funnel through **one primitive:
  `leave_session 0x140cae730`** (sole out-of-line writer of `lobby_state=OnLeaveSession`, 24 callers) +
  one inlined twin (`0x140cb08bc`). An armed flag there suppresses every game-driven disconnect (symmetric
  — both peers run our mod); do **not** gate the low-level teardown handler `0x1423f46d0`. Then an
  *additive* re-sync layer across area transitions. See [SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md).

  **RE reference docs (this wave):** [COOP-FLOW-FINDINGS.md](COOP-FLOW-FINDINGS.md) (item-use → FromNet
  spine), [FROMNET-LINK-FINDINGS.md](FROMNET-LINK-FINDINGS.md) (the transport injection point),
  [SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md) (the disconnect chokepoint). Tooling:
  `scripts/re/scan-vtable.py` (is-class-X-live), `scripts/re/watch-bt.py` (write-watch + backtrace),
  `scripts/deck.sh` (Deck as player 2).

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
