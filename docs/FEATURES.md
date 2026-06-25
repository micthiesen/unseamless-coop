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
| Session / networking layer | Replaces vanilla matchmaking so partners share one persistent world. Confirmed deps: `steam_api64` (Steam P2P), `ws2_32` (Winsock), `crypt32`/`wldap32`/`normaliz` (TLS/crypto). | **H** | The 80%. **Legally most sensitive** (wire protocol) — reimplement from observed behavior, and only pursue ERSC-session interop deliberately. |
| Load mechanism | `ersc.dll` exports `modengine_ext_init` → it's a **ModEngine2 extension**; also ships `ersc_launcher.exe` (exe-swap). | **built, rig-gated** | We diverge: we own it. The cdylib ships as the game's `dinput8.dll` proxy (auto-loaded; also loads `mods/`), and our `start_protected_game.exe` launcher starts the game outside EAC with a marker the DLL requires. No ModEngine2/EML. Export-table verified on the Mac; live load unproven on the rig. |
| Player state sync | Positions, animation/HP/SP, equipment, events across the session. | **H** | Implied by co-op; the second-hardest piece after transport. |
| Session player limit | Raise the vanilla cap (4 open world / 6 arena) so more friends fit one session. | **built; write rig-confirmed** | `coop/features/session_limit.rs` writes `CSSessionManager.session_player_limit_override` from `[session] max_players` (host-tested clamp 2..=6, default 6). Solo rig run confirms the write lands (observer logs `override=6`); the **>4-player effect** still needs a real party. |
| Seamless roaming | Explore the whole map together instead of being tethered to the host's multiplay area (the defining "seamless" behavior). | **built; write not yet rig-confirmed** | `coop/features/seamless.rs` holds `CSStayInMultiplayAreaWarpData.disable_multiplay_restriction` to host-enforced `gameplay.roam_anywhere` (default on). The roam effect needs a live session; the held write is observable solo via the session observer once a session manager is live. |
| Separate co-op saves ([COOP-SAVES.md](COOP-SAVES.md)) | Distinct save extension (`save.file_extension = co2`) so co-op never touches vanilla `.sl2`. | **built; rig-confirmed** | `coop/saves.rs` jmp-back-hooks `kernel32!CreateFileW` and rewrites `.sl2`/`.sl2.bak` → `.<ext>`/`.<ext>.bak` via the host-tested `core::saves`. Installed early in `app::install` (before the title-screen save read), not a `Feature` task; not an SDK field. Install failure is **fatal** (refuse to risk the vanilla save). Solo rig run confirmed read/backup/write all redirect and `.sl2` stays untouched. |
| Offline / non-EAC launch | Runs outside EasyAntiCheat (why it's co-op-safe). | **built, rig-gated** | Our `start_protected_game.exe` launcher starts the game directly (no EAC); the DLL aborts if it wasn't launched that way (`coop/guard.rs`). Logic written; the EAC bypass + abort behavior are **not yet validated on the rig**. |

## Session management (menu/hotkey actions — M)

> **Divergence:** ERSC triggers these via in-game items + hotkeys; we drive them through an
> overlay **menu** (`unseamless-core/menu.rs`). The actions below are the verbs; the trigger
> mechanism is ours. See ARCHITECTURE.md > Divergences.

From `OPTIONSELECT_*` / `YKNX3_*` keys. All ride on the networking layer.

- Open / Join / Lock / Unlock / Leave world
- Break-in (invasion-style join), rapid re-entry break-in, call break-in SOS, cancel break-in
- Session password (`cooppassword`, show-password action)
- "Evil session": start / seek / view / leave (invasion-style sessions)

## Combat & PvP toggles (M, gated on session layer)

- Toggle PvP, PvP teams, friendly fire
- Dried finger toggle (more concurrent players/invaders), `allow_invaders`

## Per-player scaling ([SCALING.md](SCALING.md))

`[SCALING]`, applied per connected player:

- Enemy health / damage / posture · Boss health / damage / posture

Mechanism (resolved): edit the `SpEffectParam` rate rows referenced by `MultiPlayCorrectionParam`,
once at load (idempotent — set absolute rates, never per-frame `NpcParam.hp`). Enemy/boss split is
free via `NpcParam.multi_play_correction_param_id` (no boss flag). Player count from
`CSSessionManager.players`. The concrete row/SpEffect-ID map is rig-gated. See [SCALING.md](SCALING.md).

## Gameplay modifiers (E–M)

| Feature | Config | Diff |
|---|---|---|
| Death debuffs (Rot Essence SpEffects, cured at grace) ([DEATH-DEBUFFS.md](DEATH-DEBUFFS.md)) | `death_debuffs` | E–M |
| Spirit summons allowed in MP | `allow_summons` | E |
| Give ember | (action) | M |
| Skip splash screens ([SKIP-INTROS.md](SKIP-INTROS.md)) | `skip_splash_screens` | M |
| Spectate-on-death system | `always_spectate_on_death` | M |
| Boot master volume | `default_boot_master_volume` | E |
| Overhead player display (ping / soul level / death count / Steam ID) | `overhead_player_display`, `append_steam_id_to_players` | M |

## Custom content & modes (M)

- **Enemy rush:** easy / med / hard / infinite
- **Boss rush** (+ base-DLC variants); arena waves (`YKBR2_*` battle start/wave/end)
- **Custom mod goods** (`MODGOODS_*`): hosting / joining / break-in / leaving / game-rule-change
  / rune-arc / Rot items ×5 / dried-finger items — custom inventory items driving session actions.
  **Divergence: not a build target.** These exist in ERSC only because items were its easiest
  action trigger; we use the overlay menu instead (ARCHITECTURE.md > Divergences). Catalogued
  here for reference only.
- Rune arc sharing

## UI / locale (M)

- Custom locale system (`mod_language_override`, the `english.json` FMG/menu text injection)
- Overhead display rendering, on-screen status/notification text (`YKNX3_*`, `FE_*`) — renderer is
  the hudhook DX12 overlay ([OVERLAY-RENDERING.md](OVERLAY-RENDERING.md)); simple notifications can
  use the native `CSMenuManImp::display_status_message` instead.

## Title screen / offline presentation (M)

ERSC hides the vanilla "you're offline" network-error popups at boot and rebrands the bottom-right
version/`OFFLINE` watermark as `Seamless Co-op X.Y.Z`. We want the same (popups suppressed; corner
shows `unseamless-coop <version>`). Both ride the FMG/`MsgRepository` text system and the
online-state (`CSNetMan`/`CSSessionManager`) trigger; the modal-dialog renderer and the FMG
read/write API are **not charted at our SDK pin** (rig RE / SDK-pin bump needed). Full research,
message IDs, and the suppression mechanisms in [`OFFLINE-TITLE-SCREEN.md`](OFFLINE-TITLE-SCREEN.md).

## Suggested milestone ordering

Full parity is large. A sane path that front-loads the genuinely hard part:

1. **M0 — harness (done):** DLL loads, frame task fires, logs. ✅
2. **M1 — easy wins, no networking:** the **E** items (scaling params, summons, splash skip,
   volume). Proves the SDK-write loop end-to-end and is independently useful/testable.
3. **M2 — transport spike:** two instances exchange a heartbeat over Steam P2P. The make-or-break
   feasibility test for the whole project; do this before over-investing elsewhere.
4. **M3 — minimal shared session:** two players, position + basic state sync, one shared world.
   "First playable."
5. **M4+ — layer on:** session actions, PvP toggles, debuffs, then modes (boss/enemy rush) and
   custom goods.

> Reality check from the triage: ERSC's binary is Themida-virtualized, so we reimplement from
> **observed behavior + the public `fromsoftware-rs` SDK + the ER modding community's
> knowledge**, not from its code. That suits the clean-room posture (you can't copy what you
> can't read) but means M2/M3 lean on experimentation, not on reading theirs.
