//! Non-overlapping rectangle merge of a 1-bit glyph bitmap.
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

/// Cover `bitmap`'s lit pixels with a small set of **non-overlapping** rectangles (an exact
/// partition: every lit pixel in exactly one rectangle, no unlit pixel ever covered).
///
/// We run three complementary partition strategies and keep whichever yields the fewest rectangles:
/// [`greedy_largest_area`] (the textbook largest-rectangle-first greedy), [`row_runs_merged`]
/// (per-row horizontal runs merged down where their x-extent repeats), and [`column_runs_merged`]
/// (the same pass on the transposed bitmap, i.e. column runs merged across). Each alone is exact and
/// non-overlapping, so the minimum of the three is too — and since the greedy baseline is one of the
/// candidates, the result is **never worse than greedy on any glyph**. On a tie the greedy output
/// wins, so glyphs it already handles optimally keep byte-identical codegen (minimal churn); the
/// whole pass is deterministic, so `generated.rs` is stable across runs.
///
/// # Why not push harder (overlap / exact minimum)?
///
/// This was investigated against both vendored Proggy faces and the headroom is genuinely tiny — the
/// merger is *not* the lever for dense-text cost. (The optima below were derived offline with an
/// exact per-glyph solver — minimum partition via memoised search, minimum overlap-cover via
/// branch-and-bound set cover — over the parsed BDF bitmaps; not reproduced in-tree, but re-derivable
/// from the two faces.)
///
/// - This three-strategy merge emits **908** rects across the charset (Menu 475 + Compact 433), down
///   from greedy's **918** (Menu 480 + Compact 438). The *provable* minimum with non-overlapping
///   rects (exact partition) is **906** — so greedy was already near-optimal (~1.3% of slack) and
///   this recovers most of it. The per-glyph wins are crossing-stroke glyphs greedy splits, e.g.
///   `I` 5→3 and `T` 3→2 (both already reflected in `generated.rs`).
/// - Allowing rectangles to **overlap** would drop the optimum further to **888** (~3.3%), the extra
///   wins on glyphs whose strokes truly cross, e.g. `#` 8→4 and `+` 3→2. We deliberately **forbid
///   overlap**: toast text is drawn with a per-frame fade alpha (`ui::render::widgets`'s
///   `with_alpha(theme.fg, alpha)`), and two overlapping quads composite source-over twice, so the
///   shared pixels (a crossbar centre) would darken visibly mid-fade. Disjoint rects keep the fade
///   uniform. Don't re-attempt an overlap-based merge without first making the renderer overlap-safe.
///
/// So a ~1% rect reduction is the ceiling here; the command-buffer / per-primitive cost of dense
/// text is addressed elsewhere (per-frame caps, smaller surfaces), not by this function.
pub fn merge(bitmap: &Bitmap) -> Vec<Rect> {
    // `Rect` fields are `u8`, so coordinates and extents must fit in 0..=255; glyph cells are tiny
    // (the shipping faces are 7×13 / 6×10), but make the contract loud for any future caller.
    debug_assert!(
        bitmap.width <= 255 && bitmap.height <= 255,
        "merge: bitmap {}x{} exceeds the u8 Rect coordinate range",
        bitmap.width,
        bitmap.height
    );
    // Fewest rectangles wins; `min_by_key` keeps the *first* on a tie, so order the candidates
    // greedy-first to leave already-optimal glyphs untouched.
    [greedy_largest_area(bitmap), row_runs_merged(bitmap), column_runs_merged(bitmap)]
        .into_iter()
        .min_by_key(Vec::len)
        .unwrap()
}

/// Largest-rectangle-first greedy partition: each round finds the largest-area rectangle of
/// still-uncovered lit pixels (anchored at every candidate top-left corner, width = running minimum
/// of the row runs below it), emits it, marks it covered, and repeats to fixpoint. Deterministic
/// (first corner in row-major scan order wins ties).
fn greedy_largest_area(bitmap: &Bitmap) -> Vec<Rect> {
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

/// Partition by horizontal runs merged vertically: split each row into maximal lit runs, then keep a
/// run open across rows for as long as its exact `[x0, x1)` extent repeats, emitting a rectangle when
/// it ends. Optimal for stems and crossbars that greedy splits (a `T` becomes 2 rects, not 3). Exact
/// and non-overlapping: every lit pixel belongs to exactly one run, and runs in a row are disjoint.
fn row_runs_merged(bitmap: &Bitmap) -> Vec<Rect> {
    let (w, h) = (bitmap.width, bitmap.height);
    let mut rects = Vec::new();
    // Rectangles still growing downward: (x0, x1_exclusive, y_start).
    let mut active: Vec<(usize, usize, usize)> = Vec::new();

    for y in 0..h {
        // Maximal lit runs in this row, left to right.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut x = 0;
        while x < w {
            if bitmap.get(x, y) {
                let x0 = x;
                while x < w && bitmap.get(x, y) {
                    x += 1;
                }
                runs.push((x0, x));
            } else {
                x += 1;
            }
        }
        // Continue any active rectangle whose extent reappears this row; close the rest.
        let mut next: Vec<(usize, usize, usize)> = Vec::new();
        for &(x0, x1, ys) in &active {
            if runs.iter().any(|&(a, b)| a == x0 && b == x1) {
                next.push((x0, x1, ys));
            } else {
                rects.push(Rect { x: x0 as u8, y: ys as u8, w: (x1 - x0) as u8, h: (y - ys) as u8 });
            }
        }
        // Open a fresh rectangle for each run not already continued.
        for &(x0, x1) in &runs {
            if !next.iter().any(|&(a, b, _)| a == x0 && b == x1) {
                next.push((x0, x1, y));
            }
        }
        active = next;
    }
    // Flush rectangles that ran to the bottom row.
    for (x0, x1, ys) in active {
        rects.push(Rect { x: x0 as u8, y: ys as u8, w: (x1 - x0) as u8, h: (h - ys) as u8 });
    }
    rects
}

/// The column-wise peer of [`row_runs_merged`]: transpose the bitmap (so column runs become row
/// runs), partition by [`row_runs_merged`], then un-transpose each rectangle (a row-run in the
/// transposed grid is a column-run here). Exact and non-overlapping for the same reasons.
fn column_runs_merged(bitmap: &Bitmap) -> Vec<Rect> {
    let t = transpose(bitmap);
    row_runs_merged(&t).into_iter().map(|r| Rect { x: r.y, y: r.x, w: r.h, h: r.w }).collect()
}

/// Transpose a bitmap (swap x/y), so a "row runs" pass over the result merges *column* runs.
fn transpose(bitmap: &Bitmap) -> Bitmap {
    let mut t = Bitmap::new(bitmap.height, bitmap.width);
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if bitmap.get(x, y) {
                t.set(y, x, true);
            }
        }
    }
    t
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

