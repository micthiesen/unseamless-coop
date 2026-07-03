# Driving a Session Directly (rung-3 call spec)

What it takes to **drive a `CSSessionManager` session directly** — so that the moment the create/join
initiation functions are charted (the rung-3 RE in [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) /
[SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md)), we already know exactly **how to call them**: the
arguments each takes, the game state that must hold first, the keys/identity to feed in, and the
ordering against the rung-2 side-channel and the rung-4 Steam lobby.

This is desk/static research — **no rig run**. It's the spec a future driven session implements
against, not a record of a confirmed call.

> **Scope & legitimacy.** Static RE on a game we own, on the developer's own machine, to drive its
> session layer for our own co-op mod — co-op-only, *outside* anti-cheat, no DRM-cracking. Where the
> text notes an "EAC / anti-tamper" check, that's us *identifying* why a function is encrypted so we
> can reimplement around it on our own machine, not defeating protection to reach the official
> servers. Behavioral notes only; no upstream code copied. See CLAUDE.md > Safety / legitimacy +
> Clean-room hygiene.

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

> **Current truth (updated 2026-07-03, in-world + rig↔Deck).** A **solo** direct-drive create
> cannot succeed. The create wrapper fires and is rejected **synchronously** (`lobby_state None →
> FailedToCreateSession`, returns `false` the same frame). With the leg-A gate bypassed and reject #1
> forced, create passes every static gate charted below — leg A, rejects #1/#2/#3, and the 4th gate —
> then first hits **leg B's tail capacity check** when the session-slot array has **capacity 0**. Slot
> fabrication clears that capacity branch, including with a real linked peer, but create still fails.
> Static re-read now narrows the remaining zero producer to **the finalize handle at `0x1423f5cb7`**:
> `0x1423fab40` can return a zero registry-node id even after allocation succeeds. The next rig check is
> the finalize-handle probe below. The static anatomy and how each candidate was ruled out follow;
> superseded conclusions are kept as one-line tombstones.
>
> **Paths forward:**
> 1. **Finalize-handle probe (next rig run):** wire the `legb-finalize` trace described below, run the
>    existing fabricate+peer recipe once, and confirm whether `0x1423fab40` returns handle `0` before
>    the slot store. This is the current highest-EV check.
> 2. **Registry-id proof or seed:** if `handle=0`, confirm whether `[[NetworkSession+0x08]+0x6b8]`
>    consumed id `0` (direct hook inside `0x1423fa100`, or infer `post-next-id=1`). If confirmed, test a
>    tightly-scoped seed of that counter before finalize.
> 3. **Game match-setup comparison:** chart how the game's own matchmaking path seeds the registry id
>    space and slot array. Protocol reference for what the peer-join exchanges: vswarte's
>    `waygate-server`, cloned locally at `../waygate-server`
>    (`message/src/eldenring/{session,sign,matchingticket}.rs` + `wire/`); see the "Protocol reference:
>    `waygate-server`" note in [COOP-CONNECTION.md](COOP-CONNECTION.md) > Rung 3. An **annotated**
>    community / other-mod Ghidra DB of the session/network subsystem would short-cut identification.
>    *Clean-room (CLAUDE.md):* read it for
>    the **game's** behavior and reimplement from that — never transcribe pseudocode/annotations; if it's
>    ERSC's own decompilation, study the game, not ERSC.
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
> - *"Create dies because the session-object registry/init chain (`0x1423fa1b0`) returns null."* Wrong.
>   The null paths are allocation-shaped, but a later pass found a separate zero-id return through
>   `0x1423fab40`; see the finalize-handle chart below.
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

**Finalize/registry correction (supersedes the old "OOM-only" conclusion):** the helper can allocate
successfully and still return a create-failing zero handle. The null-return branches below are still
OOM-shaped, but the final value returned to leg B is a registry-node id, and that id can be `0`:

- **`0x1423fab40`** (finalize) calls `0x1423fa1b0(new_obj, cmp=0x1423fc6a0, mode)`. If that returns a
  null registry entry, finalize returns `0`. Otherwise finalize reads the backing node pointer from
  `entry+0x30` and returns the dword at `node+0x10`.
- **`0x1423fa1b0`** is a registry / hashmap lookup-or-insert on the new session object: bucket count from
  the **numeric** global `0x144842d28` (used as a `div` modulus, **not** a pointer), comparator callback
  `0x1423fc6a0`, resolving via `[new_obj_vtable+0xd8]` (`0x1423fdfa0`) then a secondary lookup
  `0x1423fa100`. Both null-return points **only fail on allocation:** `0x1423fdfa0` allocates `0x60` via
  `0x141eb9ed0` (null iff alloc fails, else constructs an entry); `0x1423fa100` allocates the backing
  node and returns null only on allocation / constructor failure.
- **What the earlier pass missed:** a non-null entry is not enough. `0x1423fa100` stores a per-container
  id into `node+0x10`, and `0x1423fab40` returns that id to leg B. If the first id consumed outside the
  game's own match setup is `0`, finalize returns `0`, and leg B takes the cleanup branch even though the
  registry allocations succeeded. The post-capacity tail chart below is the current model.
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

