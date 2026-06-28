# fromsoftware-rs SDK coverage

What the `eldenring` SDK (pinned commit `8c67a84`) already exposes, mapped to what the rewrite
needs. This is the inventory behind the "drive the game's own systems, not reinvent them"
decision in [ARCHITECTURE.md](ARCHITECTURE.md). Verdicts: **CHARTED** = a real API we can call;
**PARTIAL** = struct layout we can read but no action method; **ABSENT** = needs our own RE.

Singletons are reached via `fromsoftware_shared::FromStatic` (`X::instance()` / `instance_mut()`,
`unsafe`, main-thread only — wrap with `coop::sdk::with_instance`). Source paths below are under
`crates/eldenring/src/` in the pinned checkout.

| Area | Key types / API | Verdict |
|---|---|---|
| **Task system** | `CSTaskImp` (`cs/task.rs`): `wait_for_instance`, `run_recurring(fn, CSTaskGroupIndex)`. 168 phases incl. `FrameBegin`, `WorldChrMan_PostPhysics`, `TaskLineIdx_FrpgNet_*`, `NetFlushSendData`. | **CHARTED** ⭐ |
| **Params / scaling** | `SoloParamRepository` (`cs/solo_param_repository.rs`): `get_mut::<P>(id)`, `rows_mut`, 194 `SoloParam` types incl. `NpcParam`, `SpEffectParam`, `MultiPlayCorrectionParam`. Runtime read/write of regulation.bin. | **CHARTED** ⭐ |
| **Networking / session** | `CSSessionManager` (`cs/session_manager.rs`): `lobby_state`/`protocol_state` FSMs, `players: DLVector`, `host_player`, `session_player_limit`, AES cipher. `NetworkSessionVmt` (`cs/network_session.rs`): `broadcast_packet`, `receive_packet`, `kick`, `remote_identity`. `CSNetMan`, `net_chr_sync` (inbound HP/placement). | **CHARTED** (vtable-level; pointer buffers) |
| **Characters / players** | `WorldChrMan` (`cs/world_chr_man.rs`): `player_chr_set` (phantoms) vs `open_field_chr_set` (enemies), `main_player`, `chr_ins_by_handle`. `ChrIns` modules. | **CHARTED** |
| **Event flags** | `CSEventFlagMan` (`cs/event_flag.rs`): `get_flag`/`set_flag`. | **CHARTED** |
| **World time** | `WorldAreaTime`: `request_time`. | **CHARTED** |
| **SpEffects** | `SpecialEffect` (`cs/sp_effect.rs`): iterate active effects + timers. `ChrIns::apply_speffect(id, dont_sync)` / `remove_speffect(id)` (`cs/chr_ins.rs`, RVA-backed `0x3e8be0`/`0x3ee0b0`). | **CHARTED** (apply/remove callable — see [DEATH-DEBUFFS.md](DEATH-DEBUFFS.md)) |
| **Summon signs / party** | `SosSignMan`, `PartyMemberInfo`: read sign DB + phantom counts. No create/accept API. | **PARTIAL** |
| **Player game data** | `PlayerGameData` (`cs/player_game_data.rs`): full remote-player stats, read-only. | **PARTIAL** |
| **Menus / HUD** | `CSMenuMan`, `CSFeMan` (HUD state), status-message ID constants. | **PARTIAL** (read state; no "show message" API) |
| **FMG / text** | `MsgRepository`: marker singleton only. | **ABSENT** — and not needed: the only planned consumer (offline-watermark restyle) is **won't-do** (we draw our own overlay watermark instead), so no FMG override is on the roadmap. |
| **Save files** | `cs/file.rs` (asset loader `CSFileImp`/`CSFileRepository`), `cs/game_man.rs` (save *state*: `save_slot`/`save_requested`/`save_state`). No save-path/extension API. | **PARTIAL** (separate-saves is a `CreateFileW` hook, not an SDK field — see [COOP-SAVES.md](COOP-SAVES.md)) |

