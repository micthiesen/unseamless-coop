//! A minimal parser for the **BDF** (Glyph Bitmap Distribution Format) bitmap fonts we vendor under
//! `assets/fonts/`. BDF stores glyphs as literal 1-bit bitmaps, so it is the cleanest possible source
//! for a pixel font: no rasteriser, no antialiasing, no thresholding — the bytes in the file *are* the
//! pixels we draw. (The shipping overlay's `menu-font.otf` is the same Spleen 8x16 face; we parse the
//! BDF rather than rasterise the OTF precisely to avoid the grid-alignment guesswork a rasteriser
//! introduces.)
//!
//! This parser only handles the subset of BDF that Spleen uses — enough to turn each glyph into a
//! cell-local [`merge::Bitmap`]. It is used by the `gen-bitmap-font` generator binary and by
//! [`crate::bitmap_font`]'s tests; it has no runtime role (the cdylib draws from the precomputed
//! tables in `generated.rs`). It is intentionally strict: a malformed line is a hard error, since the
//! only inputs are files we vendor and control.

use super::merge::Bitmap;

/// A parsed BDF font: face-level metrics plus one decoded glyph per encoded codepoint.
#[derive(Debug)]
pub struct BdfFont {
    /// Cell width in pixels (the font bounding box width). Spleen is monospaced, so every glyph's
    /// advance equals this.
    pub cell_w: usize,
    /// Cell height in pixels (the font bounding box height = ascent + descent).
    pub cell_h: usize,
    /// `FONT_ASCENT`: pixels from the cell top down to the baseline.
    pub ascent: usize,
    /// `FONT_DESCENT`: pixels from the baseline to the cell bottom.
    pub descent: usize,
    /// Decoded glyphs, in the order they appear in the file.
    pub glyphs: Vec<BdfGlyph>,
}

/// One glyph: its codepoint, advance, and a cell-sized 1-bit bitmap with the glyph placed at the
/// correct position within the cell (top-left origin, `y` down).
#[derive(Debug)]
pub struct BdfGlyph {
    pub codepoint: u32,
    pub advance: usize,
    pub bitmap: Bitmap,
}

/// Parse a BDF document. Errors carry a human-readable reason; the inputs are vendored files, so a
/// parse failure means we changed or corrupted an asset, not bad user input.
pub fn parse(src: &str) -> Result<BdfFont, String> {
    let mut cell_w = None;
    let mut cell_h = None;
    let mut fbb_xoff = 0i32;
    let mut ascent = None;
    let mut descent = None;
    let mut glyphs = Vec::new();

    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("FONTBOUNDINGBOX") => {
                let v = parse_ints(&mut it, 4, "FONTBOUNDINGBOX")?;
                cell_w = Some(v[0] as usize);
                cell_h = Some(v[1] as usize);
                fbb_xoff = v[2];
            }
            Some("FONT_ASCENT") => ascent = Some(parse_ints(&mut it, 1, "FONT_ASCENT")?[0] as usize),
            Some("FONT_DESCENT") => {
                descent = Some(parse_ints(&mut it, 1, "FONT_DESCENT")?[0] as usize)
            }
            Some("STARTCHAR") => {
                let (cw, ch, asc) = (
                    cell_w.ok_or("STARTCHAR before FONTBOUNDINGBOX")?,
                    cell_h.ok_or("STARTCHAR before FONTBOUNDINGBOX")?,
                    ascent.ok_or("STARTCHAR before FONT_ASCENT")?,
                );
                glyphs.push(parse_glyph(&mut lines, cw, ch, fbb_xoff, asc)?);
            }
            _ => {}
        }
    }

    Ok(BdfFont {
        cell_w: cell_w.ok_or("missing FONTBOUNDINGBOX")?,
        cell_h: cell_h.ok_or("missing FONTBOUNDINGBOX")?,
        ascent: ascent.ok_or("missing FONT_ASCENT")?,
        descent: descent.ok_or("missing FONT_DESCENT")?,
        glyphs,
    })
}

/// Parse one glyph block, consuming lines through its `ENDCHAR`. `lines` is positioned just after the
/// `STARTCHAR` line.
fn parse_glyph<'a>(
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
    cell_w: usize,
    cell_h: usize,
    fbb_xoff: i32,
    ascent: usize,
) -> Result<BdfGlyph, String> {
    let mut codepoint = None;
    let mut advance = None;
    let mut bbx = None; // (w, h, xoff, yoff)

    for line in lines.by_ref() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("ENCODING") => {
                codepoint = Some(parse_ints(&mut it, 1, "ENCODING")?[0] as u32)
            }
            Some("DWIDTH") => advance = Some(parse_ints(&mut it, 2, "DWIDTH")?[0] as usize),
            Some("BBX") => {
                let v = parse_ints(&mut it, 4, "BBX")?;
                bbx = Some((v[0] as usize, v[1] as usize, v[2], v[3]));
            }
            Some("BITMAP") => {
                let (bw, bh, bxoff, byoff) = bbx.ok_or("BITMAP before BBX")?;
                let codepoint = codepoint.ok_or("BITMAP before ENCODING")?;
                let advance = advance.ok_or("BITMAP before DWIDTH")?;
                let bitmap = parse_bitmap(
                    lines, cell_w, cell_h, ascent, bw, bh, bxoff, byoff, fbb_xoff,
                )?;
                return Ok(BdfGlyph { codepoint, advance, bitmap });
            }
            _ => {}
        }
    }
    Err("glyph block ended before BITMAP".into())
}

