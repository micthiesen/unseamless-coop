//! Native overhead nameplate **markers** — a colored, camera-facing disc over each co-op player, drawn
//! by the game's own `CSEzDraw` renderer ([`crate::native_draw`]) from a frame task, with **no overlay
//! and no present-hook**. The game composites each disc, depth-tested, into the 3D scene itself.
//!
//! **This is the shipped nameplate.** It's deliberately scoped to a *shape* (a colored dot), not
//! in-world text: there's no usable in-world game text (the debug text font isn't loaded in retail;
//! see `docs/RE-GAME-UI.md`), and a per-player colored marker is what we want here — no perspective
//! text, no distance LOD. Each player reads as a distinct palette color. (An earlier imgui
//! projected-label nameplate feature was removed; the native dot is the one nameplate surface — see
//! `docs/NAMEPLATES.md` and `docs/UI-LIBRARY.md` > OUTCOME.)
//!
//! Config-gated by `[nameplates] enabled` (**on by default**; set it `false` to turn the dots off; it's
//! surfaced in the settings menu as "Overhead nameplates"). Marks **your own head** too, so it's
//! verifiable solo on the rig with no session.

use eldenring::cs::{
    CSCamExt, CSCamera, CSTaskGroupIndex, ChrIns, ChrLoadStatus, ChrSet, RendMan, WorldChrMan,
};
use eldenring::position::{HavokPosition, PositionDelta};
use fromsoftware_shared::Subclass; // `superclass()` on ChrIns subclasses
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};

/// Clearance above a character's physics position (~feet), in meters along world up, so the marker
/// floats clearly *above* the head (an ER character is ~1.8m tall, so this must exceed that). Rig-tuned.
const HEAD_OFFSET_M: f32 = 2.5;
/// World radius (meters) of the overhead marker disc. Fixed size (shrinks naturally with distance — no
/// LOD/screen-constant scaling, by design). Rig-tunable.
const DISC_RADIUS_M: f32 = 0.18;
/// Triangle-fan segment count for the disc (smoothness vs primitive count; a marker is cheap).
const DISC_SEGMENTS: u32 = 16;
/// Color for the self marker: a warm near-white, distinct from the peer palette (other players).
const SELF_RGBA: [u8; 4] = [255, 242, 217, 255];

/// Draws overhead nameplate markers with the native renderer. On by default; no-op while
/// `[nameplates] enabled` is off.
pub struct NativeNameplates {
    /// Log on the on/off transition rather than every frame.
    active: Latch<bool>,
}

impl NativeNameplates {
    pub fn new() -> Self {
        Self { active: Latch::new() }
    }
}

impl Feature for NativeNameplates {
    fn name(&self) -> &'static str {
        "native_nameplates"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Same phase the SDK's `debug-line` example draws from: physics is settled (positions are this
        // frame's) and the ez_draw command buffer is still open for this frame's render. We only read
        // positions + the camera and enqueue draws; we never mutate game state.
        CSTaskGroupIndex::ChrIns_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        let enabled = crate::state::with(|c| c.nameplates.enabled);
        if self.active.changed(&enabled) {
            log::debug!("native nameplates {}", if enabled { "enabled" } else { "disabled" });
        }
        if !enabled {
            return;
        }

        // Camera right/up to billboard the disc toward the screen. `None` early (no camera singleton /
        // unwired sub-camera) → skip this frame.
        let Some((right, up)) = crate::sdk::with_instance::<CSCamera, _>(camera_basis).flatten() else {
            return;
        };

        // Gather head positions + colors (read WorldChrMan), then draw (RendMan, mut). Collecting first
        // keeps the two singleton borrows from nesting.
        let markers = crate::sdk::with_instance::<WorldChrMan, _>(gather).unwrap_or_default();
        if markers.is_empty() {
            return;
        }

