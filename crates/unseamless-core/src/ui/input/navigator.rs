//! The cursor state machine: events in, selection/tab/scroll/modal state + [`Action`]s out.

use crate::menu::{first_enabled, step_enabled};

use super::action::Action;
use super::event::InputEvent;
use super::view::{Tab, View};

/// Specification for a modal pushed onto the focus stack: one `enabled` flag per option.
///
/// While a modal is open it **captures every input event** — the underlying menu cursor is frozen —
/// until `Activate` (→ [`Action::ModalResolved`]) or `Cancel` (→ [`Action::Cancelled`]) pops it.
/// Modals nest: opening another while one is up stacks on top, and only the topmost receives input.
#[derive(Debug, Clone)]
pub struct ModalSpec {
    /// Per-option enabled flags, in display order. Navigation skips disabled options exactly like a
    /// list; activating a disabled option is a no-op.
    pub options: Vec<bool>,
}

impl ModalSpec {
    /// A modal of `count` options, all enabled (the common yes/no/confirm case).
    pub fn uniform(count: usize) -> Self {
        Self { options: vec![true; count] }
    }

    /// A modal with explicit per-option enabled flags.
    pub fn new(options: Vec<bool>) -> Self {
        Self { options }
    }
}

#[derive(Debug, Clone)]
struct Modal {
    selected: usize,
    options: Vec<bool>,
}

impl Modal {
    fn new(options: Vec<bool>) -> Self {
        let selected = first_enabled(options.len(), |i| options[i]);
        Self { selected, options }
    }

    fn step(&mut self, forward: bool) {
        self.selected = step_enabled(self.selected, self.options.len(), forward, |i| self.options[i]);
    }
}

/// The controller half of the native UI: a pure cursor over tabs / rows / a scroll viewport / a
/// modal focus stack. Feed it an [`InputEvent`] plus the current [`View`]; read its state
/// ([`selected`](Self::selected), [`active_tab`](Self::active_tab), [`scroll`](Self::scroll)) and the
/// returned [`Action`] back out.
///
/// **Navigation choices (documented per the brief):**
/// - Up/Down selection **wraps** around the list and **skips disabled** rows (matching
///   `crate::menu`'s `step_enabled`). Tabs also **wrap**.
/// - Scroll offset is **clamped** (never wraps) to keep the selected row in view, or to a direct
///   page/scroll request, within `0..=content-viewport`.
/// - A tab with no selectable rows (e.g. a log) treats Up/Down/Page/Home/End as **content
///   scrolling** instead of selection movement.
/// - Empty tab list, empty tab, and all-disabled tab are all handled without panicking and never
///   yield an `Activated`/`Adjusted` for an invalid or disabled row.
#[derive(Debug, Clone, Default)]
pub struct Navigator {
    active_tab: usize,
    selected: usize,
    scroll: usize,
    /// Modal focus stack; the last entry is topmost and owns input while non-empty.
    modals: Vec<Modal>,
}

impl Navigator {
    /// A fresh controller: tab 0, row 0, no scroll, no modal. Call [`focus`](Self::focus) once the
    /// view is known to home the cursor onto the first enabled row.
    pub fn new() -> Self {
        Self::default()
    }

    // ----- state accessors (plain data the renderer reads) -----

    /// The active tab index.
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    /// The selected row index **within the active tab**. Two degenerate cases to note: in an
    /// all-disabled tab it points at a (disabled) row whose activation is a no-op, and in an
    /// **empty** tab it is `0` — out of range for the row slice. Index rows with `.get()`, never
    /// `items[nav.selected()]`.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The scroll offset (first visible row) of the active tab's viewport.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// How many modals are stacked (`0` = none open).
    pub fn modal_depth(&self) -> usize {
        self.modals.len()
    }

    /// Whether a modal currently captures input.
    pub fn in_modal(&self) -> bool {
        !self.modals.is_empty()
    }

    /// The topmost modal's selected option, or `None` when no modal is open.
    pub fn modal_selection(&self) -> Option<usize> {
        self.modals.last().map(|m| m.selected)
    }

