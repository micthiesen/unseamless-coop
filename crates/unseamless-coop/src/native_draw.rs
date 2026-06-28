//! Native in-game rendering substrate — draws via the game's own `CSEzDraw` (`RendMan.debug_ez_draw`)
//! from a frame task, with **no overlay and no present-hook**. This is the foundation for moving our UI
//! off the hudhook/imgui overlay (see `docs/NAMEPLATES.md` > native rendering): `CSEzDraw` draws
//! untextured, depth-tested colored geometry that the game composites into the scene itself.
//!
//! What's here:
//!  - [`draw_billboard_disc`] — a camera-facing filled disc (the colored overhead nameplate marker).
//!  - [`ScreenSpace`] / [`draw_screen_rect`] / [`draw_filled_quad`] — the screen-space 2D layer (a
//!    near-plane billboard), the substrate for native toasts/menus. `CSEzDraw` geometry is world-space,
//!    so 2D UI is drawn on a plane locked just in front of the camera.
//!  - [`draw_text_screen`] / [`text_width_ndc`] — screen-space bitmap text: real Spleen glyphs
//!    ([`unseamless_core::bitmap_font`]) rasterized to filled quads. This is the working native text path.
//!  - [`draw_text_world`] — a wrapper over the game's `CSEzDraw::draw_text` that we RE'd. **It does not
//!    work in retail** (kept only as the RE record); see its docs.
//!
//! ## Cost model (rig-measured 2026-06-28)
//! `CSEzDraw` charges **per primitive** (~3µs/quad on the rig; the cost is the game's debug-renderer
//! enqueue/render, likely unbatched — caching the resolved draw fn pointer made no measurable
//! difference, so we just call the SDK's draw per primitive). Cheap for a handful of shapes (nameplate
//! markers) and small transient text (toasts); a dense always-redrawn surface (a big menu) costs real
//! frame time, so keep primitive counts down (merge rects, gate to when shown).

use std::sync::OnceLock;

use eldenring::cs::{CSEzDraw, DlColor32, EzDrawFillMode, EzDrawTextCoordMode};
use eldenring::position::HavokPosition;
use fromsoftware_shared::program::Program;
use fromsoftware_shared::{F32Vector4, Triangle};
use pelite::pe64::Pe; // brings `rva_to_va` into scope on `Program`
use unseamless_core::bitmap_font::{self, Face, PositionedRect};

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

/// Draw a solid quad (two filled triangles) with corners `a,b,c,d` in winding order (e.g. TL,TR,BR,BL).
/// Uses the SDK's `CSEzDraw::draw_triangle` directly — it resolves through the SDK's version-detected
/// RVA bundle (panics, caught by the per-task firewall, on an unsupported game build) rather than a
/// hardcoded literal. Its per-call re-resolution is not the bottleneck (rig-measured: the per-primitive
/// cost is the game's enqueue/render, see the cost note above).
pub fn draw_filled_quad(ez: &mut CSEzDraw, a: &HavokPosition, b: &HavokPosition, c: &HavokPosition, d: &HavokPosition, rgba: [u8; 4]) {
    set_fill(ez, rgba);
    ez.draw_triangle(&tri(a, b, c));
    ez.draw_triangle(&tri(a, c, d));
}

/// Draw a filled **camera-facing disc** (a clean colored "dot") of world-`radius` meters at `center`, in
/// the camera plane spanned by unit vectors `right`/`up`, as a `segments`-triangle fan. Depth-tested in
/// the world by the game. This is the native overhead nameplate marker — no text, no font, no LOD.
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

/// The render camera's frame: position + orthonormal basis (right/up/forward) + vertical fov (radians)
/// and aspect. The caller reads this from `cs/camera.rs`; [`ScreenSpace`] turns it into a screen plane.
pub struct CamFrame {
    pub pos: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub fwd: [f32; 3],
    pub fov_y: f32,
    pub aspect: f32,
}

