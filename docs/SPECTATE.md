# Spectate-on-death

When the local player dies in a co-op session, hand the camera to a living partner instead of leaving
it on their own corpse. Local, per-player preference (`gameplay.always_spectate_on_death`, **off by
default** — it's a personal preference, not a synced `SharedSetting`).

Model: pure target-selection in [`unseamless-core/spectate.rs`](../crates/unseamless-core/src/spectate.rs)
(host-tested). Binding: [`coop/features/spectate.rs`](../crates/unseamless-coop/src/features/spectate.rs).

## What ERSC does (behavioral note, in our own words)

On death, rather than the vanilla "you died → sent back to the last Site of Grace" flow that would pull
you out of the shared world, the dead player stays in the session and the camera follows a living
teammate until someone revives them / everyone is up again. Two distinct halves: **(a)** the camera
follows a partner, and **(b)** the respawn-to-grace is suppressed so you keep spectating. This lane
ships **(a)**; **(b)** is a deeper respawn-FSM lever, called out under [Rig asks](#rig-asks) below.

## Mechanism (all charted SDK fields — no AOB/RE needed to *reach* the lever)

The whole path is named, typed SDK state at the pinned commit `8c67a84`; how each piece was located:

- **Detect death** — `WorldChrMan.main_player` → `ChrInsFlags1c5::death_flag` (bit 7).
  Found in the SDK's `cs/chr_ins.rs` (`bitfield!` for `ChrInsFlags1c5`, doc'd "Controls whether the
  character is dead or not"). Debounced by the host-tested `DeathDebounce` (reused from death-debuffs)
  so a scripted/cutscene `death_flag` blip doesn't trip spectate. We gate on `chr_flags1c8.is_active()`
  first (CLAUDE.md load-status caveat) and hold state across a load gap (no active main player).
- **Pick a partner** — iterate `WorldChrMan.player_chr_set` via the shared `active_characters` helper
  (`coop/features/nameplates.rs`, gates on `chr_load_status == Active` so a mid-join phantom with
  unwired modules is never deref'd), skip the local player by `ChrIns` pointer identity, mark each
  living (`!death_flag`), and hand them to `select_target` (sticky: keep the current target while alive,
  else first living, else `None`).
- **Drive the camera** — `WorldChrMan.chr_cam` (`cs/world_chr_man.rs:96`, `Option<NonNull<ChrCam>>`)
  → `ChrCam` (`cs/camera.rs:88`) exposes `death_cam_target: Option<NonNull<ChrIns>>` (line 108) and
  `camera_type: ChrCamType` with a `DeathCam = 7` variant (line 121). We set `death_cam_target` to the
  chosen partner's `ChrIns` and force `DeathCam`, re-asserted each frame. With no living partner this
  frame we write `death_cam_target = None` (never leave a stale/freed pointer installed); on a load/hold
  frame while spectating we clear the target the same way. On revive/disable we hand the camera fully
  back: clear the target, pull `camera_type` back to `Unk0` (the normal follow cam — we *forced*
  `DeathCam`, so we must un-force it ourselves rather than trust the reset, else a revive could strand in
  the death cam), and set `request_camera_reset = true` (the SDK-documented "reset to default
  behind-player position"). Phase `WorldChrMan_PostPhysics` runs after the game's `CameraStep`, so our
  write re-asserts on top of the game's camera each frame (1-frame lag, fine).

The SDK charts **no** spectate/observer/free-camera concept — `death_cam_target` is the only camera
follow hook, which is why this rides it rather than building a free cam.

## Rig asks

Static work can reach and write the lever but can't see the result. Precise asks for the orchestrator
(toggle `always_spectate_on_death = true`, get into a 2-player session, die):

1. **Does it pan to the partner?** With the toggle on, on local death the debug build logs one
   `spectate probe (first death): ...` `info!` line (chr_cam present + Active entry count) and then
   `spectate: following partner 0x… (N living of M)`. Watch whether the view actually frames the living
   partner. If not, the next thing to try is registering in `CameraStep` instead of `PostPhysics`
   (write ordering), then whether `death_cam_target` alone (without forcing `camera_type`) is the
   intended single write. **Also confirm the revive path:** that pulling `camera_type` to `Unk0` +
   `request_camera_reset` on release returns a clean follow cam (and that `Unk0` is the right normal
   type — if not, note which `ChrCamType` the game uses in normal play).
2. **Respawn-suppression (half b, separate follow-up):** confirm whether the dead player is still
   sent back to grace while we hold the camera. The respawn-FSM candidates the SDK charts are
   `CSEventWorldAreaTimeCtrl::respawn_wait_flag` / `reset_main_character` (`cs/event_man.rs`) and the
   `DeathState` enum (`cs/game_data_man.rs`). That's its own RE pass; this lane deliberately stops at
   the camera.

## Status

Scaffold wired to the SDK camera lever; the *effect* is rig-gated (asks above). Host-tested
selection policy, builds green (`cargo build --release`, `clippy -D warnings`, `scripts/test-core.sh`).
