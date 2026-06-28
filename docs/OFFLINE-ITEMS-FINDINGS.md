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
