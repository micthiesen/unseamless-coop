# Driving a Session Directly (rung-3 call spec)

> ## ★ DECISION (2026-07-04): pivot rung-3 to the "let the game establish it" (true ERSC) model — READ FIRST
>
> **★★ UPDATE (2026-07-04, later) — LIVE CAPTURE DONE. See [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md).**
> We captured a real working 2-player ERSC session (rig host + Deck joiner) in memory. Two corrections:
> (1) **`[context+0x168]` is the reject-stub `0x1423fdf00` EVEN in a working session** — the whole gate-c /
> "install a real member-lookup" theory (avenue a) is a **dead end**; members come from the session layer,
> not the transport admit gate. (2) The full DLNR3D/DLNW3D graph (`SessionSteam`, 6 `SessionMemberSteam`,
> the context, live `SteamConnectionManager`, **`SteamServiceImpl`** — the standup that's null offline) is
> all present and enumerated with offsets. The reproduce target is now precise: drive the game's
> establishment to build `SessionSteam` + members + stand up the transport keyed to the rung-4 peer
> SteamID64; the one concrete wall is why `SteamServiceImpl` standup (`0x142638b40`) returns null offline.
> The `+0x168`/gate-c framing throughout the blocks below is **superseded** (kept as history).
>
> **We are no longer hand-synthesizing the session object graph offline. We will drive the game's own
> session-*establishment* machinery, fed our rung-4-discovered peer, and let the game build its own
> members/context/connection — reproduced from a LIVE CAPTURE of a real working ERSC session.**
>
> **Why (empirical, this session's 3-lane RE — full detail in the DLNR3D-reframe block below):** every piece
> that's missing offline is a *runtime object the game's establishment flow builds*, not something forgeable
> from the static image:
> - the live-session array capacity is 0 offline, so a created `SessionSteam` is destroyed the instant it's
>   built (Lane C);
> - add-member takes two ref-counted *game handle objects* the connect handshake produces, not a scalar
>   SteamID we can fabricate (Lane A);
> - the transport context's accept-callback at `+0x168` is baked to a reject-stub with **no code path anywhere
>   in the binary** that installs a real one — it can only arrive via a runtime Steam callback (Lane B).
>
> Hand-forging these field-by-field has been whack-a-mole across many sessions and is a dead end. But the
> game's native session **does** establish outside EAC over Steam P2P — **ERSC proves it** (ERSC players see
> each other in-world with no FromSoft matchmaking), and our own **Steam P2P transport is already rig-proven
> two-machine**. So the winning move is to *observe a working establishment and reproduce its sequence*, letting
> the game wire its own construction-time sub-objects (the exact thing we kept losing by hand).
>
> **What "the ERSC / online-matchmaking model" means here (and does NOT mean):** drive the game's own
> *matchmaking/session-establishment code* (its create/join/establish entry points) with a peer we discovered
> ourselves. It does **NOT** mean reaching FromSoft's matchmaking servers — those stay unreachable outside EAC
> and off-limits by design. The peer is still found via our rung-4 password-keyed Steam-lobby side-channel; the
> session still rides **Steam P2P** (transport already proven). We're reproducing what ERSC does, not going
> back onto the official servers.
>
> **\* The offline hand-synthesis avenue (a) is paused, not killed.** If the live capture shows the
> establishment reduces to a small deterministic set of fields/calls we *can* reproduce without a live handshake
> — i.e. offline turns out viable or simpler — we re-open it. The capture is the arbiter of offline-vs-drive; it
> serves both paths (it also hands avenue (a) the real objects to clone).
>
> **► NEXT STEPS (in order):**
> 1. **Live-read capture of a REAL working co-op establishment (ERSC).** Attach the standalone ptrace watcher
>    `scripts/re/watch-write.py` (no mod needed — ERSC runs outside EAC; `kernel.yama.ptrace_scope=0`) to a live
>    ERSC host process during a real host+join with a second player, and watch the charted offsets as the
>    session comes up:
>    - `SessionManagerSteam +0x18/+0x20/+0x24` (session array ptr / **capacity** / count) — *who sizes capacity?*
>    - `SessionSteam` slot-26 add-member args (the two ref-counted handle objects) — *what are they, where from?*
>    - `MTInternalThreadSteamSocket +0x168` — *what real value lands, and what installs it (the Steam callback)?*
>    - `[container(ManagerImplSteam)+0x48]` owner/config — *what makes the `SteamServiceImpl` standup
>      `0x142638b40` return non-null* (this is exactly what the native-builder dead-end couldn't satisfy offline);
>    - `[container+0x708]` socket-manager wrapper; `[container+0x710]` embedded `SessionManagerSteam`.
>    Capture the **values**, the **call order**, and **what triggers each writer**. (Reachability chain to all of
>    these from a live `CSSessionManager*` is charted — see the Lane C block below.) Needs Michael + one real
>    player in an ERSC session; orchestrator drives the watcher.
> 2. **Chart the minimal establishment entry sequence** from the capture, then **reproduce it in the mod**
>    driven by the rung-4-discovered peer (the game builds its own graph).
> 3. **Validate two-machine (rig + Deck):** host-admit-success `0x142640ee4` fires → roster `players=2` → both
>    players in one another's world.
>
> ---

## ★★ MEMBER PIPELINE CHARTED (2026-07-05, static) — how a connected peer becomes a member

Static RE over `eldenring.exe` (clean binary; behavioral notes in our own words) charted the **entire
consumer side** of the joiner-member: what turns "a peer is here" into "a `SessionMemberSteam` with the
peer's SteamID64 at `+0x80`". This is the pipeline our host-repro already runs every frame — we just
need to feed the Deck into its front.

**The per-frame chain (host side), all in the DLNR3D session layer:**

```
update_step 0x140cafd10                         (CSSessionManager per-frame)
 → 0x1423f2cfa                                   (per-frame tick; computes elapsed ms)
   → 0x1423f6bf0   SessionManagerSteam.update    (mgr = container+0x710)
       • ticks the SocketManagerHolder [container+0x708] via 0x14203f1f0  (drains Steam P2P)
       • for each live session in mgr's array [mgr+0x18][0..[mgr+0x24]]:
         → 0x1423fb690   session.update(session, elapsed)         ← THE per-session pipeline
             • loop the PENDING-CONN queue [session+0x4f0 .. +0x4f8]:
                 → 0x1424007e0  conn.pump  — reads DLNW3D handshake msgs from the conn's transport
                    endpoint [conn+0x130] (holder API 0x14203f250) and runs an 8-case jump-table
                    handshake state machine; on completion flips conn+0x150..0x153, then the conn is
                    moved into the member registry ([session+0x508]/+0x528/+0x538 via 0x1423fa2a0)
             • [session_vt+0xe0] = slot 28 = 0x1423ff440   session.drainEvents
                 → drains a lock-free MPSC event queue [session+0x578 buf / +0x580 / +0x588 idx]
                 → dispatch on event.type (first dword of each 0x28-byte record):
                     type 1 (ADD) → 0x1423fe350 → 0x1423fdc80   ADD-PEER:
                         · resolve the peer key via [session+0x568] resolver vt[0x118]
                         · 0x1423fbd80 lookup-by-key (dedup) — skip if the peer already has a conn
                         · 0x1423fb980 alloc a new session-peer conn
                         · 0x142402d70 init the conn from peerInfo → this is where the peer SteamID64
                            is written to the member's +0x80 (leaf copy 0x142400480: src[0]=SteamID64,
                            src+8=name UTF-16, src+0xb0..=flags)
                         · 0x1424004e0 finalize; on success ENQUEUE the conn onto [session+0x4f0..+0x4f8]
                     type 0 → 0x1423fe4e0 ;  type 2 → lookup + 0x1424005c0 (update existing)
```

**So a member is born when a type-1 "add-peer" event carrying the peer's identity is posted to the
session's event queue; the running per-frame drain does the rest** (alloc conn → init identity → enqueue
→ pump handshake → register member). The host's OWN member (`member[5]` in host-repro) is added by this
exact path — something posts a type-1 self add-peer event during establishment, which is why our solo
host-repro already shows `member[5]+0x80 = host SteamID`.

**Two ancillary corrections to the older notes:**
- The pre-alloc `0x1423faf60` (`SessionSteam` vt[26] `0x1423fdf20`, our existing `add-member` hook) builds
  the **6 empty member slots** with an *empty* identity handle (`0x142400170`, vtable `0x1431fa4a8`) — it
  does NOT write any SteamID. The SteamID is written later, on the per-peer path above. So the "7× add-member"
  in host-repro is the pool pre-alloc, not the identity population; **`0x1423fdc80` (add-peer) is the hook
  that actually signals a peer being brought in.** (New read-only hook `add-peer`, under `instrument_host_accept`.)
- The writer-trace's function labels were skewed by the `static.py` `.pdata`-spill; the true leaf chain is
  `0x1423fb690 → … → 0x1423ff440(drain) → 0x1423fe350 → 0x1423fdc80(add-peer) → 0x142402d70(init) →
  0x142400480(+0x80 write)`.

**The ONE remaining unknown = the PRODUCER.** What posts the type-1 add-peer event (with the peer's
identity) is invoked via a runtime/vtable path the static image doesn't pin (the internal event enqueue
helpers `0x1423fda40/b00/bc0` have no static callers; the identity domain is the `[session+0x568]`
resolver + a `[event+8]` peer handle, both runtime objects). This matches Lane B's standing caveat that the
real installer is a runtime Steam callback. **This is now an empirical question**, and the two ways to
resolve it map cleanly to a two-machine experiment:
1. **Observe:** with the `add-peer` hook live on a two-machine run, does `0x1423fdc80` fire for the Deck's
   SteamID? If YES → the natural producer works and the only gap is the handshake pump getting the Deck's
   packets (transport already proven). If NO → the event is never posted for the Deck.
2. **If NO, post it ourselves:** build a type-1 event / drive `0x1423fdc80` for the Deck's rung-4-known
   SteamID64, and let the running per-frame pipeline do the rest — provided the conn's `+0x130` transport
   endpoint binds to the Deck's live P2P packets (which `0x142402d70` wires from the peer identity + holder).

### ★ EMPIRICAL RESULTS (2026-07-05, solo + two-machine) — model validated, drive built, one gap left

The pipeline was **validated live** and a **direct-drive lever built** (`[debug.probes] drive_add_peer`,
`session_probe.rs::try_drive_add_peer`). What we learned, in order:

1. **Live-validated the model (solo host-repro).** Read off the live `SessionSteam` (reached
   `[[[G+0x48]…]]`→ actually `container+0x710`→`+0x18`→`[0]`): vtable `0x1431fa248`, member count
   `[+0x68]=6`, and the graph is **exactly** as charted — `member[5]+0x80` = the host's own SteamID64,
   `member[0-4]` empty, and the host self-member sits in the **pending-conn queue** `[session+0x4f0..+0x4f8]`
   (1 entry) with `+0x130=0`, flags `(1,1,0,0)`. The event queue `[+0x580/+0x588/+0x590]` had already
   advanced (0x20 events drained) during establishment.
2. **`add-peer` (`0x1423fdc80`) does NOT fire for the host's own member** — the self-member is populated by
   the establishment directly, not the event-driven add-peer. So `add-peer` firing is specifically the
   *remote-peer* signal.
3. **Two-machine: no natural producer.** The Deck's 14-byte DLNW3D SYN reaches host-admit `0x142640e30`
   ~11× (sender `peer-cf17b9f9`), gate-c rejects each time, and **`add-peer` NEVER fires** — then the Deck
   crashes ~30s in. So nothing in our setup posts the add-peer event for the Deck.
4. **`drive_add_peer` works mechanically.** Driving `0x1423fdc80(session, &DeckSteamID, &hostSteamID, 1)`
   host-side returns 1, **pops an empty member from the pool** (`[session+0x538]` head moves) and **enqueues
   a conn** on the pending queue (`[+0x4f8]` grows +8) — `member+0x80` = Deck SteamID, via the game's own
   function. Confirmed `member+0x80 = peerInfo[0]` (the SteamID goes straight in).
   - **Timing matters:** firing during `TryToCreateSession` disrupted the create→Host transition (session
     tore down to `None` ~30s later). Gating the drive on **`lobby_state == Host`** fixed it — the host
     then stays stable.
5. **The one gap: the driven member has a null transport endpoint (`+0x130=0`), so the per-frame pump
   (`0x1424007e0`, which reads handshake msgs from `conn+0x130`) can't advance it, and the session update
   **drops the member** (solo: pending queue returns to 1, `member[0-4]` empty again — the session
   survives, the member is just discarded).** `+0x130` is a **transient handshake endpoint** — the live
   ERSC capture confirms even a *working* remote member reads `+0x130=0` in steady state (it's set only
   while handshaking in the pending queue, then cleared once the member moves to the active registry
   `[+0x528]`).
6. **The transport admit is a confirmed dead end (again, at the instruction level).** gate-c's identity
   callback `0x142639810→0x142639d00` calls `[context+0x168]` (the reject stub, returns 1) and
   **`cmp eax,1; je reject` short-circuits before the find-or-create** — so pre-creating the member can't
   unblock gate-c. The stub is present in real ERSC too. The connection+member+**endpoint** are all built
   by the session-layer producer path, not the transport admit.

**⇒ The remaining work is to WIRE THE ENDPOINT.** The driven member is correct except for `+0x130`. In a
real join, the producer (unreached in our setup; enqueue helpers `0x1423fda40/b00/bc0` + the identity
callback have **no static refs at all** → runtime/Steam-callback-installed, matching Lane B) builds the
member *with* a transport endpoint bound to the peer, so the handshake pump can consume the peer's DLNW3D
packets. Two ways in for next time:
- **(a) New ERSC capture, watching the writers:** arm `watch-write.py` on a fresh remote member's `+0x130`
  (and the event queue `[+0x578]`) during a live Deck **join** on real ERSC — catch the RIP that sets
  `+0x130` and what posts the add-peer event. This pins the endpoint source directly (Michael-gated, ~10
  min, the Deck is still set up). This is the highest-leverage next step.
- **(b) Build the endpoint ourselves:** after `drive_add_peer` pops the member, bind its `+0x130` to a
  transport endpoint on the stood-up holder keyed by the Deck SteamID (the holder API `0x14203f2xx` family
  the pump uses), so the Deck's real P2P packets feed the pump. Needs charting the endpoint-open call.

`drive_add_peer` (host-side, gated on `lobby_state==Host`) is the foundation both build on: it creates a
correct member for the Deck; only the endpoint bind is missing.

### ★★ ENDPOINT CAPTURED (2026-07-05, live ERSC) — how `member+0x130` gets built + bound

A live 2-player ERSC capture (rig host + Deck joiner, standalone ptrace via `scripts/re/capture-endpoint.py`
+ `watch-write.py`/`watch-bt.py` on a Deck leave→rejoin) pinned the endpoint mechanism completely:

- **`member+0x130` is a `DLNW3D::MTInternalThreadSteamConnection`** (RTTI-confirmed, vtable `0x143277750`) —
  the per-peer Steam P2P connection. In the working session the Deck member (`member[4]`) had `+0x130` SET
  to a live connection with a back-ptr to the member at `endpoint+0x50`, an index `endpoint+0x8` = the peer
  index, and callbacks at `endpoint+0x20/+0x28`. (Correcting the older dump: a *connected* remote member's
  `+0x130` is **non-zero**; the host's own self-member keeps `+0x130=0`.)
- **The SET writer is `0x14203ef70`** (a ref-counted pointer assign, `member+0x130 = src[0]`), called from
  **`0x142401110`**: that function builds a descriptor from the session's transport fields
  (`[session+0xc0]`, `[session+0xc8]`, `[session+0xd1]` + 4 local callbacks), calls the member's own
  **vtable slot 13 (`[member+0x68]`)** to construct the `MTInternalThreadSteamConnection`, then binds it via
  `0x14203ef70`. The CLEAR-on-leave writer is `0x14203f050` (release).
- **The trigger (live backtrace):** `SessionManagerSteam.update 0x1423f6bf0 → per-session update
  0x1423fb690 → the conn PUMP 0x1424007e0 → 0x1423ffd00 → 0x142401110 → 0x14203ef70`. So the endpoint is
  built **automatically by the per-frame pump** as it advances a member in the pending-conn queue — there is
  no separate "bind the endpoint" driver; the member just needs to be in the queue and its handshake to
  progress (which needs the peer's DLNW3D messages arriving at the pump).

**⇒ Reframes the fix — the payoff test is `drive_add_peer` two-machine (untested).** `drive_add_peer`
already puts the Deck member in the pending-conn queue; the game's own per-frame pump should then build the
endpoint (`0x142401110`) and complete the handshake **once the Deck's real packets reach the pump**. My
earlier two-machine run predated the `drive_add_peer` lever, so the combination "member driven into the
queue **while the Deck is connected**" was never run. That is the next test:
1. Both machines on our mod (`rig.sh apply` host + `deck.sh apply --auto-session join`).
2. Rig reaches `Host`/`Ingame`; `drive_add_peer` fires (gated on `lobby_state==Host`) → Deck member enqueued.
3. Deck connected + sending → watch whether the per-frame pump builds `member[4]+0x130` and completes the
   handshake → `member+0x80` = Deck ID persists, roster → 2. `capture-endpoint.py` reads `+0x130` on the
   rig to confirm the endpoint got built.
- If the pump still drops the member, the gap is narrower still: the Deck's handshake messages aren't
  reaching the pump's read (`0x14203f250` on `conn+0x130`/the holder) — chart that read's source next.

## ★ JOINER-ADMIT (2026-07-05, two-machine) — the transport admit path is the WRONG mechanism

Two-machine (reproduced rig host + Deck joiner, both our mod, `--auto-session host`/`join`): **the Deck's
SYN reaches the host admit path** `0x142640e30` (10–23×) and the side-channel links (`coop: linked`), but
**no joiner member is added** — and forcing the transport admit is a dead end:

- gate-c `0x142640ecd` (`test eax,eax; jne bail` after the identity callback) rejects — the callback is the
  stub `0x1423fdf00` (`mov eax,1; ret`). **Forcing it to ACCEPT** (`force_gatec_accept`, writes `rax=0`)
  **does NOT reach admit-success** `0x142640ee4`: there's a **SECOND gate at `0x142640ed5`
  (`cmp [rsp+0x80],0; je bail`)** — `[rsp+0x80]` is the *existing* connection for the peer, null here (the
  Deck has no host-side connection yet), so admit bails regardless.
- And the **live capture already showed gate-c rejects even in a real working ERSC session** — so this
  transport admit (`0x142640e30`) is **not** how a joiner becomes a member. Wrong door.

**⇒ The joiner-member is added by the SESSION-LAYER establish flow** (the same `add-member 0x1423fdf20`
that populated the host's own `member[5]`), using the joiner's **arg2 identity handle**, which is derived
from the joiner's *connection*. So the real remaining work is: **chart how the host's establishment
incorporates a connecting peer** — where the joiner's connection object + its identity handle come from,
and what triggers `add-member` for it — then drive that with the rung-4/side-channel-known Deck SteamID.
`force_gatec_accept` is charted but OFF (not the path). **Also: the Deck (joiner drive) crashes after
~30–60s** — needs stabilizing for sustained two-machine testing.

Confirmed two-machine wins: host reproduction (`member[5]`=rig), transport SYN reaches host-admit,
side-channel link.

## ★★ HOST-SIDE ESTABLISHMENT REPRODUCED (2026-07-05) — the game builds the member graph offline

**Path A worked on the first try.** With the input descriptor to `0x1423f2820` seeded from the stood-up
socketmgr's post-ctor defaults (`[desc+0..0x48]` ← `socketmgr[0x58..0xa0]`, so the config survives the
handler's pipe into the builder's socketmgr config region — the local-copy GAPS turned out **non-fatal**),
the driven establish handler **succeeds offline** and the game runs its own full establishment:

- `0x1423f2820` **returns 1**, builds + wraps a connection at `[container+0x708]` (by the handler itself).
- **★ `ADD-MEMBER` (`0x1423fdf20`) fires** (7×, caller `0x1423fb118`) — the game's own member-add runs.
- Result, **solo + offline**, stable (`lobby_state=Host(3)`, `protocol=Ingame(6)` held): **1 `SessionSteam`
  + 6 `SessionMemberSteam`** (the pre-alloc pool), **`member[5]+0x80` = the rig's own SteamID64**
  `76561198004789432` (the host is a member of its own session — EXACTLY as the live ERSC capture),
  `members[0-4]+0x80 = 0` (empty slots), `SessionManagerSteam` count=1 cap=16.

This reproduces the live-captured **host-side** graph exactly, via the game's own establishment machinery.
The "let the game establish it" model is proven. Config: `drive_establish_handler` on,
`drive_session_established` off, `stand_up_transport` + `land_socket_holder` on, seed as above.

**► NEXT (needs a 2nd machine): the JOINER.** A real Deck joiner over the (rig-proven) DLNW3D Steam P2P
transport should make the host's establish/admit flow populate one of the empty slots (`member[0-4]`) with
the Deck's SteamID64 → roster grows to 2 → both players in each other's world. That's the two-machine test.

---

## ★ REPRODUCTION (2026-07-05) — establish handler reaches the builder; wall = one descriptor field

Post-capture, `drive_establish_handler` was re-opened (the "standup null" that shelved it was a probe
artifact — ERSC-LIVE-CAPTURE-FINDINGS). Rig result (`drive_establish_handler` on,
`drive_session_established` **off** so the handler owns the first `0x1423f4870` call, transport-standup +
`land_socket_holder` on):

- **The establish handler `0x1423f2820` now passes EVERY gate offline** — readiness gate `0x1423f5190`=1,
  session-established gate2 `0x1423f4870`=1 — and **reaches the builder** (`vtable[0x80]` thunk `0x1423f46b0`
  → `0x142637440`), with the config `0x143d87750` in hand. The gates we suspected are not the wall.
- **The builder `0x142637440` fails at one spot.** Its body ≈ what `land_socket_holder` does by hand:
  alloc `0x150` socketmgr → ctor `0x142638140` → **call socketmgr `vtable[8]` (the sub-init) with the
  descriptor**, bail if it returns 0. The thunk `0x1423f46b0` tail-calls the builder with `rcx = &local`
  (a stack local: `[&local+0] = config`), and the builder passes `&local` as the sub-init's descriptor.
- **★ CORRECTION (charted `0x1423f2820`'s local setup): gate 3 is NOT the wall.** The builder's descriptor
  is a stack local `&local = [rbp-0x49]` the establish handler builds *unconditionally*:
  `local[0] = container+0x48 (config)`, **`local[8] = 0x1423f2d70` (hardcoded `lea [rip+0x49d]`)**,
  `local[0x10] = container`, and **`local[0x18..0x5c] = dwords copied from OUR input descriptor
  `[rbx+0..0x34]``** (+ bytes `[rbx+0x3c/0x3d]`), then `call [container_vt+0x80]` (the builder). So gate 3
  (`[descriptor+8]`) always passes — my earlier "wall = `[+8]==0`" was **wrong**.
- **The real failure is downstream in the sub-init, and its cause is the descriptor *content*.** The sub-init
  copies `local[0..0x60] → socketmgr[0x40..0xa0]`, so `local[0x18..0x60]` (= our **zeroed** input descriptor)
  **clobbers the socketmgr config region `socketmgr[0x58..0xa0]`** — exactly the base-ctor defaults
  (`+0x58/0x5c/0x60/0x74..0x9c`) that `land_socket_holder` deliberately *preserves*. That is why the
  standalone `land_socket_holder` init succeeds and the establish-handler builder's does not: same code, but
  ours feeds it a good config and the handler feeds it our zeros.
- `ADD-MEMBER` (`0x1423fdf20`, the new reach-hook) did **not** fire — the builder gates the whole
  `establish → session-create 0x1423f7070 → add-member` chain, so a builder bail stops it before the member.

**⇒ The wall is the input-descriptor CONTENT, not one field.** The establish handler pipes our input
descriptor (`[rbx+0..0x34]`) into the socketmgr config, so a zeroed input kills the build. **Two ways
forward:**
- **Path A (seed the input):** populate the descriptor we hand `0x1423f2820` (`[rbx+0..0x34]`) with the
  socketmgr config defaults `land_socket_holder` already knows (it reads them off a freshly-ctored
  socketmgr). Then the establish-handler builder builds a good socketmgr → succeeds → the handler proceeds
  to `session-create → add-member` (watch the `ADD-MEMBER` hook + member registry `0x143dcd758`).
- **Path B (skip the builder):** `land_socket_holder` already builds a *working* socketmgr+service+holder;
  drive `session-create 0x1423f7070 → add-member 0x1423fdf20` directly on it, feeding the host SteamID (the
  capture charted this chain). Avoids reconstructing the descriptor entirely.

Path A is a small change (seed `[rbx+0..0x34]`) that lets the game do the rest; try it first, fall back to B.

> ## STATUS (2026-07-04, DLNR3D reframe) — read this FIRST, it corrects the block below
>
> **The "member machinery is a runtime closure with no static function" conclusion below was partly wrong.**
> A fresh xref pass climbed the builder chain out of the DLNW3D transport layer into the **DLNR3D session
> layer** and put real class names on it (RTTI-confirmed):
> - The context builder that the block below calls an *"unrelated lobby/session-info cache"* (`0x1423fe030`,
>   reached via `0x142637410`→`0x142638410`) is actually **`DLNR3D::SessionSteam` vtable slot 25** (vtable
>   `0x1431fa248`, RTTI `.?AVSessionSteam@DLNR3D@@`). It is the *session's own* transport-context builder, not
>   an unrelated cache. It allocates a 0x190-byte context (alloc `0x141eb9ed0`), ctor `0x142639870`, then
>   registers via `0x142639b70`.
> - The member-descriptor builder `0x142402e10` (via `0x1426373e0`→`0x1426382f0`) is **`DLNR3D::SessionMemberSteam`
>   vtable slot 13** (vtable `0x1431fa978`, RTTI `.?AVSessionMemberSteam@DLNR3D@@`).
> - **The real "add member" is `0x1423fdf20`** (sits right next to the gate-c stub `0x1423fdf00`): given
>   `(rcx=session, rdx=arg1, r8=arg2)` it reads the session's sub-object `[session+0x58]` (=S), reads allocator
>   `[S+0x48]`, allocates **0x170 bytes** (the `SessionMemberSteam` size), and calls the member ctor
>   `0x142402bf0(alloc, S, session, arg1, arg2)`. It is a vtable slot (no direct callers) — the online flow
>   invokes it when a peer joins.
> - **`DLNR3D::SessionSteam` ctor `0x1423fd300`** (created by fn `0x1423f7070`); **`SessionMemberSteam` ctor
>   `0x142402bf0`**.
>
> **⇒ Reframed plan (matches the goal "let the game build a real member from fed peer info"):** rather than
> hand-synthesizing the member + context + lookup (avenue a below), **drive the game's own DLNR3D SessionSteam
> to add a member for the peer SteamID our side-channel already discovered.** That runs the real member ctor →
> the real context/member wiring the transport admit path expects. Three RE lanes in flight (2026-07-04) to
> nail the exact drive: (A) which vtable/slot is add-member `0x1423fdf20`, its two identity args, and the
> `0x1423f7070` session-create call chain; (B) whether `[context+0x168]` ever gets a *non-stub* real lookup
> installed and by whom (does `0x142639b70` really always write the stub, or conditionally?); (C) where the
> live SessionSteam instance is stored (reachable from CSSessionManager/socketmgr?) + its vtable slot map.
> Findings land here when they return.
>
> **Lane A result (2026-07-04):**
> - **add-member `0x1423fdf20` = `SessionSteam` vtable slot 26** (offset `0xD0`).
> - **Its two args are NOT a raw SteamID** — both `rdx`/`r8` are pointers to intrusive ref-counted objects
>   (refcount at `obj+8`; the ctor `AddRef`s each), stored on the new member at `+0x70` (arg1) and `+0x78`
>   (arg2). The real member ctor is a base ctor `0x142400210` (member base vtable `0x1431fa688`): it also sets
>   `[member+0x58]=S`, `[member+0x60]=session`, hooks into a member registry rooted at `S+0x1e8`, and sets
>   flags `+0xa4=0x10002`. So any `CSteamID` is *inside* one of the two handle objects (one more deref than
>   charted); which of `+0x70`/`+0x78` is identity vs transport-handle is **unconfirmed**. ⇒ Driving add-member
>   needs the game's own identity+transport handle objects, which the join handshake produces — not a scalar we
>   fabricate. This is why the member is "online-populated": the handles come from the connect flow.
> - **session-create `0x1423f7070` = `SessionManagerSteam` vtable slot 33** (offset `0x108`; RTTI
>   `.?AVSessionManagerSteam@DLNR3D@@`, vtable `0x1431f9140`). Allocates 0x5f8 (SessionSteam size) from
>   `[[mgr+8]+0x48]`, builds from `[mgr+8]` (=S), the manager, and one opaque arg; bumps a manager counter
>   `[mgr+0xa8]`.
> - **Two drivers of slot 33**, both gated by manager predicate slot 29 under a lock: **slot 1 `0x1423f5c00`**
>   (flag `r8b=1`, host-create) and **slot 2 `0x1423f62e0`** (flag `0`, join, extra args, distinct follow-up
>   `0x1423fb260`). **Slot 1 is exactly the probe's existing `LEGB_ENTRY_OFFSET 0x1423f5c00`** — we already
>   drive the host-create path; **slot 2 `0x1423f62e0` is the DLNR3D-level join** (below the CS join wrapper
>   `0x140cae640`).
> - Sibling slot 27 `0x1423fdfa0` = a *second* creator (0x60-byte object, vtable `0x1431fa238`, same arg pair)
>   — a plausible "pending connection/ticket" companion, not a finder. No SteamID-keyed member lookup found
>   (Q4 unresolved). Tooling caveat: `static.py fn` spills past `ret`/`jmp` into the next function — cross-check
>   with `func_bounds()`.
>
> **Lane B result (2026-07-04) — the `+0x168` question, exhaustive static answer:**
> - The context at `[socketmgr+0x48]` is **`DLNW3D::MTInternalThreadSteamSocket`** (vtable `0x1432770b0`,
>   RTTI-confirmed). The socket-manager itself is **`DLNW3D::SteamConnectionManager`** (vtable `0x143278020`;
>   slot 3 = worker thread `0x142640bc0`, slot 4 next to admit helper `0x142640e30` — confirms it's "socketmgr").
> - The ctor `0x142639870` **zeroes** `+0x168/+0x170/+0x178` (not the stub — value-level correction). The
>   reject-stub `0x1423fdf00` is manufactured by the **driver `0x1423fe030` (SessionSteam slot 25)** on its own
>   stack (`srcstruct[0]=0x1423fdf00`) and threaded **unconditionally on both branches** through register
>   `0x142639b70` (a generic 24-byte copy `srcstruct→context+0x168`, no real-vs-stub special case).
> - **Exhaustive search (call-graph + rip-ref + raw-pointer + a full `disp==0x168` write scan across
>   0x1423x–0x1426x, RTTI-cross-checked) finds NO writer of any real (non-stub, non-zero) function to
>   `MTInternalThreadSteamSocket+0x168` anywhere in the image.** The stub is the only value ever assigned. The
>   `0x1423fdf00` hardcode occurs exactly once (`0x1423fe052`); the register fn is never stored as a
>   callback/vtable entry (no hidden indirect installer).
> - **Caveat (the load-bearing one):** this is exhaustive over everything a *static* disassembler sees. A
>   Steamworks networking callback registered with the Steam API at runtime (external to this binary) could
>   install a different `+0x168` value invisibly. Confirming where a non-stub value lands online needs a **live
>   `watch-write.py` capture during a real online host start** — the twice-recommended step.
>
> **⇒ Combined A+B read:** the real member handle-objects (add-member's two ref-counted args) AND any real
> `+0x168` lookup are **produced at runtime by the connect handshake / Steam callbacks, not present statically**.
> Offline synthesis of roster→2 is therefore blocked short of reimplementing the matchmaker's runtime object
> graph. This is the empirical case for the **real-online-matchmaking (true ERSC) pivot** over offline forcing —
> see the recommendation being written up top once Lane C lands.
>
> **Lane C result (2026-07-04) — live-session storage + full reachability from `CSSessionManager`:**
> - **Live sessions are stored in a flat `SessionSteam*[]` array on `SessionManagerSteam`:** `+0x18` = array
>   ptr, `+0x20` = capacity (dword), `+0x24` = count (dword). Both create drivers (host slot 1 `0x1423f5c00`,
>   join slot 2 `0x1423f62e0`) do, after a successful `0x1423f7070` create: `if count<cap { array[count++]=session }
>   else { session->vtable[2](tick); session->vtable[0](DESTROY) }`. **Offline, capacity stays 0 → every created
>   session is immediately destroyed** before it can be stored/observed. This is the mechanical root of the
>   documented "create dies at leg B's tail capacity check". (The probe's `FABRICATE_SLOT_ARRAY` targets exactly
>   this array.)
> - **Reachability chain from the mod's live `CSSessionManager*`:**
>   ```
>   CSSessionManager +0x48 (container2 holder) +0x18 -> ManagerImplSteam* ("container"/S; RTTI
>       .?AVManagerImplSteam@DLNR3D@@, size 0x908; == CSSessionManager+0x60 shorthand)
>   ManagerImplSteam +0x708 -> SocketManagerHolder* (null until stood up)
>                    +0x710 -> embedded SessionManagerSteam ("NetworkSession", vtable 0x1431f9140, size 0xb0)
>   SessionManagerSteam +0x08 owner back-ptr (= ManagerImplSteam) | +0x18/+0x20/+0x24 session array/cap/count
>                       +0xa8 session-id/generation counter (starts 1) | vtable[29] create-gate predicate
>                       vtable[33]=0x1423f7070 create-session
>   ```
> - **`[SessionSteam+0x58]` = back-pointer to the owning `ManagerImplSteam` container** — the same object whose
>   `+0x708` holds the socket-manager wrapper. So session→container→socket-manager is one deref apart; AddMember
>   (slot 26) reads `[session+0x58]` for the container, then `[container+0x48]` for the allocator.
> - **SessionSteam vtable (`0x1431fa248`) map:** slot 0 = dtor (0x1423fd480, `operator delete` size 0x5f8),
>   **slot 1 = IsReady/readiness gate `0x1423fd7a0` (= the probe's existing `CREATE_GATE4_OFFSET`)**, slot 2 =
>   tick, slot 25 = build-context (0x1423fe030), slot 26 = AddMember (0x1423fdf20), slot 27 = 0x60-byte companion
>   creator (0x1423fdfa0), slot 33 = adjustor thunk into `[session+0x5a8]`. Sizes: SessionSteam 0x5f8,
>   SessionManagerSteam 0xb0 (embedded), ManagerImplSteam 0x908, SessionMemberSteam 0x170.
> - Unconfirmed: whether any *other* method caches a "current session" pointer elsewhere; the `+0x18/+0x20/+0x24`
>   array is the only storage the two create sites touch.
>
> ---
>
> ## STATUS (2026-07-04 night, updated) — superseded in part by the DLNR3D reframe above
>
> **★★ HOST-SIDE ADMIT REACHED OFFLINE — the joiner's synthetic SYN crosses to the host's game layer; the sole
> remaining wall is the socket-manager context's member-lookup STUB.** This session drove the joiner→host
> transport-level connect and instrumented the host's inbound path end-to-end on a two-machine run (rig host +
> Deck joiner). The full charted chain, all rig-confirmed:
> 1. The host's socket-manager **worker thread RUNS offline** (`0x142640bc0`) and reads P2P on **channel 30**
>    (`nChannel=[socketmgr+0x50]`, live-observed — NOT channel 0).
> 2. The joiner sends a real **14-byte DLNW3D SYN** `[0x0e, 0x40, …]` (the exact shape the admit gate
>    `0x142642830` accepts: size 14, header control-length 14) **on channel 30**, and it **REACHES the host's
>    admit-new-peer helper `0x142640e30`** (`host-admit` fires, sender=joiner, msgSize=14). First time the
>    joiner's game-P2P ever crossed to the host game layer offline.
> 3. Admit gates **a (size `[socketmgr+0x5c]≥14`) and b (SYN shape) PASS.** It bails at **gate c**
>    (`0x142640ecd`): the identity callback **`[socketmgr+0x40]` = `0x142639810`→`0x142639d00`** returns **1
>    (REJECT)** every time, so `host-admit-success` (`0x142640ee4`, connection creation) never fires and the
>    **host roster stays `players=1`**.
> 4. **Root cause pinned to one instruction:** `0x142639d00` calls the context's member-lookup
>    **`[context+0x168]`**, and on our synthesized socket-manager **that slot is a STUB `0x1423fdf00` = `mov
>    eax,1; ret`** (always reject; populates no out-struct — the following `cmp [rbp],0; je reject` would also
>    fail). The context is `[socketmgr+0x48]` (vtable `0x1432770b0`, near `SteamServiceImpl` `0x143277270`); its
>    member collection is `[context+0x170]`. The online flow installs a real lookup + registered members here;
>    our offline standup left the stub.
>
> **⇒ THE remaining piece for host roster → 2 — and why it's a reimplementation, not a wire (RE-verified):**
> gate c's accept needs the host context to hold real per-peer **member** state. Re-read of `0x142639d00`: it
> runs **two** member producers that must both succeed — the lookup `[context+0x168]` (must return ≠1 and set
> the local) **and** the find-or-create `0x142639950(context, sender, &local)` (must return a non-null member) —
> then `0x14263d060` builds the connection descriptor from that member object (a rich struct: sub-graph
> `[[[member+0x18]+0x18]+0xa0]` vtable, fields `+0x40..0x68/+0x100..0x110`; the descriptor's init-callback is
> `0x142639830`, sender stored at `out+0x68`). A focused RE pass (agent, 2026-07-04 night) established: the two
> ctors of the context class (vtable `0x1432770b0`; ctors `0x142639870`/`0x1426398c0`) **zero** `+0x168/+0x170`;
> the only *static* installer of that slot (`0x142639b70` via `0x1423fe030`) belongs to an **unrelated
> lobby/session-info cache** and always writes the same reject-stub `0x1423fdf00`; **no real member-lookup
> exists as a static function** (it's a runtime closure, plausibly over live Steam lobby-member enumeration);
> and the actual writer of `[socketmgr+0x48]` (the context itself) is **vtable/callback-driven and never fires
> offline**. So the member machinery is populated only by the **online/matchmaker session flow** — offline our
> synthesized context permanently has the reject stub. **The register thunk `0x14263b7c0` is a generic
> container-insert, not a SteamID membership register**, and `[context+0x170]` is opaque closure-capture data,
> not a peer collection you register into. ⇒ Passing gate c offline is **avenue (a): synthesize the member
> ourselves** — write a native replacement at `[context+0x168]` **and** stand in for `0x142639950` **and**
> fabricate the member object `0x14263d060` consumes (then still complete the SYN handshake + session promotion
> so the connection yields a roster *message*). That's reimplementing the matchmaker's per-peer member
> construction — a substantial multi-function build, not a single field-wire. Roster-add itself
> (`0x140cb31b0`) has **no offline gate**, so once a real member/connection exists the roster grows to 2.
>
> **Cheaper disambiguation before building (agent's recommendation):** a live `watch-write.py` on
> `[socketmgr+0x48]` and its `+0x168` during a **real online** host start (matchmaker up) to capture the actual
> install site + the real closure fn-ptr — that pins exactly what to reproduce, instead of fabricating the
> member blind.
>
> Levers/code (this session): read-only host instrumentation under `[debug.probes] instrument_host_accept`
> (`host-admit`/`gate-c`/`success`/`roster-add`/`worker-drain` hooks in `session_probe.rs`); the joiner's
> repeating 14-byte SYN on channel 30 + host-role drain suppression in `TransportStandupDriver::drive_p2p`; the
> rung-3 role now derives from `auto_session` (`rung3_role`), so a single shared seed + `--auto-session host|join`
> drives both machines. Correction banked: the earlier "`0x1423f18a0` transport↔session bridge" lead was wrong —
> `0x1423f18a0` is just a locked getter of the container identity `[+0x7f8]`.
>
> **★ HOST REACHED AND STICKS — solo `lobby_state=Host`, `protocol=Ingame`, warped into the co-op world.** The
> full host path now works offline: stand up the socket-manager wrapper at `[container+0x708]`, drive its own
> init (the `SteamServiceImpl` standup works offline — see below), drive create → `TryToCreateSession`, and
> **force host-setup's final online-availability gate `0x140de2620` true** (patch to `mov al,1; ret` — bypassing
> the item-grey online signal, legitimate for offline co-op). Rig-confirmed: `None → TryToCreateSession → Host`,
> `players=1` (`player[0] host=true local=true`), the warp into map `1800001` COMPLETES, and the session HOLDS
> (no teardown, game running, `in_gameplay=true`). Config: `stand_up_transport`+`land_socket_holder`+`drive_create`
> +`drive_session_established`+`suppress_leave` on, `drive_establish_handler` off.
>
> **★ JOINER BUILT + TWO-MACHINE RUN (rig host + Deck joiner).** `SessionJoinDriver` (`[debug.probes] drive_join`)
> drives the join wrapper `0x140cae640` → **`TryToJoinSession`** (holds, no crash), with two join-side gate
> bypasses (`bypass_session_join_gate` + `bypass_session_join_blob_gate` in `app.rs`). Two-machine, rig-confirmed:
> rig = stable `Host`/`Ingame` (in the co-op world), Deck = `TryToJoinSession`, and the legacy P2P transport is
> **live both ways** (`game-p2p — RECV` on both). **► FINAL GAP — the session-layer handshake:** the Deck never
> reaches `Client` and the rig roster stays `players=1`, because bypassing the blob-parse left the joiner's session
> with **no host connection endpoint**. Wire that endpoint (a real SteamID-only blob, or drive the joiner's
> socket-manager to connect to the host SteamID) so the establish handshake flows to `Client`. See milestone #9
> in "HOST-SETUP DRIVE (2026-07-04 pm)" below for the full chain + the two avenues.
>
> **The `SteamServiceImpl` standup WORKS OFFLINE — the "native-builder dead end" was a misdiagnosis.**
> Driving the socket-manager's own init `0x14263a9d0` stood up a real service offline (`init returned 1`,
> `[socketmgr+0x38]` non-null); the service-init check `0x14263f450` always returns true, so the standup
> `0x142638b40` only nulls on `owner==0`. We landed the CORRECT object at `[container+0x708]` (a 0x10-byte
> socket-manager **wrapper**, not a raw connection) and cleared host-setup faults #1–#3 (dispatch, heap, listen
> slot-pool). **Current wall: fault #4 — the socket-manager's spawned WORKER THREAD** (`MTInternalThread`)
> crashes at `0x142640bc0` doing per-connection `SteamInternal_ContextInit` (`[0x144c0d0a4]`→garbage). The full
> writeup + fault chain is **"HOST-SETUP DRIVE (2026-07-04 pm)"** below — read that first; the older
> "NATIVE-BUILD TRACE" section is now marked superseded.
>
> **(Superseded reading, kept for history:)** ~~The driven establish handler REACHES the game's own connection
> builder, but the build fails at the `SteamServiceImpl` standup — offline AND two-machine. Driving the native
> builder is a DEAD END.~~ In short:
> - **SOLVED:** driving create (`0x140cad4c0`) reaches `TryToCreateSession`; the session-established handler
>   (`0x1423f4870`) populates real container state (veto bit `[+0x7c0]` bit 2, identity `+0x7f8`).
> - **The build is now REACHED** (this session): the establish handler `0x1423f2820` runs gate1 (readiness) →
>   gate2 (session-established as `[vtable+0x68]`) → the builder `[vtable+0x80]`. Two corrections unblocked it:
>   the live derived vtable is **`0x1431f8780`** (not the static `0x1431f8360`, so the real builder is
>   `0x1423f46b0` — a plain fn, **no Arxan**), and `drive_session_established` must be **off** (it double-drove
>   `0x1423f4870`, making the handler's own gate2 call return "already established" and bail).
> - **The wall (precise):** the builder constructs an `MTInternalThreadSteamSocketManager@DLNW3D` whose init
>   calls the **`SteamServiceImpl` standup `0x142638b40`, which returns null** → `+0x708` stays null. The
>   descriptor was never the blocker.
> - **TWO-MACHINE RESULT (rig + Deck):** with a real peer **linked** over the rung-2 side-channel, the driven
>   build fails **identically** (standup null, `lobby_state → FailedToCreateSession`). A private-side-channel
>   peer does NOT put the game into its online-session flow, so its DLNW3D transport stays dormant.
> - **⇒ ► NEXT: pivot to path 2 (own-transport standup, the ERSC model)** — resolve `ISteamNetworking006`
>   (`0x142640b90`), instantiate the `SteamServiceImpl`/`SteamConnectionManager` ourselves, register the P2P
>   callbacks, drive connect/accept with the rung-4 peer SteamID64s so a real `SteamConnection` lands at
>   `[container+0x708]`. See "NATIVE-BUILD TRACE" > "► NEXT STEP" and "Standup chain charted (for path 2)".
>
> **Reading guide for the rest of this doc:** the top half (SDK survey, drive requirements, AES key,
> ordering) is the still-useful call spec. The long **"Why a direct create fails offline"** investigation
> (from that heading down to "TRANSPORT CHARTED") is the **historical create-veto RE** that *led* to
> discovering the transport is the wall — kept for its re-derivation facts (the create gates, the
> container fields `+0x7c0`/`+0x708`/`+0x7f8`), but its chronology and its many superseded/disproven
> hypotheses are no longer the live picture. The **TRANSPORT CHARTED**, **RIG-PROVEN**, and **DLNW3D
> service standup chain** sections are the current, load-bearing ones.

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

> **LATEST (2026-07-03): the transport layer is charted + runtime-proved — see "TRANSPORT CHARTED" and
> "RIG-PROVEN: the DLNW3D transport is DORMANT offline" below.** The `[container+0x708]` connection is a
> `SteamConnection@DLNW3D`, part of a separate lower transport namespace (DLNW3D) that rides
> **ISteamNetworking006** (legacy Steam P2P). A live-memory scan in-world offline found **0** DLNW3D
> objects (service/manager/connection) vs 3 live containers — the whole transport is never stood up
> offline, so `+0x708` is null because the layer below it is dormant (the gate is flow-entry, above the
> connection layer). The finish is now two sharp paths: **(1)** crack the flow-entry signal (unifies with
> item-grey), or **(2, recommended, bounded)** stand up the DLNW3D transport ourselves (ERSC-model) —
> service factory `0x142638b40`, connection-creator `0x142640560`, connect/register `0x14263b720`/`0x14263b7c0`,
> all charted. Both terminate at a two-machine (rig + Deck) validation. The VERDICT subsection below is the
> prior (static-inference) framing; the transport sections supersede it with the runtime-proven layer map.
>
> **PRIOR (2026-07-03, static): see the VERDICT subsection below** ("the container-init gate is the
> item-grey signal"). The rung-3 create veto is now root-caused: the veto is *satisfiable* (rig
> milestone `df12f2d` reached `TryToCreateSession` by seeding bit 2 of `+0x7c0` **and** a fabricated
> `+0x708`), but a functional Host needs the game's real session graph. Both fields are written by one
> container vmethod (`0x1423f4870`, ManagerImplSteam vtable slot `+0x68`) gated on the live Steam
> interface/service context, reached through the **same service-manager singleton `0x144842d40` the
> item-grey hunt hit** — so rung-3 create and the greyed multiplayer items are almost certainly one
> signal, and it is **not** `is_offline`. Static walls on that signal (obfuscated leaf) → **finish with
> a runtime execution trace** of it, unified with the item-grey trace. The narrative below is the
> chronological investigation; the finalize-handle / registry-id paths in this first blockquote are
> **superseded** by the L3 + VERDICT sections.

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

### Leg B post-capacity tail (finalize handle) — TOMBSTONE (disproven; superseded by the transport solution)

The "leg B's tail rejects on a zero finalize handle" hypothesis was **rig-DISPROVEN** (2026-07-03): leg
B never reaches its finalize tail on the failure path (the finalize/cleanup hooks never fired). It was
also mooted entirely by the later finding that the real wall is the **transport** (see the STATUS block
at the top + "TRANSPORT CHARTED"), not any leg-B tail reject. Re-derivation facts that survive: the slot
array is three inline `NetworkSession` fields (`+0x18` base / `+0x20` cap / `+0x24` count), and the
finalize helper is `0x1423fab40`. Detail removed to compact; git history has the full disproven chart.
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

### REAL-INIT BREAKTHROUGH (2026-07-03): driving the session-established handler populates REAL session state

Instead of fabricating stubs, `[debug.probes] drive_session_established` drives the container's real
session-established handler **`ManagerImplSteam@DLNR3D::0x1423f4870`** (the one worker create-veto-writer
charted, vtable slot +0x68) with the live container as `this`, at the veto-vmethod entry. **It works:**

```
DRIVING session-established handler 0x1423f4870(container=0x143dcd480) [before: bit2=0 +0x708=0x0]
handler returned; container now [+0x7c0]=0xe bit2=1 +0x708=0x0 +0x7f8=0x110000102a760b8
```

The handler passed its live-Steam-interface gate (Steam is up in our process) and did the **real** init:
`[container+0x7c0]` = `0xe` (bits 1/2/3 set — bit 2 = the create veto, now genuinely set, not forced),
and `+0x7f8` = `0x110000102a760b8` = **our real SteamID64** (76561198004789432). So the "cold subsystem"
is warmable: **driving the one real handler brings up the genuine session state** — this is the ERSC-model
mechanism, reachable as a targeted drive. That materially **refines the worker's "unbounded graph, no
lever" verdict:** it is *not* unbounded fabrication — a single real handler populates the session-level
state coherently.

**What remains: `[container+0x708]` (the DLNR3D connection object) is still null after the handler** — so
create still crashes at the same `ConnectionRefInfo` ctor (`0x1423f3230`, `lock xadd [null+8]`). `+0x708`
is a *per-connection* object, populated by the DLNR3D **network-connection** layer when a real connection
forms — not by session-established setup (the handler sets identity + status but no connection). So the
last piece of rung-3 create is the DLNR3D connection: either drive the connection-established handler too
(the sibling of `0x1423f4870`), or have a real DLNR3D peer connection populate it. Our rung-2 side-channel
(a *separate* Steam P2P channel) does **not** populate the game's DLNR3D `+0x708` — the game's own
network layer must. **Next: chart/drive the DLNR3D connection-established path** (the handler that writes
`+0x708`), which with the session state now warmable is the final gate to `Host`. `drive_session_established`
stays committed but off (crashes on the null connection). This is the closest rung-3 create has come: real
session state up, one connection object short.

**Sharpened final gate (2026-07-03): the `ConnectionRefInfo` crash is a LOOP over `max_players`.** In
helper `0x1423faf60` the ctor call sits in a loop: `0x1423fb05a: cmp [rdi+0x68], r15d; jbe skip` (rdi =
session_obj, r15 = counter), and `[session_obj+0x68]` = our **`max_players` (6)** (the field the gate-4
tracer logged as `helper[+0x68]=6`). So create builds **6 connection slots**, each a `ConnectionRefInfo`
wrapping `[container+0x708]`, and faults on the first because `+0x708` is null. Two levers this exposes
for the next attempt: (a) `[session_obj+0x68]` (the connection count) — for a host with no peers this
arguably should be 0/1, and `max_players=6` may be over-driving it; (b) `[container+0x708]` needs a real
DLNR3D **connection** (`SessionSteam`) object, populated by the connection layer. **The genuine
remaining work is the DLNR3D connection/transport layer** — a real game-network connection (the ERSC
model routes DLNR3D over its own Steam P2P) or driving the connection-established handler with the loop
count matched to reality. That's a networking-layer RE effort (next session), not one more field poke:
create's *initiation* and *session state* are solved; the *transport* is the last subsystem — the same
one ERSC had to reimplement.

**Conclusive (2026-07-03): the connection object cannot be faked — it must be a real DLNR3D
connection.** Ran the strongest combination — drive the real session-established handler (real session
state: bit 2, real SteamID) **plus** fabricate `[container+0x708]`. Result: `drive-create returned true
→ TryToCreateSession` (again), then the **same** crash as fabricate-alone at `0x14203f1f0` (read
`[null+8]`), whose caller `0x1423f6c00` is a vtable-dispatch loop that calls `[elem]`/`[elem+0x10]`/
`[elem+0x68]` on each session member. A hollow `+0x708` has no valid vtable, so the session machinery
faults calling into it. So across all three fabrication attempts (hollow-only, real-state+hollow,
force-bit) the wall is identical: **the running session dispatches vmethods on the connection object, so
it must be a real, coherent DLNR3D `SessionSteam` connection — fabrication is definitively a dead end.**
The finish is unavoidably the DLNR3D connection/transport layer: two real ELDEN RING instances forming
a game-native connection over Steam P2P (ERSC's networking model), driven with `[session_obj+0x68]`
(the connection count) matched to the real roster. That is the scoped next effort — a networking
subsystem, empirically proven un-shortcuttable, not another field poke.

### TRANSPORT CHARTED (2026-07-03, static) — the `+0x708` connection is a `SteamConnection@DLNW3D`; the whole DLNW3D P2P layer is now mapped

The previously-black-box "`+0x708` needs a real connection" is now a **charted layer**. The connection
object is not a DLNR3D type at all — it lives in a **separate lower namespace, `DLNW3D`** (Dantelion
network **W**ire/transport, vs `DLNR3D` = the **R**untime/session layer built on top). Static RE on the
same pinned 2026-06-02 image (base `0x140000000`; own words, addresses are facts, no decompiler output).

**The two-layer architecture (the ERSC transport surface, exactly):**

```
DLNR3D (session/game layer):  ManagerImplSteam (container, vtable 0x1431f8780)
                              → NetworkSession/SessionManagerSteam (container+0x710)
                              → SessionSteam (leg-B 0x5f8 obj) → ConnectionRefInfo (wraps [container+0x708])
    │ built on
DLNW3D (Steam P2P transport): SteamServiceImpl → SteamConnectionManager → SteamConnection
                              + SteamSocket / SteamSocketManager (MTDispatched/MTInternalThread = worker threads)
                              + FsdpConnection / FsdpConnectionManager / FsdpConnectionBufferContainer (protocol framing)
                              + ProtocolConnectionAdapter
                              + CCallback<SteamCallbackWrapper>  → P2PSessionRequest_t / P2PSessionConnectFail_t
```

Full DLNW3D RTTI set (from the image's `.?AV…@DLNW3D@@` names): `SteamServiceImpl`, `ServiceImpl`,
`SteamConnectionManager`, `SteamConnection`, `MTBaseConnection/Socket/SocketManager`,
`MTDispatchedConnection/Socket/SocketManager/SteamConnection/SteamSocket/SteamSocketManager`,
`MTInternalThread…` (same set), `FsdpConnection/Manager/BufferContainer`, `ProtocolConnectionAdapter`.

**1. The transport is `ISteamNetworking006` — the LEGACY Steam P2P API** (`SendP2PPacket` /
`ReadP2PPacket` / `AcceptP2PSessionWithUser` + the `P2PSessionRequest_t` callback), **not**
`SteamNetworkingSockets`/`Messages` (neither string is in the image; only `SteamNetworking006` at
`0x143277fd0`). Interface holder (`SteamInternal_ContextInit` target) = **`0x143c602b0`**, resolved by
`0x142640b90` (`SteamInternal_FindOrCreateUserInterface("SteamNetworking006")`). This is the whole
transport primitive set the ERSC-model reimplementation talks to. **Wrappers over the interface vtable**
(all reached via holder `0x143c602b0`):
| Wrapper fn | ISteamNetworking006 slot | Steam call |
|---|---|---|
| `0x142640b20` | `[iface+0x00]` | **SendP2PPacket** (caller `0x142643dd0` = SteamConnection send: `rdx=[conn+0x128]`=peer, `rcx=[conn+8]`=iface) |
| `0x142640bc0` | `[iface+0x10]` | **ReadP2PPacket** (= SteamConnectionManager vtable slot 3, the poll) |
| `0x1426408b0` | `[iface+0x18]` | **AcceptP2PSessionWithUser** |
| `0x142641150` | `[iface+0x28]` | **CloseP2PChannelWithUser** |

**2. `[container+0x708]` holds a `SteamConnection@DLNW3D`** (vtable **`0x143278370`**, base vtable
`0x143278358`; ctor **`0x142643b50`** installs both).
> **CORRECTED 2026-07-04 — see "SEAM CHARTED" below.** `[container+0x708]` does **not** hold the raw
> `SteamConnection` directly. It holds a **`SocketManagerHolder@DLNR3D`** (a 0x18-byte
> `ReferenceCountObject` subclass) that *wraps* the `SteamConnection` at its own `+0x10`. This matters:
> the holder's `+0x8` is a **refcount** (create does `lock xadd [holder+8]`), whereas a raw
> `SteamConnection`'s `+0x8` is the *iface* — so writing a bare `SteamConnection` at `+0x708` (as the
> "real-connection test" below proposed) would corrupt the iface pointer and mis-wrap. The rest of this
> paragraph (the `SteamConnection` internals) is correct *about the wrapped connection at holder+0x10*.
> Its slot 0 (`0x142643dd0`) is send-on-connection:
it reads `[conn+0x8]` = the ISteamNetworking interface and `[conn+0x128]` = the **peer SteamID64**, then
calls the SendP2PPacket wrapper. So the connection object the DLNR3D `ConnectionRefInfo` loop wraps is a
low-level Steam P2P connection carrying `{iface, peerSteamID}` — exactly what `AcceptP2PSessionWithUser`
/ `SendP2PPacket` operate on.

**3. Connection-creation entry points (the "listen-connection creator" the goal names):**
- **`0x142640560`** — the SteamConnectionManager's *create-a-connection* method: `rcx` = manager, `rdx`
  = a **params struct** (`[rdx]!=0`, `[rdx+0x18]!=0` guarded; copies 0x30 bytes of config — buffer sizes
  — into `[manager+0x40..0x70]`; `+0x5c` ring size defaults `0x4b0`, clamped `≤0x7ff`), allocates the
  connection's ring buffers (`0x142641a40`/`0x142641b90`/`0x142641e30`/`0x142641ce0`), constructs the
  `SteamConnection` (`0x142643b50`), and runs the per-connection Accept/setup `0x14263ffe0`.
- **`0x14263ffe0`** — per-connection Accept + wire-up: registers the connection's callbacks
  (`0x14263fcf0`/`0x14263fd00` via `0x1426440b0`), initializes fields (`+0x70/+0xa8/+0xf0/+0x118`), and
  calls `AcceptP2PSessionWithUser`. Called by both the create path (`0x142640560`) and the register path.
- Service entry thunks (Arxan-obscured, tail-jump into the methods): **`0x14263b720`** = *create/connect*
  (`(service, params)`: allocs a `0x1b8` connection-manager via ctor `0x14263f700`, then `0x142640560`);
  **`0x14263b7c0`** = *register/activate an existing connection* (`(service, connection)`: runs
  `0x14263ffe0` on it, then hooks it into the service's collection via `[[service+8]]+0x68`). Both are
  dispatched (0 direct callers) from the DLNW3D service factory region `0x142638b40..0x142638c40`.
- `SteamServiceImpl@DLNW3D` ctor = `0x14263b6e0` (installs vtable `0x143277270`), base ctor `0x14263b6b0`
  called from factory `0x142638b40`. The service/manager are **factory+vtable dispatched, 0 static
  callers** — i.e. instantiated dynamically by the online session flow, which is why static can't reach
  the trigger (the same runtime-resolution wall the VERDICT below describes).

**4. Why this refines the finish (and one NEW untested lever).** The container ctor `0x1423f20b0` only
**zeroes** `+0x708` (at `0x1423f2159`, right before the embedded `NetworkSession` at `+0x710`); no
DLNR3D-cluster code writes a real value there. `+0x708` is populated by the **DLNW3D layer** when a real
Steam P2P `SteamConnection` forms — created by `SteamConnectionManager` either on the host's own
listen/create (`0x14263b720`/`0x142640560`) or on an incoming `P2PSessionRequest_t`
(`AcceptP2PSessionWithUser` → register `0x14263b7c0`). Two consequences:
- Every fabrication attempt failed because a hollow `+0x708` has no real `SteamConnection` vtable
  (`0x143278370`); the session machinery dispatches its vmethods and faults. A **real DLNW3D
  `SteamConnection`** at `+0x708` (never yet tested — the doc only ever fabricated a *hollow* one) has
  valid vtables and might survive the `0x1423f6c00` collection-dispatch. That is the highest-EV next
  experiment, but it requires the DLNW3D service+manager to exist, which offline they may not (see 3).
- The connection carries `{iface=[+8], peerSteamID=[+0x128]}` and rides `SendP2PPacket`/`ReadP2PPacket`
  on channel-scoped `ISteamNetworking006`. This is precisely the surface ERSC drives: feed peer
  SteamID64s directly (from rung-4 lobby discovery) into the SteamConnectionManager and let the game's
  own DLNW3D transport connect over Steam P2P — no FromSoft matchmaking server. **The reimplementation
  target is now concrete:** stand up / drive `SteamConnectionManager@DLNW3D` for the resolved peers so a
  real `SteamConnection` lands at `[container+0x708]`, with `[session_obj+0x68]` (connection count)
  matched to the roster.

**Next-experiment spec (for the next rig/two-machine run):**
1. **Observe first (read-only, safe):** at boot / title / in-world offline, walk the container
   (`ManagerImplSteam@DLNR3D`, live ~`0x143dcdxxx`) and check whether a DLNW3D `SteamServiceImpl`
   (vtable `0x143277270`) / `SteamConnectionManager` (vtable `0x143278020`) is instantiated at all. If
   the service exists idle offline, the connect path is drivable solo with the Deck's SteamID; if not,
   the service itself is gated on the online signal (same as `+0x7c0`/`+0x708`) and the two-machine
   game-native connection is required.
2. **Real-connection test (if the manager exists):** drive `0x142640560` (or the service create thunk
   `0x14263b720`) with a params struct pointing at the peer SteamID64, capture the resulting
   `SteamConnection`, store it at `[container+0x708]`, then drive create — watch whether it survives the
   `ConnectionRefInfo` loop **and** the `0x1423f6c00` collection-dispatch that hollow `+0x708` crashed on.
   Guard with `watch-bt.py` on `container+0x708` to capture the write chain / any crash backtrace.
3. **Two-machine (the doc's baseline):** rig host + Steam Deck joiner; the connection forms game-native
   over Steam P2P. The open question is the *trigger* — whether driving the DLNW3D
   connect/listen with the peer's SteamID (ERSC model) brings up the SteamConnection, or whether the
   whole DLNW3D service is dormant until the online-availability signal (VERDICT §7) is set.

**Re-derive after a game update:** the connection type is `.?AVSteamConnection@DLNW3D@@` (RTTI off vtable
`[-8]` COL); its send slot 0 reads `{+0x8=iface, +0x128=peerSteamID}`; the transport is whichever
`SteamNetworking0NN` string the image contains (resolver = its lone rip-ref), and the wrapper fns are the
`call [holder-resolved iface + slot]` sites; the connection-creator is the manager method that constructs
a `SteamConnection` and calls the `AcceptP2PSessionWithUser` wrapper.

### HOST-SETUP DRIVE (2026-07-04 pm, rig) — the SteamServiceImpl standup WORKS OFFLINE; wall is now the socket-manager worker thread

> **HEADLINE — the "standup returns null offline" claim below is WRONG (misdiagnosis, now corrected).** Driving
> the socket-manager's own init `0x14263a9d0` with a hand-built descriptor **stood up a real `SteamServiceImpl`
> offline**: `socketmgr init returned 1`, `[socketmgr+0x38]service = <non-null heap ptr>`. The service init
> check `0x14263f450` **always returns true** (both branches `mov al,1`), so the standup `0x142638b40` only
> returns null when its `owner` (rcx) is 0 — it is **not** online-gated. The earlier "standup returns null"
> reading came from the removed `svc-standup` probe perturbing flags mid-function; the native-builder path was
> abandoned on a false wall.
>
> **What this session proved (all rig-logged 2026-07-04 pm; solo, `drive_session_established`+`force_host_transition`
> on, `stand_up_transport`+`land_socket_holder`+`drive_create` on, `drive_establish_handler` off):**
> 1. **`[container+0x708]`'s `SocketManagerHolder` holds a 0x10-byte WRAPPER at `+0x10`, NOT a raw connection.**
>    The wrapper is `{ vtable=0x143276a00, [+8]=socketmgr }` around an `MTInternalThreadSteamSocketManager@DLNW3D`
>    (0x150 bytes, vtable `0x143276cb8`). Host-setup dispatches `wrapper->[+8]socketmgr->vtable[3]` (slot `+0x18`).
>    Landing a raw `SteamConnection` there made host-setup read a connection data field (`[conn+0x20]`) as a
>    vtable → garbage `0x100000000` → fault. Re-derive: builder body `0x142637440` (alloc 0x150 → socketmgr ctor
>    `0x142638140` → init `[vt+8]` → on success alloc 0x10 → wrapper init `0x14203f100(wrap, sm)` → `[wrap]=0x143276a00`).
> 2. **The socket-manager standup succeeds offline** (see headline). Descriptor from the native trace:
>    `[0]=owner=[container+0x48]`, `[8]=0x1423f2d70` (non-null, satisfies the sub-init's 2nd null-check),
>    `[0x10]=container`; the sub-init `0x14263ce40` copies `descriptor[0..0x60] → socketmgr[0x40..0xa0]` then
>    calls the standup with `owner=descriptor[0]`. **Seed the descriptor from the socketmgr's post-ctor state**
>    (preserve the base-ctor config defaults at `+0x58/0x5c/0x60/0x74..0x9c`) or the worker spins up misconfigured.
> 3. **Host-setup fault chain — charted + cleared in order** (each fix exposed the next, deeper one):
>    - **#1 dispatch** `0x14203f1f0` (`wrapper->socketmgr->vtable[+0x18]`): fixed by landing the socket-manager
>      wrapper instead of a raw connection.
>    - **#2 null heap** `0x14263845f` (`r8=[socketmgr+0x40]`; allocator `0x141eb9ed0` derefs `[r8]`=null): fixed
>      by the full init (which sets `+0x40`). (Do **not** pre-set `+0x40` — the sub-init bails if it's non-null.)
>    - **#3 empty listen slot-pool** `0x14263aff0` (free-list `[socketmgr+0xd0]`=0xffffffff → no free slot → the
>      failed listen releases the sub-object → cleanup faults on its null vtable): fixed by the init sizing the
>      pool at `socketmgr+0xc0` from `[socketmgr+0x60]` = `descriptor[0x20]` (the connection count).
> 4. **FIXED — fault #4 was OUR bug corrupting the SteamInternal context.** The socketmgr worker thread
>    (`MTInternalThreadSteamSocketManager`, `0x142640bc0`) calls `SteamInternal_ContextInit(holder 0x143c602b0)`,
>    which invokes the context's `pFn` (at `[holder+0]`) once to resolve the iface. Our standup used to call the
>    raw resolver `0x142640b90(holder)` **directly** — but that resolver IS the `pFn`, and it does `mov [rbx],rax`
>    (`rbx`=arg), so calling it with the holder BASE stored the iface at `[holder+0]`, **overwriting `pFn`**. The
>    worker then read the corrupted `pFn` (now the iface) and called the iface as a function → execute-garbage
>    (changing per run). **Fix: resolve via `SteamInternal_ContextInit` (the IAT import `[0x144c0d0a4]`), the same
>    idempotent path the game uses** — it leaves `[holder+0]=pFn` intact and lands the iface at `*[holder+0x10]`.
>    With this, host-setup runs to completion, no crash.
> 5. **NEW MILESTONE — the host session FORMS, then a session-layer online-availability gate resets it.** After the
>    fix, driving create (either `force_host_transition` or the natural session-update task) advances:
>    `TryToCreateSession` → **`protocol_state=Ingame`, `players=1` with `player[0] host=true local=true`, and a warp
>    into the co-op map begins** (`start_area_id=1800001`, `warp_delay` from ~10s). So host-setup genuinely creates
>    the session and starts warping us into the shared world — the furthest the project has reached. But `lobby_state`
>    goes to `None` at create-completion (not `Host`), and ~2s later the roster drops `1 -> 0` and protocol resets to
>    `None`.
> 6. **The teardown is NOT `leave_session` and NOT the async handler — it's host-setup's own final validity gate.**
>    Read-only hooks on `leave_session 0x140cae730` AND the async teardown handler `0x1423f46d0` **neither fired** at
>    the reset. The reset is inside `0x140cb2ae0`'s own tail: it ends with `call 0x140ddfb20` (→ `0x140de2620`), a
>    validity check that reads `rcx=[0x143d855c8]; if [rcx+0x10]==0 return false` and otherwise touches the
>    **online-session-manager singleton `0x144842d40`** (creating it via `0x141eceb10` if null) — **the same
>    online-availability signal that gates the greyed multiplayer items** ([OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)).
>    On `false`, `0x140cb2ae0` calls `0x140cb3b80(this, dl=1)` — the degraded/reset path. So the final wall for a
>    *sticking* solo `Host` is that one online-flow-availability signal: the transport builds, the standup works, the
>    session forms and starts warping — then the session layer's "are we really in an online session?" gate says no
>    and unwinds it. This is the docs' long-standing "path 1 / elusive online-flow signal."
>    **► NEXT (two independent tracks):**
>    - **(a) Satisfy the online-availability gate** — chart `[0x143d855c8]+0x10` and the `0x144842d40` singleton
>      state a real online session leaves them in, and whether it's seedable offline (ties into OFFLINE-ITEMS). If
>      solved, the solo host should stick and the warp complete.
>    - **(b) Two-machine with a real peer** — the reset may relax once a real joiner's connection backs the session.
>      Needs a **joiner driver** (drive the join wrapper → `Client`), which doesn't exist yet — the create driver is
>      host-only. Build that, then rig-hosts / Deck-joins within the window.
>    Diagnostics added this session (`session_probe.rs`): read-only `leave-session` + `teardown-handler` hooks, and a
>    `[debug.probes] suppress_leave` patch lever (repurposed to force the host-validity gate true — this is what
>    made the host stick).
> 7. **★ HOST STICKS (rig-confirmed).** With the gate forced: `None → TryToCreateSession → Host`, `protocol=Ingame`,
>    `players=1` (`player[0] host=true local=true`), warp into map `1800001` completes (`warp_pending` clears,
>    `in_gameplay=true`), session HOLDS (no teardown, game running). The solo host is DONE.
> 8. **JOIN DRIVER — reaches TryToJoinSession (built + rig-confirmed).** `SessionJoinDriver` (`[debug.probes]
>    drive_join`) mirrors the create driver: builds a minimal blob from the rung-4 host SteamID64 and calls the join
>    wrapper `0x140cae640` → inner `0x140cb2470`. Two join-side gate bypasses (in `app.rs` under
>    `bypass_session_create_gate`, join-specific bytes so inert on the host): `bypass_session_join_gate` (the
>    availability gate `0x140cb4b50` call site `E8 DB 25 00 00 … 75 07`, flip `jne`→`jmp`) and
>    `bypass_session_join_blob_gate` (the blob-parse result gate `41 FF 52 10 89 43 28 85 C0 75 04`, flip `jne`→`jmp`
>    — our synthesized host produces no real matchmaker blob). Rig-confirmed: the driven join returns true and holds
>    at `lobby=TryToJoinSession` (no crash/teardown), waiting for a host connection.
> 9. **★ TWO-MACHINE (rig host + Deck joiner) — both in the correct FSM states, transport live, session handshake
>    NOT yet complete.** Rig drives create → stable `Host`/`Ingame` (in the co-op world); Deck drives join → holds at
>    `TryToJoinSession`; the legacy `ISteamNetworking006` P2P transport is confirmed bidirectional between them
>    (`game-p2p — RECV` on both). But the Deck never advances to `Client` and the rig roster stays `players=1` — the
>    game's **session-layer join handshake doesn't run over the transport**. Root cause: we bypassed the blob-parse
>    (`bypass_session_join_blob_gate`), so the joiner's session has **no host connection endpoint** to reach — the
>    blob is what normally wires the joiner's session to the host's connection. **► FINAL GAP / NEXT:** wire the host
>    connection into the joiner's `TryToJoinSession` session so the game's establish handshake flows to `Client`
>    (roster → 2 on the host). Two avenues: (a) chart the minimal blob fields the parse (`[r10+0x10]` on `[begin,end)`)
>    needs to set the host endpoint, and supply a real (SteamID-only) blob instead of bypassing; or (b) after
>    `TryToJoinSession`, drive the joiner's stood-up socket-manager to CONNECT to the host SteamID (the game's connect
>    thunk `0x14263b720`) and have the host accept (its P2P callbacks are registered by the connection-creator), so
>    the game's session-establish packets cross our proven transport. The join inner's post-blob calls to chart:
>    `0x1423f1930` (network-session setup on `[this+0x60]`), `0x140caeb30`, `0x140cb55b0` (`[this+0x2f0]`).
> 10. **EXACT STUCK POINT charted — the joiner has no connection handle `[session+0x24]`.** The session-update task
>    `0x140cafd10` gates its whole per-frame body on `cmp [r14+0x24], 0; je skip` (r14 = CSSessionManager). `[+0x24]`
>    is the **network connection handle**: the task fetches the net-session (`[r14+0x60]` → `0x1423f1920`) and polls
>    the handle's status (`call [netsess_vt+8]` with `edx=[r14+0x24]`); a live handle drives the host branch
>    (`0x140cb2ae0`) or, past the roster loop, the **Client transition `0x140cb2f80`** (called unconditionally once
>    the join branch is entered — so Client is NOT separately gated; the block is the handle). On the joiner `[+0x24]`
>    is **0** (the bypassed blob never established the connection), so the task early-returns every frame and the
>    joiner sits at `TryToJoinSession` forever. **⇒ The precise remaining work: establish a real connection to the
>    host so `[session+0x24]` becomes a live handle** — drive the joiner's socket-manager connect to the host SteamID
>    (avenue b), obtain the game's connection handle, route it to `[r14+0x24]`, and have the host accept so its roster
>    grows. This is the genuine session-establish piece; the transport underneath it is already proven two-machine.
> 11. **DEEPER: the joiner polls `[G+0x28]`, not `[G+0x24]` — and `[G+0x28]` is the JOIN registry handle.** Re-read
>    of the update task: `cmp [r14+0x24],0; je 0x140caff11`, and **`0x140caff11: cmp [r14+0x28],0; je skip`** — so
>    the JOINER branch polls `[G+0x28]`. Both handles come from the same connection registry
>    `registry = *(G+0x60)+0x710` (rig-logged live: `vtable 0x1431f9140`), whose vtable slot **`+8` = `0x1423f5c00`**
>    (create/leg-B → `[G+0x24]`) and **`+0x10` = `0x1423f62e0`** (join, connection-from-blob → `[G+0x28]`). The join
>    inner calls `0x1423f62e0(registry, descriptor, blob_begin, blob_len)`; it returns **0** for our dummy blob, so
>    `[G+0x28]=0`. Inside `0x1423f62e0`: a registry-ready check (`0x141eba210` on `[registry+0x10]`), two descriptor
>    validations (`[reg_vt+0xe8]`, `[reg_vt+0x108]` → a connection object), then the blob parser
>    **`0x1423fb260(conn, blob_begin, blob_len, …)`** (needs `begin!=0 && len!=0` — our 8-byte blob passes — then
>    processes the blob into `[conn+0x58]+0x1e8`); on success it appends the connection to the registry array
>    (`[registry+0x18][count]=conn; inc [registry+0x24]`) and yields the handle. **⇒ Precise next target: make
>    `0x1423f62e0` return a nonzero handle** — instrument its four fail branches to find which check rejects our blob
>    (registry-ready / descriptor / create-conn / blob-parse), chart the minimal blob `0x1423fb260` accepts (likely a
>    SteamID-only structure into `[conn+0x58]+0x1e8`), and supply it instead of the bypass. Then `[G+0x28]` is a live
>    handle, the update task polls it, and TryToJoinSession advances to `Client` as the host connection completes over
>    the proven transport. (Diagnostic added: the join driver logs `[G+0x24]/[G+0x28]` + the registry vtable/slots.)
> 12. **JOINER NOW CREATES A CONNECTION + HANDLE — the blocker was an uninitialized registry, not the blob.**
>    Localized `0x1423f62e0` with entry + blob-parse hooks: it fires but the blob parser `0x1423fb260` is **not**
>    reached, so it fails at its FIRST check — the registry-ready `0x141eba210` (which is just `return [registry+0x10]`).
>    Live read confirmed the joiner's registry (`*(G+0x60)+0x710`) is **fully uninitialized**: `[+0x10]=0`, array
>    `[+0x18]=0`, cap `[+0x20]=0`, count `[+0x24]=0`. The host's create inits it; the join expects it pre-inited. The
>    join driver now **initializes it** (ready `[+0x10]=1`, a leaked slot array `[+0x18]`, cap `[+0x20]=16`, count 0)
>    before the join call. Result (rig solo AND two-machine): `0x1423f62e0` passes the ready check, the game **CREATES
>    a connection** (`[conn+0x58]` populated), registers it (`count=1`), and returns a handle — **`[G+0x28]=1`** at
>    last, joiner reaches `TryToJoinSession`. **But it resets to `None` in ~9 frames** (two-machine, host up), and
>    neither `leave_session` nor the teardown handler fires — so the update task's poll returns "failed" immediately.
>    **⇒ Two remaining causes, both from the FAKED registry init:** (a) the connection-create registers into the
>    `+0x710` registry, but the update task polls the **`+0x670`** net-session (`0x1423f1920`) sub-object — my fake
>    `+0x710` init does NOT establish the real `+0x670`↔`+0x710` linkage a proper init would, so the poll with
>    `handle=1` finds nothing → immediate fail; and (b) the created connection has **no host peer** (the blob would
>    set it — our raw 8-byte SteamID isn't the right format, and the peer isn't bound on the connection). **► NEXT:
>    initialize the net-session/registry the GAME's way** (find the init that links `+0x670`/`+0x710` and allocates
>    the array — the host's create does it; chart its writer of `[registry+0x10]`), then wire the host peer into the
>    created connection (valid blob or bind the peer at the connection's peer offset). Then the poll should see a real
>    connecting→connected status over the proven transport and advance to `Client`. This is the genuine net-session
>    init + peer-wire piece; everything up to a live `[G+0x28]` handle now works.
> 13. **JOINER REACHES CLIENT (forced) — then JoinCheck needs the host's real session data.** The update task's
>    joiner branch resets on any nonzero connection-status poll (`mov rbx,rax` at `0x140caff36`; `rbx!=0` →
>    `0x140cb3b80` reset, `rbx==0` → roster loop + Client `0x140cb0076`). Our connection returns a not-connected
>    status. Forcing the poll to 0 (`xor rbx,rbx`, `force_join_poll_connected` in app.rs — joiner-branch-only, inert
>    on the host) **advances the joiner `None → TryToJoinSession → Client`** (`protocol=JoinCheck`, `players=1`,
>    rig-confirmed). It then **faults ~30s later in JoinCheck** at `0x1403f4860` (`movzx eax,[rcx+0x1c5]`) because
>    `rcx=[r14+0x1e508]` is **null** — the client reading host-provided session data (roster/world/session-key) that
>    only exists once the host actually transmits it over a real connection. **⇒ CONCLUSION / the true remaining
>    work:** every forced shortcut (host online-gate, joiner poll) reaches the right FSM state but the underlying
>    connection is not a real game session connection to the peer, so JoinCheck (joiner) / world-sync has no host
>    data and the host never sees the joiner (roster stays 1). Completing "two sessions in each other's worlds"
>    genuinely requires the game's **session-establish protocol to run over the transport** — the joiner's DLNR3D
>    connection actually connecting to the host over `ISteamNetworking006` (proven working), the host accepting via
>    its registered P2P callbacks, and the host↔joiner join-data/world-sync exchange. That is the core FromSoft
>    session netcode between two independently-synthesized offline sessions — a substantial multi-session RE +
>    reimplementation with real offline-feasibility uncertainty, not a further one-field wire. This session took it
>    from "believed impossible" to: host lives in the co-op world; joiner reaches Client; transport proven
>    two-machine; the exact remaining protocol layer charted (net-session `+0x670`/`+0x710`, `0x1423fa1b0` hash-map
>    registration, the JoinCheck host-data derefs).
>
> Levers/code: the socket-manager wrapper build + `dump_conn_graph` are in `session_probe.rs`'s `TransportStandupDriver`;
> the full-init drive is in `land_socket_holder`. Config: `drive_session_established=true` (real bit2 → create passes
> the veto → `TryToCreateSession`), `force_host_transition=true` (drive `0x140cb2ae0` to reach host-setup).

### NATIVE-BUILD TRACE (2026-07-04, rig) — SUPERSEDED: the "standup null offline" wall was a misdiagnosis (see HOST-SETUP DRIVE above)

> **⚠ SUPERSEDED by "HOST-SETUP DRIVE (2026-07-04 pm)" above.** The "SteamServiceImpl standup `0x142638b40`
> returns null offline" conclusion in this section is **WRONG** — driving the socket-manager init directly
> stood up a real service offline (init returned 1, service non-null). The standup only nulls on `owner==0`;
> its service-init check always returns true. This section's two-machine reasoning (a real peer's Steam context
> is needed) rests on that false wall and no longer holds. Kept for the address chart only.

> **STATUS (supersedes "RESULT 5 / viable offline" below — that reading was wrong).** The driven establish
> handler now **reaches the game's own connection builder** and runs it to the point where it tries to stand
> up the DLNW3D transport. The offline wall is precise and deep: the **`SteamServiceImpl` service-standup
> factory `0x142638b40` returns null offline**, so the socket-manager build fails cleanly and `+0x708` stays
> null. This is the transport-dormant fact pinned to one function. **The descriptor was never the blocker.**
> The path to `Host` is now a **two-machine** run (a real peer's Steam P2P context is what lets the service
> stand up). Reproducible baseline: `drive_create` + `drive_establish_handler` on, `drive_session_established`
> **off** (see the gate2 fix below).

**Four corrections to the prior "RESULT 5" writeup (all rig-verified 2026-07-04):**

1. **The "Arxan builder `vtable[0x80]` = `0x14251c480`" was a WRONG-VTABLE artifact.** That trampoline was
   read from the *static base* vtable `0x1431f8360`. The **live** container is `ManagerImplSteam` with derived
   vtable **`0x1431f8780`** (the `vmethod-target` probe already flagged this). The real slots are
   `[0x1431f8780+0x80] = 0x1423f46b0` (the builder — a **plain thunk**, not Arxan) and `[+0x68] = 0x1423f4870`
   (the session-established handler). So no runtime Arxan capture is needed; the builder is statically
   readable. (Re-derive: read `[live_vtable+slot]` from `/proc/<pid>/mem`; the live vtable is off the
   `vmethod-target` capture or `[container]`.)
2. **`+0x42` is NOT a bail localizer.** The handler's cleanup `0x1423f2f30` **resets `+0x41` and `+0x42` to 0**
   on every bail (`0x1423f2fa5`/`0x1423f2fd7`), so `+0x42=0` can't distinguish "failed the readiness gate"
   from "passed it and failed later." RESULT 5's "bail was `vtable[0x80]`" inference rested on this and is
   void. Use the `gate2-ret` (`0x1423f289c`) + `builder-entry` (`0x1423f46b0`) hook localizers instead.
3. **The real bail *was* the session-established gate — because we drove it TWICE.** The handler calls
   `[vtable+0x68] = 0x1423f4870` itself as its second gate (`0x1423f2899`). Pre-driving it via
   `drive_session_established` made that internal call return `al=0` (idempotent "already established"), so the
   handler bailed before the builder. **Fix: `drive_session_established = false`** — let the establish handler
   own the first call. Then `gate2-ret` reads `al=1` and `builder-entry` fires.
4. **`+0x708` null offline is the service standup, not a descriptor gap.** With gate2 passing, the handler runs
   the builder `0x142637440`, which constructs an **`MTInternalThreadSteamSocketManager@DLNW3D`** (ctor
   `0x142638140`, vtable `0x143276cb8`) and inits it (`0x14263a9d0` → sub-init `0x14263ce40`). The sub-init's
   first real step is `call [obj_vtable+0x50]` (`0x142638590` = `mov rcx,[rcx+0x40]; jmp 0x142638b40`) — the
   **`SteamServiceImpl` standup `0x142638b40`**, which returns **null offline**. Sub-init takes its clean
   failure path → builder returns null → handler returns 0, no crash. (The descriptor's non-null fields
   `local[0x00]=[container+0x48]`, `local[0x08]=0x1423f2d70` already satisfy the sub-init's null-checks; the
   wall is below them, at the service standup.)

**The full native-build path, end to end (own words; addresses are facts):**

```
establish handler 0x1423f2820(container, descriptor)      driven at the veto hook
  gate1 0x1423f5190 (readiness)                            standalone returns 1
  gate2 call [vtable+0x68] = 0x1423f4870 (session-established)   al=1  ← only after NOT pre-driving it
  test [container+0xa0] & 0x40                              set → build path (clear → copy-only, returns 1)
  build local_struct at [rbp-0x49]: local[0]=[container+0x48], local[8]=0x1423f2d70, local[0x10]=container,
     local[0x18..]=descriptor dwords [desc+0..0x38] + byte [desc+0x3c]
  builder call [vtable+0x80] = 0x1423f46b0(container, &local, [desc+0x3d])   thunk: [desc+0x3d]?0x1426372e0:0x142637440
    0x142637440: construct MTInternalThreadSteamSocketManager (0x142638140, vt 0x143276cb8, 0x150 bytes)
      init [obj_vt+8] = 0x14263a9d0(obj, &local)
        sub-init 0x14263ce40(obj, &local): null-checks local[0]/local[8] (pass), copies local[0..0x60]→obj[0x40..0xa0]
          call [obj_vt+0x50] = 0x142638590 → SteamServiceImpl standup 0x142638b40   ← RETURNS NULL OFFLINE
        → sub-init clean-fails → init fails → builder returns null → +0x708 stays null
```

**Rig-guide/config for this run:** `[debug] guide="rung3-create-drive"`, `[debug.probes] drive_create=true
drive_establish_handler=true drive_fire_solo=true drive_session_established=false`. The localizer log lines:
`gate2-ret … al=1`, `builder-entry REACHED …`, `0x1423f2820 returned 0; [container+0x708]=0x0`.

**⚠ Do NOT hook mid-way through the deep transport fns with a jmp-back.** A `svc-standup` probe at
`0x14263ce9c` (`mov rcx,rax` after the standup call) perturbed rax/flags so the sub-init's `jne 0x14263ceb4`
misfired into its teardown path and **faulted** (write to `0x0` at `0x14263cea7`). Without the hook the same
drive returns 0 cleanly. To read a deep fn's return, hook it at a **function boundary** (entry + a return
trampoline), never mid-caller.

**TWO-MACHINE RESULT (2026-07-04, rig host + Steam Deck joiner) — a real peer does NOT unblock the standup.**
Ran it: the rung-2 side-channel **linked** (`coop: linked with partner peer-cf17b9f9 … versions match`), the
create driver fired **with the peer present** (`drive-create armed … side-channel linked`), the establish
handler passed gate1+gate2 and **reached the builder** (`builder-entry REACHED`) — and then failed
**identically to offline**: `0x1423f2820 returned 0`, `[container+0x708]=0x0`, the DLNW3D singleton still the
empty sentinel `0x144852dd0` with readiness `0`, `lobby_state → FailedToCreateSession`. So the
**`SteamServiceImpl` standup `0x142638b40` returns null even with a real linked peer.** The block is **not**
"no peer" — it's that the game's DLNW3D transport is gated on the game's **own online-session flow** (the
EAC/matchmaker path we bypass by construction). A peer reachable over our *private* rung-2 side-channel does
not put the game into that flow, so its transport stays dormant. (The joiner Deck, meanwhile, drove create
too and crashed at `0x141eba203` — the refcount addref near `0x141eba1c0`, the classic `+0x708`-null path;
the host's clean result is the load-bearing datum.)

**CONFOUND RULED OUT (2026-07-04, same session).** The standup-null was NOT the missing `ISteamNetworking006`
iface: re-ran with `stand_up_transport` on (which resolves the iface into the global holder `0x143c602b0`) —
`ISteamNetworking006 = 0x43cdf910 (resolved OK)`, our own `SteamServiceImpl` built, a connection created — and
the establish handler's build **still failed** (`0x1423f2820 returned 0`, `+0x708=0x0`). So the native
standup `0x142638b40` fails on its **`owner`/config** (`[container+0x48]`), which is only valid inside the
game's own online-session flow — not on the iface (resolved) and not on a peer (linked). Native builder =
confirmed dead end.

**⇒ The finish is the SEAM, not the transport — and NOT a from-scratch path 2.** We already build a working
DLNW3D connection ourselves (`stand_up_transport`: iface resolved, `SteamServiceImpl` + `SteamConnectionManager`
+ `SteamConnection` built off the game heap, legacy P2P **rig-proven two-machine**), and we already land it at
`[container+0x708]` (`land_socket_holder` wraps it in a `SocketManagerHolder`) → create reaches
`TryToCreateSession`. The ONE remaining gap is **activation**: the session-update task / host-setup
(`0x140cb2ae0`) faults driving our connection to `Host`, because it derefs sub-objects a full game
session-establish would have wired that our standup doesn't. **Next: chart exactly what the host-setup path
touches/derefs on the `SteamConnection`, and wire those on our stood-up connection so the FSM activates it →
`Host`.** Scoped in [PATH2-TRANSPORT-STANDUP.md](PATH2-TRANSPORT-STANDUP.md).

**► NEXT STEP.** Path 2 (own-transport standup) — **scoped in
[PATH2-TRANSPORT-STANDUP.md](PATH2-TRANSPORT-STANDUP.md)** (start there). The make-or-break first milestone:
resolve `ISteamNetworking006` ourselves (`0x142640b90`, holder `0x143c602b0`) and see if the
`SteamServiceImpl` standup `0x142638b40` then returns non-null outside the online flow. If yes, the rest is
assembly off the charted chain ("Standup chain charted (for path 2)" below). Everything below this line is
older (partly-superseded) narrative.

---

### SEAM + the native-builder finish (2026-07-04) — SUPERSEDED by the NATIVE-BUILD TRACE above

> **STATUS.** The driven create reaches `TryToCreateSession`. The finish is now **one bounded RE**: the
> **descriptor** for the game's own connection builder. The native path is **rig-proven viable offline** —
> the establish handler `0x1423f2820` runs without crashing, its readiness gate passes, and the *only* bail
> is the Arxan builder rejecting our guessed descriptor. See "► NEXT STEP" below. (The chronological
> rig-by-rig narrative this section used to be is compacted into "The path we took / ruled out" at the end.)
>
> **⚠ This section's key claims are CORRECTED by the NATIVE-BUILD TRACE above** — the "Arxan builder
> `vtable[0x80]`" is a wrong-vtable artifact, `+0x42` is cleanup-reset (not a localizer), and the real
> offline wall is the `SteamServiceImpl` standup, not the descriptor. Read the trace section first.

The thread-2 "seam" (transport → session FSM → `Host`) is fully charted. Static + live RE on the pinned
2026-06-02 image (own words; addresses are facts; no decompiler output). Supersedes the earlier guess
("TRANSPORT CHARTED §2") that `+0x708` holds a raw `SteamConnection`.

**What `[container+0x708]` actually is.** A **`SocketManagerHolder@DLNR3D`** (RTTI off vtable
`0x1431f9280`) — a tiny **0x18-byte** object, subclass of `ReferenceCountObject@DLNR3D` (base vtable
`0x1431f85c0`). Layout, from its ctor **`0x1423f7180`**:

```
struct SocketManagerHolder {   // 0x18 bytes, ReferenceCountObject@DLNR3D
    void*            vtable;    // +0x00  = 0x1431f9280
    uint32_t         refcount;  // +0x08  (ctor sets 0; establish-handler addrefs → 1)
    // +0x0c pad
    SteamConnection* conn;      // +0x10  = the raw DLNW3D SteamConnection (vtable 0x143278370)
};
```

So `+0x708` is a **refcounted DLNR3D wrapper around the DLNW3D transport** — not the transport itself.
`+0x8` is the refcount create atomically increments (`0x141eba1c0 = lock xadd dword [rcx],1`), which is
why a raw `SteamConnection` (whose `+0x8` is the iface) can never stand in.

**Who writes `+0x708` in the real path — the connection-establish handler `0x1423f2820`** (`rcx` =
container; 0 static callers, Arxan/vtable-dispatched):
1. `r14 = container->vtable[0x80](descriptor, …)` — container-vtable slot `+0x80` (`0x14251c480`, an
   Arxan trampoline) builds the raw `SteamConnection@DLNW3D`.
2. `buf = game_alloc(0x18, 8, [container+0x48])`; `holder = 0x1423f7180(buf, r14)` — wrap it.
3. `[container+0x708] = holder`; `lock xadd [holder+8],1` (refcount → 1).

**Why create crashed offline (the whole rung-3 wall, in one line).** The **create-gate4 helper
`0x1423faf60`** (`rdi` = the 0x5f8-byte `SessionSteam`; `container = [rdi+0x58]`) loops over the player
count `[rdi+0x68]` and per player: allocates a **0x10c0-byte `ConnectionRefInfo@DLNR3D`** (vtable
`0x1431f85d8`) and calls its ctor **`0x1423f3230(refinfo, rdx=[container+0x708], r8=&[rdi+0x268])`**. The
ctor stores `[container+0x708]` at `refinfo+0x18` and does `lea rcx,[[container+0x708]+8]; call
0x141eba1c0` (addref). With `+0x708` null offline → `lock xadd [0x8],1` → **the fault**. (The gate4 helper
also runs the container's Arxan-encoded **veto vmethod** `[container_vtable+8]` = `0x1423f4330`, which
gates on `[container+0x7c0]` bit 2 — set by the session-established handler — *before* it reaches the
`+0x708` read.)

**Two facts that decided the approach (both rig-proven this session).**
1. **The connection must be REAL — hand-building it is whack-a-mole.** Its sub-objects are *construction-time
   wired* from a fully-built service, not settable fields: `[conn+0x8]` is a lock-bearing DLNW3D sub-object
   (not the iface — FROMNET's "`+0x8`=iface" holds only for a *bare* connection), `[conn+0x120]` is an
   iface-**holder** object (its `vtable[0x18]` returns a context), plus ring buffers + worker-thread locks.
   We descended layer by layer (land a real holder → build via the connection-creator `0x142640560` → don't
   clobber `+0x8` → run Accept setup `0x14263ffe0` → bind `+0x120`), each fix clearing one crash and exposing
   a deeper uninitialized sub-object. There is no bottom reachable by hand — the whole graph comes from the
   service, which itself needs the factory (`0x142638b40`, and its container-owner register is a no-op so the
   service isn't retrievable). **Dead end for hand-building.**
2. **Forcing the FSM does NOT reach a real Host.** The sole `lobby_state=Host(3)` writer is `0x140cb2ae0`
   (also sets `protocol_state=Ingame(6)` + runs the host setup), called by the session-update task
   `0x140cafd10`. Driving it directly writes `Host(3)` at entry, but its host-setup body faults on the
   incomplete connection and the game resets `lobby_state` to `None`. **Host genuinely requires a working
   connection.**

**THE VIABLE PATH — drive the game's own native builder (rig-proven offline).** Drive the connection-establish
handler **`0x1423f2820(container, descriptor)`** at the veto hook: it calls `container->vtable[0x80]`
(`0x14251c480`, the game's own builder) to construct a fully-wired connection, wraps it in the
`SocketManagerHolder` (`0x1423f7180`), stores it at `[container+0x708]`, and addrefs — **the entire seam, the
game's way, so all the sub-object wiring is done for us.** Proven with the `drive_establish_handler` lever:
- **No crash offline** — unlike every hand-build attempt, the game's own path runs clean (create just returns
  `FailedToCreateSession`).
- **The readiness gate passes offline.** `0x1423f5190(container)` — a get-or-create + lock + readiness check
  on the DLNW3D singleton `0x144852dc0` — **returns 1**, so the DLNW3D layer is **not** hard-gated on being
  online here.
- **The only bail is the Arxan builder `vtable[0x80]` returning null** on our guessed zeroed descriptor — a
  *clean* reject (`[container+0x8ac]=1` confirms the body ran; `+0x708` stays null), not a fault.

So the finish reduces to **one bounded problem: the descriptor** — what `~0x120-byte` config makes
`vtable[0x80]` actually build the connection. `vtable[0x80]` is Arxan-obfuscated (a trampoline: `lock cmpxchg`
on `0x1448577d8`, decode via cookie `0x143c5adb0`, `call rbx`), so its consumer can't be read statically —
but it *runs* correctly when invoked through the vtable, and we can read the descriptor it needs by capturing
its **runtime-decoded** target.

**► NEXT STEP (do this next — closes rung-3 create).**
1. **Capture the Arxan-decoded builder.** Hook the establish handler's call site `0x1423f2939` (`call
   [rax+0x80]`, where `rax` = the live container vtable, so `[rax+0x80]` is the decoded builder pointer at
   call time) — or the trampoline's `test rbx,rbx` at `0x14251c4a5` (`rbx` = decoded target) — read + latch +
   log the address. Same technique as the veto `vmethod-target` probe (`log_vmethod_target`); see
   [the `reverse-engineer` skill > "Capturing Arxan-decoded call targets at runtime"](../.claude/skills/reverse-engineer/SKILL.md).
2. **Disassemble the decoded builder** offline (`python3 scripts/re/static.py fn <decoded-addr>` — it lands in
   clean, readable `.text`, not the trampoline) and read off which descriptor dwords/bytes it consumes, the
   buffer/count config it needs, and where the peer SteamID64 goes.
3. **Fill the descriptor** in `drive_establish_handler` (`session_probe.rs`) and re-drive. `+0x708` populated
   → create's `ConnectionRefInfo` loop wraps a real connection → the session-update task activates it →
   watch `lobby_state` advance `TryToCreateSession → Host`.
4. **Two-machine (rig + Steam Deck)** once `Host` holds: prove a real peer join over the connection (the
   Deck's SteamID as the peer), then move to the teardown gate (task: seamlessness).

Preconditions the establish handler needs (already handled by the lever): `[container+0x40]=1`,
`[container+0x41]=0`, and the veto bit `[container+0x7c0]` bit 2 (set by `drive_session_established` →
`0x1423f4870`). The live container is `[SessionSteam+0x58]` during create, and the veto-vmethod hook
(`0x1423f4330`, `rcx`=container) is the injection point that guarantees we hit the create's own container.

**The path we took / ruled out (compacted — full rig-by-rig history is in git log 2026-07-04).**
- **Land a real holder around a hand-built connection** (`land_socket_holder` lever) — cleared the original
  `+0x708` null-deref crash, got create to `TryToCreateSession`. Kept as a lever, but the wrapped connection
  isn't activatable (see fact 1) → superseded by the native-builder path.
- **Hand-build the connection field-by-field** (creator `0x142640560` + Accept `0x14263ffe0` + bind `+0x8`/
  `+0x120`) — whack-a-mole; no settable fields left, sub-objects are construction-time-wired. **Ruled out.**
- **`force_host_transition` (`0x140cb2ae0`)** — writes `Host` but doesn't stick (fact 2). **Ruled out**; kept
  as a charted lever.
- **Service factory `0x142638b40(container)`** — returns the *adapter* not the service, and the container's
  register vmethod is a no-op, so the service isn't retrievable to build a manager off. **Dropped.**
- **Raw `SteamConnection` at `+0x708`** (old "TRANSPORT CHARTED §2" guess) — wrong shape (`+0x8` is the
  holder's refcount, not the connection's iface). **Superseded** by the `SocketManagerHolder` chart above.

**RTTI map (this seam):** `[container+0x708]` = `SocketManagerHolder@DLNR3D` (vt `0x1431f9280`, ctor
`0x1423f7180`); its `+0x10` = `SteamConnection@DLNW3D` (vt `0x143278370`); the per-player wrapper =
`ConnectionRefInfo@DLNR3D` (vt `0x1431f85d8`, 0x10c0 bytes, ctor `0x1423f3230`); container =
`ManagerImpl@DLNR3D` (vt `0x1431f8360`); addref = `0x141eba1c0` (`lock xadd`). Re-derive: the single
caller of ctor `0x1423f3230` is the gate4 helper `0x1423faf60`; the two writers of qword `[reg+0x708]`
in the DLNR3D range are the container ctor `0x1423f20b0` (zero-init) and the establish handler
`0x1423f2820` (real); the holder ctor `0x1423f7180` is a 5-instruction fn that installs vt `0x1431f9280`
and stores its `rdx` arg at `+0x10`.

### RIG-PROVEN (2026-07-03) — the entire DLNW3D transport is DORMANT offline; the gate is above the connection layer

Ran a live-memory vtable scan (`scripts/re/scan-vtable.py`, read-only `/proc/<pid>/mem`, no sudo) on the
game **in-world, offline**, for the container + the three DLNW3D transport vtables:

```
vtable 0x1431f8780 (ManagerImplSteam@DLNR3D, container):     3 live objects   ← positive control (born unconditionally)
vtable 0x143277270 (SteamServiceImpl@DLNW3D):                0 live objects
vtable 0x143278020 (SteamConnectionManager@DLNW3D):          0 live objects
vtable 0x143278370 (SteamConnection@DLNW3D):                 0 live objects
```

**The DLNW3D transport is never instantiated offline — no service, no manager, no connection exists in
the process.** This converts VERDICT §7's static inference into a proven runtime fact and settles the
finish path decisively:

- **`[container+0x708]` is null offline because the whole transport below it is dormant**, not because a
  single field failed to get written. The gate sits **above** the connection layer — the game never
  enters the flow that stands up `SteamServiceImpl`/`SteamConnectionManager`.
- **The solo "drive the connection-manager" lever is dead:** there is no live `SteamConnectionManager` to
  drive `0x142640560` on. (The manager is created dynamically by the online session flow; offline it
  doesn't exist — consistent with the factory/vtable-dispatched, 0-static-caller charting above.)
- **The ISteamNetworking006 interface has never even been fetched offline:** its holder `0x143c602b0`
  still holds the resolver fn `0x142640b90` with a null resolved-pointer (qword1 = 0) — `SteamInternal_ContextInit`
  for the P2P interface has not run. (By contrast the container's identity Steam-context holders
  `0x143b48fd0`/`0x143b48a00` read as initialized, which is why the session-established handler's
  Steam-context branch *passes* when we drive it — Steam is up in our process; the handler simply is
  never *dispatched* offline. So the blocker is event-dispatch/flow-entry, not the Steam context itself.)

**The two finish paths, now sharply defined (pick on the next session):**
1. **Crack the flow-entry / online-availability signal (unifies with item-grey, VERDICT §8).** Find what
   makes the game enter the online session flow so it dispatches session/connection-established events and
   stands up its own DLNW3D transport. This is the runtime-trace task the item-grey doc calls for; if
   solved, the game builds the real graph and a driven-or-item-triggered create reaches `Host`. Highest
   leverage (fixes create **and** the greyed items together), but the signal has beaten every static pass.
2. **Stand up the DLNW3D transport ourselves (the full ERSC-model reimplementation).** Resolve
   `ISteamNetworking006` (via `0x142640b90` / `SteamInternal_FindOrCreateUserInterface`), instantiate a
   `SteamServiceImpl` + `SteamConnectionManager`, register the `P2PSessionRequest_t`/`P2PSessionConnectFail_t`
   callbacks (`CCallback<SteamCallbackWrapper>`), and drive connect/accept with the rung-4-resolved peer
   SteamID64s so a real `SteamConnection` lands at `[container+0x708]`. This is a substantial build (the
   transport subsystem in miniature), but every entry point it needs is now charted above, and it does
   **not** depend on cracking the elusive signal — it bypasses the game's flow-entry entirely, which is
   precisely how ERSC runs co-op outside the matchmaker. **This is the recommended track:** it's bounded
   (the transport surface is finite and mapped) where path 1 is an open-ended signal hunt.

Both paths still terminate at a **two-machine** validation (rig host + Steam Deck joiner) — a
`SteamConnection` is inherently peer-to-peer, so a lone host's `+0x708` self/listen connection only
becomes exercised once a real peer sends the first P2P packet. The transport standup (path 2) is the
scoped next build; the Deck is the validation partner.

Tooling: `scripts/re/scan-vtable.py` (committed) answers "is class X live right now?" for any vtable VA —
reuse it after a game update to re-confirm which layers are up in a given state.

**Standup chain charted (for path 2).** The DLNW3D service is created on demand by a DLNR3D-side owner:
- **`0x142638b40(owner)`** — the service factory: allocates `0x18`, base-ctors a `SteamServiceImpl`
  (`0x14263b6b0`, installs vtable `0x143277270`, sub-ctor `0x14263f1e0`), calls the service's init vmethod
  `[vtable+8]` (`0x14263b820`) with the owner/config, then wraps it in a `0x10` adapter (`0x14263b5a0`),
  runs `[service+0x10]`/`[service+0]` start vmethods, and **registers the service back into the owner** via
  `[owner_vtable+0x68](owner, service)`. So the `owner` (the factory's `rcx`) is a real DLNR3D bridge
  object whose vtable slot `+0x68` accepts the service — the online flow supplies it.
- After the service exists, listen/connect (`0x14263b7c0`/`0x14263b720`) → connection-creator `0x142640560`
  (params = buffer sizes, `+0x5c` ring `0x4b0`) → `SteamConnection` ctor `0x142643b50` + Accept setup
  `0x14263ffe0` (`AcceptP2PSessionWithUser`). That is the full path 2 build target; the one piece not
  statically pin-able is the `owner`/config, which is a live game object best captured at runtime when the
  transport comes up (two-machine, or by cracking the flow-entry signal for path 1).

### VERDICT (2026-07-03, static, worker:create-veto-writer) — the container-init gate is the item-grey signal; static walls → finish with a runtime trace

**Bottom line: the veto chain is *satisfiable* (rig milestone `df12f2d`: seeding bit 2 of `+0x7c0`
**and** a fabricated `+0x708` drove create to `lobby_state None → TryToCreateSession` for the first
time) — so the create gate is not the wall.** The wall is that the container's whole session graph is
never built offline, and building it for real is gated on a signal static analysis cannot decode. I
charted the exact writer of both fields and its condition branch: the branch tests the **live Steam
interface/service context**, and the DLNR3D session code reaches this through the **same service-manager
singleton `0x144842d40` the item-grey hunt hit** — so rung-3 create and the greyed multiplayer items
are almost certainly one signal, the one "none of the static passes found" (OFFLINE-ITEMS-FINDINGS.md).
It is **not** `is_offline`: `enable_offline_multiplayer` forces `is_offline()` false on every drive yet
the container stays uninitialized. **Recommendation: finish with a *runtime execution trace* of that one
signal** (hook the writer + read its condition, and/or Frida-trace the item-grey decision offline, which
reads the same signal) — not another static pass, not hand-seeding stubs. Exact trace addresses below.
All VAs static == live (base `0x140000000`, 2026-06-02 image); own words, no decompiler output
reproduced.

**1. Container identity (RTTI, re-derived).** The container is **`ManagerImplSteam@DLNR3D`** — read
from the RTTI complete-object-locator behind the *live* vtable `0x1431f8780` (`[vtable-8]` → COL
`0x1433e7230` → type-descriptor `0x143d50078` → name `.?AVManagerImplSteam@DLNR3D@@`). Its base is
**`ManagerImpl@DLNR3D`** (vtable `0x1431f8360`, the "static vtable" earlier charts followed — the L3
live/static mismatch was base-vs-derived). It's a **heap object, `0x908` bytes**, allocated in
`0x1423f1e80` (`mov ecx,0x908; call allocator 0x141eb9ed0`; source string `NRManager_steam.cpp` at
`0x1431f80c0`). So DLNR3D = FromSoft's Dantelion network runtime; `ManagerImplSteam` is its **Steam
network-runtime manager**. Related types: session object = `SessionSteam@DLNR3D` (vtable `0x1431fa248`);
the crash sub-object is a **`ConnectionRefInfo@DLNR3D`** (ctor `0x1423f3230`, vtable `0x1431f85d8`,
derives `ReferenceCountObject@DLNR3D` `0x1431f85c0`).

**2. `[container+0x7c0]` is a status bitfield; its writer is container vmethod `0x1423f4870`
(vtable slot +0x68).** Found by a disp-`0x7c0` write scan filtered to the DLNR3D cluster
(`/tmp/scan-disp.py 0x7c0`): the only real (non-zeroing) writers are `0x1423f4870` (sets bits) and
`0x1423f46d0` (`and [rbx+0x7c0], 0xffffffe1`, clears bits 1-4). `0x1423f4870`'s stores, in order:
`or [rbx+0x7c0],2` (`0x1423f48d2`, bit 1) → **`or [rbx+0x7c0],4`** (`0x1423f49b4`, **bit 2 = the create
veto gate**) → `or [rbx+0x7c0],0x10` (`0x1423f49c4`, bit 4, only if `[rbx+0xa0]&0x10`) → `or
[rbx+0x7c0],8` (`0x1423f4a4a`, bit 3). `0x1423f4870` is **vtable slot `+0x68`** of the `ManagerImplSteam`
vtable (its address sits at `0x1431f87e8 = 0x1431f8780 + 0x68`; found by an absolute-pointer search — it
has **0 `E8` callers**, so it is dispatched polymorphically, as a container virtual invoked by the
session subsystem on a session/connection-established event). This is the "session established" handler.

**3. The condition that sets bit 2: the live Steam interface context.** `0x1423f4870` opens with two
`SteamInternal_ContextInit(&holder)` calls (the import at IAT `0x144c0d0a4`, name resolved from its
hint/name RVA) on Steam interface-context holders `0x143b48fd0` and `0x143b48a00`, each followed by
`cmp qword [rax],0; je 0x1423f4a5a` (return false). **Both Steam interfaces must be live** or the method
bails at `0x1423f48b5`/`0x1423f48cc` — before it sets *any* bit, before it stores the identity, before
it registers handlers. Past the gate it pulls a Steam identity/name via more interface vmethods
(`call [rax+0x10]`, `call [rax]`) and stores it into the container at **`+0x7f8`** (helper `0x1423f31d0`,
`mov [rcx+0x7f8],rdx`), *then* sets bit 1, *then* **bit 2**, *then* registers per-session handlers
(`0x1423f8520/0x1423f85a0/0x1423f8860` keyed off singleton `0x1423f8410`) and allocates further
sub-objects (tail calls `0x1423f2a70`/`0x1423f2b00`, both `→ allocator 0x141eb9ed0`; `0x1423f2b00`
links a node into the collection at `+0x7e8`). Offline / outside a live Steam session these interfaces
are null (or the vmethod is never dispatched), so the whole block is skipped: **bit 2 stays clear,
`+0x7f8` stays null, `+0x708` stays null.** The same singleton family (`0x143b489e8 → 0x140e718b0`)
is shared with the game's network-status module (`0x140e717c0`, `0x140e72bc0` — the `0x140e7xxxx`
neighborhood of `is_offline` / the status getters in OFFLINE-ITEMS-FINDINGS.md), tying DLNR3D session
state to the same Steam-online plumbing.

**4. `[container+0x708]` is a refcounted connection sub-object, zeroed at ctor, populated only by the
live-session flow.** The base ctor `0x1423f20b0` explicitly zeroes it (`mov [rbx+0x708],rsi` with
`rsi=0`, `0x1423f2159`); the disp-`0x708` scan finds **no** DLNR3D-cluster store of a real value to it
(only zeroing writes at `0x1423f2159/0x1423f2980/0x1423f2fcc`). The `ConnectionRefInfo` ctor
`0x1423f3230` has a **single caller** — the gate4 helper `0x1423faf60` at `0x1423fb0b1` — which does
`mov rdx,[container+0x708]` and passes it in; the ctor stores `rdx` at `new_obj+0x18` and refcounts
`[rdx+8]` (`lea rcx,[rdi+8]; call 0x141eba1c0`, an interlocked increment). So `+0x708` is the *connection
object a `ConnectionRefInfo` wraps*; null offline → `lock xadd [null+8]` → the rig-captured
`ACCESS_VIOLATION write 0x8`.

**5. What hand-seeding proved (the object-graph result).**
- *Set bit 2 alone* (`set_create_veto_bit`): flips the veto vmethod to `al=1` but leaves `+0x708`
  null → the gate4 helper constructs a `ConnectionRefInfo` over `[container+0x708]==null` → null-deref.
  Rig-confirmed.
- *Set bit 2 **and** fabricate `+0x708`* (milestone `df12f2d`): create **returns true** and reaches
  `TryToCreateSession`, then crashes deeper in session establishment **iterating a collection of hollow
  fabricated objects** — proving a *functional* Host needs the game's real session graph, not stubs.
  So the veto is fully satisfiable; the remaining wall is the uninitialized graph, whose real
  population is what we must trigger.

**6. The container is born unconditionally; only its session-state population is gated.** Container-create
`0x1423f1e80` has a single caller `0x140caa320` (in `CSSessionManager`, `0x140cxxxxx`) which is
**unconditional** — it always allocates the container and sets `[obj+0x20]=1`, with no branch on any
signal. That is why the rig sees a live container (`0x143dcd360`) offline. So the gate is not "is the
container created" but "does its session state get populated" — i.e. does the setter vmethod
`0x1423f4870` run and pass.

**7. The condition branch, and why it is the item-grey signal (not `is_offline`).** The exact,
runtime-readable branch inside the writer is the two `cmp qword [rax],0; je 0x1423f4a5a` at
`0x1423f48b5` / `0x1423f48cc`, where `rax = SteamInternal_ContextInit(&holder)` for holders
**`0x143b48fd0`** and **`0x143b48a00`** — read `[0x143b48fd0]`/`[0x143b48a00]` live to see the signal.
Two facts tie this to the item-grey wall: (a) `enable_offline_multiplayer` forces `is_offline()` false
on every drive yet the container stays uninitialized, so the gate is **not** `is_offline` / the mode
enum (both already rig-eliminated on the item-grey side too); (b) the DLNR3D session cluster
(`0x142403240`, `0x1424032e0`, `0x142403380`, `0x142403420`, `0x1424034d0`) reaches the **same
service-manager singleton `0x144842d40`** the item-grey leaf reads a status through — here as a
lazy-init factory (`mov rbx,[0x144842d40]; test rbx,rbx; jne .; call 0x141eceb10; mov [0x144842d40],rbx`).
The real online/offline *status* is queried on an object that singleton vends, through the same
control-flow-obfuscated leaf class (`0x144d985fd`) that beat three static passes — so static cannot
decode the predicate. **Honest caveat:** I proved the two paths *share the singleton*, not that they
read the identical bit; that identity is the thing to confirm at runtime. (This *refines*, does not
contradict, the note below that the create-time helper `0x1423faf60`/gate4 read no offline global:
correct — by create time the bit is already set-or-not; the signal enters upstream at the writer/init
path `0x1423f4870`, not at the create-time veto read.)

**8. VERDICT — finish with a runtime trace of the unified signal, not more static or hand-seeding.**
The create veto is satisfiable, but a working Host needs the real session graph, built only when writer
`0x1423f4870` runs and passes its Steam-context branch — a signal reached through the same obfuscated
service-manager path as the greyed items, and *not* `is_offline`. Static hits the same wall the
item-grey hunt hit three times. **Recommended finish (one unified runtime probe):**
1. **Hook the writer entry** `0x1423f4870` (prologue `48 8B C4 55 57 41 56` = `mov rax,rsp; push rbp;
   push rdi; push r14`) during a boot and a `drive_create`, and simultaneously read `[0x143b48fd0]` /
   `[0x143b48a00]`. If it fires and bails at `0x1423f48b5/cc`, the signal **is** those Steam holders
   (concrete, done). If it never fires, the gate is the upstream session-FSM dispatch of vtable slot
   `+0x68` — trace that call site.
2. **Frida / exec-trace the item-grey decision offline** (OFFLINE-ITEMS-FINDINGS.md's pending pass):
   the branch it takes on the greyed item reads a status off the same singleton `0x144842d40`. Capture
   the exact global/field it tests.
3. **Unify:** if (1) and (2) land on the same status field, that one address is the rung-3-create +
   item-grey master signal — seed/neutralize *it* (or let a real Steam lobby set it) and both unblock
   together. That is the convergence to aim for.

Do **not** keep hand-seeding container fields (`+0x7c0`/`+0x708`/`+0x7f8`/…): the milestone proved each
stub only exposes the next null in a graph the real Steam session builds atomically. (Re-derivation
after a game update: container = `.?AVManagerImplSteam@DLNR3D@@` via the live vtable's `[-8]` COL; its
`+0x7c0` setter is the vtable-slot-`+0x68` method that opens with two `SteamInternal_ContextInit` calls
each `cmp [rax],0; je`; bit 2 is its `or [this+0x7c0],4`; the shared service singleton is `0x144842d40`.)

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
