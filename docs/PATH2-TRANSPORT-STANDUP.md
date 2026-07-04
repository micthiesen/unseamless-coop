# Path 2 — Stand up our own DLNW3D Steam P2P transport (the ERSC model)

> **Scope doc for the next session.** Written 2026-07-04 after the native-builder path was proven a dead
> end for reaching `Host` (offline *and* two-machine — see [SESSION-DRIVE.md](SESSION-DRIVE.md) >
> "NATIVE-BUILD TRACE (2026-07-04)" and "TWO-MACHINE RESULT"). This is the recommended replacement track.

## The one-sentence goal

Make a **real, game-native `SteamConnection@DLNW3D`** land at `[container+0x708]` — built by driving the
game's *own* transport constructors ourselves, fed the peer's SteamID64 we already resolve via rung 4 —
so the session FSM advances `TryToCreateSession → Host` and the game's world netcode runs over it. We are
**not** repurposing our rung-2 side-channel socket as that transport; we build a new game-native one and
keep rung-2 as our control channel (config sync, presence, version).

## Why this and not the native builder

The game's establish handler `0x1423f2820` reaches its builder, but the builder's `SteamServiceImpl`
standup `0x142638b40` returns **null** — because the DLNW3D transport is gated on the game's **own
online-session flow** (the EAC/matchmaker path we bypass by construction). A peer over our private
side-channel does not put the game into that flow. So we stop asking the game to build it and build it
ourselves the way ERSC does — outside the matchmaker entirely.

## The make-or-break first milestone (do this before anything else)

**Can the DLNW3D `SteamServiceImpl` stand up at all outside the online flow?** `0x142638b40` returned null
even when the *game* drove it, so the first question is whether *we* can make it succeed by supplying what
the online flow normally sets up. Concretely:

1. **Resolve `ISteamNetworking006` ourselves.** Its holder `0x143c602b0` reads null offline
   (`SteamInternal_ContextInit` for the P2P interface never ran). Drive the resolver `0x142640b90`
   (`SteamInternal_FindOrCreateUserInterface("SteamNetworking006")`) to populate it, and confirm the holder
   now holds a live iface pointer. **This alone may be the gate** — the service init likely fails only
   because the iface is unresolved.
2. **Re-check the service standup.** With the iface resolved, drive `0x142638b40(owner)` again (or re-drive
   the establish handler) and see if it now returns non-null. If yes → the rest of the chain is mechanical.
   If no → chart what else the service init `0x14263b820` reads from the `owner`/config that's still unset,
   and capture the real `owner` at runtime (it's a live DLNR3D bridge object; the factory's `rcx` =
   `[obj+0x40]` = `[container+0x48]` in the native path).

Gate the whole effort on this: if the service can't stand up outside the online flow even with the iface
resolved and a real owner, that's the true wall and needs its own decision (ERSC-style session
neutralization, or a deeper look at what the online flow primes).

## The build, once the service stands up (all addresses charted — see SESSION-DRIVE.md)

The standup chain and the transport surface are already reverse-engineered; this is assembly, not new RE:

- **Service:** factory `0x142638b40` → `SteamServiceImpl` (vtable `0x143277270`, sub-ctor `0x14263f1e0`),
  init vmethod `0x14263b820`, adapter `0x14263b5a0`, registered into the owner via `[owner_vtable+0x68]`.
- **Manager:** `SteamConnectionManager` (vtable `0x143278020`) — created by the online flow; instantiate
  it off the service.
- **Connect / accept:** listen/connect `0x14263b7c0` / `0x14263b720` → connection-creator `0x142640560`
  (`rdx` = params: buffer sizes, `+0x5c` ring `0x4b0`) → `SteamConnection` ctor `0x142643b50` + per-connection
  Accept setup `0x14263ffe0` (registers callbacks, `AcceptP2PSessionWithUser`).
- **Peer identity:** the resolved partner SteamID64 (rung 4; `p2p_test_peer_a/b` in the seed config are the
  two machines' IDs) goes at `conn+0x128`.
- **Land it:** wrap the built `SteamConnection` in a `SocketManagerHolder@DLNR3D` (ctor `0x1423f7180`, the
  `land_socket_holder` lever already does this) and store at `[container+0x708]` + addref. Then drive create
  and watch `lobby_state → Host`.
- **Callbacks:** register `P2PSessionRequest_t` / `P2PSessionConnectFail_t` (`CCallback<…>`) so incoming P2P
  sessions from the peer are accepted — this is what a lone host's `+0x708` needs before a real peer packet
  exercises it.

The transport read/write wrappers (`SendP2PPacket` `0x142640b60`, `ReadP2PPacket` `0x142640bc0`, etc., all
via holder `0x143c602b0`) are charted in SESSION-DRIVE.md > "TRANSPORT CHARTED".

## Milestones (suggested order)

1. **Iface resolve + service standup** (the make-or-break above). Deliverable: `0x142638b40` returns a real
   service offline. A read-only probe first (does resolving the iface flip the standup?), then the driver.
2. **Manager + connection** — build a `SteamConnection` via the creator/ctor/Accept path, verify its vtable
   (`0x143278370`) and that its sub-objects are wired (not the whack-a-mole hand-build — use the game's ctors).
3. **Land at `+0x708` + drive create** solo — expect `TryToCreateSession → Host` if the connection is real
   (a lone host's self/listen connection is valid until a peer packet arrives).
4. **Two-machine** (rig + Deck) — register the P2P callbacks, use the Deck's resolved SteamID as the peer,
   prove a real join over the connection. Then the seamless teardown gate
   ([SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md)).

## Risks / open questions

- **The service may still refuse to stand up outside the online flow** even with the iface resolved. That's
  milestone 1's job to answer; don't build 2–4 until it's green.
- **The `owner`/config** the factory needs is a live game object; parts may only exist mid-online-flow. Be
  ready to capture it at runtime and synthesize a minimal stand-in (function-boundary hooks only — a
  mid-function jmp-back hook in the deep transport code faulted this session; see the reverse-engineer skill).
- **Reuse of the game's own connection vs. a fully hand-rolled Steam P2P** (ERSC-pure) is a fork: if driving
  the game's ctors keeps hitting online-flow gates, the fallback is ERSC's fully-independent transport
  (our own send/recv over `ISteamNetworking006`, feeding the game only the minimal session state it checks).

## Where the rig config stands

`drive_session_established` must stay **off** (the gate2 double-drive fix). The native-builder probes
(`drive_establish_handler`, `land_socket_holder`, etc.) are charted levers to reuse. The two-machine role
footgun (cycle re-applies the shared seed) is being fixed on `worker/rig-config-footgun` — integrate that
before the next two-machine run so host/join roles are explicit per-machine flags.
