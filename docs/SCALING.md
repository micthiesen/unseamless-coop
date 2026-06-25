# Per-Player Enemy & Boss Scaling

How the mod makes the host's world tougher as players join, the way ERSC's `[SCALING]` block does:
a configurable percentage of extra enemy/boss health, damage, and posture **per connected player**.
The math is already done and host-tested in [`unseamless-core/scaling.rs`](../crates/unseamless-core/src/scaling.rs);
this note resolves the open question SDK-COVERAGE.md flags — *what game lever applies those
multipliers, and how to do it idempotently* — and specs `features/scaling.rs`.

This is a **research note + mechanism decision**, not yet an implemented feature. Game-internal
claims are grounded in the pinned `fromsoftware-rs` SDK source (cited) or in datamined param
definitions (cited), with inference hedged as such. Per [CLAUDE.md](../CLAUDE.md) > Clean-room
hygiene: ERSC is closed + Themida-packed, so its exact code path is inference; we reimplement from
the *vanilla mechanism* (public params + SDK), which is the honest clean-room substrate anyway.

> The headline result, and it overturns the doc's framing: at our pin, **`MultiPlayCorrectionParam`
> is not a table of HP/damage rates**. It's pure SpEffect *indirection* — three SpEffect-ID columns
> keyed by additional-player count. The actual multipliers live in the **SpEffect rows it points
> at**. So the idempotent lever isn't "edit the correction param" *or* "mutate NpcParam HP" (the two
> options SDK-COVERAGE.md weighed) — it's **edit the referenced SpEffect rate rows once at load.**

## How Vanilla Elden Ring Scales Co-op

When a phantom joins, the host's enemies and bosses get tougher through a small chain of params.
The pieces, and which are documented vs inferred:

1. **Each NPC names a correction row.** `NpcParam` (the per-enemy stat row) has a field
   `multi_play_correction_param_id` — an ID into `MultiPlayCorrectionParam` (or `-1` for "none").
   **Documented**: the field is in the SDK (`NpcParam::multi_play_correction_param_id`, a getter/setter
   pair) and in Paramdex (`マルチプレイ補正パラメータID`). *There is no `isBoss` flag driving scaling* —
   "boss vs normal enemy" is encoded entirely by **which correction row the NPC points at**. Bosses
   reference a row whose SpEffects multiply harder; trash mobs reference a weaker (or no) row. The
   regulation author assigned each NPC its row.

2. **The correction row selects a SpEffect by additional-player count.** At our pin,
   `MULTI_PLAY_CORRECTION_PARAM_ST` contains only:
   `client1_sp_effect_id`, `client2_sp_effect_id`, `client3_sp_effect_id`, a `b_override_sp_effect`
   flag, and a `disable_param_nt` bit. **Documented** (SDK struct read directly, and Paramdex
   DisplayNames: client1 = "1 cooperating client", client2 = "2 clients", client3 = "3 clients").
   So column N = "N extra players present" — exactly the 3 columns for the vanilla 4-player cap. The
   game picks the column from the connected-player count and applies that SpEffect to the enemy.

   > This is a deliberate ER-era redesign. In **Dark Souls 3** the same-named param had *direct*
   > integer fields (`correctionVal0/1`); ER replaced them with SpEffect indirection. That history
   > is the strongest single confirmation that ER does **not** carry HP rates on the correction param.

