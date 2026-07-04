# Session lifecycle / leave-teardown findings (static)

A **static** pass over the pinned **2026-06-02 `eldenring.exe`** (size 86,998,096; image base
`0x140000000`, loaded at its preferred base so static VA == live VA) answering one question: **is
Elden Ring's co-op session leave/teardown guardable at a single early "chokepoint"?** i.e. can
seamless-coop suppress game-driven disconnects (boss defeat, area transition, player death) with
**one armed flag**, instead of hooking every event that can end a session?

No game running; addresses are facts about the binary; all behavior is in my own words (CLEAN-ROOM,
CLAUDE.md > Clean-room — no decompiler/disassembler output transcribed). Every result has a
re-derivation recipe at the bottom so a game update can be re-charted fast.

> **Scope & legitimacy.** Interoperability RE on a game we own, on the developer's own machine, to
> reimplement a co-op mod that loads *outside* anti-cheat. We study *what* the game does at session
> teardown, then reimplement around it. See CLAUDE.md > Safety / legitimacy.

## TL;DR — the verdict

**Yes (pending one rig confirmation — see Risk #1), a single armed-flag gate is viable, and the
chokepoint is the FSM leave-transition, not the low-level transport teardown.** All game-driven
session leaves converge on **one primitive**: `leave_session = 0x140cae730` ("begin leaving the
session"). It is the *only out-of-line function* that writes `lobby_state = OnLeaveSession(7)` and
kicks off the transport-side session close. **24 distinct external functions call it** (plus the
update task; 25 distinct enclosing functions in total), and these include game-logic and
packet-handler paths — boss/area/death/host-migration/remote-leave-packet all funnel here. There is
exactly **one** second site that does the same work — an **inlined copy** of `leave_session`'s body
inside the CSSessionManager per-frame update task (`update_step = 0x140cafd10`, the inline at
`0x140cb08bc`), which handles the game's *self-initiated* leaves (last peer gone, network status
polled low, received-leave-packet).

So the picture is **"many sources, two code sites, one logical chokepoint"**: teardown *decisions*
start in ~25 places, but the actual leave transition happens in only those two spots, and they share
identical structure. An armed flag that early-returns from `leave_session` (and the twin inline)
**before** the `lobby_state = 7` write suppresses every game-driven disconnect at once.

**The catch (state it plainly):** this gate covers *our-side, FSM-driven* leaves only. When a peer
**genuinely drops**, the DLNW3D transport raises an async connection-down event and the DLNR3D session
layer reacts by dispatching the teardown handler `container.slot+0x70 = 0x1423f46d0` **without ever
touching `leave_session`** — so that teardown is *not* suppressed by this gate. (Note the distinction
from `update_step`'s *polled* "network status low" branch, which *does* route through `leave_session`
and *is* gated: the polled FSM reaction is a source we gate; the async transport disconnect is not.)
For seamless-coop that is arguably the desired split (stay connected through boss/area/death; still
tear down if a peer genuinely drops), but it means the chokepoint is **not** a universal "never
disconnect" switch. Gate the **source** (`leave_session`), never the low-level **teardown handler**
(`0x1423f46d0`): by the time that handler runs, teardown is committed and its cleanup is load-bearing.

## The three layers of "leaving"

Leaving a co-op session touches three distinct layers. Naming them keeps the chokepoint unambiguous:

```
  (SOURCE — game logic decides to leave)                       ← GATE HERE
    24 ext fns  ─┐
                 ├─▶  leave_session = 0x140cae730   ─┐          FSM: writes lobby_state = 7
    update_step ─┘    (+ inlined twin @0x140cb08bc)  │          (CSSessionManager+0xc)
                                                      │
  (DISPATCH — orderly transport close)               ▼
                     SessionManagerSteam.slot3 = 0x1423f64f0    scans slot array, sends close
                     (container+0x710 vtable +0x18)             packet (tag 0x8104000500000000)
                                                      │
                                                      ▼  … network round-trip / socket close …
  (TEARDOWN HANDLER — DLNR3D session layer reacts to actual disconnect)
                     container.slot+0x70 = 0x1423f46d0          unregisters handlers, destroys
                     (ManagerImplSteam@DLNR3D vtable)           node list, clears status bits
                     0 direct callers — dispatched              [container+0x7c0] &= ~0x1e
                     polymorphically on a DLNW3D disconnect
```

Per-peer operations (`kick`, `request_leave`) are a **separate, finer granularity** — "one player
leaves", not "I leave the whole session" — and are charted below but are **not** the boss/area/death
chokepoint.

## Task 1 — every writer of `lobby_state = OnLeaveSession(7)`

`lobby_state` is `CSSessionManager+0xc` (SDK-named; keystone `G = 0x143d7a4d0`, `[G]` = live
manager). Scanned both `.text` sections for immediate stores `mov dword [reg+0xc], imm` (all base
regs, disp8/disp32, ±REX). The full **FSM writer map** inside the CSSessionManager method block
(`0x140cad000..0x140cb3000`) is clean and complete for states 1–7:

| LobbyState | value | writer site | enclosing fn | role |
|---|---|---|---|---|
| TryToCreateSession | 1 | `0x140cb208e` | `0x140cb1f70` | create inner |
| FailedToCreateSession | 2 | `0x140cad4f0` | `0x140cad4c0` | create wrapper |
| Host | 3 | `0x140cb2af9` | `0x140cb2ae0` | create success |
| TryToJoinSession | 4 | `0x140cb25f0` | `0x140cb2470` | join inner |
| FailedToJoinSesion | 5 | `0x140cae68f` | `0x140cae640` | join fail |
| Client | 6 | `0x140cb2fb3` | `0x140cb2f80` | join success |
| **OnLeaveSession** | **7** | **`0x140cae79c`** | **`0x140cae730`** (`leave_session`, **A**) | **whole-session leave** |
| **OnLeaveSession** | **7** | **`0x140cb08bc`** | **`0x140cafd10`** (`update_step`, **B**) | **inlined twin** |
| FailedToLeaveSession | 8 | *(none in block)* | — | see asymmetry note |

**Only two sites write `7`**, and they are the leave chokepoint. (The other seven `imm=7`→`[reg+0xc]`
hits in the image — `0x1409faf7c`, `0x1419d5d13`, `0x1419d6290`, `0x141aa5ac1`, three in
`0x141b95xxx` — are unrelated functions where `+0xc` is some other field; none touch
CSSessionManager.)

**Asymmetry worth flagging: `FailedToLeaveSession(8)` has NO immediate writer in the block** (states
1–7 all do). So the game has no synchronous "failed to leave" fallback via an immediate store here —
`leave_session` never fails-closed to state 8; if the transport close fails it simply returns without
transitioning (state stays whatever it was). Either 8 is written elsewhere / via a register store, or
it is effectively dead in this build. **Rig-verify** with a write-watch on `[G]+0xc` if it matters;
for gating it does not (we suppress before state 7 is ever written).

### `leave_session` (A = `0x140cae730`) — the primitive, charted

`this` = `CSSessionManager*` in `rcx`. Behavior, in order:

1. **Guard on the singleton's state.** Load `[G]`; if `lobby_state ∈ {None(0), FailedToCreate(2),
   FailedToJoin(5)}` → return, do nothing (you're not in a session).
2. **Idempotency guard.** If `[this+0x2c] != 0` (a leave already in flight / result latched) → return.
3. **Defer if mid-handshake.** If `this`'s own `lobby_state ∈ {TryToCreate(1), TryToJoin(4)}` → set a
   **deferred-leave byte** `[this+0x20] = 1` and return (leave once the handshake resolves).
4. **Otherwise commit the leave:** write **`lobby_state = 7`** (`0x140cae79c`), resolve the embedded
   `NetworkSession` at `container+0x710` (`lea rcx,[this+0x60]; call 0x1423f1930`) — the same object
   this doc elsewhere calls `SessionManagerSteam.slot3`'s `this` (`NetworkSession` is the SDK name;
   `SessionManagerSteam@DLNR3D` its RTTI name; vtable `0x1431f9140`) — and call its
   **vtable slot 3** (`[vtable+0x18] = 0x1423f64f0`) with `edx = [this+0x8]` (the session id/tag),
   storing the return to `[this+0x2c]`. On a **zero** return, tail-call `0x140cb3b80` (a post-leave
   notify) with `dl=1`; a **nonzero** return skips the notify and returns immediately
   (`test eax,eax; jne` at `0x140cae7bd`).

So the "point of no return" is step 4's `lobby_state = 7` write. To suppress the leave, an armed flag
is cleanest **ahead of step 3** — early-returning at the top of the function. (Gating only at step 4
still lets step 3 latch the deferred-leave byte `[this+0x20] = 1` on the mid-handshake path, so a
queued leave would fire the moment the flag is disarmed; gating before step 3 avoids that.)

### `update_step` (B = `0x140cafd10`) — the twin, and the self-initiated leaves

B is the **CSSessionManager per-frame update task** (`this`=`r14`; **0 direct callers** → registered
as a task fn-ptr and dispatched by the scheduler, consistent with the SDK's `update_task:
CSEzUpdateTask`). It contains an **inlined copy of A's exact body** at `0x140cb0840..0x140cb08f7`
(same three guards, same `lobby_state = 7` write at `0x140cb08bc`, same `call [transport+0x18]`), plus
**four** branches that call A **out-of-line** — these are the game's *own* leave conditions:

- **Post-roster-processing flag → leave.** After the pending-entry loop and a `call 0x140cb2f80`, if
  the byte `[this+0x1a] != 0` → `call leave_session` (`0x140cb0085`). (An earlier, flag-driven leave in
  the same update pass, before the inline-twin region.)
- **Host with no remaining peers → grace-timer leave.** If `lobby_state == Host(3)` and the slot
  array holds ≤1 entry (`([r14+0x80]-[r14+0x78])>>8 ≤ 1`) and a countdown at `[r14+0x14]` expires →
  `call leave_session` (`0x140cb0980`).
- **Network status polled low → leave.** If `[this+0x8] != 0` and a net-status query
  (`[global].vtable[0x30]()`) reads `< 0x10000` → `call leave_session` (`0x140cb09b5`). (This is the
  *polled* FSM reaction — distinct from the transport's async connection-down event, which bypasses
  `leave_session` entirely; see the "catch" in the TL;DR and Risk #4.)
- **Received a leave/disband packet → leave.** The packet-drain loop (`0x1403a43a0` = receive) yields
  a specific type → `call 0x140a604a0` then `call leave_session` (`0x140cb0a24`).

This is the strongest evidence that A is *the* leave primitive: even the update task's self-initiated
leaves route through it (or its verbatim inline). **Gating implication:** patching the `leave_session`
symbol alone misses B's inline — the gate must cover **both** the A body **and** the
`0x140cb0840` inline entry.

## Task 2 — `request_leave` and `kick` (per-peer; different granularity)

The SDK's `NetworkSessionVmt` (`kick` slot 7, `request_leave` slot 8) is the vtable of the **per-peer**
object `PlayerNetworkSession@CS` (RTTI-confirmed), vtable **`0x142b9eb30`**:

| slot | method | address |
|---|---|---|
| 0 | destructor | `0x140cc5000` |
| 1 | broadcast_packet | `0x140cc54d0` |
| 2 | send_hit | `0x140cc5540` |
| 3 | receive_packet | `0x140cc50d0` |
| 4 | receive_latest_packet | `0x140cc51c0` |
| 7 | **kick** | `0x140cc5050` |
| 8 | **request_leave** | `0x140cc5390` |
| 9 | remote_identity | `0x140cc5380` |

(The abstract-base `PlayerSession@CS` vtable `0x142b9e938` has a real destructor in slot 0
(`0x140cc4bf0`); slots 1–9 are Arxan cookie-encoded stubs pointing at `0x14251c480`. The concrete
`PlayerNetworkSession` overrides them with the real code above.)

- **`request_leave` (`0x140cc5390`)** — "ask the remote party to leave." Reads `[G]`, checks `[G]+8`,
  resolves this peer's connection by its `remote_identity` (`[this+8]`) via a lookup `0x1404e8a40`, and
  sends a leave request to that one peer. It does **not** write `lobby_state`; it is a per-peer
  transport message, not the FSM leave.
  - **Its callers (Task 2 sub-ask):** it has **0 direct `E8` callers** — it is invoked polymorphically
    through `PlayerNetworkSession` vtable slot 8, so its call sites are out of static `E8`-xref reach
    (whoever dispatches on a peer's `NetworkSession` vtable). Enumerating who *actually* fires it is rig
    territory (hook slot 8 / capture the dispatch), not statically resolvable. That it is vtable-only,
    not directly called, is itself the finding: no game-logic site takes a hard reference to it — it's a
    transport-layer courtesy message, reinforcing that it is **not** the boss/area/death funnel.
- **`kick` (`0x140cc5050`)** — resolves the peer, then calls **`0x140cae6d0`** (a "remove player"
  function adjacent to `leave_session`; 8 callers; also does not write `lobby_state`). Removes one
  player from the roster/session.

**Takeaway:** these are the wrong granularity for the boss/area/death question. They act on a single
remote player; the whole-session leave is `leave_session`. They matter for co-op only in that a host
removing/kicking a departing client, or a client asking to leave, is *distinct* from that client's own
`leave_session` firing — worth keeping straight when we later drive/observe leaves on the rig.

## Task 3 — the container teardown handler `0x1423f46d0` (slot +0x70), charted

The container is `ManagerImplSteam@DLNR3D` at `CSSessionManager+0x60` (`manager_impl_steam`), vtable
`0x1431f8780`. Confirmed the slot map by reading the vtable:

- slot `+0x68` = `0x1423f4870` — **session-established** handler (sets `[container+0x7c0]` bits 1–4;
  charted in SESSION-DRIVE.md).
- slot `+0x70` = **`0x1423f46d0`** — its **teardown counterpart**.

`0x1423f46d0` (`this`=container in `rcx`), behavior:

1. **Unregister two per-session handlers.** Via the session-handler-registry singleton `0x1423f8410`,
   calls `0x1423f8860(registry, &0x1423f41d0, container)` then `0x1423f88f0(registry, &0x1423f41e0,
   container)` — the inverse of the established handler's registrations.
2. **Lock and destroy the connection-node collection.** Takes the lock at `[container+0x8b0]`, walks
   the doubly-linked list rooted at `[container+0x8e8]`/`[container+0x8f0]`, unlinks each node
   (splicing its prev/next at `[node+8]`/`[node+0x10]`) and, per unlinked node, dispatches through a
   container-held sub-object: `rcx = [container+0x8e0]; call [[container+0x8e0]+0x68]` (a
   release/free vmethod on that sub-object — the node allocator/manager — not a method on the node
   itself). Then sets `[container+0x900] = 0` and releases the lock.
3. **Clear the status bits:** `and dword [container+0x7c0], 0xffffffe1` — clears bits 1–4 (`~0x1e`),
   i.e. undoes exactly what the established handler set.
4. Tail-call `0x1423f2ed0` (further container cleanup).

**Relationship to the FSM leave:** this handler has **0 `E8` callers** — like its established twin, it
is invoked **polymorphically** (as container vtable slot `+0x70`) by the DLNR3D session layer when the
underlying DLNW3D connection actually goes down (session closed, peer dropped, socket died). It is the
**reaction**, not the initiator. The FSM leave (`leave_session`) *requests* a close via the dispatch
layer (`0x1423f64f0` sends a close packet); the connection later tears down and this handler fires to
clean up once it is truly gone. Because it also fires for **peer-initiated** and **network-loss**
teardowns that never went through `leave_session`, gating here would (a) be too late (teardown already
committed) and (b) catch disconnects we may *want* to honor. **Do not gate this teardown handler; gate
the FSM source.**

## Task 4 — tracing up: is there one chokepoint, or partial teardown in many places?

**One chokepoint, reached after per-event setup.** `leave_session` is called from **34 sites across
25 distinct enclosing functions** (one of which is `update_step` itself, so **24 external callers** +
the update task), plus `update_step`'s inline twin. The callers span both game-event/packet handlers
in the multiplayer/session-gameplay region (`0x1401d8xxx`, `0x1409fxxxx`, `0x140a22xxx`–`0x140a5fxxx`,
`0x140afcxxx`–`0x140b05xxx`, `0x140c9bxxx`, `0x140ddfbf0`/`0x140de1500`) **and** internal
CSSessionManager methods (`0x140cafc30`, `0x140cb2640`, `0x140cb2e20`, `0x140cb31b0`, `0x140cb46a0`,
`0x140cb7a90`, `0x140cb82e0`) — so "25 callers" is the convergence count, not "25 game events." Sampled
external callers confirm they are independent event/packet handlers, e.g.:

- `0x140c9ba80` — receives packet type 30, then `call leave_session`: a **remote-driven** leave
  (peer/host signalled a disband).
- `0x140a22ce0` — a session-gameplay handler (updates a per-player timer at `+0x58`) that calls
  `leave_session` on two branches.
- `0x1401d8230` — a lower-region handler (likely a shutdown / quit-to-menu path) that calls it once.

They **diverge above** `leave_session` (each event has its own logic) and **converge on it** — which is
exactly the "gate the convergence point, not every source" win the task asked about, at the FSM layer.
(The assignment framed this as "gate the sink, not every source"; note that here the convergence point
is the *initiator* `leave_session`, **not** the low-level teardown handler this doc calls the sink —
see the layer diagram. Same idea, opposite ends of the flow.) Two honest qualifications:

- **"Earliest common" = `leave_session` itself.** There is no single higher wrapper all callers funnel
  through; they call `leave_session` directly. So `leave_session` (step-4 body) *is* the earliest point
  common to all game-driven leaves. Above it, there is no shared node to gate.
- **Partial teardown before the funnel?** The load-bearing teardown action — the `lobby_state = 7`
  transition and the transport close — happens **only** inside `leave_session`/its twin; callers do
  their own bookkeeping first but do not start the session teardown before calling it. The one caveat
  is *sibling* effects: an **area-transition** path may have already queued a map reload, and a
  **death** path may have already started a fade — suppressing the *session* leave without suppressing
  those sibling effects could desync (see risks). That is a per-source concern, not a second teardown
  funnel.

An even lower **single-function** chokepoint exists if wanted: the transport dispatch
`SessionManagerSteam.slot3 = 0x1423f64f0` (0 `E8` callers; reached only from A's body and B's inline
via `call [transport+0x18]`). Gating that one function suppresses the actual transport close from
both leave sites at a single site — **but** it leaves `lobby_state` stuck at `7` (the caller already
wrote it), i.e. the FSM believes it is leaving while the transport does nothing. That inconsistency
makes it a worse gate than the FSM-level one. Mentioned for completeness; **not** recommended as the
primary.

## Task 5 — verdict: is a single armed-flag gate viable, and at what level?

**Viable (pending Risk #1's rig confirmation that no third writer exists) — gate at the FSM
leave-transition (`leave_session`), covering both code sites.**

- **Level:** the `lobby_state = OnLeaveSession(7)` write, i.e. inside `leave_session` (A =
  `0x140cae730`) **and** its inlined twin in `update_step` (B, inline entry `0x140cb0840`, write at
  `0x140cb08bc`). One logical gate, two patch/hook sites.
- **Mechanism:** an armed flag checked at the top of `leave_session` (ahead of step 3, so the
  deferred-leave byte can't latch). When armed, early-return from `leave_session`
  without writing `lobby_state` and without calling the transport dispatch — the session stays
  `Host(3)`/`Client(6)` and the co-op link survives the event. When disarmed, vanilla behavior. This
  mirrors the project's existing boot-patch pattern (`coop/app.rs::apply_boot_patches`,
  `bypass_session_create_gate`), except it is a *runtime-armable* gate, not a one-shot boot patch —
  best implemented as a frame-task-installed hook/patch we can toggle, or a detour at both sites.
- **What it covers:** boss defeat, area transition, player death, host-with-no-peers grace timeout,
  and every one of the ~24 external caller paths — all funnel through the gated primitive.

**Risks / what to verify on the rig (write-watch on `[G]+0xc` + targeted event triggers):**

1. **Completeness of the writer set.** The immediate-store scan found exactly two `lobby_state = 7`
   sites; a **register-store** write (`mov [reg+0xc], eax` after `mov eax, 7`) would be missed
   statically. Confirm with a HW **write-watch on `<[G]>+0xc`** (`scripts/re/watch-write.py`) that
   every observed transition to 7 stops at `0x140cae79c` or `0x140cb08bc` and nowhere else. This is
   the single most important rig check — it validates the whole "two sites" claim.
2. **Which caller = which event.** Trigger boss defeat / area transition / death individually and read
   the **return address** captured at the `[G]+0xc = 7` write to attribute each event to its caller
   (or to `update_step`). Confirms boss/area/death actually route through `leave_session` and not some
   untested path.
3. **Sibling-effect desync.** For the area-transition and death paths specifically, verify that
   suppressing the session leave leaves the game in a **consistent, playable** state — i.e. those paths
   don't independently commit a map reload / player-state change that assumes the session ended. If
   they do, the gate needs to also neutralize that sibling effect (a per-event follow-up, out of scope
   here but flagged).
4. **Bypass paths stay un-gated (by design).** Confirm a genuine **peer disconnect / network loss**
   still tears down via the transport sink `0x1423f46d0` with the gate armed (we do **not** want to
   trap the player in a dead session when a peer really leaves). If we later want to suppress *those*
   too, that is a separate, harder problem at the transport layer — not this chokepoint.
5. **`FailedToLeaveSession(8)`** — no writer found in the block; confirm it is not reached via a
   register store on a leave-failure path (moot for gating, relevant for completeness).

## Re-derivation recipes (after a game update shifts addresses)

- **FSM writer map:** scan both `.text` sections for `mov dword [reg+0xc], imm` (opcode `C7`, ±REX,
  disp8/disp32; `/tmp/scan_leave.py` in this pass, built on `scripts/re/static.py`'s `PE`), filter to
  the CSSessionManager block (the function containing the unique `mov [reg+0x25c], 1` ctor
  fingerprint, per SESSION-RE-FINDINGS.md). The two `imm=7` hits in that block are the leave sites.
- **`leave_session` (A):** the `imm=7` writer whose function opens with the `[G]`-null log guard then
  a `cmp state,5; ja; mov eax,0x25; bt eax,ecx; jb` skip-mask (states {0,2,5}) and a `[this+0x2c]`
  idempotency check. `update_step` (B) is the *other* `imm=7` site, inside the large stack-cookie
  function with **0 direct callers** (the update task) that also `call`s A three times.
- **Container teardown handler:** read `container` vtable `0x1431f8780` slot `+0x70` (the slot after
  the established handler `+0x68`, which is the one opening with two `SteamInternal_ContextInit` calls);
  it is the method ending in `and [this+0x7c0], 0xffffffe1`.
- **Per-peer `kick`/`request_leave`:** RTTI `.?AVPlayerNetworkSession@CS@@` → its vtable → slots 7/8.
- **Transport dispatch:** `NetworkSession` = `container+0x710`, vtable `0x1431f9140`; the leave
  dispatch is slot 3 (`[+0x18]`), the one A calls right after writing `lobby_state = 7`.

## Tooling

Found with `scripts/re/static.py` (the committed PE workhorse): `fn` to disassemble each function,
`calls`/`xref` for caller enumeration, `vtable` for the RTTI→vtable→slot resolution, `ascii` for RTTI
name lookup. The immediate-store FSM scan (`/tmp/scan_leave.py`) and a small vtable-slot reader
(`/tmp/vt.py`) / windowed disassembler (`/tmp/window_disasm.py`) are throwaway `static.py`-based
scripts kept in `/tmp` per the RE-skill's "promote only the reusable shape" rule.

## Cross-references

- [SESSION-DRIVE.md](SESSION-DRIVE.md) — the container/transport map (established handler `0x1423f4870`
  slot `+0x68`, the `[container+0x7c0]` status bitfield, the DLNW3D transport). This doc is the
  **leave/teardown** counterpart to that create/establish map.
- [SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md) — the keystone `G = 0x143d7a4d0`, the ctor
  fingerprint, and the field offsets this pass builds on.
- SDK (pinned `8c67a84`): `crates/eldenring/src/cs/session_manager.rs` (`LobbyState`, offsets),
  `crates/eldenring/src/cs/network_session.rs` (`NetworkSessionVmt`: `kick`/`request_leave` slots).
