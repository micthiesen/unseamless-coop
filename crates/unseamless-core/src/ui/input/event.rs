//! The abstract input vocabulary the controller understands.

/// A device-independent input event. The cdylib maps raw keyboard / controller input onto these
/// (e.g. WASD or D-pad → the four directions, Enter / A → [`Activate`](InputEvent::Activate), Esc / B
/// → [`Cancel`](InputEvent::Cancel), the shoulder buttons → [`NextTab`](InputEvent::NextTab) /
/// [`PrevTab`](InputEvent::PrevTab)); the controller stays free of any key/button details.
///
/// `Home` / `End` / `PageUp` / `PageDown` are convenience navigation; a binding that lacks the keys
/// can simply never emit them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputEvent {
    /// Move the selection up one row (or scroll up when the active tab has no selectable rows).
    Up,
    /// Move the selection down one row (or scroll down when there's nothing selectable).
    Down,
    /// Decrease / nudge-left an adjustable (range) row.
    Left,
    /// Increase / nudge-right an adjustable (range) row.
    Right,
    /// Confirm: fire the selected row, or resolve the open modal with its current choice.
    Activate,
    /// Back out: dismiss the open modal, or (with none open) close the menu.
    Cancel,
    /// Switch to the next tab (wraps).
    NextTab,
    /// Switch to the previous tab (wraps).
    PrevTab,
    /// Jump to the first selectable row (or scroll to the top).
    Home,
    /// Jump to the last selectable row (or scroll to the bottom).
    End,
    /// Page the selection / scroll up by one viewport.
    PageUp,
    /// Page the selection / scroll down by one viewport.
    PageDown,
}