## What this means for the rewrite

- **Layer 1 (buildable blind):** event flags, summons toggle, world time — all CHARTED params/flags.
  Write against the SDK; verify on the rig. Scaling's *config + math* are host-built and host-tested
  (`unseamless-core/scaling.rs`), and its concrete in-game lever is now **resolved and rig-verified**:
  `coop/features/scaling.rs` overwrites the referenced `SpEffectParam` rate rows for all 38 classified
  correction rows, live on the rig (`scaling applied to … SpEffect row(s)`) — see the resolved-mechanism
  bullet below.
- **Splash/intro skip is NOT a param** (despite the name): the SDK charts no movie-player type or
  skip function (only the `MovieStep` task phase and a `pre_opening_movie_wait_sec` param, neither a
  lever). It's an AOB-scan + NOP of the boot-flow logo gate — see [SKIP-INTROS.md](SKIP-INTROS.md).
  The shared AOB-scan + memory-patch utility it needs is designed in
  [CODE-PATCHING.md](CODE-PATCHING.md) (reuses pelite's scanner + `Program::current`, already in the
  tree via the SDK).
- **Layer 2 (RE-gated, the hard part):** the co-op core rides the **CHARTED** networking — drive
  `CSSessionManager`'s FSM + `NetworkSessionVmt` rather than building transport. The **out-of-band
  connection stack (rungs 1/2/4) is shipped and confirmed across two machines** (2026-06-27 friend test);
  what's left is **rung 3** — the create/join initiation the SDK doesn't chart. That gap is now narrowed
  to **leg B**, the charted network-create dispatch: the encrypted availability gate is bypassed and
  reject #1 (`*(NetworkSession+0x10) == 0`) is isolated (#2/#3 eliminated statically), but forcing it is
  rig-proven **insufficient** — the offline blocker is deeper in leg B's session registry/init chain
  (likely needs a real peer) — see
  [COOP-CONNECTION.md](COOP-CONNECTION.md) and [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Leg B charted". What still needs
  *observing* on the rig is the FSM's post-`Host`/`Client` behavior (which count is "players in my
  world", how sessions persist across map transitions) — see [RIG-RUNBOOK.md](RIG-RUNBOOK.md).
- **Needs internal-function RVAs (not just struct layout):** creating/accepting summon signs,
  showing native on-screen messages. (SpEffect apply/remove is now charted — see
  [DEATH-DEBUFFS.md](DEATH-DEBUFFS.md). FMG text override is **won't-do**, so its RVA isn't needed —
  see [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md).) These are the diagnostic/RE tasks the SDK
  doesn't hand us.
- **Scaling mechanism resolved (see [SCALING.md](SCALING.md)):** at our pin `MultiPlayCorrectionParam`
  is *SpEffect indirection*, not a rate table — it holds `client1/2/3_sp_effect_id` keyed by
  extra-player count, and the real multipliers live in the referenced `SpEffectParam` rate rows
  (`max_hp_rate`, `*_attack_power_rate`, posture rate). So the lever is **editing those `SpEffectParam`
  rate rows once at load** — idempotent, and neither the correction param nor a per-frame `NpcParam.hp`
  write. Enemy vs. boss split comes from `NpcParam.multi_play_correction_param_id` (there's no boss
  flag); `MultiSoulBonusRateParam` is runes-only. The concrete row/SpEffect-ID map is now **resolved and
  rig-verified** (no longer rig-gated): `coop/features/scaling.rs` classifies all 38 correction rows
  (`CORRECTION_ROW_CLASSES`) from the rig param dump and overwrites the referenced rate rows live — the
  SDK charts the param types and the rig dump supplied which IDs map to which player count.

> This table reflects the pinned commit. If the `fromsoftware-rs` rev is bumped, re-verify the
> field/method names — struct layouts are read against a specific revision.
