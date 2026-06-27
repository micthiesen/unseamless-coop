# Session-FSM RE findings (rung 3) — static pass, 2026-06-27

A solo static analysis of `eldenring.exe` (no game running, no rig driving) to chart the
create/join initiation for [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md). It did **not** land the
two initiation function entries (those need a runtime write-watch — see "Why static stops here"),
but it produced the anchors that turn that runtime step from an open hunt into a one-shot confirm,
and it corrected a wrong assumption baked into the runbook's strategy B.

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

## Why static stops here (and the strategy-B correction)

The runbook's **strategy B** says to scan for an immediate store of the enum constant,
`mov dword ptr [reg+0xc], 1` (`C7 4? 0C 01 …`). **On this build that does not find the initiation**,
for two reasons established by the pass:

- **`lobby_state` on the singleton is written by a *register* store, never an immediate.** Every
  decode-verified `mov [reg+0xc], 1|4` immediate in the image lands on an *unrelated* object — DLRF
  reflection-descriptor constructors that embed `"CSSessionManager"` (len-16) with a `+0xc` kind
  field, and a `"FACE"`-tagged (`0x45434146`) message struct whose `+0xc`/`+0x10` are its own type
  and size. `+0xc` is a ubiquitous struct offset, so the immediate-store scan is almost all false
  positives, and none touch the real session manager.
- **The real `lobby_state` writes are register stores in `this`-param callees.** The writes that
  *are* on `CSSessionManager`-shaped objects are register copies (`mov [rbx+0xc], eax` where
  `eax = [rsi+0xc]`) inside a session-state assignment/clone family near `0x141b8a470` — i.e. the
  field is moved around, and the value of the *initiating* transition flows from a parameter/memory,
  not a literal. No function that loads `G` performs the `None→TryToCreateSession` store directly,
  and no immediate `+0xc` writer is called with `rcx = G` from its immediate caller — so the
  transition happens a call-level or two below an entry that takes `this` as an argument.

Net: the value-store can't be statically tied to the *initiation entry* on this build. This matches
why the runbook already calls **strategy A (runtime write-watch) "most direct"** — now it's the
*only* reliable route, and the keystone makes it cheap.

The `CSSessionManager` vtable is **`0x142b9a0c8`** (the ctor's `[this+0]` store, runtime-confirmed
as `[base+0]` above). It is a short vtable (~2 slots); one slot observes a *different* singleton's
state, which is why a quick read of it looked off — but the live object genuinely carries this
vtable. Create/join are **non-virtual** anyway (not reachable by walking a vtable), so the write-watch
remains the route.

## The cheap runtime confirm (hand-off for the next rig/driven session)

This replaces the open-ended "find these two functions". **Frida is not required** — the runtime
confirmation showed the exe loads at its preferred base, so a Linux-native hardware watchpoint works:

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
3. **Host once**: the watch fires at the exact instruction writing `1` on the first `None →
   TryToCreateSession` edge (the probe's transition line timestamps it for correlation — ignore
   later copies in the assignment family). Subtract the `0x140000000` load base from `RIP` to get the
   static VA; its `.pdata`-enclosing function is the **create** initiation. **Join once** for the
   `… → 4` write → the **join** initiation.
4. Walk each writer to its function prologue, take ~16 unique bytes as the pelite landmark, and fill
   `SESSION_CREATE_SITE` / `SESSION_JOIN_SITE` in `coop/session_probe.rs` (the scaffold is otherwise
   ready). Because the write is in a `this`-param callee, also note from the captured call stack the
   outermost entry you actually want to hook (the one the host/join UI calls), if it differs from
   the leaf writer.

The one thing still needed is the **in-game host/join *trigger***: our overlay "Open World" drives
only the rung-4 lobby, not the game session FSM, so the transition must be kicked by a native
multiplayer action (a summon-sign / multiplayer item). That's the step beyond "boot + enter
gameplay." Solo reaches the **host/create** edge only (hosting initiates locally; joining needs a
peer), so a solo driven session can chart **create**; **join** waits for the two-player friend test
(which the runbook already folds in).

## Re-usable method notes

The scan scripts written for this pass (capstone + numpy over the raw PE, `.pdata` for exact
function bounds, a fast rip-relative xref finder, and a port of `from_singleton`'s getter pattern)
are scratch, but the *techniques* are the reusable part and are described inline above. The two that
earn their keep next time: (a) **unique-field-fingerprint → constructor → instance global** (the
`+0x25c=1` route here), and (b) **rip-relative xref via `disp32 == target − (field_va + 4)`** for
fast, desync-free xref finding without a full linear disassembly.
