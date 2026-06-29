# Offline Multiplayer-Item Re-enable — RE Findings

Worker lanes `offline-items` / `item-gate` / `item-gate-static`. Goal: re-enable Elden Ring's online
multiplayer items (Tarnished's Furled Finger, Furlcalling Finger Remedy, Small Golden Effigy,
Duelist's/Bloody/Recusant Finger, Taunter's Tongue, …) when the game is launched **offline / outside
EAC**, where the game greys them out because FromSoft matchmaking is unreachable. This is the rung-3
unblock: with the items selectable, an item-use can drive the game's own `CSSessionManager` FSM (see
[SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) > "Blocker found (2026-06-27 friend test)").

**Binary:** static RE on our own legitimately-owned **2026-06-02 `eldenring.exe`** (image base
`0x140000000`, two `.text` sections at `0x140001000` + `0x144c0e000`, `.pdata` at `0x144863000`; the
exe loads at its preferred base, so static VA == live VA — SESSION-RE-FINDINGS confirmed). Behavioral
notes are in my own words; no upstream ERSC code or decompiler output is reproduced (CLAUDE.md >
Clean-room). Static inference has now **failed three times** on this exact problem, so every static
claim is paired with the runtime check that settles it.

## Current State (2026-06-28)

Three successive static candidate families have each been **rig-eliminated** — do not re-pursue any:

1. **Mode enum / `is_offline()`** — patched to "not offline" at its root; items stayed greyed.
2. **`IsEnableOnlineMode()`** — reads `1` (TRUE) outside EAC, so forcing it true is a no-op.
3. **The lazily-cached "online-available" bool chain** — a read-watch on its cached bool fired
   **zero times** during inventory navigation onto the greyed item; the grey decision never reads it.

