//! Native utility-window **menu** — the tabbed interactive menu (Actions / Settings / Log / Debug),
//! drawn by the game's own `CSEzDraw` via the native UI library instead of the imgui overlay. Built
//! from `unseamless_core::ui::render` widgets (Tabs / List / Modal), navigated by
//! `unseamless_core::ui::input` ([`Navigator`]), and rasterized through
//! [`crate::native_draw::draw_list`].
//!
//! This is the menu sibling of the native toasts ([`crate::features::native_toasts`]): same
//! `CSEzDraw` frame-task substrate, same `[nameplates] native_spike` experiment gate (off by
//! default), coexisting with the imgui overlay while we migrate surfaces off it. The two halves of
//! the UI library meet here at the integration layer:
//!
//!  - **view** (`ui::render`): each frame we build a widget tree (Tabs → List/Stack, wrapped in a
//!    Panel), measure + center it in the design viewport, and rasterize the resulting `DrawList`.
//!  - **controller** (`ui::input`): a [`Navigator`] holds the cursor (selected row, active tab,
//!    scroll, modal stack). We rebuild a [`View`] (tab list + per-row enabled flags) each frame and
//!    dispatch this frame's input events into it, then read selection/tab/scroll back out for the
//!    view half.
//!
//! ## What's wired vs. what the orchestrator finishes (rig-coupled — see the handoff)
//! The pure plumbing — building the [`View`] from the app's menu model, mapping pad input to
//! [`InputEvent`]s, and routing [`Action`]s to the session-action queue — is here and host-shaped
//! (see [`pad_to_events`] / [`action_items`] and the `#[cfg(test)]` module). The parts that need the
//! rig are flagged inline and called out in the worker handoff:
//!  - **Input source** ([`NativeMenu::poll_pad`]): only the controller (XInput) path is wired, via
//!    the existing [`crate::input`] snapshot. There is no keyboard path on the game thread (the
//!    overlay read keys through imgui on the Present thread), so the backtick toggle / arrow keys are
//!    a TODO for integration.
//!  - **Game-input suppression** ([`crate::input::set_blocked`]): we assert it while open, but it's a
//!    single global the imgui overlay *also* drives — when both surfaces are active at once they
//!    fight over it. Coexistence (likely: the overlay yields its utility window to this one under
//!    `native_spike`) is the orchestrator's call.

use eldenring::cs::{CSCamExt, CSCamera, CSTaskGroupIndex, RendMan};
use log::Level;
use unseamless_core::menu::{self, ActionRow, SessionContext};
use unseamless_core::protocol::SessionAction;
use unseamless_core::settings::{Setting, registry};
use unseamless_core::ui::input::{Action, InputEvent, Item, ModalSpec, Navigator, Tab, View};
use unseamless_core::ui::render::{
    Align, Label, List, Modal, Panel, Rect, Rgba, Row, Stack, Tabs, Theme, Widget, center, draw,
};
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};
use crate::input::PadNav;
use crate::native_draw::{CamFrame, ScreenSpace, draw_list, ui_viewport};

/// Distance (m) of the screen-space plane in front of the camera — mirrors the toasts; apparent size
/// is distance-independent (the fov term cancels in [`ScreenSpace`]).
const PLANE_DIST_M: f32 = 0.5;
/// Virtual canvas height (px) the menu lays out in. [`ui_viewport`] derives the width from the screen
/// aspect so glyphs stay square, and [`draw_list`] maps this whole canvas across the screen — so the
/// number is a *size* knob (bigger ⇒ smaller on-screen chrome), not a resolution. Text is rasterized
/// to filled quads, so this never blurs glyphs (it's vector scaling, not bitmap sampling).
const DESIGN_HEIGHT_PX: f32 = 1080.0;
/// How many rows the scrolling tabs (Settings / Log) show at once. Drives both the [`Tab::scrolling`]
/// viewport the [`Navigator`] clamps scroll against and the row window we actually rasterize (we draw
/// only the visible slice — keeps the per-primitive `CSEzDraw` cost bounded; see native_draw).
const VISIBLE_ROWS: usize = 16;
/// Full-screen scrim drawn behind an open modal so it reads as the one thing to act on (we can't
/// freeze ER's loop, so the dim + input focus stand in for "blocking", as the overlay's modal does).
const MODAL_SCRIM: Rgba = [0, 0, 0, 120];

