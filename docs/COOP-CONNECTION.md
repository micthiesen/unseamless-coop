# Co-op Connection Plan

How two unseamless-coop players actually get connected. This is the Layer-2 "getting players into
one another's worlds" problem from [ARCHITECTURE.md](ARCHITECTURE.md), planned out: what's possible
today, what's reverse-engineering-gated, the incremental build order, and the decided approach for
talking to Steam.

> **Scope & legitimacy.** Interop RE on a game we own, on the developer's own machine, to connect
> two co-op players over our own private Steam side-channel — co-op-only, *outside* anti-cheat, no
> DRM-cracking or reaching other players' systems. See CLAUDE.md > Safety / legitimacy +
> Clean-room hygiene.

> **Status: rungs 1, 2, and 4 shipped; rung 3 is the remaining hard RE.** The mod loads, configures,
> observes a session, reads our own SteamID (rung 1, `coop/steam.rs`), and — on demand from the
> in-overlay **Open World / Join world** actions — stands up a private Steam P2P **side-channel**
> running the host-tested `Peer`/`Session` for real (handshake, host config push, liveness, client→host
> log forwarding). The side-channel finds its peer by **password-keyed Steam lobby discovery** (rung 4):
> both players share a password, the one who picks **Open World** creates the lobby and the one who picks
> **Join world** enters it. The role is the user's **choice**, not derived — only the host ever creates a
> lobby. (The joiner-finds-host leg + the rung-2 link across two machines were **CONFIRMED in the
> 2026-06-27 friend test**: `coop: linked … versions match`, sent 2674 / received 2011 messages.) None of
> this yet puts players in one another's *world* — that's the
> game-session RE (rung 3). This doc is the spec for the rest, written for session handoff. Everything
> game-internal is grounded in the pinned `fromsoftware-rs` SDK or flagged as inference to confirm on the
> rig (per [CLAUDE.md](../CLAUDE.md) > Clean-room hygiene).

## The one fact that makes a native path viable: "offline" ≠ no network

When we launch outside EAC, the game can't reach **FromSoft's servers** (login + matchmaking). That
is the *only* thing that fails, and it's what fires the "Starting in offline mode" popup (see
[OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md)). **Steam stays fully connected.**

Crucially, Elden Ring co-op *gameplay* rides **Steam P2P**, not FromSoft servers. FromSoft's servers
are only the matchmaker (they broker *who* connects to whom via summon signs / invasions); once two
players are paired, the session itself is peer-to-peer over Steam. So the whole game is:

> **Skip FromSoft matchmaking, find the peer another way, then run the game's normal session over
> Steam P2P.**

That's what ERSC does, and it's why friends connect fine despite the title saying "offline." It also
means we do **not** need ERSC or vanilla-online as a crutch to build this — we need to build the two
pieces below in the right order.

## "Connecting" decomposes into two independent channels

| Channel | What it does | Needs |
|---|---|---|
| **Game session** (`NetworkSession` / `CSSessionManager`) | Makes you **see each other in the world** (position/HP/state sync, the actual co-op). | The hard RE: driving the session FSM to `Host`/`Client`. Unavoidable for in-world co-op. |
| **Private side-channel** (our `Peer`/`Session` logic) | **Mod coordination**: version handshake, config sync, session actions, log-forward. | A transport. Can be built/tested *independent of the game session*. |

The key realization for incremental progress: **the side-channel does not have to wait for the game
session.** [ARCHITECTURE.md](ARCHITECTURE.md) plans to ride it *in-band* on the game's
`broadcast_packet` (one connection, shared lifecycle). But for bootstrapping we can run it over **our
own Steam P2P channel** (`ISteamNetworkingMessages`), which exists before any game session. That lets
us get two real mods talking — and finally test the whole host-tested side-channel layer-5 for real —
without first cracking the game-session RE.

Both are valid; the private-channel-first route is the build order (below). Long term the side-channel
may migrate in-band to `broadcast_packet`, or stay a separate channel; that's a later call.

## What the SDK gives us vs. the RE gap

Grounded in [SDK-COVERAGE.md](SDK-COVERAGE.md) (pin `fromsoftware-rs` rev `8c67a84`):

- **Charted (usable now):** the in-session transport `NetworkSessionVmt.{broadcast_packet,
  receive_packet, kick, remote_identity}`; the FSM `CSSessionManager.{lobby_state, protocol_state}`;
  the roster (`players: DLVector`, `host_player`, `steam_id`s); `session_player_limit`; the session
  **AES cipher**. The [observer](../crates/unseamless-coop/src/features/observer.rs) already reads the
  FSM (solo confirmed: `lobby=None`, `players=0`).
- **The RE gap (not charted):** the **create/join initiation** — the internal functions that drive
  `LobbyState None → TryToCreateSession → Host` (host) and `None → TryToJoinSession → Client`
  (joiner) for a given peer. SDK-COVERAGE flags this as "Needs internal-function RVAs (not just struct
  layout): creating/accepting summon signs …". We have the session object + state + transport, but not
  the call that *starts* a session.
- **Also not in the SDK:** our **own SteamID while solo** (the roster is empty solo). That comes from
  the Steamworks API directly (see Steam integration below).

## Build order (incremental rungs)

Each rung is independently testable and de-risks the next. Rungs 1, 2, and 4 need no game-session RE
and together stand up the **out-of-band connection** (identity → side-channel → discovery); rung 3 is
the one genuinely hard step — driving the game's own session so players see each other in the world.