        crate::sdk::with_instance_mut::<RendMan, _>(|r| {
            // `debug_ez_draw` can be unwired very early (a live `RendMan` doesn't guarantee it) — guard
            // the deref the same way `camera_basis` guards `pers_cam_1`.
            if r.debug_ez_draw.as_ptr().is_null() {
                return;
            }
            let ez = r.debug_ez_draw.as_mut();
            for (pos, rgba) in &markers {
                crate::native_draw::draw_billboard_disc(ez, pos, right, up, DISC_RADIUS_M, *rgba, DISC_SEGMENTS);
            }
        });
    }
}

/// Camera right/up basis from the composited render camera (`pers_cam_1`), for billboarding. `None`
/// when the sub-camera pointer isn't wired yet: a live `CSCamera` doesn't guarantee its composited
/// sub-camera pointer is set (early boot / loading), and an unwired deref is a segfault `catch_unwind`
/// can't catch — so we null-guard the `OwnedPtr` (reading the pointer's address is not a deref).
fn camera_basis(cam: &CSCamera) -> Option<([f32; 3], [f32; 3])> {
    if cam.pers_cam_1.as_ptr().is_null() {
        return None;
    }
    let c = &*cam.pers_cam_1;
    let (right, up) = (c.right(), c.up());
    Some(([right.0, right.1, right.2], [up.0, up.1, up.2]))
}

/// Build the marker set: your own head (always, so it's solo-testable) plus every fully-loaded phantom,
/// each with its color.
fn gather(wcm: &WorldChrMan) -> Vec<(HavokPosition, [u8; 4])> {
    let mut out = Vec::new();

    // Local player pointer, to both mark and exclude it from the phantom roster (it's an entry there).
    let main_ptr =
        wcm.main_player.as_ref().map(|p| std::ptr::from_ref::<ChrIns>((**p).superclass()) as usize);

    if let Some(p) = wcm.main_player.as_ref() {
        let base = (**p).superclass();
        if base.chr_flags1c8.is_active() {
            out.push((head_pos(base), SELF_RGBA));
        }
    }

    // Phantoms (other players in a real session). `active_characters` skips any entry whose
    // `chr_load_status` isn't `Active`, so each `base` deref is safe (modules wired).
    for chr in active_characters(&wcm.player_chr_set) {
        let base = chr.superclass();
        let ptr = std::ptr::from_ref(base) as usize;
        if main_ptr == Some(ptr) {
            continue;
        }
        // Stable per-peer color keyed off the phantom pointer (constant across frames for a loaded
        // phantom), so a peer keeps its color as the roster reorders.
        // TODO(rung-3 / co-op core): swap `ptr` for the peer's SteamID once the session core can map a
        // phantom→identity. Color-by-SteamID is the one remaining nameplate follow-up; it's gated on the
        // session layer landing (see docs/COOP-CONNECTION.md) — the pointer is a stable per-frame
        // stand-in until then, not the final key.
        let c = unseamless_core::palette::peer_color_for_id(ptr as u64);
        out.push((head_pos(base), [to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), 255]));
    }

    out
}

/// Iterate a `ChrSet` yielding only **fully loaded** (`ChrLoadStatus::Active`) characters. The SDK's
/// `ChrSet::characters()` yields a `ChrIns` *regardless* of load status (the CLAUDE.md UAF caveat), and
/// a `player_chr_set` phantom mid-join transits `Initializing`/`NetworkInitializing`/`ReadyForActivation`
/// with its `modules` pointers not yet wired — so reading `modules.physics.position` off such an entry
/// is a segfault `catch_unwind` can't catch. We gate on the **entry's** `chr_load_status` (the robust
/// form CLAUDE.md prescribes), not the in-`ChrIns` `is_active` flag, because the two aren't guaranteed
/// to flip in lockstep for a joining network peer (a rig-confirm item). Mirrors the SDK's own entry
/// walk, reading the status alongside the pointer.
pub(crate) fn active_characters<T>(set: &ChrSet<T>) -> impl Iterator<Item = &mut T> + '_
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

/// Head position = settled physics position lifted by [`HEAD_OFFSET_M`] along world up.
fn head_pos(base: &ChrIns) -> HavokPosition {
    base.modules.physics.position + PositionDelta(0.0, HEAD_OFFSET_M, 0.0)
}

/// Quantize a 0.0..=1.0 color channel to 0..=255.
fn to_u8(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}