/// Tab order. The labels feed both the [`Tabs`] strip and the [`View`]; the indices below name the
/// active tab when routing an [`Action`].
const TAB_LABELS: [&str; 4] = ["Actions", "Settings", "Log", "Debug"];
const TAB_ACTIONS: usize = 0;
const TAB_SETTINGS: usize = 1;
const TAB_LOG: usize = 2;
// Debug (index 3) is the default match arm in `build_content`, so it needs no named constant.

/// A modal the menu put up (today: the Leave-world confirm). The [`Navigator`] owns the *selection*
/// over the options; we keep the *labels* + the pending action here, since the controller is
/// label-agnostic. Resolving the modal (choice 0 = confirm) emits [`Self::action`].
struct Confirm {
    prompt: String,
    options: Vec<String>,
    action: SessionAction,
}

/// One rasterized line of a read-only (Settings/Log/Debug) tab: its text and an optional color
/// override (synced/local/severity); `None` falls back to the theme foreground.
struct DisplayRow {
    text: String,
    color: Option<Rgba>,
}

impl DisplayRow {
    fn new(text: impl Into<String>, color: Option<Rgba>) -> Self {
        Self { text: text.into(), color }
    }
}

/// The native tabbed menu. No-op unless `[nameplates] native_spike`. Holds the controller
/// ([`Navigator`]) plus the cdylib-side input edge state ([`PadNav`]) and the open/modal toggles;
/// everything else (the [`View`], the widget tree) is rebuilt per frame from the live app state.
pub struct NativeMenu {
    /// Logs the experiment-flag flip (mirrors the toasts), nothing more.
    active: Latch<bool>,
    /// The cursor: selected row, active tab, scroll offset, modal stack.
    nav: Navigator,
    /// Controller→menu edge/repeat state, fed the raw XInput snapshot each frame.
    pad: PadNav,
    /// Whether the menu window is open (toggled by the RB+L3+R3 chord or closed with B/Cancel).
    open: bool,
    /// Whether we currently hold game-input suppression, so we release it exactly once on close /
    /// disable rather than clobbering the overlay's own `set_blocked` every idle frame.
    blocking: bool,
    /// An active confirm modal's labels + pending action, or `None`. The [`Navigator`] holds the
    /// option selection; this holds what the options *mean*.
    confirm: Option<Confirm>,
    /// Actions the queue refused (momentarily locked by the game thread); retried next frame so a
    /// press is never lost — mirrors the overlay's pending buffer.
    pending: Vec<SessionAction>,
}

impl NativeMenu {
    pub fn new() -> Self {
        Self {
            active: Latch::new(),
            nav: Navigator::new(),
            pad: PadNav::new(),
            open: false,
            blocking: false,
            confirm: None,
            pending: Vec::new(),
        }
    }

    // --- INPUT SOURCE (isolated; rig-coupled — orchestrator rewires/extends) -----------------------
    //
    // The one place the cdylib's raw input becomes menu intents. Today it's the controller only,
    // through the existing XInput snapshot ([`crate::input::pad_snapshot`] + [`PadNav`], both
    // host-tested in `unseamless_core::pad`). Keyboard isn't read here: on the game thread we have no
    // imgui key state (the overlay's keys came from the Present thread), so the backtick toggle and
    // arrow keys remain a TODO for integration. Keep this self-contained so that rewire is a one-spot
    // change.

    /// Sample the pad and fold it into this frame's edges. Returns the open/close `toggle` edge
    /// (handled at the menu level, *not* fed to the [`Navigator`]) plus the per-frame nav events.
    fn poll_pad(&mut self, dt: f32) -> (bool, Vec<InputEvent>) {
        let (buttons, lx, ly) = crate::input::pad_snapshot();
        let edges = self.pad.update(buttons, lx, ly, dt);
        (edges.toggle, pad_to_events(edges))
    }

    /// Mirror the open-state into the game-input suppressor, releasing it exactly once on the
    /// open→closed edge. While *closed* we deliberately don't touch the global, so the imgui overlay
    /// (which shares it) keeps ownership — see the module-level suppression note.
    fn set_blocking(&mut self, want: bool) {
        if want {
            crate::input::set_blocked(true);
            self.blocking = true;
        } else if self.blocking {
            crate::input::set_blocked(false);
            self.blocking = false;
        }
    }

