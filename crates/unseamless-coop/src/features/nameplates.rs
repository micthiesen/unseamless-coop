//! Overhead peer **nameplates** — screen-space labels drawn over each co-op partner.
//!
//! Each frame this reads the live camera and every peer's world position, projects the position to
//! screen NDC with the host-tested [`unseamless_core::projection`] math, culls (behind camera /
//! off-screen / too far), and publishes the survivors to [`crate::nameplates`] for the overlay to
//! draw. Doing the projection here (game thread) keeps the overlay free of any game-state reads — it
//! only draws the published NDC (see `docs/OVERLAY-RENDERING.md` > "(Later) Overhead nameplates").
//!
//! ## Peer source (rig-gated)
//! There is **no live remote-peer roster** until the co-op/session core lands (rung 3). Phantoms do
//! live in `WorldChrMan.player_chr_set`, but mapping a phantom `ChrIns` to a peer *identity* (name,
//! ping, soul level, death count — the content [`OverheadDisplay`] selects) needs the session layer.
//! So today:
//!  - we iterate `player_chr_set` phantoms (the other players in a real session), skipping the local
//!    player, and label each with a placeholder — **TODO: wire real per-peer identity** when the
//!    co-op core arrives, and carry ping/SL/death-count on [`NameplateLabel`];
//!  - solo, `player_chr_set` holds only the local player, so that yields nothing — the
//!    `[nameplates] show_self` debug knob draws a label over your *own* head so the projection + draw
//!    are exercisable solo on the rig before any session exists.
//!
//! ## Conventions to confirm on the rig
//! The projection assumes `forward` points where the camera looks, `fov` is vertical, and world `+Y`
//! is up (so [`HEAD_OFFSET_M`] lifts the label above the feet/center physics position). If labels land
//! mirrored, squashed, or at the wrong height on the rig, those are the knobs — see
//! [`unseamless_core::projection`].

use eldenring::cs::{CSCamera, CSTaskGroupIndex, ChrIns, WorldChrMan};
use fromsoftware_shared::Subclass;
use unseamless_core::config::OverheadDisplay;
use unseamless_core::projection::Camera;
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};
use crate::nameplates::NameplateLabel;

/// Head clearance above a character's physics position (~feet/center), in meters along world up, so
/// the label floats above the head rather than at the navel. Rig-tunable (confirm `+Y` up and height).
const HEAD_OFFSET_M: f32 = 1.8;
/// NDC slop kept on every screen edge before culling — a label hanging slightly off the frame edge is
/// fine (its anchor can be just off-screen while the text is still partly visible).
const ON_SCREEN_MARGIN: f32 = 0.1;

/// Draws overhead nameplates over co-op peers. Config-gated (`[nameplates] enabled` +
/// [`OverheadDisplay`]); a no-op that publishes nothing while off.
pub struct Nameplates {
    /// Tracks the active/inactive transition so we log on change (debug) and clear stale labels once
    /// when switched off, rather than every frame.
    active: Latch<bool>,
}

impl Nameplates {
    pub fn new() -> Self {
        Self { active: Latch::new() }
    }
}

impl Feature for Nameplates {
    fn name(&self) -> &'static str {
        "nameplates"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // After physics so character positions are settled this frame (same reasoning as crit_coop /
        // death_debuffs). We only read positions + the camera; we never write game state.
        CSTaskGroupIndex::WorldChrMan_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        let (enabled, mode, max_dist, show_self) = crate::state::with(|c| {
            (c.nameplates.enabled, c.gameplay.overhead_display, c.nameplates.max_distance_m, c.nameplates.show_self)
        });
        // `OverheadDisplay::None` is the "show nothing overhead" choice, so it disables the feature
        // just as the master toggle does.
        let active = enabled && mode != OverheadDisplay::None;
        if self.active.changed(&active) {
            log::debug!("nameplates {}", if active { "enabled" } else { "disabled" });
            if !active {
                crate::nameplates::publish(Vec::new()); // clear any stale labels once
            }
        }
        if !active {
            return;
        }

