# Co-op Flow via Multiplayer Items — RE Findings

Worker lane `coop-flow`. Goal: chart how the game's **own** co-op flow enters from *using a
multiplayer item* (place a summon sign, reveal signs, summon a phantom) down to the point where it
hands a request to matchmaking — so we can (a) trigger that flow in code out-of-menu and (b) find the
matchmaking handoff to intercept and substitute our own password-peer (the ERSC model: reuse the
game's request/transport machinery, replace the peer-brokering).

> **Scope & legitimacy.** Static RE on our own legitimately-owned **2026-06-02 `eldenring.exe`**
> (image base `0x140000000`; the main exe is not typically relocated, so static VA should equal live VA
> — as prior lanes confirmed on the rig). We study
> how a game we bought behaves so we can reimplement co-op behavior in clean Rust, co-op-only and
> *outside* anti-cheat. Behavioral notes are in my own words; **no decompiler/disassembler output is
> reproduced** (CLAUDE.md > Clean-room hygiene). Addresses are facts about the binary.

## TL;DR

Using a co-op item does **not** stand up a peer-to-peer session. It routes into the **sign / SosDb
subsystem**, which issues a **`FromNet` request to the FromSoft matchmaking server**. The request →
job → server-send portion of the path is charted end-to-end; the item-use → stepper *top* is
task/vtable-dispatched and inferred, not statically traced (see [task 1](#1-the-item--job--server-spine-charted)):

```
item-use / sign interaction (goods handler + SosSignCtrl steppers, task-dispatched — inferred top)
   → SosDb "request X" method            (0x140a173c0 … 0x140a1e4a0 / 0x1401d5xxx)    ┐
      → job factory                       (0x140a138a0 … 0x140a13c93, one per action) │ charted
         → SosSign*Job object             (8 classes, vtables 0x142b3a990 … 0x142b3ab18) │ end-to-end
            → job Execute (vtable slot 3)  builds a FromNet::Request*Params payload      │
               → virtual SEND on the Execute's dispatch context  ◀══ THE MATCHMAKING HANDOFF ┘
                  (summon: mov rax,[r14]; call [rax+0xb0] → call [+0x38] at 0x140a528b8;
                   the context r14 is inferred to be the FromNet request dispatcher singleton 0x143d87350)
```

The **eight matchmaking requests** (each a `SosSign*Job` that sends a `FromNet::Request*Params`) are the
ERSC intercept surface. The outbound *send* is what's charted here statically; the response/notification
leg is not traced in this lane — its shape comes from the SDK and the `../waygate-server` protocol
reference (see [handoff](#2-the-matchmaking-handoff-the-ersc-intercept-point)):

| Action (item) | Job class (vtable) | Factory | Request method | FromNet payload |
|---|---|---|---|---|
| Place white sign (Tarnished's Furled Finger) | `CSSosSignCreateSignJob` `0x142b3a9c8` | `0x140a13930` | `0x140a1e4a0` | `RequestCreateSignParams` |
| Place match-area sign | `CSSosSignCreateMatchAreaSignJob` `0x142b3a990` | `0x140a138a0` | `0x140a1df40` | `RequestCreateMatchAreaSignParams` |
| Reveal signs (Furlcalling Finger Remedy) | `CSSosSignDownloadSignListJob` `0x142b3aa70` | `0x140a13a30` | `0x140a19ce0` | `RequestGetSignListParams` |
| Reveal match-area signs | `CSSosSignDownloadMatchAreaSignListJob` `0x142b3aa38` | `0x140a139a0` | `0x140a173c0` | `RequestGetMatchAreaSignListParams` |
| **Summon a phantom (sign interaction)** | `CSSosSignSummonJob` `0x142b3aaa8` | `0x140a13ba0` | `0x140a1a110` | `RequestSummonSignParams` |
| Reject a summon | `CSSosSignRejectJob` `0x142b3ab18` | `0x140a13ac0` | `0x140a19e70` | `RequestRejectSignParams` |
| Remove a sign | `CSSosSignRemoveSignJob` `0x142b3aae0` | `0x140a13b30` | `0x140a1a020` | `RequestRemoveSignParams` |
| Update a sign | `CSSosSignUpdateSignJob` `0x142b3aa00` | `0x140a13c60` | `0x140a1a2e0` | `RequestUpdateSignParams` |

**Headline answers to the four tasks:**

1. **Where use routes.** Item-use reaches the SosDb "request" methods above through the **SosSignCtrl
   lifecycle steppers** (the `0x1401cf…/0x1401d2…/0x1401d9…` cluster, all **0 static callers** =
   FD4-task-dispatched). The request methods create a job and enqueue it; a stepper on **CSNetMan's
   update task** runs it. The exact *goods-handler → enqueue* link for a specific item is
   task/vtable-dispatched (an honest static limit), but the whole request→job→server-send spine
   *below* the steppers is charted end-to-end.
2. **The standup point & the matchmaking handoff.** The item-use → FromNet request spine charted here
   does **not** create a P2P session — it never calls the `CSSessionManager` create wrapper
   `0x140cad4c0` and never stands up the DLNW3D transport. (The SosDb subsystem *is* separately wired to
   the create wrapper on the downstream host/join legs — SESSION-DRIVE.md names a sign/host create driver
   `0x140a23010` — but that is post-broker, not on the request spine.) The spine reads `CSSessionManager`
   (`G = 0x143d7a4d0`) only for **identity** and issues a **server request** via the job Execute's virtual
   send. **That send is the matchmaking handoff / ERSC intercept point**: the game asks FromSoft's server
   to broker the match. Per SESSION-DRIVE.md's rig-proven finding, the P2P transport (`CSSessionManager`
   Host/Client FSM + the DLNW3D `SteamConnection`) stands up only once a real Steam P2P session is
   brokered (host or join side), which is *downstream* of this request. So this lane charts the **broker
   side** that precedes the create-path the other lanes drove.
3. **Internal gate on a direct trigger.** The request/controller/stepper spine does **not** re-check
   `is_offline()`, `IsEnableOnlineMode()`, the availability gate `0x140cb4b50`, the mode enum, the
   bool-chain, or the service singleton (verified by a call/rip-ref scan of every function in the
   spine — **zero hits**). So **calling the request logic directly would dodge the menu-grey gate.**
   Note this does *not* mean the grey is cosmetic: OFFLINE-ITEMS-FINDINGS.md establishes the grey keys
   on FromSoft **matchmaking reachability** (a real online-availability signal, read in the menu-draw /
   `EquipParamGoods` layer, which this lane did **not** chart). The finding here is narrower — the
   *request spine below the menu* has no such re-gate — so a direct request-layer call skips the UI
   grey. **But it still does not help offline**: the request methods have a light readiness gate
   (`cmp [SosDb+8], 1`), and the send targets the FromSoft matchmaking server, unreachable outside EAC.
   Dodging the grey buys a request with nowhere to go — which is exactly why the ERSC model *intercepts
   the send* rather than triggering the item.
4. **Session model.** This is the **classic guest-phantom summon model**, not seamless. The SDK charts
   it precisely: `SosSignMan.summon_requests` + `join_data: DLList<PhantomJoinData>`, with
   `PhantomJoinData` carrying `state: Waiting→Joining`, `summonee_player_id`, a 55s/180s join timeout,
   and an `apply_multiplayer_rules` flag. Lifecycle-heavy: a guest is warped in
   (`MultiplayAreaWarpStep`) and sent home after the boss. **This is the model seamless-coop
   deliberately does *not* want** — see [Session model](#4-session-model-guest-phantom-not-seamless).

## 1. The item → job → server spine (charted)

### The eight jobs and their factories

The `SosSign*Job` classes are a consecutive vtable family in `.rdata`
(`0x142b3a990 … 0x142b3ab18`); RTTI type names are `.?AVCSSosSign…Job@CS@@` (note the class name
itself carries a `CS` prefix — the RTTI-name search must include it or the substring match offsets the
VA). Each job's vtable shares slots `[0]/[2]/[7]/[9]` (base task/stepper plumbing) and differs at
`[3]/[4]/[5]` — the **Execute / build-request / on-response** logic. Each is built by a tiny factory in
the SosDb module (`0x140a138a0 … 0x140a13c93`), each factory called by exactly one "request" method.
See the TL;DR table for the full map.

### The request methods and their state gate

Each request method allocates the job (`0xb8` bytes for summon), calls the factory to construct it,
refcounts it, and enqueues it for the stepper to run. **Summon request `0x140a1a110`** opens with a
readiness check:

```
cmp dword [SosDb+8], 1     ; SosDb network/sign subsystem state
jne  <error path>          ; if not "ready", call 0x1407a7200 + set error, return WITHOUT a job
```

So `[SosDb+8] == 1` is a required precondition at the request layer — a **light internal state gate**,
distinct from the `is_offline` family (it is a SosDb liveness flag, almost certainly `0` offline since
the sign subsystem never comes up without the matchmaking server). This is the one internal gate a
direct trigger would still meet; it is a single-field state, not the obfuscated online-availability
signal the item-grey/create-veto hunts chase.

The request methods are driven by the **SosSignCtrl lifecycle steppers** — e.g. summon:
`request 0x140a1a110 ← ctrl 0x1401d5b40 ← 0x1401d9570 ← 0x1401d3ae0 ← 0x1406ff190 ← 0x1406fb740 ←
0x140b06bb0` (top = **0 static callers**, FD4-task-dispatched). CreateSign is symmetric
(`request 0x140a1e4a0 ← 0x140a19ca0/0x140a19e30 ← steppers 0x1401d74e0 / 0x1401d8550`, both 0-caller).
Reveal-signs likewise (`0x1401d6c20` / `0x1401d6e30`, which read `G = 0x143d7a4d0` directly). The
steppers run on **CSNetMan's update task** (the SDK's `CSNetMan.update_task`, documented as "pulls in
new data from server, spawn received signs, …"). This is why the SDK note on `SosSignMan.summon_requests`
says inserting there "will not do anything unless you also have data in `join_data`": the item-use
handler *enqueues intent*; the stepper machinery *issues the job*.

### Singletons the spine reads

- **`G = 0x143d7a4d0`** — `CSSessionManager` (the keystone global from SESSION-DRIVE.md; 1446 rip-refs).
  The sign controllers and job Executes read it for **session/player identity** (summon Execute calls
  `0x140caf2a0` → id dword, and `0x140cadd40` → local session identity, both `CSSessionManager` methods
  in the `0x140cad…/caf…` module — the same module as the create wrapper `0x140cad4c0`, but the sign
  path calls the *accessors*, never *create*).
- **`0x143d5ae60`** — the **SosDb** sign-database singleton (986 rip-refs; read across the whole
  `0x1401b…/0x1401d…/0x140a1…` sign cluster). The summon Execute reads `[SosDb+0xa8]+0x70` for the
  sign-request context.
- **`0x143d87350`** — the **FromNet request dispatcher** singleton (returned by accessor `0x1407a72a0`;
  the request methods fetch it and thread it, together with the freshly-built job, into the enqueue
  helpers). It is **inferred** to be the same object the job Execute later dispatches through (the
  Execute's `r14` context — see below); that identity is well-supported by the enqueue threading but is
  not re-proven inside Execute itself. This is **the intercept target**.

## 2. The matchmaking handoff (the ERSC intercept point)

Each job's **Execute (vtable slot 3)** builds a `FromNet::Request*Params` struct and dispatches it via
a **virtual send on its dispatch-context argument** (inferred to be the FromNet request dispatcher
`0x143d87350`). Worked example — **summon Execute `0x140a52650`** (`rcx` = job, `rdx`/`r14` = the
dispatch context):

1. Resolve identity: read `G = 0x143d7a4d0`, call `0x140caf2a0` (→ id), `0x140cadd40` (→ local session
   identity into a local buffer). Assert-guards on the null singletons (`FD4Singleton` source string at
   `0x1429c7aa0`) confirm these are singleton reads.
2. Build the request payload from the job's fields (`job+0x80` / `job+0x88` = the
   `RequestSummonSignParams` body; sign id, sign identifier, target).
3. **Send:** `mov rax,[r14]; call [rax+0xb0]` (get the sender from the dispatch context) → then
   `mov r9,[rax]; call [r9+0x38]` with `rdx = job+0x80` (the request payload) and `r8` = a local
   result slot. **This `call [r9+0x38]` at `0x140a528b8` is the request dispatch** — inferred (from the
   `FromNet::Request*Params` payload naming) to be where the game hands the summon to FromSoft
   matchmaking. `eax` (the send handle/result) is captured into `edi`.

The payloads are the **`FromNet::Request*` family** (in `.rdata`, `0x143086f48 … 0x143087070` for the
eight sign requests): `RequestCreateSignParams`, `RequestCreateMatchAreaSignParams`,
`RequestUpdateSignParams`, `RequestRemoveSignParams`, `RequestGetSignListParams`,
`RequestGetMatchAreaSignListParams`, `RequestSummonSignParams`, `RequestRejectSignParams` (with
blood-message/bloodstain/ghost siblings of the same shape sharing the surrounding block). The `FromNet`
namespace is **FromSoft's online server protocol** — request params up, and (per the SDK / the protocol
reference, not traced statically here) a result/notification back (`SummonSignResultLogParams@FromNet`,
`UseItemLogParams@FromNet`). Vswarte's `waygate-server` (cloned at `../waygate-server`, cross-ref
SESSION-DRIVE.md) reimplements this same protocol on the server side, and is the clean-room reference
for **what each `Request*` carries and what response completes the summon** — read it for the *protocol
shape*, reimplement from that.

**ERSC-model intercept.** The single choke point is the job Execute's send — dispatched through its
`r14` context (inferred to be the FromNet request dispatcher `0x143d87350`) via `[context]→[+0xb0]→
sender`, `[sender+0x38]→send(request)`. Substituting our
own peer-brokering means: instead of letting `[sender+0x38]` reach the FromSoft server, resolve the
password-peer (rung-4 lobby discovery already does this) and satisfy the request locally — e.g. a
`RequestGetSignList` returns *our* peer's sign; a `RequestSummonSign` connects *our* peer. That is the
same "skip the matchmaker, keep the peer-to-peer" premise COOP-CONNECTION.md is built on, now pinned to
a concrete function surface. One dispatcher, eight request types, one send vtable slot.

## 3. Does a direct trigger dodge the gate? (task 3)

**Menu grey — the request spine doesn't re-gate on it.** OFFLINE-ITEMS-FINDINGS.md establishes the
item-grey keys on FromSoft **matchmaking reachability** — a real online-availability signal (read in the
menu-draw / `EquipParamGoods` layer, still unpinned after three static passes), **not** cosmetic. This
lane did **not** chart that menu-layer decision. What it *did* check: the request/job/stepper spine was
scanned for calls to `is_offline()` `0x140e55180`, `IsEnableOnlineMode()` `0x140e56310`, the mode getter
`0x140e0e960`, the availability gate `0x140cb4b50`, the bool-chain (`0x14073cd40`/`0x140e0ec90`/
`0x140e43610`), and rip-refs to their globals (`0x144842d40`, `0x143d87220`, `0x143b400bc`,
`0x143d6a840`, `0x143d87228`, `0x144588afc`) — **zero hits** across all ~30 functions in the spine. So
the *request logic below the menu* does not itself re-consult the online-availability signal;
**calling it directly bypasses the menu-layer grey check** (which lives above the spine).

**But it does not make co-op work offline.** Two reasons a direct trigger still fails outside EAC:
- The request layer's **`[SosDb+8] == 1`** readiness gate (above) — a state the sign subsystem only
  reaches when it is online; likely `0` offline, so summon/create early-return without even building a
  job.
- Even past that, the job **sends to the FromSoft matchmaking server** (`[sender+0x38]`), which is
  unreachable outside EAC. A dodged grey yields a request with no server to answer it.

**So the verdict aligns with the create-path lanes and unifies the finish:** the item path is *not* a
back-door around the offline wall. What it gives us is a **cleaner, higher-level intercept surface**
than the low-level `CSSessionManager`/DLNW3D create the other lanes drove: the FromNet request
dispatcher. The ERSC model doesn't *trigger the item offline* — it lets the (online-enabled) request
fire and **answers the request as the peer** instead of the server. Whether the request layer can be
reached offline at all still turns on the same dormant-subsystem signal as the create-veto/item-grey
(the `[SosDb+8]` readiness and the DLNW3D transport both come up only in the online flow — see
SESSION-DRIVE.md "the whole DLNW3D transport is DORMANT offline"). This lane does not crack that
signal; it charts the request surface that sits on top of it.

## 4. Session model: guest-phantom, not seamless (task 4)

The item/sign path builds the **classic summon session**, charted exactly in the SDK
(`cs/sos_sign_man.rs`):

- **`SosSignMan.signs: DLMap<i32, SosSignData>`** — placed signs. `SosSignData` carries
  `sign_identifier: ObjectIdentifier` (**server-assigned**), `steam_id`, `multiplay_type`,
  `from_group_password`, and `apply_multiplayer_rules` ("check if the player is allowed to see signs in
  the area … for the summoning frame check").
- **`SosSignMan.summon_requests: DLList<i32>` + `join_data: DLList<PhantomJoinData>`** — the summon
  pipeline. `PhantomJoinData` is a **join push-notification** with `state: Waiting(0) → Joining(1)`, a
  **55s Waiting / 180s Joining timeout**, `summonee_player_id` ("Player id of the sign owner **from the
  server**"), `summon_job_error_code`, and the physics-space `pos`/`rotation`/`block_id` the phantom is
  warped to.
- **`MultiplayAreaWarpStep`** (FD4 reflection string at `0x142b9a7e0`; MSVC RTTI near `0x143cfb85c`) —
  the guest is **warped into the host's world**;
  the guest is a visitor in someone else's session, sent home after the objective (boss). Covenant/roster
  overrides on `SosSignMan` (`override_map_guardian_count`, `override_normal_white_count`, …) confirm
  the phantom-count / summon-limit machinery of the classic model.

**Implication for seamless-coop.** This is the lifecycle-heavy **guest-phantom** model: server-assigned
sign identifiers, a warp-in / send-home lifecycle, phantom caps, area-scoped summon rules. Seamless-coop
deliberately diverges — players **stay hosts of their own synced worlds**, no warp-home, no boss-gated
teardown (ARCHITECTURE.md > Divergences). So the value of this path to us is **not** its session model
(we don't want guest-phantoms) but its **transport reuse + intercept surface**: the FromNet request
dispatcher is where the game brokers *who connects to whom*, and that brokering is exactly what ERSC
replaces with a password-peer while keeping the game's own P2P underneath. We intercept the broker; we
do **not** adopt the summon lifecycle.

## 5. Cross-references & how this fits the create-path work

- **SESSION-DRIVE.md** drove the `CSSessionManager` **create** FSM (host side) and proved the **DLNW3D
  transport is dormant offline**, gated by a flow-entry signal unified with item-grey. This lane charts
  the **layer above** that: the *matchmaking-broker request* (FromNet) an item issues *before* any P2P
  session forms. The two dovetail — the FromNet summon/join is what normally *causes* the DLNW3D
  connection to stand up (server brokers → peers connect). ERSC intercepts the FromNet broker so the
  connection stands up for *our* peer.
- **OFFLINE-ITEMS-FINDINGS.md** — the item-grey signal (keys on matchmaking reachability, read in the
  menu layer). This lane confirms the *request spine below the menu* does not itself re-gate on the
  online signal (so a direct request-layer call skips the menu grey), and that the same dormant-subsystem
  readiness (`[SosDb+8]`, DLNW3D) blocks the path regardless.
- **COOP-CONNECTION.md** rung 3 — the "reuse the game's transport, replace the peer-brokering" plan;
  the FromNet request dispatcher `0x143d87350` is a concrete name for "the peer-brokering" to replace.
- **`../waygate-server`** — clean-room protocol reference for the `FromNet::Request*` payloads and their
  server responses (`message/src/eldenring/{sign,session,matchingticket}.rs`).

## 6. Re-derivation after a game update

- **The eight jobs:** RTTI-search `.?AVCSSosSign…Job@CS@@` (include the `CS` prefix); the type
  descriptors are a consecutive block (`0x143ce4ec8 … 0x143ce5040`), the vtables a consecutive `.rdata`
  family. Slot `[3]` of each vtable is its Execute.
- **The factory family:** rip-refs to each job vtable land in one tiny constructor each, all clustered
  in `0x140a138a0 … 0x140a13c93`; each has exactly one caller = its request method.
- **The request state gate:** in the request method, the `cmp [rcx+8],1; jne` before the `0xb8`-byte
  alloc + factory call.
- **The send / intercept point:** in a job Execute, the tail `mov rax,[r14]; call [rax+0xb0]` (get the
  sender from the dispatch context's vtable) then `call [sender+0x38]` with the `Request*Params` in
  `rdx`. The dispatcher accessor is the 2-instruction getter `0x1407a72a0` (`mov rax,[0x143d87350]; ret`).
- **The FromNet payloads:** ASCII-search `FromNet::Request` — the sign requests are the
  `RequestCreateSign` / `RequestGetSignList` / `RequestSummonSign` / … block at `0x143086f48 …`.
- **The session model:** the SDK `SosSignMan` / `PhantomJoinData` layout (pinned rev `8c67a84`), and
  the `MultiplayAreaWarpStep` FD4 reflection / RTTI strings.

## Tooling / method notes

Found with `scripts/re/static.py` (the committed PE workhorse): `vtable` to resolve the eight job
vtables from RTTI (via COL), `xref`/`calls` to walk vtable→ctor→factory→request→controller→stepper,
`fn` to disassemble the summon request + Execute and read the send vtable-dispatch. Two reusable gotchas:

- **RTTI substring offset:** `find_ascii("SosSignSummonJob@CS@@")` matches *inside* the real name
  `.?AVCSSosSignSummonJob@CS@@`, so the returned VA is mid-string and `name−0x10` misses the type
  descriptor. Walk back to the preceding NUL (the `.?AV` start) before computing `td = name_start − 0x10`.
- **0-static-caller = task-dispatched:** the SosSignCtrl steppers and the job Executes have no `E8`
  callers because the FD4 stepper/task framework invokes them via vtable. Trace *down* from them
  (they're reached from the CSNetMan update task), not *up* by call-xref.
