//! Spectate-on-death: when the local player dies in a co-op session, hand the camera to a living
//! partner instead of leaving it on their own corpse. The *who-to-follow* decision is the host-tested
//! [`unseamless_core::spectate`] policy (sticky, no per-frame flicker); this feature is the binding —
//! it detects the local death, builds the live partner roster, and drives the game's own death-camera
//! at the chosen partner. Local, per-player preference (`gameplay.always_spectate_on_death`), off by
//! default. Design + the open rig questions: `docs/SPECTATE.md`.
//!
//! ## Status: scaffold wired to the SDK camera lever; the *effect* is RE/rig-gated
//! The mechanism is reachable entirely through charted SDK fields (no AOB/RE needed to *reach* it):
//! - **Detect** local death — `WorldChrMan.main_player`'s `ChrInsFlags1c5::death_flag` (bit 7, the
//!   game's own "is this character dead" toggle), debounced by the host-tested [`DeathDebounce`] so a
//!   scripted/cutscene dip doesn't trip it.
//! - **Choose** a partner — iterate `player_chr_set` (via [`active_characters`], which gates on
//!   `chr_load_status == Active` so we never deref a half-wired joining phantom — CLAUDE.md UAF
//!   caveat), skip the local player, and hand the living ones to [`select_target`].
//! - **Drive** the camera — `WorldChrMan.chr_cam` (a `ChrCam`) exposes `death_cam_target:
//!   Option<NonNull<ChrIns>>` and `camera_type: ChrCamType` with a `DeathCam = 7` variant. We point
//!   `death_cam_target` at the chosen partner and force `DeathCam`, re-asserted each frame.
//!
//! **What's rig-gated (can't be verified by static work — see `docs/SPECTATE.md` > Rig asks):**
//!  1. Whether writing `death_cam_target` + `DeathCam` actually makes the camera *follow* that partner
//!     (the field is named for exactly this, but the game's death-cam controller behaviour is unread),
//!     and whether `WorldChrMan_PostPhysics` is late enough for the write to stick (vs. `CameraStep`).
//!  2. The **respawn-suppression** half ERSC also does — keeping the dead player *in* the session
//!     spectating rather than being sent back to the last grace. That's a deeper respawn-FSM lever
//!     (`CSEventWorldAreaTimeCtrl::respawn_wait_flag` / `reset_main_character` are the candidates) and
//!     is intentionally **out of scope here**; this lane ships the camera half only.
//!
//! Logging is `debug!` (silent unless `[debug] verbose`), except a one-shot `info!` probe on the first
//! confirmed death so a rig log reveals the live camera state to confirm/refute (1) above.

use std::ptr::NonNull;

use eldenring::cs::{CSTaskGroupIndex, ChrCamType, ChrIns, WorldChrMan};
use fromsoftware_shared::Subclass;
use unseamless_core::death_debuffs::DeathDebounce;
use unseamless_core::spectate::{SpectateCandidate, select_target};
use unseamless_core::util::{Edge, Transition};

use crate::feature::{Feature, Tick};
use crate::features::native_nameplates::active_characters;

pub struct SpectateFeature {
    /// Debounced "the local player really died" detector (sustained `death_flag`), so one death enters
    /// spectate once — not on a transient scripted HP/death dip. Re-arms when the player is seen alive.
    ///
    /// We debounce `death_flag` (the game's own settled is-dead bit), not `hp <= 0` like `death_debuffs`.
    /// Reusing that feature's `DeathDebounce::default()` (~0.5s confirm window, tuned for the HP-dip case)
    /// is deliberate and conservative here: `death_flag` is already a settled signal, so the window is
    /// just transient-filtering insurance and only delays *entering* spectate by ~0.5s — there's no
    /// correctness coupling to the HP tuning, so the shared default is fine.
    death: DeathDebounce,
    /// Whether we're currently driving the spectator camera (between a confirmed death and the revive).
    active: bool,
    /// Sticky target: the partner we're following, by stable id (the phantom `ChrIns` pointer until the
    /// session core wires SteamIDs). Fed back into [`select_target`] so the view doesn't flicker.
    current_target: Option<u64>,
    /// Tracks the toggle so an on→off edge releases the camera (we may be mid-spectate when disabled).
    enabled_edge: Edge,
    /// `debug_assertions` only: gate the one-shot camera-state probe to the first confirmed death.
    #[cfg(debug_assertions)]
    probed: bool,
}

impl SpectateFeature {
    pub fn new() -> Self {
        Self {
            death: DeathDebounce::default(),
            active: false,
            current_target: None,
            enabled_edge: Edge::new(),
            #[cfg(debug_assertions)]
            probed: false,
        }
    }
}