3. **The SpEffect carries the actual multipliers.** `SP_EFFECT_PARAM_ST` rows hold multiplicative
   *rate* fields, all defaulting to `1.0` (no change). **Documented** (SDK struct + Paramdex/libER
   field briefs). The ones that map onto ERSC's six knobs:

   | ERSC knob | SpEffect rate field(s) (`SP_EFFECT_PARAM_ST`) |
   |---|---|
   | health | `max_hp_rate` |
   | damage (enemy *deals*) | `physics_attack_power_rate`, `magic_attack_power_rate`, `fire_attack_power_rate`, `thunder_attack_power_rate` (+ dark) |
   | posture | `toughness_damage_cut_rate` and/or `sa_receive_damage_rate` (poise/stance) — *exact field to confirm on rig* |

   (Defense rates — `physics_diffence_rate` etc. — exist too but aren't an ERSC knob.)

4. **Runes scale separately.** `MultiSoulBonusRateParam` (`MULTI_SOUL_BONUS_RATE_PARAM_ST`) is a
   different mechanism: per-role float multipliers (`host`, `white_ghost_*`, `black_ghost_*`, …) for
   the **rune reward**, not HP. **Documented** (SDK + Paramdex). It is *not* a scaling lever for us —
   noted only to retire it from the "which param?" question. It governs reward souls, not difficulty.

So the vanilla data flow is: **player count → correction column → SpEffect → rate fields on the
enemy**. Numbers community-datamined for vanilla co-op (illusory wall): ~+60% boss HP with one
summon, +130% with two — consistent with `client1` SpEffect ≈ `max_hp_rate` 1.6, `client2` ≈ 2.3.

## What This Means for Our Mechanism Decision

SDK-COVERAGE.md framed the choice as **`MultiPlayCorrectionParam` (idempotent) vs mutating
`NpcParam.hp` per-frame (compounds, wrong)**. With the struct read, the real shape is:

- **Editing `MultiPlayCorrectionParam` directly is a non-starter** for *custom percentages* — there
  are no rate fields there to edit. You could only *repoint* its SpEffect IDs, which doesn't change
  magnitudes.
- **Mutating `NpcParam.hp` per-frame is still wrong** (compounds; also you'd have to special-case
  every enemy, fight the game's own spawn-time application, and there's no clean per-enemy "is this a
  boss" signal beyond the correction-row id it already carries).
- **The right lever is the third option, which the doc didn't enumerate: overwrite the SpEffect
  *rate rows* that the correction params reference, once at load.** This is idempotent (a rate is a
  multiplier the game applies against base stats at effect-application time, so re-writing the same
  value is a no-op), it reuses the game's own scaling pipeline (the game still picks column by player
  count), and the **enemy/boss split falls out for free** — bosses and trash already point at
  different correction rows → different SpEffects → we write different multipliers into the
  boss-row SpEffects vs the enemy-row SpEffects.

This is also the most plausible thing ERSC does (closed-source, so **inference**): it's consistent
with ERSC's documented "changes to `regulation.bin` won't break Seamless Co-op" (it edits params in
memory, doesn't ship a regulation), and with its separate enemy/boss knobs mapping onto separate
correction rows.

### The math is already ours; this is just the binding

[`unseamless-core/scaling.rs`](../crates/unseamless-core/src/scaling.rs) already turns
`(per_player_percent, player_count)` into a `StatMultipliers { health, damage, posture }` for
enemies and bosses separately, with `solo → identity`, host-tested. The cdylib feature's job is
narrow: read the player count, ask core for the multipliers, and **write them into SpEffect rate
fields** — float-rate writes, not the `scale_i32` path (that helper was written for the rejected
"scale NpcParam HP integer" approach and isn't used by the SpEffect mechanism).

## How `features/scaling.rs` Should Be Built

Pattern follows [`features/session_limit.rs`](../crates/unseamless-coop/src/features/session_limit.rs):
a `Feature` that self-heals by re-asserting its target, with the actual values computed in core.

**When:** *not* per-frame hot-path math. Recompute the multipliers only when the **player count
changes** (cache last count); re-assert the SpEffect rows on that edge and periodically (cheap
self-heal in case the game reloads params on a map/regulation event). Default phase
(`FrameBegin`) is fine — this is param data, not frame-ordered live state. Writing before a session
forms is harmless; the rates are read when the SpEffect is applied to a spawned enemy.

**What to write (the load-time shape):**

1. **Resolve the target SpEffect rows.** This is the one piece that needs the rig: enumerate
   `MultiPlayCorrectionParam` rows (`SoloParamRepository::rows::<MultiPlayCorrectionParam>`), read
   their `client1/2/3_sp_effect_id`s, and learn which rows are "boss" vs "enemy" and which SpEffect
   IDs they reference. Two viable strategies, decide after the rig dump:
   - **(a) Edit the referenced SpEffect rows in place.** For each correction row, for each populated
     `clientN_sp_effect_id`, fetch that `SpEffectParam` row
     (`SoloParamRepository::get_mut::<SpEffectParam>(id)`) and set `max_hp_rate` /
     `*_attack_power_rate` / the posture-rate field to the core multiplier *for that player count*
     (`clientN` ⇒ `player_count = N+1`). Idempotent: we compute the absolute rate from
     `unseamless-core`, not by multiplying the current value.
   - **(b) Mint our own SpEffect rows and repoint the correction columns** (`set_clientN_sp_effect_id`
     to our IDs). Cleaner separation (we never stomp vanilla rows, restore is trivial), but needs
     free SpEffect IDs and a way to create rows — heavier. Prefer (a) unless (a) clashes with the
     area/NG+ scaling that *also* rides SpEffects.

2. **Store originals for restore.** Unlike `session_limit` (a single field that self-heals), we're
   editing shared regulation rows; keep the pre-edit rate values so we can restore on teardown / when
   scaling is disabled, mirroring the `Drop`-restores-bytes pattern erfps2 uses for FMG edits.

**Reading the player count (CHARTED):** `CSSessionManager.players` is a `DLVector` (derefs to a
slice), so `players.len()` gives the connected count; `host_player` is separate. Read via
`coop::sdk::with_instance::<CSSessionManager, _>`. Solo / no session ⇒ treat as 1 player ⇒ core
returns identity ⇒ vanilla, which is the correct degenerate behavior. **Confirm on the rig** that
`players.len()` is "everyone in my world including/excluding me" so the off-by-one matches core's
"total connected count" contract (core's `multiplier` does `players - 1` for the extra-player count).

**Avoiding compounding:** never read-modify-write a rate. Always compute the absolute target rate
from `unseamless-core` for the current player count and *set* it. Re-running the feature with the
same count writes the same value (no-op); a count change overwrites cleanly. This is the whole reason
the SpEffect-rate lever is correct where per-frame `NpcParam.hp` mutation is not.

## Caveats

- **The SDK can read/write all of this blind; the *row map* needs the rig.** `SoloParamRepository`
  (`get_mut`, `rows_mut`), `MultiPlayCorrectionParam`, `SpEffectParam`, `NpcParam`, and
  `CSSessionManager.players` are all CHARTED at our pin. What's *not* knowable from the Mac: which
  concrete correction-row IDs and SpEffect IDs are "boss" vs "normal", and whether ER's
  multiplayer-correction SpEffects collide with the area/NG+ scaling SpEffects (ranges 7000–7200 /
  7400+ per Paramdex) — i.e. whether editing them in place would also perturb single-player NG+
  scaling. That's a regulation dump (Smithbox/Paramdex on the rig).
- **Posture field is unconfirmed.** `toughness_damage_cut_rate` vs `sa_receive_damage_rate` (both
  exist on `SP_EFFECT_PARAM_ST`) — pick by experiment on the rig; ERSC's "posture" knob is one of
  these.
- **RVA / version pin.** No RVA-backed call is needed here (pure param read/write), so the
  ER 2.6.2.0/2.6.2.1 RVA-bundle constraint doesn't gate this feature — but the *param layouts* are
  read against pin `8c67a84`; re-verify field names if the SDK pin moves.
- **`scale_i32` in core is now vestigial.** It was built for the rejected NpcParam-HP-integer path.
  The SpEffect mechanism only needs the `f32` `StatMultipliers`. Leave it (harmless, tested) or prune
  when wiring the feature; don't build the binding around it.

## Status / Next Steps

- [ ] **Rig: dump the param map.** Enumerate `MultiPlayCorrectionParam` rows + their
      `client1/2/3_sp_effect_id`s; identify which rows bosses vs normal enemies use (cross-ref
      `NpcParam.multi_play_correction_param_id` for known boss NpcParam IDs). Record the SpEffect IDs.
- [ ] **Rig: confirm the rate fields.** Verify `max_hp_rate` drives HP, an `*_attack_power_rate` set
      drives damage, and which field is "posture"; confirm `1.0` = vanilla.
- [ ] **Rig: confirm player-count semantics** of `CSSessionManager.players.len()` vs core's
      "total connected" contract (off-by-one check against `host_player`).
- [ ] **Decide edit-in-place (a) vs mint-and-repoint (b)** based on whether multiplayer SpEffects
      overlap the area/NG+ scaling SpEffects.
- [ ] **Implement `features/scaling.rs`:** read count → `Scaling::{enemy,boss}_multipliers` →
      write SpEffect rates on the resolved rows; cache count, re-assert on change + self-heal; store
      originals for restore. No per-frame math, no read-modify-write.
- [ ] **Reclassify** in SDK-COVERAGE.md / FEATURES.md: scaling lever = **SpEffect rate rows behind
      `MultiPlayCorrectionParam`**, not the correction param itself and not `NpcParam.hp`.

## Sources

- Pinned SDK `fromsoftware-rs` rev `8c67a84` (read directly):
  `crates/eldenring/src/param/generated.rs` (`MULTI_PLAY_CORRECTION_PARAM_ST`,
  `MULTI_SOUL_BONUS_RATE_PARAM_ST`, `SP_EFFECT_PARAM_ST`, `NPC_PARAM_ST` incl.
  `multi_play_correction_param_id`, `FINAL_DAMAGE_RATE_PARAM_ST`);
  `crates/eldenring/src/cs/solo_param_repository.rs` (`get_mut`, `rows_mut`, `SoloParam` table);
  `crates/eldenring/src/cs/session_manager.rs` (`players: DLVector`, `host_player`,
  `session_player_limit`).
- [soulsmods/Paramdex](https://github.com/soulsmods/Paramdex) — `ER/Defs/MultiPlayCorrectionParam.xml`,
  `MultiSoulBonusRateParam.xml`, `SpEffect.xml`, `NpcParam.xml`; `ER/Names/SpEffectParam.txt`
  (area-scaling 7000–7200, NG+ 7400+).
- [Dasaav-dsv/libER](https://github.com/Dasaav-dsv/libER) — English-annotated paramdef headers for
  `MULTI_PLAY_CORRECTION_PARAM_ST` and `SP_EFFECT_PARAM_ST` (multiplicative rate fields, default 1.0).
- [veeenu/eldenring-practice-tool](https://github.com/veeenu/eldenring-practice-tool) — typed param
  structs (`multi_play_correction_param_id` on NpcParam); and
  [veeenu/darksoulsiii-practice-tool](https://github.com/veeenu/darksoulsiii-practice-tool) — DS3's
  `MultiPlayCorrectionParam { correction_val0, correction_val1 }`, showing the ER redesign.
- [ERSC settings docs](https://ersc-docs.github.io/seamless-modding/) — the six scaling knobs +
  defaults (enemy_health 35, boss_health 100, etc.); [ERSC FAQ](https://ersc-docs.github.io/faq/)
  ("changes to regulation.bin won't break Seamless Co-op").
- [LukeYui/EldenRingSeamlessCoopRelease](https://github.com/LukeYui/EldenRingSeamlessCoopRelease)
  — release-artifacts only (no source); ERSC's exact mechanism is inference.
- [illusory wall](https://x.com/illusorywall/status/1514677640455761929) — datamined vanilla co-op
  boss HP scaling (+60% / +130% with 1 / 2 summons).
- Difficulty mods using the edit-`max_hp_rate`-once pattern, e.g.
  [Nexus mods/2570](https://www.nexusmods.com/eldenring/mods/2570) — corroborates idempotent
  SpEffect-rate edits.
- Readable-for-mechanism (not copied): [ClayAmore/ER-Save-Editor](https://github.com/ClayAmore/ER-Save-Editor)
  (Apache-2.0 param structs).
