# Driving a Session Directly (rung-3 call spec)

What it takes to **drive a `CSSessionManager` session directly** — so that the moment the create/join
initiation functions are charted (the rung-3 RE in [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) /
[SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md)), we already know exactly **how to call them**: the
arguments each takes, the game state that must hold first, the keys/identity to feed in, and the
ordering against the rung-2 side-channel and the rung-4 Steam lobby.

This is desk/static research — **no rig run**. It's the spec a future driven session implements
against, not a record of a confirmed call.

> **Scope.** This answers *"once we have the function address, how do we drive it?"* It does **not**
> find the address — that's the runtime write-watch in SESSION-RE-RUNBOOK.md. The two dovetail: the RE
> runbook hands back two function entries + a register→meaning mapping; this doc says what to put in
> those registers and what must be true around the call.

## TL;DR — the headline finding (SDK survey)

**The pinned `fromsoftware-rs` SDK exposes NO callable session-create / host / join API. There is no
"free" no-RE path.** I checked specifically for one (see [SDK survey](#sdk-survey-the-highest-leverage-question)
below): the SDK charts the session **state** exhaustively — the `CSSessionManager` struct, the
`LobbyState`/`ProtocolState` FSM enums, the roster, the player limit, the AES cipher pointers, and the
`NetworkSessionVmt` transport vtable — but its **entire callable-function surface** (the `RvaBundle`,
~93 charted RVAs) contains **zero** session-initiation entries (~93 RVAs:
`apply_speffect`, `spawn_bullet`, `display_status_message`, `execute_action_button`, `cast_ray`, plus a
wall of vtable RVAs — none of them session). The create/host/join calls are
**non-virtual** functions the SDK does not name, so they can't be reached by walking a vtable either.

So driving a session still requires the rung-3 RE (two function RVAs from the write-watch). What this
doc adds is everything *else* a driver needs, so that RE is the **only** missing piece:

| To drive a session we need… | Source | Status |
|---|---|---|
| **create-session fn entry** (host: `None → TryToCreateSession → Host`) | rung-3 write-watch RE | **NOT charted** (the gap) |
| **join-session fn entry** (joiner: `None → TryToJoinSession → Client`) | rung-3 write-watch RE | **NOT charted** (the gap) |
| the `CSSessionManager*` `this` pointer | SDK singleton / global `G = 0x143d7a4d0` | **have it** |
| the peer SteamID (joiner needs host's; host accepts joiner's) | rung-4 lobby owner / roster | **have it** |
| the session **AES key** (all peers must share one) | derived from the shared password | **mechanism known, value is ours to define** |
| state preconditions (in-game, `lobby_state == None`, manager live) | SDK FSM fields, observer | **have / observable** |
| ordering (host up → joiner joins), the "go now" channel | rung-2 side-channel + rung-4 role | **have it** |

Everything but the **two function entries** is in hand. That is the precise residual RE.

## SDK survey (the highest-leverage question)

Pin `fromsoftware-rs` rev `8c67a84` (`crates/eldenring` + `crates/shared`). Asked directly: *does it
name any create/host/join call, or a `CSSessionManager` method, or a session-initiation RVA?*

**Answer: no callable initiation; rich state only.** What it does and doesn't give:

### Charted — usable the moment we have a live `CSSessionManager*`

- **The session object** — `CSSessionManager` (`cs/session_manager.rs`) as a fully-typed `#[repr(C)]`
  struct, reached via the `#[singleton("CSSessionManager")]` accessor (`FromStatic::instance()` /
  `instance_mut()`, `unsafe`, main-thread only). Named fields we'll read/write while driving:
  - `lobby_state: LobbyState` (`+0xc`) and `protocol_state: ProtocolState` (`+0x10`) — the two FSMs.
  - `players: DLVector<SessionManagerPlayerEntry>` — the roster; each entry carries `base.steam_id: u64`
    plus `is_host`, `is_local_player`, `character_event_id`, …. And `host_player`
    (`MaybeEmpty<SessionManagerPlayerEntryBase>`) — the host, carrying just the base fields
    (`steam_id`, `steam_name`); `host_player.base.steam_id` is the host SteamID a joiner feeds to Call B.
  - `session_player_limit: u32` (`+0x170`) and `session_player_limit_override: u32` (`+0x25c`) — the
    seat count (ERSC's "raise the limit" lever lives here).
  - `serial_cipher_key` / `aes_encrypter` / `aes_decrypter` (`OwnedPtr`, `+0x238/+0x240/+0x248`) — the
    **session AES cipher** (see [the AES key](#the-session-aes-key-the-one-cryptographic-input) below).
- **The FSM enums** — `LobbyState { None=0, TryToCreateSession=1, FailedToCreateSession=2, Host=3,
  TryToJoinSession=4, FailedToJoinSesion=5, Client=6, OnLeaveSession=7, FailedToLeaveSession=8 }` and
  `ProtocolState { None=0, JoinCheck=1, WaitInitData=2, …, Ingame=6, … }`. These are the **named
  targets** the create/join walk drives toward — we don't have to RE the enum values, only the writer.
- **The in-session transport** — `NetworkSessionVmt` (`cs/network_session.rs`): `broadcast_packet`,
  `receive_packet` / `receive_latest_packet`, `send_hit`, `kick`, `request_leave`, `remote_identity`.
  This is the *post-connection* API: once `lobby_state` is `Host`/`Client`, this is how packets flow.
  It is **vtable-charted** (we can call it once a session exists) — but it does **not** start a session.
- **The other multiplayer managers**, as readable state (no initiation methods): `CSNetMan`,
  `QuickmatchManager` / `CSQuickMatchingCtrl` (its own `CSQuickMatchingCtrlState` stepper for arena),
  `BreakInManager` (invasion search state), `SosSignMan` (sign DB). All struct-charted, none expose a
  "create a session" or "accept this sign" call.

### The gap — what the SDK does **not** chart

- **No create/host/join function.** The `RvaBundle` (the SDK's whole list of callable game functions —
  `apply_speffect`, `spawn_bullet`, `display_status_message`, `execute_action_button`, `cast_ray`, plus
  a wall of vtable RVAs) has **no session-initiation entry**. There is no `cs_session_manager_*` RVA at
  all. SDK-COVERAGE.md already flags this row "needs internal-function RVAs (not just struct layout)."
- **No `CSSessionManager` methods.** The struct has named fields but **zero** `impl` methods that act
  on a session. The only `impl`s anywhere near here are pure helpers on value types (`QuickMatchSettings`
  bit accessors, `SteamIdStr::to_u64`).
- **Create/join are non-virtual.** SESSION-RE-FINDINGS.md confirmed the manager's vtable is short
  (~2 slots) and does not contain the initiation calls — so a vtable walk won't reach them; only the
  write-watch's function-entry capture will.

**Conclusion to flag loudly:** there is no shortcut. The SDK turns "drive a session" into the *minimal*
RE problem — two function addresses — but it does not eliminate it. Everything around those two
addresses, this doc specifies.

## Drive requirements — the precise input list

For each of the two calls, what it needs. Register/arg names use the win64 ABI (`rcx`=`this`, then
`rdx`/`r8`/`r9`); the exact argument *meaning* per register is the thing the rung-3 write-watch + hook
confirms (`session_probe.rs` dumps `rcx/rdx/r8/r9` precisely so we can read it off a real call).

### Call A — create / host (host side)

- **`this`** = the live `CSSessionManager*` (`rcx`). Get it from the SDK singleton accessor or, for an
  RE cross-check, `[G]` where `G = 0x143d7a4d0` (the keystone global; equals the `base` the FSM probe
  prints). The observer/probe already prints this so a hooked call's `rcx` can be matched against it.
- **session parameters** — likely a settings/struct argument (player limit, password/match flags). The
  candidate registers are `rdx/r8/r9`; the hook capture tells us which. At minimum the host wants
  `session_player_limit` (and/or `session_player_limit_override`) set to the co-op seat count, which we
  can also just write to the named fields around the call.
- **the session AES key** — see below. The host establishes it; the joiner must derive the **same** key.
- **state preconditions:** `lobby_state == None` (not already in/forming a session), the player loaded
  into the world (in-game, not at a menu/loading boundary), `CSSessionManager` live (true from the
  title screen onward). Drive on a frame-ordered task (the project's standard hooking discipline), not a
  free thread.
- **effect:** walks `None → TryToCreateSession → Host`; `protocol_state` then advances toward `Ingame`.
  Solo reaches the **host/create** edge by itself (hosting initiates locally) — which is why *create*
  can be charted in a solo driven session and *join* needs a peer.

### Call B — join a peer (joiner side)

- **`this`** = the live `CSSessionManager*` (`rcx`), as above.
- **peer SteamID** = the **host's** SteamID64 (`u64`), almost certainly in `rdx`/`r8`/`r9`. We already
  have it: rung-4 lobby discovery resolves the host as the lobby owner (`GetLobbyOwner`), and it also
  appears in the host's roster entry. The hook capture pins which register carries it (and that register
  must be `peer_tag`-scrubbed in logs — a raw SteamID64 resolves to a Steam profile).
- **the session AES key** — the **same** key the host used (so the game's encrypted P2P packets between
  the two modded clients decrypt). Derived from the shared password (below).
- **state preconditions:** same as Call A (`lobby_state == None`, in-game, manager live).
- **effect:** walks `None → TryToJoinSession → Client`; `protocol_state` advances `JoinCheck →
  WaitInitData → … → Ingame`, at which point the game's own net sync (`net_chr_sync`, position/HP)
  takes over.

### The session AES key (the one cryptographic input)

Vanilla ELDEN RING establishes the per-session AES key (`serial_cipher_key` → `aes_encrypter` /
`aes_decrypter`) as part of the **server-brokered** matchmaking handshake — every player paired into a
session ends up with a common key so the P2P packets are mutually decryptable. Launched outside EAC we
have **no** FromSoft server to broker that key, so a driven session must establish the shared key some
other way. Two shapes, both "drive requirements," pick on the rig:

1. **Derive the key from the shared co-op password** and populate the cipher so every peer computes the
   *same* key from the *same* password — the same trick we already use for `lobby_discovery_token` and
   `auth_proof` ([`crypto.rs`](../crates/unseamless-core/src/crypto.rs)). This is the natural fit: the
   password is already the single pairing input, already on both machines, already authenticated by the
   rung-2 handshake. The concrete derivation (KDF, salt, key length to match the game's AES expectation)
   is **ours to define clean-room** — it must only be *deterministic and identical across peers*, like
   the existing tokens. This is what "the password derives the session AES key" in COOP-CONNECTION.md
   rung 3 means.
2. **Neutralize / replace the game's session encryption** so it doesn't depend on a server-brokered key
   at all. Heavier and riskier; only if (1) proves impractical.

Either way the requirement is the same: **a session key both peers agree on without a matchmaking
server, anchored to the password.** This is a distinct RE/clean-room sub-task from the two initiation
addresses (it's about the cipher fields at `+0x238/+0x240/+0x248`, not the FSM writer), and should be
charted on the rig alongside the create/join capture — when the create hook fires, also observe how/when
those cipher pointers get populated.

### State / online-availability gate (precondition risk to verify)

The create/join initiation function may itself **gate on an online-availability flag** before it does
anything — the same family of offline checks that greys out the in-game multiplayer items outside EAC
(the active investigation in [OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)). If it does, then
*calling the function directly is not enough* — the same gate ERSC patches must be neutralized first, or
the call early-returns to `FailedToCreateSession`/`FailedToJoinSesion` (states 2/5). **Flag for the rig:**
on the first driven call, if `lobby_state` jumps to `2`/`5` instead of `1`/`4`, an internal gate
rejected it — chase that gate (it overlaps the offline-items work). Driving *directly* may dodge the
**menu-grey** gate (that's a UI-layer check) while still hitting a **function-internal** gate; only the
rig tells us which. This is the one precondition we can't settle at the desk.

## Ordering — against rung-2 (side-channel) and rung-4 (Steam lobby)

The connection stack (rungs 1, 2, 4) already resolves identity, role, and a coordination channel before
any session call. The drive sequence:

1. **Both in-game, Steam ready.** Actions are gated on `steam_ready` + `playstate` (already enforced for
   Open/Join). `CSSessionManager` is live (from the title screen).
2. **Role + peer resolved (rung 4).** One player picks **Open World** (host), the other **Join world**
   (joiner). Lobby discovery on the shared-password token resolves the **peer SteamID** (joiner learns
   the host via `GetLobbyOwner`). Role is the user's choice, never derived — only the host creates a
   lobby, so there's no both-create race.
3. **Side-channel linked (rung 2).** The two mods complete the password-authenticated handshake over the
   private Steam P2P channel. This is the **coordination wire** for step 4–6 ("host is up, go now") and
   the place a version mismatch / wrong password is caught *before* we touch the game session.
4. **Host drives Call A.** Host runs create-session → `lobby_state` `None → TryToCreateSession → Host`;
   set the seat limit; establish the password-derived AES key.
5. **Host signals "session up"** over the rung-2 side-channel (or the joiner simply proceeds knowing the
   host SteamID — but an explicit side-channel "ready" is the clean ordering and avoids a join-before-host
   race).
6. **Joiner drives Call B** with the host SteamID + same password-derived key → `lobby_state`
   `None → TryToJoinSession → Client`; `protocol_state` walks to `Ingame`. The game's net sync takes over;
   players see each other in-world.
7. **Steady state.** The observer logs the live transitions; the side-channel can optionally migrate
   in-band onto `broadcast_packet`. Roster shrink → prune the departed peer from the linked set
   (COOP-CONNECTION.md rung 3).

Key point: **rung 4 supplies the peer identity, rung 2 supplies the timing.** The session calls
themselves carry only `this` + peer SteamID + the (password-derived) key; everything that *picks* the
peer and *sequences* the two calls is already built.

## ERSC behavioral reference (clean-room — public knowledge, in our own words)

High-level, behavior-only, no ERSC code/bytes/decompiler output — just the publicly-known shape of what
ERSC does to run co-op offline, as a sanity check that the requirements above are the right ones:

- **Skip the matchmaker, keep the peer-to-peer.** ER co-op gameplay rides Steam P2P; FromSoft's servers
  only broker *who* pairs with whom (via summon signs / invasions). ERSC's whole idea is to **bypass that
  brokering** and pair players another way, then run the game's normal session over Steam P2P. (This is
  the premise COOP-CONNECTION.md is built on.)
- **One shared password is the pairing key.** Everyone in a co-op group sets the same password; it's
  what stands in for the matchmaking server deciding who connects, and it's tied to the session's
  encryption so the modded clients can talk without a server-issued key. (We mirror this: password →
  lobby token + auth proof today, and password → session AES key for rung 3.)
- **The host opens a session; others join it directly** rather than placing/answering a summon sign that
  the server would route. The mod drives the game's own session setup into the host/client roles for the
  chosen peers — i.e. it pushes the same `CSSessionManager` FSM we're charting, just reached without the
  server.
- **Raise the seat limit** beyond vanilla so more than one phantom can be in a world (the
  `session_player_limit` lever the SDK already names).
- **Re-enable what offline normally disables.** Outside the official online flow the game disables its
  multiplayer UI/affordances; the mod patches past those gates so a session can form. (This is exactly
  the offline-items / online-availability gate we flag as a precondition risk above.)

Nothing here is copied from ERSC; it's the publicly-understood *behavior* of offline-co-op mods, used
only to confirm our requirement list (password-derived key, direct host/join drive, seat limit, offline
gate) matches the known shape of the problem.

## The minimal-call spec (deliverable summary)

| Call | `this` | Other args | Key/identity | Preconditions | SDK provides? | Effect |
|---|---|---|---|---|---|---|
| **create / host** | `CSSessionManager*` (singleton / `[G]`) | session params (seat limit, flags) — register TBD by hook | password-derived **AES session key** | `lobby_state==None`, in-game, manager live, online-gate passed | **fn: NO** (RE gap); `this`+state+limit fields: **yes** | `None→TryToCreateSession→Host` |
| **join peer** | `CSSessionManager*` | **host SteamID64** (`u64`, register TBD by hook) | **same** password-derived AES key | same as above + host already `Host` | **fn: NO** (RE gap); peer id (rung 4) + state: **yes** | `None→TryToJoinSession→Client`, `protocol→…→Ingame` |
| *(post-connect transport)* | `NetworkSession*` | buffer/len/type | — | session up | **YES** (`NetworkSessionVmt`) | `broadcast_packet`/`receive_packet` |

**Bottom line:** the SDK hands us the session object, the FSM target states, the roster, the seat-limit
lever, the cipher fields, and the entire post-connection transport. It does **not** hand us the two
initiation calls or a server-free session key. So a direct-drive implementation needs exactly three RE
deliverables, all rig-gated and all already scoped: **(1)** the create-session function entry +
argument mapping, **(2)** the join-session function entry + which register is the peer SteamID, and
**(3)** the password→AES-session-key derivation (+ confirmation of any function-internal online gate).
The peer identity and the call ordering are already solved by rungs 4 and 2.

## Why a direct create fails offline (the rung-3 create wall)

> **Current truth — root cause CONFIRMED (2026-06-29, in-world rig).** A **solo** direct-drive create
> cannot succeed. The create wrapper fires and is rejected **synchronously** (`lobby_state None →
> FailedToCreateSession`, returns `false` the same frame). With the leg-A gate bypassed and reject #1
> forced, create passes every static gate charted below — leg A, rejects #1/#2/#3, and the 4th gate —
> then dies in **leg B's tail capacity check**: the session-slot array has **capacity 0** offline
> (`cmp count,[rbx+0x20]; jae fail` with `rbx=[[NetworkSession+8]+0x48]` and `[rbx+0x20]==0` → `0>=0` →
> fail), because no real match/lobby ever allocated it, so the finished session object has nowhere to be
> stored. It is **not** OOM, the gate, the 4th gate, or the finalize registry. **The unblock is the
> 2-player drive** (a live rung-4 lobby + a real peer is what sizes the slot array). The static anatomy
> and how each candidate was ruled out follow; superseded conclusions are kept as one-line tombstones.
>
> **Paths forward:**
> 1. **2-player drive (highest-EV, the real fix):** drive create with a live rung-4 lobby + a real peer
>    so the game allocates the slot array (cap>0), then leg B's tail store succeeds. Set `[debug.probes]
>    drive_create` + `[gameplay] bypass_session_create_gate` + `[debug.probes] force_netsession_ready`
>    on both machines. Open question: *what* allocates the slot array (normal match setup vs. whether our
>    rung-4 lobby alone triggers it) — answerable only with a real peer.
> 2. **Fabricate the slot array (risky fallback):** allocate a backing array, write `[NetworkSession+0x18]`
>    = base and `[+0x20]` = capacity (≥ seat count), and let the tail store proceed. Heavy and likely
>    produces a malformed session the game can't run; only if (1) is impossible.
>
> **Superseded hypotheses (tombstones — do not revisit):**
> - *"The leg-A gate `0x140cb4b50` is the blocker."* Wrong. A hardware write-watch on `[G]+0x24`
>   (`scripts/re/watch-write.py --addr <G+0x24> --access write`, with `enable_offline_multiplayer` +
>   `bypass_session_create_gate` applied) **HIT at `RIP=0x140cb2086`** — 3 bytes past the leg-B store
>   `mov [this+0x24], eax` at `0x140cb2083`, which is reached **only if the gate branch passed**. So the
>   bypass gets control to leg B; the gate is not the blocker. (A passive peek of `[G]+0x24 == 0` is
>   *ambiguous* — never-written vs. leg-B-wrote-`eax=0`; only the write-watch disambiguates, and it shows
>   leg B ran. The earlier peek-only read of `[G]+0x24=0` / `[G]+0xc=2` wrongly concluded "gate rejected.")
> - *"Reject #1 (`NetworkSession+0x10==0`) is the blocker."* Real (it is 0 offline) but **not sufficient**
>   — forcing it nonzero did not unblock.
> - *"Create dies in the session-object registry/init chain (`0x1423fa1b0`)."* Wrong — that chain is
>   OOM-only.
> - *"The 4th gate `0x1423fd7a0` is the blocker."* Wrong — in-world its fields are populated and it
>   returns true.
> - *"The create blocker shares the item-grey service `0x144842d40`."* Wrong — the finalize uses numeric
>   global `0x144842d28` (a hash modulus, merely a `.data` neighbor), no proven link.

The rung-3 direct-drive is **proven to fire but be rejected**: calling the create wrapper
`0x140cad4c0` on `[G]` (`this`=live `CSSessionManager`, `flag=0`, `mode=4`, `settings={u16:0,u32:2}`,
no item, no peer) moved `lobby_state None → FailedToCreateSession` **synchronously** — one transition,
the call returned `false` the same frame. So a *synchronous software check* rejected it (not an async
matchmaking timeout — we never reached `TryToCreateSession`). And `enable_offline_multiplayer`
(forces `is_offline()` false) was applied this run and was **insufficient** — so the rejecting gate is
something other than `is_offline()`.

This pass traced the create chain's failure paths statically on the same pinned **2026-06-02
`eldenring.exe`** (image base `0x140000000`). Behavior is in my own words; addresses are facts; no
decompiler output reproduced (CLAUDE.md > Clean-room).

### The chain has exactly two synchronous reject points (the builder isn't one)

```
wrapper 0x140cad4c0  ── inner returns false ──▶ sets lobby_state = 2 (FailedToCreateSession)
   └▶ inner 0x140cb1f70:
        ├ guard: lobby_state ∈ {1,3} → return true (already creating/host)
        ├ guard: lobby_state ∈ {4,6} → return false (busy joining/client)
        ├ call [0x143b3acd8]()                         ; obfuscated pre-gate helper (thunk → 2nd .text)
        ├ call 0x140cb4b50(this)  ──▶ test al,al  ──▶ FALSE = FAIL   ◀═══ LEG A (the gate)
        ├ call build_params() [callee body @ 0x140cb20d0](this,out,flag,count) ; ← is_offline() lives HERE, never rejects
        │     (listed in execution order; 0x140cb20d0 is the callee's entry, not a later call site)
        ├ accessor 0x1423f1930([this+0x60]) → *(…)+0x710 = NetworkSession*
        └ call [vtable+8](netsession, out, 0)  ──▶ store [this+0x24]=eax ──▶ eax==0 = FAIL  ◀═ LEG B
              on success: [this+0xc]=1 (TryToCreateSession), [this+0x1b]=1, return true
```

The **params builder `0x140cb20d0` never rejects** — it returns `void`/builds the struct and is the
*only* place `is_offline()` (`0x140e55180`) is consulted (twice), but those calls just set param
fields (`out[0] |= 1`, the `0x101` vs `0x100` word, the MTU/buffer size), they never gate the inner's
return. So forcing `is_offline()` false changes the params, not whether create succeeds. **That is
exactly why `enable_offline_multiplayer` was insufficient.** The two real reject points are:

- **Leg A — the shared availability gate `0x140cb4b50(this)`** (create call site `0x140cb2025`, join
  call site `0x140cb2570`). Returns a bool; `false` → fail. Runs **before** the params builder, hence
  **before/independent of `is_offline()`**.
- **Leg B — the network-session create vmethod** `[netsession_vtable + 8]` (create dispatch at
  `0x140cb207f`). Returns a `u32` stored to `[this+0x24]`; `0` → fail. Dynamic target (resolved at
  runtime), not statically decodable.

### Leg A — the availability gate `0x140cb4b50` (was the lead suspect; rig: bypass works, not the blocker)

The gate `0x140cb4b50` was the leading *static* suspect for the synchronous reject. The rig later proved
the bypass clears it and the real reject is leg B (see the write-watch tombstone above), but the static
charting stands and is load-bearing for re-derivation:

1. **It's `is_offline()`-independent.** It runs first, before the builder; `is_offline()` only sets param
   fields downstream and never rejects. This is why `enable_offline_multiplayer` didn't help — the gate is
   on a different signal.
2. **It takes only `this`** (`mov rcx, rbx` is the sole arg setup before the call; `rdx/r8/r9` are
   leftovers), so our `flag`/`mode`/`settings` cannot influence its verdict — for leg A, **hypothesis (b)
   (arg validation) is ruled out.**
3. **It's Arxan-encrypted in place.** Its body (`.pdata` `0x140cb4b50..0x140cb4c6d`, 285 bytes) reads as
   high-entropy garbage on disk (Shannon entropy **7.27** vs **5.59** for its clean neighbors) — the
   **only** encrypted function in the whole `0x140cb4000..0x140cb6000` block; every sibling decodes
   cleanly or is an `e9` jump-thunk. Selective encryption of one function is the signature of an **EAC /
   anti-tamper / online-entitlement** check. It also **can't be passively dumped**: it's encrypted *in
   memory* too (live ciphertext `af 34 c0…` ≠ on-disk `2a 8b 84…`) and re-encrypts after execution
   (post-drive peek == pre-drive peek), so only an in-execution capture could read its body — its exact
   predicate (which global/service it reads) **cannot be decoded statically.**
4. **It's shared by create AND join.** `0x140cb4b50` has exactly two callers — the create inner and the
   join inner — each calling it (`call [0x143b3acd8]()` then `call 0x140cb4b50(this)`, identical sequence)
   right after the `lobby_state` guards and bailing to `FailedToCreate`/`FailedToJoin` on false. That is a
   generic "is multiplayer permitted right now?" availability gate, not a create-specific argument check.

It is plausibly *related to* the elusive item-grey signal (both are "is online play available" gates) but
**likely a distinct 4th signal**: the item-grey hunt already rig-eliminated the mode enum / `is_offline()`,
`IsEnableOnlineMode`, and the cached online-available chain (see
[OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)), and this gate is separately Arxan-protected and
consulted by the **session FSM** rather than the menu. If a runtime hook ever shows it reading the same
service singleton (`0x144842d40`) the item leaf reached, they converge; otherwise it's a new signal.
Either way it's moot for the create unblock — the bypass already passes it.

### Hypothesis (b) (arg validation) is unlikely — and we charted what the args actually do

Neither the inner nor the builder validates `flag`/`mode`/`settings`; they only flow the args into the
params struct. So (b) is only in play if Leg A *passes* and Leg B (the network create) rejects on an
arg. For the record, what our drive's args become:

- **`flag=0`** (`dl`) → forwarded into `build_params` as its `flag` byte and written into the params
  struct; no `cmp flag, …; jne fail` exists in the inner or builder. The natural sign/host path sources
  this from `byte[SosSignData+0x2e]`; the no-peer driver `0x140a23010` sources it from `[reqobj+0x68]`.
  Not validated synchronously here.
- **`mode=4`** (`r8d`) → the inner moves it to `esi` then passes it to the builder as the **player
  count** (`r9d`). The builder clamps it against `[this+0x25c]` (`session_player_limit_override`, =1
  from the ctor: `cmp eax,1; cmovg r9d,eax` leaves `r9d=4` since the override isn't >1) and writes
  `session_player_limit` `[this+0x170] = 4`. So "mode=4" is really "**4 seats**" — a sane value, not a
  mode rejection.
- **`settings={u16:0,u32:2}`** (`r9` → the inner's `extra`/`void*`) → passed to the builder as its
  stacked 5th arg; consumed as session-config fields, no validation-reject.

So if the rig shows Leg A passes and create *still* fails, the next move is to vary these args against
Leg B — but the static read says they're well-formed and (b) is the weaker hypothesis.

### Re-derivation: disambiguating leg A vs leg B (write-watch on `[G]+0x24`)

Both legs end identically (inner returns false → wrapper sets `lobby_state=2`), so timing can't
disambiguate; one observation can. The exe loads at preferred base `0x140000000` (confirmed), so static
VA == live VA; read `[G]` (`[0x143d7a4d0]`) for the live `this`. `[this+0x24]` is written **only** at
`0x140cb2083`, reached **only if leg A passed**, so a 4-byte write-watch on `<G_instance>+0x24`
(`scripts/re/watch-write.py --addr <base+0x24>`) across a `[debug.probes] drive_create` fire tells the
legs apart: **fires** ⇒ leg A passed, leg B rejected; **never fires** ⇒ leg A rejected. (Run: it fired —
see the leg-A tombstone above; a *passive peek* of `[G]+0x24` can't substitute since `0` is ambiguous
between never-written and leg-B-wrote-`0`.) To actively confirm, set `[gameplay]
bypass_session_create_gate = true` (landed, default-off — below) and re-drive: the bypass flips leg A's
verdict so any remaining failure is leg B's.

### Patch candidate (landed, default-off): `gameplay.bypass_session_create_gate`

Wired in `coop/app.rs::apply_boot_patches`, mirroring the other experimental boot patches. It patches
the **create call site** (clean, un-encrypted code in the inner) — not the encrypted gate body — so the
gate still *runs* but its `false` verdict no longer fails the create:

```
0x140cb2025  e8 26 2b 00 00   call 0x140cb4b50     ; the gate
0x140cb202a  90               nop
0x140cb202b  48 8d 4c 24 30   lea  rcx, [rsp+0x30]
0x140cb2030  84 c0            test al, al
0x140cb2032  75 07            jne  0x140cb203b      --> EB 07  jmp 0x140cb203b   (always take success)
```

- **landmark (unique, 15 bytes):** `E8 26 2B 00 00 90 48 8D 4C 24 30 84 C0 75 07` — exactly one match
  in the image. The leading `E8 26 2B 00 00` is the gate's **call rel32**, which is create-specific (the
  join site's rel32 to the same gate differs), so this stays unique to create; the bare
  `48 8D 4C 24 30 84 C0 75 07` tail occurs 2× (create + join). offset **13** is the `75` (`jne`),
  expect `0x75`, replacement `EB` (`jmp`). Fail-safe (no-op + logged) on miss/ambiguous/drift, like the
  other boot patches.
- **Why flip the branch, not NOP the call:** keeping the `call 0x140cb4b50` preserves any side effects
  the gate performs (it may set up state the later network create reads); only its veto is ignored. The
  alternative — overwrite `e8 26 2b 00 00 90` with `b0 01 90 90 90 90` (`mov al,1`; nops) to skip the
  encrypted gate entirely — is riskier (drops side effects) and is the fallback only if running the gate
  is itself the problem.
- **Caveat:** if the gate is load-bearing for the network create, this bypass just moves the failure to
  Leg B (still `FailedToCreateSession`). The write-watch confirmed this is exactly what happens — the
  bypass passes leg A and the failure moves to leg B (charted below).
- **Join:** the join inner has the identical gate site (`0x140cb2570` → `jne` at `0x140cb257d`); a
  parallel bypass would flip that `jne`, but join is the two-player leg and not solo-confirmable, so
  this lane wires create only.

### Leg B charted — the network-session create vmethod

Leg B is the real synchronous blocker. **Resolving the vmethod (live, sudo-free pointer walk):**
`[ *( *(this+0x60) + 0x710 ) + 8 ]` — the create inner does `lea rcx,[this+0x60]; call 0x1423f1930` (a
3-instruction getter: `rax=[rcx]; rax+=0x710; ret`), then `r9 = *(rax)` (a vtable ptr), and `call [r9+8]`
with `this' = *(this+0x60)+0x710`. So `this`=`[G]` → `P = *(this+0x60)` → the embedded `NetworkSession`
sub-object at `P+0x710` → its vtable `VT = *(P+0x710)` → leg B = `VT[1] = *(VT+8)`. **Walked live:**
`P = 0x143dcd470` (a stable `.data` singleton), `VT = 0x1431f9140` (`.rdata`), **leg-B vmethod =
`0x1423f5c00`** (`.text`); create dispatch is at `0x140cb207f`. (`P` drifts across runs/states —
`…3f0`/`…450`/`…470`, all `.data`; a post-failure transient at `0x143dcd450` resolves `+0x710` into
`.data` garbage — ignore it, the valid `.text` chain is the `0x143dcd470` one. Don't hand-peek-walk the
chain blindly; it's `scripts/re/watch-write.py --peek`-able, `/tmp/walk-legb.py` did it.)

**Leg B is CLEAN, not Arxan-encrypted** (entropy 5.30; disassembles to real x86), so it reads statically.
Its return is `esi`: the success path sets `esi` = the result of the session-register/finalize call
`0x1423fab40` (nonzero); **every early reject jumps to `0x1423f5cf9: xor esi,esi`** → returns 0 → inner
returns false → wrapper sets `FailedToCreateSession`. The synchronous rejects, in order:

1. **Reject #1 — `*(NetworkSession+0x10) == 0`** (`lea rcx,[this+0x10]; call 0x141eba210` where
   `0x141eba210` is `mov eax,[rcx]; ret`, a getter for the dword at `+0x10`; `test eax,eax; je fail` at
   `0x1423f5c4f`). A readiness/enabled flag on the NetworkSession, **0 offline** — the dword at
   `*([G]+0x60)+0x710 + 0x10`.
2. **Reject #2 — `this->vtable[0xe8](this, params, true) == false`** (virtual at `0x1423f5c61`; `je fail`
   at `0x1423f5c69`). Vmethod `[0xe8]→0x1423f6fb0` (from `VT=0x1431f9140`) is `mov al,1; ret` — **always
   true, can never reject.**
3. **Reject #3 — `this->vtable[0x108](this, params, true) == null`** (virtual at `0x1423f5c7b` returning a
   pointer; `je fail` at `0x1423f5c87`; on success `rdi` = the new session object). Vmethod
   `[0x108]→0x1423f7070` allocates a `0x5f8`-byte session object (`call 0x141eb9ed0(ecx=0x5f8, edx=8)`),
   returns null **only on alloc failure (OOM)**, else bumps a counter `[this+0xa8]`, constructs the object
   (`0x1423fd300`), returns it. Not an offline gate.
4. **4th gate — `[new_obj_vtable+8](new_obj) == false`** (call at `0x1423f5c8f`; `test al,al; jne
   0x1423f5cab` at `0x1423f5c92`; false → cleanup → `esi=0`). Charted below; **passes in-world.**

So #2/#3 are eliminated statically. Reject #1 was initially the lead offline suspect (the only reject that
can fire offline) — the rig confirmed it's real but **not sufficient**:

> **Rig (`force_netsession_ready` probe).** Drove create with `bypass` + `enable_offline_multiplayer` + a
> probe that resolves `NetworkSession = *([G]+0x60)+0x710` and writes `[NetworkSession+0x10]` nonzero just
> before the call. Confirmed `NetworkSession+0x10 = 0` offline (static read was right); forcing it to 1
> (persisted — a post-run peek read `1`) **did not unblock** (still `false → FailedToCreateSession`,
> `[G]+0x24 = 0`). Caveat: `P` drift (`…3f0`/`…450`/`…470`, all `.data`) means a pre-write may not land on
> the exact object leg B reads at call time, so a rigorous force writes from *inside* a leg-B-entry hook —
> but the gates below mean reject #1 alone can't clear create regardless. `[debug.probes]
> force_netsession_ready` stays a charted, default-off probe.

**The finalize/registry chain is OOM-only — not an offline gate** (this corrects an earlier note that
blamed it):

- **`0x1423fab40`** (finalize) → `0x1423fa1b0(new_obj, cmp=0x1423fc6a0, mode)`; returns 0 only if that
  returns null.
- **`0x1423fa1b0`** is a registry / hashmap lookup-or-insert on the new session object: bucket count from
  the **numeric** global `0x144842d28` (used as a `div` modulus, **not** a pointer), comparator callback
  `0x1423fc6a0`, resolving via `[new_obj_vtable+0xd8]` (`0x1423fdfa0`) then a secondary lookup
  `0x1423fa100`. Both null-return points **only fail on allocation:** `0x1423fdfa0` allocates `0x60` via
  `0x141eb9ed0` (null iff alloc fails, else constructs an entry); `0x1423fa100` is the same shape
  (allocates `0x58`, null only on alloc/`0x1423f7290` failure). So `0x1423fa1b0` (hence finalize) **always
  succeeds offline barring OOM** — it is not where create dies.
- **Correction:** an earlier pass claimed `0x144842d28` was the **same** online-availability service as the
  item-grey hunt's `0x144842d40` ("merging the two hunts"). Wrong — `0x144842d28` is a numeric
  hash-modulus, merely a `.data` neighbor of `0x144842d40`; there is **no proven link** between the create
  blocker and the item-grey service.

### The 4th gate charted (`0x1423fd7a0`) — session-config fields; passes in-world

Between reject #3 and the finalize call there is a 4th synchronous gate (read statically on the same
2026-06-02 image). After reject #3 returns the new `0x5f8` session object (`rdi`), leg B does `call
[new_obj_vtable + 8](new_obj)` at `0x1423f5c8f` and proceeds to the register path only if it returns true
(`0x1423f5c92: test al,al; jne 0x1423f5cab`); false falls through to cleanup → `esi=0` →
`FailedToCreateSession`.

- The new object's vtable is **`0x1431fa248`** (installed by the constructor `0x1423fd300`: `mov [obj],
  0x1431fa248`); slot `+0x8` = **`0x1423fd7a0`** (the 4th gate), slot `+0xd8` = **`0x1423fdfa0`** (the
  registry-key vmethod above).
- **4th gate `0x1423fd7a0`** returns false if **both** `[new_obj+0x3b0]==0` **and** `[new_obj+0x3b4]==0`;
  otherwise it calls helper **`0x1423faf60`** and returns its result.
- **Helper `0x1423faf60`** bails false if any of five dwords `[new_obj+0x68], +0x6c, +0x70, +0x74, +0x78`
  is zero, then runs a vmethod (`[[new_obj+0x58]]+0x8`) and three `0x1423fd110` sub-checks that all must
  pass.

These are **session-configuration fields** (seat counts / peer slots / match params). Statically they
looked like the offline blocker (all zero in a freshly-constructed object with no peer context), but the
rig overturned that: in-world they are populated and the gate passes (next).

### Rig: the 4th gate passes — create dies at leg B's tail capacity check (root cause)

Ran the leg-B gate tracer (`[debug.probes] drive_create` + the two `gate-trace` hooks) **in-world** (main
player present) with `bypass_session_create_gate` + `force_netsession_ready` + `enable_offline_multiplayer`:

```
gate-trace legb-entry  REACHED — NetworkSession=0x143dcdb30  reject#1 [+0x10]=1
gate-trace create-gate4 REACHED — obj=0x7ffe93851cd0
   gate[+0x3b0]=35000  gate[+0x3b4]=5000  helper[+0x68..0x78]=[6,30000,30000,30000,30000]
drive-create returned false — FailedToCreateSession
```

In-world the 4th gate's fields are **populated, not zero** (`[+0x3b0]=35000`, `[+0x3b4]=5000`, helper
dwords `[6, 30000, 30000, 30000, 30000]` — the `6` is `max_players` from the rig config, the rest read
like network timeouts in ms), so `0x1423fd7a0` returns **true**. The earlier "4th gate is the blocker"
was an artifact of driving **too early**: when create is driven during the load transition
(`GameState::in_game()` flips true before `WorldChrMan` is populated) leg B isn't even reached (neither
hook fires); with the main player actually present it sails through reject #1–3 and the 4th gate. (Fixed
in code: `SessionCreateDriver` now gates on `sdk::with_active_main_player(...).is_some()`, not just
`GameState::in_game()`, so the drive fires only once the world is genuinely loaded.)

So the failure is in leg B's **tail**, past every gate. A second in-world run with the tracer extended to
read the NetworkSession's session-slot array (`rcx` at entry IS the NetworkSession; the array is at
`+0x18`/`+0x20`/`+0x24`) pinned it:

```
gate-trace legb-entry REACHED — NetworkSession=0x143dcdad0  reject#1 [+0x10]=1
   slot-array [+0x20]cap=0  [+0x24]count=0
gate-trace create-gate4 REACHED — fields populated (35000/5000/[6,30000,30000,30000,30000])
drive-create returned false — FailedToCreateSession
```

**Root cause: the slot-array capacity is 0.** Leg B's tail store is `mov eax,[rbx+0x24]; cmp
eax,[rbx+0x20]; jae fail` with `rbx = [[NetworkSession+8]+0x48]`; `cap=0` → `0 >= 0` → fail, so the
freshly-built (and likely finalized) session object **can't be stored** — the slot array was never
allocated. It is not OOM, not the gate, not the 4th gate, not the finalize registry. It is precisely what
a **real match/lobby allocates** (the slot array on the NetworkSession is sized when a multiplayer session
is actually set up), which is why the unblock is the 2-player drive (see "Paths forward" at the top of
this section). The considered alternative tail point — the finalize `0x1423fab40` returning 0 — is ruled
out: that chain is OOM-only (above).

**Rig tooling:** autonomous in-world is now solved — `scripts/rig.sh cycle --in-world` (the new
`enter-world` step) selects "Continue", loads the save, and waits for `in_gameplay` (~33s), so the
one-shot drive fires unattended. The leg-B gate tracer (entry: reject #1 + slot-array cap/count; 4th-gate:
the config fields) stays a charted default-off probe under `drive_create`.

### Tooling / re-derivation

Found with `scripts/re/static.py` (the committed PE workhorse): `fn` to disassemble the inner/builder,
`calls`/`xref` to prove the gate's two callers and the `[0x143b3acd8]` fnptr sites, `.pdata` bounds +
a byte/entropy read to prove the gate is the lone encrypted function in its block. After a game update:
the create inner is the `mov [this+0xc],1` function in the `CSSessionManager` method block
(`0x140cad000..0x140cb3000`); the gate is the **bool-returning call it makes after the `lobby_state`
guards and before the params builder `0x140cb20d0`** — re-take the `call + nop + lea rcx,[rsp+0x30] +
test al,al + jne rel8` as the landmark (the concrete call rel32 keeps it create-specific) and flip the
`75` to `EB`.

## Cross-references

- [COOP-CONNECTION.md](COOP-CONNECTION.md) — the connection plan; rung 3 is the section this spec serves.
- [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) — *how to find* the two function entries (the
  write-watch). This doc is *how to call* them once found.
- [SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md) — the static anchors: the `G = 0x143d7a4d0` keystone,
  the constructor, the field offsets, and why static stops at the write-watch.
- [SDK-COVERAGE.md](SDK-COVERAGE.md) — the networking/session row this survey expands.
- [OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md) — the offline online-availability gate that may
  also gate the initiation function (precondition risk).
- SDK source (pinned `8c67a84`): `crates/eldenring/src/cs/session_manager.rs`,
  `crates/eldenring/src/cs/network_session.rs`, `crates/eldenring/src/cs/net_man.rs`,
  `crates/eldenring/src/rva/bundle.rs` (the full callable-RVA list).
- Probe scaffold: [`coop/session_probe.rs`](../crates/unseamless-coop/src/session_probe.rs).
</content>
</invoke>
