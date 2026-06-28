//! Bitmap-font → draw-shapes for the native (non-imgui) overlay renderer.
//!
//! # Why this exists
//!
//! We are spiking a replacement for the imgui overlay that draws through the game's own `CSEzDraw`
//! primitive. `CSEzDraw` paints **untextured solid geometry only** (filled triangles/quads, lines) —
//! there is no texture sampling, so there is no font atlas. A bitmap font therefore has to be drawn
//! as **solid rectangles**, one or more per glyph. This module turns text into exactly that: a list
//! of positioned rectangles the renderer fills.
//!
//! # How it works
//!
//! The glyph→rectangles data is **precomputed**, not rasterised at runtime. The `gen-bitmap-font`
//! generator binary parses the vendored Proggy [`bdf`] bitmaps, merges each glyph's lit pixels into
//! the fewest possible rectangles ([`merge`] — fewer rectangles, fewer draw calls), and emits the
//! static tables in `generated.rs`. At runtime [`shape`] just *positions* those cached rectangles by
//! advancing a pen across the text. Output is in glyph-cell-local **integer pixel** coordinates with
//! the origin at the top-left and `y` increasing downward; the renderer maps pixels to screen space,
//! so this module stays renderer-agnostic.
//!
//! # The two faces
//!
//! [`Face::Menu`] and [`Face::Compact`] are the two roles the overlay draws (toasts, the utility
//! window, debug panes). Both come from the classic **Proggy** bitmap family by Tristan Grimmer — the
//! same lineage as imgui's bundled default font (ProggyClean) — sourced from its native X11 PCF
//! bitmaps, so the glyphs are the hand-designed pixels, not a thresholded outline render. We name the
//! faces by **role**, not size: `Menu` is the interactive-menu face (**ProggyClean, 7x13** — the
//! imgui default size), `Compact` is the glanceable-info face (**ProggyTiny, 6x10** — a smaller,
//! tighter Proggy). Pixel fonts only look right at a native size, so we source two real native sizes
//! rather than downscaling one.

pub mod bdf;
pub mod merge;

mod generated;

pub use merge::Rect;

/// One glyph: the set of rectangles (in glyph-cell-local pixels) whose union is the glyph's lit
/// pixels. Positioned by [`shape`]; the static instances live in `generated.rs`.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub rects: &'static [Rect],
}

/// Precomputed data for one font face: cell metrics plus the per-glyph rectangle sets, indexed by
/// `codepoint - first`. Proggy is monospaced, so a single `advance` covers every glyph.
#[derive(Clone, Copy, Debug)]
pub struct FaceData {
    /// Cell width in pixels (the font bounding box width).
    pub cell_w: u8,
    /// Cell height in pixels (= line height).
    pub cell_h: u8,
    /// Pixels from the cell top down to the baseline.
    pub ascent: u8,
    /// Pixels from the baseline down to the cell bottom.
    pub descent: u8,
    /// Pen advance per glyph (monospaced, so constant).
    pub advance: u8,
    /// Codepoint of `glyphs[0]` (the start of the contiguous covered range — printable ASCII `0x20`).
    pub first: u8,
    /// Glyphs for `first ..= first + glyphs.len() - 1`, in codepoint order.
    pub glyphs: &'static [Glyph],
}

/// The two font faces the overlay uses, named by role (see the module docs for the naming rationale).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// The interactive-menu face: ProggyClean, 7x13 (the classic imgui-default size).
    Menu,
    /// The compact glanceable-info face (toasts, debug panes): ProggyTiny, 6x10.
    Compact,
}

/// Layout metrics for a face, in integer pixels. Everything the renderer needs to place lines and
/// align to a baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// Horizontal pen advance per glyph.
    pub advance: i32,
    /// Vertical advance per line (the cell height).
    pub line_height: i32,
    /// Baseline offset from a glyph cell's top edge.
    pub ascent: i32,
    /// Pixels below the baseline.
    pub descent: i32,
    /// Glyph cell width.
    pub cell_w: i32,
    /// Glyph cell height.
    pub cell_h: i32,
}

/// One glyph rectangle, positioned in the output coordinate space (origin top-left of the shaped run,
/// `y` down). This is what the renderer fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionedRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Face {
    fn data(self) -> &'static FaceData {
        match self {
            Face::Menu => &generated::MENU,
            Face::Compact => &generated::COMPACT,
        }
    }

    /// Layout metrics for this face.
    pub fn metrics(self) -> Metrics {
        let d = self.data();
        Metrics {
            advance: d.advance as i32,
            line_height: d.cell_h as i32,
            ascent: d.ascent as i32,
            descent: d.descent as i32,
            cell_w: d.cell_w as i32,
            cell_h: d.cell_h as i32,
        }
    }

    /// The glyph for `ch`, or `None` if it is outside the covered range (printable ASCII).
    fn glyph(self, ch: char) -> Option<&'static Glyph> {
        let d = self.data();
        let c = ch as u32;
        let first = d.first as u32;
        let last = first + d.glyphs.len() as u32; // exclusive
        (first..last).contains(&c).then(|| &d.glyphs[(c - first) as usize])
    }
}

