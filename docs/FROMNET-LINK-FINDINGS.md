# FromNet ↔ DLNW3D Link — RE Findings (worker:fromnet-link)

Worker lane `fromnet-link`. Goal: pin the **injection point** for the ERSC-faithful connection —
where a matchmaking "connect to peer X" response causes the game to stand up its own DLNW3D P2P
transport — and map the summon/join wire protocol so we know **what** to inject to substitute our
password-peer for the FromSoft server.

> **Scope & legitimacy.** Static RE on our own legitimately-owned **2026-06-02 `eldenring.exe`**
> (image base `0x140000000`; the main exe isn't relocated, so static VA == live VA, as prior lanes
> confirmed on the rig) + reading vswarte's public MIT `waygate-server` for the **protocol shape**.
> We study how a game we bought and its matchmaking wire *behave* so we can reimplement co-op
> behavior in clean Rust, co-op-only and *outside* anti-cheat. Behavioral notes are in my own words;
> **no decompiler/disassembler output is reproduced** and **no waygate code is copied** (CLAUDE.md >
> Clean-room hygiene). Addresses are facts about the binary.

## TL;DR

The connection is brokered by a request/notification pair over **FromNet** (FromSoft's online
protocol) and executed by a lower **DLNW3D** Steam-P2P transport. Charted end to end:

```
 REQUEST leg (this client asks to be matched):
   item/sign use → SosSign*Job::Execute → FromNet request dispatcher send   [COOP-FLOW-FINDINGS.md]
      (summon: [dispatcher]→[+0xb0]→[sender+0x38] @ 0x140a528b8, payload RequestSummonSignParams)

 RESPONSE/ACK leg (server confirms the match; the connect itself is upstream + runtime-bound):
   FromNet push → CSMultiplayNotifyJoinJob::Execute (0x140a24d80, vtable 0x142b3c2a8)   ◀ NEW (this lane)
      + the NotifyLog@U*ResultLogParams@FromNet result-notification family
      → checks CSSessionManager lobby_state ∈ {Host=3, Client=6}, builds a role-tagged ack, re-dispatches
      (this job is the ack/result-log leg; it presupposes a formed session, it does NOT bind the peer)

 TRANSPORT leg (the connect that the response triggers — DLNW3D, ISteamNetworking006):
   SteamServiceImpl factory 0x142638b40 → connect thunk 0x14263b720 → connection-creator 0x142640560
      → SteamConnection ctor 0x142643b50 → Accept setup 0x14263ffe0 (AcceptP2PSessionWithUser)
      → register thunk 0x14263b7c0 → SteamConnection lands at [container+0x708]
   The SteamConnection carries {iface=[+0x8], peerSteamID64=[+0x128]}; send reads exactly those.
```