    // ----- modal stack -----

    /// Push a modal onto the focus stack. It homes onto its first enabled option and captures all
    /// input until resolved. Safe to call while a modal is already open (modals nest).
    pub fn open_modal(&mut self, spec: ModalSpec) {
        self.modals.push(Modal::new(spec.options));
    }

    // ----- driving -----

    /// Home the cursor onto the first enabled row of the current active tab and reconcile scroll.
    /// Call this when the menu opens (or the enabled-set changes) so the initial selection isn't a
    /// dead, disabled row — the analogue of `Menu::home`.
    pub fn focus(&mut self, view: &View) {
        self.sanitize(view);
        if let Some(tab) = view.tabs.get(self.active_tab) {
            self.home_selection(tab);
        }
    }

    /// Dispatch one input event against `view`, mutating state and returning the [`Action`] it
    /// meant (or `None` for pure movement/scroll). A modal, if open, captures the event first.
    pub fn handle(&mut self, event: InputEvent, view: &View) -> Option<Action> {
        self.sanitize(view);

        if self.in_modal() {
            return self.handle_modal(event);
        }

        // Cancel resolves even with no tabs (it closes the menu).
        if event == InputEvent::Cancel {
            return Some(Action::Cancelled);
        }
        if view.tabs.is_empty() {
            return None;
        }

        match event {
            InputEvent::Cancel => unreachable!("handled above"),
            InputEvent::Activate => self.activate(view),
            InputEvent::Left => self.adjust(view, -1),
            InputEvent::Right => self.adjust(view, 1),
            InputEvent::NextTab => self.switch_tab(view, true),
            InputEvent::PrevTab => self.switch_tab(view, false),
            InputEvent::Up => {
                self.move_or_scroll(view, -1);
                None
            }
            InputEvent::Down => {
                self.move_or_scroll(view, 1);
                None
            }
            InputEvent::PageUp => {
                self.page(view, -1);
                None
            }
            InputEvent::PageDown => {
                self.page(view, 1);
                None
            }
            InputEvent::Home => {
                self.home_end(view, true);
                None
            }
            InputEvent::End => {
                self.home_end(view, false);
                None
            }
        }
    }

    // ----- modal input -----

    fn handle_modal(&mut self, event: InputEvent) -> Option<Action> {
        match event {
            InputEvent::Up | InputEvent::PageUp => {
                if let Some(m) = self.modals.last_mut() {
                    m.step(false);
                }
                None
            }
            InputEvent::Down | InputEvent::PageDown => {
                if let Some(m) = self.modals.last_mut() {
                    m.step(true);
                }
                None
            }
            InputEvent::Activate => {
                let top = self.modals.last()?;
                let choice = top.selected;
                let enabled = top.options.get(choice).copied().unwrap_or(false);
                if enabled {
                    self.modals.pop();
                    Some(Action::ModalResolved { choice })
                } else {
                    None // activating a disabled option does nothing
                }
            }
            InputEvent::Cancel => {
                self.modals.pop();
                Some(Action::Cancelled)
            }
            // Everything else (Left/Right/tab switches/Home/End) is captured and ignored.
            _ => None,
        }
    }

    // ----- non-modal handlers -----

    fn activate(&self, view: &View) -> Option<Action> {
        let item = view.tabs[self.active_tab].items.get(self.selected)?;
        item.enabled.then_some(Action::Activated { index: self.selected })
    }

    fn adjust(&self, view: &View, delta: i32) -> Option<Action> {
        let item = view.tabs[self.active_tab].items.get(self.selected)?;
        (item.enabled && item.adjustable).then_some(Action::Adjusted { index: self.selected, delta })
    }

    fn switch_tab(&mut self, view: &View, forward: bool) -> Option<Action> {
        let n = view.tabs.len();
        if n <= 1 {
            return None;
        }
        let new = step_enabled(self.active_tab, n, forward, |_| true);
        self.active_tab = new;
        self.home_selection(&view.tabs[new]);
        Some(Action::TabChanged { tab: new })
    }