/// Read the `bh` hex rows of a `BITMAP` block (through `ENDCHAR`) and place them into a cell-sized
/// [`Bitmap`]. The placement maps BDF's baseline-relative bounding box into cell-local top-left
/// coordinates: row `r` (0 = top of the glyph bbox) lands at cell row `ascent - byoff - bh + r`, and
/// column `col` (MSB-first within the row's bytes) lands at cell column `bxoff - fbb_xoff + col`.
#[allow(clippy::too_many_arguments)]
fn parse_bitmap<'a>(
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
    cell_w: usize,
    cell_h: usize,
    ascent: usize,
    bw: usize,
    bh: usize,
    bxoff: i32,
    byoff: i32,
    fbb_xoff: i32,
) -> Result<Bitmap, String> {
    let mut bmp = Bitmap::new(cell_w, cell_h);
    for r in 0..bh {
        let row = lines.next().ok_or("BITMAP truncated")?.trim();
        // Each row is ceil(bw/8) bytes, MSB-first; only the first `bw` bits are meaningful.
        let bytes = parse_hex_bytes(row)?;
        // Cell row for this bitmap row (see doc comment for the derivation).
        let cy = ascent as i32 - byoff - bh as i32 + r as i32;
        for col in 0..bw {
            let byte = bytes.get(col / 8).copied().unwrap_or(0);
            let on = (byte >> (7 - (col % 8))) & 1 == 1;
            if !on {
                continue;
            }
            let cx = bxoff - fbb_xoff + col as i32;
            if cx < 0 || cy < 0 || cx >= cell_w as i32 || cy >= cell_h as i32 {
                return Err(format!(
                    "glyph pixel ({cx},{cy}) falls outside the {cell_w}x{cell_h} cell"
                ));
            }
            bmp.set(cx as usize, cy as usize, true);
        }
    }
    // Consume up to and including ENDCHAR.
    for line in lines.by_ref() {
        if line.trim_start().starts_with("ENDCHAR") {
            break;
        }
    }
    Ok(bmp)
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err(format!("odd-length hex row {s:?}"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("bad hex {s:?}: {e}")))
        .collect()
}

fn parse_ints<'a>(
    it: &mut impl Iterator<Item = &'a str>,
    n: usize,
    what: &str,
) -> Result<Vec<i32>, String> {
    let v: Vec<i32> = it.take(n).map(|t| t.parse().map_err(|_| format!("bad {what} field {t:?}"))).collect::<Result<_, _>>()?;
    if v.len() != n {
        return Err(format!("{what} expected {n} fields, got {}", v.len()));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tiny 2-glyph BDF: a full-cell 'A' (codepoint 65) and a space (32).
    const TINY: &str = "\
STARTFONT 2.1
FONTBOUNDINGBOX 8 16 0 -4
FONT_ASCENT 12
FONT_DESCENT 4
CHARS 2
STARTCHAR space
ENCODING 32
DWIDTH 8 0
BBX 8 16 0 -4
BITMAP
00
00
00
00
00
00
00
00
00
00
00
00
00
00
00
00
ENDCHAR
STARTCHAR A
ENCODING 65
DWIDTH 8 0
BBX 8 16 0 -4
BITMAP
00
00
7C
C6
C6
C6
FE
C6
C6
C6
C6
C6
00
00
00
00
ENDCHAR
ENDFONT
";

    #[test]
    fn parses_metrics() {
        let f = parse(TINY).unwrap();
        assert_eq!((f.cell_w, f.cell_h, f.ascent, f.descent), (8, 16, 12, 4));
        assert_eq!(f.glyphs.len(), 2);
    }

    // A glyph whose bounding box is *smaller* than the cell and offset from the origin, to exercise
    // the placement math at non-identity offsets. Every vendored Spleen glyph is full-cell
    // (BBX == FONTBOUNDINGBOX), so without this the `cx`/`cy` formulas are only ever tested at
    // bxoff=0 / byoff=-descent. Here a 2x3 solid block sits at BBX `2 3 3 5` in an 8x16 cell.
    const OFFSET: &str = "\
STARTFONT 2.1
FONTBOUNDINGBOX 8 16 0 -4
FONT_ASCENT 12
FONT_DESCENT 4
CHARS 1
STARTCHAR block
ENCODING 64
DWIDTH 8 0
BBX 2 3 3 5
BITMAP
C0
C0
C0
ENDCHAR
ENDFONT
";

    #[test]
    fn places_offset_subcell_bbox() {
        let f = parse(OFFSET).unwrap();
        let g = &f.glyphs[0];
        // cx = bxoff - fbb_xoff + col = 3 + col -> cols 3,4. cy = ascent - byoff - bh + r
        //    = 12 - 5 - 3 + r = 4 + r -> rows 4,5,6. So a 2x3 block at cols 3..=4, rows 4..=6.
        for y in 0..f.cell_h {
            for x in 0..f.cell_w {
                let expect = (3..=4).contains(&x) && (4..=6).contains(&y);
                assert_eq!(g.bitmap.get(x, y), expect, "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn decodes_a_glyph_bitmap() {
        let f = parse(TINY).unwrap();
        let a = f.glyphs.iter().find(|g| g.codepoint == 65).unwrap();
        assert_eq!(a.advance, 8);
        // Row 2 (0x7C = .#####..) is the top of the cap.
        let row2: String = (0..8).map(|x| if a.bitmap.get(x, 2) { '#' } else { '.' }).collect();
        assert_eq!(row2, ".#####..");
        // Row 6 (0xFE = #######.) is the crossbar.
        let row6: String = (0..8).map(|x| if a.bitmap.get(x, 6) { '#' } else { '.' }).collect();
        assert_eq!(row6, "#######.");
        // Rows 0,1 and 12-15 are blank.
        assert!(!a.bitmap.get(2, 0) && !a.bitmap.get(2, 13));
    }
}
