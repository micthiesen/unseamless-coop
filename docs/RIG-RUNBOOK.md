# Rig runbook — unblocking the co-op core

This is the run/verify side of the workflow: everything here needs the game running on the rig
(the gaming PC). The goal of the first rig session is **not** to make co-op work — it's to
**observe the session state machine** so Layer 2 (see [ARCHITECTURE.md](ARCHITECTURE.md)) can be
designed against reality instead of guesses.

## Deploy

Use `scripts/rig.sh` — it wraps the deploy with a one-time backup of the machine's existing ERSC +
Elden Mod Loader stack, a seed config, and launch/log/restore helpers (see the `/test-loop` skill,
Layer 4):

```bash
scripts/rig.sh cycle                  # backup (once) + build (diag) + apply + launch + wait for heartbeat
scripts/rig.sh log -f                 # follow the run log
scripts/rig.sh restore                # explicit: put the original ERSC stack back
```

**Check the game isn't already running before `apply`/`cycle`** (a running ELDEN RING you didn't
launch = Michael is playing — hands off entirely: no apply, no launch, no kill). `apply` refuses on
its own when `eldenring.exe` is up, both because of that and because the plain `cp` would truncate
the process's mapped `dinput8.dll` inode in place, corrupting the live image's not-yet-faulted
pages. `--force` overrides for the rare case you know the running instance is disposable
(incident that motivated this: 2026-07-01, an autonomous `cycle` fired mid-play-session).

**On a clean rig with no real ERSC stack to protect**, the bare primitive is:

```bash
cargo build --release                 # -> unseamless_coop.dll + start_protected_game.exe
UNSEAMLESS_DEPLOY_STANDALONE=1 ./scripts/deploy.sh   # dinput8.dll + our launcher into ELDEN RING/Game/
rm -f "ELDEN RING/Game/unseamless-coop/logs/"*.log   # so a fresh load is unambiguous
```

`deploy.sh` is the bare install primitive (no backup safety), so it **refuses to run standalone**
unless you pass `UNSEAMLESS_DEPLOY_STANDALONE=1` — the explicit acknowledgement for this case: a
clean rig with no real ERSC stack to protect. On a machine that *does* run the real stack (the gaming
PC) use `scripts/rig.sh apply` instead (it snapshots first). `deploy.sh` installs the cdylib as the
game's `dinput8.dll` proxy (auto-loaded, no separate mod loader) and our launcher as
`start_protected_game.exe` (backing up the original to `start_protected_game_eac.exe`). **Launch via Steam "Play"** — it runs our launcher, which starts
`eldenring.exe` directly (outside EAC) with `UNSEAMLESS_LAUNCH=1` set. The mod **requires** that
marker: launched any other way it aborts the process (the EAC guard), so always launch via Steam
after deploying. On first run it writes a default `unseamless-coop/unseamless_coop.toml` and a run log
under `unseamless-coop/logs/`.

## What to confirm first (harness sanity + the new install layer)

The install layer (proxy / launcher / EAC guard / mod loader) is built and export-verified
statically, but its live behavior is **only confirmable with the game running**. First-rig checklist:

1. **Game launches via Steam "Play"** and reaches the title screen (proves our launcher started
   `eldenring.exe` directly and the `dinput8.dll` proxy forwarded DirectInput without breaking input).
2. **EAC guard:** rename/remove our launcher and launch the original `start_protected_game_eac.exe`
   instead — the game must show the abort message box and close (no marker → no run). Then restore
   our launcher.
3. **Mod loader:** drop a known-good simple DLL mod in `mods/` and confirm a `loaded mod: …` line.

Then the framework sanity, from the **title screen** — the log should show, in order:
1. `loaded config from …` (or `wrote default config …`)
2. mod-loader lines (`no extra mods …` or `loaded mod: …`)
3. `registered feature 'session-observer' in FrameBegin`
4. `observer live; no CSSessionManager yet` heartbeats (every ~30s)

If those appear, the framework works end to end. (`FrameBegin` ticks in menus, so this needs no
save — same trick as er-crit-coop.) Logs are under `unseamless-coop/logs/`.

## The observation run (the actual deliverable)

**The ordered steps are the committed `rig-observation` guide** (set `[debug] guide = "rig-observation"`),
not a hand-driven step-list here — its steps live only in `crates/unseamless-core/src/guide/guides.rs`.
The solo legs **auto-finish** off live state + the observer's `session change @frame …` log line; the
multiplayer legs (player count, in-combat scaling, area-boundary persistence) ship as committed
**stubs**, revived during the friend test (FRIEND-TEST-RUNBOOK) once a real second player is available.
The full create/join FSM *capture* is its own flagship (`rung3-create-chart`), which `rig-observation`
points at rather than duplicating.