    fn move_or_scroll(&mut self, view: &View, dir: i32) {
        let tab = view.tabs[self.active_tab];
        if tab.items.is_empty() {
            return;
        }
        if tab.has_selectable() {
            self.selected = step_enabled(self.selected, tab.items.len(), dir > 0, |i| tab.items[i].enabled);
            self.reconcile_scroll(&tab);
        } else {
            self.scroll = add_clamped(self.scroll, dir as isize, tab.max_scroll());
        }
    }

    fn page(&mut self, view: &View, dir: i32) {
        let tab = view.tabs[self.active_tab];
        let n = tab.items.len();
        if n == 0 {
            return;
        }
        // A page is one viewport; for a non-scrolling tab the whole list is the "viewport", so
        // PageUp/PageDown jump to the ends rather than nudging a single row.
        let vp = if tab.viewport_rows == 0 { n } else { tab.viewport_rows }.max(1) as isize;
        if tab.has_selectable() {
            let target = (self.selected as isize + dir as isize * vp).clamp(0, n as isize - 1) as usize;
            self.selected = snap_enabled(&tab, target, dir);
            self.reconcile_scroll(&tab);
        } else {
            self.scroll = add_clamped(self.scroll, dir as isize * vp, tab.max_scroll());
        }
    }

    fn home_end(&mut self, view: &View, start: bool) {
        let tab = view.tabs[self.active_tab];
        if tab.items.is_empty() {
            self.scroll = if start { 0 } else { tab.max_scroll() };
            return;
        }
        if tab.has_selectable() {
            self.selected = if start {
                first_enabled(tab.items.len(), |i| tab.items[i].enabled)
            } else {
                last_enabled(tab.items.len(), |i| tab.items[i].enabled)
            };
            self.reconcile_scroll(&tab);
        } else {
            self.scroll = if start { 0 } else { tab.max_scroll() };
        }
    }

    // ----- shared helpers -----

    /// Home the cursor onto the first enabled row of `tab` and reset scroll to show it.
    fn home_selection(&mut self, tab: &Tab) {
        self.selected = first_enabled(tab.items.len(), |i| tab.items[i].enabled);
        self.scroll = 0;
        self.reconcile_scroll(tab);
    }

