# Death Debuffs (Rot Essence)

How ERSC's stacking death penalty works and how we reimplement it. On each death the mod hangs a
debuff (SpEffect) on the player; the stack grows per death and is **cured only by resting at a Site
of Grace**. ERSC's locale names five tiers — Emaciation (stamina-recovery down), Hopelessness (max
HP/FP/stamina down), Decay (rune acquisition + item discovery down), Vulnerability (defence down),
Despair (attack down) — each driven by an applied SpEffect. The config toggle is
[`death_debuffs`](../crates/unseamless-core/src/settings.rs) (already declared in the settings
registry; `c.gameplay.death_debuffs`).

This is a **research note**, not an implemented feature. Game-internal claims are grounded in the
pinned `fromsoftware-rs` SDK (rev `8c67a84`, paths cited) or flagged as behavioral inference to
confirm on the rig. Per [CLAUDE.md](../CLAUDE.md) > Clean-room hygiene, ERSC is closed and
Themida-packed, so the *exact* SpEffect IDs and stacking rules it uses are not readable and not
copied here — we reimplement the effect from observed behavior + the public param/SpEffect system.

> Good news for our toolchain: unlike the splash-skip and FMG features, **death debuffs need no new
> RE.** The whole mechanism (apply/remove a SpEffect on the player by ID) is already a callable SDK
> method at our pin. This is a Layer-1-adjacent feature gated only by rig observation of the death
> and grace *signals*, not by any uncharted function.

## A. The Apply / Remove Mechanism (Charted Today)

The thing SDK-COVERAGE marks **PARTIAL** ("struct layout we can read but no action method") is
**stale for apply/remove.** The pinned SDK ships both as real wrapper methods.

### `apply_speffect` / `remove_speffect` are callable

`cs/chr_ins.rs` defines, on `Subclass<ChrIns>` (so every `PlayerIns`/`EnemyIns` gets them via the
`#[for_all_subclasses]` impl):

```rust
fn apply_speffect(&mut self, sp_effect: i32, dont_sync: bool) { /* rva_to_va + transmute + call */ }
fn remove_speffect(&mut self, sp_effect: i32)                 { /* rva_to_va + transmute + call */ }
```

Both resolve a game function by RVA and call it — the same pattern as the charted
`CSMenuManImp::display_status_message` (`cs/menu_man.rs`). The RVAs are in the bundle:

| Field | RVA (WW 2.6.2.0 / JP 2.6.2.1) | Source |
|---|---|---|
| `chr_ins_apply_speffect` | `0x3e8be0` | `rva/bundle.rs:15`, `rva/rva_ww.rs:14`, `rva/rva_jp.rs:14` |
| `chr_ins_remove_speffect` | `0x3ee0b0` | `rva/bundle.rs:16`, `rva/rva_ww.rs:15`, `rva/rva_jp.rs:15` |

**Signature (confirmed from the SDK wrapper, not inferred):**
- apply = `fn(&mut ChrIns-subclass, sp_effect_id: i32, dont_sync: bool)`
- remove = `fn(&mut ChrIns-subclass, sp_effect_id: i32)`

The `dont_sync` bool is interesting: the apply path can optionally tell the game *not* to network the
effect. For death debuffs we want each player's own debuff applied locally (the death is the local
player's), so `dont_sync = true` is the likely choice — confirm on the rig whether co-op partners
should each carry their own stack (almost certainly yes; ERSC debuffs are per-player). The same
`dont_sync` knob is why this method underlies **give-ember** and **rune-arc sharing** too (see §F).

Calling it is exactly our existing pattern: reach `WorldChrMan` via `crate::sdk::with_instance_mut`,
take `main_player` (`Option<OwnedPtr<PlayerIns>>`, `cs/world_chr_man.rs:59`), and call
`apply_speffect(id, true)` on it.

> RVA caveat (project-wide): `rva::get()` only resolves on ER **2.6.2.0 WW / 2.6.2.1 JP** and panics
> otherwise (`rva.rs`). Any apply/remove call assumes the rig is on that version — the same
> constraint every RVA-backed call already carries.

### Reading active SpEffects (to detect/stack)

