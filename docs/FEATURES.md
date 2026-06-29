# Feature inventory

What the upstream Seamless Co-op mod does, to scope and prioritize the rewrite. Built from
first-party sources only: the shipped `ersc_settings.ini`, the locale key set in
`english.json`, and a static triage of `ersc.dll` (libraries/sections/exports — **not** its
code, which is virtualized; see [DEVELOPMENT.md](DEVELOPMENT.md) > "Reverse-engineering ERSC").
Behavior below is **inferred** from those names; confirm each by observation before relying on
it.

Difficulty legend (rough, from our side of the rewrite):
- **E (Easy)** — a `regulation`/runtime param or a typed `fromsoftware-rs` field; little or no RE.
- **M (Medium)** — hook a game function or sync some state each frame; SDK helps but needs work.
- **H (Hard)** — the networking/session substrate, or deep game-systems hooks. Real RE.

## Core infrastructure (the substrate — mostly H)

| Feature | What it does | Diff | Notes |
|---|---|---|---|
| Session / networking layer | Replaces vanilla matchmaking so partners share one persistent world. Confirmed deps: `steam_api64` (Steam P2P), `ws2_32` (Winsock), `crypt32`/`wldap32`/`normaliz` (TLS/crypto). | **H; out-of-band stack shipped** | The 80%. **Legally most sensitive** (wire protocol) — reimplement from observed behavior, and only pursue ERSC-session interop deliberately. The **out-of-band connection stack (rungs 1/2/4)** is shipped and **confirmed live across two machines** (2026-06-27 friend test: side-channel linked, versions matched). What remains is **rung 3** — driving the game's own `CSSessionManager` FSM so players see each other in-world — with the **root cause now confirmed (2026-06-29, in-world rig):** a *solo* create passes every static gate (leg A, rejects #1–#3, the 4th gate) then dies in leg B's **tail capacity check** — the session-slot array is **capacity 0** offline (`[NetworkSession+0x20]cap=0`), since no real match/lobby allocated it, so the finished session has nowhere to land. The unblock is a **2-player drive** (a live rung-4 lobby + a real peer is what sizes the slot array). See [COOP-CONNECTION.md](COOP-CONNECTION.md) and [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Leg B charted". |
| Load mechanism | `ersc.dll` exports `modengine_ext_init` → it's a **ModEngine2 extension**; also ships `ersc_launcher.exe` (exe-swap). | **built; rig-confirmed** | We diverge: we own it. The cdylib ships as the game's `dinput8.dll` proxy (auto-loaded; also loads `mods/`), and our `start_protected_game.exe` launcher starts the game outside EAC with a marker the DLL requires. No ModEngine2/EML. Export-table verified statically; **live load confirmed on the rig** — the DLL loads and its features fire in-game across many cycles (scaling applies, nameplates project, the friend test linked). |
| Player state sync | Positions, animation/HP/SP, equipment, events across the session. | **H** | Implied by co-op; the second-hardest piece after transport. |
| Session player limit | Raise the vanilla cap (4 open world / 6 arena) so more friends fit one session. | **built; write rig-confirmed** | `coop/features/session_limit.rs` writes `CSSessionManager.session_player_limit_override` from `[session] max_players` (host-tested clamp 2..=6, default 6). Solo rig run confirms the write lands (observer logs `override=6`); the **>4-player effect** still needs a real party. |
| Seamless roaming | Explore the whole map together instead of being tethered to the host's multiplay area (the defining "seamless" behavior). | **built; write is session-gated (2-player)** | `coop/features/seamless.rs` holds `CSStayInMultiplayAreaWarpData.disable_multiplay_restriction` to host-enforced `gameplay.roam_anywhere` (default on). **Solo rig run (2026-06-29):** the feature registers + ticks (`seamless-roam = ok`) but the warp-data `OwnedPtr` is **unwired without a live session**, so `session::tether_mut` returns `None` and the write never fires solo — confirming it's **2-player-gated, not solo-verifiable** (reclassified from the earlier "observable solo" assumption). Both the write and the roam effect need a live multiplayer session. |
| Separate co-op saves ([COOP-SAVES.md](COOP-SAVES.md)) | Distinct save extension (`save.file_extension = co2`) so co-op never touches vanilla `.sl2`. | **built; rig-confirmed** | `coop/saves.rs` jmp-back-hooks `kernel32!CreateFileW` and rewrites `.sl2`/`.sl2.bak` → `.<ext>`/`.<ext>.bak` via the host-tested `core::saves`. Installed early in `app::install` (before the title-screen save read), not a `Feature` task; not an SDK field. Install failure is **fatal** (refuse to risk the vanilla save). Solo rig run confirmed read/backup/write all redirect and `.sl2` stays untouched. |
| Offline / non-EAC launch | Runs outside EasyAntiCheat (why it's co-op-safe). | **built; launch path rig-confirmed** | Our `start_protected_game.exe` launcher starts the game directly (no EAC); the DLL aborts if it wasn't launched that way (`coop/guard.rs`). The launch-outside-EAC + marker path is **rig-proven** — the mod runs through our launcher every cycle (and linked two machines outside EAC in the friend test), so the marker is set and the guard does *not* abort. Only the narrow negative case (the guard's abort when the marker is **absent**) is still unexercised on the rig. |

## Session management (menu/hotkey actions — M)

> **Divergence:** ERSC triggers these via in-game items + hotkeys; we drive them through an
> overlay **menu** (`unseamless-core/menu.rs`). The actions below are the verbs; the trigger
> mechanism is ours. See ARCHITECTURE.md > Divergences.

> **Status: UI shipped, apply layer rung-3-gated.** The overlay Actions tab renders the live verbs from
> `menu::action_rows(ctx)` (paired verbs collapsed into one stateful row, inapplicable rows hidden; see
> [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md)). **Open World / Join world / Leave world are wired** to
> the on-demand lobby-discovery connection (rungs 1/2/4; see [COOP-CONNECTION.md](COOP-CONNECTION.md)),
> with a Steam-readiness gate, and **confirmed across two machines** (2026-06-27 friend test linked the
> side-channel). **Lock / Unlock / PvP / PvP teams / Friendly fire are surfaced but still
> inert** ("not wired up yet"; `coop/features/session_actions.rs` toasts a placeholder): rung 3 is the
> apply layer that connects them to real game calls and sources their on/off state from the session FSM.

From `OPTIONSELECT_*` / `YKNX3_*` keys. All ride on the networking layer.

- Open / Join / Lock / Unlock / Leave world
- Session password (`cooppassword`, show-password action)

(ERSC's "Evil session" — invasion-style sessions — is **dropped**; see "Custom content, modes &
original-MP sessions — WON'T DO" below.)

## Combat & PvP toggles (M, gated on session layer)

- Toggle PvP, PvP teams, friendly fire

(Crit co-op is **not** in this group — it's built, host-enforced, and solo-observable, so it's
**not session-gated**; see "Gameplay modifiers" below.)

## Per-player scaling ([SCALING.md](SCALING.md)) — **built; live, rig-verified**

`[SCALING]`, applied per connected player:

- Enemy health / damage / posture · Boss health / damage / posture

Mechanism (resolved + shipped in `coop/features/scaling.rs`): overwrite the `SpEffectParam` rate rows
referenced by `MultiPlayCorrectionParam`, once at load (idempotent — set absolute rates, never per-frame
`NpcParam.hp`). Enemy/boss split is free via `NpcParam.multi_play_correction_param_id` (no boss flag);
the boss-vs-normal classification falls out of which SpEffect *family* a row references. Player count
from `CSSessionManager.players`. The concrete row/SpEffect-ID map is **resolved** — `CORRECTION_ROW_CLASSES`
holds all 38 correction rows (classified from the rig param dump), and the rate fields are **rig-verified**
(`max_hp_rate`/`*_attack_power_rate` direct; posture inverted into `sa_receive_damage_rate`; `1.0` == vanilla;
boss +1 → `max_hp` 2.0). A live rig run logs `scaling applied to 24 SpEffect row(s)`; an in-world HP/posture
confirmation is the one remaining rig-TODO. See [SCALING.md](SCALING.md).

## Gameplay modifiers (E–M)

| Feature | Config | Diff |
|---|---|---|
| Crit co-op | `crit_coop` (on by default) | **built; not session-gated** — `coop/features/crit_coop.rs` clears the crit-invuln flag on open-field enemies every frame so a co-op partner can damage them during a riposte/backstab/guard-counter crit window (instead of only the player who landed it). Host-enforced and **solo-observable**: the flag clear runs *regardless of session*, so it is not gated on the session layer. |
| Death debuffs (Rot Essence SpEffects, cured at grace) ([DEATH-DEBUFFS.md](DEATH-DEBUFFS.md)) | `death_debuffs` | **built** — `coop/features/death_debuffs.rs` advances the host-tested stacking model on the debounced death edge and applies each tier's `SpEffectParam` row; the grace-rest flag (9000, rig-confirmed) clears the stack. Tier rows `7210..7250`, both rig blanks resolved. **Data path rig-confirmed (2026-06-29):** the startup probe shows all five tier rows defined with correct `SpEffectRates` (Emaciation/Hopelessness/Decay/Vulnerability/Despair), `rows_defined=true`, feature healthy. Remaining: an actual death to confirm the debuff lands / clears at grace (needs gameplay; not solo-automatable). |
| Spirit summons allowed in MP | `allow_summons` | **scaffold — apply RE-pending** — `coop/features/summons.rs` wires the host-enforced toggle (config + `SharedSettings` sync + settings registry + overlay toggle all already done) and latch-logs its enable/disable edge, but performs **no game write yet**: the open-world "a phantom is in your world ⇒ no Spirit Ash" gate is **not** a charted SDK field (the only `spirit_ashes_allowed` is PvP-quickmatch-only; `SummonBuddyManager` models the summon mechanic but has no MP-allow lever), so the gate needs **dynamic** RE. Double-blocked: both locating the gate *and* verifying it need a live "phantom present" state, i.e. working rung-3 co-op. The `apply_gate` seam + the rig-probe recipe (HW-watchpoint `SummonBuddyManager.request_summon_speffect_id`, offset `0x20`, solo vs. phantom-present) are documented in the module. |
| Skip splash screens ([SKIP-INTROS.md](SKIP-INTROS.md)) | `skip_splash_screens` | M |
| Spectate-on-death system ([SPECTATE.md](SPECTATE.md)) | `always_spectate_on_death` | **camera half built; rig-gated** — `coop/features/spectate.rs` debounces the local death (`ChrInsFlags1c5::death_flag`), picks a living partner from `player_chr_set` via the host-tested `select_target` (sticky), and aims the game's own death camera (`WorldChrMan.chr_cam` → `ChrCam::death_cam_target` + `camera_type=DeathCam`) at them, releasing on revive. Local (per-player) setting, off by default. **Rig-gated:** whether that write actually pans the view to the partner, and the deeper **respawn-suppression** half (not sent back to grace) — see SPECTATE.md > Rig asks. |
| Boot master volume | `boot_master_volume_enabled` + `boot_master_volume` | **built** — `coop/features/boot_volume.rs` writes `GameDataMan::game_settings.master_volume` (0..=10, charted) once when the singleton is live, then leaves the in-game slider free. Opt-in via `boot_master_volume_enabled` (off by default). **Write rig-confirmed (2026-06-29):** sets `master_volume` 10→5 at boot, re-asserts across main-menu entry, then closes the re-assert window (in-game slider left free) — full lifecycle in the log. Whether the audio engine *audibly* applies it needs a human ear. |
| Lock time of day (permanent day/night) | `world_time.{lock,hour,minute}` | **built** — `coop/features/world_time.rs` re-asserts `WorldAreaTime::request_time` each frame (charted). Menu-adjustable. Host-enforced (synced across the party via `SharedSettings`). |
| Overhead nameplates (per-player marker) ([NAMEPLATES.md](NAMEPLATES.md)) | `[nameplates] enabled` (was ERSC `overhead_player_display`) | **shipped — native colored dot, on by default** (`[nameplates] enabled`). We diverge from ERSC's text/stat labels: nameplates are a per-player colored **disc** drawn by the game's own `CSEzDraw` renderer (world-space, depth-tested, present-hook-free — `coop/features/native_nameplates.rs` + `native_draw::draw_billboard_disc`), over each player and your own head (so it's verifiable solo, no LOD by design). The earlier imgui projected-label nameplates + their projection/text-content core modules were removed (the dot is the one nameplate surface). **One follow-up:** color-by-SteamID — the dot color is keyed off the phantom pointer today; swap it for the SteamID once the session core maps a phantom→identity (rung-3-gated). |

## Custom content, modes & original-MP sessions — WON'T DO

Dropped (decided 2026-06): unseamless-coop is a **co-op-only** reimplementation targeting core
co-op gameplay. No original PvP/invasion modes and no bolted-on game modes:

- **"Evil session"** (`OPTIONSELECT_*`: start / seek / view / leave) — invasion-style /
  original-multiplayer sessions, out of scope for a co-op-only mod.
- **Enemy rush:** easy / med / hard / infinite
- **Boss rush** (+ base-DLC variants); arena waves (`YKBR2_*` battle start/wave/end)
- **Custom mod goods** (`MODGOODS_*`): hosting / joining / leaving / game-rule-change / rune-arc
  / Rot items ×5 — custom inventory items driving session actions. (Also superseded by the overlay
  menu; ARCHITECTURE.md > Divergences.) Only the *item triggers* are dropped here, including the
  rune-arc one; the rune-arc *sharing feature* itself survives (below).

Catalogued here for reference only. **Rune arc sharing** (the feature, not its MODGOODS item) is
*not* dropped — it rides on the session layer; see [ROADMAP.md](ROADMAP.md).

## UI / locale (M)

- Custom locale system (`mod_language_override`, the `english.json` FMG/menu text injection)
- Overhead display rendering, on-screen status/notification text (`YKNX3_*`, `FE_*`) — renderer is
  the hudhook DX12 overlay ([OVERLAY-RENDERING.md](OVERLAY-RENDERING.md)); simple notifications can
  use the native `CSMenuManImp::display_status_message` instead.

## Title screen / offline presentation — WON'T DO

ERSC hides the vanilla "you're offline" network-error popups at boot and rebrands the bottom-right
version/`OFFLINE` watermark as `Seamless Co-op X.Y.Z`. **We won't do either** (decided 2026-06):

- **Popup suppression** — the trigger is behind a non-standard Arxan code-restoration guard the SDK's
  scanner doesn't neutralize; not worth the RE for a cosmetic title-screen popup (ERSC doesn't
  suppress it either).
- **Watermark restyle** — superseded by our own overlay watermark (`coop/overlay.rs` `draw_watermark`),
  which needs no FMG mutation or SDK-pin bump.

Full RE record (addresses, the Arxan wall, FMG IDs) is kept in
[`OFFLINE-TITLE-SCREEN.md`](OFFLINE-TITLE-SCREEN.md) so it's never re-derived from scratch.

## Suggested milestone ordering

Full parity is large. A sane path that front-loads the genuinely hard part:

1. **M0 — harness (done):** DLL loads, frame task fires, logs. ✅
2. **M1 — easy wins, no networking:** the **E** items (scaling params, summons, splash skip,
   volume). Proves the SDK-write loop end-to-end and is independently useful/testable. ✅ scaling is
   live + rig-verified; the SDK-write loop is proven end-to-end.
3. **M2 — transport spike:** two instances exchange a heartbeat over Steam P2P. The make-or-break
   feasibility test for the whole project; do this before over-investing elsewhere. ✅ the rung-2
   side-channel runs the full handshake/config-sync/liveness over Steam P2P and **linked two machines
   in the 2026-06-27 friend test**.
4. **M3 — minimal shared session:** two players, position + basic state sync, one shared world.
   "First playable." ← current frontier (rung 3; the in-world session FSM, blocked in leg B's session registry/init chain — likely needs a real peer).
5. **M4+ — layer on:** session actions, PvP toggles, debuffs. (Original-MP modes — boss/enemy
   rush, arena waves, custom goods — and evil sessions are **out of scope**; see "Custom content,
   modes & original-MP sessions — WON'T DO".)

> Reality check from the triage: ERSC's binary is Themida-virtualized, so we reimplement from
> **observed behavior + the public `fromsoftware-rs` SDK + the ER modding community's
> knowledge**, not from its code. That suits the clean-room posture (you can't copy what you
> can't read) but means M2/M3 lean on experimentation, not on reading theirs.