    /// Pull the scroll offset so the selected row is visible, then clamp to the tab's range.
    fn reconcile_scroll(&mut self, tab: &Tab) {
        let vp = tab.viewport_rows;
        if vp == 0 {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + vp {
            self.scroll = self.selected + 1 - vp;
        }
        self.scroll = self.scroll.min(tab.max_scroll());
    }

    /// Clamp the cursor/scroll to be valid for `view`, self-healing across a view that shrank or
    /// whose active tab vanished. Never force-moves off an in-range-but-disabled row. Index and
    /// scroll are clamped independently — a stale cursor can momentarily sit outside the viewport,
    /// which the next movement event's `reconcile_scroll` corrects; we don't reconcile here so the
    /// underlying scroll stays untouched while a modal has frozen it.
    fn sanitize(&mut self, view: &View) {
        if view.tabs.is_empty() {
            self.active_tab = 0;
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.active_tab = self.active_tab.min(view.tabs.len() - 1);
        let tab = view.tabs[self.active_tab];
        self.selected = if tab.items.is_empty() { 0 } else { self.selected.min(tab.items.len() - 1) };
        self.scroll = self.scroll.min(tab.max_scroll());
    }
}

/// The last index in `0..total` for which `enabled` is true, or `0` if none. The `End`/last-row
/// mirror of [`first_enabled`].
fn last_enabled(total: usize, enabled: impl Fn(usize) -> bool) -> usize {
    (0..total).rev().find(|&i| enabled(i)).unwrap_or(0)
}

/// Snap `target` onto an enabled row without wrapping: `target` itself if enabled, else the nearest
/// enabled row scanning first in `dir`, then the other way. Returns `target` if the tab has no
/// enabled rows. Used by paging, which clamps rather than wraps.
fn snap_enabled(tab: &Tab, target: usize, dir: i32) -> usize {
    let items = tab.items;
    if items.is_empty() || items[target].enabled {
        return target;
    }
    let n = items.len() as isize;
    let primary = if dir >= 0 { 1 } else { -1 };
    for step in [primary, -primary] {
        let mut i = target as isize + step;
        while (0..n).contains(&i) {
            if items[i as usize].enabled {
                return i as usize;
            }
            i += step;
        }
    }
    target
}

/// `v + delta`, clamped to `0..=max`.
fn add_clamped(v: usize, delta: isize, max: usize) -> usize {
    (v as isize + delta).clamp(0, max as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::input::view::Item;

    /// Build a `View` from one tab's items (no scrolling) and run a closure with it.
    fn with_tab<R>(items: &[Item], f: impl FnOnce(&View) -> R) -> R {
        let tabs = [Tab::new(items)];
        f(&View { tabs: &tabs })
    }

    /// Build a single-tab scrolling view.
    fn with_scroll<R>(items: &[Item], vp: usize, f: impl FnOnce(&View) -> R) -> R {
        let tabs = [Tab::scrolling(items, vp)];
        f(&View { tabs: &tabs })
    }

    // ---- selection: skip disabled + wrap ----

    #[test]
    fn down_skips_disabled_then_wraps() {
        // rows: 0 enabled, 1 disabled, 2 enabled. Down from 0 -> 2 (skip 1), Down again wraps -> 0.
        let items = [Item::action(true), Item::disabled(), Item::action(true)];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!(nav.selected(), 0);
            assert_eq!(nav.handle(InputEvent::Down, view), None);
            assert_eq!(nav.selected(), 2, "Down skips the disabled row 1");
            assert_eq!(nav.handle(InputEvent::Down, view), None);
            assert_eq!(nav.selected(), 0, "Down wraps from the last enabled row to the first");
            assert_eq!(nav.handle(InputEvent::Up, view), None);
            assert_eq!(nav.selected(), 2, "Up wraps backward, skipping the disabled row");
        });
    }

    #[test]
    fn focus_homes_onto_first_enabled() {
        // Row 0 disabled, row 1 enabled: focus() must not leave the cursor on the dead row 0.
        let items = [Item::disabled(), Item::action(true), Item::action(true)];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!(nav.selected(), 1);
        });
    }

    // ---- empty + all-disabled don't panic or select an invalid index ----

    #[test]
    fn empty_list_is_inert() {
        with_tab(&[], |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            for ev in [InputEvent::Down, InputEvent::Up, InputEvent::Left, InputEvent::Right, InputEvent::PageDown, InputEvent::Home, InputEvent::End] {
                assert_eq!(nav.handle(ev, view), None);
            }
            assert_eq!(nav.selected(), 0);
            assert_eq!(nav.handle(InputEvent::Activate, view), None, "nothing to activate");
        });
    }

    #[test]
    fn all_disabled_list_never_activates() {
        let items = [Item::disabled(), Item::disabled(), Item::disabled()];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!(nav.selected(), 0, "first_enabled falls back to 0 when nothing is enabled");
            assert_eq!(nav.handle(InputEvent::Down, view), None);
            assert_eq!(nav.selected(), 0, "Down can't move when every row is disabled");
            assert_eq!(nav.handle(InputEvent::Activate, view), None, "the disabled row 0 doesn't activate");
        });
    }

    #[test]
    fn empty_view_only_cancel_resolves() {
        let view = View { tabs: &[] };
        let mut nav = Navigator::new();
        nav.focus(&view);
        assert_eq!(nav.handle(InputEvent::Down, &view), None);
        assert_eq!(nav.handle(InputEvent::Activate, &view), None);
        assert_eq!(nav.handle(InputEvent::Cancel, &view), Some(Action::Cancelled), "Cancel still closes the menu");
    }

    // ---- activate ----

    #[test]
    fn activate_fires_enabled_row() {
        // Row 0 enabled, row 1 disabled. The cursor homes onto 0; Activate fires it. Navigation
        // can never land on the disabled row 1 (only row 0 is enabled), so Activate always targets
        // an enabled row.
        let items = [Item::action(true), Item::action(false)];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!(nav.selected(), 0);
            assert_eq!(nav.handle(InputEvent::Activate, view), Some(Action::Activated { index: 0 }));
            // Down can't reach the disabled row 1, so the cursor stays on 0.
            nav.handle(InputEvent::Down, view);
            assert_eq!(nav.selected(), 0);
        });
    }

    // ---- adjust (range rows) ----

    #[test]
    fn left_right_adjusts_a_range_row_only() {
        // row 0: range, row 1: plain action.
        let items = [Item::range(true), Item::action(true)];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!(nav.handle(InputEvent::Right, view), Some(Action::Adjusted { index: 0, delta: 1 }));
            assert_eq!(nav.handle(InputEvent::Left, view), Some(Action::Adjusted { index: 0, delta: -1 }));
            // Move to the action row: Left/Right do nothing there.
            assert_eq!(nav.handle(InputEvent::Down, view), None);
            assert_eq!(nav.selected(), 1);
            assert_eq!(nav.handle(InputEvent::Right, view), None, "Left/Right are no-ops on a non-adjustable row");
            // ...but Activate fires it.
            assert_eq!(nav.handle(InputEvent::Activate, view), Some(Action::Activated { index: 1 }));
        });
    }

    #[test]
    fn disabled_range_does_not_adjust() {
        let items = [Item::range(false)];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!(nav.handle(InputEvent::Right, view), None);
        });
    }

    // ---- scroll: selecting into an offscreen row scrolls ----

    #[test]
    fn moving_into_offscreen_row_scrolls_the_viewport() {
        // 6 rows, viewport shows 3. Scroll should follow the selection down and clamp at the end.
        let items = [Item::action(true); 6];
        with_scroll(&items, 3, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!((nav.selected(), nav.scroll()), (0, 0));
            nav.handle(InputEvent::Down, view); // -> sel 1, still visible (0..3)
            assert_eq!((nav.selected(), nav.scroll()), (1, 0));
            nav.handle(InputEvent::Down, view); // -> sel 2, last visible
            assert_eq!((nav.selected(), nav.scroll()), (2, 0));
            nav.handle(InputEvent::Down, view); // -> sel 3, now offscreen: scroll to 1 (rows 1..4)
            assert_eq!((nav.selected(), nav.scroll()), (3, 1), "selecting an offscreen row scrolls down");
            nav.handle(InputEvent::Down, view);
            nav.handle(InputEvent::Down, view); // -> sel 5 (last), scroll clamps at 3 (rows 3..6)
            assert_eq!((nav.selected(), nav.scroll()), (5, 3), "scroll clamps to content - viewport");
            // Wrap back to top: scroll snaps to 0.
            nav.handle(InputEvent::Down, view);
            assert_eq!((nav.selected(), nav.scroll()), (0, 0), "wrapping to the top resets scroll");
        });
    }

    #[test]
    fn home_end_jump_and_scroll() {
        let items = [Item::action(true); 6];
        with_scroll(&items, 3, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            nav.handle(InputEvent::End, view);
            assert_eq!((nav.selected(), nav.scroll()), (5, 3));
            nav.handle(InputEvent::Home, view);
            assert_eq!((nav.selected(), nav.scroll()), (0, 0));
        });
    }

    #[test]
    fn page_moves_by_a_viewport() {
        let items = [Item::action(true); 10];
        with_scroll(&items, 4, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            nav.handle(InputEvent::PageDown, view); // 0 -> 4
            assert_eq!(nav.selected(), 4);
            nav.handle(InputEvent::PageDown, view); // 4 -> 8
            assert_eq!(nav.selected(), 8);
            nav.handle(InputEvent::PageDown, view); // 8 -> clamps to 9
            assert_eq!(nav.selected(), 9);
            nav.handle(InputEvent::PageUp, view); // 9 -> 5
            assert_eq!(nav.selected(), 5);
        });
    }

    #[test]
    fn page_on_non_scrolling_tab_jumps_to_the_ends() {
        // viewport_rows == 0 (everything visible): a page is the whole list, so PageDown/PageUp
        // land on the last/first enabled row rather than nudging one row.
        let items = [Item::action(true), Item::action(true), Item::disabled(), Item::action(true)];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            nav.handle(InputEvent::PageDown, view);
            assert_eq!(nav.selected(), 3, "PageDown jumps to the last enabled row");
            nav.handle(InputEvent::PageUp, view);
            assert_eq!(nav.selected(), 0, "PageUp jumps back to the first enabled row");
        });
    }

    // ---- scroll-only tab (a log): Up/Down scroll content, no selection ----

    #[test]
    fn scroll_only_tab_scrolls_without_selecting() {
        // All rows disabled => a log: nothing selectable, so Up/Down move the scroll offset.
        let items = [Item::disabled(); 8];
        with_scroll(&items, 3, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            assert_eq!(nav.scroll(), 0);
            nav.handle(InputEvent::Down, view);
            assert_eq!(nav.scroll(), 1, "Down scrolls content one row when nothing is selectable");
            nav.handle(InputEvent::PageDown, view);
            assert_eq!(nav.scroll(), 4, "PageDown scrolls a viewport");
            nav.handle(InputEvent::End, view);
            assert_eq!(nav.scroll(), 5, "End scrolls to the bottom (8 - 3)");
            nav.handle(InputEvent::Home, view);
            assert_eq!(nav.scroll(), 0);
            assert_eq!(nav.selected(), 0, "selection never moves in a scroll-only tab");
        });
    }

    // ---- tabs: NextTab wraps + re-homes selection ----

    #[test]
    fn next_prev_tab_wraps_and_homes_selection() {
        let a = [Item::action(true), Item::action(true)];
        let b = [Item::disabled(), Item::action(true)]; // first enabled is row 1
        let c = [Item::action(true)];
        let tabs = [Tab::new(&a), Tab::new(&b), Tab::new(&c)];
        let view = View { tabs: &tabs };
        let mut nav = Navigator::new();
        nav.focus(&view);
        assert_eq!(nav.active_tab(), 0);

        // Move selection in tab 0, then switch: selection re-homes for the new tab.
        nav.handle(InputEvent::Down, &view);
        assert_eq!(nav.selected(), 1);
        assert_eq!(nav.handle(InputEvent::NextTab, &view), Some(Action::TabChanged { tab: 1 }));
        assert_eq!((nav.active_tab(), nav.selected()), (1, 1), "tab 1 homes onto its first enabled row");

        assert_eq!(nav.handle(InputEvent::NextTab, &view), Some(Action::TabChanged { tab: 2 }));
        assert_eq!(nav.active_tab(), 2);
        assert_eq!(nav.handle(InputEvent::NextTab, &view), Some(Action::TabChanged { tab: 0 }), "NextTab wraps");
        assert_eq!(nav.active_tab(), 0);
        assert_eq!(nav.handle(InputEvent::PrevTab, &view), Some(Action::TabChanged { tab: 2 }), "PrevTab wraps backward");
        assert_eq!(nav.active_tab(), 2);
    }

    #[test]
    fn single_tab_does_not_switch() {
        let a = [Item::action(true)];
        let tabs = [Tab::new(&a)];
        let view = View { tabs: &tabs };
        let mut nav = Navigator::new();
        nav.focus(&view);
        assert_eq!(nav.handle(InputEvent::NextTab, &view), None, "one tab: NextTab is a no-op");
        assert_eq!(nav.active_tab(), 0);
    }

    // ---- modal focus stack ----

    #[test]
    fn modal_captures_input_and_cancel_pops() {
        let items = [Item::action(true), Item::action(true), Item::action(true)];
        with_tab(&items, |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            nav.handle(InputEvent::Down, view); // underlying selection -> 1
            assert_eq!(nav.selected(), 1);

            nav.open_modal(ModalSpec::uniform(2));
            assert!(nav.in_modal());
            assert_eq!(nav.modal_selection(), Some(0));

            // Underlying selection is frozen while the modal is open.
            nav.handle(InputEvent::Down, view);
            assert_eq!(nav.modal_selection(), Some(1), "Down moves the modal cursor");
            assert_eq!(nav.selected(), 1, "the underlying selection stays frozen");

            // Tab/adjust are captured (no-op) while the modal is up.
            assert_eq!(nav.handle(InputEvent::NextTab, view), None);
            assert_eq!(nav.handle(InputEvent::Right, view), None);
            assert_eq!(nav.selected(), 1);

            // Cancel pops the modal and reports Cancelled; underlying state survives.
            assert_eq!(nav.handle(InputEvent::Cancel, view), Some(Action::Cancelled));
            assert!(!nav.in_modal());
            assert_eq!(nav.selected(), 1, "the underlying cursor is right where it was left");
            // Now Down resumes moving the underlying cursor.
            nav.handle(InputEvent::Down, view);
            assert_eq!(nav.selected(), 2);
        });
    }

    #[test]
    fn modal_activate_resolves_with_choice() {
        with_tab(&[Item::action(true)], |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            nav.open_modal(ModalSpec::uniform(3));
            nav.handle(InputEvent::Down, view); // choice 0 -> 1
            assert_eq!(nav.handle(InputEvent::Activate, view), Some(Action::ModalResolved { choice: 1 }));
            assert!(!nav.in_modal());
        });
    }

    #[test]
    fn modal_skips_disabled_options_and_guards_activate() {
        with_tab(&[Item::action(true)], |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            // Option 0 disabled, 1 enabled, 2 disabled: homes onto 1; Down/Up wrap on 1 only.
            nav.open_modal(ModalSpec::new(vec![false, true, false]));
            assert_eq!(nav.modal_selection(), Some(1));
            nav.handle(InputEvent::Down, view);
            assert_eq!(nav.modal_selection(), Some(1), "only option 1 is enabled");
            assert_eq!(nav.handle(InputEvent::Activate, view), Some(Action::ModalResolved { choice: 1 }));
        });
    }

    #[test]
    fn modal_with_no_enabled_option_cannot_activate() {
        with_tab(&[Item::action(true)], |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            nav.open_modal(ModalSpec::new(vec![false, false]));
            assert_eq!(nav.handle(InputEvent::Activate, view), None, "no enabled option to choose");
            assert!(nav.in_modal(), "still open");
            assert_eq!(nav.handle(InputEvent::Cancel, view), Some(Action::Cancelled));
        });
    }

    #[test]
    fn modals_nest_and_pop_in_order() {
        with_tab(&[Item::action(true)], |view| {
            let mut nav = Navigator::new();
            nav.focus(view);
            nav.open_modal(ModalSpec::uniform(2)); // depth 1
            nav.open_modal(ModalSpec::uniform(2)); // depth 2 (topmost)
            assert_eq!(nav.modal_depth(), 2);
            // Activate resolves only the topmost.
            assert_eq!(nav.handle(InputEvent::Activate, view), Some(Action::ModalResolved { choice: 0 }));
            assert_eq!(nav.modal_depth(), 1, "inner modal still open");
            assert_eq!(nav.handle(InputEvent::Cancel, view), Some(Action::Cancelled));
            assert_eq!(nav.modal_depth(), 0);
        });
    }

    // ---- self-healing against a shrinking view ----

    #[test]
    fn sanitize_clamps_a_stale_cursor() {
        let big = [Item::action(true); 5];
        let small = [Item::action(true); 2];
        let tabs_big = [Tab::new(&big)];
        let tabs_small = [Tab::new(&small)];
        let mut nav = Navigator::new();
        nav.focus(&View { tabs: &tabs_big });
        nav.handle(InputEvent::End, &View { tabs: &tabs_big });
        assert_eq!(nav.selected(), 4);
        // The view shrinks under us: the next handle clamps the cursor into range, no panic.
        assert_eq!(nav.handle(InputEvent::Up, &View { tabs: &tabs_small }), None);
        assert!(nav.selected() < 2, "cursor clamped into the smaller list");
    }
}
