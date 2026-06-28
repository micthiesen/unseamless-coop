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
