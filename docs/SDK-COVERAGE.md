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
| **SpEffects** | `SpecialEffect` (`cs/sp_effect.rs`): iterate active effects + timers. **No apply/remove** method. | **PARTIAL** (needs an internal fn RVA to apply) |
| **Summon signs / party** | `SosSignMan`, `PartyMemberInfo`: read sign DB + phantom counts. No create/accept API. | **PARTIAL** |
| **Player game data** | `PlayerGameData` (`cs/player_game_data.rs`): full remote-player stats, read-only. | **PARTIAL** |
| **Menus / HUD** | `CSMenuMan`, `CSFeMan` (HUD state), status-message ID constants. | **PARTIAL** (read state; no "show message" API) |
| **FMG / text** | `MsgRepository`: marker singleton only. | **ABSENT** (FMG override needs RE) |
| **Save files** | `cs/file.rs`: layout. | **PARTIAL** |

## What this means for the rewrite

- **Layer 1 (buildable blind):** scaling, event flags, summons toggle, splash skip, world time —
  all CHARTED params/flags. Write against the SDK; verify on the rig.
- **Layer 2 (RE-gated, the hard part):** the co-op core rides the **CHARTED** networking — drive
  `CSSessionManager`'s FSM + `NetworkSessionVmt` rather than building transport. What's left to
  *observe* on the rig is the FSM's behavior (which count is "players in my world", how sessions
  persist across map transitions) and where ERSC relaxes the limits — see [RIG-RUNBOOK.md](RIG-RUNBOOK.md).
- **Needs internal-function RVAs (not just struct layout):** applying SpEffects (death debuffs),
  creating/accepting summon signs, showing native on-screen messages, overriding FMG text. These
  are the diagnostic/RE tasks that the SDK doesn't hand us.
- **Scaling mechanism is a known open question:** `MultiPlayCorrectionParam` / `MultiSoulBonusRateParam`
  exist and are the *likely-correct, idempotent* lever (vs. mutating `NpcParam` HP per-frame, which
  would compound). Confirm on the rig before wiring `features/scaling.rs`.

> This table reflects the pinned commit. If the `fromsoftware-rs` rev is bumped, re-verify the
> field/method names — struct layouts are read against a specific revision.
