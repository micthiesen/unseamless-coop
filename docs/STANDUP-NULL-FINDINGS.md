# SteamServiceImpl Standup — Static Check-Tree Chart (2026-07-04)

Static-only RE of the DLNW3D `SteamServiceImpl` service factory `0x142638b40` and every callee on its
success/fail paths, to answer the one open question: **which check forces the "standup returns null
offline" wall, and what would satisfy it?** Charted from the pinned game image alone (no rig, no live
process) — the read-only `eldenring.exe` at
`/mnt/games/SteamLibrary/steamapps/common/ELDEN RING/Game/eldenring.exe` (2026-06-02 build, 87 MB,
confirmed present read-only), disassembled with `scripts/re/static.py`.

Clean-room: everything below is behavior described in my own words plus raw facts (addresses, offsets,
byte patterns). No decompiler/disassembler pseudocode is reproduced.

> **Headline (the reconciliation the task asked for).** **The standup factory `0x142638b40` is *not* the
> offline wall.** Statically, the factory returns null in exactly one interesting case — its `owner`
> argument (`rcx`) is null — and its one boolean gate (`0x14263f450`, reached through the init vmethod
> `0x14263b820`) is **unconditionally true**: both of its branches end in `mov al,1` and it reads *nothing*
> off `owner`/config beyond storing the pointer. One level up, the sub-init `0x14263ce40` **null-checks the
> config (`descriptor[0]` = `[container+0x48]`) and copies it into `[socketmgr+0x40]` *before* dispatching
> the factory**, so by the time the factory runs, `owner` is *guaranteed non-null*. Therefore **the factory
> cannot return null in the natural flow**, and the earlier "standup returns null offline" reports were a
> probe artifact (as HOST-SETUP DRIVE already suspected) — my static read confirms it independently.
>
> The two contradictory notes are both correct once separated by level: **the factory nulls only on
> `owner==0`** (SESSION-DRIVE ~L257), *and* **the establishment fails offline despite a non-null config**
> (live capture, task #15) — because the failure is **elsewhere**, not in the factory. The real offline
> walls are (1) **upstream**: nothing ever *builds + inits the socket-manager with a config* offline, so
> the owner-bearing factory is simply never invoked (the whole DLNW3D transport is dormant — 0 live objects,
> RIG-PROVEN 2026-07-03); and (2) **downstream**: once the transport *is* driven up (HOST-SETUP DRIVE
> proved the service stands up non-null offline), the session forms and is then torn down by the
> online-session-availability gate `0x140de2620` (`[[0x143d855c8]+0x10]==0` offline). **The standup null is
> a red herring.** This routes to the writer-trace fallback (task #16): the one field that actually
> differs offline-vs-live is a *runtime* availability object, not anything the factory reads.

---

## 1. The check tree (own words; addresses are facts)

### 1a. The factory `0x142638b40(rcx = owner)` — full read/test set

The factory allocates and stands up a `SteamServiceImpl@DLNW3D` and returns a **0x10-byte adapter** wrapping
it (not the service directly). Its complete branch set, in order:

| # | Test (at) | Meaning | Null-return? |
|---|---|---|---|
| 1 | `test rcx,rcx` (`0x142638b60`) | **`owner == 0`** | **yes → returns 0 immediately** |
| 2 | `test rax,rax` after `0x141eb9ed0(0x18)` (`0x142638b7e`) | service alloc (0x18 bytes) failed | yes (OOM only) |
| 3 | `test al,al` after `call [svc_vt+8]` (`0x142638ba3`) | **init vmethod `0x14263b820` returned false** | yes → teardown, return 0 |
| 4 | `test rax,rax` after `0x141eb9ed0(0x10)` (`0x142638bc2`) | adapter alloc (0x10 bytes) failed | yes (OOM only) → teardown, return 0 |

- On **success** (`owner!=0`, both allocs OK, init true): builds the service via base-ctor `0x14263b6b0`
  (installs service vtable `0x143277270`), calls init `[svc_vt+8]`, allocs the 0x10 adapter, wraps it with
  `0x14263b5a0`, overwrites the adapter vtable with `0x143276d88`, and **returns the adapter** (`rsi`).
- The `[svc_vt+0x10]` "stop" and `[svc_vt+0]` "dtor" vmethods, and the `[owner_vt+0x68]` call at
  `0x142638bfe`, are on the **failure/cleanup path only** (reached after init-false or adapter-alloc-fail),
  not the success path. (This corrects the earlier "Standup chain charted" note in SESSION-DRIVE.md ~L1799,
  which read those as success-path "start vmethods" and an owner "register" — they are teardown +
  deregister on the null-return path.)

**Check #3 is the only content-dependent gate, and it is a no-op gate.** `[svc_vt+8] = 0x14263b820`
(vtable slot 1, verified by reading `0x143277270`) is a 4-instruction shim: it calls `0x14263f450` and
returns `setne al` of its result. And `0x14263f450(rcx=service, rdx=owner)`:

- tests `[service+8]` (the service's own owner slot, cleared to 0 by the sub-ctor).
- If already set, it logs an assertion (`ServiceImpl.cpp` line 0x2b, via `0x141eb97a0`) — a debug warning,
  no behavioral effect.
- **Both branches then do `mov al,1` and store `owner` at `[service+8]`.** It **always returns 1** and
  **never reads any field off `owner`/config.**

⇒ Check #3 always passes. So with a non-null `owner` and a working allocator, **the factory always reaches
the success path.** There is no deeper check.

### 1b. The callees (what each dereferences)

- **`0x142638590`** (trampoline, service vtable-owner slot `[obj_vt+0x50]`): `owner = [rcx+0x40]; jmp
  0x142638b40`. So the factory's `owner` is **always `[socketmgr+0x40]`** — the factory has **0 direct
  `E8` callers**; it is only ever reached through this vtable slot.
- **`0x14263b6b0`** (base ctor): calls sub-ctor `0x14263f1e0`, installs service vtable `0x143277270`,
  returns the buffer. No config read, no null path.
- **`0x14263f1e0`** (sub-ctor): installs an interim vtable `0x143277b08`, **clears `[service+8]=0`**
  (the owner slot check #3 relies on), then reads the global clock `[0x144842d28]` (`.data`), reciprocal-
  divides it (magic `0x624dd2f1a9fbe77`, `shr 9`), and stores the derived tick at `[service+0x10]`. This is
  the *only* global read in the whole factory subtree, and it is a timer value, **not a gate** (no branch
  depends on it).
- **`0x14263b5a0`** (adapter wrap): installs adapter vtable `0x143277238`, stores `service` at
  `[adapter+8]`. No config read, no null path.

**No function in the factory subtree tests the content of `owner`/config `[container+0x48]`.** The only
config-derived value that leaves the subtree is `[service+8] = owner` (a copy) and `[service+0x10]` (a
clock tick). Nothing can force a null based on *what the config contains*.

### 1c. One level up — the sub-init `0x14263ce40(rcx = socketmgr, rdx = descriptor)`

This is the immediate caller that supplies the factory's `owner`. Reached from the socket-manager init
`0x14263a9d0` (slot, the fn HOST-SETUP DRIVE drove) and the locked wrapper `0x1426396b0`. Its gates, in
order:

1. `cmp [socketmgr+0x40],0; jne bail` — if `+0x40` already set → return 0 (double-init guard).
2. **`cmp [descriptor+0],0; je bail`** — **if the config (`descriptor[0]`) is null → return 0.**
3. **`cmp [descriptor+8],0; je bail`** — if `descriptor[8]` is null → return 0.
4. copies `descriptor[0..0x60] → socketmgr[0x40..0xa0]` (six 16-byte `movups` load/store pairs). **This sets `[socketmgr+0x40] =
   descriptor[0] = config`**, which becomes the factory's `owner`.
5. `call [socketmgr_vt+0x50]` = the trampoline → factory; result stored at **`[socketmgr+0x38] = service`**.
6. `test rax,rax; je cleanup` — **if the factory returned null → `0x14263cd10` cleanup, return 0.**
7. else: `[socketmgr+0x70] = [socketmgr+0x5c]`; `0x14263b610` (a `[socketmgr+8]`-based slot-pool alloc →
   `[socketmgr+0xa0]`; **null here → cleanup, return 0** — this is HOST-SETUP DRIVE's "fault #3" region);
   then `0x14263ccc0` → `[socketmgr+0xa8]/+0xb0]`; return 1.

**The load-bearing consequence:** gate 2 guarantees `descriptor[0]` (the config) is non-null, and step 4
copies it to `[socketmgr+0x40]` = the factory's `owner`. So **if control reaches the factory call (step 5),
`owner` is provably non-null → the factory returns non-null (check #1 can't fire, and #3 always passes).**
The sub-init's *real* offline-relevant null paths are the descriptor null-checks (gates 2/3 — config not
supplied) and the slot-pool alloc (step 7) — **never the factory itself.**

---

## 2. Best hypothesis for THE failing check offline

**There is no factory-internal check that fails offline.** The static image proves the factory is
*satisfiable* offline, and HOST-SETUP DRIVE empirically confirmed it (drove `0x14263a9d0` with a
hand-seeded descriptor → `socketmgr init returned 1`, `[socketmgr+0x38] = <non-null service>`, offline).
So the "standup returns null offline" wall does not exist as a property of `0x142638b40`. The two walls
that *do* gate offline co-op, in flow order:

1. **Upstream (flow-entry, the primary wall): the socket-manager is never built + inited with a config
   offline.** The factory is only reached via the socketmgr's `[vt+0x50]` slot, which fires from the
   socketmgr init (`0x14263a9d0`/`0x14263ce40`), which fires from the transport builder `0x142637440`,
   which the container's establish handler `0x1423f2820` invokes through container-vtable slot `+0x80`.
   Offline, that handler is **never dispatched** (RIG-PROVEN 2026-07-03: 0 live `SteamServiceImpl` /
   `SteamConnectionManager` / `SteamConnection` objects; the `ISteamNetworking006` holder `0x143c602b0` was
   never even resolved). So the failure offline is **"the owner-bearing factory is never called,"** not
   "a valid owner is rejected." Nothing rejects the config — the code that would consume it never runs.
   When *driven* (HOST-SETUP DRIVE), it runs and succeeds.

2. **Downstream (the sticking wall, once the transport is driven up): the online-session-availability gate
   `0x140de2620`.** After a driven standup the session *forms* (`TryToCreateSession → Ingame`, `players=1`,
   warp into map `1800001` begins), then `0x140cb2ae0` loads `rcx = [0x143d855c8]` (`.data`, static,
   present offline) and forwards it into `0x140ddfb20 → 0x140de2620(rcx)`, which:
   - **`cmp [rcx+0x10],0; jne continue`** — tests `[+0x10]` on that passed-in pointer; **if
     `[[0x143d855c8]+0x10] == 0`, it sets `[rcx+0x9c]=0xf4241`
     and returns `al=0` (FALSE)** → the session-layer reads "not really online" and unwinds the formed
     session;
   - only if `[+0x10]!=0` does it touch the online-session-manager singleton `0x144842d40` (creating it via
     `0x141eceb10` if null) and query availability through its `[vt+0x18]`.

   This is the same online-availability signal that greys out the multiplayer items
   (OFFLINE-ITEMS-FINDINGS.md). HOST-SETUP DRIVE forced this gate true (repurposed `suppress_leave`) and the
   solo host then **stuck**. So **the one statically-pinnable "check that fails offline" is
   `0x140de2620`'s `[[0x143d855c8]+0x10] != 0` test** (plus the `0x144842d40` singleton state it reads) —
   and it lives **below** the standup, not in it.

**Confidence:** high on "the factory is not the wall" (fully static-decidable: `0x14263f450` provably
always-true, sub-init provably guarantees `owner!=0`). Medium-high on "the availability gate `0x140de2620`
is the operative wall" — the *branch* is static (`[obj+0x10]==0`), but what a real online session writes
into `[[0x143d855c8]+0x10]` and the `0x144842d40` singleton is a **runtime value behind an opaque
availability query** (`[singleton_vt+0x18]`), so its satisfying state is not statically pin-able. Per the
task's scope-guard, **that is the answer that routes to the writer-trace fallback (task #16).**

---

## 3. Cross-reference against the live-capture graph

| Field read on the standup path | Value / where | In live-capture graph? |
|---|---|---|
| `owner` = `[socketmgr+0x40]` = `descriptor[0]` = `[container+0x48]` | `0x143d87750` (`.data`, static) | **Named** — ERSC-LIVE-CAPTURE L66 (`container+0x48 = 0x143d87750`, config/owner; also `SessionSteam+0x570`). Present offline *and* live → not the differ. |
| `descriptor[8]` (sub-init gate 3) | `0x1423f2d70` (`.text`, a code/vtable ptr) | New (static const; non-null offline). Passes offline. |
| `[service+8]` (check #3 owner slot) | set to `owner` by the factory | Live: the real `SteamServiceImpl` at `0x7fff66cdfe00` holds it. Derived, not a gate. |
| `[0x144842d28]` (sub-ctor clock) | `.data` global timer | New. Benign (tick derivation, no branch). |
| `[0x143d855c8]` + `[+0x10]` (downstream gate `0x140de2620`) | `.data` static ptr → runtime availability object | **New (charted here).** Hypothesis: offline `[+0x10]==0`, live `!=0`. **This is the real offline≠live field.** |
| `0x144842d40` online-session-manager singleton | `.data` | **Named** — HOST-SETUP DRIVE milestone #6. Offline null/empty; live populated. |
| service vtable `0x143277270` live count | — | **Named** — 1 live (`0x7fff66cdfe00`) vs **0 offline** (RIG-PROVEN). Confirms *flow non-entry*, not a rejected field. |

**Net:** every field the factory subtree *reads* (`[container+0x48]`, `descriptor[8]`, the clock) is a
static object present offline too — none of them differ offline-vs-live. The only fields that differ are
(a) whether the transport objects **exist at all** (they don't offline — flow never entered) and (b) the
downstream availability object `[[0x143d855c8]+0x10]` / the `0x144842d40` singleton. This is exactly why
the standup "nulls offline" story never reconciled: people were looking inside the factory for a field
that differs, and there isn't one.

---

## 4. Concrete dump list (offline vs live — the diff that pins it)

Read these on the rig; the offline↔live diff isolates the true wall. (Orchestrator-run; static lane only
specifies them.) Ordered by decisiveness:

1. **`[0x143d855c8]` then `[[0x143d855c8]+0x10]`** — solo/offline (idle in-world) **vs** a live 2-player
   ERSC session. **Predicted: offline `[+0x10]==0`, live `[+0x10]!=0`.** If so, this single dword is the
   operative wall (`0x140de2620` returns false offline) — the whole "standup null" investigation collapses
   to "the online-availability object is empty offline." **Highest-value dump.**
2. **`0x144842d40`** (the online-session-manager singleton pointer) and, if non-null, its vtable `[obj]`
   and the result of its availability query `[[obj]+0x18](obj, &out, 0)` — offline vs live. Confirms whether
   the singleton itself, or its query result, is what flips the gate.
3. **`[container+0x48]`** (`ManagerImplSteam+0x48`, co-op container = `0x143dcd3d0` live) — offline vs live.
   **Predicted: identical non-null `0x143d87750` both times.** A same-value result is the *positive control*
   proving the config/owner is not the differ (kills the "standup reads something off `+0x48` that's null
   offline" hypothesis for good).
4. **scan-vtable for `0x143277270` (SteamServiceImpl), `0x143278020` (SteamConnectionManager),
   `0x143276cb8` (MTInternalThreadSteamSocketManager)** — offline vs live. **Predicted: 0 offline, ≥1
   live.** Confirms the wall is *flow non-entry* (objects absent), not field rejection. (Already observed
   1/1/— live in ERSC-LIVE-CAPTURE; 0/0/0 offline RIG-PROVEN — re-confirm together.)
5. **In a driven-offline run** (HOST-SETUP DRIVE config): after the socketmgr init, read `[socketmgr+0x38]`
   (service) and `[socketmgr+0x40]` (owner). **Predicted: both non-null offline** — the standup succeeds
   when driven, which is the empirical proof the factory is satisfiable offline. Pairs with #1 to show the
   session then dies *downstream* at `0x140de2620`, not at the standup.

The pin: if #1 shows `[+0x10]` is the lone offline-zero field on the whole standup→form→availability chain,
task #16's writer-trace should **watch-write `[0x143d855c8]+0x10` (and `0x144842d40`) during a real ERSC
host+join** to catch who sets it and to what — that is the value to reproduce for offline co-op, and it is
unrelated to `SteamServiceImpl`.

> **RIG-OBSERVED (2026-07-04, clean vanilla-offline, at the title/menu — drivers all off).** Ran the
> offline half of this list. **Confirmed:** #4 — scan-vtable for `0x143277270`/`0x143278020`/`0x143276cb8`
> = **0/0/0** undriven (and 2/2/1 with our `stand_up_transport`/`land_socket_holder` on, one `SteamServiceImpl`
> at `0x7ffe93856190` carrying owner `0x143d87750` = the config) — so flow-non-entry is real and the driven
> factory succeeds offline, exactly as charted. **Refuted the #1 prediction:** `[0x143d855c8]` is populated
> offline (`0x7fffb09e0000`) and `[[0x143d855c8]+0x10]` reads **`1`, not `0`**. So gate `0x140de2620`'s first
> test (`cmp [rcx+0x10],0; jne`) does **not** take the false path offline — the operative offline≠live differ
> is **below** `[+0x10]`, at the `0x144842d40` singleton availability query (`[singleton_vt+0x18]`), which is
> where the writer-trace (task #16) should aim. (Caveat: read at the title/menu, not idle-in-world; an
> in-world + live read is still owed to be certain, but a non-zero `[+0x10]` already rules it out as *the*
> zero-offline field.)

---

## 5. Minimal writes/calls to make the standup pass offline

**The standup already passes offline when driven — this is solved, and it is not the blocker.** Per
HOST-SETUP DRIVE (rig-confirmed), the minimal recipe is:

1. Build the socket-manager: allocate 0x150 bytes, ctor `0x142638140` (installs vtable `0x143276cb8`).
2. Seed a descriptor with `descriptor[0] = [container+0x48]` (the config `0x143d87750`, non-null offline)
   and `descriptor[8] = 0x1423f2d70` (non-null), and the config dwords from the socketmgr's post-ctor state
   (preserve base-ctor defaults at `+0x58/0x5c/0x60/0x74..0x9c`).
3. Drive the socketmgr init `0x14263a9d0(socketmgr, &descriptor)` → sub-init `0x14263ce40` copies the
   config to `[socketmgr+0x40]`, calls the factory with a non-null owner, and lands a **real
   `SteamServiceImpl` at `[socketmgr+0x38]`**. (Do **not** hook mid-function — a flags-perturbing probe at
   `0x14263ce9c` faults the sub-init; hook only at function boundaries. This mid-hook is the origin of the
   spurious "standup returns null" report.)

**To make the standup pass "naturally" (undriven) offline you cannot** — the factory is only reached by the
establish handler `0x1423f2820`, which the online-session flow never dispatches offline (wall #1). Driving
it is the only path, and it works.

**To make the formed session *stick* offline** (the actual goal the standup was a proxy for), the minimal
lever is the **downstream** availability gate, not the standup:
- either force `0x140de2620` true (flip the `jne` at `0x140de265c` to `jmp`, or make `[[0x143d855c8]+0x10]`
  non-zero) — HOST-SETUP DRIVE did the equivalent via `suppress_leave` and the solo host then stuck; or
- properly satisfy the availability signal (seed `[[0x143d855c8]+0x10]` + the `0x144842d40` singleton to
  the state a real online session leaves — a runtime value to be captured by task #16, ties into
  OFFLINE-ITEMS-FINDINGS.md).

---

## 6. How each address was found (re-derive rules)

All addresses are on the pinned 2026-06-02 `eldenring.exe`; a game update shifts them — re-derive thus:

- **Factory `0x142638b40` / callees**: taken from SESSION-DRIVE.md's charted standup chain, then verified
  by `python3 scripts/re/static.py fn 0x142638b40` and following each `call`/`jmp` target. Re-find from the
  `SteamServiceImpl@DLNW3D` vtable: `static.py vtable '.?AVSteamServiceImpl@DLNW3D@@'` → the vtable VA
  (`0x143277270`); the factory is the fn that allocates 0x18, base-ctors an object installing that vtable,
  and dispatches `[vtable+8]`.
- **Service vtable `0x143277270`, slots 0/1/2 = `0x14263b6e0`/`0x14263b820`/`0x14263b810`**: read directly
  from the image (`va_to_off` + `struct.unpack` of 8-byte slots). Slot 1 (`+0x8`) is the init the factory
  calls at `call [rax+8]`. (Slots ≥3 in this vtable are not real code pointers — the vtable has ~3 live
  slots; dtor / init / stop.)
- **`0x14263f450` always-true**: `static.py fn 0x14263b820` shows it calls `0x14263f450` and returns
  `setne al`; `static.py fn 0x14263f450` shows both branches `mov al,1`. The `ServiceImpl.cpp` string at
  `0x143277a00` (rip-ref `lea rcx,[rip+0xc38586]`) is the landmark that names the function's source file
  (a service-init "owner already set" assert).
- **`owner = [socketmgr+0x40]`**: `static.py fn 0x142638590` (the vtable-`+0x50` trampoline): one insn
  `mov rcx,[rcx+0x40]` before the `jmp` to the factory. The factory has **0 `E8` callers**
  (`static.py calls 0x142638b40`) — reached only via that slot.
- **Sub-init `0x14263ce40` gates**: `static.py fn 0x14263ce40`; the two `cmp qword [rdx…],0; je` before the
  `call [rax+0x50]` are the config null-checks; the `movups` block is the `descriptor→socketmgr[0x40..]`
  copy. Callers via `static.py calls 0x14263ce40` (`0x14263a9d0`, `0x1426396b0`).
- **Downstream gate `0x140de2620`**: from HOST-SETUP DRIVE milestone #6 (`0x140cb2ae0` →
  `0x140ddfb20` → `0x140de2620`). The `[0x143d855c8]` pointer is loaded in the **parent** `0x140cb2ae0`
  (`static.py fn 0x140cb2ae0` → `mov rcx,[rip→0x143d855c8]` at `0x140cb2dbc`/`0x140cb2def`, then
  `call 0x140ddfb20`) and forwarded as `rcx`; `static.py fn 0x140de2620` then shows it test `[rcx+0x10]`
  on that passed-in pointer (`cmp [rcx+0x10],0; jne`, false-path `[rcx+0x9c]=0xf4241; xor al,al`) and, on
  the pass branch, the `[rip→0x144842d40]` singleton get-or-create (`0x141eceb10`). The `"Tried to create
  container with incompatible heap."` string at `0x1430acf90` is a landmark inside `0x140de2620`.
- **Global classification** (`0x144842d28`, `0x144842d40`, `0x143d855c8`, `0x143d87750` all `.data`;
  `0x1423f2d70` `.text`): section-membership check against the PE section table (`static.py` `sections`).
  All four `.data` globals are static → present offline, which is the whole reason the config-read
  hypothesis is dead.