/// A camera-locked screen plane: maps normalized screen coords (`nx,ny` in -1..1, origin center, +x
/// right, +y up) to world points on a plane `dist` in front of the camera. Apparent on-screen size is
/// `dist`-independent (the fov term cancels), so `dist` only needs to clear the near clip plane. This is
/// how we draw screen-space 2D UI (toasts/menus) with world-space `CSEzDraw` geometry.
pub struct ScreenSpace {
    pos: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fwd: [f32; 3],
    half_w: f32,
    half_h: f32,
    dist: f32,
}

impl ScreenSpace {
    pub fn new(cam: &CamFrame, dist: f32) -> Self {
        let half_h = dist * (cam.fov_y * 0.5).tan();
        let half_w = half_h * cam.aspect;
        Self { pos: cam.pos, right: cam.right, up: cam.up, fwd: cam.fwd, half_w, half_h, dist }
    }

    /// Screen aspect (width/height). A given NDC extent covers `aspect`× more screen pixels in x than
    /// in y, so anything that must stay square on screen (text glyphs) divides its x scale by this.
    pub fn aspect(&self) -> f32 {
        if self.half_h != 0.0 { self.half_w / self.half_h } else { 1.0 }
    }

    /// World point for screen NDC (`nx,ny` in -1..1).
    pub fn point(&self, nx: f32, ny: f32) -> HavokPosition {
        let sx = nx * self.half_w;
        let sy = ny * self.half_h;
        HavokPosition(
            self.pos[0] + self.fwd[0] * self.dist + self.right[0] * sx + self.up[0] * sy,
            self.pos[1] + self.fwd[1] * self.dist + self.right[1] * sx + self.up[1] * sy,
            self.pos[2] + self.fwd[2] * self.dist + self.right[2] * sx + self.up[2] * sy,
            1.0,
        )
    }
}

/// Draw an axis-aligned screen-space rect (NDC center `cx,cy`, half-extents `hw,hh`) as a filled quad.
pub fn draw_screen_rect(ez: &mut CSEzDraw, ss: &ScreenSpace, cx: f32, cy: f32, hw: f32, hh: f32, rgba: [u8; 4]) {
    let tl = ss.point(cx - hw, cy + hh);
    let tr = ss.point(cx + hw, cy + hh);
    let br = ss.point(cx + hw, cy - hh);
    let bl = ss.point(cx - hw, cy - hh);
    draw_filled_quad(ez, &tl, &tr, &br, &bl, rgba);
}

/// Draw `text` (bitmap `face`) as filled quads at screen-NDC top-left `anchor`, `scale` NDC per
/// font-pixel (in y), with a 1-pixel dark shadow for contrast on any background. The x scale is divided
/// by the screen aspect so glyphs stay square on screen rather than stretching with the viewport (NDC
/// is non-uniform: a unit covers more screen px in x than y). Font pixels are top-left-origin, y-down
/// ([`bitmap_font::shape`]) and map to y-up NDC. The native text primitive that replaces imgui text:
/// real Spleen glyphs rasterized to `CSEzDraw` solid quads. Use [`text_width_ndc`] to right-align.
pub fn draw_text_screen(ez: &mut CSEzDraw, ss: &ScreenSpace, text: &str, face: Face, anchor: [f32; 2], scale: f32, rgba: [u8; 4]) {
    let rects = bitmap_font::shape(text, face);
    let sx = scale / ss.aspect(); // aspect-correct horizontal scale
    let sy = scale;
    let shadow = [0, 0, 0, (rgba[3] as u16 * 4 / 5) as u8];
    blit_rects(ez, ss, &rects, [anchor[0] + sx, anchor[1] - sy], sx, sy, shadow);
    blit_rects(ez, ss, &rects, anchor, sx, sy, rgba);
}

/// On-screen NDC width of `text` at `scale` (matching [`draw_text_screen`]'s aspect-correct x scale).
/// For right-aligning a run: `anchor_x = right_edge - text_width_ndc(...)`.
pub fn text_width_ndc(ss: &ScreenSpace, text: &str, face: Face, scale: f32) -> f32 {
    let (w_px, _) = bitmap_font::measure(text, face);
    w_px as f32 * scale / ss.aspect()
}

