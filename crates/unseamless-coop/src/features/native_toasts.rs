//! Native **toasts** — the notification toasts ([`crate::notify`] / `unseamless_core::notifications`)
//! drawn by the game's own `CSEzDraw` renderer ([`crate::native_draw`]) in screen space, instead of the
//! imgui overlay. Real Spleen glyphs (`unseamless_core::bitmap_font`) are rasterized to solid quads on a
//! near-plane billboard — no overlay, no present-hook.
//!
//! This is the first screen-space surface to move off imgui (the path to dropping it entirely; see
//! `docs/NAMEPLATES.md` > native rendering). Gated by `[nameplates] native_spike` (the native-rendering
//! experiment flag), off by default; coexists with the overlay's toasts. Each toast right-aligns at the
//! top, stacks downward, is colored by severity, and fades with its remaining lifetime.

use eldenring::cs::{CSCamExt, CSCamera, CSTaskGroupIndex, RendMan};
use unseamless_core::bitmap_font::{self, Face};
use unseamless_core::notifications::Severity;
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};
use crate::native_draw::{CamFrame, ScreenSpace};

/// Distance (m) of the screen-space plane in front of the camera (just clears the near plane; apparent
/// size is distance-independent).
const PLANE_DIST_M: f32 = 0.5;
/// NDC units per font-pixel — sets the on-screen text size (Spleen Compact is 12px tall).
const SCALE: f32 = 0.0035;
/// Right edge / top edge anchors in NDC, and the gap between stacked toasts.
const RIGHT_NDC: f32 = 0.96;
const TOP_NDC: f32 = 0.9;
const GAP_NDC: f32 = 0.015;

/// Draws notification toasts with the native renderer. No-op unless `[nameplates] native_spike`.
pub struct NativeToasts {
    active: Latch<bool>,
}

impl NativeToasts {
    pub fn new() -> Self {
        Self { active: Latch::new() }
    }
}

impl Feature for NativeToasts {
    fn name(&self) -> &'static str {
        "native_toasts"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Same CSEzDraw frame phase as the other native surfaces; we only read state and enqueue draws.
        CSTaskGroupIndex::ChrIns_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        let enabled = crate::state::with(|c| c.nameplates.native_spike);
        if self.active.changed(&enabled) {
            log::debug!("native toasts {}", if enabled { "enabled" } else { "disabled" });
        }
        if !enabled {
            return;
        }

        // Snapshot the live toasts (cheap clone, skip when empty — the steady-state cost when nothing's
        // showing). `notifications.rs::tick` ages them on a separate frame task, as today.
        let toasts = match crate::notify::try_read(|n| n.toasts().to_vec()) {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };

        let Some(frame) = crate::sdk::with_instance::<CSCamera, _>(camera_frame).flatten() else {
            return;
        };

        crate::sdk::with_instance_mut::<RendMan, _>(|r| {
            if r.debug_ez_draw.as_ptr().is_null() {
                return;
            }
            let ez = r.debug_ez_draw.as_mut();
            let ss = ScreenSpace::new(&frame, PLANE_DIST_M);
            let line_h = bitmap_font::metrics(Face::Compact).line_height as f32;
            let mut y = TOP_NDC;
            for t in &toasts {
                let (w_px, h_px) = bitmap_font::measure(&t.message, Face::Compact);
                let tlx = RIGHT_NDC - w_px as f32 * SCALE; // right-align
                crate::native_draw::draw_text_screen(ez, &ss, &t.message, Face::Compact, [tlx, y], SCALE, severity_rgba(t.severity, fade(t)));
                // Stack downward by the rendered text height (multi-line aware) plus a gap.
                y -= (h_px as f32).max(line_h) * SCALE + GAP_NDC;
            }
        });
    }
}

/// Alpha factor (0..=1) from a toast's remaining lifetime, so it fades as it expires.
fn fade(t: &unseamless_core::notifications::Toast) -> f32 {
    if t.duration > 0.0 { (t.remaining / t.duration).clamp(0.0, 1.0) } else { 1.0 }
}

/// Toast color by severity, with `alpha` (0..=1) applied. ER-plain palette: warm white / amber / red.
fn severity_rgba(sev: Severity, alpha: f32) -> [u8; 4] {
    let [r, g, b] = match sev {
        Severity::Info => [235, 235, 245],
        Severity::Warning => [255, 200, 80],
        Severity::Error => [255, 90, 90],
    };
    [r, g, b, (alpha.clamp(0.0, 1.0) * 255.0) as u8]
}

/// Full camera frame from the composited render camera (`pers_cam_1`). `None` when the sub-camera
/// pointer isn't wired yet (early boot / loading) — mirrors the overlay nameplates feature's guard.
fn camera_frame(cam: &CSCamera) -> Option<CamFrame> {
    if cam.pers_cam_1.as_ptr().is_null() {
        return None;
    }
    let c = &*cam.pers_cam_1;
    let (right, up, fwd, pos) = (c.right(), c.up(), c.forward(), c.position());
    Some(CamFrame {
        pos: [pos.0, pos.1, pos.2],
        right: [right.0, right.1, right.2],
        up: [up.0, up.1, up.2],
        fwd: [fwd.0, fwd.1, fwd.2],
        fov_y: c.fov,
        aspect: c.aspect_ratio,
    })
}
