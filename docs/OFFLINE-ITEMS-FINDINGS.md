# Offline multiplayer-item re-enable — RE findings (static pass, 2026-06-27)

Worker lane `offline-items`. Goal: re-enable Elden Ring's online multiplayer items (Tarnished's
Furled Finger, Furlcalling Finger Remedy, Small Golden Effigy, Duelist's/Bloody/Recusant Finger,
Taunter's Tongue, …) when the game is launched **offline / outside EAC**, where the game greys them
out because FromSoft matchmaking is unreachable. This is the rung-3 unblock: with the items
selectable, an item-use can drive the game's own `CSSessionManager` FSM (see
[SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) > "Blocker found (2026-06-27 friend test)").

This is **static-only** RE on our own legitimately-owned binary (no rig, no running game).
Addresses are facts about the **2026-06-02 `eldenring.exe`** (image base `0x140000000`, two `.text`
sections at `0x140001000` + `0x144c0e000`, `.pdata` at `0x144863000`). Behavioral notes are in my
own words; no upstream ERSC code or decompiler output is reproduced (CLAUDE.md > Clean-room).

## What gates the items (in my own words)

The game keeps a single **network-mode** state as a small int enum in a global at **`0x143d87220`**
(values constrained to `0/1/2` — a runtime assert in the getter fires on `>= 3`). From its one
writer and the consumers below, the meaning is:

| value | meaning |
|---|---|
| 0 | not-yet-determined / initializing |
| 1 | online |
| 2 | **offline** |

- **Getter `0x140e0e960`** — returns the `0x143d87220` enum (with the `>= 3` debug assert). Sole
  reader-of-record for the mode.
- **Writer/recompute `0x140e0ea60`** — the only function that stores `0x143d87220`. It recomputes the
  mode from several sub-queries and is called once, from the network-flow step `0x140c901af`
  (the boot-time online/offline decision; cf. the `CSNetworkFlowStep::STEP_OnlineMode` /
  `STEP_OfflineMode` RTTI strings).