`cs/sp_effect.rs` gives us read access for free. `ChrIns::special_effect` is an
`OwnedPtr<SpecialEffect>` (`cs/chr_ins.rs:203`); `SpecialEffect::entries()` iterates the active
effects as a linked list, and each `SpecialEffectEntry` exposes:

- `param_id: i32` — which SpEffect is active (so we can detect "tier N already on the player"),
- `removal_timer: f32`, `duration: f32` — time left / total (so we can tell a permanent debuff from
  a transient one, and avoid re-stacking).

So we don't have to track the stack purely in our own state: we can **read the player's live
SpEffect list each frame** and reconcile against the intended stack. That makes the feature
self-healing across loads (if the game drops an effect we re-add it) and lets the cure-at-grace step
just call `remove_speffect` for each tier ID we own.

## B. The SpEffect IDs

Two honest layers here: which param *fields* produce each tier (charted), and which concrete *row
IDs* to apply (a choice we make).

### The param levers (charted — `param/generated.rs`, `SP_EFFECT_PARAM_ST`, INDEX 86)

Every effect ERSC's tiers describe maps onto a named `SP_EFFECT_PARAM_ST` field with a `set_*`
accessor. Field offsets/names read directly from `param/generated.rs` (struct at line 53417):

| ERSC tier | In-game effect | `SP_EFFECT_PARAM_ST` field(s) | Notes |
|---|---|---|---|
| **Emaciation** | reduce stamina recovery | `stamina_recover_change_speed: i32` | "add/subtract to the standard stamina recovery speed" — negative = slower. (Recent ERSC made this stamina-recovery-only; older builds also cut attributes.) |
| **Hopelessness** | reduce max HP/FP/stamina | `max_hp_rate`, `max_mp_rate`, `max_stamina_rate` (all `f32`) | multipliers; `1.0` = normal, e.g. `0.9` = −10% each. (FP = "MP" in FromSoft naming.) |
| **Decay** | reduce rune acquisition + item discovery | `have_soul_rate: f32` (rune gain) + `item_drop_rate: f32` (**requires `state_info` = 66** to take effect) | item discovery needs the `state_info`/`save_category` gate set on the row, per the modding wiki. |
| **Vulnerability** | reduce defence | `physics_diffence_rate`, `magic_diffence_rate`, `fire_diffence_rate`, `thunder_diffence_rate`, `dark_diffence_rate` (all `f32`) | defence *rate* multipliers (note FromSoft's "diffence" spelling). |
| **Despair** | reduce attack | `physics_attack_rate` … `dark_attack_rate` (the `*_attack_rate: f32` family) | outgoing-damage multipliers; `1.0` = normal. |

Persistence levers on the same row (also charted, confirmed against the modding wiki):
- `effect_endurance: f32` — `-1` = permanent (the debuff lasts until we remove it; what we want),
  `0` = instant, positive = seconds.
- `state_info` / `save_category` (`i16`/`i8`) — category/save gating; `state_info = 66` is the one
  that *enables* `item_drop_rate`, and `save_category` controls whether an effect survives reload.

### Which row IDs to apply (our decision: define our own rows)

There is **no published list** of the exact SpEffect IDs ERSC applies (closed + Themida; the
ersc-docs only say "Debuffs (Rot Essence) … cured when you sit at a bonfire"). Vanilla ER has no
ready-made "−10% all defence, permanent, player-applied" rows matching these five tiers either —
vanilla rot/curse SpEffects don't line up with ERSC's design.

**Recommendation: define our own SpEffectParam rows at install via `SoloParamRepository`, in a
private ID block, and apply those.** We already commit to runtime param writes
(`SoloParamRepository::get_mut::<SpEffectParam>(id)` / `rows_mut`, `cs/solo_param_repository.rs:331`)
for scaling, and SDK-COVERAGE marks params **CHARTED**. The approach:

1. Pick a high, unused ID block (e.g. `7_400_0xx` — well above vanilla rows; **verify free on the
   rig** before committing, params get added across patches). One row per tier, with stacking
   variants if we want potency to grow per stack (see §D).
2. At install, write each row's fields per the table above with `effect_endurance = -1.0` (permanent
   until cured) and the right `state_info` for item discovery.
