//! Native **notifications** — toasts *and* banners ([`crate::notify`] /
//! `unseamless_core::notifications`) drawn by the game's own `CSEzDraw` renderer
//! ([`crate::native_draw`]) in screen space, instead of the imgui overlay. The layout + styling come
//! from the pure, host-tested UI library (`unseamless_core::ui::render`): we read the live
//! notifications, lay them out into a [`DrawList`](unseamless_core::ui::render::DrawList) with that
//! library's widgets (`toast_stack` for the corner toast stack, `Banner` for the top-center banner
//! strip), and hand each list to [`native_draw::draw_list`] to rasterize via `CSEzDraw` — real
//! bitmap-font glyphs on a near-plane billboard, no overlay, no present-hook.
//!
//! This is the first screen-space surface to move off imgui (the path to dropping it entirely; see
//! `docs/NAMEPLATES.md` > native rendering, and `docs/UI-LIBRARY.md`). Gated by
//! `[nameplates] native_spike` (the native-rendering experiment flag), off by default; coexists with
//! the overlay's notifications. Toasts stack down the top-right corner, colored + faded by the UI
//! library (`ToastView`); banners stack top-center, colored by severity (`Banner`).

use eldenring::cs::{CSCamExt, CSCamera, CSTaskGroupIndex, RendMan};
use unseamless_core::ui::render::{
    anchor, draw, toast_stack, Align, Anchor, Banner, Rect, Stack, Theme, Widget,
};
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};
use crate::native_draw::{self, CamFrame, ScreenSpace};

/// Distance (m) of the screen-space plane in front of the camera (just clears the near plane; apparent
/// size is distance-independent).
const PLANE_DIST_M: f32 = 0.5;
/// Virtual layout canvas height (px). The UI library lays out in a `DESIGN_HEIGHT`-tall canvas whose
/// width is `DESIGN_HEIGHT * aspect` (so glyphs stay square — see [`native_draw::ui_viewport`]), and
/// `native_draw` scales that uniformly to the real viewport. 1080 is the standard 1080p virtual canvas
/// (`docs/UI-LIBRARY.md`); a smaller value would enlarge the on-screen UI (the orchestrator can tune
/// it on the rig if the text reads too small).
const DESIGN_HEIGHT: f32 = 600.0;

/// Draws notifications (toasts + banners) with the native renderer. No-op unless
/// `[nameplates] native_spike`.
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

        // Snapshot the live notifications under one non-blocking read (cheap clones). `notifications.rs`
        // ages toasts on a separate frame task. Skip all work when there's nothing to show — the
        // steady-state cost when idle.
        let (toasts, banners) = match crate::notify::try_read(|n| (n.toasts().to_vec(), n.banners().to_vec())) {
            Some((t, b)) if !(t.is_empty() && b.is_empty()) => (t, b),
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
            // The UI library lays out in this px viewport; `draw_list` maps it back to screen space.
            let vp = native_draw::ui_viewport(&ss, DESIGN_HEIGHT);
            let viewport = Rect::new(0, 0, vp[0] as i32, vp[1] as i32);
            let theme = Theme::default();
            // Cell-aligned margins/gaps from the theme (one menu cell of inter-element gap).
            let (margin, gap) = (theme.gap, theme.gap);

            // Toasts: a top-right corner stack, faded + severity-striped by the UI library.
            if !toasts.is_empty() {
                let dl = toast_stack(&theme, &toasts, viewport, Anchor::TopRight, margin, gap);
                native_draw::draw_list(ez, &ss, vp, &dl);
            }

            // Banners: a top-center stack of severity-colored strips (persistent until cleared).
            if !banners.is_empty() {
                let mut stack = Stack::vertical().spacing(gap).cross_align(Align::Center);
                for b in &banners {
                    stack = stack.child(Banner::new(b.message.clone(), b.severity));
                }
                let bounds = anchor(stack.measure(&theme), viewport, Anchor::TopCenter, margin);
                let dl = draw(&stack, &theme, bounds);
                native_draw::draw_list(ez, &ss, vp, &dl);
            }
        });
    }
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
