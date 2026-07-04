# Writer-Trace Targets — Static Aim Sheet for the Task-#16 Live Capture (2026-07-04)

The rung-3 headline needs a **live** watch-write capture of a real ERSC host+join (task #16) to see
how the game builds a session member and what the online-availability signal reads. That live session
is scarce (needs Michael + the Deck as an ERSC peer, serialized through the orchestrator). This doc
makes that capture a **fast, aimed confirm** instead of open-ended discovery: it charts, statically,
exactly which addresses to arm watchpoints on, what writer/value to expect at each, and the ready
command lines.

Charted from the pinned game image only — the read-only `eldenring.exe` at
`/mnt/games/SteamLibrary/steamapps/common/ELDEN RING/Game/eldenring.exe` (2026-06-02 build, 87 MB),
disassembled with `scripts/re/static.py`. **No rig, no live process, no game launch** — this lane is
static-image-only by construction. Every address is a fact off that image; a game update shifts them,
so §"Re-derive rules" gives the landmark for each.

Clean-room: everything below is behavior described in my own words plus raw facts (addresses,
offsets, byte patterns, vtable slots). No decompiler/disassembler pseudocode is reproduced.

Builds on (read those for the object graph; not re-tread here): `ERSC-LIVE-CAPTURE-FINDINGS.md`
(the live 2-player graph + offsets), `STANDUP-NULL-FINDINGS.md` §2/§3 + the RIG-OBSERVED addendum
(the availability gate `0x140de2620`, offline `[[0x143d855c8]+0x10]==1`), `SESSION-DRIVE.md` Lanes
A/C (add-member slot 26, the registry root, the session array).

> **The one-paragraph orientation.** Two things must go right in a real session that fail offline:
> (1) a **member** gets built and hooked into the session's member registry, and (2) the
> **online-availability** signal reads "available" so the formed session isn't torn down. Target 1
> aims a watchpoint at the member-registry writer; Target 2 aims one at the availability signal. Both
> writers are, statically, **reached through container/service vtable dispatch** rather than a single
> exposed store, so for each I give the *broadest stable static anchor* to arm plus exactly what the
> live backtrace must disambiguate. That is the honest, useful result the task asked for.

---

## TL;DR — the four watchpoints to arm

| # | Target | Arm this (stable, static `.data`) | Catches | Expected writer chain |
|---|---|---|---|---|
| A1 | member registry | `0x143dcd5b8` (= container+0x1e8), the embedded registry container's inline state | a member being hooked into the registry | member base-ctor `0x142400210` → registry-cursor build `0x1423ff7c0` (via container vtable ops), driven by add-member `0x1423fdf20` (SessionSteam vt[26]) |
| A2 | session array (secondary) | `0x143dcdae0 + 0x24` = `0x143dcdb04` (SessionManagerSteam session **count** dword) | a `SessionSteam` being stored into the manager's array | create-session `0x1423f7070` (SessionManagerSteam vt[33]) tail: `array[count++]=session` |
| B1 | availability gate input | `[0x143d855c8] + 0x10` (deref first; heap, per-session) | who sets the gate's first-test field | runtime; RIG-OBSERVED already =1 offline, so **watch to see the online value**, not expected to be the differ |
| B2 | availability singleton | `0x144842d40` (the online-session service singleton pointer, `.data`) | first get-or-create of the singleton | lazy accessor `0x141eceb10` storing the ptr; the *availability state* it later reflects is written **inside** the singleton's container by the Steam-callback populate path (not a static store — see caveat) |

Arm all four in one live session (host + a fresh Deck join). A1/A2 fire on the **host** as the
session forms and the member is added; B1/B2 fire as the availability gate runs during
`TryToCreateSession → Ingame`.

---

## TARGET 1 — the session-member writers

### What it is

In a real session, members live as `SessionMemberSteam` slots (vtable `0x1431fa978`, 6 pre-allocated,
`+0x1280` stride) built by the DLNR3D session layer and hooked into a **member registry** that is an
**embedded intrusive container inside the static co-op `ManagerImplSteam`** at `container+0x1e8`. With
the live co-op container at `0x143dcd3d0` (static), the registry object is at **`0x143dcd5b8`** — and
crucially it is *embedded/static*, not a per-session heap alloc: its first qword is a vtable pointer
(`0x142bbce18`, `.rdata`, RIG-observed live), and its inline node/count fields live at fixed addresses
just past `0x143dcd5b8`. That is what makes it the ideal stable watch anchor: the object survives
across sessions; only its contents change when a member is added. (Confirms
ERSC-LIVE-CAPTURE L116: "member registry = SessionSteam+0x8/+0x10 = 0x143dcd5b8 = container+0x1e8
region, a map/container holding the member objects.")

### The writer chain (charted)

The "add a member" entry is **add-member `0x1423fdf20` = `SessionSteam` vtable slot 26** (offset
`0xD0`; verified by reading vtable `0x1431fa248`). It has **no direct `E8` callers** — the online
flow invokes it through the vtable when a peer joins. Its body:

- reads `S = [session+0x58]` (the owning `ManagerImplSteam` container — same object whose `+0x1e8` is
  the registry), reads the allocator `[S+0x48]`, allocates **0x170 bytes** (the add-member alloc; note
  this is smaller than the `0x1280` pre-allocated slot stride the live capture observed — the exact
  relationship is a live-disambiguation item, see caveats), and calls the member ctor
  `0x142402bf0(alloc, S, session, arg1, arg2)`.
- **arg1** (`rdx`) and **arg2** (`r8`) are the two ref-counted handle objects (ERSC-LIVE-CAPTURE:
  arg1/`member+0x70` = a ref to the host's `SocketManagerHolder`; arg2/`member+0x78` = the per-peer
  identity handle). Neither is a scalar SteamID — the identity is *inside* arg2.

`0x142402bf0` installs the member vtable `0x1431fa978` and calls the **base ctor `0x142400210`**, which
is where the registry hook happens:

- sets `[member+0x58]=S`, `[member+0x60]=session`, flags `[member+0xa4]=0x10002`, AddRefs arg1/arg2
  and stores them at `[member+0x70]`/`[member+0x78]`;
- **computes `rdx = S + 0x1e8` (the registry root) and calls `0x1423ff7c0(member, S+0x1e8, 0)`** — the
  registry-link builder. `0x1423ff7c0` reads the registry container's vtable and calls its
  accessor/insert method `[container_vt+0x18]`, building the member's two cursor sub-objects (at
  `member+0x10` and `member+0x28`) that thread the member into the container. The actual node store
  into the embedded container is performed **through the container's own vtable ops**, not by an
  exposed instruction in the member ctor.

So the member-add writer is: **`0x1423fdf20` (vt[26]) → `0x142402bf0` → `0x142400210` → `0x1423ff7c0`
→ [container_vt+0x18] insert**, and the state that changes is the inline fields of the embedded
registry container at `0x143dcd5b8`.

### Anchor table

| Stable anchor (static `.data`) | Expected writer RIP (top of backtrace) | Expected value / meaning | What drives it |
|---|---|---|---|
| `0x143dcd5b8` (container+0x1e8; embedded registry container, low dword of its inline state) | inside the container-insert reached via `[container_vt+0x18]`; **backtrace should show** `0x1423ff7c0` → `0x142400210` → `0x142402bf0` → `0x1423fdf20` | a member node/count change (registry gains an entry) | a real peer join: the game invokes SessionSteam vt[26] `0x1423fdf20` with the two runtime handle objects the connect handshake produced |
| `0x143dcdb04` (= `0x143dcdae0`+0x24; SessionManagerSteam session **count** dword) — *secondary, session-create not member-add* | tail of create-session `0x1423f7070` (`array[count++]=session`), reached via SessionManagerSteam vt[33]; backtrace shows host-create driver `0x1423f5c00` (vt slot 1) or join driver `0x1423f62e0` (vt slot 2) | count `0 → 1` (and array cap `0x143dcdb00` grows from 0) | host presses the ERSC host item / a join drives vt[33]; **offline this immediately reverts** because cap stays 0 (SESSION-DRIVE Lane C) — a live session is where you first see it stick |

### Arm recipe (Target 1)

```bash
# Primary: catch the member being hooked into the registry, with the call chain.
# 0x143dcd5b8 is the embedded registry container's inline state (static, survives sessions).
scripts/re/watch-bt.py --addr 0x143dcd5b8 --max-hits 4
#   Expect: writer inside a container-insert; backtrace frames (as static VAs) should include
#   0x1423ff7c0, 0x142400210, 0x142402bf0, 0x1423fdf20.  Fires on the HOST as the joiner is admitted.

# Secondary: catch the SessionSteam being stored into the manager's array (session forms + sticks).
scripts/re/watch-bt.py --addr 0x143dcdb04 --max-hits 4
#   Expect: count 0->1; backtrace includes 0x1423f7070 and a create driver (0x1423f5c00 / 0x1423f62e0).

# If you only want the raw writer RIP (cheaper, no stack scan), swap watch-bt.py -> watch-write.py:
scripts/re/watch-write.py --addr 0x143dcd5b8 --access write --max-hits 8
```

### Caveats (Target 1) — what the live trace must disambiguate

- **The registry insert is behind container-vtable dispatch, so the immediate writer RIP will be a
  generic container/map insert routine, not `0x1423fdf20` itself.** That is *why* the arm uses
  `watch-bt.py` (backtrace) rather than bare `watch-write.py`: the backtrace is what proves the write
  came from the member-add chain (`0x1423ff7c0 → 0x142400210 → 0x1423fdf20`) versus some unrelated
  touch of that `.data` region. Confirm the chain in the backtrace before trusting the hit.
- **`0x143dcd5b8` is *embedded/static*, but the exact inline offset that changes on insert is not
  statically pinned** (the container class layout past its vtable isn't decoded here). A 4-byte
  watchpoint watches one dword; if the first arm at `0x143dcd5b8` catches only the vtable/head write
  and misses the count, widen by re-arming a few dwords along (`0x143dcd5bc`, `0x143dcd5c0`, …) — the
  live capture already located this container at `SessionSteam+0x8/+0x10`, so the mutated field is
  within ~0x20 bytes of the base. **This is the one value the live trace should nail down: which inline
  offset is the member count/head**, so a future offline reproduction knows exactly what to write.
- **The two handle args (arg1/arg2) are produced at runtime by the connect handshake** (a ref to the
  `SocketManagerHolder` and a per-peer identity handle) — they are *not* static and *not* fabricated
  from a scalar SteamID (SESSION-DRIVE Lane A). The watch-bt hit is where to read them live: at the
  `0x1423fdf20` frame, `rdx`/`r8` are those two objects — dump them (`--peek` on the member after, or
  read `member+0x70`/`+0x78`) to chart what the join flow builds.
- **The add-member alloc is 0x170 bytes, but the live capture saw a 0x1280 slot stride** between the 6
  pre-allocated member objects. These are not obviously the same size; statically I only see the 0x170
  alloc in the add-member path. The live trace should confirm whether the 0x170 object *is* the member
  (and 0x1280 is per-slot reserve/padding in a pre-alloc array), or whether the pre-allocated slots are
  a distinct larger structure the 0x170 record links into. Don't assume 0x170 == the full slot size.
- **Slot index is not join-order-dense** (live capture: active slots were [4]/[5]). Don't assume the
  first member lands in slot 0.

---

## TARGET 2 — the online-availability signal

### What it is

After a driven standup the session *forms* (`TryToCreateSession → Ingame`), then the parent
`0x140cb2ae0` loads `rcx = [0x143d855c8]` (`.data` static ptr → a per-session heap object) and forwards
it into `0x140ddfb20 → 0x140de2620`, the **online-session-availability gate**. If the gate returns
FALSE, the session layer reads "not really online" and unwinds the formed session. This is the same
online signal that greys out the multiplayer items (OFFLINE-ITEMS-FINDINGS.md). Charted behavior of
`0x140de2620(rcx = obj = [0x143d855c8])`:

1. **`cmp [obj+0x10], 0; jne continue`** — if `[[0x143d855c8]+0x10] == 0`, it stamps
   `[obj+0x9c] = 0xf4241` and returns `al = 0` (FALSE). **RIG-OBSERVED (offline, title/menu):
   `[[0x143d855c8]+0x10]` reads `1`, not `0`** — so this first test does *not* take the false path
   offline. The differ is **below** this test.
2. It get-or-creates the **online-session service singleton `0x144842d40`** (via `0x141eceb10` if
   null), then does `rax = [singleton]; call [rax+0x18]` — **the availability query at
   `[singleton_vt+0x18]`** (args `rdx = &out`, `r8 = 0`). It repeats this query several times, each
   time building a small string key (`.rdata` at `0x142bcd520`, `…580`, `…700`, `…750`) and calling
   indirect global fn-ptrs (`[0x143d5ad08]`, `[0x143d5ad18]`, `[0x143d5ad28]`) with the query result,
   storing outputs to `[obj+0x60]`, `[obj+0x68]`.
3. Each failure path stamps a distinct diagnostic code at `[obj+0x9c]` (`0xf4241`, `0xf4244`,
   `0xf424a`…) and returns `al = 0`; only the full-pass path (`0x140de2c21: mov al,1`) returns TRUE.

So the operative offline≠live differ is **the availability state the `[singleton_vt+0x18]` query
reflects**, i.e. the contents of the `0x144842d40` singleton — not the `[obj+0x10]` field (which is
already non-zero offline) and not anything the `SteamServiceImpl` factory reads (STANDUP-NULL §2/§3).

### The singleton — get-or-create and what it is

`0x141eceb10` is a thunk → `0x141f1d190`, which reads a parent singleton `[0x144844008]` (built via
`0x141f1d620` if null) and returns it (`0x141f61e70` is `mov rax,rcx; ret` — identity). So
`0x144842d40` caches a **deeply-shared engine online-session/matchmaking service singleton**; its
pointer is referenced by *hundreds* of get-or-create sites across the image (the lazy-accessor pattern
`mov rax,[0x144842d40]; test; jne; call 0x141eceb10; mov [0x144842d40],rax`). Its `[vt+0x18]` is a
container-accessor: it returns the singleton's internal availability storage (the gate then checks a
heap-compat flag `shr 5; test 1` on it, the shared `"Tried to create container with incompatible
heap."` landmark at `0x1430acf90`). The **availability entries inside that container are written by the
online-session-manager's own populate path, driven by Steam session/matchmaking callbacks at
runtime** — analogous to Target 1's registry, the store is behind the container's own insert vtable
op, not an exposed static instruction.

### Anchor table

| Stable anchor | Expected writer RIP | Expected value / meaning | What drives it |
|---|---|---|---|
| `[0x143d855c8] + 0x10` (deref the `.data` ptr first; the gate's heap `obj`, per-session) | runtime init of the `obj` object (not statically pinned) | **offline = `1`** (RIG-OBSERVED). Watch live to learn the **online** value and whether it ever differs; not expected to be the differ, but confirms it | online-session subsystem init that builds `[0x143d855c8]`'s object |
| `0x144842d40` (singleton ptr, `.data`) | the get-or-create store at `0x140de2688` (or any of the hundreds of accessor sites), source = `0x141eceb10` | ptr `null → <heap singleton>` — fires **once**, early | first code path that touches the online-session service (the gate itself, or an earlier item-grey check) |
| **the availability container returned by `[singleton_vt+0x18]`** (heap, address only known live) | inside the singleton's container-insert, reached via the online-session-manager populate path | offline: empty / query returns "unavailable" → gate FALSE. live: populated → gate TRUE | **Steam session/matchmaking callback** delivering online availability at runtime |

### Arm recipe (Target 2)

```bash
# B2 — catch the singleton's first creation + who triggered it (fires once, early).
scripts/re/watch-bt.py --addr 0x144842d40 --max-hits 2
#   Expect: source 0x141eceb10; backtrace shows the first availability check (the gate 0x140de2620
#   or an earlier item-grey path). Gives you the LIVE singleton pointer to chase next.

# B1 — read the gate's heap object, then watch its +0x10 field (deref required; do NOT pass
# 0x143d855c8 directly — that's a pointer, not the field). Read the pointer live first:
#   PTR=$(python3 - <<'PY'
#   import subprocess,struct
#   pid=int(subprocess.check_output(["pgrep","-f","[e]ldenring.exe"]).split()[0])
#   p=open(f"/proc/{pid}/mem","rb"); p.seek(0x143d855c8); obj=struct.unpack("<Q",p.read(8))[0]
#   print(hex(obj+0x10))
#   PY
#   )
# then:  scripts/re/watch-write.py --addr $PTR --access write --max-hits 8
#   Expect offline value already 1; live-watch tells you the online writer/value if it changes.

# The real differ (the availability container contents) can only be armed AFTER B2 gives the live
# singleton address: read [0x144842d40] live, follow [vt+0x18]'s returned container, and watch a
# field inside it. That address is per-session heap, so it is a live-only, second-step arm.
```

### Caveats (Target 2) — statically unpinnable to a single store

- **The availability-writer is not a statically pinnable instruction.** The value that flips the gate
  offline→online lives *inside* the `0x144842d40` singleton's container, written by the
  online-session-manager's Steam-callback-driven populate path (external to this binary's static
  control flow, like the `+0x168` lookup in SESSION-DRIVE Lane B). Per the task scope-guard, the honest
  result is: **the broadest stable static anchor to arm is the singleton pointer `0x144842d40`** (catch
  creation + backtrace), and the *contents* writer must be found live as a two-step (arm B2 → read the
  live singleton → arm a watchpoint inside its container).
- **`[[0x143d855c8]+0x10]` is a red herring for the *offline-zero* hypothesis** — RIG-OBSERVED already
  read it `=1` offline (STANDUP-NULL addendum), so the gate's first test passes offline. It is still
  worth arming B1 live to confirm the online value and rule it out for good, but do not expect it to be
  the differ. The differ is the singleton availability query below it.
- **Read at title/menu, not idle-in-world (the caveat STANDUP-NULL flagged).** The offline RIG-OBSERVED
  read of `[+0x10]=1` was at the title/menu. If the live capture wants apples-to-apples, capture the
  online availability state from the same point in the flow the offline read was taken, or note the
  difference.
- **The singleton is shared far beyond co-op.** Because hundreds of sites get-or-create `0x144842d40`,
  the B2 watchpoint may fire from an unrelated early caller (menus, item-grey checks) *before* the
  co-op gate runs. That is fine — the first hit still hands you the live singleton pointer; just don't
  assume the triggering backtrace is the co-op path. Latch the pointer, then move to the contents.

---

## Re-derive rules (every address above; a game update shifts them)

All addresses are on the pinned 2026-06-02 `eldenring.exe` at preferred base `0x140000000` (the exe
loads there under Wine, so static VAs == live absolute addresses; that is why `watch-write.py --addr
<static VA>` works directly for `.data` anchors). Re-find after an update thus:

**Target 1**
- **add-member `0x1423fdf20` = SessionSteam vtable slot 26 (offset 0xD0):** `static.py vtable
  '.?AVSessionSteam@DLNR3D@@'` → vtable VA (`0x1431fa248`); read slot 26 (`vtable+0xD0`) from the image
  (`va_to_off` + `struct.unpack` of the 8-byte slot). Cross-check: slot 25 (`0xC8`) = `0x1423fe030`
  (build-context / stub-writer), slot 27 (`0xD8`) = `0x1423fdfa0` (0x60-byte companion creator).
- **member ctors `0x142402bf0` / base `0x142400210`:** `static.py fn 0x1423fdf20` shows the 0x170-byte
  alloc (`0x141eb9ed0(0x170, 8)`) then `call 0x142402bf0`; `static.py fn 0x142402bf0` installs member
  vtable `0x1431fa978` and `call 0x142400210`.
- **registry root `container+0x1e8 = 0x143dcd5b8` + link builder `0x1423ff7c0`:** in `static.py fn
  0x142400210`, the `add rdx, 0x1e8; call 0x1423ff7c0` sequence names both — `rdx` on entry is `S`
  (`= [session+0x58]`, the `ManagerImplSteam` container), and co-op container is the static
  `0x143dcd3d0` (ERSC-LIVE-CAPTURE), so `S+0x1e8 = 0x143dcd5b8`. The embedded container's vtable ptr at
  `[0x143dcd5b8]` is `0x142bbce18` (`.rdata`) *live* — on the static image `.data` holds an
  uninitialized value (the ctor writes the vtable at runtime), so confirm the vtable live, not on disk.
- **SessionManagerSteam session array/count `0x143dcdae0 + 0x18/0x20/0x24`:** the co-op
  SessionManagerSteam is `container+0x710 = 0x143dcd3d0+0x710 = 0x143dcdae0` (ERSC-LIVE-CAPTURE L56 /
  SESSION-DRIVE Lane C); array ptr `+0x18`, cap dword `+0x20` (`0x143dcdb00`), count dword `+0x24`
  (`0x143dcdb04`). Create-session `0x1423f7070` = SessionManagerSteam vt[33] (`vtable 0x1431f9140 +
  0x108`); its tail does `if count<cap { array[count++]=session }`.

**Target 2**
- **availability gate `0x140de2620` + its input `[0x143d855c8]`:** from STANDUP-NULL §2 — the parent
  `0x140cb2ae0` loads `mov rcx,[rip→0x143d855c8]` (at `0x140cb2dbc`) and `call 0x140ddfb20` (→
  `0x140de2620`). `static.py fn 0x140de2620` shows the first test `cmp [rcx+0x10],0; jne` at
  `0x140de265c` (false path `[rcx+0x9c]=0xf4241; xor al,al`), then the `[rip→0x144842d40]` get-or-create
  and `call [singleton_vt+0x18]`. Landmark inside `0x140de2620`: the `"Tried to create container with
  incompatible heap."` string at `0x1430acf90` (rip-ref).
- **singleton `0x144842d40` + factory `0x141eceb10`:** the get-or-create pattern
  `mov rax,[rip→0x144842d40]; test; jne; call 0x141eceb10; mov [rip→0x144842d40],rax` appears verbatim
  in `0x140de2620` (`0x140de2674`..`0x140de2688`). `0x141eceb10` thunks to `0x141f1d190`, which caches a
  parent singleton `[0x144844008]` (built via `0x141f1d620`) — `static.py fn 0x141eceb10` /
  `0x141f1d190` to re-walk. Section-classify all globals with `static.py sections` (`0x143d855c8`,
  `0x144842d40`, `0x144844008` are `.data`; the string keys `0x142bcd520/580/700/750` and the registry
  vtable `0x142bbce18` are `.rdata`).
- **the indirect availability fn-ptrs `[0x143d5ad08]/[0x143d5ad18]/[0x143d5ad28]`:** rip-refs inside
  `0x140de2620` (`call qword [rip→…]`); `.data` function-pointer table entries the gate calls with each
  query result. Not resolved statically (they are populated at runtime); noted as the query plumbing.

**Tooling notes**
- `watch-write.py --addr <A> [--access write|rw] [--max-hits K] [--peek <A2> --peek-len N]` — bare
  writer RIP (DR0/DR7 HW watchpoint, no sudo on this box; `ptrace_scope=0`). RIP reported is the
  instruction **after** the store, so the writer is a few bytes back.
- `watch-bt.py --addr <A> [--max-hits K]` — same watchpoint plus a stack scan for return addresses into
  `.text`, printed as static VAs = the call chain. **Use this (not bare watch-write) for A1/A2/B2**,
  because the immediate writer is a generic container routine and only the backtrace proves the chain.
  Defaults to `--max-hits 1` to minimize ptrace residency (long attach + Arxan can crash the game) — bump it
  deliberately per the recipes above.
- Both watch a **4-byte** dword at an absolute address. For an 8-byte pointer field, watching the low
  dword suffices. For a heap field like `[[0x143d855c8]+0x10]`, deref the static pointer live first,
  then pass `pointer+0x10` as `--addr` (recipe B1).