3. On death, `apply_speffect(our_row_id, true)` the next tier; at grace, `remove_speffect` each.

This is cleaner than hunting for vanilla rows that *almost* match, sidesteps clobbering a real
effect, and keeps potency a config knob. ERSC notes (community/changelog) that Rot Essence got
"double debuff potency per stack" — exactly the kind of tuning we'd own in our own rows rather than
inherit.

> Caveat: defining our own rows assumes `SoloParamRepository` row *insertion* (not just mutation of
> an existing row) works at runtime for SpEffectParam. `get_mut`/`rows_mut` mutate existing rows;
> whether we can add a brand-new ID, or must repurpose an existing unused-looking row, is the one
> rig question on this path. Fallback: pick existing high vanilla rows that already carry the field
> we want and overwrite them (riskier — could collide).

## C. Event Detection

Two rising edges to detect on a frame task: **died** and **rested at grace**.

### Death

Cleanest signal: poll the **main player's HP** via `WorldChrMan`. `main_player` →
`PlayerIns`/`ChrIns` → `modules.data` (`OwnedPtr<CSChrDataModule>`, `cs/chr_ins/module.rs:43`) →
`hp: i32`, `max_hp: i32` (`cs/chr_ins/module/data.rs:29-30`). Detect the **rising edge of `hp <= 0`**
(was >0 last frame, now ≤0) so we fire once per death, not every frame of the death state.