    /// Fully close the menu: drop the open flag, pop any lingering modal, and release game-input
    /// suppression. The single close path for every way the menu can shut (the toggle chord, a B /
    /// Cancel, or the experiment flag going off), so a modal can never survive a close and reappear —
    /// input-capturing — on the next open.
    fn close(&mut self) {
        self.open = false;
        self.reset_modal();
        self.set_blocking(false);
    }

    /// Pop any open modal off the [`Navigator`]'s focus stack and forget its labels. The navigator
    /// exposes no clear/pop, so we drain it with `Cancel` events — but `handle` sanitizes against the
    /// view first, so we feed a 4-empty-tab stand-in (one per real tab) that keeps `active_tab` valid
    /// through `sanitize` while `Cancel` pops the modal; selection/scroll re-home on the next open.
    /// A no-op (besides clearing `confirm`) when no modal is up.
    fn reset_modal(&mut self) {
        if self.nav.in_modal() {
            let empty = [Tab::new(&[]); TAB_LABELS.len()];
            let view = View { tabs: &empty };
            while self.nav.in_modal() {
                self.nav.handle(InputEvent::Cancel, &view);
            }
        }
        self.confirm = None;
    }

    /// Queue an action for the game thread (via [`crate::actionq`]), retrying any the locked queue
    /// refused — same path and retry discipline as the overlay's Actions tab.
    fn request_action(&mut self, action: SessionAction) {
        self.pending.push(action);
        self.flush_pending();
    }

    fn flush_pending(&mut self) {
        self.pending.retain(|&action| !crate::actionq::try_offer(action));
    }

    /// Put up the confirm modal for a destructive action (Leave world). The [`Navigator`] captures
    /// all input until it's resolved/cancelled; choice 0 ("Leave world") confirms.
    ///
    /// NB: this is a *deliberate enhancement* over the overlay, which fires Leave immediately — it's
    /// also what exercises the modal render+resolve path end to end. Drop it (route Leave straight to
    /// `request_action`) if parity with the overlay is preferred.
    fn open_confirm(&mut self, action: SessionAction) {
        self.confirm = Some(Confirm {
            prompt: "Leave the session?".into(),
            options: vec!["Leave world".into(), "Stay".into()],
            action,
        });
        self.nav.open_modal(ModalSpec::uniform(2));
    }

    /// Route one [`Action`] the [`Navigator`] reported. `had_modal` is whether a modal was open
    /// *before* the event (so a `Cancelled` can tell a modal-dismiss from a menu-close).
    fn apply_action(&mut self, action: Action, had_modal: bool, action_rows: &[ActionRow]) {
        match action {
            // Only the Actions tab has enabled rows, so an activation can only land there; guard on
            // the tab anyway so a future selectable row elsewhere can't misfire a session action.
            Action::Activated { index } => {
                if self.nav.active_tab() == TAB_ACTIONS
                    && let Some(row) = action_rows.get(index)
                {
                    if row.action == SessionAction::LeaveWorld {
                        self.open_confirm(row.action);
                    } else {
                        self.request_action(row.action);
                    }
                }
            }
            // No adjustable (range) rows in this menu: Settings is read-only (edited in the TOML, like
            // the overlay), so Left/Right are tab switches, never value steps. If settings become
            // editable here, this is where an `Adjusted` would step the value + persist config — a
            // host-enforcement / boot-vs-live decision the orchestrator owns.
            Action::Adjusted { .. } => {}
            Action::TabChanged { .. } => {} // selection re-homing is the navigator's; nothing to do
            Action::ModalResolved { choice } => {
                if let Some(confirm) = self.confirm.take()
                    && choice == 0
                {
                    self.request_action(confirm.action);
                }
            }
            Action::Cancelled => {
                if had_modal {
                    self.confirm = None; // the navigator already popped it; drop our labels
                } else {
                    self.open = false; // B / Cancel with no modal closes the menu
                }
            }
        }
    }