/// Assert a rectangle set is an *exact, non-overlapping* cover of `bmp`'s lit pixels: it rasterises
/// back byte-for-byte (no missing/extra pixels) **and** no two rectangles share a pixel.
/// Disjointness is load-bearing — the overlay draws faded toast text, so overlapping quads would
/// source-over twice and darken shared pixels mid-fade (see [`merge`]'s docs). Shared across this
/// crate's font tests (here and in the parent module) so the invariant check is defined once.
#[cfg(test)]
pub(crate) fn assert_exact_and_disjoint(rects: &[Rect], bmp: &Bitmap) {
    assert_eq!(rasterize(rects, bmp.width, bmp.height), *bmp, "merge changed the pixels");
    let mut hits = vec![0u32; bmp.width * bmp.height];
    for r in rects {
        for yy in r.y as usize..r.y as usize + r.h as usize {
            for xx in r.x as usize..r.x as usize + r.w as usize {
                hits[yy * bmp.width + xx] += 1;
            }
        }
    }
    assert!(hits.iter().all(|&n| n <= 1), "rectangles overlap: {rects:?}");
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
    fn merge_is_exact_and_disjoint_on_assorted_shapes() {
        let shapes: &[&[&str]] = &[
            &[".#####.", "##...##", "##...##", "#######", "##...##"], // an 'A'-ish cap
            &["#.#.#.#", ".#.#.#.", "#.#.#.#"],                       // checkerboard (worst case)
            &["#######", "...#...", "...#...", "...#..."],            // a 'T'
            &["##....##", "##....##", "########", "##....##"],        // an 'H'
            &[".#.", "###", ".#."],                                   // a '+' (crossing strokes)
        ];
        for art in shapes {
            let bmp = from_art(art);
            assert_exact_and_disjoint(&merge(&bmp), &bmp);
        }
    }

    #[test]
    fn tall_t_partitions_into_two_not_three() {
        // A 'T' whose stem has greater area than the bar: largest-area greedy takes the 1×5 stem
        // first and splits the bar into two 1×1 corners, yielding 3 rects. The row-run strategy keeps
        // the bar whole (1) plus the stem (1) = 2, and `merge` keeps that minimum. This is the
        // per-glyph win the merge buys.
        let bmp = from_art(&["###", ".#.", ".#.", ".#.", ".#."]);
        let rects = merge(&bmp);
        assert_eq!(rects.len(), 2, "tall T should be 2 rects, got {rects:?}");
        assert_exact_and_disjoint(&rects, &bmp);
        assert!(greedy_largest_area(&bmp).len() > rects.len(), "greedy alone is worse here");
    }

    #[test]
    fn sideways_t_needs_the_column_strategy() {
        // A 'T' rotated 90°: a vertical bar on the left with a horizontal stem out of its middle. The
        // wide stem row dominates by area, so greedy takes it and splits the bar (3 rects); the
        // row-run strategy also yields 3 (bar pixel, stem row, bar pixel). Only the *column*-run
        // strategy keeps the bar whole (1) + stem (1) = 2 — so this pins the transpose/un-transpose
        // mapping: if that coordinate swap were wrong, `merge` would silently fall back to 3 here.
        let bmp = from_art(&["#....", "#####", "#...."]);
        let rects = merge(&bmp);
        assert_eq!(rects.len(), 2, "sideways T should be 2 rects, got {rects:?}");
        assert_exact_and_disjoint(&rects, &bmp);
        assert!(greedy_largest_area(&bmp).len() > 2, "greedy alone is worse here");
        assert!(row_runs_merged(&bmp).len() > 2, "row strategy alone is worse here");
        assert_eq!(column_runs_merged(&bmp).len(), 2, "the column strategy is the one that wins");
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
