//! `ui::render` — the view half of the native UI library (see `docs/UI-LIBRARY.md`): primitives
//! (`Rect`/`Rgba`/`DrawCmd`/`DrawList`), the layout engine, the theme, and the widget set. Pure +
//! host-tested (ASCII-art rendering tests). Emits a `DrawList` in integer pixels (origin top-left,
//! y-down) that `native_draw` maps to screen space.
//!
//! (Skeleton — implemented by the `ui-render` worker lane.)