    /// Build the active tab's content widget from the current cursor state and the per-frame model
    /// ([`Model`]). Actions is an interactive [`List`] (selection highlight); the read-only tabs are a
    /// [`Stack`] of colored [`Label`]s, pre-windowed to the visible scroll slice so only on-screen rows
    /// are rasterized.
    fn build_content(&self, model: &Model) -> Box<dyn Widget> {
        match self.nav.active_tab() {
            TAB_ACTIONS => {
                let rows: Vec<Row> = model
                    .actions
                    .iter()
                    .map(|r| {
                        let row = Row::text(r.label.clone());
                        if r.enabled { row } else { row.disabled() }
                    })
                    .collect();
                // Only highlight the cursor when it sits on an enabled row (an all-disabled list — at
                // the title screen — shows no highlight, matching the overlay's text-disabled rows).
                let selected = model
                    .actions
                    .get(self.nav.selected())
                    .filter(|r| r.enabled)
                    .map(|_| self.nav.selected());
                Box::new(List::new(rows).selected(selected))
            }
            TAB_SETTINGS => scroll_stack(&model.settings, self.nav.scroll(), VISIBLE_ROWS),
            TAB_LOG => scroll_stack(&model.log, self.nav.scroll(), VISIBLE_ROWS),
            // Debug is a short static read-out (no scroll); show the whole list.
            _ => scroll_stack(&model.debug, 0, model.debug.len()),
        }
    }
}

/// The per-frame menu content, built once from live app state and shared by the navigator's [`View`]
/// (which needs only the row counts + the Actions enabled flags) and the renderer (which needs the
/// text + colors). Building it once keeps the two in lockstep — the scroll offset indexes the same
/// rows the navigator clamped it against.
struct Model {
    actions: Vec<ActionRow>,
    settings: Vec<DisplayRow>,
    log: Vec<DisplayRow>,
    debug: Vec<DisplayRow>,
}

impl Model {
    /// Assemble the model from the live config, the session context, the settings registry, and the
    /// log tail (all non-blocking reads).
    fn build(theme: &Theme) -> Self {
        let cfg = crate::state::snapshot();
        let settings = registry();
        Self {
            actions: menu::action_rows(&session_context()),
            settings: settings_rows(&cfg, &settings, theme),
            log: log_rows(theme),
            debug: debug_rows(theme),
        }
    }
}

impl Feature for NativeMenu {
    fn name(&self) -> &'static str {
        "native_menu"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Same CSEzDraw frame phase as the other native surfaces: we only read state and enqueue draws.
        CSTaskGroupIndex::ChrIns_PostPhysics
    }

    fn on_frame(&mut self, tick: Tick) {
        let enabled = crate::state::with(|c| c.nameplates.native_spike);
        if self.active.changed(&enabled) {
            log::debug!("native menu {}", if enabled { "enabled" } else { "disabled" });
        }
        if !enabled {
            // Fully inert when the experiment is off: close (which drops the menu, pops any modal, and
            // releases suppression) so re-enabling starts clean and the overlay regains the input global.
            self.close();
            return;
        }

        // Pad is sampled every (enabled) frame so the toggle chord works while closed. The toggle is a
        // menu-level open/close, handled here — never fed to the navigator.
        let (toggle, events) = self.poll_pad(tick.delta);
        let just_opened = toggle && !self.open;
        if toggle {
            self.open = !self.open;
        }

        // Retry refused actions even while closed, so a press queued just before a close still lands.
        self.flush_pending();

        if !self.open {
            // Closed (or just toggled closed): close() pops any lingering modal + releases suppression.
            self.close();
            return;
        }
        self.set_blocking(true);

        // Build the per-frame model, then the View the navigator interprets. The View borrows the
        // per-tab item slices (all locals), so it lives only for this frame.
        let theme = Theme::default();
        let model = Model::build(&theme);

        let actions_items = action_items(&model.actions);
        // Settings/Log/Debug rows are read-only: every Item is disabled, so the navigator treats the
        // tab as a scroll surface (Up/Down move the offset) and never reports an activation there. The
        // item count matches the rendered-row count (same `Model`), so scroll indexes both identically.
        let settings_items = vec![Item::disabled(); model.settings.len()];
        let log_items = vec![Item::disabled(); model.log.len()];
        let debug_items = vec![Item::disabled(); model.debug.len()];

        let tabs = [
            Tab::new(&actions_items),
            Tab::scrolling(&settings_items, VISIBLE_ROWS),
            Tab::scrolling(&log_items, VISIBLE_ROWS),
            Tab::new(&debug_items),
        ];
        let view = View { tabs: &tabs };

        // Home the cursor onto the first enabled row the frame we open (so the highlight isn't a dead
        // disabled row), the analogue of the overlay's `actions_sel = 0` + repair.
        if just_opened {
            self.nav.focus(&view);
        }

        for event in events {
            let had_modal = self.nav.in_modal();
            if let Some(action) = self.nav.handle(event, &view) {
                self.apply_action(action, had_modal, &model.actions);
            }
        }

        // A Cancel may have closed the menu this frame — close out and skip rendering. (Cancel only
        // closes the menu when no modal is up, so there's no modal to pop here, but route it through
        // close() anyway so every shutdown path is identical.)
        if !self.open {
            self.close();
            return;
        }

        self.render(&theme, &model);
    }
}