/// Fill each shaped glyph rect at NDC top-left `anchor`, scaled by `scale_x`/`scale_y` (NDC per
/// font-pixel; x and y differ to keep glyphs square on a non-square viewport).
fn blit_rects(ez: &mut CSEzDraw, ss: &ScreenSpace, rects: &[PositionedRect], anchor: [f32; 2], scale_x: f32, scale_y: f32, rgba: [u8; 4]) {
    for r in rects {
        let cx = anchor[0] + (r.x as f32 + r.w as f32 * 0.5) * scale_x;
        let cy = anchor[1] - (r.y as f32 + r.h as f32 * 0.5) * scale_y; // y-down px -> y-up NDC
        draw_screen_rect(ez, ss, cx, cy, r.w as f32 * 0.5 * scale_x, r.h as f32 * 0.5 * scale_y, rgba);
    }
}

// --- CSEzDraw::draw_text (RE record; non-functional in retail) -------------------------------------
//
// We located `draw_text` (RVA 0x264efd0) by static RE over eldenring.exe (scripts/re/static.py): the
// charted `cs_ez_draw_draw_line`'s sibling debug-draw consumers call it with UTF-16 labels
// (`自動近接ターゲット`) and printf format strings (`%s[%d]:%d`); its body null-guards the string,
// locks `command_queue_lock`, and enqueues a copy (so a temporary arg is safe). Signature below.
// RIG-CONFIRMED DEAD IN RETAIL (2026-06-28): the call returns, then the game hard-faults at render
// because the debug text font isn't initialized in the shipping build (same in world- and screen-space
// coord modes). Kept only so the address/signature survive for a future session if the font is ever
// found/initialized. Native text instead uses a bitmap font rasterized to filled quads (see the
// `bitmap_font` work + `docs/NAMEPLATES.md`).

/// RVA of `CSEzDraw::draw_text` in `eldenring.exe`. Guarded by the 2-byte prologue below.
const DRAW_TEXT_RVA: u32 = 0x264efd0;
/// Entry opcodes at `draw_text`: `push rdi` with a redundant REX prefix (`40 57`). Drift guard.
const DRAW_TEXT_PROLOGUE: [u8; 2] = [0x40, 0x57];

type FnDrawText = extern "C" fn(*const CSEzDraw, *const HavokPosition, *const u16);

/// Resolve `draw_text` once, verifying the prologue so a game update that shifts the address fails
/// closed rather than jumping into the wrong code.
fn draw_text_fn() -> Option<FnDrawText> {
    static RESOLVED: OnceLock<Option<usize>> = OnceLock::new();
    let addr = *RESOLVED.get_or_init(|| {
        let va = Program::current().rva_to_va(DRAW_TEXT_RVA).ok()?;
        let prologue = unsafe { std::slice::from_raw_parts(va as *const u8, DRAW_TEXT_PROLOGUE.len()) };
        if prologue != DRAW_TEXT_PROLOGUE {
            log::warn!(
                "native_draw: draw_text prologue {prologue:02X?} != {DRAW_TEXT_PROLOGUE:02X?} \
                 (RVA drift after a game update?); native text disabled this session"
            );
            return None;
        }
        Some(va as usize)
    });
    addr.map(|a| unsafe { std::mem::transmute::<usize, FnDrawText>(a) })
}

/// Draw `text` at world position `pos` via `CSEzDraw::draw_text`. **DOES NOT WORK IN RETAIL — DO NOT
/// CALL** (the game faults at render: debug text font not initialized; see the section comment above).
/// Kept as the RE record only.
#[allow(dead_code)]
pub fn draw_text_world(ez: &mut CSEzDraw, pos: &HavokPosition, text: &str, rgba: [u8; 4], font_size: f32) {
    let Some(draw) = draw_text_fn() else { return };
    {
        let buf = ez.current_buffer_mut();
        let st = &mut buf.ez_draw_state.base;
        st.text_coord_mode = EzDrawTextCoordMode::HavokPosition2;
        st.draw_flags.set_text_coord_mode(true);
        st.text_color = DlColor32::from_rgba(rgba[0], rgba[1], rgba[2], rgba[3]);
        st.draw_flags.set_text_color(true);
        st.font_size = font_size;
        st.draw_flags.set_font_size(true);
    }
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    draw(ez as *const CSEzDraw, pos as *const HavokPosition, wide.as_ptr());
}