**First tail blocker in the non-fabricated run: the slot-array capacity is 0.** Leg B's tail store is `mov eax,[rbx+0x24]; cmp
eax,[rbx+0x20]; jae fail` with `rbx = the NetworkSession` (the array is **embedded on it** at
`+0x18/+0x20/+0x24`; the `[[NetworkSession+8]+0x48]` written here earlier is a mislabel — that's leg B's
cleanup return-pool, corrected in "Slot-array allocator charted" below); `cap=0` → `0 >= 0` → fail, so the
freshly-built (and likely finalized) session object **can't be stored** — the slot array was never
allocated. Later fabricate and fabricate+peer runs clear this capacity branch but still return false, so
capacity is necessary but not sufficient. The current next blocker is the finalize-handle gate charted
below.

**Rig tooling:** autonomous in-world is now solved — `scripts/rig.sh cycle --in-world` (the new
`enter-world` step) selects "Continue", loads the save, and waits for `in_gameplay` (~33s), so the
one-shot drive fires unattended. The leg-B gate tracer (entry: reject #1 + slot-array cap/count; 4th-gate:
the config fields) stays a charted default-off probe under `drive_create`.

## Slot-array allocator charted (static, 2026-07-02, worker:slot-allocator-re)

A **static** pass over the same pinned 2026-06-02 `eldenring.exe` (base `0x140000000`) to answer the
open question from "Paths forward": *who sizes the session-slot array, where does the size come from,
and is there a solo path to size it without a real peer?* No game running; addresses are facts about
the binary; behavior is in my own words (CLEAN-ROOM — no decompiler pseudocode transcribed).

### Correction: the checked array is embedded directly on the `NetworkSession`, not behind `[[+8]+0x48]`

The root-cause note above says leg B's tail check uses `rbx = [[NetworkSession+8]+0x48]`. That's a
mislabel. Reading leg B (`0x1423f5c00`) end-to-end, `rbx` is the **entry `rcx` (the `NetworkSession`)
unchanged** on the success path — the tail store is:

```
0x1423f5cbb  mov eax,[rbx+0x24]      ; count      (rbx = NetworkSession)
0x1423f5cbe  cmp eax,[rbx+0x20]      ; capacity
0x1423f5cc1  jae  fail
0x1423f5cc3  mov ecx,eax
0x1423f5cc5  mov rax,[rbx+0x18]      ; base
0x1423f5cc9  mov [rax+rcx*8],rdi     ; base[count] = session_obj
0x1423f5ccd  inc dword [rbx+0x24]    ; count++
```

i.e. `if (count < capacity) base[count++] = obj;` — a **bounded, no-grow push**. So the slot array is
**three inline fields on the `NetworkSession` object itself**: `base` (`T*`) at **+0x18**, `capacity`
(`u32`) at **+0x20**, `count` (`u32`) at **+0x24**. This matches the rig probe exactly (it read
`NetworkSession+0x20`/`+0x24` and got 0/0). The `[[NetworkSession+8]+0x48]` expression only appears on
leg B's **reject/cleanup** branches (`0x1423f5c96`, `0x1423f5cdb`): `[NetworkSession+8]` is a
sub-manager and `+0x48` is the **session-object return pool** the cleanup hands the object back to
(`pool->vtable[0x68](pool, obj)`), not the capacity array. Load-bearing because a fabrication/probe
writes `NetworkSession+0x18/+0x20` **directly** — chasing `[[+8]+0x48]` would target the wrong object.

### What the `NetworkSession` is, and why its slot-manager is a stub

- **Type.** The object at `*([G]+0x60)+0x710` (RTTI on its vtable `0x1431f9140`) is
  **`DLNR3D::SessionManagerSteam`**, derived from **`DLNR3D::SessionManager`** (base vtable
  `0x1431f8fe8`, installed by the base ctor `0x1423f5b90`). Its size is `0xb0` (the deleting-dtor
  `0x1423f7000` frees `0xb0`). Object shape used by leg B: `+0x00` vtable, `+0x08` sub-manager/lock+pool
  (`[+8]+0x10` is a critical section leg B takes via `0x141ed6210`/releases via `0x141ed6280`; `[+8]
  +0x48` the return pool), `+0x10` the reject-#1 readiness dword, `+0x18/+0x20/+0x24` the slot array,
  `+0xa8` a session-object counter (bumped by the reject-#3 allocator).
- **The player-slot vmethods are stubs.** The `CSSessionManager` layer's player-count tracker
  (`0x140cb6150`) resolves the `NetworkSession` and calls its vtable slots **+0x78** and **+0x80** to
  notify "player count changed" (`(net, count, add?)`). On **both** `SessionManager` and
  `SessionManagerSteam` those slots are **no-op stubs**: `+0x78 = 0x1423f69a0` (`xor al,al; ret`),
  `+0x80 = 0x1423f65b0` (`mov al,1; ret`). So changing the seat count does **not** size the slot array
  through this path. Leg B (`+0x08` in the vtable) is shared across the sibling session-manager vtables
  too (`0x1431f8fe8`, `0x1431f9140`, …), i.e. it's a base-class method, and none of them override the
  slot-manager stubs.

### Who sizes the array — and why nothing reachable does it solo

Working backwards from the tail store, the writer of `+0x18`(base)/`+0x20`(capacity) is **not** on any
path a solo create reaches:

- **Not the SMS methods.** None of the 34 `SessionManagerSteam` vtable methods writes `+0x18/+0x20`, and
  none calls a generic vector-reserve helper (checked every method + one level of callees).
- **Not the `CSSessionManager` create/host path.** The create inner (`0x140cb1f70`) reaches the
  `NetworkSession` via the accessor **`0x1423f1930`** (`rax = *(rcx); rax += 0x710; ret`) and immediately
  dispatches leg B — nothing reserves in between (confirmed: the params builder `0x140cb20d0` only sets
  `session_player_limit`, and leg B is the very next call). The accessor's 22 callers are all
  `CSSessionManager` methods in `0x140cad000..0x140cb6000`; each only **queries** the roster or calls a
  **stub** slot vmethod — none reserves.
- **Not a constant, not `session_player_limit`.** Because the notify-count path (`+0x78`) is a stub, the
  seat limit at `CSSessionManager+0x170` never reaches the slot array. The array stays `base=0, cap=0`
  from construction until *something outside these paths* sizes it.

That "something" is the **DLNR3D Steam-session layer when a real Steam P2P session actually forms** —
the same gate as the reject-#1 readiness flag (`NetworkSession+0x10`, also 0 offline). Both fields go
live together only with a real lobby/peer. This is the static confirmation of the doc's "a real
match/lobby allocates it": there is **no callable/settable game-native `reserve(N)` reachable without a
Steam session.** (Scans for the reserve — inlined `+0x18/+0x20/+0x24` writers, P-relative `+0x728/+0x730`
writers, `+0x710` navigators that reserve, allocator-fed `+0x18` stores in the DLNR3D neighborhood — all
came back either empty or as unrelated containers/ctors, e.g. the `CSSessionManager` ctor `0x140cabb60`
sizing its *own* member vectors. The reserve genuinely isn't in reach of the create path.)

### The solo path: fabricate the array (now principled, low-risk) — probe landed

Since no native reserve is reachable, the realistic solo route is the doc's path-2 **fabrication** — but
the correction above turns "blind fabrication" into a two-field write on a well-known object: point
`NetworkSession+0x18` at a small pointer buffer and set `NetworkSession+0x20` to a capacity, leaving
`+0x24 = 0`. Leg B's tail then stores the host's session object in slot 0 and create can walk
`None → TryToCreateSession → Host` with **no peer** (join still needs a real peer).

Implemented as `[debug.probes] fabricate_slot_array` (default off) in `coop/session_probe.rs`, riding
the existing **leg-B entry tracer** (`drive_create`'s `legb-entry` hook at `0x1423f5c00`). Fabricating
from *inside* that hook is deliberate: `rcx` there is the exact `NetworkSession` leg B will use (dodging
the `*(this+0x60)` P-drift that dogged `force_netsession_ready`), and it runs **before** the tail
`cmp count,capacity`. It only writes when the array is still empty (`cap==0 && base==0`), so it can't
clobber a real one. The backing buffer is a leaked, zero-filled `Vec<usize>` (process-lifetime stable);
the teardown tradeoff (the game may free a foreign pointer at disconnect) is acceptable for a one-shot
*does-create-reach-`Host`?* proof and is documented on the flag.

**Rig recipe (hand-off to the orchestrator).** On the local rig (solo, in-world), set all of:
`[debug.probes] drive_create = true`, `fabricate_slot_array = true`, `force_netsession_ready = true`,
`drive_fire_solo = true` (solo runs never link, so the drive must not wait for rung 2),
`[gameplay] bypass_session_create_gate = true`, `enable_offline_multiplayer = true`. Watch the
`session-probe:`/`gate-trace` lines:

- `gate-trace legb-entry REACHED … cap=0 …` then `fabricate-slot-array — sized empty array … capacity(+0x20)=16 …` → the array was empty and we sized it.
- `drive-create returned true — lobby_state now Host` ⇒ **fabrication is sufficient**: a sized slot array is the last thing solo create needs, and we can drive to `Host` with no second machine (huge — unblocks the create leg of rung 3 solo).
- `drive-create returned false … FailedToCreateSession` (with the fabricate line present) ⇒ the slot array is necessary but not sufficient; the finished object needs more real-session context. That result argues for the 2-player drive (path 1) and against fabrication as a shortcut — either way the log says which.

**Rig result (2026-07-03, solo, in-world) — NECESSARY BUT NOT SUFFICIENT.** Ran the full recipe. The
solo drive fired (`drive-create armed … — solo (fabricate_slot_array)`), reject #1 was forced
(`NetworkSession+0x10` 0→1), and leg B was reached with `cap=0 count=0`; fabrication then sized the array
in place (`fabricate-slot-array — sized empty array … base(+0x18)=… capacity(+0x20)=16 count(+0x24)=0`),
so leg B's tail `cmp count,capacity` can now store the session. **But create still returned
`FailedToCreateSession`** (`drive-create returned false — lobby_state now Some(FailedToCreateSession)`).
So: the capacity-0 wall is real and fabrication clears *that* gate, but it is **not the last blocker** —
create fails further in with the array sized, i.e. the finished session genuinely needs real-session
context a fabricated array can't supply. **Conclusion: fabrication is not a solo shortcut; the 2-player
drive (Steam Deck as player 2, or a friend) remains the path to rung-3 create.** The `fabricate_slot_array`
probe stays as a charted lever (it *does* clear the slot-array gate, useful alongside the 2-player drive).
Prereq found this run: the create-drive had been gated on a linked peer (commit `0a71d9b`), which never
links solo — so the drive's timing got a solo mode (skip the link wait). It was briefly implied by
`fabricate_slot_array` itself; since 2026-07-03 it's the separate `[debug.probes] drive_fire_solo` key,
decoupled so the fabricate+peer combination below can hold the drive for a real link *with* fabrication
armed.

**Two-machine result (2026-07-03, rig host + Steam Deck joiner, linked) — A REAL PEER DOES NOT SIZE THE
ARRAY EITHER.** First full rig↔Deck run of `docs/RUNG3-DRIVE-RUNBOOK.md` (fabricate OFF, drive holding
for the link): rungs 4+2 linked cleanly (password lobby found, `coop: linked` both sides, two-way
messages, **no Steam-friends requirement**), the drive fired 90 frames after link on both machines — and
both read `legb-entry … slot-array [+0x20]cap=0 [+0x24]count=0`, `drive-create returned false`,
`None->FailedToCreateSession`. Symmetric on host and joiner. Two corollaries: (a) the hypothesized
"real match/lobby allocates it" is narrower than 'any live rung-4 lobby + peer' — our private Steam
lobby + an accepted P2P session with ~1400 messages flowing left `NetworkSession+0x10` at 0 *and* the
array at cap 0, so the allocator is tied to the game's **own** matchmaking flow, not generic Steam
session traffic; (b) with solo-fabricate also failing deeper, the one untested cheap combination was
**fabricate + peer** (array sized while a real peer/lobby context exists), run the same day:

**Combo result (2026-07-03, same rig↔Deck pair, fabricate + linked peer) still fails; the static read
below narrows the blocker to the finalize-handle gate.** Same procedure with `fabricate_slot_array =
true` + `drive_fire_solo = false` on both machines: linked, drive fired post-settle, `legb-entry …
cap=0`, then `fabricate-slot-array — sized empty array … capacity(+0x20)=16`, and create still
`returned false → None->FailedToCreateSession`, symmetric on host and joiner. Every previously charted
gate was clear at the moment of failure (leg-A bypassed, reject #1 forced, rejects #2/#3 and gate 4
passed, slot array sized, real peer + live lobby present), so the cheap-combination space is exhausted.
The next confirmation is the finalize-handle probe below, with the game's own match setup as the model
(protocol reference: vswarte's `waygate-server`), or ERSC-style session neutralization as the fallback.

### Leg B Post-Capacity Tail Charted: Finalize Handle, Not a Later Store Reject

> **RIG RESULT (2026-07-03) — the finalize-handle hypothesis below is DISPROVEN: leg B never reaches
> its finalize tail.** Wired both the fires-always finalize-result hook (`0x1423f5cb5`, right after
> `call 0x1423fab40`) and the failure-only cleanup hook (`0x1423f5cd2`), ran the drive (solo-fabricate
> and fabricate+peer). **Neither hook ever fired**, though both installed (prologue-verified). The
> observed sequence is `legb-entry REACHED (cap=0)` → `fabricate (cap=16)` → `create-gate4 REACHED` →
> `drive-create returned false` — with **no** `legb-finhandle`/`legb-finalize`. So on the failure path
> leg B exits **before** its finalize/store tail (`0x1423f5cb0..`), i.e. the whole tail chart below —
> finalize handle, registry-id counter, slot store — is **off the executed path** and cannot be the
> blocker. Meanwhile `create-gate4` (`0x1423fd7a0`) *is* reached and, by its logged fields
> (`+0x3b0=35000`, `+0x3b4=5000`, helper `[6,30000,…]` all nonzero), should **pass** its charted veto.
> **New model:** the real reject is a branch **between `create-gate4` and the finalize call** —
> either `create-gate4` returns false for a reason beyond the two charted fields, or a separate check
> right after it sends leg B to an early failure exit. The static chart below is kept as the (partial,
> now-superseded) tail anatomy, not the failure explanation.
>
> **Static follow-up (2026-07-03, same image) — the veto is create-gate4's HELPER `0x1423faf60`.**
> Disassembling leg B: the branch that gates the finalize tail is `0x1423f5c8f: call [rdx+8]`
> (= `create-gate4` `0x1423fd7a0`) → `0x1423f5c92: test al,al` → `0x1423f5c94: jne 0x1423f5cab`. The
> finalize path (`0x1423f5cab`, incl. `0x1423f5cb5`/finhandle) is reached **only when gate4 returns
> nonzero**. finhandle never fired ⇒ **gate4 returned 0**. Disassembling gate4 (`0x1423fd7a0`): it
> early-returns false only when **both** `[rcx+0x3b0]==0 && [rcx+0x3b4]==0`; ours are `35000`/`5000`,
> so it skips that and `call 0x1423faf60` (the helper) → `test al,al; je ret-false`. So the helper
> `0x1423faf60` is returning 0 (or gate4's tail past `0x1423fd7cc` vetoes). The helper's `+0x68..0x78`
> fields (`[6,30000,…]`, all nonzero) are **not** the whole story — it fails for a reason past them.
> **DONE (2026-07-03): see "create-gate4 Helper `0x1423faf60` Charted" below.** The helper's return-0 is
> its first predicate past the five `[6,30000,…]` config-field checks: an **Arxan cookie-encoded vmethod**
> (`[[session_obj+0x58]+8]` = container-vtable `0x1431f8360` slot `+8` → trampoline `0x14251c480`) whose
> real target is not statically decodable; every other return-0 in the helper is allocation-shaped. A
> runtime probe reading that vmethod's `al` (Hook B) + the helper's return at gate4 (Hook A) is specced
> there. This is the true rung-3 create blocker, not the finalize handle.

Static re-read of leg B (`0x1423f5c00`, same 2026-06-02 image) corrects the "post-store reject" wording
above: there is **no reject after a successful slot-array store**. Once the tail executes
`base[count] = session_obj; count++` at `0x1423f5cc9..0x1423f5ccd`, it jumps straight to unlock and
returns the value already in `esi`. So if the rig logs `fabricate-slot-array` and the wrapper still gets
`eax=0`, the decisive branch is one of the two checks that can reach the cleanup block at `0x1423f5cd2`
**before** the store:

- `0x1423f5cb0` calls finalize helper `0x1423fab40(session_obj, mode)`.
- `0x1423f5cb5` copies the helper's return to `esi`; `0x1423f5cb7` tests it.
- `0x1423f5cb9` branches to cleanup if the helper returned `0`.
- Only if that value is nonzero does leg B run the already-charted capacity check
  (`0x1423f5cbb..0x1423f5cc1`) and store.

The capacity check is now instrumented and fabricated, so the remaining likely zero producer is
`0x1423fab40`, not a later post-store call. The helper is not merely "allocation succeeded": it calls
the generic session-object registry helper `0x1423fa1b0` and returns a dword from the backing node behind
the registry entry (`entry+0x30 -> node+0x10`). If the registry entry is null, it returns `0`; if the
entry exists but the node's `+0x10` id is `0`, it also returns `0`, and leg B treats that as create
failure.

The path that can manufacture a zero id is visible in the registry helper:

- `0x1423fa1b0` creates a registry entry via the session object's `+0xd8` vmethod
  (`0x1423fdfa0`), then creates/allocates the backing node with `0x1423fa100`.
- `0x1423fa100` uses a counter at `container+0x48` as the new node id, then increments the counter.
  Here `container = session_obj+0x58+0x670`; since `session_obj+0x58` is copied from
  `NetworkSession+0x08` by the session-object constructor (`0x1423fd300`), this counter is
  `[[NetworkSession+0x08]+0x6b8]`.
- The node constructor `0x1423f7290` stores that id at `node+0x10`. If the counter starts at `0`, the
  first node's id is `0`; `0x1423fab40` returns that `0`; leg B jumps to cleanup at `0x1423f5cd2`; the
  slot-array store never happens even though capacity has been fabricated.
- If the counter starts at `-1`, `0x1423fa100` first normalizes it to `1`, so the first node id is
  nonzero. That makes `0x1423f5cb7` pass and the fabricated slot array should let the store succeed.

So the concrete reject site to confirm live is **`0x1423f5cb7` in leg B**, testing the finalize handle
returned by **`0x1423fab40`**. The likely field behind that zero handle is the registry-node id counter
at **`[[NetworkSession+0x08]+0x6b8]`** being `0` when create is driven outside the game's own match setup.
That is distinct from the slot array at `NetworkSession+0x18/+0x20/+0x24`: fabrication clears capacity,
but it does not seed the registry id space.

**Proposed runtime probe (do not need the rig worker to wire this):** add one more `gate-trace` hook next
to `legb-entry`/`create-gate4`, then run one fabricate+peer rig cycle. Preferred hook:
**`0x1423f5cb5`** (right after `call 0x1423fab40`, bytes
`8B F0 85 C0 74 17 8B 43 24 3B 43 20 73 0F`). In the callback:

- read `rbx` as `NetworkSession`, `rdi` as `session_obj`, and `eax` as the finalize handle before the
  original `mov esi,eax`;
- read slot `cap/count` from `NetworkSession+0x20/+0x24`;
- read `sub = *(session_obj+0x58)` and, if non-null, `post_finalize_next_id = *(sub+0x6b8)` after the
  helper returns;
- log one line like
  `session-probe: gate-trace legb-finalize handle=<eax> post-next-id=<post_finalize_next_id> cap=<cap> count=<count>`.

That `post-next-id` value is post-consumption: if the failing id was created from a pre-increment counter
of `0`, the post-finalize read is expected to show `1`. For direct proof of the consumed id, add a second
temporary hook inside `0x1423fa100` around the node construction path and log the id passed into
`0x1423f7290` / written to `node+0x10`.

If the hook installer dislikes relocating that short in-function branch window, use the cleanup target
**`0x1423f5cd2`** instead (bytes `48 8B 07 48 8B CF FF 50 10 48 8B 43 08`). That hook fires only on
failure, but it still distinguishes the cause: `esi==0` means the finalize handle failed at
`0x1423f5cb7`; `esi!=0` means finalize passed and the cleanup came from the capacity branch.

Expected outcomes:

- `handle=0`, `post-next-id=1`, `cap=16`, `count=0` with fabricate armed confirms the likely zero-id
  model by inference: cleanup is entered from `0x1423f5cb7`, before the store, after consuming id `0`.
- `handle!=0`, `count>=cap` would mean the capacity fabrication did not land on the exact object leg B
  uses, or the count changed before the tail check.
- `handle!=0`, `count<cap`, followed by wrapper `false` would contradict this static chart, because the
  only path after the store returns that nonzero handle; in that case hook `0x140cb2083` (the caller's
  `mov [CSSessionManager+0x24],eax`) and log the actual leg-B return in the wrapper.

Re-derive after a game update: find leg B by the `NetworkSession` vtable slot `[+0x08]`, then locate the
unique tail region around the finalize helper call, the finalize-result test, the
`NetworkSession+0x24`/`+0x20` capacity check, and the slot-count increment. The cleanup target is the
block that calls `session_obj->vtable[0x10]`, returns the session object to
`[NetworkSession+0x08]+0x48`, then zeroes `esi`.

### create-gate4 Helper `0x1423faf60` Charted: the Veto Is an Arxan-Encoded Vmethod

> **RIG CONFIRMED (2026-07-03, solo-fabricate in-world) — the Arxan vmethod IS the veto.** Wired Hook A
> (`0x1423fd7c8`, helper return read at gate4) and Hook B (`0x1423fafcc`, the encoded vmethod's `al`).
> Both installed (prologue-verified). The drive logged, in order: `create-gate4 REACHED` (fields
> 35000/5000/[6,30000,…] all pass) → **`gate4-vmethod … returned al=0`** → `gate4-helper-ret … al=0` →
> `drive-create returned false`, with no `legb-finhandle`. So the sole in-world rung-3 create veto is
> the Arxan cookie-encoded vmethod at `[[session_obj+0x58]+8]` returning 0; helper `0x1423faf60`, gate4
> `0x1423fd7a0`, and leg B's whole finalize/slot tail are all just downstream of it. The blocker is now
> localized to one encoded predicate. **Next: L3** — capture the vmethod's decoded target at runtime
> (via the trampoline `0x14251c480` heal) and identify what it reads, so we can seed its input rather
> than patch-bypass (L1/L2, which risk a malformed session).

### L3 RESOLVED (2026-07-03, rig) — the veto is bit 2 of `[container+0x7c0]`; the "Arxan vmethod" was a live-vtable mismatch

Capturing the vmethod's real target at the **helper call site** (hook at `0x1423fafc4`, reading `rax` =
the live container vtable and `[rax+8]`) showed the static chart followed the **wrong vtable**: the live
container's vtable is **`0x1431f8780`**, not the static `0x1431f8360`. Slot `+8` of the *live* vtable is
a **direct function `0x1423f4330`** — no Arxan trampoline on the real path (the `0x14251c480`
cookie-decode trampoline the static vtable pointed to is never executed for this call; a hook on its
decode path never fired). So "statically undecodable Arxan vmethod" was an artifact of reading a
sibling/base vtable, not the live one. *(Lesson: for a vmethod, capture `[live_vtable+slot]` at the call
site — don't trust a statically-guessed vtable address.)*

Disassembling the real vmethod `0x1423f4330`, its **first predicate is the veto**:

```
0x1423f434b: mov eax,[rcx+0x7c0]   ; rcx = container = [session_obj+0x58]
0x1423f4354: shr eax,2
0x1423f4357: test al,1              ; bit 2 of [container+0x7c0]
0x1423f4359: je  0x1423f4390        ; clear -> xor al,al -> return FALSE (create veto)
```

**Rig-confirmed:** `veto-field — container=0x143dcd360 [+0x7c0]=0x0 bit2=0` → the field is **0 offline**,
bit 2 clear, so the vmethod returns false at its first branch and create fails. This is the true,
minimal rung-3 create root cause: **bit 2 of the dword at `[[session_obj+0x58]+0x7c0]` is clear when we
drive create offline.**

**The lever (next):** set bit 2 of `[container+0x7c0]` before/at the create call (a two-instruction
field write, like `fabricate_slot_array`, keyed off the live container the veto vmethod reads) and see
whether the vmethod passes and create walks toward `Host`. Caveats: the bit-2-set path in `0x1423f4330`
does further work and *can* still return false (there may be a second predicate past `0x1423f435b`), and
we don't yet know what that bit normally *means* (some "session/host capability enabled" state a real
match sets) — so a naive set may yield a malformed session. But it's a precise, cheap next probe, and
`0x7c0`/bit 2 is now the exact thing to chart upstream (who sets it, and to what) for a clean seed. The
probes that found this (`vmethod-target`, `veto-field`) are committed gate-traces under `drive_create`.

**Lever tested (2026-07-03, rig, `set_create_veto_bit`) — bit 2 IS the gate, but it's a consequence
flag, not a switch.** Writing bit 2 set on the live container before the vmethod's read flipped it
exactly as predicted: `gate4-vmethod … returned al=1` (was `al=0`) — so bit 2 of `[container+0x7c0]`
is definitively the create gate. But create then **crashed**: `crashdump: ACCESS_VIOLATION … write
0x8 at eldenring.exe+0x1eba1c5`, which disassembles to `lock xadd [rcx],eax` (an interlocked refcount
increment) with `rcx=8` — i.e. `[null+8]`. So passing the vmethod made create skip the setup that
**allocates** the object bit 2 vouches for, then it incremented that (null) object's refcount and
faulted. **Conclusion: bit 2 is set as a *result* of real session/match setup allocating its state;
forcing the bit without that state yields a null-deref, not a session.** The clean path is therefore
*not* "set the bit" but **chart who sets `[container+0x7c0]` bit 2 and what allocation it accompanies**
(the object whose refcount is bumped at `+0x1eba1c5`), then reproduce that setup — the same "the game's
own match setup does the allocation" conclusion the slot-array capacity hunt reached, now pinned to a
specific bit + object. (`set_create_veto_bit` stays committed but **off** — it crashes.) Next session:
trace the writer of `[container+0x7c0]` (a `[debug.probes]` write-watch on `container+0x7c0`, or find
the store statically) and the allocation it gates.

**Crash chain captured (2026-07-03, crashdump stack backtrace).** Enhanced `crashdump` to scan the
stack for in-image return addresses; the lever crash's chain (add `0x140000000`) is
`create wrapper 0x140cad4c0` → `leg B 0x1423f5c92` → `gate4 helper 0x1423fd7c8` → helper cont.
`0x1423fb0b6` → **constructor `0x1423f3230`** → the refcount helper (`lock xadd [null+8]`). Disassembling
`0x1423f3230`: it's a constructor `(rcx=new_obj, rdx=sub_obj, r8=…)` that vtables/zeroes a large object
(vtables `0x1431f85c0`/`0x1431f85d8`; a 256-count init loop at `+0xc0`), then at `0x1423f3325`
`lea rcx,[rdi+8]; call refcount; mov [rbx+0x18],rdi` — i.e. it **stores its 2nd arg `rdx` into
`new_obj+0x18` and refcounts it**, and `rdx` is **null offline**. So bit 2 vouches for a session
**sub-object** (passed as `rdx` to `0x1423f3230`, stored at `+0x18`) that create never allocated
offline; forcing the bit reaches this ctor with a null sub-object and faults on its refcount. This is
the concrete shape of "the game's own match setup allocates the state": there is an object graph, not a
single flag. **Path to finish:** find the *one upstream condition* whose being-set makes the normal
flow both set `[container+0x7c0]` bit 2 **and** allocate this sub-object graph (candidate: a "session
subsystem online/available" gate the boot online-flow sets), so flipping it runs the game's own
allocation — vs. hand-seeding each null object (whack-a-mole, unbounded). Charting the writer of
`[container+0x7c0]` bit 2 + the allocator of the `rdx` sub-object is the next step.

**Sharpened (2026-07-03): the null `rdx` is `[container+0x708]` — a sibling field on the SAME
container.** Tracing the ctor's args in helper `0x1423faf60`: `0x1423fb0a7: mov rdx,[rcx+0x708]` with
`rcx = [session_obj+0x58]` = the container. (The `0x10c0`-byte alloc at `0x1423fb08a` is the *new*
object `rcx`; `rdx` is read from `[container+0x708]`, and it's null offline — the ctor `0x1423f3230`
then stores it at `new_obj+0x18` and refcounts it → the `[null+8]` fault.) So **both** the create veto
(`[container+0x7c0]` bit 2, clear offline) **and** the null sub-object (`[container+0x708]`, null
offline) are fields on the *one* container object (vtable `0x1431f8780`, live at ~`0x143dcdxxx` near
`NetworkSession 0x143dcdb80`). That is strong evidence for a **single container session-init** that
populates both — so the finish reduces to: **who initializes the container's `+0x708`/`+0x7c0`, and is
that init reachable/flippable offline** (a `is_offline`/online-availability gate → flip it; or the
game only runs it once a real Steam match forms → then the synthetic drive can't finish and the path
is the game's own create via un-greyed items). This is the one question both "finish" paths reduce to;
`worker/create-veto-writer` is charting it. `+0x708` pointer-writers found by the disp scan (candidates,
none in the container's own `0x1423fxxxx` cluster, so an external session-setup writer):
`0x1412799a0`, `0x141ab2560`, `0x141ab5f10` — to be resolved to the container type.

### MILESTONE (2026-07-03): driven create now returns true → `TryToCreateSession` — the veto is fully clearable

Seeding **both** container fields at the vmethod entry (`set_create_veto_bit`: set `[container+0x7c0]`
bit 2, **and** fabricate `[container+0x708]` = a leaked 0x800-byte buffer with refcount=1) got the driven
create to **succeed for the first time**:

```
LEVER set bit2 + fabricated [container+0x708]
 → gate4-vmethod al=1                         (veto passed)
 → legb-finhandle handle=1 post-next-id=2 cap=16 count=0   (leg B reaches its finalize tail, handle nonzero)
 → drive-create returned TRUE — lobby_state None -> TryToCreateSession   (FSM ADVANCED — never before)