/// Layout metrics for a face (free-function alias of [`Face::metrics`]).
pub fn metrics(face: Face) -> Metrics {
    face.metrics()
}

/// Turn `text` into a flat list of positioned rectangles to fill.
///
/// The pen starts at `(0, 0)` (top-left). Each character advances the pen by the face advance; `\n`
/// returns the pen to `x = 0` and drops it one line height. Characters outside the covered range
/// (anything but printable ASCII — e.g. tabs or non-ASCII) consume a blank advance so layout stays
/// stable; the overlay constrains user-facing strings to printable ASCII for this reason.
pub fn shape(text: &str, face: Face) -> Vec<PositionedRect> {
    let m = face.metrics();
    // Pre-size against the char count (an O(1) upper bound via byte length): shape() runs per frame
    // in the overlay loop, so reserving up front trims the doubling reallocations as rects push.
    let mut out = Vec::with_capacity(text.len());
    let mut pen_x = 0i32;
    let mut pen_y = 0i32;
    for ch in text.chars() {
        if ch == '\n' {
            pen_x = 0;
            pen_y += m.line_height;
            continue;
        }
        if let Some(g) = face.glyph(ch) {
            for r in g.rects {
                out.push(PositionedRect {
                    x: pen_x + r.x as i32,
                    y: pen_y + r.y as i32,
                    w: r.w as i32,
                    h: r.h as i32,
                });
            }
        }
        pen_x += m.advance;
    }
    out
}