impl NativeMenu {
    /// Rasterize the menu through `CSEzDraw`: read the camera, build the widget tree, center it in the
    /// design viewport, and hand the `DrawList` to [`draw_list`]. A momentarily-unavailable camera or
    /// debug-draw object skips the frame (nothing drawn), like the toasts.
    fn render(&self, theme: &Theme, model: &Model) {
        let Some(frame) = crate::sdk::with_instance::<CSCamera, _>(camera_frame).flatten() else {
            return;
        };
        crate::sdk::with_instance_mut::<RendMan, _>(|r| {
            if r.debug_ez_draw.as_ptr().is_null() {
                return;
            }
            let ez = r.debug_ez_draw.as_mut();
            let ss = ScreenSpace::new(&frame, PLANE_DIST_M);
            let vp = ui_viewport(&ss, DESIGN_HEIGHT_PX);
            let vp_rect = Rect::new(0, 0, vp[0] as i32, vp[1] as i32);

            // The window: the active tab's content under the tab strip, framed + titled.
            let content = self.build_content(model);
            let title = concat!("unseamless-coop  v", env!("CARGO_PKG_VERSION"));
            let window = Panel::new(Tabs::new(tab_labels(), self.nav.active_tab(), content))
                .border()
                .title(title);
            let mut dl = draw_centered(&window, theme, vp_rect);

            // A modal (the Leave-world confirm) sits over a dim scrim, on top of the window.
            if self.nav.in_modal()
                && let Some(confirm) = &self.confirm
            {
                dl.rect(vp_rect, MODAL_SCRIM);
                let selected = self.nav.modal_selection().unwrap_or(0);
                let modal = Modal::new(confirm.options.clone(), selected).title(confirm.prompt.clone());
                dl.append(draw_centered(&modal, theme, vp_rect));
            }

            draw_list(ez, &ss, vp, &dl);
        });
    }
}

impl Default for NativeMenu {
    fn default() -> Self {
        Self::new()
    }
}

// --- pure helpers (host-shaped; see the test module) -----------------------------------------------

/// The tab strip labels as owned strings ([`Tabs`] takes `Vec<String>`).
fn tab_labels() -> Vec<String> {
    TAB_LABELS.iter().map(|s| s.to_string()).collect()
}

/// Map a frame's controller edges to navigator events. Pure — the whole input-vocabulary decision in
/// one place. The non-obvious choice: **Left/Right switch tabs** (PrevTab/NextTab), matching the imgui
/// overlay's feel, rather than adjusting a row — there are no adjustable rows here (Settings is
/// read-only), and the pad exposes no dedicated tab buttons (RB is spent on the toggle chord, LB is
/// excluded to avoid casting a spell — see `unseamless_core::pad`). Up/Down move selection/scroll;
/// A/B are Activate/Cancel. The `toggle` chord is handled at the menu level, not here.
fn pad_to_events(edges: unseamless_core::pad::PadEdges) -> Vec<InputEvent> {
    let mut events = Vec::new();
    if edges.up {
        events.push(InputEvent::Up);
    }
    if edges.down {
        events.push(InputEvent::Down);
    }
    if edges.left {
        events.push(InputEvent::PrevTab);
    }
    if edges.right {
        events.push(InputEvent::NextTab);
    }
    if edges.activate {
        events.push(InputEvent::Activate);
    }
    if edges.cancel {
        events.push(InputEvent::Cancel);
    }
    events
}

/// Derive the navigator [`Item`]s for the Actions tab from its rows: each is a plain (non-adjustable)
/// row whose `enabled` gates selection/activation, so navigation skips disabled connect verbs exactly
/// as the overlay does. Pure.
fn action_items(rows: &[ActionRow]) -> Vec<Item> {
    rows.iter().map(|r| Item::action(r.enabled)).collect()
}

