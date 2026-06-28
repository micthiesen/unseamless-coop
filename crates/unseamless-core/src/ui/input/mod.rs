//! `ui::input` — the controller half of the native UI library (see `docs/UI-LIBRARY.md`): the
//! interaction/focus/navigation model. Pure logic over item counts / enabled flags / tabs / a modal
//! focus stack -> current selection state + an action/outcome enum. No geometry; does not depend on
//! `ui::render`. Generalizes `crate::menu`. Host-tested (selection/action sequences).
//!
//! (Skeleton — implemented by the `ui-input` worker lane.)
