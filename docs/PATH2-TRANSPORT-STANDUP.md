# Rung 3 finish — the ACTIVATION SEAM (wire our stood-up connection into the session FSM)

> **UPDATE 2026-07-04 pm — substantial progress; two claims below are now CORRECTED.** (a) The object at
> `[container+0x708]` is a `SocketManagerHolder` whose `+0x10` holds a 0x10-byte **socket-manager wrapper**
> (`{vtable 0x143276a00, [+8]=MTInternalThreadSteamSocketManager}`), **not** a raw `SteamConnection` — landing a
> connection there is the type error that caused the first host-setup fault. (b) The `SteamServiceImpl` standup
> `0x142638b40` **WORKS OFFLINE** (driving the socket-manager init stood up a real service; the "standup null →
> native-builder dead end" was a misdiagnosis). Host-setup faults #1–#3 are cleared; the live wall is the
> socket-manager's **worker thread** doing Steam context init. Read [SESSION-DRIVE.md](SESSION-DRIVE.md) >
> **"HOST-SETUP DRIVE (2026-07-04 pm)"** for the full chain + next steps; the rest of this file predates it.
>
> **Scope doc (original).** Written 2026-07-04. Supersedes an earlier draft of this file that
> framed the work as "stand up our own transport" — that turned out to be already done. The real remaining
> gap is the **seam**: making the game's session FSM *activate* the connection. Full RE state now in
> "HOST-SETUP DRIVE (2026-07-04 pm)".

## The one-sentence goal

Get a `SteamConnection@DLNW3D` at `[container+0x708]` into a state the session FSM will drive to `Host`, so
players share a world. We already build a working connection and land it there (reaching
`TryToCreateSession`); the finish is making the game's host-setup **activate** it instead of faulting on it.

## What is already DONE (don't rebuild these)

- **Transport standup works** (`[debug.probes] stand_up_transport`): resolve `ISteamNetworking006`
  (`0x142640b90` → global holder `0x143c602b0`), construct `SteamServiceImpl` (vtable `0x143277270`) +
  `SteamConnectionManager` + `SteamConnection` off the game heap, all rig-confirmed live.
- **Legacy P2P works two-machine** (rig + Deck): both machines drove the game's own `ISteamNetworking006`
  `Accept`/`Send`/`Read` at each other's SteamID64 and exchanged packets bidirectionally, no matchmaker.
- **Landing at `+0x708` works** (`land_socket_holder`): wrap the stood-up connection in a
  `SocketManagerHolder@DLNR3D` (ctor `0x1423f7180`) and store at `[container+0x708]` → the driven create
  clears its `ConnectionRefInfo` addref and reaches `lobby_state = TryToCreateSession`.

## What is RULED OUT (don't retry these)

- **Making the game build the connection (native builder).** Driving the establish handler `0x1423f2820`
  reaches the game's own builder (`[live vtable 0x1431f8780 +0x80] = 0x1423f46b0`, a plain fn), but its
  `SteamServiceImpl` standup `0x142638b40` returns **null** — offline, **two-machine with a linked peer**, and
  **with `ISteamNetworking006` already resolved**. The standup fails on its `owner`/config (`[container+0x48]`),
  which is only valid inside the game's own online-session flow (EAC/matchmaker) that we bypass. Dead end.
- **Forcing the FSM to `Host` directly** (`0x140cb2ae0`): writes `Host` but the host-setup body faults on the
  connection and the game resets `lobby_state` to `None`. It doesn't stick.
- **Hand-building the connection field-by-field:** whack-a-mole; sub-objects are construction-time-wired. Use
  the game's own ctors (the `stand_up_transport` path) instead.

## The remaining gap: ACTIVATION

Our stood-up connection does real P2P, but the **session host-setup** path expects a connection that a full
game session-establish would have wired — and faults on sub-objects ours doesn't populate. Two symptoms of the
same gap: (a) `land_socket_holder` → `TryToCreateSession` then the session-update task faults advancing to
`Host`; (b) forcing `0x140cb2ae0` faults in host-setup and resets to `None`.

**The job: chart exactly what the host-setup path touches/derefs on the `SteamConnection`, then wire those on
our stood-up connection** so the FSM drives it to `Host` without faulting.

## Milestones (suggested order)

1. **Chart the fault.** With `stand_up_transport` + `land_socket_holder` on (connection landed at `+0x708`),
   drive create to `TryToCreateSession`, then let the session-update task advance (or drive `0x140cb2ae0`) and
   **capture the exact faulting instruction + which connection sub-object/offset it derefs** (the `crashdump`
   handler already logs the faulting module+offset; use a `--diag` build + `addr2line`). Repeat to peel each
   successive missing field — one crash at a time — building a list of "what host-setup needs on the connection."
   ⚠ Hook only at **function boundaries** — a mid-function jmp-back hook in this deep transport code faulted
   this session (see the reverse-engineer skill).
2. **Wire the missing sub-objects** on the stood-up connection (via the game's own ctors/setup fns where
   possible — `0x142643b50` ctor, Accept setup `0x14263ffe0`, the `+0x120` iface-holder, `+0x128` peer
   SteamID), until host-setup runs clean.
3. **Solo `→ Host`.** Once host-setup no longer faults, the driven create should advance
   `TryToCreateSession → Host` (a lone host's self/listen connection is valid until a peer packet arrives).
4. **Two-machine (rig + Deck):** joiner reaches `Client` over the connection, players see each other in-world.
   Then the seamless teardown gate ([SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md)).

## Risks / open questions

- **The host-setup may deref something only the online session-establish produces** (a server-issued token,
  a session key, a broker blob). If so, chart whether it's derivable from the password (the session AES key is)
  or genuinely server-sourced. Legacy P2P is addressed by SteamID alone (no broker blob needed for transport),
  but the *session* layer may want more — this is the key unknown to resolve early in milestone 1.
- **Fallback:** if activating a game-native connection proves to need online-only session state, the ERSC-pure
  route is to neutralize the game's session dependency and carry world sync over our own transport — a larger
  change, only if milestone 1 shows the host-setup is truly online-gated.

## Rig config for this work

`drive_session_established` **off** (the gate2 double-drive fix). Turn `stand_up_transport` + `land_socket_holder`
+ `drive_create` **on** (that's the landed-connection path); `drive_establish_handler` **off** (native builder
ruled out). Integrate `worker/rig-config-footgun` first so host/join roles are explicit per-machine flags before
the next two-machine run.