```

So the entire create-veto chain (leg A, rejects #1-3, gate 4, the `[container+0x7c0]` bit-2 vmethod,
the finalize handle, the slot store) is **satisfiable**, and the FSM leaves `None`. This is the core of
rung-3 create: **the create call is no longer vetoed.**

**But a *functional* session (→ `Host`) needs the game's real session-object graph — fabrication can't
supply it.** Immediately after `TryToCreateSession`, session establishment crashes at
`0x14203f1f0` (`read [null+8]`), and the caller `0x1423f6c00` is a **vtable-dispatch loop over a
collection** (`cmp ebp,[rsi+0x24]; jb …` calling `[elem]`/`[elem+0x10]`/`[elem+0x68]` per element): the
session machinery iterates its members/sessions and calls their vmethods, and our hollow fabricated
objects have null/garbage entries. Seeding one field just moves the crash one collection deeper — the
running session expects a coherent graph of real objects, not stubs.

**Verdict (definitive): the finish is NOT synthetic fabrication — it is making the game allocate its own
real session state.** And that state is gated on a **dormant-subsystem "session available" signal that
is NOT `is_offline`**: `enable_offline_multiplayer` already neutralizes `is_offline()` (it's on for every
drive) yet the container stays uninitialized. So the container init is gated on the **same elusive,
not-yet-found signal the item-grey hunt hit** (docs/OFFLINE-ITEMS-FINDINGS.md: three static candidate
families rig-eliminated; "a signal none of the static passes found"). **This unifies the two open
problems:** the multiplayer-item grey gate and the rung-3 container-init gate are almost certainly the
same "is the session subsystem live offline?" signal. Cracking it (the runtime execution trace the
item-grey doc calls for) both un-greys the items **and** lets the game set up the real session state so a
driven-or-item-triggered create reaches `Host`. That trace is now the single highest-value RE task; the
create side is otherwise fully charted and satisfiable. (`set_create_veto_bit` stays committed but off —
it crashes past `TryToCreateSession`.)

> **Static result (2026-07-03, same 2026-06-02 image).** Fully disassembled the helper
> `0x1423faf60` and gate4's tail past `0x1423fd7cc`. **The in-world return-0 is the helper's very
> first predicate past the five config-field checks: an Arxan cookie-encoded vmethod call whose real
> target is not statically decodable.** Everything else in the helper that can return 0 is
> allocation-shaped (OOM only), so with the config fields populated the vmethod is the sole plausible
> in-world veto. gate4's own tail past the helper is charted below too, but it runs *only* if the
> helper already returned nonzero, so it is off the failing path. **Neither the helper nor gate4 reads
> any offline-landscape global** (`mode_enum 0x143d87220`, `is_offline 0x140e55180`, `net_status
> 0x143b400bc`, `svc_singleton 0x144842d40`, `hash_mod 0x144842d28`) — verified by rip-ref scan — so
> the create blocker is **independent** of the item-grey offline signal, confirming the earlier
> correction that they are separate.

**Object identities (re-derived, all facts).** gate4 (`0x1423fd7a0`) and the helper (`0x1423faf60`)
both take `rcx` = the freshly-built `0x5f8`-byte **session object** (vtable `0x1431fa248`, slot `+8`
of which *is* gate4 — confirmed by reading the vtable). Its `+0x58` holds a pointer copied by the
session-object ctor (`0x1423fd300` → base ctor `0x1423fa320`, `mov [this+0x58], rdx`) from
`[NetworkSession+0x08]`. Following that chain: the session object is allocated in `0x1423f7070`
(`ecx=0x5f8`, a `SessionManagerSteam` vtable method) with `rdx=[this+0x08]` where `this` = the
`SessionManagerSteam` (= `NetworkSession`) itself. That `NetworkSession` lives at `container+0x710`,
built by the container ctor `0x1423f20b0`; its base ctor `0x1423f5b60` opens with `mov
[NetworkSession+0x08], container`. **So `session_obj+0x58 = [NetworkSession+0x08] = the container`**
(vtable `0x1431f8360`, set at `0x1423f20d7`). That is the object the helper's first vmethod dispatches
on.

**Helper `0x1423faf60` return-0 map** (every path that lands `al=0`; `rdi` = session object):

1. **Five config-field checks** `cmp [rdi+0x68/0x6c/0x70/0x74/0x78],0; je 0x1423fb1cb` — in-world these
   are `[6,30000,30000,30000,30000]`, all nonzero, so **all five are skipped**. (These are the
   `[6,30000,…]` fields the prior note flagged as "not the blocker" — confirmed here as the *first*,
   not the only, gate.)
2. **The decisive vmethod** at `0x1423fafc9`: `mov rcx,[rdi+0x58]` (the container) → `mov rax,[rcx]`
   (its vtable `0x1431f8360`) → `call [rax+8]` with `rdx = &local[rsp+0x40]` (a zeroed 16-byte
   out-slot). `0x1423fafcc: test al,al; je 0x1423fb1cb` → **`al==0` returns 0.** The vtable slot
   resolves to `0x14251c480`, which is **not a normal function**: it does `lock cmpxchg
   [0x1448577d8], 0` to read an encoded pointer, `xor`/`ror`-decodes it against the security cookie
   `0x143c5adb0`, and `call`s the decoded target (fail-fast `int3` via `0x142548670` if the decode is
   0). That is Arxan's cookie-encoded-pointer call gate; the encoded pointer is healed at runtime by
   `0x14251c4dc`/`0x14251f38e` (the only other refs to `0x1448577d8`). **The real predicate is
   therefore virtualized/undecodable statically** — same class of protection as leg A's gate
   `0x140cb4b50`, and the same reason we must chart it behaviorally and read its boolean at runtime.
   Behaviorally it produces a 64-bit value in the out-slot that the helper immediately consumes as an
   identity/key: `0x141ed60d0` returns the constant divisor `0x2710` (10000), `rax=[rsp+0x40]; div
   rcx`, and the quotient is stored at `obj+0x548`. A "get verified host identity / session key"
   call that has nothing to return offline fits this shape exactly.
3. **Three vector-reserves** `0x1423fd110(&obj+0x4e8/+0x508/+0x528, count=[obj+0x68])` each `test al,al;
   je 0x1423fb1c2` — `0x1423fd110` is a `std::vector` grow-to-capacity helper; it returns 0 only on
   allocation failure. Not an offline signal.
4. **Per-slot build loop** (`count`=6 iterations): two allocations (`0x141eb9ed0` sizes `0x10c0`,
   `0x20`) each null-check to cleanup, then `call [obj_vtable+0xd0]` (= `0x1423fdf20`, itself just an
   `alloc 0x170 + construct` returning null only on OOM) with `test rax,rax; je 0x1423fb166`. All
   three exits are allocation-shaped. If the loop completes, `0x1423fb1b8: mov al,1` → **return 1**.

So with the `[6,30000,…]` fields populated and memory available, **the only path to `al=0` is step 2's
Arxan vmethod returning false.** That is the true rung-3 create veto.

**gate4 `0x1423fd7a0` tail (past `0x1423fd7cc`), charted for completeness** — reached only if the
helper already returned nonzero, so *not* on the failing path, but its own return-0 set is: (a) the
0x528-object allocation `mov r14,[container+0x48]; call [[r14]+0x50]` (an allocator/manager sub-object's
vmethod, not the container's own vtable) → `je 0x1423fd95a` (null alloc → false); then a 32-entry `lock
cmpxchg` init loop; then four boolean setup/register calls
keyed off the singleton accessor `0x1423f8410` (→ `0x1423f4fa0`): (b) `0x1423f84a0`, (c) `0x1423f8420`,
(d) `0x1423f8620`, (e) `0x1424020a0(edx=[rbx+0x68], [rbx+0x258], [rbx+0x260])` — each `test al,al; je`
to the cleanup at `0x1423fd951` (calls `[obj_vtable+0x10]`, returns false at `0x1423fd95a`). Only if
all pass does `0x1423fd9c8: mov al,1` return true. These are map-insert/registration operations (lock +
insert into the manager's internal collections at `+0x80/+0xa0`), i.e. dedup/allocation-shaped, not an
obvious offline gate — and moot until the helper stops vetoing.

**Runtime probe spec** (wire into `crates/unseamless-coop/src/session_probe.rs`, modeled on
`log_create_gate4`/`log_legb_finhandle`; both are read-only mid-function hooks, so guard on the
charted prologue bytes and skip on mismatch):

- **Hook A — helper return, read at gate4** (`0x1423fd7c8`, the `test al,al` right after gate4's `call
  0x1423faf60`). Prologue guard `84 C0 74 EF 48 8B 43 58 BA 28` (`test al,al` / `je 0x1423fd7bb` /
  `mov rax,[rbx+0x58]` / `mov edx,0x28…`). In the detour read `al = rax as u8` = **the helper's
  return**; `rbx` = the session object (log its `+0x548` id and the five `+0x68..0x78` fields for
  context). `al==0` confirms the helper vetoed (expected); `al!=0` would move the veto into gate4's
  tail (checks a–e above).
- **Hook B — the decisive vmethod result, inside the helper** (`0x1423fafcc`, the `test al,al` right
  after `call [container_vtable+8]`). Prologue guard `84 C0 0F 84 F7 01 00 00 48 8D` (`test al,al` /
  `je 0x1423fb1cb` rel32 / `lea …`). Read `al = rax as u8` = **the Arxan vmethod's verdict**. This is
  the money datum: `al==0` proves the encoded vmethod is the veto (and localizes it precisely,
  distinguishing it from the reserves/loop). Optionally also read the out-slot the vmethod filled at
  `rsp+0x40` (frame-relative; best-effort) to see whether it returned a zero identity.

Run one solo-fabricate and one fabricate+peer cycle. Expected: `create-gate4 REACHED` → Hook B
`vmethod al=0` → Hook A `helper ret=0` → `drive-create false`, with no `legb-finhandle` (already
confirmed leg B never reaches finalize). That nails the veto to the encoded vmethod.

**Candidate levers** (all flagged risky — this gate does *real* session setup, so bypassing it likely
yields a **malformed** session, not a working one):

- **L1 — force the helper past the vmethod** (patch `0x1423fafcd`: the `je 0x1423fb1cb` `0F 84 F7 01
  00 00` → six `90` NOPs). The helper then ignores the vmethod verdict and proceeds to the div + slot
  build. **High malformation risk:** the out-slot the vmethod was supposed to fill stays zero, so
  `obj+0x548` (the session key/id) is 0; downstream code that keys on it may reject or corrupt the
  session. Only worth trying to *observe* whether create then advances, not as a shipping fix.
- **L2 — force gate4 true** (patch `0x1423fd7ca`: `je 0x1423fd7bb` `74 EF` → `90 90`, so gate4 falls
  into its tail regardless of `al`; or short-circuit gate4 to `mov al,1; ret`). Analogous to leg A's
  `bypass_session_create_gate`. **Same malformation risk, worse:** skipping the helper entirely means
  none of the per-slot substructures at `obj+0x4e8/+0x508/+0x528` get built. Diagnostic only.
- **L3 (preferred, non-destructive) — don't patch; identify what the vmethod reads and seed it.** The
  vmethod is Arxan-encoded, so its predicate is only visible at runtime. Once Hook B confirms `al=0`,
  the next step is an in-execution capture of the decoded target (single-step from the trampoline
  `call [decoded]` at `0x14251c4b2`, or a Frida `Interceptor` on the resolved address) to learn which
  global/service it queries — then seed *that* input (the way we seed the slot array / registry
  counter) rather than defeating the check. This is the only path likely to yield a *working* session,
  and it stays clean-room (study the game's own precondition, satisfy it).

Re-derive after a game update: gate4 is session-object vtable slot `+8`; its helper is the `call`
between gate4's `[+0x3b0]==0 && [+0x3b4]==0` early-out and its `test al,al; je`. Inside the helper,
the decisive vmethod is the **first `call [ [rdi+0x58] +8 ]`** after the five `cmp [rdi+0x68..0x78],0`
checks; if that slot points at a `lock cmpxchg` + cookie-decode + `call`, it is the Arxan gate — hook
the `test al,al` right after it (`84 C0 0F 84`).

### Tooling / re-derivation

Found with `scripts/re/static.py` (the committed PE workhorse): `fn` to disassemble the inner/builder,
`calls`/`xref` to prove the gate's two callers and the `[0x143b3acd8]` fnptr sites, `.pdata` bounds +
a byte/entropy read to prove the gate is the lone encrypted function in its block. After a game update:
the create inner is the `mov [this+0xc],1` function in the `CSSessionManager` method block
(`0x140cad000..0x140cb3000`); the gate is the **bool-returning call it makes after the `lobby_state`
guards and before the params builder `0x140cb20d0`** — re-take the `call + nop + lea rcx,[rsp+0x30] +
test al,al + jne rel8` as the landmark (the concrete call rel32 keeps it create-specific) and flip the
`75` to `EB`.

**Slot-array pass (2026-07-02).** Leg B was disassembled with `static.py fn 0x1423f5c00` and
decompiled via the persistent Ghidra cache (`GHX_PROJECT_DIR=/var/tmp/ghidra-projects`,
`scripts/re/ghidra-decompile.sh`) to read the tail store's register semantics (rbx = entry rcx). The
`NetworkSession` type + the sibling session-manager vtables came from an RTTI read off the vtable's
`-8` COL/type-descriptor (`0x1431f9140 → .?AVSessionManagerSteam@DLNR3D@@`; base `0x1431f8fe8 →
.?AVSessionManager@DLNR3D@@`). The stub slot-vmethods (`+0x78`/`+0x80`) were read from the vtable +
`static.py fn`. The accessor `0x1423f1930` and its 22 `CSSessionManager` callers came from
`static.py fn`/`calls`. The "no reachable reserve" conclusion is a set of throwaway capstone scans over
`.pdata` (all committed-tool-shaped, kept in `/tmp`): functions writing the `+0x18/+0x20/+0x24` triple,
those referencing `+0x710`, P-relative `+0x728/+0x730` writers, and allocator-fed `+0x18` stores — none
landed on a path a solo create reaches. Re-derive after a game update: re-confirm leg B by the
`mov [rbx+0x24]; cmp [rbx+0x20]; jae` tail (rbx = the vmethod's `this`), and re-read the three slot
offsets `+0x18/+0x20/+0x24` off that object.

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