See [Candidate History](#candidate-history--tombstones) for the disproofs. **The item-grey gate is a
signal none of the static passes found.** Next move (orchestrator): a **runtime execution trace** —
Frida-stalk / hook the inventory item-availability draw, or the `EquipParamGoods` "usable now" check,
to see what the grey decision actually branches on. Static guessing is exhausted; the charted
landscape below makes that trace short and targeted. Two fail-safe patches are landed (one default-on
but benign, one default-off; both now known insufficient) and a third is charted-but-unlanded.

## The Charted Online/Offline Landscape

All the online/offline state the game exposes, charted statically. The first three families are
rig-eliminated; the fourth (bool chain) is the most recent and also eliminated, but its charting is
the launchpad for the runtime trace.

### Mode enum + `is_offline()` (eliminated)

A single **network-mode** enum lives in a global at **`0x143d87220`** (values `0/1/2`; a runtime
assert in the getter fires on `>= 3`): `0` = not-yet-determined/initializing, `1` = online,
**`2` = offline**.

- **Getter `0x140e0e960`** — returns the `0x143d87220` enum (with the `>= 3` debug assert). The
  enum's **only** reader.
- **Writer/recompute `0x140e0ea60`** — the **only** function that stores `0x143d87220` (via
  `0x140e5b530`). Recomputes the mode from several sub-queries; called once, from the boot
  network-flow step `0x140c901af` (the boot-time online/offline decision; cf. the
  `CSNetworkFlowStep::STEP_OnlineMode` / `STEP_OfflineMode` RTTI strings). It also writes a sibling
  status block (see below).
- **`is_offline()` `0x140e55180`** — `return getter() == 2`. **45 call sites** across the image gate
  online features on it. It is the getter's **only** caller, so `is_offline()` is the *sole* consumer
  of the mode enum — patching it false neutralizes every path the enum can influence.

Launched offline/without-EAC the mode is `2`, so `is_offline()` is true everywhere. The natural
hypothesis ("that's what greys the items") was **disproved** — see Candidate History #1.

### `IsEnableOnlineMode()` (eliminated)

Getter **`0x140e56310`**, a lazily-initialized cached bool (MSVC magic-static guard), **not** the
"static dev CVAR that defaults true" an earlier note assumed:

- On first call it reads the config value keyed by the UTF-16 string **`"Menu.IsEnableOnlineMode"`**
  (`0x142beac58`) and stores the result byte.
- **Cached return byte: `0x144588afc`** (writable `.data`). Every call's return path converges on
  `movzx eax, byte [0x144588afc]` at **`0x140e56444`**.
- **Magic-static init-guard dword: `0x144588b00`** (`-1` once initialized).
- Live read (rig) showed byte0 = `1` ⇒ **TRUE outside EAC** — eliminated (Candidate History #2).

> Earlier red herring: the leaf `0x140764fe0` (`IsEnableOnlineMode() && !is_offline()`) looked like
> the polarity-confirming "online available for menu" gate, but it has **zero callers** — no `call`,
> `jmp`, or rip-relative ref; its one absolute-pointer ref is a slot in a dev/CVAR type-dispatch
> table in `.data` (`0x143b350e0`, reached only via `jmp [table + type_byte*8]` at `0x140765037`). It
> never gated the inventory. This is *why* `is_offline()` looked like the gate but wasn't.

### The network-status getter family + boot sibling block

The boot writer `0x140e0ea60` does **not** compute a single offline bool — alongside the mode enum it
writes a whole **sibling status block** from a series of platform/EAC/login sub-queries (it calls
`0x140e5b410/530/650/770/890/9b0`, each fetching a service object and calling a virtual on it):

- sibling status dword **`0x143b400bc`** (13 readers) and friends `0x143b400b0` (6 readers),
  `0x143b400b8`, `0x143b400c0`, plus `0x143d87224`.

These siblings are read **only inside** the status module (`~0x140e0dfc0..0x140e0ec80`), surfaced
through a family of small **getter functions** — the public "what's my online status" API. The ones
with external callers:

| getter | reads / builds | external callers (sample) |
|---|---|---|
| `0x140e0dfc0` | `0x143b400bc` | `0x1408c6bcf`, `0x1408cde5d` |
| `0x140e0dfe0` | `0x143b400bc` | `0x140909b1c`, `0x140933909`, `0x140997301/451` |
| `0x140e0e000` | `0x143b400bc` | `0x140b04224` |
| `0x140e0e0c0` | built object | `0x140d77290/2ce/60c` |
| `0x140e0e1d0` | built object | `0x140d76e90`, `0x140d77b0c` |
| `0x140e0e2e0` | `0x143b400bc` | `0x140b042a7` |
| `0x140e0e3a0` | `0x143b400bc` | `0x140cbbb48` |
| `0x140e0e550` | built object (consults the bool chain) | **only** `0x140d76fa0`, `0x140d77bec` |
| `0x140e0e960` | mode enum | `is_offline()` only |

The `0x140d76xxx`/`0x140d77xxx` caller cluster is the most multiplayer-menu-shaped. These siblings
are **not** cleared by the `is_offline()` patch, so they remain at their offline-computed values —
secondary runtime candidates if the chain trace comes up empty.

### The lazily-cached "online-available" bool chain (charted, then eliminated)

The one getter in the family the menu cluster actually calls (`0x140e0e550`) consults a four-function
cached-bool chain that is **independent of the mode enum / `is_offline()`** (so the disproof left it
untouched — which is exactly why it was the strongest "different signal" candidate). Four functions +
two cached bytes:

```
multiplayer-menu cluster            network-status getter           cached online-available bool chain
0x140d76fa0 / 0x140d77bec  ───────▶  0x140e0e550  ──(7 call sites)──▶  0x14073cd40 ──▶ 0x140e0ec90 ──▶ 0x140e43610
(the two ONLY callers of                (consults the bool             (caches byte      (caches byte    (leaf body at
 getter 0x140e0e550)                     chain; independent of          0x143d6a840,      0x143d87228,    0x144d985fd;
                                         is_offline/mode getter)        guard 0x143d6a844) guard 0x143d8722c) reads a service
                                                                                                           singleton, derives
                                                                                                           the bool)
```

All four are **magic-static lazy getters / their leaf** (MSVC pattern: TLS guard at `gs:[0x58]`,
init-guard dword == `-1` once computed, cached result byte). Charting confidence **high** (exact
`E8`-call finder + tight in-module rip-reader scans):

| addr | role | reads / caches | callers | note |
|---|---|---|---|---|
| **`0x140e0e550`** | network-status getter | builds a status object; consults the bool chain (7 call sites to `0x14073cd40` in body region `0x140e0e664..0x140e0e944`) | **only** `0x140d76fa0`, `0x140d77bec` (menu cluster) | the bridge from menu code to the online signal |
| **`0x14073cd40`** | outer cached-bool getter | caches **bool `0x143d6a840`** (guard `0x143d6a844`); value from `0x140e0ec90`. Sole return site `movzx eax, byte [0x143d6a840]` at **`0x14073cd9c`** | `0x140e0e550` (7 calls) + `0x140dfc9d8` | the byte the menu getter consults |
| **`0x140e0ec90`** | inner cached-bool getter | caches **bool `0x143d87228`** (guard `0x143d8722c`); value computed by `0x140e43610(1)` | sole caller `0x14073cd85` (within `0x14073cd40`) | distinct byte/guard from `IsEnableOnlineMode` |
| **`0x140e43610`** | leaf compute (real body `0x144d985fd`) | reads a **service-manager singleton (ptr at `0x144842d40`)**, derives the bool from a status compare | `0x140e0ecd7` (within `0x140e0ec90`) | where offline/no-EAC state actually enters; body is control-flow-obfuscated (movabs+push return trampolines, stack-pivot `lea rsp`) — read its predicate at runtime, not by decode |

Eliminated at the rig (Candidate History #3): the read-watch never fired on the item interaction.

### Supporting facts & corrections

- **Menu cluster** (consumers nearest the inventory decision): functions
  `0x140d76d20..0x140d770cf`, `0x140d771d0..0x140d773f4`, `0x140d77550..0x140d7782e`,
  `0x140d77ae0..0x140d77bba`, `0x140d77bc0..0x140d77c9a`. They query the getter family
  (`0x140e0e0c0/e1d0/e550`). `0x140d76d20` and `0x140d77bc0` have **no direct callers** (reached via
  vtable / obfuscated dispatch — menu virtual methods); the others are dispatched from `0x140d6aXXX`.
- **Independence from the ruled-out path (load-bearing):** getter `0x140e0e550`'s body calls
  `0x14073cd40` and **not** `0x140e0e960` (mode-enum getter) nor `0x140e55180` (`is_offline()`).
  Forcing `is_offline()` false could not move this signal.
- **`0x143d87228` ownership correction:** an earlier note filed byte `0x143d87228` under the boot
  sibling block. It is **not** boot-written — its *only* writer is `0x140e0ecdc`, inside the
  magic-static getter `0x140e0ec90` (verified by exact write-xref). It merely sits in `.data`
  immediately after the boot block (`…87220` enum, `…87224` dword, `…87228` byte, `…8722c` guard) —
  **adjacency, not a shared initializer.** `0x143d8722c` is confirmed its init-guard by the
  thread-safe-init acquire/release calls (`0x1424fad58` / `0x1424facf8`) and the `cmp [guard], -1` it
  wraps.

## Candidate History — Tombstones

Disproofs, do not re-pursue.

### #1 — Mode enum / `is_offline()` → patch `enable_offline_multiplayer` — BENIGN but INSUFFICIENT

Hypothesis: neutralize `is_offline()` so the game never believes it's offline, re-opening every
online-gated surface (the multiplayer items among them) without touching `regulation.bin`, the
session FSM, or the network globals.

Rig (orchestrator, 2026-06-28): the patch resolved with its `expect` guard matching live
(`patched 'enable_offline_multiplayer': [0F, 94, C0] -> [31, C0, 90]`), the game booted to title
cleanly and stayed stable — **no `network error / return to title` popup** (cf.
`CSEventNetworkErrorReturnTitleStep`), no hang. But in a loaded save the multiplayer items were
**still greyed** (Tarnished's Furled Finger unselectable).

**Disproof (load-bearing):** the patch hits `is_offline()` at its *root*, so **all 45 consumers** —
including the menu-availability leaf `0x140764fe0` — see `is_offline() == false`. The items didn't
move ⇒ the greying does **not** depend on `is_offline()` (nor, since `is_offline()` is the enum's sole
consumer, on the mode enum at all). Patch **left in place**: benign, and forcing `is_offline()` false
is plausibly still a prerequisite for the *session FSM*, which `is_offline()` also gates.

### #2 — `IsEnableOnlineMode()` → patch `force_online_menu_mode` — RULED OUT (no-op)

Hypothesis: with `is_offline()` forced false, any item path of shape
`IsEnableOnlineMode() && !is_offline()` reduces to `IsEnableOnlineMode()`; if that's false outside
EAC, forcing it true would ungrey the items.

Rig (orchestrator, 2026-06-28): read `0x144588afc` live at the title screen
(`watch-write.py --peek 0x144588afc --peek-len 8` → `01 00 00 00 05 05 00 80`, byte0 = `1`).
`IsEnableOnlineMode()` is already **TRUE** outside EAC, so `force_online_menu_mode` is a **no-op** and
cannot be the gate. Patch left in (inert, default OFF, known no-op) pending removal.

### #3 — The "online-available" bool chain → read-watch — RULED OUT (not on the item path)

Hypothesis: the menu-reachable, disproof-independent bool chain (above) is the gate.

Rig (orchestrator, 2026-06-28): with the greyed Tarnished's Furled Finger highlighted, the cached
bytes `0x143d6a840` and `0x143d87228` both read `01` (available) — identical to their title-screen
baseline, no flip. Decisive probe: a **read-watch** (`watch-write.py --addr 0x143d6a840 --access rw`,
a 4-byte read-or-write hardware watchpoint armed on all 104 game threads) fired **zero times** across
full inventory navigation onto/off the greyed item. The menu-grey decision **never reads this bool** ⇒
the chain is consulted, but not by the item-availability path.

## The Landed Patches (config-gated, fail-safe)

Both wired in `coop/app.rs::apply_boot_patches` via `patch::overwrite_landmark`, next to skip-intros;
a missed/ambiguous/drifted scan **fails safe** (game runs unmodded, logged). Both are now known
insufficient (Candidate History) but left in place.

### `[gameplay] enable_offline_multiplayer` (default on) — neutralize `is_offline()`

Force `is_offline()` `0x140e55180` to always return 0:

```
0x140e55180: 48 83 EC 28              sub  rsp, 0x28
0x140e55184: E8 ?? ?? ?? ??           call 0x140e0e960        ; eax = network mode
0x140e55189: 83 F8 02                 cmp  eax, 2
0x140e5518c: 0F 94 C0   --> 31 C0 90  sete al  -->  xor eax,eax ; nop   (al/eax = 0 always)
0x140e5518f: 48 83 C4 28              add  rsp, 0x28
0x140e55193: C3                       ret
```

- **pelite landmark (unique, the whole 20-byte function):**
  `48 83 EC 28 E8 ? ? ? ? 83 F8 02 0F 94 C0 48 83 C4 28 C3` (the 4 wildcards are the `call` rel32).
  Exactly one match. The bare tail `cmp eax,2; sete al; add rsp,0x28; ret` occurs 4× and is **not**
  unique, which is why the landmark spans prologue + call.
- **offset `+12`** (match-start is the entry; `sete al` is 12 bytes in). **expect `0x0F`.**
  **replacement `31 C0 90`** over `0F 94 C0`; the dead `cmp eax,2` is left (its flags go unused).

### `[gameplay] force_online_menu_mode` (default off) — force `IsEnableOnlineMode()` true

Force the getter's sole return site (it can be A/B'd independently of `enable_offline_multiplayer`):

```
0x140e56444: 0F B6 05 B1 26 73 03   movzx eax, byte [0x144588afc]   ; getter's sole return site
          -> B8 01 00 00 00 90 90   mov eax, 1 ; nop ; nop          ; force "online mode enabled"
```

- **landmark (unique, 7 bytes):** `0F B6 05 B1 26 73 03` — exactly one match (the cached-bool disp
  `B1 26 73 03` is what makes it unique). **offset `0`, expect `0x0F`.** All of the getter's code
  paths (the two fast-path `jle`/`jne` exits and the post-init fall-through) converge on this `movzx`,
  so this forces the return on **every** call.

### Charted-but-unlanded — force the bool chain (do NOT land)

If a future trace ever pins the bool chain, the fail-safe patch mirrors the above: force
`0x14073cd40`'s sole return site `movzx eax, byte [0x143d6a840]` at **`0x14073cd9c`**
(`0F B6 05 9D DA 62 03`) → `mov eax,1; nop; nop` (`B8 01 00 00 00 90 90`). **Do not land** — the chain
is rig-eliminated; recorded only for shape.

## Rig Recipes

The exe loads at its preferred base, so static VA == live VA; read via
`watch-write.py --peek <VA> --peek-len <n>` or `/proc/<pid>/mem`.

### Cheap pre-reads (no Frida) — all done, all eliminated, kept for the method

- **`IsEnableOnlineMode` byte** `0x144588afc` (1 byte) + guard `0x144588b00` (4 bytes) — read *after*
  the title screen is up (lazy init; if the guard is not `-1`/non-zero it hasn't initialized, so open
  a menu that touches online state and re-read). `0` ⇒ false outside EAC; `1` ⇒ ruled out. (Got `1`.)
- **Bool-chain bytes** `0x143d6a840` + `0x143d87228` (1 byte each) and their guards `0x143d6a844` /
  `0x143d8722c` (4 each) — read at title, then **open the inventory on the greyed item** (drives the
  menu cluster → `0x140e0e550` → the chain, forcing lazy init) and re-read; whichever reads
  "unavailable" while the item is greyed is the live candidate (**polarity unknown — read it off the
  greyed-item state directly**). Also dump the boot sibling block `0x143b400b0..0x143b400c0` (20
  bytes) + `0x143d87224` (4). (Both chain bytes read `01`, unchanged from baseline.)
- A hardware **read-watch** on a candidate global (`watch-write.py --addr <VA> --access rw`) is the
  decisive "is global X on the item-grey path?" probe — proven clean. (Zero hits on `0x143d6a840`
  during inventory nav ⇒ chain off-path.) A read-watch on the boot sibling `0x143b400bc` works too but
  it has 13 readers and is noisy; getter hooks are cleaner.

### The decisive pass (current hand-off) — execution-trace the grey decision

Repro: a loaded save where Tarnished's Furled Finger is **visible and greyed** in the pouch. Static
guessing is exhausted, so pin the gate by watching what the grey decision *actually* branches on:
**Frida-stalk / hook (RUNTIME-RE.md option B) the inventory item-availability draw, or the
`EquipParamGoods` "usable now" / use-condition check.** The branch it takes on the greyed item, and
the signal it reads to take it, is the gate. Likely it's a live matchmaking/login-session availability
read inside the service singleton (`0x144842d40`), reached through the obfuscated leaf body
`0x144d985fd` — the EAC handshake never completes for us.

Confirm by forcing that signal and checking the item ungreys with **no "network error / return to
title" popup** (cf. `CSEventNetworkErrorReturnTitleStep`); then wire a fail-safe patch in the shape of
the landed ones. Success criterion for the whole unblock: with `[debug.probes] session_probe = true`,
using Tarnished's Furled Finger should move `CSSessionManager.lobby_state` off `None`.

**Network-module hook control (mostly eliminated, kept as a baseline):** hooking the getter family
`0x140e0dfc0/dfe0/e000/e0c0/e1d0/e2e0/e3a0/e550` + the chain `0x14073cd40 / 0x140e0ec90 /
0x140e43610` + the controls `is_offline()` `0x140e55180` / `IsEnableOnlineMode()` `0x140e56310`
(log entry + return value + return-address) and scrolling onto the greyed item is how the chain was
shown off-path. Re-run only to re-confirm the family after a game update.

## Re-derivation After a Game Update (per CLAUDE.md)

Addresses shift on a patch; re-find each target from its landmark.

- **`is_offline()`:** statically locate the unique pair `cmp eax,2; sete al` immediately preceded by
  `sub rsp,0x28; call <getter>` and followed by `add rsp,0x28; ret` — that 20-byte function is
  `is_offline()`. Confirm `2 = offline` by the `cmp eax,2`, and that the getter reads a `0/1/2` enum
  with a `>= 3` assert. Re-take the 20-byte function as the landmark, keep `offset +12`,
  `expect 0x0F`, replacement `31 C0 90`. (Equivalently: scan UTF-16 for `Menu.IsEnableOnlineMode`,
  take its getter's callers; each computes `enable && !is_offline()`, naming `is_offline()`.)
- **`IsEnableOnlineMode()` + its cached byte:** scan UTF-16 for `"Menu.IsEnableOnlineMode"`, find the
  unique `lea rdx,[string]` (`0x140e563dc`); its enclosing function is the getter. Its return path
  ends in a unique `movzx eax, byte [<cached bool>]` — that disp names the live byte (`0x144588afc`)
  and the guard dword sits 4 bytes after (`0x144588b00`). The `movzx` is the `force_online_menu_mode`
  landmark.
- **The network-status getter family + sibling block:** find the only writer of the mode enum
  (`0x140e0ea60`, the function whose `mov [enum], eax` you reach from `is_offline`'s getter), list the
  other globals it writes (`0x143b400bc` & neighbours), and take their readers — all clustered in one
  module; each small reader is a status getter. Filter to those with callers *outside* the module.
- **The bool chain:** from getter `0x140e0e550` (whose *only* two callers are the menu cluster), find
  the `E8` to `0x14073cd40` in its body; that getter's return site `movzx eax, byte [cached]` names
  the outer cached byte + its guard (guard = byte+4). Its sole inner `call` (`0x140e0ec90`) and that
  one's `call` (`0x140e43610` → jmp-thunk to the real body in the second `.text`) walk the chain down
  to the service-singleton read. The two callers of `0x140e0e550`, via their enclosing `.pdata`
  functions, are the menu-facing query bridge.

## Reusable Method Notes

The techniques are the reusable part — throwaway scripts live in `/tmp`, not committed (cf.
SESSION-RE-FINDINGS.md > "Re-usable method notes"). Candidates for `scripts/re/` if this kind of pass
recurs:

- **`scripts/re/` capstone helpers** (already committed): PE section map, `.pdata` function bounds,
  rip-relative disp xref finder.
- **Exact `E8`/`E9` call-site finder** (decode-free; `target == site + 5 + rel32`) — reliable where
  the broad rip-relative xref back-scan throws false positives on heavily-aligned globals.
- **Absolute 8-byte pointer search** — finds vtable / fn-pointer-table slots holding a function VA;
  rip-relative xref scans miss these (table entries are absolute), which is why the leaf `0x140764fe0`
  looked "uncalled."
- **`.pdata` enclosing-function lookup** — turn any xref site into its owning function bounds.
- **Incremental-link jump-thunk following** — ER routes many `.text` functions through a `jmp` into the
  second `.text` section (`0x144c0e000`); the real, often control-flow-obfuscated body lives there.
- **Hardware read-watch as an on-path probe** (`watch-write.py --addr <VA> --access rw`, armed on all
  threads) — decisive "is global X read by code path Y?" test; clean when the candidate has few
  readers (noisy for many-reader globals like `0x143b400bc`).