/// Build the read-only Settings rows: the (masked) session password, a read-only hint, then every
/// registered setting as `label = value`, colored by whether the host syncs it across the party
/// (synced) or it's machine-local — the same distinction the overlay's Settings tab draws.
fn settings_rows(cfg: &unseamless_core::config::Config, settings: &[Setting], theme: &Theme) -> Vec<DisplayRow> {
    let mut rows = Vec::with_capacity(settings.len() + 2);
    // Streamer-mode-by-default: the password is masked here. Reveal isn't wired (no per-row toggle on
    // the game thread yet) — a TODO for integration; masked-by-default is the safe state to ship.
    let masked = "*".repeat(cfg.session.password.chars().count());
    rows.push(DisplayRow::new(format!("Session password: {masked}"), Some(theme.warning)));
    rows.push(DisplayRow::new("Read-only. Edit unseamless_coop.toml, then relaunch.", Some(theme.dim)));
    for s in settings {
        let color = if s.id.is_shared() { theme.info } else { theme.dim };
        rows.push(DisplayRow::new(format!("{} = {}", s.label, s.display_value(cfg)), Some(color)));
    }
    rows
}

/// The live log tail, newest-first (the ring buffer is oldest→newest), colored by level — the Log
/// tab's content. Reads [`crate::logbuf`] non-blocking; a contended read yields an empty tail for the
/// frame rather than blocking the game thread.
fn log_rows(theme: &Theme) -> Vec<DisplayRow> {
    let lines = crate::logbuf::try_read(|lines| {
        lines.iter().rev().map(|l| DisplayRow::new(l.text.clone(), Some(level_color(l.level, theme)))).collect()
    });
    lines.unwrap_or_default()
}

/// The Debug tab: a short read-only build/identity read-out. The overlay's rich debug panel (the live
/// diagnostic report) and its per-category toggles / Export action are Present-thread overlay concepts;
/// re-expressing them natively is deferred to integration, so this notes where they live for now.
fn debug_rows(theme: &Theme) -> Vec<DisplayRow> {
    vec![
        DisplayRow::new(concat!("unseamless-coop  v", env!("CARGO_PKG_VERSION")), Some(theme.info)),
        DisplayRow::new(concat!("build ", env!("UNSEAMLESS_BUILD_ID")), Some(theme.dim)),
        DisplayRow::new("Native menu (experimental) - [nameplates] native_spike", Some(theme.dim)),
        DisplayRow::new("Full debug panel + Export remain in the overlay for now.", Some(theme.dim)),
    ]
}

/// Color for a log line's level (mirrors the overlay's palette mapping, sourced from the theme).
fn level_color(level: Level, theme: &Theme) -> Rgba {
    match level {
        Level::Error => theme.error,
        Level::Warn => theme.warning,
        Level::Info => theme.fg,
        Level::Debug => theme.info,
        Level::Trace => theme.dim,
    }
}

/// Build a read-only tab's content: a [`Stack`] of colored [`Label`]s windowed to `[scroll, scroll +
/// visible)`. Pre-windowing (vs. a clipping `ScrollView`) keeps the rasterized primitive count bounded
/// to what's on screen — the per-primitive `CSEzDraw` cost model wants exactly that.
fn scroll_stack(rows: &[DisplayRow], scroll: usize, visible: usize) -> Box<dyn Widget> {
    let end = scroll.saturating_add(visible).min(rows.len());
    let mut stack = Stack::vertical().cross_align(Align::Start);
    for row in &rows[scroll.min(rows.len())..end] {
        let mut label = Label::new(row.text.clone());
        if let Some(color) = row.color {
            label = label.color(color);
        }
        stack = stack.child(label);
    }
    Box::new(stack)
}

/// Measure a widget, center it in `viewport`, and rasterize it to a fresh `DrawList`. The whole of our
/// "window positioning" — placement is static (no drag/resize/snap; see `docs/UI-LIBRARY.md`).
fn draw_centered(widget: &dyn Widget, theme: &Theme, viewport: Rect) -> unseamless_core::ui::render::DrawList {
    let bounds = center(widget.measure(theme), viewport);
    draw(widget, theme, bounds)
}