impl Feature for SpectateFeature {
    fn name(&self) -> &'static str {
        "spectate"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // After the game's `CameraStep` (which precedes `WorldChrMan_PostPhysics`), so our
        // `death_cam_target`/`camera_type` write re-asserts on top of the game's own camera update each
        // frame. Same proven phase the other death-driven features read in. Whether this is late enough
        // for the override to stick (vs. registering in `CameraStep`) is a rig-confirm — see module docs.
        CSTaskGroupIndex::WorldChrMan_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        let enabled = crate::state::with(|c| c.gameplay.always_spectate_on_death);
        // On the on→off edge, hand the camera back if we were driving it (the toggle can flip mid-death).
        // Reset our state unconditionally (so `active` can't get stuck if WorldChrMan isn't live), then
        // best-effort clear the game camera.
        if self.enabled_edge.update(enabled) == Transition::Falling && self.active {
            self.reset_state();
            crate::sdk::with_instance_mut::<WorldChrMan, _>(reset_chr_cam);
            log::debug!("spectate: released camera (disabled)");
        }
        if !enabled {
            return;
        }
        crate::sdk::with_instance_mut::<WorldChrMan, _>(|wcm| self.tick(wcm));
    }
}

impl SpectateFeature {
    /// One frame of spectate logic against the live `WorldChrMan`. Holds state (does nothing) across a
    /// load/teardown gap where there's no active main player, so a respawn transition doesn't false-revive.
    fn tick(&mut self, wcm: &mut WorldChrMan) {
        // Read the local player's liveness as copied scalars, then drop the borrow before touching
        // `player_chr_set`/`chr_cam`. Presence + `is_active` guard per the CLAUDE.md load-status caveat
        // (a half-wired `ChrIns` mid-transition has unwired modules; `death_flag`/pointer are in the base
        // struct but we still skip a non-active one so we only ever act on a settled state).
        let main_state = wcm.main_player.as_ref().and_then(|m| {
            let base = m.superclass();
            base.chr_flags1c8
                .is_active()
                .then(|| (std::ptr::from_ref::<ChrIns>(base) as u64, base.chr_flags1c5.death_flag()))
        });
        let Some((main_ptr, local_dead)) = main_state else {
            // No readable, settled main player (title / loading / half-wired). Hold spectate *state*
            // across the gap, but never leave a target pointer installed while we can't refresh it — a
            // partner's `ChrIns` can be freed during the load (the CLAUDE.md UAF window). Clearing only
            // the target is safe: during a real load the game isn't running its death cam anyway.
            if self.active {
                clear_death_cam_target(wcm);
            }
            return;
        };

        // Confirmed-death rising edge → enter spectate.
        if self.death.update(local_dead) && !self.active {
            self.active = true;
            log::debug!("spectate: local death confirmed — entering spectator camera");
            #[cfg(debug_assertions)]
            if !self.probed {
                self.probed = true;
                probe_camera_state(wcm);
            }
        }

        // Revived while spectating → release the camera back to the default follow cam.
        if self.active && !local_dead {
            self.release(wcm, "revived");
            return;
        }
        if !self.active {
            return;
        }

        // Spectating: build the living-partner roster (stable ids + live ChrIns pointers), choose a
        // sticky target, and aim the death cam at it. `active_characters` skips non-Active entries, so
        // each `base` here is safe to deref.
        let mut candidates: Vec<SpectateCandidate> = Vec::new();
        let mut targets: Vec<(u64, NonNull<ChrIns>)> = Vec::new();
        for chr in active_characters(&wcm.player_chr_set) {
            let base = chr.superclass();
            // Stable per-partner id = the phantom's `ChrIns` pointer (constant for a loaded phantom
            // across frames — what stickiness needs). Known limitation (shared with `nameplates`): if a
            // partner leaves and a fresh `ChrIns` is later allocated at the same address, stickiness would
            // treat the new player as the old target. TODO(co-op core): swap for the peer's SteamID once
            // the session layer maps phantom → identity.
            let ptr = std::ptr::from_ref::<ChrIns>(base) as u64;
            if ptr == main_ptr {
                continue; // the local player is also an entry in this set — never spectate yourself.
            }
            candidates.push(SpectateCandidate::new(ptr, !base.chr_flags1c5.death_flag()));
            targets.push((ptr, NonNull::from(base)));
        }

        let prev = self.current_target;
        let chosen = select_target(&candidates, prev);
        self.current_target = chosen;
        // Resolve the chosen id to *this frame's* live `ChrIns` pointer (never a pointer cached from a
        // prior frame — that could dangle if the partner left the session and their `ChrIns` was freed).
        // `chosen` always comes from `candidates`, built in lockstep with `targets`, so the lookup hits
        // whenever it's `Some`; `None` ⇒ no living partner this frame ⇒ clear the target.
        let target = chosen.and_then(|id| targets.iter().find(|(pid, _)| *pid == id).map(|&(_, t)| t));
        if prev != chosen {
            match chosen {
                Some(id) => log::debug!(
                    "spectate: following partner {id:#x} ({} living of {})",
                    candidates.iter().filter(|c| c.alive).count(),
                    candidates.len()
                ),
                None => log::debug!("spectate: no living partner — clearing the death-cam target"),
            }
        }
        // Stay `active` even with no target: if a partner revives we pick them up next frame.
        aim_death_cam(wcm, target);
    }

