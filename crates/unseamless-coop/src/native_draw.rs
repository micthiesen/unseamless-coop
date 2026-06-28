//! Native in-game rendering substrate — draws via the game's own `CSEzDraw` (`RendMan.debug_ez_draw`)
//! from a frame task, with **no overlay and no present-hook**. This is the foundation for moving our UI
//! off the hudhook/imgui overlay (see `docs/NAMEPLATES.md` > native rendering): `CSEzDraw` draws
//! untextured, depth-tested colored geometry that the game composites into the scene itself.
//!
//! What's here:
//!  - [`draw_billboard_disc`] — a camera-facing filled disc (the colored overhead nameplate marker).
//!  - [`draw_filled_quad`] / [`ScreenSpace`] / [`draw_screen_rect`] — the screen-space 2D layer (a
//!    near-plane billboard), the substrate for native toasts/menus. `CSEzDraw` geometry is world-space,
//!    so 2D UI is drawn on a plane locked just in front of the camera.
//!  - [`draw_text_world`] — a wrapper over the game's `CSEzDraw::draw_text` that we RE'd. **It does not
//!    work in retail** (kept only as the RE record); see its docs.
//!
//! ## Cost model (rig-measured 2026-06-28)
//! `CSEzDraw` charges **per primitive** (~3µs/quad on the rig; caching the resolved fn pointer did not
//! change it, so it's the game's debug-renderer enqueue/render, likely unbatched). Cheap for a handful
//! of shapes (nameplate markers) and small transient text (toasts); a dense always-redrawn surface (a
//! big menu) costs real frame time, so keep primitive counts down (merge rects, gate to when shown).

use std::sync::OnceLock;

use eldenring::cs::{CSEzDraw, DlColor32, EzDrawFillMode, EzDrawTextCoordMode};
use eldenring::position::HavokPosition;
use fromsoftware_shared::program::Program;
use fromsoftware_shared::{F32Vector4, Triangle};
use pelite::pe64::Pe; // brings `rva_to_va` into scope on `Program`

fn rgba_to_vec4(rgba: [u8; 4]) -> F32Vector4 {
    F32Vector4(
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
        rgba[3] as f32 / 255.0,
    )
}

/// Cached `CSEzDraw::draw_triangle` (RVA `0x264fb10`, from the SDK bundle). The SDK's `ez.draw_triangle`
/// re-resolves the address via `Program::current().rva_to_va()` on **every** call — fine for a few debug
/// shapes, wasteful at many quads/frame. Resolve once and call directly.
type FnDrawTriangle = extern "C" fn(*const CSEzDraw, *const Triangle);
fn draw_triangle_fn() -> Option<FnDrawTriangle> {
    static RESOLVED: OnceLock<Option<usize>> = OnceLock::new();
    let addr = *RESOLVED.get_or_init(|| Program::current().rva_to_va(0x264fb10).ok().map(|v| v as usize));
    addr.map(|a| unsafe { std::mem::transmute::<usize, FnDrawTriangle>(a) })
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
/// Part of the screen-space 2D substrate; not wired until native toasts/menu land (see module docs).
#[allow(dead_code)]
pub fn draw_filled_quad(ez: &mut CSEzDraw, a: &HavokPosition, b: &HavokPosition, c: &HavokPosition, d: &HavokPosition, rgba: [u8; 4]) {
    set_fill(ez, rgba);
    let Some(draw) = draw_triangle_fn() else { return };
    let (t1, t2) = (tri(a, b, c), tri(a, c, d));
    draw(ez as *const CSEzDraw, &t1);
    draw(ez as *const CSEzDraw, &t2);
}

/// Draw a filled **camera-facing disc** (a clean colored "dot") of world-`radius` meters at `center`, in
/// the camera plane spanned by unit vectors `right`/`up`, as a `segments`-triangle fan. Depth-tested in
/// the world by the game. This is the native overhead nameplate marker — no text, no font, no LOD.
pub fn draw_billboard_disc(ez: &mut CSEzDraw, center: &HavokPosition, right: [f32; 3], up: [f32; 3], radius: f32, rgba: [u8; 4], segments: u32) {
    set_fill(ez, rgba);
    let Some(draw) = draw_triangle_fn() else { return };
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
        let t = tri(center, &prev, &cur);
        draw(ez as *const CSEzDraw, &t);
        prev = cur;
    }
}

/// A camera-locked screen plane: maps normalized screen coords (`nx,ny` in -1..1, origin center, +x
/// right, +y up) to world points on a plane `dist` in front of the camera. Apparent on-screen size is
/// `dist`-independent (the fov term cancels), so `dist` only needs to clear the near clip plane. This is
/// how we draw screen-space 2D UI (toasts/menus) with world-space `CSEzDraw` geometry.
/// Part of the screen-space 2D substrate; not wired until native toasts/menu land (see module docs).
#[allow(dead_code)]
pub struct ScreenSpace {
    pos: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fwd: [f32; 3],
    half_w: f32,
    half_h: f32,
    dist: f32,
}

#[allow(dead_code)]
impl ScreenSpace {
    pub fn new(pos: [f32; 3], right: [f32; 3], up: [f32; 3], fwd: [f32; 3], fov_y: f32, aspect: f32, dist: f32) -> Self {
        let half_h = dist * (fov_y * 0.5).tan();
        let half_w = half_h * aspect;
        Self { pos, right, up, fwd, half_w, half_h, dist }
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
/// Part of the screen-space 2D substrate; not wired until native toasts/menu land (see module docs).
#[allow(dead_code)]
pub fn draw_screen_rect(ez: &mut CSEzDraw, ss: &ScreenSpace, cx: f32, cy: f32, hw: f32, hh: f32, rgba: [u8; 4]) {
    let tl = ss.point(cx - hw, cy + hh);
    let tr = ss.point(cx + hw, cy + hh);
    let br = ss.point(cx + hw, cy - hh);
    let bl = ss.point(cx - hw, cy - hh);
    draw_filled_quad(ez, &tl, &tr, &br, &bl, rgba);
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
