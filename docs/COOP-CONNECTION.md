# Co-op Connection Plan

How two unseamless-coop players actually get connected. This is the Layer-2 "getting players into
one another's worlds" problem from [ARCHITECTURE.md](ARCHITECTURE.md), planned out: what's possible
today, what's reverse-engineering-gated, the incremental build order, and the decided approach for
talking to Steam.

> **Status: rungs 1-2 shipped (rung 2 pending two-player rig verification); rungs 3-4 are design +
> research.** The mod loads, configures, observes a session, reads our own SteamID (rung 1,
> `coop/steam.rs`), and now stands up a private Steam P2P **side-channel** to a manually-entered
> partner (rung 2, `coop/coop.rs` + `coop/steam.rs` networking) — running the host-tested
> `Peer`/`Session` for real (handshake, host config push, liveness, client→host log forwarding). It
> does **not** yet put players in one another's *world* — that's the game-session RE (rung 3). This
> doc is the spec for the rest, written for session handoff. Everything game-internal is grounded in
> the pinned `fromsoftware-rs` SDK or flagged as inference to confirm on the rig (per
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

Each rung is independently testable and de-risks the next. Rungs 1-2 need no game-session RE; rung 3
is the one genuinely hard step; rung 4 is deferred.

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
  ID in production"), over poll-based `ISteamNetworkingMessages` to a **manually-entered** peer
  SteamID (`[coop] peer_steam_id` + `is_host`, swapped out of band via rung 1's copy button).
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
  SessionRequest callback (we proactively `AcceptSessionWithUser`), and that the `coop` line in the
  diag report goes `linking → linked`.
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
- Net surface for rungs 1-3 is ~10-15 flat functions, e.g. (accessor names are versioned — resolve by
  exact exported name, re-resolve after a Steam client update). The names below were dumped from ELDEN
  RING's own `steam_api64.dll` on 2026-06-25 (`x86_64-w64-mingw32-objdump -p … | grep SteamAPI_…`):
  - identity (rung 1, **confirmed live**): `SteamAPI_SteamUser_v021` accessor (rung 1 probes a
    descending version window so a bump self-heals) + `SteamAPI_ISteamUser_GetSteamID` (unversioned).
  - networking (rung 2, **now bound + called** in `coop/steam.rs`): `SteamAPI_SteamNetworkingMessages_SteamAPI_v002`
    accessor + `SteamAPI_ISteamNetworkingMessages_SendMessageToUser` / `_ReceiveMessagesOnChannel` /
    `_AcceptSessionWithUser`. (`_CloseSessionWithUser` is present in the dump but left unbound — there's
    no session teardown in rung 2; the channel lives for the process.)
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
`Transport`, and the cdylib had **no live transport** in real co-op. **Rung 2 provides it.** With a
partner configured (`[coop] peer_steam_id`) and `forward_to_host` on, a client's
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
- **Lobby callbacks (rung 4).** When we need lobbies, we either accept manual-dispatch on a dedicated
  pipe, prototype/validate it in the harness with the crate, or keep manual SteamID exchange.

## Concrete next step

Rungs 1-2 are shipped (`coop/steam.rs` + `coop/coop.rs` + `coop/forward.rs`). The immediate next
action is a **two-player rig run** of rung 2: on two machines, set each other's `[coop] peer_steam_id`
(one with `is_host = true`), launch both, and confirm the side-channel links — the `coop` line in the
diag report should go `linking → linked`, an overlay "Co-op partner connected" toast should fire, the
client should adopt the host's config, and (with `forward_to_host`) the host's `LogBundle` should pick
up the client's lines. Watch the NAT/auth open question (the peers may need to be Steam friends).

Once that's confirmed, build **rung 3** — drive the *game's* session (`CSSessionManager` →
`Host`/`Client`) so players see each other in-world. Rung 2 gives us the coordination channel ("both
call join now") and two instances we control to RE the create/join functions against.

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
