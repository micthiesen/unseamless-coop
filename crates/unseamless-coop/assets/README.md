# Overlay assets

## `menu-font.otf`

The crisp UI font for the in-game overlay. A bitmap/pixel font is only crisp at its **native** size, so
instead of scaling imgui's 13px default (which blurs) we bake a font designed at a larger native size:

**Spleen 8x16** — a clean monospace pixel font, crisp at 16px. Subset here to printable ASCII
(U+0020–U+007E) with hinting/DSIG dropped, so it's ~10 KB (vs ~67 KB for the full face). Baked at 16px
(`src/overlay.rs`).

- Source: [Spleen](https://github.com/fcambus/spleen) 2.1.0, `spleen-8x16.otf`.
- License: **BSD-2-Clause** — see [`menu-font.LICENSE.txt`](menu-font.LICENSE.txt).
- Regenerate: `pyftsubset spleen-8x16.otf --unicodes=U+0020-007E --output-file=menu-font.otf
  --no-hinting --desubroutinize --drop-tables+=DSIG`

It's a CFF/OpenType font; imgui's vendored stb_truetype supports CFF, so it loads via `FontSource::TtfData`
without conversion. Embedded into the DLL via `include_bytes!`, so the shipped mod stays self-contained.