- Phase: a `ChrIns`-ordered phase (the project's `WorldChrMan_PostPhysics` worked example) so HP is
  read after the game writes it — and respect the `characters()`/load-status caveat from
  [CLAUDE.md](../CLAUDE.md): only deref `main_player` when its `load_state.is_active()`
  (`cs/chr_ins.rs:486`) to avoid touching a mid-teardown `ChrIns`.
- Alternative/confirmation signal: the "YOU DIED" status message is `STATUS_MESSAGE_YOU_DIED = 5`
  (`cs/menu_man.rs:14`) — that's a *display* constant, not a queryable event, so HP-edge is the
  primary detector; we don't have a clean "did the YOU DIED banner fire" read.
- Open question for the rig: HP also hits ≤0 transiently in some scripted/cutscene cases. Confirm the
  edge corresponds to a real death+respawn (e.g. gate on the player actually entering the death/respawn
  flow), or debounce so we don't double-count.

### Rest at a Site of Grace (the cure)

Two candidate signals, pick after a rig observation:

1. **Event flag via `CSEventFlagMan`** (`cs/event_flag.rs`, **CHARTED** `get_flag`/`set_flag`).
   Resting at grace reinitializes loaded event scripts (community-confirmed: "when you … warp to a
   grace, all loaded event scripts are loaded in again"), and there are known grace/last-grace flags.
   If a single "rested" flag or a reliable last-grace flag exists, watch its rising edge. **Which flag
   is the rig task** — the Elden Ring Debug Tool exposes grace/last-grace management and a flag
   set/unset tab, so the flag ID is discoverable on the rig.
2. **State transition** — the menu/world enters the rest-at-grace state. Less clean to read at our pin
   (no charted "is resting" bool surfaced), so event-flag is preferred.

Because a rest also reloads event state, a robust cure is: on the grace edge, **remove every debuff
row we own** (`remove_speffect` per tier ID) and reset our in-memory stack counter to 0.

## D. Stacking + Persistence

Model the stack as a simple **counter → tiers applied**:

- Keep a `stack: u8` in the feature. On a death edge, increment and apply the row(s) for the new
  level. The five named tiers suggest either (a) one new tier per death up to 5 then deeper stacks
  intensify, or (b) all-at-once scaling potency. ERSC's "double potency per stack" remark points at
  potency growth; the exact curve is **ours to tune** (we own the rows) — start with "death N applies
  tier min(N,5), deaths past 5 re-apply the strongest" and refine on the rig.
- **Persistence across deaths until grace:** with `effect_endurance = -1.0` (permanent), an applied
  debuff stays on the player through subsequent deaths/respawns — we only remove it at grace. We
  should still **reconcile each frame** against the live SpEffect list (§A): if a load transition or
  the game drops one of our rows, re-apply it so the stack survives map changes. (This is why reading
  `SpecialEffect::entries()` matters — it's the source of truth, our counter is a cache.)
- **Cure at grace clears all:** the grace edge removes every owned row and zeroes the counter. After
  that, dying again starts the stack from tier 1.

## E. Building `features/death_debuffs.rs`

A single `Feature` (the [`Feature` trait](../crates/unseamless-coop/src/feature.rs), like
`session_limit.rs`), gated on the `death_debuffs` config flag at construction, phase set to a
`ChrIns`/`PostPhysics`-style phase (HP read after the game writes it). Sketch:

```text
struct DeathDebuffs { stack: u8, prev_hp_positive: bool, prev_grace: bool, rows: [i32; 5] }

on install (once): write our 5 SpEffectParam rows via SoloParamRepository (effect_endurance = -1, fields per §B)

on_frame:
  with_instance::<WorldChrMan> { main_player, active && load_state.is_active() }:
    hp = main_player.modules.data.hp
    // death edge
    if prev_hp_positive && hp <= 0 { stack += 1; apply next tier row via main_player.apply_speffect(id, true) }
    prev_hp_positive = hp > 0
    // reconcile: ensure every row up to `stack` is present in special_effect.entries(); re-apply if missing
  grace = CSEventFlagMan.get_flag(REST_FLAG)        // flag id is the rig task
  if grace && !prev_grace { for id in owned rows: main_player.remove_speffect(id); stack = 0 }
  prev_grace = grace
```

Notes:
- Use `crate::sdk::with_instance_mut::<WorldChrMan, _>` for the player (mutable, since apply/remove
  take `&mut`) and `with_instance::<CSEventFlagMan, _>` for the flag — same accessors the existing
  features use.
- Per the project's [error-surfacing rule](../CLAUDE.md): this is past install, so any failure here
  **degrades + toasts**, never `guard::fatal`. If a SpEffect call panics, disable the feature for the
  session and notify via `unseamless-core/notifications.rs`.
- Logging rule: the per-frame HP/flag reads must be `log::debug!`/`trace!`; only milestones
  (a debuff applied, cured at grace) may be `info!`.

## F. Shared Mechanism: Give-Ember and Rune-Arc

`apply_speffect` is **the same lever** behind two other ERSC features in
[FEATURES.md](FEATURES.md): **give ember** and **rune-arc sharing** are "apply SpEffect X to the
player" actions, just triggered from a session action instead of a death edge. So building the apply
helper for death debuffs gives us those for near-free — they differ only in *which* row and *what
triggers it* (a menu action vs. the death/grace edges). Worth factoring the "apply a SpEffect to the
main player by ID" call into a small shared helper (e.g. on the sdk module) that all three use.

## Status / Next Steps

- [x] Confirm `chr_ins_apply_speffect` / `chr_ins_remove_speffect` exist in the RVA bundle and are
      callable (they are — wrapper methods on `Subclass<ChrIns>`, `cs/chr_ins.rs:339-355`).
- [x] Confirm the call signature (`apply(id: i32, dont_sync: bool)`, `remove(id: i32)`).
- [x] Map each ERSC tier to charted `SP_EFFECT_PARAM_ST` fields.
- [x] **Host-tested stacking model** (`core::death_debuffs`): death counter → cumulative tiers +
      an intensity curve for deaths past the cap. Fully config-driven ([`DeathDebuffTuning`]).
- [x] **Configurable + reasonable defaults**: `[death_debuffs]` section — `max_tiers` (5),
      `intensify_past_cap` (true), `intensity_step_percent` (50), `max_intensity_percent` (300);
      validated/clamped. The on/off toggle stays the synced `gameplay.death_debuffs`.
- [x] **Shared apply/remove helper**: `sdk::apply_speffect_to_main_player` /
      `remove_speffect_from_main_player` (active-player guarded) — reused by give-ember + rune-arc.
- [x] **Death-edge detection + feature scaffold** (`coop/features/death_debuffs.rs`): rising edge of
      main-player `hp<=0` (active-guarded, `WorldChrMan_PostPhysics` phase) drives the model; live
      config; inert + silent until the two rig blanks below are filled (no bogus SpEffect ids sent).
- [ ] **Rig (solo):** confirm the death edge corresponds to a real death/respawn and debounce
      scripted HP dips (the feature `debug!`-logs each detected death for this).
- [ ] **Rig (solo):** find the "rested at grace" event flag (ER Debug Tool grace/flag tabs); set
      `GRACE_REST_FLAG` in `features/death_debuffs.rs`.
- [x] **Effect-value model** (`core::death_debuffs::DebuffTier::rates(intensity)` → `SpEffectRates`):
      the concrete `SP_EFFECT_PARAM_ST` rate-field values each tier writes, scaled by intensity,
      host-tested. So the rig no longer has to *design* the values — only confirm row ids + insertion
      and write these onto the rows. `reconcile()` already computes + `debug!`-logs them per death.
- [ ] **Rig (solo):** confirm `SoloParamRepository` can *insert* new SpEffectParam rows at runtime
      (vs. only mutating); pick a verified-free ID block; write each tier's `rates(...)` onto its row
      and set `TIER_ROW_IDS`. Fallback: overwrite high unused rows. Tune the base magnitudes (and
      confirm the `stamina_recover_change_speed` units + the poise/posture field) to taste on the rig.
- [ ] Confirm `dont_sync = true` is right (per-player debuff) and partners each carry their own stack.
- [ ] Update [SDK-COVERAGE.md](SDK-COVERAGE.md): SpEffect **apply/remove is CHARTED**, not PARTIAL.

## Sources

- Pinned SDK `fromsoftware-rs` rev `8c67a84`, under `crates/eldenring/src/` — read directly:
  `cs/chr_ins.rs` (`apply_speffect`/`remove_speffect`, `special_effect`, `load_state`),
  `cs/sp_effect.rs` (`SpecialEffect::entries`, `removal_timer`/`duration`/`param_id`),
  `cs/chr_ins/module/data.rs` (`hp`/`max_hp`), `cs/chr_ins/module.rs` (`data` module),
  `cs/world_chr_man.rs` (`main_player`), `cs/event_flag.rs` (`CSEventFlagMan` get/set),
  `cs/solo_param_repository.rs` (`get_mut`/`rows_mut`), `cs/menu_man.rs` (RVA call pattern,
  `STATUS_MESSAGE_YOU_DIED`), `param/generated.rs` (`SP_EFFECT_PARAM_ST`, INDEX 86), and
  `rva/{bundle,rva_ww,rva_jp}.rs` (the two SpEffect RVAs).
- [ERSC docs — Seamless Modding](https://ersc-docs.github.io/seamless-modding/) and
  [Workarounds](https://ersc-docs.github.io/workarounds/) — "Debuffs (Rot Essence) … cured when you
  sit at a bonfire" (the only first-party behavioral description; no IDs published).
- [Souls Modding Wiki — SpEffectParam](http://soulsmodding.com/doku.php?id=bb-refmat%3Aparam%3Aspeffectparam)
  — field semantics: `max*Rate`, `staminaRecoverChangeSpeed`, `physicsDiffenceRate`,
  `physicsAttackRate`, `haveSoulRate`, `itemDropRate` (needs `stateInfo` 66), `effectEndurance`,
  `saveCategory`.
- [Souls Modding Wiki — Event Flags (ER)](https://soulsmodding.com/doku.php?id=er-refmat%3Aevent-flag-list)
  and [EMEVD intro](http://soulsmodding.wikidot.com/tutorial:intro-to-elden-ring-emevd) — rest-at-grace
  reinitializes loaded event scripts; flag-driven detection.
- [Nordgaren/Elden-Ring-Debug-Tool](https://github.com/Nordgaren/Elden-Ring-Debug-Tool) — grace /
  last-grace management + event-flag set/unset tabs (the rig tool for finding the grace flag).
- [Steam — regulation editing guide](https://steamcommunity.com/sharedfiles/filedetails/?id=3279872316)
  and [soulsmods/Paramdex](https://github.com/soulsmods/Paramdex) — SpEffectParam row references for
  picking a free ID block.
</content>
</invoke>