What the run is *for* (the rationale the guide steps encode — the data to read out of the captured
`session change:` lines):

- **Solo snapshot:** `players` small + a `limit`; `lobby`/`protocol`/`players`/`limit` and the
  per-player roster (`host/local/cid`). Loading / fighting / dying must **not** drive the FSM solo.
- **Host (create edge):** the `lobby` transition off `None` (`TryToCreateSession`), then `→ Host` once
  a peer joins; the `protocol` FSM alongside.
- **Second player joins** (the key data point): does `players` go to 2, and is the local player counted
  in `players.len()` or separate (`host_player`)? → settles the true "player count" for scaling. What
  is `limit` (`session_player_limit`)? Expect 4 (open world). Roster: which entry is `host`, which
  `local`, the `cid`s and steam_ids.
- **Area boundary / fast-travel together:** whether `protocol` passes through `WaitReentryToMap` and
  the session persists or tears down — the heart of "seamless".
- **Boss fight:** any state change (for boss-vs-enemy scaling targeting later).

Save the full `unseamless_coop.log` from this run — it's the spec for designing Layer 2.

## Findings so far (rig)

From the first solo runs on the PC rig (no second player yet):

- **Binding / load timing.** As the `dinput8.dll` proxy we initialize earlier than an EML-loaded
  mod, before the game's Dantelion2 singleton reflection registers `CSTask`. The SDK's
  `wait_for_instance` returns `InvalidRva` immediately in that window (it only polls the hINSTANCE
  and null-instance cases). `app::install` now retries until the registry is up; in practice it
  binds after a single ~250 ms retry. Confirmed stable across load/combat/death/respawn.
- **Solo session state.** In single-player offline, `CSSessionManager` reads cleanly but holds no
  active session: `lobby=None`, `protocol=None`, `players=0`, `session_player_limit=6`. The **local
  player is not in `players`** — that vector counts *networked* members, so it's 0 solo (we floor to
  1 for scaling, which stays ×1.00). Loading a save / fighting / dying does **not** drive the
  session FSM. So roster/host/local flags and the in-session limit only appear with a real party.
- **Player-limit lever confirmed (write).** Writing `session_player_limit_override = 6` lands and
  reads back (`session change: … override=6`). The active `session_player_limit` is read when a
  session is *created*, so the >4-player effect itself still needs a second player to verify.
- **SDK enum semantics** (from the SDK, to confirm live with a session): `LobbyState`
  (`None`/`TryToCreateSession`/`Host`/`TryToJoinSession`/`Client`/…) and `ProtocolState`
  (`None`/`JoinCheck`/`WaitInitData`/`Ingame`/`WaitReentryToMap`/…) are fully named; `WaitReentryToMap`
  is the seamless map-transition state to watch at an area boundary.

## Follow-up experiments (once the FSM is understood)

In rough order, each a small, reversible probe:

- **Scaling application:** confirm the mechanism. Check whether ERSC-style scaling maps to
  `MultiPlayCorrectionParam` / `MultiSoulBonusRateParam` (idempotent, the likely-correct lever)
  vs raw `NpcParam` HP (which would compound if written per-frame). Prototype by setting the
  correction param once and observing enemy HP, using the multipliers the observer already logs.
- **Session limit:** try raising `session_player_limit` and see whether >4 players can connect,
  and what else gates it.
- **Side-channel:** register a `receive_packet` task on a custom packet type and a manual
  `broadcast_packet` send; confirm round-trip between two modded clients. This is the foundation
  for config sync and session actions.
- **Inbound action authorization:** when wiring the apply layer, gate host-only `SessionAction`s
  (lock/unlock world, the toggles) on the *sender's* host status, not the local client's. The
  menu's `SessionContext` gating only constrains the local UI; a decoded action from a peer is
  structurally valid regardless of who sent it, so the apply site must re-check the sender role.
- **Debug log forwarding:** once the side-channel round-trips, wire `ModMessage::Log` →
  `broadcast_packet` on clients with `[debug] forward_to_host`, and on the host feed received
  records into `diagnostics::LogBundle` and write the merged file. Then a friend's whole session
  lands in one place on the host. Model + message are already host-tested; this is just the
  transport glue. Rate-limit it so debug logging can't flood the channel.
- **Persistence:** find where the game tears down the session on map change and whether keeping
  it alive is viable.

## Menu overlay + settings application (rig-gated UI work)

The menu *model* and the settings *registry* are done and host-tested
(`unseamless-core/menu.rs`, `settings.rs`). What needs the game:

- **Overlay renderer** (`coop/features/menu_overlay.rs`, planned): hook the game's DX12 `Present`
  and draw `Menu::rows(&cfg, &ctx)` with egui (or similar). Pick the hooking approach with the
  game in front of you (this can't be designed blind). Toggle visibility with a hotkey, and read
  `CSFeMan` HUD state to optionally auto-show it when the pause menu is open.
- **Input → menu**: map keys to `select_next`/`select_prev`/`activate`/`adjust` (poll
  `GetAsyncKeyState`). Trivial wiring; just needs the overlay to exist to verify.
- **Notifications renderer + wiring**: draw `Notifications::toasts()` (transient) and `banners()`
  (persistent) from the same overlay, or forward them to the game's native announcement system
  (the `YKNX3_*`-style messages) once that display function is reverse-engineered — the model
  (`unseamless-core/notifications.rs`) feeds either. Wiring decisions to make then:
  - **Owner/cadence:** put one `Notifications` in `App` and `tick(delta)` it **once per frame**
    (not per feature, or toasts age N× too fast). Features reach it through shared app state.
  - **Producers to connect:** the cdylib config-load `Note`s (currently only `log::log!`-replayed
    in `app::install`) → toasts via `Severity::from(log::Level)`; the `Hello` version handshake →
    a `set_banner("version", …)`; the session observer's roster/state changes → "player joined"
    toasts and a "connection lost" banner (`clear_all_banners()` on disconnect).
- **Building `SessionContext`**: fill `in_session`/`is_host` from `CSSessionManager`
  (lobby/protocol state + the local player's `is_host`) — confirm the mapping from the observer
  log first.
- **Applying `MenuOutcome`**: `SettingChanged` → persist config (add `coop/config.rs::save`) and
  re-apply effects (e.g. scaling); `Action(a)` → the session verb, which depends on the co-op
  core. So menu *navigation* can be demoed early; the *actions* light up as Layer 2 lands.

## Tooling on the rig (when needed)

For anything the observer log can't answer (e.g. *which* function relaxes a limit), set up
runtime instrumentation per [RUNTIME-RE.md](RUNTIME-RE.md): a diagnostic build of our own DLL
(preferred) or frida-gadget.

## Rig constraints & gotchas

- **The rig can't run two ELDEN RING instances at once** (one game install, one Steam). So any
  RE/test that needs a *real connect* (the rung-3 **join** leg — driving `lobby_state` to `Client`
  for a peer) needs a **friend / second machine**. The **create/host** leg is solo-confirmable (drive
  the create fn on `[G]` with no peer — see [SESSION-DRIVE.md](SESSION-DRIVE.md)), and the rung-4
  `RunCallbacks`/`CreateLobby` probe is solo too; only the two-modded-games link waits for a friend.
- **Periodic in-game FPS stalls during rig tests are almost always the worker fleet's `cargo` builds,
  not a mod regression.** The gaming PC both builds and runs ELDEN RING, and the rig renders small /
  GPU-light, so concurrent release/clippy/diag builds across worker sessions spike all 16 threads and
  the game lags exactly during that window. Before treating an FPS drop as a bug, check whether workers
  are mid-build; for a clean framerate read, quiesce the fleet and retest on an idle machine.
- **`rig.sh cycle` reaches in-game autonomously** (apply + launch + a ydotool popup-dismiss that
  overshoots into Continue → a loaded save), which makes the drive-probe / in-game RE solo-runnable. It
  needs the KWin + ydotool stack healthy; if a run sticks at the popups, a manual `scripts/rig.sh
  dismiss` clears them. Tune with `RIG_DISMISS_PRESETTLE` / `RIG_DISMISS_PRESSES`.
- **Rig runs clamp the game's saved graphics config.** The game persists whatever display it saw:
  inside the small rig gamescope it rewrites `GraphicsConfig.xml` (in the Proton prefix's
  `AppData/Roaming/EldenRing/`) to WINDOW mode with resolutions clamped down, which would make the
  next manual fullscreen launch a blurry upscale. `scripts/rig/gamescope-wrapper.sh` handles this on
  its gaming path: it runs `scripts/rig/normalize-graphics-config.py` to reset the file to
  FULLSCREEN at the native resolution before launching (defaults 3440x1440; override with
  `UNSEAMLESS_GAMING_WIDTH/HEIGHT`). Two related gamescope facts: `-W/-H` default to **1280x720**
  when omitted (a bare `gamescope -f` is a blurry 720p fullscreen — always pass the size), and the
  file is **UTF-16** (don't sed it).

## Feeding results back

Record findings as behavioral notes **in our own words** (clean-room — see CLAUDE.md), update
FEATURES.md / ARCHITECTURE.md, and turn the confirmed mechanics into:
- host-tested types/state-machines in `unseamless-core`, then
- thin bindings in `unseamless-coop`.
