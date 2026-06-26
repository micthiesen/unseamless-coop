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
//! The projection assumes `forward` points where the camera looks, `fov` is vertical **and in radians**,
//! and world `+Y` is up (so [`HEAD_OFFSET_M`] lifts the label above the feet/center physics position).
//! If labels land mirrored, squashed, or at the wrong height, those are the knobs. The fov-unit one is
//! the nastiest: a degrees-vs-radians mismatch grossly mis-projects *everything* (a silent total
//! failure, not a cosmetic skew), so confirm it first — see [`unseamless_core::projection`].

use eldenring::cs::{
    CSCamExt, CSCamera, CSTaskGroupIndex, ChrIns, ChrLoadStatus, ChrSet, WorldChrMan,
};
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
        // death_debuffs). We only read positions + the camera; we never write game state. This phase
        // must also stay *after* `CameraStep` (it is, in the task order) so the camera matrix we read
        // is this frame's — moving this earlier would read a stale/half-updated camera.
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

/// Build the host-tested [`Camera`] from the live main camera's named `CSCam` accessors. Uses
/// `pers_cam_1` — the composited camera the game actually renders from (the `camera_mask` blends the
/// others into it). `None` when `pers_cam_1` isn't wired yet: a live `CSCamera` doesn't guarantee its
/// composited sub-camera pointer is set (early boot / loading), and an unwired deref is a segfault
/// `catch_unwind` can't catch — so we null-guard the `OwnedPtr` the same way [`crate::session::read`]
/// guards the tether warp-data pointer (reading the pointer's address is not a deref).
fn camera_from(cam: &CSCamera) -> Option<Camera> {
    if cam.pers_cam_1.as_ptr().is_null() {
        return None;
    }
    let c = &*cam.pers_cam_1;
    // Named SDK basis accessors (`CSCamExt`) rather than raw matrix indexing: rows 0/1/2 are the
    // right/up/forward basis, row 3 the world position, and the trait reads exactly those rows.
    // `fov` is the engine's **vertical** fov in **radians** and `aspect_ratio` is width/height — both
    // are rig-to-confirm (a degrees-vs-radians mismatch mis-projects everything, not just mirrors it).
    let (right, up, forward, position) = (c.right(), c.up(), c.forward(), c.position());
    Some(Camera {
        position: [position.0, position.1, position.2],
        right: [right.0, right.1, right.2],
        up: [up.0, up.1, up.2],
        forward: [forward.0, forward.1, forward.2],
        fov_y: c.fov,
        aspect_ratio: c.aspect_ratio,
    })
}

/// Iterate a `ChrSet` yielding only **fully loaded** (`ChrLoadStatus::Active`) characters. The SDK's
/// `ChrSet::characters()` yields a `ChrIns` *regardless* of load status (the CLAUDE.md UAF caveat), and
/// a `player_chr_set` phantom mid-join transits `Initializing`/`NetworkInitializing`/`ReadyForActivation`
/// with its `modules` pointers not yet wired — so reading `modules.physics.position` off such an entry
/// is a segfault `catch_unwind` can't catch. We gate on the **entry's** `chr_load_status` (the robust
/// form CLAUDE.md prescribes), not the in-`ChrIns` `is_active` flag, because the two aren't guaranteed
/// to flip in lockstep for a joining network peer (a rig-confirm item). Mirrors the SDK's own entry
/// walk, reading the status alongside the pointer.
fn active_characters<T>(set: &ChrSet<T>) -> impl Iterator<Item = &mut T> + '_
where
    T: Subclass<ChrIns> + 'static,
{
    let mut current = set.entries;
    let end = unsafe { current.add(set.capacity as usize) };
    std::iter::from_fn(move || {
        while current != end {
            let entry = unsafe { current.as_ref() };
            let (status, chr_ins) = (entry.chr_load_status, entry.chr_ins);
            current = unsafe { current.add(1) };
            if status != ChrLoadStatus::Active {
                continue;
            }
            if let Some(mut chr_ins) = chr_ins {
                return Some(unsafe { chr_ins.as_mut() });
            }
        }
        None
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
    // `active_characters` already skips any entry whose `chr_load_status` isn't `Active`, so each
    // `base` here is safe to deref (its modules are wired) — see the helper's docs for why that's the
    // load-bearing guard rather than the in-`ChrIns` `is_active` flag.
    for chr in active_characters(&wcm.player_chr_set) {
        let base = chr.superclass();
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
    // `depth` is forward (view-Z) distance, not radial distance, so `max_distance_m` culls an off-axis
    // peer at a slightly larger true range than the name implies — acceptable for a "don't draw distant
    // labels" cull. `as f32` is lossless: max_dist is clamped to a small integer-meter range.
    if projected.depth > max_dist as f32 || !projected.on_screen(ON_SCREEN_MARGIN) {
        return; // too far, or off the edges of the frame
    }
    labels.push(NameplateLabel { ndc: projected.ndc, depth: projected.depth, text });
}
