//! `ui::render` — the view half of the native UI library (see `docs/UI-LIBRARY.md`): primitives
//! (`Rect`/`Rgba`/`DrawCmd`/`DrawList`), the layout engine, the theme, and the widget set. Pure +
//! host-tested (ASCII-art rendering tests). Emits a `DrawList` in integer pixels (origin top-left,
//! y-down) that `native_draw` maps to screen space.
//!
//! ## Shape of the API
//! - [`primitives`] — the draw-list contract: [`Rect`], [`Rgba`], [`DrawCmd`], [`DrawList`].
//! - [`theme::Theme`] — palette + spacing + the two `bitmap_font` faces; widgets read it.
//! - [`layout`] — the [`Widget`] trait, the [`Stack`] container, [`anchor`]/[`center`] placement, and
//!   [`clip`]ping for scroll viewports. All placement is **static** (no drag/resize/snap).
//! - [`widgets`] — [`Label`], [`Panel`], [`Divider`], [`List`]/[`Row`], [`Tabs`], [`Modal`],
//!   [`Banner`], [`ToastView`]/[`toast_stack`], [`ScrollView`].
//!
//! Selection index, active tab, and scroll offset are plain input *data* (the controller half,
//! `ui::input`, computes them); this half only turns geometry + data into pixels.

pub mod layout;
pub mod primitives;
pub mod theme;
pub mod widgets;

#[cfg(test)]
mod tests;

pub use layout::{anchor, center, clip, draw, Align, Anchor, Axis, Length, Sizing, Size, Stack, Widget};
pub use primitives::{rgb, with_alpha, DrawCmd, DrawList, Insets, Rect, Rgba};
pub use theme::Theme;
pub use widgets::{
    toast_alpha, toast_stack, Banner, Divider, Label, List, Modal, Panel, Row, ScrollView, Tabs,
    ToastView,
};
