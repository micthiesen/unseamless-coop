# Vendored Fonts

Bitmap-font sources for the native (non-imgui) overlay renderer's
[`bitmap_font`](../../src/bitmap_font/mod.rs) module. The runtime draws from the **precomputed**
tables in `src/bitmap_font/generated.rs`; these BDF files are the *source* the `gen-bitmap-font`
generator parses and merges (BDF is already 1-bit raster — no rasterisation step), plus the input to
the merge-correctness tests. Nothing here ships in the DLL.

## Files

| File | Spleen size | Role / `Face` |
|------|-------------|---------------|
| `spleen-8x16-ascii.bdf` | 8x16 | `Face::Menu` — the crisp interactive-menu face (same face as the overlay's `menu-font.otf`) |
| `spleen-6x12-ascii.bdf` | 6x12 | `Face::Compact` — the smaller glanceable-info face (toasts, debug panes) |
| `Spleen-LICENSE.txt` | — | BSD-2 license (Frederic Cambus) |

Both are trimmed to **printable ASCII** (`U+0020`..=`U+007E`, 95 glyphs) — the only charset the overlay
renders (see `docs/OVERLAY-RENDERING.md` > "Rendered strings are ASCII-only"). The full Spleen BDFs
carry thousands of glyphs we don't use; trimming keeps the vendored sources small.

## Provenance

[Spleen](https://github.com/fcambus/spleen) **2.1.0**, by Frederic Cambus, **BSD-2-Clause**. A
monospaced bitmap font shipped in several *native* pixel sizes. We use two real native sizes rather
than downscaling one, because a bitmap font is only crisp at a size it was hand-designed for.

We pin **2.1.0** to match the exact Spleen release the overlay's `menu-font.otf` was subset from
(`crates/unseamless-coop/assets/README.md`), so the `Menu` face is genuinely the same face the imgui
overlay renders. (The printable-ASCII 8x16 and 6x12 glyph bitmaps are in fact byte-identical between
Spleen 2.1.0 and 2.2.0, but pinning the same release keeps the "same face as `menu-font.otf`" claim
literally true.) The 8x16 face is that same font; parsing the BDF gives the exact 1-bit pixels with no
rasteriser/antialiasing guesswork. The 6x12 face replaces the imgui overlay's bundled default (ProggyClean) for the compact
role: ProggyClean isn't part of the Spleen family and can't be cleanly reproduced as a 1-bit pixel
font, so we unify on one BSD-2 pixel family at two native sizes.

## Regenerating

After changing these files (or the covered charset), regenerate the static tables and re-run the tests:

```sh
cargo run -p unseamless-core --bin gen-bitmap-font \
  --features gen-bitmap-font --target x86_64-unknown-linux-gnu
scripts/test-core.sh bitmap_font
```

The `*_matches_*_bdf` tests assert the committed tables still match these sources, so a stale
`generated.rs` fails loudly.
