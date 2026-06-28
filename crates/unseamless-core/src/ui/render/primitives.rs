//! The draw-list contract: the renderer-agnostic primitives `ui::render` emits and `native_draw`
//! rasterizes via `CSEzDraw`. Integer pixels, origin **top-left**, **y-down** — matching
//! `bitmap_font` and `native_draw::draw_text_screen`. See `docs/UI-LIBRARY.md` > the draw-list
//! contract.

use crate::bitmap_font::Face;

/// An 8-bit RGBA color. `[r, g, b, a]`; `a` is opacity (`0` = transparent, `255` = opaque). Matches
/// what `native_draw` expects per vertex.
pub type Rgba = [u8; 4];

/// Construct an opaque [`Rgba`] (alpha `255`). `const` so palette entries can be declared inline.
pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r, g, b, 255]
}

/// Scale a color's alpha by `factor` (`0..=255`), rounding. Used for toast fade — `255` leaves the
/// color unchanged, `0` makes it fully transparent.
pub fn with_alpha(color: Rgba, factor: u8) -> Rgba {
    let a = (color[3] as u16 * factor as u16 + 127) / 255;
    [color[0], color[1], color[2], a as u8]
}

/// A rectangle in integer pixels: top-left `(x, y)`, size `w × h`. A non-positive `w`/`h` is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// The exclusive right edge (`x + w`).
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    /// The exclusive bottom edge (`y + h`).
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Whether the rect covers no pixels (zero or negative extent on either axis).
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// This rect shifted by `(dx, dy)`.
    pub const fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, w: self.w, h: self.h }
    }

    /// This rect shrunk inward by `insets` on each edge (the content box). Clamped so it never goes
    /// negative — over-insetting yields a zero-area rect, not a flipped one.
    pub fn inset(&self, insets: Insets) -> Rect {
        let w = (self.w - insets.horizontal()).max(0);
        let h = (self.h - insets.vertical()).max(0);
        Rect { x: self.x + insets.left, y: self.y + insets.top, w, h }
    }

    /// The overlap of two rects, or `None` when they don't intersect (or either is empty).
    pub fn intersect(&self, other: Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        let r = Rect { x, y, w: right - x, h: bottom - y };
        (!r.is_empty()).then_some(r)
    }
}

/// Per-edge spacing in pixels (padding / margins / insets). Top-right-bottom-left like CSS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Insets {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

impl Insets {
    /// The same inset on all four edges.
    pub const fn all(v: i32) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }

    /// Independent horizontal (`x`, applied left+right) and vertical (`y`, applied top+bottom)
    /// insets. The usual choice for cell-aligned layout: `x` a multiple of the face advance, `y` a
    /// multiple of its line height.
    pub const fn symmetric(x: i32, y: i32) -> Self {
        Self { top: y, right: x, bottom: y, left: x }
    }

    /// Total horizontal inset (`left + right`).
    pub const fn horizontal(&self) -> i32 {
        self.left + self.right
    }

    /// Total vertical inset (`top + bottom`).
    pub const fn vertical(&self) -> i32 {
        self.top + self.bottom
    }
}

/// One draw command. A `DrawList` is an ordered sequence of these; later commands paint over earlier
/// ones (painter's order), which is how highlights, borders, and text layer over backgrounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrawCmd {
    /// A filled rectangle (panels, highlights, dividers, banners).
    Rect { rect: Rect, color: Rgba },
    /// A text run anchored at `pos` (the top-left of its first glyph cell). The cdylib rasterizes it
    /// through `bitmap_font::shape` — we keep it as text (not pre-rasterized glyph quads) so
    /// `native_draw` owns the glyph→quad step and the list stays compact. ASCII only.
    Text { pos: [i32; 2], text: String, face: Face, color: Rgba },
}

/// A flat, ordered list of draw commands — the output of every `ui::render` widget. The integration
/// layer hands this to `native_draw` to paint via `CSEzDraw`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrawList(pub Vec<DrawCmd>);

impl DrawList {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Append a filled rect (skips empty rects so callers don't have to guard zero-size cases).
    pub fn rect(&mut self, rect: Rect, color: Rgba) {
        if !rect.is_empty() {
            self.0.push(DrawCmd::Rect { rect, color });
        }
    }

    /// Append a text run at `pos` (top-left of the first cell). Skips empty strings.
    pub fn text(&mut self, pos: [i32; 2], text: impl Into<String>, face: Face, color: Rgba) {
        let text = text.into();
        if !text.is_empty() {
            self.0.push(DrawCmd::Text { pos, text, face, color });
        }
    }

    /// Append every command from another list (consuming it).
    pub fn append(&mut self, mut other: DrawList) {
        self.0.append(&mut other.0);
    }

    /// The commands, in paint order.
    pub fn cmds(&self) -> &[DrawCmd] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_overlap_and_disjoint() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.intersect(Rect::new(5, 5, 10, 10)), Some(Rect::new(5, 5, 5, 5)));
        assert_eq!(a.intersect(Rect::new(20, 0, 5, 5)), None, "disjoint -> None");
        // Edge-touching rects share no interior pixels (half-open ranges).
        assert_eq!(a.intersect(Rect::new(10, 0, 5, 5)), None, "edge-adjacent -> None");
    }

    #[test]
    fn inset_clamps_instead_of_flipping() {
        let r = Rect::new(0, 0, 8, 8);
        assert_eq!(r.inset(Insets::all(2)), Rect::new(2, 2, 4, 4));
        // Over-inset collapses to zero area at the inset origin, never a negative-size rect.
        let collapsed = r.inset(Insets::all(10));
        assert!(collapsed.is_empty());
        assert_eq!((collapsed.w, collapsed.h), (0, 0));
    }

    #[test]
    fn with_alpha_scales_and_rounds() {
        assert_eq!(with_alpha(rgb(10, 20, 30), 255), rgb(10, 20, 30), "255 leaves opaque unchanged");
        assert_eq!(with_alpha(rgb(10, 20, 30), 0), [10, 20, 30, 0], "0 -> transparent");
        assert_eq!(with_alpha([10, 20, 30, 200], 128)[3], 100, "half of 200 rounds to 100");
    }

    #[test]
    fn drawlist_skips_empty_rects_and_text() {
        let mut dl = DrawList::new();
        dl.rect(Rect::new(0, 0, 0, 5), rgb(1, 2, 3)); // zero width
        dl.text([0, 0], "", Face::Menu, rgb(1, 2, 3)); // empty string
        assert!(dl.is_empty(), "degenerate commands are dropped");
        dl.rect(Rect::new(0, 0, 4, 4), rgb(1, 2, 3));
        assert_eq!(dl.len(), 1);
    }
}
