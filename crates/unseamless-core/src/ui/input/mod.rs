//! `ui::input` — the controller half of the native UI library (see `docs/UI-LIBRARY.md`): the
//! interaction/focus/navigation model. Pure logic over item counts / enabled flags / tabs / a modal
//! focus stack -> current selection state + an [`Action`] enum. No geometry; does not depend on
//! `ui::render`. Generalizes `crate::menu` (it reuses that module's host-tested
//! `step_enabled`/`first_enabled` skip-disabled-and-wrap primitives so the nav algorithm stays
//! single-sourced). Host-tested via selection/action sequences.
//!
//! The integration layer (the cdylib) owns the loop:
//!
//! 1. Rebuild a [`View`] each frame from app state (tab list, per-row `enabled`/`adjustable` flags,
//!    viewport height) — plain data, no geometry.
//! 2. Map raw keyboard/controller input to an [`InputEvent`] and call
//!    [`Navigator::handle`], acting on the returned [`Action`].
//! 3. Read [`Navigator::selected`] / [`active_tab`](Navigator::active_tab) /
//!    [`scroll`](Navigator::scroll) (and the modal accessors) back out and hand them to `ui::render`
//!    as plain indices.
//!
//! ## Design choices (per the brief)
//! - **Selection wraps and skips disabled rows** (matching `crate::menu`); **tabs wrap**.
//! - **Scroll is clamped**, never wrapped, to `0..=content-viewport`; it follows the selection into
//!   view, or a direct page/scroll request.
//! - A tab with **no selectable rows** (a log) turns Up/Down/Page/Home/End into **content
//!   scrolling**.
//! - **Modals capture all input** while open (the underlying cursor freezes) and **nest**.
//! - Empty/all-disabled tabs never panic and never report an `Activated`/`Adjusted` for an invalid
//!   or disabled row.

mod action;
mod event;
mod navigator;
mod view;

pub use action::Action;
pub use event::InputEvent;
pub use navigator::{ModalSpec, Navigator};
pub use view::{Item, Tab, View};
