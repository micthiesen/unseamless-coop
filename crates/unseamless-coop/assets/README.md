# Overlay assets

## `menu-font.ttf`

The crisp UI font for the in-game overlay (the default imgui bitmap font blurs when enlarged). It is a
**printable-ASCII (U+0020–U+007E) subset of Open Sans Regular**, with hinting/glyph-names/DSIG dropped
and the font family renamed to `unseamless menu` — so it is ~11 KB (vs ~147 KB for the full face) and
isn't shipped as a modified face under Open Sans's OFL Reserved Font Name.

- Source: Open Sans Regular (`ttf-opensans`), licensed **OFL 1.1** — see [`menu-font.OFL.txt`](menu-font.OFL.txt).
- Regenerate: `pyftsubset` + a name-table rewrite (the script lived at `/tmp/subset-menu-font.py`):
  subset to `U+0020-007E`, rename name IDs 1/16 → `unseamless menu`, 4 → `unseamless menu Regular`,
  6 → `unseamless-menu-Regular`, 2/17 → `Regular`.

Embedded into the DLL via `include_bytes!` (`src/overlay.rs`), so the shipped mod stays self-contained.
