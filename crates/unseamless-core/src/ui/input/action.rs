//! The semantic outcomes the controller reports back to the app.

/// What a single dispatched [`InputEvent`](super::InputEvent) *meant*, beyond the pure cursor/scroll
/// movement already reflected in the [`Navigator`](super::Navigator)'s state.
///
/// [`Navigator::handle`](super::Navigator::handle) returns `Option<Action>`: plain movement
/// (`Up`/`Down`/`Left` on a non-adjustable row/tab-internal scrolling/etc.) returns `None` and only
/// updates state, while the events below carry a meaning the app must act on. `index` / `choice` are
/// row indices within the **active tab** / the open modal; read
/// [`Navigator::active_tab`](super::Navigator::active_tab) for the tab an `index` belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The enabled, selected row was activated (a button/toggle/action). The app performs the
    /// effect (fire the action, flip the toggle).
    Activated { index: usize },
    /// An adjustable (range) row was nudged. `delta` is `-1` for `Left`, `+1` for `Right`; the app
    /// owns the value and applies the step (with its own clamping).
    Adjusted { index: usize, delta: i32 },
    /// The active tab changed (via `NextTab`/`PrevTab`). `tab` is the new active tab index.
    TabChanged { tab: usize },
    /// The open modal was confirmed with `Activate`; `choice` is the chosen option index. The modal
    /// has been popped off the focus stack.
    ModalResolved { choice: usize },
    /// `Cancel` was pressed. With a modal open it was dismissed (popped, no choice made); with none
    /// open the app should close the menu. Inspect [`Navigator::modal_depth`](super::Navigator::modal_depth)
    /// before dispatching if you need to tell the two apart.
    Cancelled,
}