> **Numbering note.** Rung 4 (discovery) was originally scoped as deferred polish *on top of* a manual
> SteamID exchange in rung 2. That manual exchange has been **removed**: lobby discovery is now the
> only way the side-channel finds its peer, so rung 4 is no longer optional or last — it's the live
> connection path, built alongside the rung-3 RE rather than after it. The rung numbers are kept for
> continuity with the code/commits; read 1 → 2 → 4 as the connection stack and 3 as the in-world step.

### Rung 1 — Identity + copy button (small, safe, solo-testable) — **DONE**
- Bind the Steamworks flat API (below), read our own SteamID.
- Overlay: show the SteamID with a copy-to-clipboard button; also log it + surface it in the diag
  report's `steam` section (live in the debug panel, captured in every log dump).
- Establishes the Steam integration the later rungs need. Lets two players exchange IDs out of band
  (Discord).
- **Shipped** as [`coop/steam.rs`](../crates/unseamless-coop/src/steam.rs). Rig-confirmed: resolves our
  SteamID via the `SteamAPI_SteamUser_v021` accessor on `windows-gnu` (the link/resolve question is
  settled — runtime `GetProcAddress`, nothing new to link), on the second poll (~0.5 s after our early
  `dinput8` load). The Copy button uses imgui's built-in Win32 clipboard. **Still to eyeball on the
  rig:** that the Copy button actually populates the OS clipboard (needs opening the overlay in-game).

