# Roadmap

What's built vs. what's next, grouped by what **gates** each item. Detail lives in the linked design
docs; this is the map. Work proceeds in **waves** (one fleet batch each — see
[ORCHESTRATION.md](ORCHESTRATION.md)).

> **The fast-moving current state + the chosen next step live in [STATE.md](STATE.md)** — this file
> is the slower gating map, and its inline status callouts are history, not the plan. When the two
> disagree, STATE.md wins.

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
driving the game's own session so players see each other in-world, is the headline-next.** The
**transport is built** — we stand up the game's own DLNW3D transport ourselves offline and have rig-proven
its legacy P2P works two-machine (rig + Deck). **What remains is the SEAM: the game's session-*establishment*
object graph.**
>
> **★ DECISION (2026-07-04) — pivot to the "let the game establish it" (true ERSC) model.** A 3-lane RE pass
> this session proved the offline hand-synthesis avenue is a principled dead end: every piece missing offline is
> a *runtime object the establishment flow builds* — the live-session array capacity is 0 offline so a created
> `SessionSteam` is destroyed instantly (Lane C); add-member wants two ref-counted *game handle objects*, not a
> scalar SteamID (Lane A); and the transport context's accept-callback at `+0x168` has **no static installer
> anywhere in the image** — it only arrives via a runtime Steam callback (Lane B). So instead of forging that
> graph field-by-field (whack-a-mole, many sessions), we **drive the game's own establishment entry points, fed
> our rung-4-discovered peer, and let the game wire its own sub-objects** — reproduced from a **live capture of a
> real working ERSC session**. ERSC proves the native session establishes outside EAC over Steam P2P, and our
> Steam P2P transport is already rig-proven two-machine, so this is reproducing a thing that demonstrably works.
> **This does NOT mean reaching FromSoft's matchmaking servers** (off-limits, EAC): the peer is still found via
> our own password-keyed lobby side-channel and the session still rides Steam P2P. **\* Offline synthesis is
> paused, not killed** — if the capture shows establishment reduces to a small reproducible field/call set, we
> re-open it; the capture arbitrates. **► Next: a live `watch-write.py` read of a real ERSC establishment at the
> charted offsets, then reproduce the sequence. Full plan + offsets: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★
> DECISION (2026-07-04)".** *(Superseded background — the older "seam = wire the connection sub-objects" and both
> hand-build/native-build avenues — is preserved in the rung-3 callout below and in SESSION-DRIVE.md.)*
*(The earlier item-grey-gate hunt —
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
- **Rung-3 RE/drive surface (diagnostic DLL).** `coop/session_probe` now carries the charted create/join
  drivers, transport standup, endpoint/type-5 instrumentation, and the worker-thread peer registrar,
  each independently gated under `[debug.probes]`. See [SESSION-DRIVE.md](SESSION-DRIVE.md),
  [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md), and the [`/reverse-engineer`] skill.
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

  > **★ CURRENT DIRECTION (2026-07-13): two-machine roster is proven; verify presence, then productize.**
  > Both rig and Deck now reach stable `Host`/`Ingame` with the remote peer in slot 2. The native
  > `FsdpConnection` path carries a real Steam auth ticket, `BeginAuthSession` returns `OK`, and clearing the
  > add-peer suppress flag lets the member completion phase post the type-1 roster event. Both observers changed
  > to `players=2` and held there without a crash. Next is a visual/control check that the remote character is
  > actually present, followed by replacing debug peer ids and probe roles with the rung-4 peer and real Open/Join
  > lifecycle. See [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Native Transport and Two-Player Roster Result
  > (2026-07-13)".
  > **The blocks below are the historical hand-synthesis trail (2026-07-02..04). Read them as ground already
  > explored, not as the current plan.** Reference docs, so we don't re-tread:
  > - **[ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) — READ FIRST.** A real 2-player ERSC
  >   session captured in memory: the full live object graph + offsets, and the two corrections (`+0x168` is
  >   the reject-stub even when co-op works; the DLNR3D/DLNW3D graph is the real mechanism).
  > - [SESSION-DRIVE.md](SESSION-DRIVE.md) — the rung-3 call spec + all RE (DLNR3D reframe, Lanes A/B/C, the
  >   `CSSessionManager → container → SessionManagerSteam → session-array` reachability chain + every offset).
  > - [PATH2-TRANSPORT-STANDUP.md](PATH2-TRANSPORT-STANDUP.md) — how we stand up the DLNW3D transport ourselves
  >   (proven two-machine); the transport is done, don't rebuild it.
  > - [FROMNET-LINK-FINDINGS.md](FROMNET-LINK-FINDINGS.md) — the FromNet/session-establish link RE.
  > - [SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md) — FSM states, leave/teardown, the disconnect
  >   chokepoints (also the disconnect-suppression feature's basis).
  > - [COOP-FLOW-FINDINGS.md](COOP-FLOW-FINDINGS.md) — the create-flow gates (leg B, capacity-0 destroy).
  > - [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) / [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) — the rig
  >   RE + drive procedures (watch-write, the probe flags); [OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)
  >   — the item-grey-gate hunt, rig-eliminated + now moot.

  > **Historical state (2026-07-04 night, superseded by the current direction above): ★ SOLO HOST STICKS +
  > JOINER SYN REACHES HOST ADMIT two-machine; gap = the host context member-lookup stub.** Two-machine
  > (rig host + Deck joiner): rig = stable `Host`/`Ingame` (in the
  > co-op world), Deck reaches `Client`; the joiner's synthetic 14-byte DLNW3D SYN on channel 30 reaches the
  > host's admit path `0x142640e30` and is rejected only at gate c (the context member-lookup `[context+0x168]`
  > is a stub `0x1423fdf00`), so no host-side connection is created and roster stays 1. NEXT: make the host
  > context recognize the joiner as a member so admit succeeds → roster grows to 2 (see the ► callout below).
  > Details: SESSION-DRIVE.md > "STATUS (2026-07-04 night)". (Host path below:)
  >
  > **★ SOLO HOST REACHED AND STICKS.** The full offline host path works:
  > the `SteamServiceImpl` standup works offline (the "native-builder dead end" was a misdiagnosis); we land the
  > correct object at `[container+0x708]` (a socket-manager wrapper, not a raw connection) + drive its own init;
  > drive create → `TryToCreateSession`; and bypass host-setup's final online-availability gate `0x140de2620`
  > (patch to `ret true`). Rig-confirmed: `None → TryToCreateSession → Host`, `protocol=Ingame`, `players=1`, the
  > warp into the co-op map (`1800001`) COMPLETES, and the session HOLDS (no teardown). **The remaining goal —
  > TWO sessions in each other's worlds — needs a JOINER driver** (the join wrapper `0x140cae640` is peer-directed;
  > its payload is uncharted — build the driver + chart the payload with the Deck as a real joiner). PICK UP at
  > SESSION-DRIVE.md > "HOST-SETUP DRIVE (2026-07-04 pm)"; [COOP-CONNECTION.md](COOP-CONNECTION.md) > rung 3 for
  > background.**
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
  > **SEAM CHARTED + reached `TryToCreateSession` (2026-07-04).** `[container+0x708]` = a
  > **`SocketManagerHolder@DLNR3D`** (0x18-byte refcounted wrapper `{vtable 0x1431f9280, refcount@+8,
  > SteamConnection*@+0x10}`, ctor `0x1423f7180`), *not* a raw connection. Landing a real holder cleared the
  > original create crash and drove create to `TryToCreateSession`. **But** driving the FSM the rest of the way
  > faults: forcing `Host` (`0x140cb2ae0`) doesn't stick (host-setup faults on the connection), and
  > hand-building the connection field-by-field is whack-a-mole (sub-objects are construction-time-wired).
  >
  > **NATIVE-BUILDER PATH — RULED OUT (2026-07-04, offline AND two-machine).** The idea was to make the *game*
  > build a fully-wired connection by driving the establish handler `0x1423f2820`. This session got it to
  > **reach the game's own builder** (two fixes: the live derived vtable is `0x1431f8780` so the real builder
  > is the plain fn `0x1423f46b0` — the earlier "Arxan builder `0x14251c480`" was a wrong-vtable artifact; and
  > `drive_session_established` must be OFF, else it double-drives `0x1423f4870` and the handler's own gate2
  > call bails "already established"). But the builder's `SteamServiceImpl` standup **`0x142638b40` returns
  > null** — and this is now pinned to the standup's `owner`/config (`[container+0x48]`), NOT to a missing
  > iface or peer: it still fails **with a real linked peer** (rig+Deck) AND **with `ISteamNetworking006`
  > resolved** (`stand_up_transport` on). The game's standup only works inside its own online-session flow,
  > which we bypass by construction. Dead end.
  >
  > **► HOST-SIDE ADMIT REACHED (2026-07-04 night); NEXT = the context member-lookup STUB.** The solo host
  > now reaches + sticks at `Host`/`Ingame` (warped into the co-op world). The joiner→host transport connect
  > is charted end-to-end two-machine: the host's socket-manager **worker thread runs offline** (`0x142640bc0`,
  > reads P2P **channel 30**), and a joiner **14-byte DLNW3D SYN** on channel 30 **reaches the host admit
  > helper `0x142640e30`** (passes size + SYN-shape gates). It's rejected at **gate c** (`0x142640ecd`): the
  > context member-lookup `[socketmgr+0x40]`→`0x142639d00`→`[context+0x168]` is a **stub `0x1423fdf00` (`mov
  > eax,1; ret`)** — our synthesized context has no member registry, so no host-side connection is created and
  > **roster stays 1**. Finish = **make the host context recognize the joiner as a member** (install a real
  > `[context+0x168]` lookup via a fuller service/context init, or register the joiner's SteamID64 in the
  > member collection `[context+0x170]` via register thunk `0x14263b7c0`) so admit succeeds (`0x142640ee4`) →
  > roster-add `0x140cb31b0` (charted: **no offline gate**) grows `players` to 2. Step-by-step: SESSION-DRIVE.md
  > > "STATUS (2026-07-04 night)".

  **Seamlessness (independent, additive) — one armed gate, SHIPPED (2026-07-04) as
  `gameplay.stay_connected` (default off; commit `7ad9000`), live 2-player validation pending.** All
  game-driven co-op disconnects
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
