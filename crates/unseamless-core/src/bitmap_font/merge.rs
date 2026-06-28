//! Greedy maximal-rectangle merge of a 1-bit glyph bitmap.
//!
//! The native overlay renderer draws each glyph as solid rectangles (from [`crate::bitmap_font`]'s
//! output), because the game's `CSEzDraw` primitive paints untextured geometry only — there is no
//! atlas to sample. One
//! filled rectangle per *on* pixel would work but explodes the draw-call count (an `8x16` glyph can
//! be ~60 lit pixels). So at codegen time we cover each glyph's lit pixels with as few axis-aligned
//! rectangles as possible. Fewer rectangles, fewer draw calls, identical pixels.
//!
//! The cover is **exact**: every lit pixel ends up in exactly one rectangle and no unlit pixel is
//! ever covered (worst case a `1x1` rectangle per pixel), so the merged rectangles rasterise back to
//! the original bitmap byte-for-byte. [`crate::bitmap_font`]'s merge-correctness test asserts exactly
//! that. This module is pure and host-tested; it has no runtime role — `shape()` reads the
//! precomputed rectangles from `generated.rs`. It lives in the lib (rather than the generator binary)
//! only so the generator *and* the tests can share one implementation.

/// A 1-bit bitmap laid out row-major, `width * height` cells, `true` = lit. Coordinates are
/// glyph-cell-local with the origin at the top-left and `y` increasing downward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    /// `width * height` cells, row-major (`cells[y * width + x]`).
    pub cells: Vec<bool>,
}

impl Bitmap {
    /// A blank (all-off) bitmap.
    pub fn new(width: usize, height: usize) -> Self {
        Bitmap { width, height, cells: vec![false; width * height] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.cells[y * self.width + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, on: bool) {
        self.cells[y * self.width + x] = on;
    }
}

/// An axis-aligned rectangle in glyph-cell-local pixel coordinates (origin top-left, `y` down).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
}

/// Cover `bitmap`'s lit pixels with a small set of non-overlapping rectangles.
///
/// Greedy: each round finds the largest-area rectangle of still-uncovered lit pixels (anchored at
/// every candidate top-left corner, taking the width as the running minimum of the row runs below
/// it), emits it, marks it covered, and repeats until no lit pixel remains. This is the textbook
/// "largest rectangle anchored at a corner" search run to fixpoint; it is not provably the global
/// minimum cover, but on these tiny glyph cells it collapses long runs and solid blocks into single
/// rectangles, which is the whole point. Tie-break is deterministic (first corner in row-major scan
/// order wins), so codegen output is stable across runs.
pub fn merge(bitmap: &Bitmap) -> Vec<Rect> {
    let (w, h) = (bitmap.width, bitmap.height);
    // `available[i]` is true while cell `i` is lit and not yet claimed by an emitted rectangle —
    // initialised from the bitmap's lit pixels, cleared as each rectangle is emitted.
    let mut available: Vec<bool> = bitmap.cells.clone();
    let avail = |a: &[bool], x: usize, y: usize| a[y * w + x];

    let mut rects = Vec::new();
    loop {
        let mut best: Option<(usize, Rect)> = None; // (area, rect)

        for y0 in 0..h {
            for x0 in 0..w {
                if !avail(&available, x0, y0) {
                    continue;
                }
                // Grow downward. For each candidate height the usable width is the running minimum
                // of each row's available run starting at x0; the best rectangle for this corner is
                // the max-area (min_width * rows) over all heights.
                let mut min_width = w; // will be clamped by the first row's run
                for y in y0..h {
                    // Available run length at (x0, y) going right.
                    let mut run = 0;
                    while x0 + run < w && avail(&available, x0 + run, y) {
                        run += 1;
                    }
                    if run == 0 {
                        break; // this row blocks any taller rectangle at this corner
                    }
                    min_width = min_width.min(run);
                    let height = y - y0 + 1;
                    let area = min_width * height;
                    if best.is_none_or(|(ba, _)| area > ba) {
                        best = Some((
                            area,
                            Rect {
                                x: x0 as u8,
                                y: y0 as u8,
                                w: min_width as u8,
                                h: height as u8,
                            },
                        ));
                    }
                }
            }
        }

        let Some((_, rect)) = best else { break };
        for yy in rect.y as usize..rect.y as usize + rect.h as usize {
            for xx in rect.x as usize..rect.x as usize + rect.w as usize {
                available[yy * w + xx] = false;
            }
        }
        rects.push(rect);
    }
    rects
}

/// Paint `rects` back onto a fresh [`Bitmap`] of the given size — the inverse of [`merge`], used by
/// tests to assert that merging preserves the exact pixels.
pub fn rasterize(rects: &[Rect], width: usize, height: usize) -> Bitmap {
    let mut bmp = Bitmap::new(width, height);
    for r in rects {
        for yy in r.y as usize..r.y as usize + r.h as usize {
            for xx in r.x as usize..r.x as usize + r.w as usize {
                bmp.set(xx, yy, true);
            }
        }
    }
    bmp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bitmap from an ASCII-art picture (`#`/anything-non-`.`-non-space = on, `.`/space = off).
    fn from_art(art: &[&str]) -> Bitmap {
        let height = art.len();
        let width = art.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut bmp = Bitmap::new(width, height);
        for (y, row) in art.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                bmp.set(x, y, ch != '.' && ch != ' ');
            }
        }
        bmp
    }

    #[test]
    fn solid_block_is_one_rect() {
        let bmp = from_art(&["####", "####", "####"]);
        let rects = merge(&bmp);
        assert_eq!(rects, vec![Rect { x: 0, y: 0, w: 4, h: 3 }]);
    }

    #[test]
    fn empty_bitmap_yields_no_rects() {
        let bmp = from_art(&["....", "....."]);
        assert!(merge(&bmp).is_empty());
    }

    #[test]
    fn single_pixel() {
        let bmp = from_art(&[".", "#", "."]);
        assert_eq!(merge(&bmp), vec![Rect { x: 0, y: 1, w: 1, h: 1 }]);
    }

    #[test]
    fn merge_is_lossless_on_assorted_shapes() {
        let shapes: &[&[&str]] = &[
            &[".#####.", "##...##", "##...##", "#######", "##...##"], // an 'A'-ish cap
            &["#.#.#.#", ".#.#.#.", "#.#.#.#"],                       // checkerboard (worst case)
            &["#######", "...#...", "...#...", "...#..."],            // a 'T'
            &["##....##", "##....##", "########", "##....##"],        // an 'H'
        ];
        for art in shapes {
            let bmp = from_art(art);
            let rects = merge(&bmp);
            assert_eq!(
                rasterize(&rects, bmp.width, bmp.height),
                bmp,
                "merge changed the pixels for {art:?}"
            );
        }
    }

    #[test]
    fn checkerboard_cannot_be_merged_below_pixel_count() {
        // A checkerboard has no two adjacent on-pixels, so every rect is 1x1 — the floor case.
        let bmp = from_art(&["#.#", ".#.", "#.#"]);
        assert_eq!(merge(&bmp).len(), 5);
    }

    #[test]
    fn prefers_fewer_rects_than_naive_per_pixel() {
        let bmp = from_art(&["#####", "#####"]);
        // 10 lit pixels, but the whole thing is one rectangle.
        assert_eq!(merge(&bmp).len(), 1);
    }
}
