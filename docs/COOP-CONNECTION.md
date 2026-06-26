# Co-op Connection Plan

How two unseamless-coop players actually get connected. This is the Layer-2 "getting players into
one another's worlds" problem from [ARCHITECTURE.md](ARCHITECTURE.md), planned out: what's possible
today, what's reverse-engineering-gated, the incremental build order, and the decided approach for
talking to Steam.

> **Status: design + research, nothing here is built.** The mod loads, configures, and observes a
> session today; it does **not** connect players yet. This doc is the spec for building that, written
> for session handoff. Everything game-internal is grounded in the pinned `fromsoftware-rs` SDK or
> flagged as inference to confirm on the rig (per [CLAUDE.md](../CLAUDE.md) > Clean-room hygiene).

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

Each rung is independently testable and de-risks the next. Rungs 1-2 need no game-session RE; rung 3
is the one genuinely hard step; rung 4 is deferred.

### Rung 1 — Identity + copy button (small, safe, solo-testable)
- Bind the Steamworks flat API (below), read our own SteamID.
- Overlay: show the SteamID with a copy-to-clipboard button; also log it.
- Establishes the Steam integration the later rungs need. Lets two players exchange IDs out of band
  (Discord). Test: does the button copy your real SteamID, solo.

### Rung 2 — Private Steam P2P side-channel (the real unblock)
- Implement a `SteamP2PTransport` satisfying the existing
  [`Transport`](../crates/unseamless-core/src/transport.rs) trait (`PeerId = u64` is already "a Steam
  ID in production"), over poll-based `ISteamNetworkingMessages` to a **manually-entered** peer
  SteamID.
- Run the existing host-tested `Peer`/`Session` over it — the *whole* side-channel (version
  handshake, `ConfigSync`, session actions, log-forward), already proven on `Loopback`/`TcpTransport`/
  the bridge.
- **Payoff:** two real games' mods handshake, the host pushes config, and **log-forwarding actually
  starts working** (it is inert today — see "Log-forwarding status"). First real peer-to-peer test of
  the side-channel. Friends "connect" in the mod sense (toasts, synced config), not yet in-world.
- Slots into the same `Transport` seam as `BridgeTransport`; the bridge (loopback) was the dev-host
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

### Rung 4 — Discovery / lobby (deferred)
- Replace manual SteamID exchange with password-keyed **Steam lobby** discovery (host sets lobby data
  = password hash; joiner filters the lobby list by it). Steam's matchmaking lobby API makes this
  largely turnkey *at the API level*.
- Deferred on purpose, and not only for scope: lobbies are the part that genuinely needs Steam
  **callbacks/call-results**, which is the one piece with a real in-process hazard (see "Steam
  integration"). Revisit the integration choice when we get here.

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
2. **Shared callback dispatch.** Steamworks is async; consuming callbacks/call-results means running
   the manual-dispatch loop (`SteamAPI_ManualDispatch_*`), which **shares one callback queue with the
   game** (the game already pumps Steam). Pumping it ourselves steals the game's events. `steamworks-rs`
   assumes it owns dispatch — exactly what an injected DLL must not do.

Why hand-binding is small for our needs:
- **Use the poll-based data path.** `ISteamNetworkingMessages` (`SendMessageToUser` +
  `ReceiveMessagesOnChannel`, with `AcceptSessionWithUser` for the known peer) **does not require the
  callback queue** — receiving is a poll on our own frame task. Sending to a user auto-opens the
  session. This sidesteps the dispatch conflict entirely.
- **Don't manage Steam lifecycle.** The game already called `SteamAPI_Init`; we **never** call
  `SteamAPI_Init`/`Shutdown`. We just call the interface accessor + a handful of functions.
- Net surface for rungs 1-3 is ~10-15 flat functions, e.g. (names are versioned — resolve by exact
  exported name, re-resolve after a Steam client update):
  - identity: `SteamAPI_SteamUser_v0__` accessor + `SteamAPI_ISteamUser_GetSteamID`
  - networking: `SteamAPI_SteamNetworkingMessages_SteamAPI_v002` accessor +
    `SteamAPI_ISteamNetworkingMessages_SendMessageToUser` / `_ReceiveMessagesOnChannel` /
    `_AcceptSessionWithUser` / `_CloseSessionWithUser`
  - `SteamNetworkingIdentity` setup (set the peer's SteamID).

Where the crate still earns its keep:
- **As the map:** its source is the cleanest documentation of the exact `SteamAPI_*` flat names,
  argument order, and the message/identity struct layouts we FFI against. Read it, don't ship it.
- **In the harness:** [`crates/harness`](../crates/harness) is a normal native exe (no mingw/SDK
  constraint), so it *can* depend on `steamworks` to prototype the lobby + P2P flow off the rig before
  we hand-bind the flat path in the DLL. Fits the existing layered testing.

A separate MSVC-built Steam helper process is theoretically possible but not worth the two-toolchain +
IPC complexity.

## Log-forwarding status (answers a recurring question)

`[debug] forward_to_host = true` (set in the friend seed config) is **currently a no-op**. The
forwarding logic exists and is host-tested in [`peer.rs`](../crates/unseamless-core/src/peer.rs), but
it only runs over a `Transport`, and the cdylib constructs **no live transport** in real co-op (the
`bridge` is loopback + off in friend builds; the in-band `GameTransport` over `broadcast_packet`
doesn't exist yet). So today each machine writes only its own local log, and the **manual "zip your
`logs\` folder and send it" instruction in [README-FRIENDS.txt](../scripts/dist/README-FRIENDS.txt)
is the only mechanism that works.** **Rung 2 is what lights forwarding up** (it provides the missing
transport); once it lands, the host aggregates everyone's logs and the manual step can go away.

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
- **Lobby callbacks (rung 4).** When we need lobbies, we either accept manual-dispatch on a dedicated
  pipe, prototype/validate it in the harness with the crate, or keep manual SteamID exchange.

## Concrete next step

Build **rung 1**: the Steamworks flat-API binding (`coop/steam.rs`) reading our own SteamID, plus the
overlay copy button — and as part of it, do the `windows-gnu` link/resolve check for the flat-API
approach. Small, safe, solo-testable, and it bootstraps everything above.

## Cross-references

- [ARCHITECTURE.md](ARCHITECTURE.md) — the two layers, "drive the game's networking" decision, the
  in-band side-channel + self-healing design, divergences.
- [SDK-COVERAGE.md](SDK-COVERAGE.md) — per-subsystem charted/gap inventory (networking/session).
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) — the session observation run (executable once rung 3 lands).
- [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md) — the offline/EAC presentation and why we're
  "offline" but Steam-connected.
- [RUNTIME-RE.md](RUNTIME-RE.md) — Frida/Steam-API/packet tooling for the optional ERSC-observation
  accelerator and the rung-3 RE.
- Side-channel code: [`peer.rs`](../crates/unseamless-core/src/peer.rs),
  [`protocol.rs`](../crates/unseamless-core/src/protocol.rs),
  [`transport.rs`](../crates/unseamless-core/src/transport.rs),
  [`bridge.rs`](../crates/unseamless-coop/src/bridge.rs).
