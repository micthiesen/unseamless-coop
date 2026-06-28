//! The per-frame description the integration layer feeds the [`Navigator`](super::Navigator).
//!
//! The controller holds no model of its own beyond a cursor (selected index, active tab, scroll
//! offset, modal stack). Everything it needs to interpret an event — how many tabs there are, which
//! rows exist in the active tab, which are enabled, which are adjustable, how tall the viewport is —
//! is supplied fresh each call as a [`View`], exactly as `crate::menu` is handed a
//! `SessionContext` each frame. This keeps the controller a pure function of *(state, event, view)*
//! with no stale snapshot of the layout, and lets the app rebuild the view cheaply every frame.
//!
//! There is **no geometry here** (that's `ui::render`'s job): a "row" is an index and two flags, a
//! viewport is a row count. Coordinates never enter.

/// One selectable row in a tab. Carries only the two facts navigation needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// Whether the row can be selected / activated. Navigation **skips** disabled rows; activating
    /// or adjusting one is a no-op.
    pub enabled: bool,
    /// Whether `Left`/`Right` adjust this row (a range / slider). `false` for plain action or
    /// toggle rows, where `Left`/`Right` do nothing and `Activate` fires the row instead.
    pub adjustable: bool,
}

impl Item {
    /// An enabled action/toggle row: `Activate` fires it, `Left`/`Right` do nothing.
    pub const fn action(enabled: bool) -> Self {
        Self { enabled, adjustable: false }
    }

    /// An enabled range/slider row: `Left`/`Right` adjust it.
    pub const fn range(enabled: bool) -> Self {
        Self { enabled, adjustable: true }
    }

    /// A disabled row (skipped by navigation, inert to activate/adjust). Handy for non-selectable
    /// content rows, e.g. log lines in a scroll-only tab.
    pub const fn disabled() -> Self {
        Self { enabled: false, adjustable: false }
    }
}

/// A single tab's contents: its rows plus how many of them fit on screen at once.
#[derive(Debug, Clone, Copy)]
pub struct Tab<'a> {
    /// The rows, in display order.
    pub items: &'a [Item],
    /// How many rows the viewport shows at once. `0` means "no scrolling" — every row is visible
    /// and the scroll offset is pinned to `0`. The scroll offset is otherwise clamped to
    /// `0..=items.len().saturating_sub(viewport_rows)`.
    pub viewport_rows: usize,
}

impl<'a> Tab<'a> {
    /// A tab whose rows all fit on screen (no scrolling).
    pub const fn new(items: &'a [Item]) -> Self {
        Self { items, viewport_rows: 0 }
    }

    /// A tab with a fixed-height scroll viewport.
    pub const fn scrolling(items: &'a [Item], viewport_rows: usize) -> Self {
        Self { items, viewport_rows }
    }

    /// The largest valid scroll offset for this tab (`0` when everything fits).
    pub(crate) fn max_scroll(&self) -> usize {
        if self.viewport_rows == 0 {
            0
        } else {
            self.items.len().saturating_sub(self.viewport_rows)
        }
    }

    /// Whether any row in this tab can be selected.
    pub(crate) fn has_selectable(&self) -> bool {
        self.items.iter().any(|it| it.enabled)
    }
}

/// The whole layout for one frame: the tab strip plus the active tab's rows. The app rebuilds this
/// each time it dispatches an event; the controller never retains it.
///
/// Build it inline — `View { tabs: &[Tab::new(rows)] }` for a single tab, or pass a multi-tab
/// slice. An empty `tabs` slice means "no menu surface": every navigation event is a no-op, and
/// only `Cancel` still resolves (to close the menu).
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    /// The tabs, in strip order.
    pub tabs: &'a [Tab<'a>],
}