**The peer identity that seeds the transport is a Steam ID64 at `SteamConnection+0x128`.** In
waygate's protocol that same value is the push field `summoning_player_external_id` (a hex SteamID64);
the connect's other datum is an opaque `join_data` blob. **The P2P *socket* is addressed by CSteamID
alone** — statically proven: the transport is the *legacy* `ISteamNetworking006` API and send reads
only `{iface=[+0x8], peer=[+0x128]}` (Steam's own relay handles NAT/rendezvous). So opening the socket
needs just the peer SteamID64, which we already have (rung-4 lobby discovery). **Whether `join_data`
additionally carries game-internal, post-connect handshake state that the receiving client consumes
*above* the socket is UNPROVEN** — waygate treats it as opaque and its semantics live in the game's
netcode, outside the transport leg charted here. That is a rig question (two-machine), not a settled
fact; the ERSC substitution is tractable for the *socket*, with `join_data`'s role the main residual
unknown. (A usable co-op session also needs the password-derived AES session key — server-brokered in
vanilla — per SESSION-DRIVE.md §"the session AES key"; the SteamID64 seeds the connection, not the
whole session.)

**Recommended injection point:** the **transport leg** — drive the DLNW3D `SteamConnectionManager`
connect/register directly with a `SteamConnection` whose `+0x128` = our rung-4-resolved peer
SteamID64 (ERSC model, SESSION-DRIVE.md path 2). Not the FromNet send-intercept, and not the
notification-handler injection — reasons in [§4](#4-injection-strategy-task-3).

---

## 1. The link, client-side static (task 1)

### 1a. The DLNW3D standup is Arxan-walled and 0-static-caller (call-xref is a dead end)

Confirmed the transport standup entry points are all reached only through Arxan-obscured tail-jump
stubs — static call-graph cannot reach the trigger, exactly as SESSION-DRIVE.md's "runtime-resolution
wall" predicted:

| Entry point | E8 callers | rip-refs | reality |
|---|---|---|---|
| service factory `0x142638b40` | **0** | 2 (`0x142637d35`, `0x142638595`) | both refs are `jmp 0x142638b40` Arxan stubs, not calls |
| connect thunk `0x14263b720` | **0** | 2 | one is a `mov ecx,[rcx+8]; jmp` entry stub at `0x142638c24` |
| register thunk `0x14263b7c0` | **0** | 1 | `mov ecx,[rcx+8]; jmp` entry stub at `0x142638c34` |
| connection-creator `0x142640560` | 1 (`0x14263b77f`, inside the connect thunk) | — | reached only from the thunk |

The two "callers" of the factory disassemble as garbage-prologue Arxan jump thunks that tail-`jmp`
into `0x142638b40`; the connect/register thunks sit in a tiny dispatch region right past the factory
body (`0x142638c18..0x142638c40`) as `mov ecx,[rcx+8]; jmp <thunk>` stubs. So the factory + thunks are
**factory/vtable-dispatched by the online session flow** — there is no clean static caller to trace
back to a response handler. (Re-derive: the factory body itself is clean and readable — `alloc 0x18`
→ base-ctor SteamServiceImpl `0x14263b6b0` → init vmethod `[svc+8]` → adapter `0x14263b5a0` → start
vmethods → **register back into owner via `[owner_vtable+0x68](owner, service)`**. The `owner` (rcx)
is the live DLNR3D bridge the online flow supplies — the one piece static can't pin.)

### 1b. The peer SteamID64 lives at `SteamConnection+0x128`, and is bound only at runtime

Pinned the field the transport keys on, and proved it is never seeded statically:

- **`SteamConnection@DLNW3D` send (slot 0, `0x142643dd0`)** reads `rdx = [conn+0x128]` (peer) and
  `rcx = [conn+0x8]` (the ISteamNetworking iface), then calls the **SendP2PPacket** wrapper
  `0x142640b20`. So the connection is fully described by `{iface, peerSteamID64}` and **send needs only
  those two** — no session descriptor, no blob.
- **Accept setup `0x14263ffe0`** reads `[conn+0x128]` and copies it to `[conn+0x130]` before invoking
  **AcceptP2PSessionWithUser** via an inline `call [iface+0x18]` (at `0x142640072`, iface resolved
  locally through helper `0x142641a00`; the same Steam call is also wrapped standalone at `0x1426408b0`,
  which has 0 static callers). So `+0x128` is already populated by the time a connection is
  accepted/registered.
- **The SteamConnection ctor `0x142643b50` + sub-ctor `0x142642290` zero-initialize the object**
  (fields swept `+0x00..+0x138`, incl. `+0x128`) and never write a real peer value.
- **A full `.text` scan for the direct-displacement store form `mov [reg+0x128], reg64` returned ZERO
  hits** (the scan covers that one encoding — not `lea reg,[+0x128]` + indirect store, xmm/`movq`
  stores, or memset-style sweeps like the ctor's zeroing; so it rules out the common direct writer, not
  literally every possible write). Combined with the ctor zeroing `+0x128` and the Arxan wall, the peer
  SteamID is written **at runtime**, by the dynamically-resolved binding path — either an inbound
  `P2PSessionRequest_t` callback (Steam hands us `m_steamIDRemote`) or an outbound connect seeded by
  the matched-peer response. **So "which field of the response carries the peer SteamID" cannot be
  pinned on the client statically** (the write site is in the Arxan/vtable-dispatched flow); what *is*
  pinned is the **destination slot: `SteamConnection+0x128`**, and the fact that the whole transport
  keys on nothing but that `u64` + the iface.

### 1c. The FromNet RESPONSE handler family IS charted — `CSMultiplay*Notify*Job`

The client mirrors waygate's server→client **push** side with a family of **Notify jobs** (the response
counterpart to COOP-FLOW's `SosSign*Job` request family). Found by RTTI (`.?AV…@CS@@`); note the names
are `CSMultiplay…`/`CS…` — a bare `NotifyJoinJob` substring lands mid-symbol, so search the full name:

| Notify job (RTTI) | vtable | Execute (slot 3) | role |
|---|---|---|---|
| `CSMultiplayNotifyJoinJob` | `0x142b3c2a8` | **`0x140a24d80`** | **inbound JOIN push handler** |
| `CSMultiplayNotifyLeaveJob` | `0x142b3c330` | `0x140a25040` | inbound leave push |
| `CSNotifyAreaEventJob` | `0x142b3c5f0` | `0x140a262e0` | inbound area-event push |
| `BreakInAfterNotifyRecieveJob` | (RTTI `0x143ce4b28`) | — | invasion post-join push |

Plus the server-result notification family `NotifyLog@URequest*ResultLogParams@FromNet` (incl.
`RequestSummonSignResultLogParams`, `RequestBreakInResultLogParams`, `RequestJoinMultiplayLogParams`,
`RequestVisitResultLogParams`, `RequestQuickMatchResultLogParams`) and FromNet session responses
`ResponseCreateSessionParams@FromNet` / `ResponseRestoreSessionParams@FromNet`.

**What `CSMultiplayNotifyJoinJob::Execute` (`0x140a24d80`) actually does** (own words):
1. Guards on the job's type field (`[job+0x88] == 0x25`) and resolves two manager singletons
   (`0x143d65f88`→`[+0x168]`→sub-object `rbp`; `0x143d691d8`→`[+0xe8]`→id `esi`).
2. **Reads `CSSessionManager` (`G = 0x143d7a4d0`) and requires `lobby_state` (`[G+0xc]`) ∈ {Host=3,
   Client=6}** — i.e. this handler only runs *inside* an already-formed session, and it derives a
   **role flag** (`ebx = 0` if Host, `1` if Client).
3. Builds a small result struct on the stack (`[rsp+0x28..0x3c]`): a computed id
   (`0x140a60960(esi, 0x1406560d0(rdi,…))`), `esi`, `[rbp+0x68]`, the job's `[job+0x88]`, the role
   flag, and a `word [rsp+0x94]`.
4. **Re-dispatches through the FromNet context** (`r15`): `mov rax,[r15]; call [rax+0xc0]` →
   `mov r9,[rax]; call [r9+8]` with `rdx = job+0x80` (payload) and `r8 = &struct`. Same
   `[context]→vmethod→[sender+N]→send` shape as the request Execute (COOP-FLOW), just a different
   sender slot.

So the JOIN-notify job is the **acknowledge / result-log leg** of the response — it confirms a join
*within a session*, it does **not** itself stand up the transport. The actual peer→`+0x128` binding and
`SteamConnectionManager` connect happen upstream in the dynamic DLNW3D flow (§1a/§1b). This is the
honest static ceiling: the response *handler family* is named and its ack leg is decoded, but the
connect it presupposes is runtime-bound.

---

## 2. The protocol, from waygate (task 2)

Read `waygate-server` (vswarte's MIT ER matchmaking-server reimplementation) for the **wire behavior**;
described here in my own words, no code copied. waygate speaks a modern WebSocket framing, but the
**logical message set matches the client's FromNet types 1:1** (same request/response/push names), so
it is a faithful map of what each FromNet message carries.

### 2a. The summon REQUEST (client → server)

- **`RequestCreateSignParams`** (placed by the would-be guest to advertise a sign): visibility area,
  a `MatchingParameters` filter block (regulation version, level, NG+, **password**, vow, weapon/ash
  reinforce caps, cross-region), a sign-type word, an **opaque sign blob** the game produces, and
  co-op **group passwords**. Server returns a minted `ObjectIdentifier` (an `i64` sign handle).
- **`RequestGetSignListParams`** (the host searching): a list of already-known sign identifiers, the
  search areas, and the same `MatchingParameters` filter.
- **`RequestSummonSignParams`** (the host taps a sign to summon that phantom — the real "summon"):
  the target's **server player id** (`i32`), the sign's `ObjectIdentifier`, and **`data: Vec<u8>` =
  the summoning host's own connection blob** (the game produces it; forwarded verbatim to the guest).

### 2b. The summon/join RESPONSE — what triggers the P2P connect ★

**The synchronous response to `RequestSummonSign` is empty.** The peer info instead arrives as an
**unsolicited server Push** to the *other* player (the sign owner / guest), over their persistent push
channel: `Push → Join → SummonSign`. Its payload (`SummonSignParams`, which exists as a client FromNet
type too — a plain type-name string `FromNet::…SummonSignParams` at `0x143087058`, a reflection /
name-table entry, not an MSVC RTTI descriptor) carries:

- `summoning_player_id: i32` — the host's server player id.
- **`summoning_player_external_id: String` — the host's SteamID64 (hex-encoded).** ← peer identity.
- `summonee_player_id: i32` — echoes the guest's own id.
- `sign_identifier: ObjectIdentifier` — the tapped sign.
- **`join_data: Vec<u8>` — the host's connection blob, copied verbatim from the summon request.** ←
  peer connection descriptor.

So **"connect to peer X" is a `Push→Join→SummonSign`**, and its two load-bearing fields are the peer's
**SteamID64** and the opaque **`join_data`**. The guest receives this push and initiates P2P **to the
host**. waygate treats `join_data` as pure bytes — it never builds or inspects it; the semantics live
entirely in the game's Steam netcode. The identical shape recurs in every join variant (break-in
`AllowBreakInTarget`, quick-match `AcceptQuickMatch`, visit) — always `<peer>_external_id` +
`join_data`. That consistency is strong evidence the whole peer-substitution problem reduces to
**controlling a peer SteamID64 + the connect payload**.

### 2c. The sign-list flow

`GetSignList` → `ResponseGetSignListParams` with a `known_signs` echo and a list of entries, each:
phantom `player_id`, sign `ObjectIdentifier`, area, the phantom's opaque sign `data`, **the phantom's
`external_id` (hex SteamID64)**, and group passwords. The host picks an entry, then sends
`RequestSummonSign{ player_id, identifier, data = host's own join blob }`; the server maps `identifier`
back to the pooled sign (which holds the phantom's push channel + SteamID) and pushes `SummonSign` to
the phantom (§2b). Note the asymmetry: the sign list exposes the *phantom's* SteamID to the host, but
the connect blob that actually flows is the *host's* (`data`→`join_data`), pushed to the phantom.

### 2d. Session / key material

The summon flow carries **no Steam lobby id, no session key, no host session descriptor** — the only
"how to connect" datum is the opaque `join_data`. Session encryption is established at the
client↔server transport handshake (curve key exchange → `SessionData{identifier, cookie, validity}`),
which secures the RPC channel and is **unrelated** to the P2P peer link. The P2P link between the two
game clients is negotiated by the games themselves over Steam networking, seeded by `join_data`.
waygate is pure **rendezvous** — it never sees the P2P socket, its encryption, or NAT traversal. **This
matches SESSION-DRIVE.md's finding that the vanilla session AES key is server-brokered and must be
re-derived from the shared password clean-room.**

### 2e. Handshake ordering (who connects to whom)

Guest = placed the sign; Host = tapped it. (1) Guest `CreateSign` → server pools it with the guest's
SteamID + a push channel. (2) Host `GetSignList` → sees entries. (3) Host `RequestSummonSign{data =
host blob}` → empty ack + server **pushes `SummonSign` to the guest** with **host SteamID + host
join_data**. (4) **The guest (summoned side) receives the push and drives the P2P connect toward the
host** using host SteamID + join_data. (5) Reject path pushes back to the host. **Net: the summoning
host triggers it, but the summoned guest is the side that receives the peer info and opens the P2P
link, connecting to the host's SteamID.**

---

## 3. Correlating the two sides — what a synthetic response must contain

| waygate push field (server→client) | client-side meaning | how we supply it in the ERSC model |
|---|---|---|
| `summoning_player_external_id` (hex SteamID64) | the peer to connect to → `SteamConnection+0x128` | **rung-4 lobby discovery already resolves the peer SteamID64** |
| `join_data` (opaque blob) | Steam P2P rendezvous and/or game-internal post-connect state | **socket doesn't need it** — legacy `ISteamNetworking006` opens by CSteamID alone (Steam relay does NAT); whether the game consumes it *above* the socket is **UNPROVEN** (rig question) |
| `summoning_player_id` / `summonee_player_id` (i32) | server-session row ids | session-scoped, not a stable identity; irrelevant offline |
| `sign_identifier` (ObjectIdentifier) | which sign | bookkeeping; not connect-critical |

**The crux:** opening the P2P *socket* to `<our password-peer>` needs, functionally, **just the peer's
SteamID64** — statically proven, because ER's transport is the legacy P2P API keyed on CSteamID, and
that value we already have. What's *not* proven is that the socket is the whole story: `join_data` is
opaque in waygate and may carry game-internal handshake state the receiving client consumes after the
socket opens, and a usable co-op session additionally needs the password-derived AES key (§2d) and the
`CSSessionManager` FSM engaged above the socket (§4C gap). So the substitution collapses the
*rendezvous* problem to "feed a SteamID64 into the SteamConnectionManager," but the *session* problem
(join_data's role, the AES key, the FSM standup) is the residual rig work — don't read "just the
SteamID64" as "co-op for free."

---

## 4. Injection strategy (task 3)

Three candidate injection points, with the honest trade-offs:

**(A) Intercept the FromNet dispatcher SEND and fake a local response.** Hook the request send
(`[dispatcher]→[+0xb0]→[sender+0x38]` @ `0x140a528b8` for summon; dispatcher singleton `0x143d87350`,
accessor `0x1407a72a0`) and, instead of reaching the server, synthesize a local `Push→Join→SummonSign`
and feed it to the notification machinery.
*Cost:* you must fabricate the entire notification-delivery path **and** the opaque `join_data`, **and**
the transport still has to be stood up afterward (the notify job presupposes a live session and a bound
connection). High surface, and it depends on the game's own DLNW3D flow running — which is **dormant
offline** (SESSION-DRIVE.md rig-proved 0 live DLNW3D objects). Only viable if paired with cracking the
flow-entry/online-availability signal (the open-ended hunt that has beaten every static pass).

**(B) Hook the RESPONSE handler and inject a peer.** Hook `CSMultiplayNotifyJoinJob::Execute`
(`0x140a24d80`) / the notification-receive path and substitute our peer.
*Cost:* this job is the **ack/result-log leg**, not the connect — it already **requires `lobby_state ∈
{Host, Client}`** (a session must exist) and it re-dispatches a log, it doesn't bind `+0x128`. The
connect it presupposes is upstream and runtime-bound. Injecting here changes *bookkeeping*, not *who
connects*. Wrong layer.

**(C) Drive the DLNW3D transport standup directly with the peer SteamID64 — RECOMMENDED.** The
ERSC-faithful path (SESSION-DRIVE.md path 2). Stand up / drive the game's own `SteamConnectionManager@
DLNW3D` for the rung-4-resolved peers so a real `SteamConnection` with `+0x128 = peer SteamID64` lands
at `[container+0x708]`:

Two distinct DLNW3D objects are involved — keep them straight: the **`SteamServiceImpl`** (the service,
vtable `0x143277270`, made by the factory) and a **`SteamConnectionManager`** (a `0x1b8`-byte object the
connect thunk allocates internally via ctor `0x14263f700`). The thunks take the *service* as their
first arg; the connection-creator `0x142640560` operates on the *manager* it just built.

```
resolve iface: 0x142640b90  (SteamInternal_FindOrCreateUserInterface "SteamNetworking006"),
               holder 0x143c602b0  → ISteamNetworking006
stand up svc:  factory 0x142638b40(owner)  → SteamServiceImpl (vtable 0x143277270)
   ── owner = the live DLNR3D bridge; its vtable+0x68 accepts the service.
      Offline the service is dormant, so `owner` must be captured at runtime (two-machine) or
      the service built ourselves — the one non-static piece.
connect:       thunk 0x14263b720(service, params)
                  → allocs SteamConnectionManager (0x1b8, ctor 0x14263f700) from [service+8]
                  → connection-creator 0x142640560(manager, params)
                     → SteamConnection ctor 0x142643b50   (params = buffer sizes; +0x5c ring 0x4b0)
bind peer:     write peer SteamID64 → SteamConnection+0x128   (rung-4 lobby owner / roster)
accept+wire:   Accept setup 0x14263ffe0  → AcceptP2PSessionWithUser via [iface+0x18]
                  (also wrapped standalone at 0x1426408b0)
register:      thunk 0x14263b7c0(service, connection)  → hooks it into the service collection
result:        SteamConnection at [container+0x708]; send via 0x142643dd0 → SendP2PPacket 0x142640b20
```

*Why it's the clean pick:*
- It matches ERSC's actual model — *skip the matchmaker, keep the P2P* — and it bypasses the FromNet
  broker **and** the un-forgeable `join_data` entirely, because the legacy `ISteamNetworking006` API is
  addressed by **CSteamID alone** (§1b/§3).
- Every *transport-leg* entry point is charted (factory, connect/register thunks, connection-creator,
  ctor, Accept setup, the `+0x128` peer slot).
- It does **not** depend on cracking the offline-availability signal for the *transport* — it stands
  the socket up itself, which is the ERSC premise (skip the matchmaker, keep the P2P).

*The layer this does NOT yet chart (the honest gap):* standing up a raw `SteamConnection` opens the
*socket*, but co-op behavior — roster, join acks, and the game's net sync — runs off the **DLNR3D
session layer above it** (`CSSessionManager` in `Host`/`Client`, `protocol_state → Ingame`), and §1c
shows the JOIN-notify handler itself *requires* `lobby_state ∈ {Host, Client}`. SESSION-DRIVE.md proved
the `CSSessionManager` create path fails offline (leg-B tail: slot-array cap 0, finalize handle). So
"drive the transport" is **necessary but not obviously sufficient**: how a hand-built `SteamConnection`
at `[container+0x708]` drives `CSSessionManager` up to `Host`/`Client` and kicks off net sync is an
**open question** this lane does not answer — it's the seam between the charted transport leg and the
session FSM, and belongs on the two-machine rig agenda alongside the password-derived AES key.

*The one open piece* (same as SESSION-DRIVE.md path 2): the DLNW3D service is dormant offline, and the
factory needs a live DLNR3D `owner`/config object. Resolve that at runtime — either capture the
`owner` when the transport comes up on a real two-machine session, or construct the
`SteamServiceImpl` + `SteamConnectionManager` ourselves (bounded: the surface is finite and mapped
here). Validation is inherently **two-machine** (rig host + Steam Deck joiner) — a `SteamConnection` is
peer-to-peer, so it only exercises once a real peer sends the first P2P packet.

**Recommendation in one line:** inject at the **transport leg (C)** — drive `SteamConnectionManager@
DLNW3D` connect/register with a `SteamConnection` whose `+0x128` is the rung-4 peer SteamID64; do **not**
intercept the FromNet send (A) or the notify handler (B). The socket is addressed by CSteamID alone
(so the un-forgeable `join_data` isn't needed to *open* it), and the transport is dormant until
*something* stands it up — so stand it up ourselves rather than coax the gated online flow. The
residual unknowns (join_data's role above the socket, the password-derived AES key, and how the
`CSSessionManager` FSM engages above a hand-built connection) are two-machine rig work, not blockers to
starting the transport-leg build.

---

## 5. Cross-references

- **COOP-FLOW-FINDINGS.md** — the REQUEST leg: item-use → `SosSign*Job::Execute` → FromNet request
  dispatcher send. This lane adds the **RESPONSE leg** (the `CSMultiplay*Notify*Job` family) and the
  **transport leg** it feeds.
- **SESSION-DRIVE.md** — "TRANSPORT CHARTED" + "DLNW3D service standup chain" + the RIG-PROVEN dormancy
  scan. This lane confirms its 0-static-caller wall from the *caller* side (Arxan jump stubs), pins the
  peer slot `+0x128` has no static writer, and adds the FromNet response-handler family + the
  waygate protocol correlation that makes path 2 concrete.
- **`waygate-server`** (`/home/michael/Code/waygate-server`) — clean-room protocol reference: the
  `Push→Join→SummonSign` shape and its `external_id` + `join_data` fields; break-in / quick-match /
  visit share it.
- **COOP-CONNECTION.md** rung 3 — "reuse the game's transport, replace the peer-brokering." This lane
  pins "the peer-brokering" to a concrete surface (FromNet request dispatcher `0x143d87350` up; the
  `CSMultiplay*Notify*Job` family + DLNW3D `SteamConnectionManager` down) and pins "the peer" to
  `SteamConnection+0x128`.

## 6. Re-derivation after a game update

- **The Notify-job family:** RTTI-search full names `.?AVCSMultiplayNotifyJoinJob@CS@@` etc. (a bare
  `NotifyJoinJob` substring lands mid-symbol — the real prefix is `CSMultiplay…`/`CS…`); slot `[3]` of
  each vtable is its Execute. The JOIN handler's Execute reads `CSSessionManager` `[G+0xc]` and
  branches on `==3`/`==6`.
- **The peer slot:** in `SteamConnection@DLNW3D` send (RTTI `.?AVSteamConnection@DLNW3D@@`, vtable slot
  0), the `mov rdx,[conn+0x128]; mov rcx,[conn+8]; call <SendP2PPacket wrapper>` sequence — `+0x128`
  is the peer SteamID64, `+0x8` the iface.
- **The transport standup:** service factory = the fn that base-ctors `SteamServiceImpl` and registers
  it via `[owner_vtable+0x68]`; connect/register thunks = the two `mov ecx,[rcx+8]; jmp` stubs right
  past the factory body; connection-creator = the manager method that copies a buffer-size params
  struct into `[mgr+0x40..0x70]`, allocs ring buffers, and constructs a `SteamConnection` that calls
  `AcceptP2PSessionWithUser`. All are 0-static-caller / Arxan-dispatched — trace *down* from the
  factory, never *up* by call-xref.
- **The transport primitive:** `ISteamNetworking006` (the only `SteamNetworking0NN` string in the
  image, at `0x143277fd0`); resolver `0x142640b90`, holder `0x143c602b0`; wrappers SendP2PPacket
  `0x142640b20`, ReadP2PPacket `0x142640bc0`, AcceptP2PSessionWithUser `0x1426408b0`,
  CloseP2PChannelWithUser `0x142641150`.

## Tooling / method notes

`scripts/re/static.py` (`calls`/`xref`/`vtable`/`fn`/`ascii`) + ad-hoc capstone scans over the clean PE
(`/tmp/fromnet-*.py`). Two reusable gotchas:
- **RTTI substring offset (recurring):** `ascii "NotifyJoinJob"` matches *inside*
  `.?AVCSMultiplayNotifyJoinJob@CS@@` — the returned VA is mid-name and `name−0x10` misses the type
  descriptor entirely (COL search returns empty). Walk back to the preceding NUL / the `.?AV` start
  first; the class prefix here is `CSMultiplay…`, not a bare `Notify…`.
- **"caller" that is really a jump thunk:** an `xref`/rip-ref hit inside a function with **no `.pdata`
  entry** is almost always an Arxan tail-jump stub (garbage-decoding prologue, ends in `jmp <target>`),
  not a real call site — disassemble it before believing it's a caller.
