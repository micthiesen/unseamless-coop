//! Native overhead **nameplate dot** rendering via the game's own `CSEzDraw` (`RendMan.debug_ez_draw`)
//! from a frame task — no overlay, no present-hook. `CSEzDraw` draws untextured, depth-tested colored
//! geometry that the game composites into the 3D scene itself.
//!
//! This is the **lone native UI surface kept** after the native-UI exploration (toasts/banners/menu are
//! the imgui overlay — see `docs/UI-LIBRARY.md` > OUTCOME). A world-space disc is the right fit here:
//! being a 3D-world point it tracks the head and doesn't swim, unlike the screen-space billboard a 2D UI
//! would need. (The wider exploration — screen-space CSEzDraw, the dead `CSEzDraw::draw_text` RVA, the
//! per-primitive cost — is recorded in `docs/NAMEPLATES.md`, `docs/RE-SCREENSPACE.md`,
//! `docs/RE-GAME-UI.md`.)

use eldenring::cs::{CSEzDraw, EzDrawFillMode};
use eldenring::position::HavokPosition;
use fromsoftware_shared::{F32Vector4, Triangle};

fn rgba_to_vec4(rgba: [u8; 4]) -> F32Vector4 {
    F32Vector4(
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
        rgba[3] as f32 / 255.0,
    )
}

/// Build a `Triangle` (origin + two edge vectors) from three world points.
fn tri(a: &HavokPosition, b: &HavokPosition, c: &HavokPosition) -> Triangle {
    Triangle {
        origin: F32Vector4(a.0, a.1, a.2, a.3),
        edge1: F32Vector4(b.0 - a.0, b.1 - a.1, b.2 - a.2, 0.0),
        edge2: F32Vector4(c.0 - a.0, c.1 - a.1, c.2 - a.2, 0.0),
    }
}

/// Set fill mode + color for subsequent triangle draws (both short-circuit in the SDK if unchanged).
fn set_fill(ez: &mut CSEzDraw, rgba: [u8; 4]) {
    ez.set_fill_mode(EzDrawFillMode::Fill);
    ez.set_color(&rgba_to_vec4(rgba)); // sets both line + fill color
}

/// Draw a filled **camera-facing disc** (a clean colored "dot") of world-`radius` meters at `center`, in
/// the camera plane spanned by unit vectors `right`/`up`, as a `segments`-triangle fan. Depth-tested in
/// the world by the game. The overhead nameplate marker — no text, no font, no LOD.
///
/// Uses the SDK's `CSEzDraw::draw_triangle` directly — it resolves through the SDK's version-detected RVA
/// bundle (panics, caught by the per-task firewall, on an unsupported game build), and its per-call
/// re-resolution is not a bottleneck at a handful of discs.
pub fn draw_billboard_disc(ez: &mut CSEzDraw, center: &HavokPosition, right: [f32; 3], up: [f32; 3], radius: f32, rgba: [u8; 4], segments: u32) {
    set_fill(ez, rgba);
    let segments = segments.max(3);
    // Rim point at angle `ang` (radians) in the camera plane.
    let rim = |ang: f32| {
        let (s, c) = ang.sin_cos();
        HavokPosition(
            center.0 + (right[0] * c + up[0] * s) * radius,
            center.1 + (right[1] * c + up[1] * s) * radius,
            center.2 + (right[2] * c + up[2] * s) * radius,
            center.3,
        )
    };
    let mut prev = rim(0.0);
    for i in 1..=segments {
        let ang = std::f32::consts::TAU * (i as f32) / (segments as f32);
        let cur = rim(ang);
        ez.draw_triangle(&tri(center, &prev, &cur));
        prev = cur;
    }
}