        // Read the camera into a plain (SDK-free) `Camera`, so the projection below runs outside the
        // singleton borrow. `None` = no camera singleton yet (early boot / loading) — gather no labels,
        // but still publish empty below so any stale labels clear (don't leave frozen ones on screen).
        let camera = crate::sdk::with_instance::<CSCamera, _>(camera_from).flatten();

        let labels = camera
            .and_then(|camera| {
                crate::sdk::with_instance::<WorldChrMan, _>(|wcm| {
                    gather_labels(wcm, &camera, max_dist, show_self)
                })
            })
            .unwrap_or_default();

        // Publish every frame, even when empty: peers (or the whole set, on a camera/world gap) that
        // went off-screen / out of range this frame must clear rather than freeze at a stale position.
        crate::nameplates::publish(labels);
    }
}

/// Build the host-tested [`Camera`] from the live main camera's named `CSCam` fields. Uses
/// `pers_cam_1` — the composited camera the game actually renders from (the `camera_mask` blends the
/// others into it). The `matrix` rows are the camera's world-space basis + position (the SDK reads
/// them the same way via `CSCamExt::{right,up,forward,position}`), so no raw offsets here.
fn camera_from(cam: &CSCamera) -> Option<Camera> {
    let c = &*cam.pers_cam_1;
    let m = &c.matrix;
    Some(Camera {
        // row 3 = translation (camera world position); rows 0/1/2 = right/up/forward basis vectors.
        position: [m.3.0, m.3.1, m.3.2],
        right: [m.0.0, m.0.1, m.0.2],
        up: [m.1.0, m.1.1, m.1.2],
        forward: [m.2.0, m.2.1, m.2.2],
        fov_y: c.fov,
        aspect_ratio: c.aspect_ratio,
    })
}

/// Project every peer (and optionally the local player) into the on-screen, in-range label set.
fn gather_labels(wcm: &WorldChrMan, camera: &Camera, max_dist: u32, show_self: bool) -> Vec<NameplateLabel> {
    let mut labels = Vec::new();

    // Identify the local player so we can skip it in the phantom roster (it's also an entry there) and
    // optionally label it via `show_self`. Pointer identity, not a name — we have no identity yet.
    let main_ptr =
        wcm.main_player.as_ref().map(|p| std::ptr::from_ref::<ChrIns>((**p).superclass()) as usize);

    // Remote-peer roster (real session only). TODO(co-op core): replace the placeholder label with the
    // peer's real name + ping/SL/death-count once the session layer can map a phantom to an identity.
    let mut peer_n = 0;
    for chr in wcm.player_chr_set.characters() {
        let base = chr.superclass();
        // Skip a mid-load/teardown, half-wired `ChrIns` before chasing its module pointers — the
        // CLAUDE.md `characters()`-yields-regardless-of-load-status UAF caveat (same gate as crit_coop).
        if !base.chr_flags1c8.is_active() {
            continue;
        }
        // Skip the local player here; it's handled by `show_self` below.
        if main_ptr == Some(std::ptr::from_ref(base) as usize) {
            continue;
        }
        peer_n += 1;
        push_label(&mut labels, camera, base, max_dist, format!("Player {peer_n}"));
    }

    // Debug/solo self-nameplate: makes the projection + draw verifiable with no session (the only case
    // that produces a visible nameplate solo). Off in normal play — you don't label yourself.
    if show_self
        && let Some(p) = wcm.main_player.as_ref()
    {
        let base = (**p).superclass();
        if base.chr_flags1c8.is_active() {
            push_label(&mut labels, camera, base, max_dist, "You".to_string());
        }
    }

    labels
}

/// Project one character's head position and, if it survives culling, push its label. Reads the
/// physics-module world position (after `PostPhysics`, so it's this frame's settled value).
fn push_label(labels: &mut Vec<NameplateLabel>, camera: &Camera, base: &ChrIns, max_dist: u32, text: String) {
    let pos = base.modules.physics.position;
    let world = [pos.0, pos.1 + HEAD_OFFSET_M, pos.2];
    let Some(projected) = camera.project(world) else {
        return; // behind the camera
    };
    if projected.depth > max_dist as f32 || !projected.on_screen(ON_SCREEN_MARGIN) {
        return; // too far, or off the edges of the frame
    }
    labels.push(NameplateLabel { ndc: projected.ndc, depth: projected.depth, text });
}
