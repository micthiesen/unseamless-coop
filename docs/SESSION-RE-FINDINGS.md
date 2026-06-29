# Session-FSM RE findings (rung 3) — static, 2026-06-27

Three static passes over `eldenring.exe` (no game running) charting the create/join session
initiation for [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md):

1. **The keystone** (below) — found the live `CSSessionManager` singleton global `G`, the ctor, the
   field offsets, and the vtable (the singleton + offsets are also **runtime-confirmed**).
2. **[Create/join initiation chart](#session-createjoin-initiation--static-chart-landed-2026-06-27-workersession-init-re)** —
   landed both initiation functions (CREATE/JOIN) statically, bottom-up from the `lobby_state` write.
3. **[Summon-sign trace](#summon-sign--session-start-trace-top-down-static-2026-06-27-worker-summon-sign-trace)** —
   came top-down from multiplayer-item use; shows item-use drives a job/request state machine (not a
   synchronous create call) and charts the direct-drive arg recipe.

**Current state:** create/join entry functions, signatures, and (for create) the full arg set are
known by *address*; the singleton/offsets/vtable are runtime-verified. What remains is the one-shot
rig confirm of the store sites + active-drive, and join's exact peer payload (two-player test). An
earlier read that an immediate-store scan *couldn't* tie the write to an initiation entry was wrong
(pass 2 corrected it — strategy B works); the wrong turn is tombstoned inline.

All addresses are for the **2026-06-02 `eldenring.exe`** (size 86,998,096; image base
`0x140000000`; two `.text` sections at VMA `0x140001000` and `0x144c0e000`; `.pdata` exception
table at `0x144863000`). A game patch shifts these — every value below has its **re-derivation
recipe** next to it (per CLAUDE.md > "Document how to re-derive RE results"), so a future session
re-finds them in minutes rather than rediscovering the method.

This is behavioral RE on our own legitimately-owned game binary: addresses/offsets are facts about
the binary, written in our own words. No upstream ERSC code or third-party decompiler output is
reproduced (CLAUDE.md > Clean-room).

## The keystone: the live `CSSessionManager` instance global

**`G = 0x143d7a4d0`** holds the singleton pointer: `[G]` is the live `CSSessionManager*` (null until
the manager is constructed during boot). This is the single most useful result — the runtime
write-watch hangs off it.

> **Runtime-confirmed (2026-06-27, solo, undriven — boot to title only).** With
> `[debug.probes] session_probe = true`, the FSM probe logged
> `CSSessionManager @0x7fffd056a4a0 lobby=None protocol=None` by frame 51 (it's live at the title
> screen — **no gameplay needed**). Reading the live process bears out the whole static chain: Wine
> loads `eldenring.exe` at its preferred base `0x140000000`, so `G` is at runtime VA `0x143d7a4d0`,
> and `[G]` read from `/proc/<pid>/mem` was **`0x7fffd056a4a0`** — exactly the probe's base. The
> object there has `[+0] = 0x142b9a0c8` (the vtable — matches the value the ctor stores, so it *is*
> the primary vtable), `[+0xc] = 0` (`lobby_state None`), `[+0x10] = 0` (`protocol_state None`). The
> keystone, the vtable, and the offsets are all verified against the running game, not just inferred.

How it was found (re-derivable):

1. **Find the constructor by a unique fingerprint.** The SDK documents
   `CSSessionManager.session_player_limit_override` at `+0x25c` as *"set to 1 on init and never
   changed."* There is **exactly one** `mov dword ptr [reg+0x25c], 1` in the whole image, at
   **`0x140cabda5`**, inside the function **`ctor = 0x140cabb60`** (`.pdata` range
   `0x140cabb60..0x140cac250`). That same function nulls the three cipher pointers the SDK names —
   `serial_cipher_key`/`aes_encrypter`/`aes_decrypter` at `+0x238`/`+0x240`/`+0x248` — and sets
   `session_player_limit` (`+0x170`) to its init value. Three independent SDK-named fields in one
   function ⇒ this is the `CSSessionManager` constructor (`this` arrives in `rcx`, moved to `rdi`).
   - Re-derive: scan both `.text` sections for `C7 8x 5C 02 00 00 01 00 00 00` (the `[reg+0x25c]=1`
     store, disp32 form; also the `41`-prefixed r8–r15 variants). One hit; take its `.pdata`
     enclosing function.

2. **The constructor's sole caller stores the result into G.** `ctor` is called from exactly one
   site, `0x140679a68` (a big boot-time singleton-init function). Immediately after, at
   `0x140679a72`, `mov qword ptr [rip+0x3700a57], rax` writes the constructed instance into
   `0x140679a79 + 0x3700a57 = 0x143d7a4d0`.
   - Re-derive: find the one `E8` call whose target is `ctor`; the next `mov [rip+d], rax` after it
     names G.

Cross-check: ~520 distinct functions load `G` rip-relatively — consistent with a heavily-used
central singleton (all the multiplayer/session code), not a niche object.

## Field offsets (validated against the pinned SDK `8c67a84`)

From `crates/eldenring/src/cs/session_manager.rs`, confirmed against the binary's accesses:

| Field | Offset | Notes |
|---|---|---|
| `vftable` | `+0x00` | ctor sets a base-class vtable here first (see caveat below) |
| `lobby_state` | `+0x0c` | `repr(u32)`; `None=0 TryToCreateSession=1 FailedToCreateSession=2 Host=3 TryToJoinSession=4 FailedToJoinSesion=5 Client=6 OnLeaveSession=7` |
| `protocol_state` | `+0x10` | `None=0 … Ingame=6` |
| `players` (DLVector) | `+0x128`-ish | roster |
| `session_player_limit` | `+0x170` | ctor writes its init value here |
| ciphers | `+0x238/+0x240/+0x248` | `serial_cipher_key`, `aes_encrypter`, `aes_decrypter`; ctor nulls all three |
| `session_player_limit_override` | `+0x25c` | ctor writes `1`; the unique fingerprint above |

The reflection name string `"CSSessionManager"` is at `0x142b994e0` (ASCII) / `0x142b994f8`
(UTF-16); its DLRF descriptor getter is `0x1400ab920`. Not needed for the write-watch, but handy if
a future bump needs to re-anchor by name.

## The vtable, and a superseded dead end

**Tombstone (superseded by pass 2):** an earlier read here concluded that an immediate-store scan for
`mov dword [reg+0xc], <enum>` (`C7 4? 0C 01 …`) *couldn't* tie the `lobby_state` write to an
initiation entry — so the runbook's "strategy B" was dead and only a runtime write-watch (strategy A)
would do. **That was wrong:** the `None→TryToCreateSession`/`None→TryToJoinSession` transitions *are*
plain immediate stores on the real singleton (`mov [rbx+0xc],1` at `0x140cb208e` = create,
`mov [rbx+0xc],4` at `0x140cb25f0` = join), and pass 2 below found them statically. The scan missed
them because it checked `1`/`4` against a noisy whole-image scan (mostly DLRF reflection-descriptor
ctors embedding `"CSSessionManager"`, and a `"FACE"`-tagged (`0x45434146`) message struct, all with
an unrelated `+0xc`) instead of restricting to the `CSSessionManager` method block, and because it
mis-read the float-blend function `0x141b8a470` as a session "assignment/clone family" (it is not —
see the pass-2 tombstone). Both strategies are now viable; the keystone makes the rig confirm cheap.
(The stale "register store, not immediate" RIG TODO in `crates/unseamless-coop/src/session_probe.rs`
should be corrected when its consts are filled.)

The `CSSessionManager` **vtable is `0x142b9a0c8`** (the ctor's `[this+0]` store, runtime-confirmed as
`[base+0]` above). It is a short vtable (~2 slots); one slot observes a *different* singleton's state,
which is why a quick read of it looked off — but the live object genuinely carries this vtable.
Create/join are **non-virtual** (not reachable by walking a vtable).

## The cheap runtime confirm (mechanics; expected results in pass 2)

The runtime confirmation showed the exe loads at its preferred base, so a Linux-native hardware
watchpoint works and **Frida is not required**:

1. Boot with `[debug.probes] session_probe = true`. The FSM probe prints
   `session-probe: FSM live … CSSessionManager @0x<base>` once the manager is live (at the title
   screen). `<base>` is `[G]`; read it yourself any time with
   `python: open(f"/proc/{pid}/mem","rb").seek(0x143d7a4d0); read(8)` (re-read each boot — the object
   is heap-allocated, so `<base>` changes per run; `G` itself is stable at the preferred base).
2. Arm a **4-byte hardware write-watchpoint on `<base> + 0xc`** (`lobby_state`). Either:
   - **ptrace, no Frida** (preferred here): `PTRACE_ATTACH` the game pid, set `DR0 = <base>+0xc` and
     `DR7` to enable a 4-byte write watch (len=11b, rw=01b, L0=1), `PTRACE_CONT`; on the trap read
     `RIP` from `GETREGSET`. A ~40-line Python `ctypes` script does it. Yama allows same-uid attach.
   - or Frida-gadget (RUNTIME-RE.md, Option B) if you'd rather script in JS.
3. On the trap, subtract the `0x140000000` load base from `RIP` to get the static VA and its
   `.pdata`-enclosing function. The **first `→1` write = create, first `→4` = join** (the watch fires
   multiple times per connect for completion/failure/leave, so take the first edge; the probe's
   transition line timestamps it for correlation). The exact expected `RIP→static-VA` map and the
   consts to fill in `coop/session_probe.rs` are in
   [pass 2's runtime-verify recipe](#runtime-verify-recipe-orchestrator--the-cheap-one-shot-confirm)
   and [landmark hints](#landmark-hints-for-session_probers-do-not-fill-until-runtime-confirmed).

The one thing still needed is the **in-game host/join *trigger***: our overlay "Open World" drives
only the rung-4 lobby, not the game session FSM, so the transition must be kicked by a native
multiplayer action (a summon-sign / multiplayer item). That's the step beyond "boot + enter
gameplay." Solo reaches the **host/create** edge only (hosting initiates locally; joining needs a
peer), so a solo driven session can chart **create**; **join** waits for the two-player friend test
(which the runbook already folds in).

## Re-usable method notes

The scan scripts written for this pass (capstone + numpy over the raw PE, `.pdata` for exact
function bounds, a fast rip-relative xref finder, a port of `from_singleton`'s getter pattern) were
scratch; they are now committed as **`scripts/re/static.py`** (see [pass 3's tooling note](#re-derivation-after-a-game-update)).
The two *techniques* that earn their keep next time: (a) **unique-field-fingerprint → constructor →
instance global** (the `+0x25c=1` route here), and (b) **rip-relative xref via
`disp32 == target − (field_va + 4)`** for fast, desync-free xref finding without a full linear
disassembly.

---

# Session create/join initiation — STATIC chart landed (2026-06-27, worker:session-init-re)

A follow-on **static** pass (no rig) that **did land the two initiation functions** the prior pass
left to a runtime write-watch. It corrects the prior pass's central conclusion: **the `None →
TryToCreateSession` / `None → TryToJoinSession` stores _are_ plain immediate stores to
`[CSSessionManager+0xc]`, statically findable** — strategy B works after all. The orchestrator can
now skip the open hunt and go straight to a one-shot **call-the-function + write-watch confirm**.

All addresses are for the same **2026-06-02 `eldenring.exe`** (image base `0x140000000`), facts
about the binary; behavior is described in our own words (CLAUDE.md > Clean-room).

## Tombstone: `0x141b8a470` was a red herring

`0x141b8a470` is a **float-interpolated struct/pose blend** (a `comiss` lerp factor in `xmm`), whose
`+0xc` field merely *collides* with `lobby_state`'s offset — it is **not** `CSSessionManager` code.
The prior pass mis-took its register `+0xc` copies for the lobby-state writers. Ignore it for session
RE.

## The lobby_state setter family — one dedicated function per `LobbyState`

Scanning the whole primary `.text` for immediate stores `mov dword [reg+0xc], imm` with `imm` in the
`LobbyState` range `1..8`, then keeping the hits whose enclosing `.pdata` function sits in the
`CSSessionManager` method block (`0x140cad000..0x140cb3000`, right after the `ctor 0x140cabb60`),
yields **eight setter sites across the seven non-`None` `LobbyState` values, each value in its own
function** (`None=0` has no setter; `OnLeaveSession=7` has two sites) — a clean per-transition design:

| Function (entry) | store site | `lobby_state ←` | meaning |
|---|---|---|---|
| `0x140cad4c0` | `0x140cad4f0` | `2` | FailedToCreateSession (create **wrapper**, fail leg) |
| `0x140cae640` | `0x140cae68f` | `5` | FailedToJoinSession (join **wrapper**, fail leg) |
| `0x140cae730` | `0x140cae79c` | `7` | OnLeaveSession (leave) |
| `0x140cafd10` | `0x140cb08bc` | `7` | OnLeaveSession (inside the FSM **update task**) |
| **`0x140cb1f70`** | **`0x140cb208e`** | **`1`** | **TryToCreateSession — CREATE initiation** |
| **`0x140cb2470`** | **`0x140cb25f0`** | **`4`** | **TryToJoinSession — JOIN initiation** |
| `0x140cb2ae0` | `0x140cb2af9` | `3` (+`protocol=6`) | Host (create **completion**) |
| `0x140cb2f80` | `0x140cb2fb3` | `6` | Client (join **completion**) |

Identity is as strong as static analysis gets (this is not "a `+0xc` on some object"), pending the
one-shot runtime confirm below: these functions are in the method block next to the ctor; are reached `this = [G]` (`mov rcx, [0x143d7a4d0]`, the charted singleton); read/write
`+0xc` against the exact `LobbyState` enum; and their shared builder (below) reads `+0x25c`
(`session_player_limit_override`, the ctor's unique fingerprint) and `+0x240/+0x248` (the AES cipher
pointers the ctor nulls). The recurring `FD4Singleton.h:180` "singleton accessed before init" assert
string (`0x1429c7aa0`, passed as `edx=0xb4`) confirms these are real singleton-using game routines.

**Initiation vs. completion.** The *initiation* edges (`None→1`, `None→4`) are driven **synchronously
by the create/join methods** below — that's what we want to call. The *completion* edges (`1→3 Host`,
`4→6 Client`) happen **later**: the periodic FSM update task `0x140cafd10` **calls** the dedicated
Host setter `0x140cb2ae0` (store `0x140cb2af9`) / Client setter `0x140cb2f80` (store `0x140cb2fb3`)
once the network handshake finishes — `0x140cafd10` is the sole caller of both. So a `RIP→function`
write-watcher sees the store *inside the setter* (`0x140cb2af9`/`0x140cb2fb3`), with `0x140cafd10` one
frame up the stack, never an address literally inside `0x140cafd10`. A write-watch on `base+0xc`
therefore fires multiple times per connect; the **first `→1` / `→4`** edge is the initiation (the
others are completion/failure/leave).

## CREATE chain (host) — solo-confirmable

```
host flow ─▶ 0x140a23010  (driver: loads this=[G], reads a request obj)
           └▶ 0x140cad4c0  (wrapper: on fail sets lobby_state=2)
              └▶ 0x140cb1f70 (inner: guard, build params, dispatch; on success `[this+0xc]=1`)
                              store @ 0x140cb208e   ◀── the None→TryToCreateSession write
```

- **`0x140cb1f70` (inner, the real worker).** `bool create(CSSessionManager* this /*rcx*/, u8 flag
  /*dl*/, u32 settings /*r8d*/, void* extra /*r9*/)`. Guards on current `lobby_state` (bails if
  already `1/3` host-side or `4/6` join-side), builds the session-params struct via the shared
  builder `0x140cb20d0`, makes a virtual call through the sub-object at `[this+0x60]` (accessor
  `0x1423f1930` → vtable slot `[+8]`), stores its result to `[this+0x24]`, and on success writes
  `[this+0xc]=1` and sets a `[this+0x1b]=1` flag. **No peer SteamID** — a host has no peer yet; the
  args are local session settings.
- **`0x140cad4c0` (wrapper, the clean "call this" target).** Same signature
  `create(this, u8, u32, void*)`; forwards straight to the inner, and on a `false` return sets
  `lobby_state=2` (FailedToCreate) and clears some per-player flags (`+0xaae`) and `[this+0x1c0]`.
  This is the minimal self-contained create entry that takes `this` explicitly.
- **`0x140a23010` (driver, the host-flow entry).** `create_request(reqobj* /*rcx*/, out* /*rdx*/)`.
  Loads `this=[G]` itself, then calls the wrapper with `dl=[reqobj+0x68]`, `r8d=[reqobj+0x6c]`,
  `r9=&[reqobj+0x70]` — i.e. the create args are fields of a small request object. Returns a status
  object (built via `0x1407a91e0` with code `2`=ok / `3`=fail). Its sole caller `0x140a23240` passes
  `reqobj = [arg+8]`. Use this altitude if you'd rather hand a populated request object than raw regs.

## JOIN chain (joiner) — needs a peer (host connection blob)

```
join flow ─▶ 0x1406fa850  (driver: preconditions, gate lobby_state∈{0,2,5}, loads this=[G])
           └▶ 0x140cae640  (wrapper: on fail sets lobby_state=5)
              └▶ 0x140cb2470 (inner: guards, build params, dispatch; on success `[this+0xc]=4`)
                              store @ 0x140cb25f0   ◀── the None→TryToJoinSession write
```

- **`0x140cb2470` (inner).** `bool join(CSSessionManager* this /*rcx*/, u8 flag /*dl*/,
  HostBlob* blob /*r8*/, u32 arg4 /*r9d*/)`. Guards on `lobby_state` (bails if already `4/6`), runs a
  precondition check (`0x14067a2d0` → `0x1409f8f30`), builds params via the same `0x140cb20d0`, then
  **treats `blob` (r8) as a `{begin,end}` byte range** — `r8=[blob]`, `r9=[blob+8]`, `len=r9-r8` —
  and hands that range to the network session's **join** vmethod (vtable slot `[+0x10]`). On success
  writes `[this+0xc]=4`, calls `0x140caeb30(this)` and `0x140cb55b0(&[this+0x2f0])`. **The peer/host
  SteamID rides in this `blob` buffer (arg3, `r8`)** — that contiguous range is the host connection
  token the joiner must supply (on PC it carries the Steam networking identity / SteamID64).
- **`0x140cae640` (wrapper, the clean "call this" target).** `join(this, u8, HostBlob*, u32, <stack
  arg5>)`; forwards to the inner and on `false` sets `lobby_state=5` (FailedToJoin). Note the extra
  5th argument spilled at `[rsp+0x20]` from `[rsp+0x70]` — pass it through from the driver.
- **`0x1406fa850` (driver, the join-flow entry).** Threads its `r8 (→r15)` and `r9d (→r14d)` straight
  into the wrapper's `r8`/`r9d`, plus a stacked arg5 from `[rsp+0x80]`; `dl` is a local temp built by
  `0x1401db000`. **Critically it gates on `lobby_state`** before joining: `lobby_state ≤ 5` and in the
  bitset `0x25` (= bits {0,2,5} → `None`, `FailedToCreate`, `FailedToJoin`) — only an idle/failed
  manager may start a join. Its callers (`0x1401d4dc0`, `0x140a22b00`) assemble the `{begin,end}` host
  blob on the stack (e.g. `r9d=4`, `r8=&local`, etc.). **The host blob is the peer datum the
  orchestrator obtains over the rung-2 side-channel** (the host hands the joiner its connection
  token), which is why join folds into the two-player friend test.

## The shared session-params builder `0x140cb20d0`

Both inners call `build_params(CSSessionManager* this /*rcx*/, ParamsOut* out /*rdx*/, u8 flag /*r8b*/,
u32 count /*r9d*/)`. It clamps the player count against `[this+0x25c]` (`session_player_limit_override`)
and writes `[this+0x170]` (`session_player_limit`), then fills `out` with flags, the player limit, MTU
/ buffer sizes, a callback at `[out+0x50]=0x140cae930`, and copies the cipher pointers `[this+0x240]`
→`[out+0x60]`, `[this+0x248]`→`[out+0x68]` and allocator `[this+0x40]`→`[out+0x200]`. It carries **no
SteamID** — it's pure local session configuration. (Its `+0x25c`/`+0x240`/`+0x248` reads are the
clinching proof `this` is the real `CSSessionManager`.)

## SDK check — NO typed create/join (raw-address RE is required) ⚠️

Confirmed by reading the pinned SDK (`fromsoftware-rs` `8c67a84`): `cs/session_manager.rs` exposes
`CSSessionManager.{lobby_state, protocol_state}` and the roster/cipher fields as **readable layout
only** — there is **no host/create/join/leave method**. `cs/network_session.rs`'s `NetworkSessionVmt`
charts `broadcast_packet / receive_packet / send_hit / kick / request_leave / remote_identity` —
in-session transport, **not session establishment**. So there is **no SDK shortcut**; driving a
session means calling the raw functions above. (This matches SDK-COVERAGE's "Needs internal-function
RVAs" note for session create/join; no edit needed there, but the gap is now filled by *address*, not
just flagged.)

## Runtime-verify recipe (orchestrator — the cheap one-shot confirm)

Two independent confirmations, both off the charted singleton `[G]=0x143d7a4d0`:

1. **Passive write-watch (proves the store sites).** Boot with `session_probe = true`, host once and
   join once (item-gate or direct call), and run the prior pass's `base+0xc` hardware write-watch.
   Expected `RIP→static-VA` map (subtract the `0x140000000` load base; the exe loads at its preferred
   base, runtime-confirmed last pass):
   - first `→1`: `0x140cb208e` (in `0x140cb1f70`) = **create initiation** ✓
   - first `→4`: `0x140cb25f0` (in `0x140cb2470`) = **join initiation** ✓
   - later `→3`: `0x140cb2af9` (Host complete), `→6`: `0x140cb2fb3` (Client complete) — ignore for the
     entry chart; these setters are invoked by the update task `0x140cafd10`, not by the initiation
     methods.
2. **Active drive (the actual rung-3 capability).** With `this=[G]` (read `[0x143d7a4d0]` from
   `/proc/<pid>/mem`), **call the create wrapper `0x140cad4c0(this, /*flag*/0?, /*settings*/…, /*extra*/…)`**
   and watch the FSM probe log `lobby None→TryToCreateSession`. Create/host is **solo-confirmable**
   this way. For **join**, call `0x140cae640(this, flag, HostBlob*, arg4, arg5)` with a real host blob
   captured from the peer — that's the two-player leg. Start at the **wrapper** altitude (smallest
   self-contained, takes `this` explicitly); fall back to the **driver** (`0x140a23010` /
   `0x1406fa850`, which load `[G]` themselves and take a request/blob object) if the wrapper's raw
   args prove fiddly to populate. Confirm `lobby_state` actually moved by reading `[G]+0xc`.

**Sequence the two: read-only first, then drive.** The *safer primary path* is to **hook the wrappers
read-only** (the `session_probe.rs` scaffold is built for exactly this), trigger one real host/join,
and capture the exact `flag`/`settings`/`arg4`/blob register values — then replay them into a direct
call. Direct call-injection is materially harder and riskier than the passive watchpoint: it means
hijacking a game thread (set up registers + stack + a return trap, restore context), and it bypasses
the game's own call site. CSSessionManager's FSM isn't a per-frame-ordered hot path like the
`PostPhysics` feature work (the initiation methods are event-driven, called once off the UI), so a
call off the scheduler is far less fraught than a state read/write mid-frame — but still time it from
a known-idle point (manager live, `lobby_state == None`), and confirm the move by re-reading `[G]+0xc`
rather than trusting the call's return. Capturing real args read-only first also removes the guesswork
about `flag`/`arg4`/blob, so prefer it before any injection.

## Landmark hints for `session_probe.rs` (do NOT fill until runtime-confirmed)

Entry bytes (18) for the four hookable entries, for when the scaffold's `SESSION_CREATE_SITE` /
`SESSION_JOIN_SITE` get filled **after** the runtime confirm (left to the orchestrator per the
scaffold boundary — these are hints, not a committed AOB):

```
create wrapper 0x140cad4c0:  88 54 24 10 57 48 83 ec 30 48 c7 44 24 20 fe ff ff ff
create inner   0x140cb1f70:  48 8b c4 88 50 10 57 48 81 ec c0 03 00 00 48 c7 44 24
join   wrapper 0x140cae640:  88 54 24 10 57 48 83 ec 40 48 c7 44 24 30 fe ff ff ff
join   inner   0x140cb2470:  88 54 24 10 53 56 57 48 81 ec c0 03 00 00 48 c7 44 24
```

Caveat: the `88 54 24 10 ..` (`mov [rsp+0x10], dl`) prologue is common, so a raw 8-byte landmark will
**not** be unique — extend to the full ~16+ bytes shown and verify uniqueness with `resolve_landmark`
before trusting it (it fails safe if not). The inners begin with `mov rax,rsp` / `mov [rsp+0x10],dl`
then a large `sub rsp,0x3c0` frame; hooking the **wrapper** is preferable (cleaner 4-byte prologue
`88 54 24 10`, then `57 / 48 83 ec ..`) and is the right altitude to observe the call + args anyway.

## Re-derivation recipe (after a game patch)

1. **Setter family:** scan primary `.text` for `(41)? C7 4x|8x 0C <imm32 in 1..8>` immediate stores;
   keep hits whose `.pdata` function is in the `CSSessionManager` method block
   `0x140cad000..0x140cb3000` (roughly `[ctor−0x2000 .. ctor+0x7500]`; the block extends past the
   Client setter `0x140cb2f80`, so a `+0x7000` window would clip it). You get the setter table
   directly — the `imm` *is* the `LobbyState` value, so `→1` is create, `→4` is join.
2. **Wrappers/inners:** the `→1`/`→4` functions are the inners; their **sole caller** is the matching
   wrapper (the `→2`/`→5` failure setter). The wrappers' callers that do `mov rcx,[G]` are the
   drivers.
3. **Confirm identity:** each inner reads `[this+0xc]` as a guard, and the shared builder it calls
   reads `[this+0x25c]`/`[this+0x240]` — re-anchor the singleton via the `+0x25c=1` fingerprint if `G`
   moved.

---

# Summon-sign → session-start trace (top-down, static, 2026-06-27, worker `summon-sign-trace`)

Companion to the keystone pass above. That pass came at the session FSM from the *state* side
(found `G`, the ctor, the offsets, and proved the `lobby_state` write can't be tied to an initiation
entry by an immediate-store scan). The sibling worker `session-init-re` is charting the
initiation **bottom-up** from the `lobby_state` write. **This pass comes top-down from the
multiplayer-item use**, to (1) document *what using a summon-sign item actually does* (so we can
remove the items and drive the session directly — Michael's plan), and (2) hand `session-init-re` a
precise meeting point.

Static-only on the same pinned **2026-06-02 `eldenring.exe`** (image base `0x140000000`). Behavioral
notes are in my own words; all addresses are facts about the binary; no decompiler output or upstream
ERSC code is reproduced (CLAUDE.md > Clean-room). Grounded throughout in the pinned `fromsoftware-rs`
SDK (`8c67a84`): `cs/sos_sign_man.rs`, `cs/multiplay_type.rs`, `cs/net_man.rs`.

## TL;DR — the headline finding

**Item-use does NOT synchronously call a "start session" function.** There is no
`use item → … → CSSessionManager::create` call stack. Instead the whole summon system is a
**job/request state machine** that `CSNetMan`'s recurring update task drives across many frames,
with a **FromSoft matchmaking-server round-trip in the middle**:

```
item use (EzState) ─▶ SosSignMan request ─▶ a CSSosSign*Job is enqueued ─▶ CSNetMan update task
   ─▶ FromNet server request (matchmaking) ─▶ [server reply, frames later] ─▶ next job ─▶ …
   ─▶ a sign entry is *processed* ─▶ (gated by MultiplayPropertyEntry.disable_session_creation)
   ─▶ CSSessionManager leaves lobby_state = None   ← the create/join, session-init-re's target
```

The single load-bearing SDK fact that ties signs to session-start is the flag
`MultiplayPropertyEntryFlags::disable_session_creation` (`cs/multiplay_type.rs`), documented as:
*"Whether this multiplayer type should not create session when its entry in `SosSignMan::signs` is
processed."* So **the session is created while a `signs` entry is processed in the net update task**,
not inside the item-use handler and not inside the summon job itself (verified below). That is why
the create can't be pinned from the item side by a call trace — and why both prior immediate-store
scans missed it.

**Consequence for "drive the session without the items":** we don't need to call one hidden
function. We need to either (a) enqueue the same request the item enqueues (and let the net task run
it — but that needs the matchmaking server we don't have offline), or (b) drive the job/session
layer directly, bypassing the FromNet server step. The items are greyed offline precisely because
their first job (`CSSosSignCreateSignJob` / `CSSosSignDownloadSignListJob`) issues a `FromNet`
matchmaking request that can't complete without FromSoft's servers — this is the same gate the
`offline-items` lane is chasing from the availability-check side (see
[OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)); here it's seen from the *function* side.

## What each item does (mechanic, in my own words)

The canonical ELDEN RING summon mechanic, mapped to the FSM roles. **Note a label swap vs. the lane
brief:** in real ER the *sign placer* becomes the summoned **guest (Client)** and the one who
*reveals + interacts with* the sign becomes the **host (Host)** — the reverse of the brief's
"place ⇒ HOST / reveal ⇒ JOIN". The accurate mapping:

- **Tarnished's Furled Finger — "write summon sign".** Registers *your own* white sign in the sign
  pool, making you summonable. Internally this is the **`CSSosSignCreateSignJob`** path (a
  `FromNet` "create sign" matchmaking request). The placer is offering to be a **cooperator
  (guest)** → ends at `lobby_state None → TryToJoinSession → Client` *when later summoned and they
  accept*.
- **Furlcalling Finger Remedy — "see summon signs".** Reveals nearby cooperator signs (lifts the
  murk) by pulling the area's sign list. Internally the **`CSSosSignDownloadSignListJob` /
  `CSSosSignDownloadMatchAreaSignListJob`** path (a `FromNet` "get sign list" request). This only
  *reveals*; it doesn't start a session by itself.
- **Summon-sign acceptance (the actual session start).** The world owner walks onto a revealed sign
  and interacts → **`CSSosSignSummonJob`** → the owner becomes **host**
  (`None → TryToCreateSession → Host`); the sign owner gets the "you are being summoned, accept?"
  prompt and on accept **joins** (`None → TryToJoinSession → Client`). The host create and the guest
  join are the two FSM walks the runbook wants.

## The pinned anchors (facts on the 2026-06-02 exe)

**Summon-sign manager.** `SosSignMan` (RTTI `.?AVSosSignMan@CS@@`): primary **vtable `0x142a8a500`**,
**ctor `0x1406f6e90`** (stores the vtable, then inits its two `DLMap` trees `signs`@`+0x08` /
`sign_sfx`@`+0x20` via the allocator singleton at `0x143d87308` — confirms the SDK layout). The ctor
is called from exactly one site inside the net-init function **`0x140b05a30`**, so `SosSignMan` is
constructed during `CSNetMan` setup (the SDK reaches it as a `CSNetMan` member).

**The summon-sign job family** (each an RTTI-named `CS::CSSosSign*Job`; this is the complete catalog,
read from the RTTI type-descriptor names):

| job | role | vtable | builder fn | builder's one caller (request entry) |
|---|---|---|---|---|
| `CSSosSignCreateSignJob` | place own sign (Tarnished's Furled Finger) | `0x142b3a9c8` | `0x140a13930` | `0x140a1e4a0` (RequestCreateSign) |
| `CSSosSignDownloadSignListJob` | reveal/download signs (Furlcalling Finger Remedy) | `0x142b3aa70` | `0x140a13a30` | `0x140a19ce0` (RequestDownloadSignList) |
| `CSSosSignSummonJob` | summon a sign → start session | `0x142b3aaa8` | `0x140a13ba0` | `0x140a1a110` (RequestSummon) |

(siblings, not separately chased: `CSSosSignCreateMatchAreaSignJob`,
`CSSosSignDownloadMatchAreaSignListJob`, `CSSosSignUpdateSignJob`, `CSSosSignRemoveSignJob`,
`CSSosSignRejectJob` — RTTI names at `0x143ce4ed8…0x143ce5050`.) Each builder has **exactly one**
caller — clean request wrappers. The invasion side mirrors this with a `BreakIn*Job` family whose
RTTI literally includes **`BreakInJoinSessionJob`** (`0x143ce4bb8`) and `BreakInRequestJob`
(`0x143ce4228`) — useful confirmation that "join session" is a job, and a second route to the same
create/join if the sign route stalls.

**EzState bridge.** `CSEventSosSignCtrl` (RTTI `.?AVCSEventSosSignCtrl@CS@@`, **vtable
`0x142a752d8`**) is the event-script controller for sign interaction — the bridge from the
in-world/EzState item action into the `SosSignMan` requests above.

**Server request.** `FromNet::RequestSummonSignParams` (debug-name string at `0x143087048`) and the
`CSNotifyLog<RequestSummonSignResultLogParams>` RTTI (`0x143ce6b50`) are the matchmaking request/-
result for the sign flow — the FromSoft-server step that fails offline.

**The request entry points climb into the net update task, then stop at virtuals** (no `E8` caller =
dispatched by the task/step system), confirming the "polled state machine, not a call stack" model:

- RequestCreateSign `0x140a1e4a0` ← `0x140a19ca0` / `0x140a19e30` ← `0x1401d74e0` / `0x1401d8550`
- RequestDownloadSignList `0x140a19ce0` ← `0x1401d6e30` ← `0x1401d20c0` ← `0x1401cf740` ← `0x1401cfc60`
- RequestSummon `0x140a1a110` ← `0x1401d5b40` ← `0x1401d9570` ← `0x1401d3ae0` (a vtable method, ptr
  at `0x1406ff7e9`) ← `0x1406ff190`

## Where the create actually is (the meeting point for `session-init-re`)

Found **no direct (E8-reachable) create in the summon job** — which, given the create is virtually
dispatched (below), is the most a static reachability scan can establish here. From
`CSSosSignSummonJob`'s vtable methods the *only* `CSSessionManager`-module functions reached are
**`0x140caf2a0`** (21 callers) and **`0x140cadd40`** (4 callers) — both widely-used **accessors**,
reached from the job's diagnostic method **`0x140a52650`** (it is wall-to-wall `FD4Singleton`
access-asserts — a state-logging method, not the worker).

So, consistent with the `disable_session_creation` semantics, the `None→TryTo*` write lives in the
net update task's "process a `signs` entry" step — the cluster of multiplayer step virtuals in
`~0x1401cXXXX…0x1401dXXXX` and `~0x140a3XXXX` that load `G` (`0x143d7a4d0`) and are dispatched
(caller-count 0). This top-down narrowing **converged with the bottom-up pass**: its CREATE driver
`0x140a23010` was already in my sign/net `G`-referencing candidate list (flagged with 2 `G`-refs, 1
caller) — top-down and bottom-up met at the same function. The confirmed CREATE/JOIN initiation
functions, signatures, and store sites are charted once in the
[pass-2 chart above](#session-createjoin-initiation--static-chart-landed-2026-06-27-workersession-init-re)
(CREATE wrapper `0x140cad4c0` → inner `0x140cb1f70`; JOIN wrapper `0x140cae640` → inner
`0x140cb2470`).

## Arg construction for driving create/join directly (the direct-drive recipe)

The high-value deliverable for the co-op core: **what the item/sign path passes into the create/join
wrappers**, so we can call them directly (drive the session without the items). Charted statically by
reading each call site's register setup; all are facts about the 2026-06-02 exe.

### CREATE — `bool create(CSSessionManager* this, u8 flag, u32 mode, void* settings)` @ `0x140cad4c0`

The wrapper is a thin shim: it reloads `flag` and forwards `mode`(`r8d`)/`settings`(`r9`) untouched
to inner `0x140cb1f70`, which does the work and stores `lobby_state=1`; **on failure the wrapper sets
`lobby_state=2` (`FailedToCreateSession`) + cleanup**, so calling the wrapper (not the inner) gets you
the failure handling for free. The inner **guards on current `lobby_state`** — it bails if already in
`{1,3}` (creating/host) or `{4,6}` (joining/client), so you must call it from `None`.

Two call sites build the args; the **sign/host summon path is the clean template** because its args
are near-constant:

| site (enclosing fn) | `flag` (`dl`) | `mode` (`r8d`) | `settings` (`r9`) |
|---|---|---|---|
| **`0x1406f7b75`** (sign/host summon, `0x1406f7ac3`) | `byte [SosSignData + 0x2e]` (`rdx` = the sign being summoned) | **`4`** | `&{ u16 @0 = 0; u32 @4 = 2 }` — an **8-byte** stack blob |
| `0x140a23066` (no-peer driver `0x140a23010`) | `byte [this+0x68]` | `u32 [this+0x6c]` | `&this[0x70]` (settings inline in the driver object) |

So the **minimal direct create** is: `this = [G]` (the live `CSSessionManager*`),
`flag = <a byte from the sign; the no-peer driver passes its own +0x68>`, `mode = 4`, and an 8-byte
`settings = {u16:0, u32:2}`. The inner forwards `mode`+`settings` into the session-request builder
**`0x140cb20d0`** (which is where the `settings` fields are actually consumed and the async create is
issued via a vtable call; success there gates the `=1` store).

> **Offline gate caveat (ties to [OFFLINE-ITEMS-FINDINGS.md](OFFLINE-ITEMS-FINDINGS.md)):** the
> create-request builder `0x140cb20d0` **calls `is_offline()` (`0x140e55180`) twice**. So even a
> direct-drive create runs through the offline check — the already-landed, benign
> `enable_offline_multiplayer` patch (force `is_offline()` false) is plausibly a *prerequisite* for the
> direct create to proceed, which matches that doc's note that `is_offline()` gates the session FSM
> even though it does **not** gate the item greying. Confirm on the rig.

### JOIN — `bool join(CSSessionManager* this, u8 flag, void* a, u32 b, void* c)` @ `0x140cae640`

Same wrapper shape (forwards to inner `0x140cb2470`, which stores `lobby_state=4`; wrapper handles
the failure/`5` + cleanup). Join is **peer-directed**, so its args are *not* constants — at the one
call site `0x1406faac3` (in `0x1406fa850`) they thread through from that function's own params:
`flag = byte [local+0x68]`, `a (r8) = r15` (caller's 3rd param), `b (r9d) = r14d` (caller's 4th
param), and a 5th stack arg `= [rsp+0x80]` (a locally-built object). This is why **solo reaches only
create** (join needs the peer/sign payload these args carry); chart join's exact payload during the
two-player friend test, feeding the rung-4-resolved peer in.

## Net for the rewrite

To drive co-op without the items we replicate the **summon path's effect**, skipping the FromNet
server leg: stand up the peer over our own side-channel (rungs 2+4, done), then call the create/join
wrappers directly (the arg recipe above) — create is solo-drivable now; join's wrapper waits on the
peer payload from the two-player test. The items are harness for exactly this job/request machine;
the create/join entry and (for create) its full arg set are known, so the co-op core can drive the
FSM rather than only observe it. Open items for the rig: (1) does the `is_offline()` check inside the
params builder `0x140cb20d0` block a direct create offline (the `enable_offline_multiplayer` patch
likely clears it); (2) join's exact peer payload (`a`/`b`/stack5), charted with a real peer.

## Re-derivation after a game update

- **The job family:** scan RTTI type-descriptor names for `.?AVCSSosSign` (and `.?AVBreakIn`); each
  `…Job@CS@@` name walks (name → TypeDescriptor at name−0x10 → Complete-Object-Locator holding that
  TD's RVA at COL+0xC → the 8-byte absolute pointer to the COL is `vtable−8`) to the job's vtable.
  The single `lea reg,[vtable]` xref is the builder; the builder's single `E8` caller is the request
  entry.
- **SosSignMan:** RTTI `.?AVSosSignMan@CS@@`; vtable via the same RTTI walk; ctor = the one function
  whose `lea` loads that vtable into `[rcx]`; its sole `E8` caller is net-init.
- **The session tie:** the SDK flag `MultiplayPropertyEntryFlags::disable_session_creation` is the
  authority that "processing a `SosSignMan::signs` entry" is where the session is created — re-read it
  if the call topology shifts.
- Tooling: this pass's reusable helpers are now committed as **`scripts/re/static.py`** — a PE
  section map, ascii/utf16/byte search, the rip-relative xref finder, the `E8` call-site finder,
  `.pdata` function-bounds, an **RTTI-name → vtable walker** (`static.py vtable '.?AV…@CS@@'`), and a
  **single-function capstone disassembler that annotates call targets + rip-relative refs**
  (`static.py fn 0x…`). All the addresses above were (re-)found with it; it's the first stop for the
  next session re-deriving them after a patch.
