//! Native UI library — a composable, pure, host-tested widget set that emits a renderer-agnostic draw
//! list (rects + text) rasterized by the cdylib's `native_draw` via `CSEzDraw`, replacing the imgui
//! overlay. Two halves with a strict view/controller split (they share no code; see `docs/UI-LIBRARY.md`):
//!
//!  - [`render`] — the *view*: primitives, layout, widgets, theme -> a `DrawList`.
//!  - [`input`] — the *controller*: interaction/focus/navigation state -> selection + actions.
//!
//! They meet only at the integration layer (the cdylib), which passes plain data between them.

pub mod input;
pub mod render;