    /// Reset the feature's own spectate state (re-arming for the next death). Pure — no game access, so
    /// it always runs even when `WorldChrMan` isn't live (e.g. a toggle flip at the title screen),
    /// keeping `active` from getting stuck on.
    fn reset_state(&mut self) {
        self.active = false;
        self.current_target = None;
        self.death = DeathDebounce::default();
    }

    /// Stop driving the camera and hand it back to the game's default follow cam. Idempotent.
    fn release(&mut self, wcm: &mut WorldChrMan, reason: &str) {
        self.reset_state();
        reset_chr_cam(wcm);
        log::debug!("spectate: released camera ({reason})");
    }
}

/// Hand the camera fully back to the game on revive/disable: drop the death-cam target, pull
/// `camera_type` out of the `DeathCam` we forced while spectating, and request a reset to the default
/// position behind the player (the SDK-documented effect of `request_camera_reset`). We must un-force
/// `camera_type` ourselves and not trust `request_camera_reset` to do it — since we *forced* `DeathCam`
/// every spectating frame, leaving it set could strand a revived player in the death cam if the game
/// treats `DeathCam` as a latched state it only exits via its own respawn flow. `Unk0` is the normal
/// follow-cam type (enum value 0); that this is the right restore value is a rig-confirm (a wrong choice
/// shows the wrong cam on revive, which the game's per-frame `CameraStep` then corrects) — see
/// `docs/SPECTATE.md`. No-op if the camera isn't wired yet.
fn reset_chr_cam(wcm: &mut WorldChrMan) {
    if let Some(mut cam) = wcm.chr_cam {
        // SAFETY: `chr_cam` is the live per-character camera singleton pointer; non-null here.
        let cam = unsafe { cam.as_mut() };
        cam.death_cam_target = None;
        cam.camera_type = ChrCamType::Unk0;
        cam.request_camera_reset = true;
    }
}

/// Drop only the death-cam target pointer (not `camera_type`, no reset request). Used on a load/hold
/// frame while spectating, so a partner `ChrIns` that gets freed during the load can never be left
/// installed for the game to deref. No-op if the camera isn't wired yet.
fn clear_death_cam_target(wcm: &mut WorldChrMan) {
    if let Some(mut cam) = wcm.chr_cam {
        // SAFETY: `chr_cam` is the live per-character camera singleton pointer; non-null here.
        unsafe { cam.as_mut() }.death_cam_target = None;
    }
}

/// Drive the game's death camera while spectating: with `Some(target)`, point it at that partner and
/// force `DeathCam` mode; with `None` (no living partner this frame), clear the target so the game never
/// reads a stale/freed pointer. Re-asserted each frame. The exact minimal write set (does
/// `death_cam_target` alone suffice, is forcing `camera_type` needed/harmful) is a rig-confirm — see
/// [module docs](self).
fn aim_death_cam(wcm: &mut WorldChrMan, target: Option<NonNull<ChrIns>>) {
    let Some(mut cam) = wcm.chr_cam else {
        return; // camera not wired yet (early boot / loading)
    };
    // SAFETY: `chr_cam` is the live per-character camera singleton pointer; non-null here. We only write
    // two plain fields (a target pointer + an enum), no deref of `target` (the game reads through it).
    let cam = unsafe { cam.as_mut() };
    cam.death_cam_target = target;
    if target.is_some() {
        cam.camera_type = ChrCamType::DeathCam;
    }
}

/// Diag (debug builds only): on the first confirmed death, log the live camera state so a rig run can
/// confirm/refute that `death_cam_target` + `DeathCam` is the right lever. `info!` so it lands in the
/// shared rig log even with verbosity off; compiled out of shipping builds.
#[cfg(debug_assertions)]
fn probe_camera_state(wcm: &WorldChrMan) {
    let chr_cam = wcm.chr_cam.is_some();
    let partners = active_characters(&wcm.player_chr_set).count();
    log::info!(
        "spectate probe (first death): chr_cam present={chr_cam}, player_chr_set Active entries={partners} \
         (incl. self). Watching whether setting death_cam_target + camera_type=DeathCam pans the view to a \
         partner — see docs/SPECTATE.md."
    );
}
