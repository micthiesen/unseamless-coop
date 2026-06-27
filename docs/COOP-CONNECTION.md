# Co-op Connection Plan

How two unseamless-coop players actually get connected. This is the Layer-2 "getting players into
one another's worlds" problem from [ARCHITECTURE.md](ARCHITECTURE.md), planned out: what's possible
today, what's reverse-engineering-gated, the incremental build order, and the decided approach for
talking to Steam.

> **Status: rungs 1-2 shipped; rung 4 (lobby discovery) is the chosen connection path and is landing
> this wave — solo create+poll rig-proven, the joiner leg pending the friend test; rung 3 is the
> remaining hard RE.** The mod loads, configures, observes a session, reads our own SteamID (rung 1,
> `coop/steam.rs`), and stands up a private Steam P2P **side-channel** running the host-tested
> `Peer`/`Session` for real (handshake, host config push, liveness, client→host log forwarding). The
> design decision for this wave: **drop the hand-copied SteamID** in favor of **password-keyed Steam
> lobby discovery** (rung 4) to seed that side-channel — both players set the same password, a
> create-or-join resolves who hosts, and the resolved peer feeds the existing transport. (The lobby
> driver lands behind a gate this wave; until it flips, the manual path is what runs.) None of this
> yet puts players in one another's *world* — that's the game-session RE (rung 3). This doc is the spec
> for the rest, written for session handoff. Everything game-internal is grounded in the pinned
> `fromsoftware-rs` SDK or flagged as inference to confirm on the rig (per
> [CLAUDE.md](../CLAUDE.md) > Clean-room hygiene).

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

