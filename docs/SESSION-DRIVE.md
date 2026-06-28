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

## Why a direct create fails offline (static, 2026-06-27, worker `create-gate`)

> **RIG RESULT (orchestrator, 2026-06-28): the GATE rejected, and flipping its branch is NOT enough.**
> Re-drove `drive_create` with **both** `enable_offline_multiplayer` and `bypass_session_create_gate`
> applied (boot log: `patched 'bypass_session_create_gate': [75] -> [EB]`). Result: still
> `FailedToCreateSession`, and a live peek of **`[G]+0x24 = 00 00 00 00`** (never written) with
> `[G]+0xc = 2`. Per the recipe below, `[G]+0x24` un-written ⇒ the **gate rejected** — i.e. control
> never reached the network-create (leg B). So even with the gate's success-edge `jne→jmp` flipped, the
> create still fails at the gate: the encrypted gate's `false` verdict is **load-bearing deeper than the
> single branch** — it almost certainly sets failure state (or a second check) the create path consults
> downstream, before leg B. **Bypassing the one branch is insufficient.** Next options, in rough order:
> (1) **runtime-RE the gate** — it's Arxan-encrypted in `.text` but decrypted in memory at runtime, so
> a Frida/ptrace dump of `0x140cb4b50`'s live body reveals what it actually checks (likely an EAC /
> entitlement / online-session-available signal — possibly the same 4th signal the item-grey hunt
> chased); (2) **ilhook-redirect the gate *call*** to a stub that returns `true` AND skips the real
> gate entirely (no encrypted-`.text` patch, avoids its failure side-effects — but may also skip setup
> the create needs); (3) **route around it** — call the network-create leg B vmethod directly, past the
> wrapper/inner that hold the gate. The simple boot-patch lever is exhausted; this needs runtime RE.


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

### The leading suspect: gate `0x140cb4b50` — hypothesis (a), an availability gate distinct from `is_offline`

Four facts make `0x140cb4b50` the prime synchronous reject and put it squarely in **hypothesis (a)**:

1. **It's `is_offline()`-independent.** It runs first, before the builder; `is_offline()` only sets
   param fields downstream and never rejects. This directly explains why `enable_offline_multiplayer`
   didn't help — the gate is on a different signal entirely.
2. **It takes only `this`** (`mov rcx, rbx` is the sole arg setup before the call; `rdx/r8/r9` are
   leftovers). So our `flag`/`mode`/`settings` **cannot** influence its verdict. ⇒ if Leg A is the
   reject, **hypothesis (b) (arg validation) is ruled out** — the args never reach a check that could
   change the outcome.
3. **It's Arxan-encrypted in place.** Its body (`.pdata` `0x140cb4b50..0x140cb4c6d`, 285 bytes) reads
   as high-entropy garbage on disk (Shannon entropy **7.27** vs **5.59** for its clean neighbors) — it
   is the **only** encrypted function in the whole `0x140cb4000..0x140cb6000` block; every sibling
   decodes cleanly or is an `e9` jump-thunk. Selective virtualization/encryption of one function is the
   signature of an **EAC / anti-tamper / online-entitlement** check — precisely the kind of gate that
   would reject session creation outside EAC.
4. **It's shared by create AND join.** `0x140cb4b50` has exactly two callers — the create inner and the
   join inner — each calling it (`call [0x143b3acd8]()` then `call 0x140cb4b50(this)`, identical
   sequence) right after the `lobby_state` guards and bailing to `FailedToCreate`/`FailedToJoin` on
   false. That is the shape of a generic "is multiplayer permitted right now?" availability gate, not a
   create-specific argument check.

Because the gate body is encrypted, **its exact predicate (which global/service it reads) cannot be
decoded statically** — that needs a runtime hook (below). It is plausibly *related to* the elusive
item-grey signal (both are "is online play available" gates), but it is **likely a distinct 4th
signal**: the item-grey hunt already rig-eliminated the mode enum / `is_offline()`, `IsEnableOnlineMode`,
and the cached online-available chain (see [OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)), and
this gate is separately Arxan-protected and consulted by the **session FSM** rather than the menu. If
the runtime hook shows it reading the same service singleton (`0x144842d40`) the item leaf reached,
they converge; otherwise it's a new signal. Either way the *fix surface* is the same (force the gate to
pass), so the convergence question is informational, not blocking.

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

### Runtime-verify recipe (orchestrator) — disambiguate Leg A vs Leg B

Both legs end the same way (inner returns false → wrapper sets `lobby_state=2`), so timing doesn't
disambiguate; one observation does. The exe loads at its preferred base `0x140000000` (confirmed), so
static VA == live VA. Read `[G]` (`[0x143d7a4d0]`) for the live `this`.

1. **Cheapest passive probe — write-watch `[G]+0x24`.** `[this+0x24]` is written **only** at
   `0x140cb2083`, which is reached **only if Leg A passed** (the gate returned true). Arm a 4-byte
   write-watch on `<G_instance>+0x24` (`scripts/re/watch-write.py --addr <base+0x24>`), then re-drive
   create (`[debug.probes] drive_create`):
   - **`[G]+0x24` is written** (watch fires) right before `lobby_state→2` ⇒ **Leg A passed, Leg B
     (network create) rejected** — the gate is *not* the (sole) blocker; investigate the network
     create / hypothesis (b).
   - **`[G]+0x24` is never written** before `lobby_state→2` ⇒ **Leg A (the gate `0x140cb4b50`)
     rejected** — hypothesis (a) confirmed; the gate is the synchronous reject.
2. **Direct confirm — hook the gate.** Hook `0x140cb4b50` read-only and capture its return (`al`) on the
   drive: `al==0` ⇒ Leg A rejected (the gate is the gate). `al==1` ⇒ gate passed, failure is Leg B.
3. **Active confirm — flip the bypass.** Set `[gameplay] bypass_session_create_gate = true` (landed,
   default-off — see below) and re-drive:
   - create now reaches **`TryToCreateSession`** ⇒ the gate *was* the reject (hypothesis (a) confirmed,
     and the bypass is a working unblock).
   - create still **`FailedToCreateSession`** but `[G]+0x24` now *is* written ⇒ the gate was
     load-bearing and the failure moved to Leg B (the network create needs whatever the gate was
     gating) — deeper RE on the network-session create path.

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
  Leg B (still `FailedToCreateSession`). That's the informative outcome the re-drive + `[G]+0x24`
  write-watch reveals (recipe step 3).
- **Join:** the join inner has the identical gate site (`0x140cb2570` → `jne` at `0x140cb257d`); a
  parallel bypass would flip that `jne`, but join is the two-player leg and not solo-confirmable, so
  this lane wires create only.

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