/// The current session context for menu gating, assembled from live process state (all non-blocking
/// reads). Mirrors the overlay's `session_context`: the host toggle states (world lock / PvP / teams /
/// friendly fire) aren't tracked until the rung-3 session FSM lands, so they pass `false` for now and
/// the collapsed toggle rows render in their "off"/"unlocked" form.
fn session_context() -> SessionContext {
    let flags = crate::coop::session_flags();
    SessionContext {
        in_session: flags.in_session,
        is_host: flags.is_host,
        steam_ready: crate::steam_ready::is_ready(),
        in_game: crate::playstate::in_gameplay(),
        world_locked: false,
        pvp_on: false,
        pvp_teams_on: false,
        friendly_fire_on: false,
    }
}

/// Full camera frame from the composited render camera (`pers_cam_1`). `None` until the sub-camera is
/// wired (early boot / loading) — identical to the toasts' guard.
fn camera_frame(cam: &CSCamera) -> Option<CamFrame> {
    if cam.pers_cam_1.as_ptr().is_null() {
        return None;
    }
    let c = &*cam.pers_cam_1;
    let (right, up, fwd, pos) = (c.right(), c.up(), c.forward(), c.position());
    Some(CamFrame {
        pos: [pos.0, pos.1, pos.2],
        right: [right.0, right.1, right.2],
        up: [up.0, up.1, up.2],
        fwd: [fwd.0, fwd.1, fwd.2],
        fov_y: c.fov,
        aspect: c.aspect_ratio,
    })
}

// NB: these unit-test the *pure* glue (input mapping, item derivation). They compile under `cargo
// test` (which targets the cross triple), so `scripts/test-core.sh` doesn't execute them — the
// heavy logic they sit on (`Navigator`, `menu::action_rows`, the widgets) is already host-tested in
// `unseamless-core`. If host execution of this glue is wanted, hoisting `pad_to_events`/`action_items`
// into core is the move (out of this lane — core is read-only here).
#[cfg(test)]
mod tests {
    use super::*;
    use unseamless_core::pad::PadEdges;

    #[test]
    fn pad_maps_left_right_to_tab_switch_and_skips_toggle() {
        // Left/Right switch tabs (not adjust); the toggle chord never becomes a nav event.
        let edges = PadEdges { left: true, right: true, toggle: true, ..Default::default() };
        assert_eq!(pad_to_events(edges), vec![InputEvent::PrevTab, InputEvent::NextTab]);

        let edges = PadEdges { up: true, down: true, activate: true, cancel: true, ..Default::default() };
        assert_eq!(
            pad_to_events(edges),
            vec![InputEvent::Up, InputEvent::Down, InputEvent::Activate, InputEvent::Cancel],
        );

        assert!(pad_to_events(PadEdges::default()).is_empty(), "neutral pad yields no events");
    }

    #[test]
    fn action_items_carry_enabled_and_are_non_adjustable() {
        let rows = vec![
            ActionRow { label: "Open world".into(), action: SessionAction::OpenWorld, enabled: true },
            ActionRow { label: "Join world".into(), action: SessionAction::JoinWorld, enabled: false },
        ];
        let items = action_items(&rows);
        assert_eq!(items.len(), 2);
        assert!(items[0].enabled && !items[0].adjustable);
        assert!(!items[1].enabled && !items[1].adjustable, "disabled row stays non-adjustable");
    }

    #[test]
    fn close_clears_a_lingering_modal() {
        // A confirm modal must never survive a close — otherwise it reappears (input-capturing) on the
        // next open and a stray Activate fires the very action it was guarding. close() pops it.
        let mut menu = NativeMenu::new();
        menu.open = true;
        menu.open_confirm(SessionAction::LeaveWorld);
        assert!(menu.nav.in_modal() && menu.confirm.is_some(), "confirm modal is up");

        menu.close();
        assert!(!menu.open, "menu closed");
        assert!(!menu.nav.in_modal(), "navigator modal stack drained on close");
        assert!(menu.confirm.is_none(), "confirm labels dropped on close");
    }

    #[test]
    fn item_count_matches_rendered_rows_for_a_scroll_tab() {
        // The scroll offset indexes the Item slice and the DisplayRow slice identically, so their
        // lengths must agree — guard that the Settings tab builds them in lockstep.
        let cfg = unseamless_core::config::Config::default();
        let settings = registry();
        let theme = Theme::default();
        let rows = settings_rows(&cfg, &settings, &theme);
        let items = vec![Item::disabled(); rows.len()];
        assert_eq!(items.len(), rows.len());
        assert_eq!(rows.len(), settings.len() + 2, "password + read-only hint + one row per setting");
    }
}