### Rung 2 — Private Steam P2P side-channel (the real unblock) — **SHIPPED (pending two-player rig run)**
- `SteamP2PTransport` ([`coop/coop.rs`](../crates/unseamless-coop/src/coop.rs)) satisfies the existing
  [`Transport`](../crates/unseamless-core/src/transport.rs) trait (`PeerId = u64` is already "a Steam
  ID in production"), over poll-based `ISteamNetworkingMessages`. The peer SteamID and host/client
  role are moving off hand-configuration: once rung 4 lands they're handed to the transport by **lobby
  discovery** (create-or-join on the shared password resolves who hosts and who the peer is). The old
  `[coop] peer_steam_id` + `is_host` manual-pairing path is **being retired** this wave (it's still the
  only path that runs until the lobby gate flips); rung 1's copy button then stays only for
  visibility/debugging, not as the pairing mechanism.
- Runs the existing host-tested `Peer`/`Session` over it — the *whole* side-channel (version
  handshake, `ConfigSync`, liveness, log-forward), already proven on `Loopback`/`TcpTransport`/the
  bridge. The driver mirrors received config into the live config and surfaces connect / version-
  mismatch / lost-contact events to the overlay ([`coop/notify`](../crates/unseamless-coop/src/notify.rs)).
- **Log-forwarding is now wired** ([`coop/forward.rs`](../crates/unseamless-coop/src/forward.rs)): a
  `ForwardLogger` tees records into a bounded queue that the driver drains through `Peer::forward_log`
  onto the wire (a forwarding *client* only; own-module lines are dropped to avoid a feedback loop).
  This is the transport "Log-forwarding status" below was waiting on.
- **Implementation grounded; the open piece is the two-player rig run** — confirm the NAT/auth open
  question (peers may need to be Steam friends), that both sides establish without us pumping the
  SessionRequest callback (we proactively `AcceptSessionWithUser`), and that the `coop_connect` report
  goes `linking → linked`. This now happens as part of the **lobby-discovery friend test** (see
  [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md)), since discovery is what seeds the peer.
- Slots into the same `Transport` seam as `BridgeTransport`; the bridge (loopback) was the host-side
  rehearsal for exactly this.

### Rung 3 — Drive the game's session (the hard RE, on our terms)
- With two instances we control + the rung-2 channel to coordinate ("both call join now") + observer
  instrumentation, RE the create/join functions that move `CSSessionManager` to `Host`/`Client` for a
  given peer SteamID. Feed in the SteamID from rung 1; the **password derives the session AES key**.
- This is what gives **in-world presence** (the game's own net sync takes over once `Ingame`).
- Doable **without ERSC** via our own two instances + AOB-scan/hook of the `NetworkSession` vtable.
  ERSC observation stays an *optional accelerator* if blind RE stalls (restore the ERSC stack, watch
  one connect with external RE tooling — see [RUNTIME-RE.md](RUNTIME-RE.md)); the path does not
  *require* it.
- Once `Host`/`Client` is reached, the observer logs live transitions (the
  [RIG-RUNBOOK.md](RIG-RUNBOOK.md) "observation run" becomes executable *with our mod*), and the
  side-channel can optionally migrate in-band to `broadcast_packet`.

### Rung 4 — Discovery / lobby (the live connection path) — **IN PROGRESS (solo create+poll rig-proven)**
- Password-keyed **Steam lobby** discovery, becoming the **only** way the side-channel finds its peer
  (the manual SteamID exchange is being retired this wave). Host sets lobby data = password hash; joiner
  filters the lobby list by it. Steam's matchmaking lobby API makes this largely turnkey *at the API
  level*.

> **Scope reality — what rung 4 does and doesn't give.** Rung 4 is **independent of rung 3**. It makes
> two modded games with the same password auto-link their **side-channels** (handshake, config-sync,
> log-forward) instead of hand-copying a SteamID — but they still won't see each other *in the world*
> until rung 3 (the session-FSM RE) lands. So it's the live connection mechanism + a much nicer
> two-player test loop, not the in-world co-op piece. It retires the manual pairing, it doesn't replace
> rung 3.

**The connection model (decided).** No host/client choice, no IDs to copy: **both players set the same
password and launch.** Lobby discovery does a **create-or-join** — the first to come up creates the
lobby and becomes host; the other finds it and joins. The role (`is_host`) is **derived** from that
outcome, never configured. The resolved peer SteamID + role are handed to rung 2's `SteamP2PTransport`,
which then runs the side-channel exactly as before.

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
  resolve owner SteamID) is authored but still needs the **two-player friend test** to confirm
  end-to-end (see [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md)).

> Re-derive note (per [CLAUDE.md](../CLAUDE.md) > "Document how to re-derive RE results"): to re-confirm
> the dispatch model after a game update, dump `eldenring.exe`'s imports
> (`x86_64-w64-mingw32-objdump -p … | grep SteamAPI_`) and check for `RunCallbacks` (present) vs
> `ManualDispatch` (absent). The `InvalidHandle`-on-poll symptom is the tell that a handle was also
> registered somewhere — keep the lobby calls poll-only.

**The flow (feeds the existing side-channel).**
- Host: `CreateLobby` → poll `LobbyCreated_t` → `SetLobbyData("usc_pw", hash(password))` + a version tag
  so it's findable + identifiable as ours.
- Joiner: `AddRequestLobbyListStringFilter("usc_pw", hash(password))` → `RequestLobbyList` → poll
  `LobbyMatchList_t` → `GetLobbyByIndex` → `JoinLobby` → poll `LobbyEnter_t` → read the host's SteamID
  from the lobby owner/members.
- Then **hand the resolved peer SteamID + derived role to rung 2's transport** — lobbies *replace* the
  manual copy-paste, they don't add a new transport.
- The discovery token (`SetLobbyData`/filter value) is `diagnostics::lobby_discovery_token` — a
  domain-separated SHA-256 over the *verbatim* password (prefix `"unseamless-coop/lobby-discovery/v1\0"`),
  truncated to the first 16 bytes as 32 lowercase hex chars — KAT-pinned so the DLL and the harness agree byte for
  byte.

**Build order (the awkward part is resolved; what's left is the friend test).**
1. ✅ **Rig probe** — answered: ER pumps via `RunCallbacks`, `CreateLobby` succeeds, the path is
   poll-based (not register-based). Done 2026-06-26.
2. ✅ **Harness prototype** — the [`harness`](../crates/harness) crate is a normal exe and *can* link
   `steamworks-rs`; create/list/filter/join + the password-data scheme proven off the rig.
3. **DLL hand-bind (in progress)** — bind the poll-based `ISteamUtils` path in `coop/steam.rs` (replace
   the dormant register-based `CCallbackBase` machinery) and feed the resolved host SteamID + derived
   role into the side-channel. Solo `CreateLobby` is rig-proven; the joiner leg lands with the friend
   test.

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
  and never touches the game's dispatch. (Rig lesson: registering a `CCallbackBase` call-result *and*
  polling the same handle conflicts — ER's `RunCallbacks` consumes it first and the poll sees
  `InvalidHandle`; poll, don't register. See rung 4 above.)
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
  - lobby discovery (rung 4, **`CreateLobby` rig-proven; joiner leg pending the friend test**):
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

## Log-forwarding status (answers a recurring question)

`[debug] forward_to_host = true` (set in the friend seed config) was a no-op until rung 2: the
host-tested forwarding logic in [`peer.rs`](../crates/unseamless-core/src/peer.rs) only runs over a
`Transport`, and the cdylib had **no live transport** in real co-op. **Rung 2 provides it.** Once a
partner is linked (via rung-4 lobby discovery) and `forward_to_host` is on, a client's
[`ForwardLogger`](../crates/unseamless-coop/src/forward.rs) tees its records into a bounded queue that
the co-op driver drains through `Peer::forward_log` onto the Steam side-channel, where the host
aggregates them into its `LogBundle`. Caveats: it's **client→host only**, gated on a configured peer,
and bounded/rate-limited (a flood is dropped, not buffered without limit). Until a session is actually
linked, the **manual "zip your `logs\` folder and send it" instruction in
[README-FRIENDS.txt](../scripts/dist/README-FRIENDS.txt) is still the fallback** — but the automatic
path now exists and lights up the moment two modded games link.

## Open questions / risks (confirm on the rig)

- **Steam P2P auth/NAT (gates rung 2).** Messaging two arbitrary SteamIDs via `ISteamNetworkingMessages`
  may require the accounts to be Steam **friends** (or share a Steam networking session) for
  NAT-punch/auth. For friends this usually suffices; verify early. This is a *Steam* connection detail,
  not the game's matchmaking lobby, so it doesn't violate "defer the lobby."
- **`SteamNetworkingMessagesSessionRequest_t`.** Incoming sessions normally surface via this callback;
  since we avoid the callback queue, rely on **proactive `AcceptSessionWithUser`** (we know the peer)
  and/or the implicit-open-on-send behavior. Confirm both sides establish without us pumping callbacks.
- **Flat-API symbol versions.** Accessor names carry a version (`…_v002`, etc.) that must match the
  rig's `steam_api64.dll`. Resolve by name; re-derive after a Steam client update (document the names
  next to the binding per [CLAUDE.md](../CLAUDE.md) > "Document how to re-derive RE results").
- **Rung 3 is the real gate.** In-world co-op blocks on the create/join RE. Rungs 1-2 work *around* it
  but don't eliminate it.
- **Lobby async results (rung 4).** ✅ **Resolved on the rig (2026-06-26).** ER pumps Steam via legacy
  `RunCallbacks` (imports confirm it; no `ManualDispatch`), and `CreateLobby` succeeds in-process. We do
  **not** register call-results — that conflicts with the game's pump (`InvalidHandle` on poll); instead
  we **poll** each `SteamAPICall_t` via `ISteamUtils` `IsAPICallCompleted` + `GetAPICallResult`. Only the
  **joiner-finds-host** leg (filter/list/join/resolve) remains to confirm, in the two-player friend test.

## Concrete next step

Rungs 1-2 are shipped (`coop/steam.rs` + `coop/coop.rs` + `coop/forward.rs`); rung 4's solo half is
rig-proven (`CreateLobby` succeeds, polled). The immediate next action is the **two-player friend
test** — now a *single* lobby-discovery run that exercises rungs 2 and 4 together (full recipe in
[FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md)): both players set the **same password** and launch,
discovery does create-or-join (one becomes host), and we confirm the joiner finds the host's lobby, the
peer SteamID + role resolve, and the side-channel links — the `coop_connect` report should walk
`linking → linked`, an overlay "Co-op partner connected" toast should fire, the client should adopt the
host's config, and (with `forward_to_host`) the host's `LogBundle` should pick up the client's lines.
Watch the NAT/auth open question (the peers may need to be Steam friends).

**That same friend session also feeds rung 3.** With two real instances connected, turn on
`[debug.probes] session_probe` and Frida-watch our own `CSSessionManager.lobby_state` while the friend
hosts once and joins once — charting the create/join initiation functions (the runbook folds this in).
One good friend session moves rungs 2, 4, **and** 3.

Once the create/join functions are charted, build **rung 3** proper — drive the *game's* session
(`CSSessionManager` → `Host`/`Client`) so players see each other in-world. Rungs 2+4 give us the linked
coordination channel ("both go now") and two instances we control to RE against.

## Cross-references

- [ARCHITECTURE.md](ARCHITECTURE.md) — the two layers, "drive the game's networking" decision, the
  in-band side-channel + self-healing design, divergences.
- [SDK-COVERAGE.md](SDK-COVERAGE.md) — per-subsystem charted/gap inventory (networking/session).
- [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md) — the two-player smoke test: lobby-discovery
  connect (rungs 2+4) plus the folded-in rung-3 create/join capture, in one friend session.
- [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) — the rung-3 create/join RE recipe: the gated
  `session-probe` instrumentation (`coop/session_probe`) and the exact "find these two initiation
  functions" task for the rig.
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) — the session observation run (executable once rung 3 lands).
- [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md) — the offline/EAC presentation and why we're
  "offline" but Steam-connected.
- [RUNTIME-RE.md](RUNTIME-RE.md) — Frida/Steam-API/packet tooling for the optional ERSC-observation
  accelerator and the rung-3 RE.
- Side-channel code: [`peer.rs`](../crates/unseamless-core/src/peer.rs),
  [`protocol.rs`](../crates/unseamless-core/src/protocol.rs),
  [`transport.rs`](../crates/unseamless-core/src/transport.rs),
  [`bridge.rs`](../crates/unseamless-coop/src/bridge.rs).
