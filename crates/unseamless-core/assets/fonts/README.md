# Vendored Fonts

Bitmap-font sources for the native (non-imgui) overlay renderer's
[`bitmap_font`](../../src/bitmap_font/mod.rs) module. The runtime draws from the **precomputed**
tables in `src/bitmap_font/generated.rs`; these BDF files are the *source* the `gen-bitmap-font`
generator parses and merges (BDF is already 1-bit raster — no rasterisation step), plus the input to
the merge-correctness tests. Nothing here ships in the DLL.

## Files

| File | Proggy size | Role / `Face` |
|------|-------------|---------------|
| `proggy-clean-ascii.bdf` | ProggyClean, 7x13 | `Face::Menu` — the interactive-menu face (the classic imgui-default Proggy) |
| `proggy-tiny-ascii.bdf` | ProggyTiny, 6x10 | `Face::Compact` — the smaller glanceable-info face (toasts, debug panes) |
| `Proggy-LICENSE.txt` | — | MIT license (Tristan Grimmer) |

Both are trimmed to **printable ASCII** (`U+0020`..=`U+007E`, 95 glyphs) — the only charset the overlay
renders (see `docs/OVERLAY-RENDERING.md` > "Rendered strings are ASCII-only").

## Provenance

The classic **[Proggy](https://github.com/bluescan/proggyfonts)** programming bitmap fonts by Tristan
Grimmer, **MIT**. `Face::Menu` is **ProggyClean** (the font imgui ships as its default); `Face::Compact`
is **ProggyTiny**, a smaller, tighter member of the same family. We source both from Proggy's native
X11 **PCF** bitmaps and convert them to BDF (the glyphs are the hand-designed pixels — no rasteriser,
no antialiasing, no thresholding). A bitmap font is only crisp at a size it was hand-designed for, so
we use two real native sizes rather than downscaling one.

The conversion normalises each glyph to a full font-bounding-box cell (matching the BDF shape this
repo's `bitmap_font::bdf` parser expects). To re-derive after a font update: fetch
`ProggyClean.pcf.gz` / `ProggyTiny.pcf.gz` from the upstream repo and run a PCF→BDF conversion
(e.g. `pcf2bdf`, or any tool that preserves the native 1-bit bitmap), trimmed to printable ASCII.

## Regenerating

After changing these files (or the covered charset), regenerate the static tables and re-run the tests:

```sh
cargo run -p unseamless-core --bin gen-bitmap-font \
  --features gen-bitmap-font --target x86_64-unknown-linux-gnu
scripts/test-core.sh bitmap_font
```

The `*_matches_*_bdf` tests assert the committed tables still match these sources, so a stale
`generated.rs` fails loudly.