### Rung 2 — Private Steam P2P side-channel (the real unblock) — **SHIPPED + CONFIRMED (2026-06-27 friend test)**
- `SteamP2PTransport` ([`coop/coop.rs`](../crates/unseamless-coop/src/coop.rs)) satisfies the existing
  [`Transport`](../crates/unseamless-core/src/transport.rs) trait (`PeerId = u64` is already "a Steam
  ID in production"), over poll-based `ISteamNetworkingMessages`. The peer SteamID and host/client role
  come from **lobby discovery** (rung 4): the user's Open World / Join world choice sets the role, and
  the matching lobby resolves the peer. There is no hand-configured pairing — rung 1's copy button stays
  only for visibility/debugging, not as the pairing mechanism.
- Runs the existing host-tested `Peer`/`Session` over it — the *whole* side-channel (version
  handshake, `ConfigSync`, liveness, log-forward), already proven on `Loopback`/`TcpTransport`/the
  bridge. The driver mirrors received config into the live config and surfaces connect / version-
  mismatch / lost-contact events to the overlay ([`coop/notify`](../crates/unseamless-coop/src/notify.rs)).
- **The handshake now authenticates the peer with a password-keyed proof before linking.** `Hello`
  carries a per-session 16-byte `AuthNonce`; a new `Auth` message carries
  `SHA-256(domain || verifier_id || prover_id || verifier_nonce || prover_nonce || password)`, which the
  recipient recomputes and verifies. A peer is **not linked** (and none of its `ConfigSync` / session
  actions / forwarded logs are honored) until its proof verifies; a wrong password raises a plain-voice
  "Authentication failed with <peer> (wrong co-op password)" banner and never links. The proof is domain-separated
  from the world-readable `lobby_discovery_token` (distinct domain tags), replay-resistant (fresh
  per-session nonces) and reflection-resistant (bound to the directed peer-id pair). Both password-keyed
  hashes live together in [`crypto.rs`](../crates/unseamless-core/src/crypto.rs); the `nonce` + `Auth`
  bumped the wire `VERSION` 5→6 (it has moved on since — `protocol.rs` is canonical), and `MIN_PASSWORD_LEN` is 8 (a short password is
  offline-brute-forceable against this fast hash).
- **Log-forwarding is now wired** ([`coop/forward.rs`](../crates/unseamless-coop/src/forward.rs)): a
  `ForwardLogger` tees records into a bounded queue that the driver drains through `Peer::forward_log`
  onto the wire (a forwarding *client* only; own-module lines are dropped to avoid a feedback loop).
  This is the transport "Log-forwarding status" below was waiting on.
- **The link + config-adoption edges are logged milestones**, not just toasts: `coop::update_link_status`
  emits `coop: linked with partner <tag> (rung 2); versions match/mismatch` on the handshake edge and
  `coop::adopt_host_config` emits `coop: adopted host config (settings synced)` on adoption (both `info`,
  `peer_tag`-scrubbed, one-shot). So each machine's own (locally captured / exportable) log now shows
  *when* the link happened (the on-demand diag dump was previously the only place), and the
  `two-player-join` rig guide auto-finishes its connect steps on those stable substrings instead of a
  manual relay. (These stay in each machine's own log — `forward.rs` drops `unseamless_coop::coop`-target
  records as side-channel noise, so they don't reach the host's forwarded bundle; that's fine, each
  machine's guide reads its own log.)
- **Confirmed in the 2026-06-27 lobby-discovery friend test** (see
  [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md), the run that seeds the peer): both sides established
  without us pumping the `SessionRequest` callback (we proactively `AcceptSessionWithUser`) and the
  `coop_connect` report went `linking → linked`. **Still open:** those peers were Steam friends, so
  whether the NAT/auth path works for *non-friends* is unverified.
- Slots into the same `Transport` seam as `BridgeTransport`; the bridge (loopback) was the host-side
  rehearsal for exactly this.

### Rung 3 — Drive the game's session (the hard RE, on our terms)

> **STATE (2026-07-03) — create initiation + real session state are SOLVED; the wall is the game's
> TRANSPORT layer, which is dormant offline. The finish reduces to a clear architectural choice.** Full
> trace: [SESSION-DRIVE.md](SESSION-DRIVE.md); the load-bearing picture:
>
> - **Solved:** driving create moves `lobby_state None → TryToCreateSession`, and driving the container's
>   real session-established handler (`ManagerImplSteam@DLNR3D::0x1423f4870`) populates genuine session
>   state (the create-veto bit + our real SteamID). Every create gate (leg A, rejects, gate-4, the veto
>   vmethod) is charted and satisfiable.
> - **The wall — the connection/transport layer.** After `TryToCreateSession`, the session machinery
>   dispatches vmethods on the connection object at `[container+0x708]`, which is null offline.
>   Fabrication is a **proven dead end** (hollow objects crash the collection-dispatch). That connection
>   is a **`SteamConnection@DLNW3D`** — part of a lower transport namespace (**DLNW3D**) that rides
>   **`ISteamNetworking006`** (legacy Steam P2P). A live-memory scan in-world offline found **zero**
>   DLNW3D objects (service/manager/connection) vs 3 live session containers: **the whole transport is
>   never stood up offline.** So `+0x708` is null because the layer below it is dormant — the gate is
>   *flow-entry* (whether the game enters its online session flow), above the connection layer.
>
> **The finish — two paths (see SESSION-DRIVE.md > "RIG-PROVEN … DORMANT offline"):**
> 1. **Ride the game's own transport (ERSC-faithful).** Get the game to enter its online flow so it
>    stands up its *own* DLNW3D transport + session graph, then intercept the **matchmaking handoff** to
>    substitute our rung-4 password-peer (ERSC reuses the game's netcode; it replaces the peer-brokering,
>    **not** the transport). Blocked on the flow-entry / online-availability signal — the **same** signal
>    greying the multiplayer items ([OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)), which has
>    beaten three static passes and needs a runtime trace.
> 2. **Stand up the DLNW3D transport ourselves.** Charted entry points (factory `0x142638b40`,
>    connection-creator `0x142640560`, connect/register `0x14263b720`/`0x14263b7c0`). Bounded, bypasses
>    the elusive signal, but out-of-flow (needs the live `owner`/config captured at runtime). More than
>    ERSC does.
>
> Both terminate at a **two-machine** validation (rig + Steam Deck). **Correction to the old framing
> below:** the prior "leg B tail slot-array capacity-0 → a real peer sizes it" model is *superseded* —
> the slot array being empty is downstream of the same dormant transport; sizing it (fabrication or a
> peer) never reached `Host` because the connection object it needs doesn't exist offline.
>
> **Seamlessness is a separate, additive layer (matters regardless of path 1/2).** ERSC's bulk is
> *suppressing the game's own disconnect events* (send-phantom-home after a boss, area-scoping, return-on-
> death). Reasoned architecture: gate the **teardown chokepoint** (the orderly "begin leaving the session"
> transition — `lobby_state → OnLeaveSession` / `request_leave`) behind an *armed* flag we set only for
> intentional disconnects, so all the logic-driven leave triggers no-op — rather than hooking every event.
> Both peers run our mod, so the suppression is symmetric. Necessary but not sufficient: seamless also
> needs the *active* re-sync across area transitions. (Under RE now — worker `teardown-chokepoint`.)
>
> **In flight (parallel workers, 2026-07-03):** `teardown-chokepoint` (is the leave path one gateable
> chokepoint? → docs/SESSION-LIFECYCLE-FINDINGS.md) and `coop-flow` (the item-use → session/transport
> standup → matchmaking-handoff path, and whether a direct trigger dodges the menu grey →
> docs/COOP-FLOW-FINDINGS.md).

> **Protocol reference — `waygate-server` (cloned locally at `../waygate-server`).** vswarte's
> [Elden Ring matchmaking-server reimplementation](https://github.com/vswarte/waygate-server) (Rust, MIT —
> same author as our `fromsoftware-rs` SDK, so **clean-room-safe public RE**, not ERSC's closed bytes). It is
> the deepest open RE of ER's online/session **wire protocol**. It is **server-side** (it replaces FromSoft's
> matchmaking server), so it does **not** chart the client `CSSessionManager`/`NetworkSession` engine code or
> the slot-array allocation — our actual blocker is client-side and waygate won't hand us a driver. What it
> *does* give us is the **message exchange of the summon/join handshake** — i.e. what a real peer-join sends,
> which is precisely the flow that "sizes the slot array" in the capacity-0 finding above. Use it when running
> the 2-player create test to predict + interpret what the peer-join exchange does. Files of interest:
> `message/src/eldenring/session.rs` + `message/src/session.rs` (session messages), `…/sign.rs` +
> `server/src/handler/eldenring/sign.rs` (summon-sign → pulled-into-world, the in-world join we want to
> drive), `…/matchingticket.rs` + `…/quickmatch.rs` (match/session formation), and the `wire/` crate (the
> on-wire format + crypto).

> **What it takes to *call* the session is specified in [SESSION-DRIVE.md](SESSION-DRIVE.md)** — the
> minimal create/host + join calls, the args/state/keys each needs, and the loud SDK-survey result
> (the SDK charts the session object + FSM + transport but exposes **no** callable create/host/join, so
> the two initiation function entries remain the one genuine RE gap). Read it alongside the RE recipe
> below: SESSION-RE-RUNBOOK.md is *how to find* the two functions, SESSION-DRIVE.md is *how to drive*
> them once found.

- With two instances we control + the rung-2 channel to coordinate ("both call join now") + observer
  instrumentation, RE the create/join functions that move `CSSessionManager` to `Host`/`Client` for a
  given peer SteamID. Feed in the peer SteamID resolved by rung-4 lobby discovery; the **password
  derives the session AES key**.
- This is what gives **in-world presence** (the game's own net sync takes over once `Ingame`).
- **Rung 3 is also the apply layer for the UI that already ships.** The overlay's Open/Join/Leave
  already drive the connection layer (lobby + the rung-2 side-channel), but they don't yet put players
  in one another's *world*; that is rung 3. The host-only toggle verbs (Lock/Unlock/PvP/PvP
  teams/Friendly fire) are surfaced but still **inert** ("not wired up yet"); rung 3 connects them to
  real game calls. And the menu's collapsed toggle rows read `SessionContext.{world_locked, pvp_on,
  pvp_teams_on, friendly_fire_on}`, which the overlay still passes as always-`false`; the **state
  model behind those bits is now pre-built and host-tested** (see "The pre-built session core"
  below), so rung 3 only wires it: publish `Peer::session_toggles()` into the overlay's context and
  feed `Peer::observe_roster` (which already handles the departed-peer pruning on a roster shrink).
- Doable **without ERSC** via our own two instances + AOB-scan/hook of the `NetworkSession` vtable.
  ERSC observation stays an *optional accelerator* if blind RE stalls (restore the ERSC stack, watch
  one connect with external RE tooling — see [RUNTIME-RE.md](RUNTIME-RE.md)); the path does not
  *require* it.
- Once `Host`/`Client` is reached, the observer logs live transitions (the
  [RIG-RUNBOOK.md](RIG-RUNBOOK.md) "observation run" becomes executable *with our mod*), and the
  side-channel can optionally migrate in-band to `broadcast_packet`.

#### The pre-built session core (host-tested, ships ahead of the RE)

So the rung-3 landing is a thin binding rather than a design project, the session-side *decision
logic* is already built and unit-tested in `unseamless-core` (all of it exercised by
`scripts/test-core.sh`; none of it wired to the game yet). Three pieces:

- **Session toggle state** ([`session_state.rs`](../crates/unseamless-core/src/session_state.rs)) —
  the model behind the menu's collapsed toggle rows (`world_locked` / `pvp_on` / `pvp_teams_on` /
  `friendly_fire_on`). A `SessionToggles` state machine with explicit transitions
  (`apply(SessionAction)`: Lock/Unlock are absolute and idempotent, the `Toggle*` verbs flip), owned
  by `Peer` with **host authority**: the host's state moves only via its own
  `Peer::session_action` (applied at send time), and a joiner's replica follows **only
  host-confirmed transitions** — an inbound toggle mutates it only if it came from the *linked host*
  and passed the per-sender seq gate, the same authorize-by-sender rule `is_host_only` already
  enforces on actions. Each realized transition raises an ER-voiced, value-free toast
  (`ToggleChange::message`). Project the state onto the menu with `SessionToggles::write_context` —
  that call is what replaces the overlay's always-`false` `SessionContext` placeholders
  (`coop/overlay.rs` `session_context`).
- **Game-session roster + phantom→identity mapping**
  ([`roster.rs`](../crates/unseamless-core/src/roster.rs)) — `Roster` diffs per-frame roster
  snapshots (the SteamIDs in `CSSessionManager.players`) into join/leave **edges**, order- and
  duplicate-insensitive, and maintains the `PhantomHandle` (a phantom's `ChrIns` pointer, opaque to
  core) → SteamID map that color-by-SteamID nameplates and a future overhead display key on
  (NAMEPLATES.md). Bindings die with their peer, and a `retain_phantoms` sweep guards against
  pointer reuse.
- **Leave/evict integration** (`Peer::observe_roster`) — the shrink → evict → notify tie lives in
  core: a peer that disappears from the roster snapshot is fully `Peer::evict`ed from the
  side-channel (nonce, link, seq gates — so a rejoin re-links from scratch) and the lore-voiced
  departure toast is raised. Join edges are returned to the caller but deliberately not toasted
  (the arrival toast rides the side-channel link edge; the binding picks the player-facing edge).

**What the rung-3 binding wires** (and nothing more): feed `Peer::observe_roster` the roster
SteamIDs each frame (leaves are debounced in core — `roster::LEAVE_CONFIRM_SNAPSHOTS` — so a
transient partial read across a load transition can't mass-evict the party); discover the
phantom↔SteamID correlation (the one RE-gated piece) and maintain it via
`game_roster_mut().bind_phantom/retain_phantoms` (presence itself is `pub(crate)` — it moves only
through `observe_roster`/`evict`); publish `Peer::session_toggles()` to the overlay's
`SessionContext` (read on the Present thread, so mirror it through atomics like the existing
`coop::session_flags`); and drive the real lock/PvP/friendly-fire game effects **level-triggered
off `Peer::session_toggles()`** — the state is absolute, so absolute setters are the natural apply
shape. (Don't reach for `Peer::last_action` here: the host's *own* toggles never pass through it —
`session_action` applies state at send time without recording an inbound action.)

Two binding-side seams to reconcile at wiring time, recorded so they aren't rediscovered on the
rig: **(a)** `Peer`'s internal notification surface isn't the drawn one — the departure and toggle
toasts land in `Peer::notifications()`, and the binding must mirror them onto the overlay's `notify`
surface the way rung 2 already mirrors connect/version/liveness (reuse `ToggleChange::message` /
`PEER_DEPARTED_MESSAGE`; don't re-word). **(b)** eviction clears the peer's stale flag, and
`coop.rs`'s liveness tracking derives its "cooperator has returned" toast from exactly that
`is_stale` true→false transition — naive wiring would announce a *return* right after a real
departure, so the binding's lost-tracking must treat an evicted peer as gone, not recovered.

**Known residual (deliberate, wire-stable):** toggle state is carried only by the action frames
themselves — there is **no periodic state re-assert**. The exposure, precisely: the `Toggle*` verbs
are **relative flips**, so a frame lost to a drop *or* to reordering (the per-sender seq gate keeps
the stream ordered by discarding a late older frame) leaves a joiner's replica parity-inverted
**permanently** — later toggles preserve the inversion rather than heal it. The absolute verbs
(`Lock`/`Unlock`) self-heal on their next use, a joiner who links after the host already toggled
simply starts from the all-off default, and a host leave/restart is handled (eviction resets the
orphaned replica to the same default a rejoining host restarts from). Tolerable today: only the
*host's* menu renders the toggle rows, its state is always locally correct, and the actual in-world
effects are unwired. The fix, when it matters, is a ConfigSync-shaped
`SessionStateSync { epoch, generation, toggles }` re-asserted in `maintain` — a wire `VERSION` bump
(v9), which is why it's deferred rather than folded in here.

### Rung 4 — Discovery / lobby (the live connection path) — **SHIPPED + CONFIRMED (2026-06-27 friend test)**
- Password-keyed **Steam lobby** discovery, the **only** way the side-channel finds its peer (there is
  no manual SteamID exchange). The host (Open World) sets lobby data = password hash; the joiner (Join
  world) filters the lobby list by it. Steam's matchmaking lobby API makes this largely turnkey *at the
  API level*.

> **Scope reality — what rung 4 does and doesn't give.** Rung 4 is **independent of rung 3**. It links
> two modded games' **side-channels** (handshake, config-sync, log-forward) when one opens a world and
> the other joins it on the same password — no SteamID to hand-copy — but they still won't see each
> other *in the world* until rung 3 (the session-FSM RE) lands. So it's the live connection mechanism +
> a much nicer two-player test loop, not the in-world co-op piece.

**The connection model.** Co-op is triggered **on demand from the overlay menu**, never at launch — a
solo session pays nothing. The shared **password** is the only pairing input (and the lobby key). Three
explicit actions drive it:

- **Open World** (host): a best-effort existence check first (one filtered list on the password); if a
  lobby with that password already exists, it fails with a toast telling the user to **Join** instead.
  Otherwise it creates the lobby and waits — **no timeout** — for a friend to join.
- **Join world** (joiner): list on a cadence for an existing lobby keyed on the password, with a ~20 s
  timeout. Found ⇒ join the lowest-id match; none ⇒ "No open world found with this password."
- **Leave world**: tear the session down (leave the Steam lobby, stop the driver thread), re-enabling
  Open/Join.

The role is the user's **choice** (`steam::LobbyIntent::Host` / `Join`), **not derived** from a
create-or-join race — only the host ever creates a lobby, so there is no both-create race and no
owner/lobby-id tiebreak. The actions are **gated**: disabled until Steam networking is ready
(`crate::steam_ready` — a Connecting/Ready/Failed gate with a connecting banner) **and** the player is
in-game (`crate::playstate`), and disabled while already in a session (you can't host/join twice; Leave
is enabled instead). The resolved peer SteamID + chosen role are handed to rung 2's `SteamP2PTransport`,
which runs the side-channel exactly as before. Progress and results surface via an in-overlay **session
banner** + toasts.

**Poll, don't pump — the same trick rungs 1-2 use (rig-resolved 2026-06-26).** The earlier plan here
was to *register* call-result handlers (`SteamAPI_RegisterCallResult`, a `CCallbackBase*` C++-ABI) and
let ELDEN RING's own pump deliver them. The rig probe showed a cleaner path and a hazard to avoid:

- **ER pumps Steam via legacy `SteamAPI_RunCallbacks`.** `eldenring.exe`'s import table has
  `SteamAPI_RunCallbacks` + `SteamAPI_RegisterCallResult` and **no** `ManualDispatch` — so the game
  runs a normal per-frame `RunCallbacks` pump. This was the one empirical unknown gating the rung; it's
  answered.
- **Don't register *and* poll the same call — they conflict.** A `SteamAPICall_t` is consumed once.
  When the probe both registered a `CCallbackBase` call-result *and* polled the handle, ER's
  `RunCallbacks` consumed it **first**, so our poll then saw `InvalidHandle` (these were the earlier
  "IO failures"). Registering a handler is the hazard, not the fix.
- **So we POLL the call-result ourselves, no registration.** Each async lobby call (`CreateLobby`,
  `RequestLobbyList`, `JoinLobby`) returns a `SteamAPICall_t`; we poll it via **`ISteamUtils`
  `IsAPICallCompleted` + `GetAPICallResult`** (accessor `SteamAPI_SteamUtils_v010`) on the co-op driver
  thread — the exact poll-not-pump shape rungs 1-2 already use. No `CCallbackBase` vtable, no
  registration, nothing stolen from the game's queue. (Add `BLoggedOn` / `GetAPICallFailureReason` as
  diagnostics when a poll comes back empty.)
- **Rig-confirmed:** `CreateLobby` **succeeds in-process** — EResult OK, a real lobby id, polled out
  via `GetAPICallResult`. The host leg works. The **joiner-finds-host leg** (filter → list → join →
  resolve owner SteamID) is now **CONFIRMED end-to-end in the 2026-06-27 friend test**: the host
  resolved the joiner on its password-keyed lobby and the side-channel linked (see
  [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md)).

> Re-derive note (per [CLAUDE.md](../CLAUDE.md) > "Document how to re-derive RE results"): to re-confirm
> the dispatch model after a game update, dump `eldenring.exe`'s imports
> (`x86_64-w64-mingw32-objdump -p … | grep SteamAPI_`) and check for `RunCallbacks` (present) vs
> `ManualDispatch` (absent). The `InvalidHandle`-on-poll symptom is the tell that a handle was also
> registered somewhere — keep the lobby calls poll-only.

**The flow (feeds the existing side-channel).**
- Host (Open World): one filtered list to confirm no lobby with this password exists (else fail →
  "Join instead") → `CreateLobby` → poll `LobbyCreated_t` → `SetLobbyData("usc_pw", hash(password))` + a
  version tag so it's findable + identifiable as ours → wait for a member to join.
- Joiner (Join world): `AddRequestLobbyListStringFilter("usc_pw", hash(password))` → `RequestLobbyList`
  → poll `LobbyMatchList_t` → `GetLobbyByIndex` → `JoinLobby` → poll `LobbyEnter_t` → read the host's
  SteamID from the lobby owner.
- Then **hand the resolved peer SteamID + chosen role to rung 2's transport** — lobbies *replace* the
  manual copy-paste, they don't add a new transport.
- The discovery token (`SetLobbyData`/filter value) is `diagnostics::lobby_discovery_token` — a
  domain-separated SHA-256 over the *verbatim* password (prefix `"unseamless-coop/lobby-discovery/v1\0"`),
  truncated to the first 16 bytes as 32 lowercase hex chars — KAT-pinned so the DLL and the harness agree byte for
  byte.

**Build order (all three steps shipped and verified in the 2026-06-27 friend test).**
1. ✅ **Rig probe** — answered: ER pumps via `RunCallbacks`, `CreateLobby` succeeds, the path is
   poll-based (not register-based). Done 2026-06-26.
2. ✅ **Harness prototype** — the [`harness`](../crates/harness) crate is a normal exe and *can* link
   `steamworks-rs`; create/list/filter/join + the password-data scheme proven off the rig.
3. ✅ **DLL hand-bind (shipped)** — the poll-based `ISteamUtils`/`ISteamMatchmaking` path is bound in
   `coop/steam.rs` (the register-based `CCallbackBase` machinery is gone), driven on demand by the
   Open World / Join world actions and feeding the resolved host SteamID + chosen role into the
   side-channel. Verified end-to-end (rig-confirmed note above).

## Steam integration: hand-bind the flat C API at runtime (do NOT take the crate)

**Decision:** resolve the Steamworks **flat C API** at runtime via `GetProcAddress` against the
**already-loaded `steam_api64.dll`**, in a `coop/steam.rs` module shaped like
[`input.rs`](../crates/unseamless-coop/src/input.rs)/[`saves.rs`](../crates/unseamless-coop/src/saves.rs).
Do **not** add `steamworks-rs` as a cdylib dependency. Use the crate only as (a) a reference for call
shapes/struct layouts and (b) a harness-side prototyping tool.

Why not the crate in the DLL:
1. **It doesn't link on `windows-gnu`** (our cdylib target, mandated by the FromSoft SDK + hudhook).
   Confirmed in the maintainer's [issue #274](https://github.com/Noxime/steamworks-rs/issues/274):
   `steam_api64` is MSVC-oriented, the GNU target fails at link/runtime, no documented workaround.
2. **Shared callback dispatch.** Steamworks is async; the crate assumes it **owns dispatch** — it runs
   its own pump (`SteamAPI_RunCallbacks` / `ManualDispatch`) to deliver callbacks. An injected DLL must
   not: the game already pumps Steam (`eldenring.exe` imports `SteamAPI_RunCallbacks`, confirmed on the
   rig), and a second pump steals the game's events. We never pump — **every async result we need,
   including the rung-4 lobby calls, we get by *polling* the `SteamAPICall_t` ourselves** (see below),
   which is exactly the model the crate isn't built for.

Why hand-binding is small for our needs:
- **Use the poll-based data path — for both the side-channel and lobby discovery.**
  `ISteamNetworkingMessages` (`SendMessageToUser` + `ReceiveMessagesOnChannel`, with
  `AcceptSessionWithUser` for the known peer) **does not require the callback queue** — receiving is a
  poll on our own frame task; sending to a user auto-opens the session. The rung-4 lobby calls are
  async (`SteamAPICall_t`), but we poll those too via `ISteamUtils` `IsAPICallCompleted` +
  `GetAPICallResult` rather than registering a call-result handler — so the whole mod stays poll-only
  and never touches the game's dispatch. (Rig lesson: don't *also* register a `CCallbackBase`
  call-result; it conflicts with the game's pump. Poll, don't register. See rung 4 above.)
- **Don't manage Steam lifecycle.** The game already called `SteamAPI_Init`; we **never** call
  `SteamAPI_Init`/`Shutdown`. We just call the interface accessor + a handful of functions.
- Net surface for rungs 1-3 is ~10-15 flat functions, e.g. (accessor names are versioned — resolve by
  exact exported name, re-resolve after a Steam client update). The names below were dumped from ELDEN
  RING's own `steam_api64.dll` on 2026-06-25 (`x86_64-w64-mingw32-objdump -p … | grep SteamAPI_…`):
  - identity (rung 1, **confirmed live**): `SteamAPI_SteamUser_v021` accessor (rung 1 probes a
    descending version window so a bump self-heals) + `SteamAPI_ISteamUser_GetSteamID` (unversioned).
  - networking (rung 2, **now bound + called** in `coop/steam.rs`): `SteamAPI_SteamNetworkingMessages_SteamAPI_v002`
    accessor + `SteamAPI_ISteamNetworkingMessages_SendMessageToUser` / `_ReceiveMessagesOnChannel` /
    `_AcceptSessionWithUser`. (`_CloseSessionWithUser` is present in the dump but left unbound — there's
    no session teardown in rung 2; the channel lives for the process.)
  - async-call polling (rung 4, the poll-not-pump path): `SteamAPI_SteamUtils_v010` accessor +
    `SteamAPI_ISteamUtils_IsAPICallCompleted` / `_GetAPICallResult` (and `_GetAPICallFailureReason` for
    diagnostics). This is how we read a `SteamAPICall_t` result without registering a call-result.
  - lobby discovery (rung 4, **`CreateLobby` rig-proven; joiner leg CONFIRMED in the 2026-06-27 friend test**):
    the `SteamAPI_SteamMatchmaking_v0NN` accessor (resolve the exact `_v0NN` from the rig dump) +
    `SteamAPI_ISteamMatchmaking_CreateLobby` /
    `_SetLobbyData` / `_AddRequestLobbyListStringFilter` / `_RequestLobbyList` / `_GetLobbyByIndex` /
    `_JoinLobby` / `_GetLobbyOwner` (resolve names by exact exported symbol on the rig and pin them next
    to the binding; the accessor version may bump with the Steam client).
  - `SteamNetworkingIdentity` / `SteamNetworkingMessage_t` are built/parsed from the public
    `steamnetworkingtypes.h` POD layout directly (charted by offset, with compile-time `offset_of!`
    guards), rather than via the `SteamAPI_SteamNetworkingIdentity_*` helper exports.

Where the crate still earns its keep:
- **As the map:** its source is the cleanest documentation of the exact `SteamAPI_*` flat names,
  argument order, and the message/identity struct layouts we FFI against. Read it, don't ship it.
- **In the harness:** [`crates/harness`](../crates/harness) is a normal native exe (no mingw/SDK
  constraint), so it *can* depend on `steamworks` to prototype the lobby + P2P flow off the rig before
  we hand-bind the flat path in the DLL. Fits the existing layered testing.

A separate MSVC-built Steam helper process is theoretically possible but not worth the two-toolchain +
IPC complexity.

## Side-channel input hardening (audit, 2026-07-02)

The full inbound path — `framing.rs` → `protocol.rs` decode, fed by `steam.rs`/`coop.rs`
(`SteamP2PTransport`), `bridge.rs` (`BridgeTransport`), and consumed by `peer.rs` — was audited for
panic paths and unbounded allocation on untrusted peer bytes. A panic in the recv/decode path would
be caught by the per-feature `catch_unwind` firewall but would **disable co-op for the session**, so
a malformed/hostile peer must never reach one. The defenses, and where each is enforced:

- **Total decoders, no panic paths.** `protocol::Reader` bounds-checks every read (`Truncated`, never
  a slice panic); `framing::FrameDecoder` checks each declared length **before** buffering/allocating.
  Pinned by deterministic fuzz + exhaustive per-byte truncation tests in both modules, a bit-flip
  sweep, and a composed framing→decode pipeline fuzz (`full_inbound_pipeline_never_panics_…`).
- **Allocation bounds, checked before the allocation they size:** `framing::MAX_FRAME` (64 KiB) on
  the stream framing; `steam.rs`'s own 8 KiB per-message ceiling before copying a Steam payload out;
  `protocol::MAX_LOG_MSG` (2 KiB) enforced at **decode** too (`DecodeError::TooLong`), so a forged
  u16 length can't make a linked flooder retain ~32× more per record in the host's `LogBundle`
  (itself capped at 50k records, drop-oldest); `RECV_BATCH`×`RECV_MAX_CALLS` caps frames per poll.
- **Untrusted values clamped at decode:** `ConfigSync` scaling / `max_players` / world-time are held
  to the same ranges as a local config file; unknown versions, tags, actions, and levels reject.
- **Per-sender state is bounded:** `peer::MAX_TRACKED_PEERS` (64) caps the roster/nonce/liveness maps
  a stranger flood can grow (senders are transport-authenticated Steam ids, so distinct ids cost real
  accounts); a full roster turns **new** senders away quietly (one keyed banner + a
  `roster_overflow_drops` counter — no per-frame toasts a stranger could spam), and `Peer::evict`
  frees slots. Caveat: `evict` isn't wired to the session FSM yet (rung 3), so until then a
  transient flood pins the roster at the cap. Production transports additionally filter to the
  configured partner before frames reach `Peer`, so the cap is a backstop, not the first line.
- **Authorization before effect:** only a *linked* (password-proven) peer's `ConfigSync`/actions/logs
  apply; host-only actions check the sender's role; dedup gates make actions exactly-once.

Malformed frames are dropped and **counted** (`Session::decode_failures`), never fatal — per the
degrade-don't-crash rule. The wire format itself needed no change (still VERSION 8): every fix is
decode-side enforcement of bounds well-behaved encoders already obey.

## Log-forwarding status (answers a recurring question)

`[debug] forward_to_host = true` (set in the friend seed config) was a no-op until rung 2: the
host-tested forwarding logic in [`peer.rs`](../crates/unseamless-core/src/peer.rs) only runs over a
`Transport`, and the cdylib had **no live transport** in real co-op. **Rung 2 provides it.** Once a
partner is linked (via rung-4 lobby discovery) and `forward_to_host` is on, a client's
[`ForwardLogger`](../crates/unseamless-coop/src/forward.rs) tees its records into a bounded queue that
the co-op driver drains through `Peer::forward_log` onto the Steam side-channel, where the host
aggregates them into its `LogBundle`. The host driver surfaces that bundle as per-peer files under the
host's `unseamless-coop/logs/` folder, named like
`unseamless_coop-forwarded-<run_id>-peer-XXXXXXXX.log`; Export diagnostics also force-writes the latest
published snapshot and lists those files in `unseamless-coop-diagnostics.txt`. The artifacts use
`peer_tag` labels and the same SteamID scrubber as Export because forwarded records are raw client log
lines. Caveats: it's **client→host only**, gated on a configured peer, and bounded/rate-limited (a flood
is dropped, not buffered without limit). Until a session is actually linked, the **manual "zip your
`logs\` folder and send it" instruction in
[README-FRIENDS.txt](../scripts/dist/README-FRIENDS.txt) is still the fallback** — but the automatic path
now exists and lights up the moment two modded games link.

## Open questions / risks (confirm on the rig)

- **Steam P2P auth/NAT (rung 2).** Messaging two arbitrary SteamIDs via `ISteamNetworkingMessages`
  may require the accounts to be Steam **friends** (or share a Steam networking session) for
  NAT-punch/auth. **Confirmed working for friends** (2026-06-27 test); whether *non-friends* can link is
  still open. This is a *Steam* connection detail, not the game's matchmaking lobby, so it doesn't violate
  "defer the lobby." (The 2026-07-03 rig↔Deck run linked a main account with the personal throwaway
  without any friending step done for the test — suggestive, but the accounts' pre-existing friend
  status wasn't verified, so "non-friends" stays unconfirmed.)
- **DLC parity between peers (design question, undecided 2026-07-03).** Vanilla ER allows base-game
  co-op with mixed DLC ownership and gates DLC-*area* sessions itself, so nothing blocks the current
  rungs (link + create don't care; the Deck throwaway owns no DLC). Decide later whether the mod should
  surface a friendly "partner lacks the DLC" notice (the side-channel handshake could carry a DLC bit)
  instead of letting the game's own gating produce a confusing failure deep in a session. Related,
  practical: a *save* containing DLC data refuses to load on a DLC-less account — that's a seeding
  concern (see the steam-deck skill), not a connection one.
- **`SteamNetworkingMessagesSessionRequest_t`.** ✅ Incoming sessions normally surface via this callback;
  since we avoid the callback queue, we rely on **proactive `AcceptSessionWithUser`** (we know the peer)
  and the implicit-open-on-send behavior. The friend test confirmed both sides establish without us
  pumping callbacks.
- **Flat-API symbol versions.** Accessor names carry a version (`…_v002`, etc.) that must match the
  rig's `steam_api64.dll`. Resolve by name; re-derive after a Steam client update (document the names
  next to the binding per [CLAUDE.md](../CLAUDE.md) > "Document how to re-derive RE results").
- **Rung 3 is the real gate.** In-world co-op blocks on the create/join RE. Rungs 1-2 work *around* it
  but don't eliminate it.
- **Lobby async results (rung 4).** ✅ **Resolved on the rig (2026-06-26):** ER pumps Steam via legacy
  `RunCallbacks` (no `ManualDispatch`), `CreateLobby` succeeds in-process, and we **poll** each
  `SteamAPICall_t` (`ISteamUtils` `IsAPICallCompleted` + `GetAPICallResult`) rather than register
  call-results (which conflict → `InvalidHandle` on poll). Joiner-finds-host leg since CONFIRMED in the
  friend test. Full detail in rung 4 above.

## Concrete next step

Rungs 1, 2, and 4 are shipped and **CONFIRMED live across two machines** (2026-06-27 friend test, numbers
in the status header above). The out-of-band connection stack is done and verified.

The next move is **rung 3's 2-player create-drive test** (needs a friend / second machine): with
`drive_create` + `force_netsession_ready` + `bypass_session_create_gate` set on both peers, open+join a
rung-4 lobby and let create fire **with a real peer present** (the leg-B session registry/init lookup at
`0x1423fa1b0` most likely needs the peer/match context a solo drive can't give). Does `lobby_state` reach
`TryToCreateSession`/`Host`? Full state in the **Rung 3 STATE callout above**; step-by-step in
[FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md) > "Rung-3 create-drive test". If a real peer still doesn't
satisfy it, keep tracing the registry chain or fall back to ERSC-style session neutralization. Rungs 2+4
give the linked coordination channel ("both go now") and the two instances to drive against.

## Cross-references

- [ARCHITECTURE.md](ARCHITECTURE.md) — the two layers, "drive the game's networking" decision, the
  in-band side-channel + self-healing design, divergences.
- [SDK-COVERAGE.md](SDK-COVERAGE.md) — per-subsystem charted/gap inventory (networking/session).
- [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md) — the two-player smoke test: lobby-discovery
  connect (rungs 2+4) plus the folded-in rung-3 create/join capture, in one friend session.
- [SESSION-DRIVE.md](SESSION-DRIVE.md) — the rung-3 "drive a session directly" call spec: the minimal
  create/join calls + the args/state/keys each needs + the SDK-survey result (no callable initiation).
- [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) — the rung-3 create/join RE recipe: the gated
  `session-probe` instrumentation (`coop/session_probe`) and the exact "find these two initiation
  functions" task for the rig.
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) — the session observation run (executable once rung 3 lands).
- [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md) — the offline/EAC presentation and why we're
  "offline" but Steam-connected.
- [RUNTIME-RE.md](RUNTIME-RE.md) — Frida/Steam-API/packet tooling for the optional ERSC-observation
  accelerator and the rung-3 RE.
- **External reference — `../waygate-server`** (cloned locally): vswarte's ER matchmaking-server
  reimplementation (Rust, MIT, clean-room-safe). The deepest open RE of ER's online/session **wire
  protocol** (server-side); a reference for the summon/join message exchange when working rung-3. See the
  "Protocol reference — `waygate-server`" note under [Rung 3](#rung-3--drive-the-games-session-the-hard-re-on-our-terms).
- Side-channel code: [`peer.rs`](../crates/unseamless-core/src/peer.rs),
  [`protocol.rs`](../crates/unseamless-core/src/protocol.rs),
  [`transport.rs`](../crates/unseamless-core/src/transport.rs),
  [`bridge.rs`](../crates/unseamless-coop/src/bridge.rs).