- **`is_offline()` `0x140e55180`** — the central predicate: `return getter() == 2`. **45 call sites**
  across the image gate online features on it. Example consumer (`0x140764fe0`, an "online available
  for menu" leaf): `IsEnableOnlineMode() && !is_offline()` — i.e. when `is_offline()` is false, the
  online surface is reported available. Launched offline/without-EAC the mode is `2`, so
  `is_offline()` is true everywhere, which is what greys the multiplayer items (and blocks the
  session FSM).

So the clean-room equivalent of "ERSC re-enables the items" is to make the game stop believing it is
offline: neutralize `is_offline()` so it always reports **not offline**. That re-opens every
online-gated surface (the multiplayer items among them) without us touching `regulation.bin`, the
session FSM, or the network globals directly.

`Menu.IsEnableOnlineMode` (UTF-16 at `0x142beac58`) is a *separate*, static dev CVAR — a cached
config getter (`0x140e56310`) that defaults **true** in retail and is **not** the dynamic offline
signal. It was the thread that led to `is_offline()` (both of its callers AND-in `!is_offline()`),
but it is not itself the gate. Recorded so a future pass doesn't chase it again.

## The patch (config-gated, fail-safe)

Neutralize `is_offline()` `0x140e55180` to always return 0:

```
0x140e55180: 48 83 EC 28              sub  rsp, 0x28
0x140e55184: E8 ?? ?? ?? ??           call 0x140e0e960        ; eax = network mode
0x140e55189: 83 F8 02                 cmp  eax, 2
0x140e5518c: 0F 94 C0   --> 31 C0 90  sete al  -->  xor eax,eax ; nop   (al/eax = 0 always)
0x140e5518f: 48 83 C4 28              add  rsp, 0x28
0x140e55193: C3                       ret
```

- **pelite landmark (unique, 16 fixed bytes):**
  `48 83 EC 28 E8 ? ? ? ? 83 F8 02 0F 94 C0 48 83 C4 28 C3` — the whole 20-byte function (the 4
  wildcards are the `call` rel32). Verified **exactly one** match in the file; the bare
  `cmp eax,2; sete al; add rsp,0x28; ret` tail alone occurs 4× and is **not** unique, which is why
  the landmark spans the prologue + call.
- **offset:** `+12` (match-start is the function entry; `sete al` is 12 bytes in).
- **expect guard:** `0x0F` (first byte of `sete al`).
- **replacement:** `31 C0 90` (`xor eax,eax` + `nop`) over `0F 94 C0`. The dead `cmp eax,2` is left
  in place (its flags go unused). Wired in `coop/app.rs::apply_boot_patches` next to skip-intros, via
  the existing `patch::overwrite_landmark`; a miss/ambiguous/drifted scan fails safe (game runs
  unmodded, logged) exactly like skip-intros.

Gated behind a new `[gameplay] enable_offline_multiplayer` config flag.

## Confidence & risks

- **Confidence the gate is correct: high.** `is_offline()` is the single most-consulted offline
  signal (45 sites), the `online-available = enable && !is_offline()` leaf nails the polarity, and
  `2 = offline` is pinned by the getter's value and the predicate's `== 2`.
- **Confidence it cleanly re-enables the items with no bad side effects: medium — needs the rig.**
  Forcing "not offline" is broad (all 45 consumers), which is the ERSC-equivalent "make the game
  think it's online." The open risk is whether some consumer, now believing it's online, *attempts*
  a FromSoft matchmaking/login it can't complete without EAC and surfaces a "network error — return
  to title" popup (cf. the `CSEventNetworkErrorReturnTitleStep` RTTI) or hangs, rather than simply
  ungreying the items. ERSC avoids that by also owning the networking (the orchestrator's rung-3
  lane); this patch alone does not. **This is the one thing static analysis can't settle.**
- **Lower-risk fallbacks if the rig shows a popup/hang:** (a) find the narrower item/menu
  availability check downstream and neutralize only that; (b) gate the patch to apply only while in
  the inventory/equip menu. Both are follow-ups once the rig says whether the broad patch is benign.

## Rig verification recipe (hand-off to the orchestrator)

> **Result (2026-06-28, orchestrator rig): the patch is BENIGN but INSUFFICIENT — `is_offline()` is
> NOT the item-grey gate.** Boot path confirmed: the patch resolved with the `expect` guard matching
> live (`patched 'enable_offline_multiplayer': [0F, 94, C0] -> [31, C0, 90]`), the game booted to
> title cleanly and stayed stable, no `network error / return to title` popup, no hang. Then in a
> loaded save (2026-06-28, Michael at the controls): **the multiplayer items were STILL greyed —
> Tarnished's Furled Finger could not be selected.**
>
> **The disproof (load-bearing):** we patched `is_offline()` at its *root* (`0x140e55180`), so **all
> 45 of its consumers** — including the menu-availability leaf `0x140764fe0` — see `is_offline() ==
> false`. The items did not move. Therefore **the multiplayer-item greying does not depend on
> `is_offline()` at all.** The static inference ("`is_offline()` false ⇒ items available") is wrong;
> the real gate is a *different* signal. Candidates for the next pass: (a) `IsEnableOnlineMode()`
> (`0x140e56310`) actually returns **false** outside EAC (the doc's "defaults true in retail" doesn't
> hold for our no-EAC launch), so the leaf `IsEnableOnlineMode() && !is_offline()` stays false on the
> `IsEnableOnlineMode` half — read its live value to confirm; (b) a direct EAC / matchmaking-session
> -availability check the item-usability path consults, unrelated to `is_offline()`. Best found at
> runtime against a loaded-save repro (the inventory rendering a greyed item). The `enable_offline_
> multiplayer` patch is left in place (benign, and forcing `is_offline()` false is plausibly still a
> prerequisite for the *session FSM*, which the doc notes `is_offline()` also gates) — but it is **not
> sufficient** for the items on its own.

1. Set `[gameplay] enable_offline_multiplayer = true` (default), `scripts/rig.sh apply` + launch.
2. Confirm boot log shows `patched 'enable_offline_multiplayer': [0F, 94, C0] -> [31, C0, 90]`.
3. Load a save, open the inventory/pouch, and check the multiplayer items
   (Tarnished's Furled Finger, Furlcalling Finger Remedy, …) are **no longer greyed** and are
   selectable/usable.
4. Watch for any "network error / return to title" popup or hang at boot or on item-use — that would
   mean the broad patch triggers a real online attempt (see risks above) and we drop to a narrower
   target. With `[debug.probes] session_probe = true`, using Tarnished's Furled Finger should now
   move `CSSessionManager.lobby_state` off `None` (the whole point of the unblock).

## Re-derivation after a game update (per CLAUDE.md)

The addresses shift on a patch; re-find them in minutes:

1. **Find `is_offline()` again.** Statically locate the unique pair `cmp eax,2; sete al` that is
   immediately preceded by `sub rsp,0x28; call <getter>` and followed by `add rsp,0x28; ret` — that
   tiny function is `is_offline()`. (Equivalently: scan UTF-16 for `Menu.IsEnableOnlineMode`, take
   its referencing getter's callers; each caller computes `enable && !is_offline()`, naming
   `is_offline()`.)
2. **Confirm `2 = offline`** by the `cmp eax,2`, and that the getter reads a `0/1/2` enum global with
   a `>= 3` assert.
3. Re-take the 20-byte function as the landmark, re-confirm uniqueness, keep `offset = +12`,
   `expect = 0x0F`, replacement `31 C0 90`.

Tools used: `scripts/re/` capstone helpers (PE section map, `.pdata` function bounds, rip-relative
disp xref finder, `E8/E9` call-site finder) — the techniques, not the scratch scripts, are the
reusable part (cf. [SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md) > "Re-usable method notes").

---

# Item-gate follow-up pass (static, 2026-06-28, worker `item-gate`)

Picks up after the disproof above. Static-only on the same pinned `eldenring.exe` (image base
`0x140000000`). Behavioral notes in my own words; addresses are facts. **Nothing here is rig-verified
yet** — static inference already failed once on this exact problem (the disproof), so every claim
below is tagged with honest confidence and paired with a runtime check the orchestrator runs before
we trust it.

## TL;DR

- **The offline mode enum is now fully ruled out as the item gate.** `is_offline()` (`0x140e55180`)
  is the *sole* consumer of the mode enum (`0x143d87220`): the enum's only reader is its getter
  `0x140e0e960`, and that getter's only caller is `is_offline()`. So the disproof (patching
  `is_offline()` to false) already neutralized *every* path that the mode enum can influence. The
  item gate reads a **different** signal — confirmed, high confidence.
- **The prior doc's "polarity nail" was a red herring.** The leaf `0x140764fe0`
  (`IsEnableOnlineMode() && !is_offline()`) has **zero callers** — no `call`, no `jmp`, no
  rip-relative ref, and the one absolute-pointer ref to it is a slot in a **dev/CVAR type-dispatch
  table** in `.data` (`0x143b350e0`, reached only via `jmp [table + type_byte*8]` at `0x140765037`).
  It never gated the inventory. This is *why* `is_offline()` looked like the gate but wasn't.
- **Two concrete next candidates, neither confirmable statically:** (a) `IsEnableOnlineMode()` may
  return **false** at our no-EAC launch — I charted the exact live byte to read; (b) a **sibling
  network-status getter** (same module as `is_offline`, different signal) — charted the getter family.
- **Landed a default-off, fail-safe candidate patch** (`[gameplay] force_online_menu_mode`) that
  forces `IsEnableOnlineMode()` true, as a one-flip rig lever to test candidate (a) end to end.

## Candidate (a): `IsEnableOnlineMode()` — charted for a live read

`IsEnableOnlineMode()` getter = **`0x140e56310`**. It is **not** the "static dev CVAR that defaults
true" the prior doc assumed — it is a lazily-initialized cached bool (MSVC magic-static guard):

- On first call it reads the config value keyed by the UTF-16 string **`"Menu.IsEnableOnlineMode"`**
  (`0x142beac58`) and stores the result byte.
- **Cached return byte: `0x144588afc`** (a writable `.data` byte). Every call's return path converges
  on `movzx eax, byte [0x144588afc]` at `0x140e56444`.
- **Magic-static init-guard dword: `0x144588b00`** (`-1` once initialized).

**Why this is the live test that splits the problem:** the disproof forced `is_offline()` false, so
any item path of the shape `IsEnableOnlineMode() && !is_offline()` now reduces to
`IsEnableOnlineMode()`. The items stayed greyed, so **either** `IsEnableOnlineMode()` is false at
runtime **or** the item path doesn't use this predicate at all. Reading `0x144588afc` settles which:

> **Rig read (orchestrator), after reaching the title screen (the getter inits lazily, so read it
> *after* the menu is up):**
> `python3 -c "import sys; f=open(f'/proc/{sys.argv[1]}/mem','rb'); f.seek(0x144588afc); print(f.read(1).hex())" <pid>`
> (the exe loads at its preferred base `0x140000000`, confirmed in SESSION-RE-FINDINGS.md, so the
> static VA is the live VA). Also read the guard at `0x144588b00` (4 bytes) — if it's not `-1`/non-zero
> the getter hasn't initialized yet, so open any menu that touches online state first, then re-read.
> - **byte == 0 → `IsEnableOnlineMode()` is FALSE outside EAC.** Strong candidate: it's (half of)
>   the gate. Test the landed patch (below) — flip `force_online_menu_mode = true` and see if items
>   ungrey.
> - **byte == 1 → ruled out.** The item path doesn't gate on this predicate; move to candidate (b).

**Confidence this getter is correctly charted: high.** **Confidence it's the actual item gate:
low–medium** — "Menu.IsEnableOnlineMode" reads like a dev/menu CVAR that plausibly defaults true even
without EAC, in which case forcing it true is a no-op. The live read is one cheap command; do it first.

> **RIG RESULT (orchestrator, 2026-06-28): candidate (a) RULED OUT.** Read `0x144588afc` live at the
> title screen (`watch-write.py --peek 0x144588afc --peek-len 8`): `01 00 00 00 05 05 00 80` — **byte0
> = 1**, so `IsEnableOnlineMode()` is **TRUE** outside EAC. Forcing it true (`force_online_menu_mode`)
> is therefore a **no-op** and cannot be the item gate. Combined with the earlier disproof
> (`is_offline()` ruled out) and the leaf `0x140764fe0` being callerless, the whole
> `IsEnableOnlineMode() && !is_offline()` family is eliminated. **The item-grey gate is candidate (b):
> a different signal the item-usability path consults (a direct EAC / matchmaking-session-availability
> check), found via the runtime recipe below.** `force_online_menu_mode` is left in (inert, default
> OFF, now known no-op) pending removal; the next move is the Frida pass.

### The landed candidate patch (`[gameplay] force_online_menu_mode`, default OFF)

Wired in `coop/app.rs::apply_boot_patches`, behind a **new, default-off** flag so it can't affect
normal play and the orchestrator can A/B it independently of `enable_offline_multiplayer`:

```
0x140e56444: 0F B6 05 B1 26 73 03   movzx eax, byte [0x144588afc]   ; the getter's sole return site
          -> B8 01 00 00 00 90 90   mov eax, 1 ; nop ; nop          ; force "online mode enabled"
```

- **landmark (unique, 7 bytes):** `0F B6 05 B1 26 73 03` — exactly one match in the image (the
  cached-bool disp `B1 26 73 03` is what makes it unique). offset `0`, expect `0x0F`.
- All of the getter's code paths (the two fast-path `jle`/`jne` exits and the post-init fall-through)
  converge on this `movzx`, so patching it forces the return on **every** call.
- Fail-safe exactly like the other boot patches: a missed/ambiguous/drifted scan no-ops + logs.

## Candidate (b): a sibling network-status getter (same module, different signal)

The offline/online state is computed once at boot by **`0x140e0ea60`** (the only writer of the mode
enum; called from the boot network-flow step). It does **not** compute a single offline bool — it
writes a whole block of related state from a series of platform/EAC/login sub-queries:

- mode enum → `0x143d87220` (via `0x140e5b530`) — the `is_offline()` source, **ruled out**.
- a sibling status dword → **`0x143b400bc`** (13 readers) and friends `0x143b400b0` (6),
  `0x143b400b8`, `0x143b400c0`, plus `0x143d87224` and a **byte at `0x143d87228`**.

Those sibling globals are read **only** inside the same network-status module
(`~0x140e0dfc0..0x140e0ec77`), which exposes them through a family of small **getter functions** — the
public "what's my online status" API the rest of the game calls. The ones with external callers:

| getter | reads | external callers (sample) |
|---|---|---|
| `0x140e0dfc0` | `0x143b400bc` | `0x1408c6bcf`, `0x1408cde5d` |
| `0x140e0dfe0` | `0x143b400bc` | `0x140909b1c`, `0x140933909`, `0x140997301/451` |
| `0x140e0e000` | `0x143b400bc` | `0x140b04224` |
| `0x140e0e0c0` | (built object) | `0x140d77290/2ce/60c` |
| `0x140e0e1d0` | (built object) | `0x140d76e90`, `0x140d77b0c` |
| `0x140e0e2e0` | `0x143b400bc` | `0x140b042a7` |
| `0x140e0e3a0` | `0x143b400bc` | `0x140cbbb48` |
| `0x140e0e550` | (built object) | `0x140d76fa0`, `0x140d77bec` |
| `0x140e0e960` | mode enum | `is_offline()` only |

The `0x140d76xxx`/`0x140d77xxx` caller cluster queries three of these getters and is the most
multiplayer-menu-shaped, but I could **not** statically tie any one getter to the *inventory item
greyed* decision without param/menu symbols — and guessing here is exactly what failed last time.

**Confidence: low** as to which (if any) is the gate; **high** that this getter family is the right
*neighborhood* of "online availability" signals distinct from the mode enum.

## The decisive runtime recipe (hand-off — do this if the cheap read in (a) says "ruled out")

Static can narrow but not pin the gate; one runtime observation pins it directly. Repro: load a save
where a multiplayer item (Tarnished's Furled Finger) is visible and **greyed** in the pouch.

1. **First, the cheap split:** do the `0x144588afc` live read in candidate (a). If `0`, test
   `force_online_menu_mode` and you may be done. If `1`, continue.
2. **Pin the gate by watching the greyed item's availability read.** With the inventory open on the
   greyed item, the menu code that decides "greyed" reads some online-status signal. Use Frida (rig
   action — RUNTIME-RE.md option B) to hook the **getter family above** (`0x140e0dfc0`, `…dfe0`,
   `…e000`, `…e0c0`, `…e1d0`, `…e2e0`, `…e3a0`, `…e550`) plus `is_offline()` (`0x140e55180`) and
   `IsEnableOnlineMode()` (`0x140e56310`); log entry + return value, then scroll onto / try-to-use the
   greyed item. **The getter that fires from menu/item code on that interaction, returning the
   "offline/unavailable" value, is the gate.** (A hardware *read*-watchpoint on `0x143b400bc` works
   too but it has 13 readers and will be noisy; the getter hooks are cleaner.)
3. **Confirm by forcing it.** Patch the identified getter to return its "online" value (or force the
   global it reads) and verify the item ungreys with no "network error / return to title" popup. Then
   we wire that as the real patch (replacing or augmenting `force_online_menu_mode`).

If even the getter hooks don't fire on the item interaction, the gate is **outside** this network
module — most likely a live matchmaking/login-session availability check (EAC handshake never
completes for us), reachable by Frida-stalking the item-availability function itself from the
EquipParamGoods/use-condition side. That's the deeper fallback.

## Re-derivation after a game update (this pass)

- **`IsEnableOnlineMode()` + its cached byte:** scan UTF-16 for `"Menu.IsEnableOnlineMode"`, find the
  unique `lea rdx,[string]` (`0x140e563dc`); its enclosing function is the getter. Its return path
  ends in a unique `movzx eax, byte [<cached bool>]` — that disp names the live byte
  (`0x144588afc` here) and the guard dword sits 4 bytes after it (`0x144588b00`). The `movzx` is the
  `force_online_menu_mode` landmark.
- **The network-status getter family:** find the only writer of the mode enum (`0x140e0ea60`, the
  function whose `mov [enum], eax` you reach from `is_offline`'s getter), list the other globals it
  writes (`0x143b400bc` & neighbours), and take their readers — all clustered in one module; each
  small reader function is a status getter. Filter to those with callers *outside* the module.

## Reusable method note

New scan shapes used this pass, beyond the prior `scripts/re/` capstone helpers: **(1) absolute
8-byte pointer search** (find vtable / fn-pointer-table slots that hold a function VA — rip-relative
xref scans miss these because table entries are absolute, which is why the leaf looked "uncalled");
**(2) `.pdata` enclosing-function lookup** to turn any xref site into its owning function bounds. Both
are the reusable part; the throwaway scripts are not committed (kept in `/tmp`), per the
SESSION-RE-FINDINGS convention.

---

# Item-gate STATIC narrowing pass (2026-06-28, worker `item-gate-static`)

Picks up after candidates (a)/(b) above: the mode enum (`0x143d87220`) and `IsEnableOnlineMode`
(`0x144588afc`) are both **rig-eliminated** — do not re-pursue. My lane is the **static legwork to
make the orchestrator's runtime Frida pass short and targeted**: find the *other* online/EAC signal
the inventory-item-usability path consults. Static-only on the same pinned `eldenring.exe`
(image base `0x140000000`); behavioral notes in my own words; addresses are facts. Nothing here is
rig-verified — static inference has failed twice on this exact problem, so each claim is tagged with
honest confidence and paired with the runtime check that settles it.

## TL;DR — the narrowed Frida target

> **RIG RESULT (orchestrator, 2026-06-28): this chain is ALSO ruled out — it's not on the item-grey
> path.** First the cheap peek: with a save loaded and the greyed Tarnished's Furled Finger highlighted,
> the cached bytes `0x143d6a840` and `0x143d87228` both read `01` (available) — identical to their
> title-screen baseline, no flip. Then the decisive probe: a **read-watch** (`watch-write.py --addr
> 0x143d6a840 --access rw`, a 4-byte read-or-write hardware watchpoint, armed on all 104 game threads)
> fired **zero times** across full inventory navigation onto/off the greyed item. So the menu-grey
> decision **never reads this bool** — the chain is consulted, but not by the item-availability path.
> Three static candidate families are now rig-eliminated (mode enum / `is_offline()`, `IsEnableOnlineMode`,
> this online-available chain). The read-watch is proven as a clean "is global X on the item-grey path?"
> probe — but the gate is a signal none of the static passes have found. **Next is a runtime EXECUTION
> trace** (Frida-stalker / hooking the inventory item-availability draw, or the EquipParamGoods "usable
> now" check) to see what the grey decision actually branches on — static guessing is exhausted here.

Found a **lazily-cached "online-play-available" boolean chain that is genuinely distinct from both
eliminated signals** (it does not read the mode enum nor the `IsEnableOnlineMode` byte), and that is
**reached from the multiplayer-menu code** that the prior pass had only loosely flagged. The chain
also survives the `is_offline()` disproof untouched — the disproof patched only the mode-enum getter,
and nothing in this chain depends on it. That makes it the strongest "different signal" candidate to
date. The whole thing is **four functions + two cached bytes**, so the Frida pass is tiny.

```
multiplayer-menu cluster            network-status getter           cached online-available bool chain
0x140d76fa0 / 0x140d77bec  ───────▶  0x140e0e550  ──(7 call sites)──▶  0x14073cd40 ──▶ 0x140e0ec90 ──▶ 0x140e43610
(the two ONLY callers of                (consults the bool             (caches byte      (caches byte    (leaf body at
 getter 0x140e0e550)                     chain; independent of          0x143d6a840,      0x143d87228,    0x144d985fd;
                                         is_offline/mode getter)        guard 0x143d6a844) guard 0x143d8722c) reads a service
                                                                                                           singleton, derives
                                                                                                           the bool)
```

## The candidate chain (each function: what it reads, why it's a plausible item-grey gate)

All four are **magic-static lazy getters / their leaf**, MSVC pattern (TLS guard at `gs:[0x58]`,
init-guard dword == `-1` once computed, cached result byte). Confidence the chain is correctly
*charted*: **high** (exact `E8`-call finder + tight in-module rip-reader scans, the same techniques
the prior passes used). Confidence it's the *actual* item gate: **medium** — higher than (a)/(b)
because it's menu-reachable and disproof-independent, but only the rig can pin it.

| addr | role | reads / caches | external callers | why a candidate |
|---|---|---|---|---|
| **`0x140e0e550`** | network-status getter | builds a status object; consults the bool chain below (7 call sites to `0x14073cd40` in its body region `0x140e0e664..0x140e0e944`) | **only** `0x140d76fa0`, `0x140d77bec` (the multiplayer-menu cluster) | the *one* getter in the prior "getter family" that the menu cluster actually calls — the bridge from menu code to the online signal |
| **`0x14073cd40`** | outer cached-bool getter | caches **bool `0x143d6a840`** (guard `0x143d6a844`); value comes from `0x140e0ec90` | two calling functions: the menu getter `0x140e0e550` (7 call instructions inside it) + `0x140dfc9d8` | the boolean the menu getter consults; a single byte the rig can peek/force |
| **`0x140e0ec90`** | inner cached-bool getter | caches **bool `0x143d87228`** (guard `0x143d8722c`); value computed by `0x140e43610(1)` | sole caller `0x14073cd85` (which is `0x14073cd40`) | the inner cache; distinct byte, distinct guard from `IsEnableOnlineMode` (`0x144588afc`) |
| **`0x140e43610`** | leaf compute (real body `0x144d985fd`) | reads a **service-manager singleton (ptr at `0x144842d40`)** and derives the bool from a status compare | `0x140e0ecd7` (= `0x140e0ec90`) | the raw read — *this* is where the offline/no-EAC state actually enters. Body is control-flow-obfuscated (movabs+push return trampolines, stack-pivot `lea rsp`), so its exact predicate is best read at runtime, not decoded |

Supporting facts (exact, reliable — `E8`/`E9` finder and `.pdata` bounds):

- **Menu cluster** (the consumers nearest the inventory decision): functions `0x140d76d20..0x140d770cf`,
  `0x140d771d0..0x140d773f4`, `0x140d77550..0x140d7782e`, `0x140d77ae0..0x140d77bba`,
  `0x140d77bc0..0x140d77c9a`. They query the network-status getter family (`0x140e0e0c0/e1d0/e550`).
  `0x140d76d20` and `0x140d77bc0` have **no direct callers** (reached via vtable / obfuscated dispatch
  — consistent with menu virtual methods); the others are dispatched from `0x140d6aXXX`.
- **Independence from the ruled-out path (load-bearing):** getter `0x140e0e550`'s body calls
  `0x14073cd40` (the bool) and **not** `0x140e0e960` (the mode-enum getter) nor `0x140e55180`
  (`is_offline()`). So forcing `is_offline()` false (the disproof) cannot have moved this signal — which
  is exactly why the items stayed greyed and why this is the next thing to look at.
- The boot writer `0x140e0ea60` separately computes a **sibling status block** —
  `0x143b400b0/b4/b8/bc/c0` and `0x143d87224` — from a family of platform/EAC/login sub-queries (it
  calls `0x140e5b410/530/650/770/890/9b0`, each fetching a service object and calling a virtual on
  it). These siblings are read **only inside** the status module (`0x140e0dfc0..0x140e0ec80`) and
  surfaced through the getter family. They are **not** cleared by the `is_offline()` patch either, so
  they remain at their offline-computed values — secondary candidates.
  - **Correction to the prior pass:** that pass (above) listed the byte `0x143d87228` under this
    boot-writer block. It is **not** boot-written — the *only* writer of `0x143d87228` in the whole
    image is `0x140e0ecdc`, inside the magic-static getter `0x140e0ec90` (verified by an exact write
    xref). `0x143d87228` just happens to sit in `.data` immediately after the boot block
    (`…87220` enum, `…87224` dword, `…87228` byte, `…8722c` guard) — **adjacency, not a shared
    initializer.** So it has exactly one initializer (the chain's lazy compute), consistent with the
    MSVC magic-static pattern, and `0x143d8722c` is confirmed its init-guard by the thread-safe-init
    acquire/release calls (`0x1424fad58` / `0x1424facf8`) and the `cmp [guard], -1` it wraps — not a
    sibling status field.

## The cheap pre-read (orchestrator, no Frida — do this first)

The two cached bytes initialize lazily, and they init **at the moment the menu queries them** — i.e.
when you open the inventory on the greyed item. That makes the read both cheap and perfectly timed:

1. Boot to title. Read the two init-guards (4 bytes each): `0x143d6a844` and `0x143d8722c`. If a guard
   is **not** `-1`/non-zero, that bool hasn't been computed yet.
2. **Open the inventory/equip menu on the greyed Tarnished's Furled Finger** (this drives the menu
   cluster → getter `0x140e0e550` → the bool chain, forcing init). Re-read.
3. Read the chain's cached bools (1 byte each): **`0x143d6a840`** and **`0x143d87228`**. Separately,
   dump the boot sibling block `0x143b400b0..0x143b400c0` (20 bytes) + `0x143d87224` (4). (`0x143d87228`
   belongs to the chain, not the boot block — read it once, under the chain.)
   - The exe loads at its preferred base `0x140000000` (SESSION-RE-FINDINGS confirmed), so static VA ==
     live VA. Read via `watch-write.py --peek <VA> --peek-len <n>` or `/proc/<pid>/mem`.
   - Whichever of these bytes reads "unavailable/offline" while the item is greyed is the live gate
     candidate. **Polarity is unknown** — read it off directly here (i.e. determine whether `1` means
     "available" or "offline" from the value seen while the item is greyed).

## The decisive Frida pass (hand-off — short, because the chain is four functions)

Repro: loaded save where Tarnished's Furled Finger is **visible and greyed** in the pouch.
Hook (RUNTIME-RE.md option B), log `entry + return value + return-address (caller)`:

- the chain: **`0x14073cd40`**, **`0x140e0ec90`**, **`0x140e43610`**
- the menu-facing getter: **`0x140e0e550`** (and, if you want the full family, the prior list
  `0x140e0dfc0/dfe0/e000/e0c0/e1d0/e2e0/e3a0`)
- the controls: `is_offline()` `0x140e55180` and `IsEnableOnlineMode()` `0x140e56310` (expect these to
  fire returning their known values — they're the eliminated baseline)

Then scroll onto / try-to-use the greyed item. **The function in the chain that fires from menu/item
code, returning the "unavailable" value on that interaction, is the gate.** Confirm by forcing it
(patch its cached-byte return, or force the byte in memory) and checking the item ungreys with **no
"network error / return to title" popup** (cf. `CSEventNetworkErrorReturnTitleStep`). If even these
hooks don't fire on the item interaction, the gate is *below* this module — a live
matchmaking/login-session availability read inside the service singleton (`0x144842d40`) reached only
through the obfuscated leaf body `0x144d985fd`; stalk it from the EquipParamGoods/use-condition side
(the deeper fallback the prior pass named).

## Clean patch candidate (note only — runtime confirm comes first)

If the rig pins the bool chain as the gate, the fail-safe patch mirrors the existing ones: force the
cached-bool return to "available." Cleanest single landmark is `0x14073cd40`'s sole return site
`movzx eax, byte [0x143d6a840]` at **`0x14073cd9c`** (`0F B6 05 9D DA 62 03`) → `mov eax,1; nop; nop`
(`B8 01 00 00 00 90 90`), exactly the shape of `force_online_menu_mode`. **Do not land this until the
Frida pass confirms polarity and that forcing it ungreys the item without a network-error popup** —
static has guessed wrong twice here.

## Re-derivation after a game update (this pass)

- **The bool chain:** from getter `0x140e0e550` (the network-status getter whose *only* two callers are
  the menu cluster), find the `E8` to `0x14073cd40` in its body; that getter's return site
  `movzx eax, byte [cached]` names the outer cached byte + its guard (guard = byte+4). Its sole inner
  `call` (`0x140e0ec90`) and that one's `call` (`0x140e43610` → jmp-thunk to the real body in the patch
  `.text`) walk the chain down to the service-singleton read.
- **The menu cluster:** the two callers of `0x140e0e550`; their enclosing `.pdata` functions are the
  menu-facing query bridge.

## Reusable method note

Added to the `/tmp` scratch helpers this pass (techniques, not committed): an **exact `E8`/`E9`
call-site finder** (decode-free; `target == site+5+rel32`) — reliable where the broad rip-relative
xref back-scan throws false positives on heavily-aligned globals — and **incremental-link
jump-thunk following** (ER routes many `.text` functions through a `jmp` into the second `.text`
section at `0x144c0e000`; the real, often control-flow-obfuscated body lives there). Both belong in
`scripts/re/` if this kind of pass recurs.