/// The bounding size (width, height) in pixels that [`shape`] will occupy for `text`: the widest line
/// (in glyph cells) by the number of lines. Useful for centering or background sizing. Width counts
/// glyph *cells* (advance-based), so trailing whitespace contributes; height is `lines * line_height`.
pub fn measure(text: &str, face: Face) -> (i32, i32) {
    let m = face.metrics();
    let mut widest = 0;
    let mut lines = 1;
    let mut col = 0;
    for ch in text.chars() {
        if ch == '\n' {
            lines += 1;
            col = 0;
        } else {
            col += 1;
            widest = widest.max(col);
        }
    }
    (widest * m.advance, lines * m.line_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitmap_font::merge::Bitmap;

    /// Vendored BDF sources, included only for tests (the runtime draws from `generated.rs`).
    const PROGGY_CLEAN_BDF: &str = include_str!("../../assets/fonts/proggy-clean-ascii.bdf");
    const PROGGY_TINY_BDF: &str = include_str!("../../assets/fonts/proggy-tiny-ascii.bdf");

    /// Rasterise positioned rectangles onto a `w`×`h` char grid for human-readable assertions:
    /// `#` = filled, `.` = empty. Out-of-grid rectangles are an error (caught by the bounds we pass).
    fn render(rects: &[PositionedRect], w: i32, h: i32) -> String {
        let mut grid = vec![vec!['.'; w as usize]; h as usize];
        for r in rects {
            for yy in r.y..r.y + r.h {
                for xx in r.x..r.x + r.w {
                    assert!(
                        (0..w).contains(&xx) && (0..h).contains(&yy),
                        "rect {r:?} escapes the {w}x{h} grid"
                    );
                    grid[yy as usize][xx as usize] = '#';
                }
            }
        }
        grid.into_iter().map(|row| row.into_iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn menu_capital_a_draws_correctly() {
        let rects = shape("A", Face::Menu);
        let m = Face::Menu.metrics();
        let pic = render(&rects, m.cell_w, m.cell_h);
        // The exact ProggyClean 7x13 'A', drawn from the merged rectangles.
        assert_eq!(
            pic,
            "\
.......
.......
...##..
...##..
..#..#.
..#..#.
..####.
.#....#
.#....#
.#....#
.......
.......
......."
        );
    }

    #[test]
    fn menu_word_advances_the_pen() {
        let rects = shape("Hi", Face::Menu);
        let m = Face::Menu.metrics();
        // Two cells wide. 'H' in the first cell, 'i' in the second (offset by one advance).
        let pic = render(&rects, m.advance * 2, m.cell_h);
        assert_eq!(
            pic,
            "\
..............
..........#...
.#....#.......
.#....#.......
.#....#..##...
.######...#...
.#....#...#...
.#....#...#...
.#....#...#...
.#....#...#...
..............
..............
.............."
        );
    }

    #[test]
    fn newline_drops_a_line_and_resets_x() {
        // Drawn as a picture over two stacked cells: the second 'A' must land exactly one line
        // height below the first AND back at x=0 (the carriage return). A regression that dropped the
        // `pen_x = 0` reset would shift the lower 'A' right and fail this; a y-only check would not.
        let rects = shape("A\nA", Face::Menu);
        let m = Face::Menu.metrics();
        let pic = render(&rects, m.cell_w, m.line_height * 2);
        let one_a = "\
.......
.......
...##..
...##..
..#..#.
..#..#.
..####.
.#....#
.#....#
.#....#
.......
.......
.......";
        assert_eq!(pic, format!("{one_a}\n{one_a}"));
    }

    #[test]
    fn measure_matches_layout() {
        assert_eq!(measure("Hi", Face::Menu), (14, 13)); // 2 cells * 7 advance, 1 line * 13
        assert_eq!(measure("A\nBB", Face::Menu), (14, 26)); // widest line 2 cells, 2 lines
        assert_eq!(measure("", Face::Compact), (0, 10)); // empty: no width, one line high
    }

    #[test]
    fn unknown_chars_advance_blank() {
        // A non-ASCII char produces no rectangles but still advances, so the following glyph lands
        // one cell further right than it would with the char dropped.
        let with = shape("·A", Face::Menu); // middle dot (non-ASCII) then 'A'
        let m = Face::Menu.metrics();
        assert!(with.iter().all(|r| r.x >= m.advance), "'A' should be in the second cell");
    }

    #[test]
    fn compact_face_uses_the_smaller_cell() {
        let m = Face::Compact.metrics();
        assert_eq!((m.cell_w, m.cell_h, m.advance), (6, 10, 6));
        // And it actually produces glyph rectangles.
        assert!(!shape("x", Face::Compact).is_empty());
    }

    /// Build the cell bitmap a face's *generated* rectangles produce for one glyph.
    fn generated_cell(face: Face, ch: char) -> Bitmap {
        let m = face.metrics();
        let rects = shape(&ch.to_string(), face);
        let positioned: Vec<merge::Rect> = rects
            .iter()
            .map(|r| merge::Rect { x: r.x as u8, y: r.y as u8, w: r.w as u8, h: r.h as u8 })
            .collect();
        merge::rasterize(&positioned, m.cell_w as usize, m.cell_h as usize)
    }

    /// The load-bearing correctness test: for every glyph, the precomputed (merged) rectangles in
    /// `generated.rs` rasterise to **exactly** the raw 1-bit bitmap from the vendored BDF. This proves
    /// both that the rectangle merge is lossless and that the committed tables are in sync with the
    /// source font (regenerate if this fails after touching the assets or the generator).
    fn assert_generated_matches_bdf(face: Face, bdf_src: &str) {
        let font = bdf::parse(bdf_src).unwrap();
        // Face-level metrics must match the source too. shape() bakes glyph placement into the rect
        // coordinates and never consults ascent/descent/advance, so a stale baseline metric in the
        // committed tables would otherwise ship green (the pixel comparison below wouldn't catch it).
        let m = face.metrics();
        assert_eq!(m.cell_w, font.cell_w as i32, "{face:?} cell_w drift");
        assert_eq!(m.cell_h, font.cell_h as i32, "{face:?} cell_h drift");
        assert_eq!(m.ascent, font.ascent as i32, "{face:?} ascent drift");
        assert_eq!(m.descent, font.descent as i32, "{face:?} descent drift");
        assert_eq!(m.advance, font.glyphs[0].advance as i32, "{face:?} advance drift");
        // Pin the glyph count both ways: a shorter generated table silently drops glyphs, a longer
        // one would extend shape()'s covered range (`first + glyphs.len()`) with unvalidated rects.
        assert_eq!(
            face.data().glyphs.len(),
            font.glyphs.len(),
            "{face:?} generated glyph count drift"
        );
        for g in &font.glyphs {
            let ch = char::from_u32(g.codepoint).unwrap();
            // Raw bitmap straight from the BDF (no merge).
            let raw = &g.bitmap;
            // Bitmap reconstructed from the committed, merged rectangles.
            let from_generated = generated_cell(face, ch);
            assert_eq!(
                &from_generated, raw,
                "face {face:?} glyph {ch:?} (U+{:04X}): generated rects don't match the BDF source",
                g.codepoint
            );
            // And independently: merging the raw bitmap is lossless.
            let remerged = merge::rasterize(&merge::merge(raw), raw.width, raw.height);
            assert_eq!(&remerged, raw, "merge changed pixels for {ch:?}");
        }
    }

    #[test]
    fn menu_face_matches_proggy_clean_bdf() {
        assert_generated_matches_bdf(Face::Menu, PROGGY_CLEAN_BDF);
    }

    #[test]
    fn compact_face_matches_proggy_tiny_bdf() {
        assert_generated_matches_bdf(Face::Compact, PROGGY_TINY_BDF);
    }
}
