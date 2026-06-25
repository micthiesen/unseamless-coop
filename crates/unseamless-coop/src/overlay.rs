//! In-game **overlay** — hudhook's DX12 present-hook driving imgui.
//!
//! Renders two surfaces:
//!  - **Notifications** (always, passive): the banners + toasts from [`crate::notify`], in a
//!    borderless, input-transparent corner window.
//!  - **Utility window** (toggle with backtick): a movable, titled window with tabs — an interactive
//!    **session-action menu** ([`unseamless_core::menu`]), a **read-only settings** view (synced vs
//!    local, coloured), and a live **log** tail ([`crate::logbuf`]). Settings are intentionally not
//!    editable here (boot-vs-live + host-enforcement questions); they're edited in the config file.
//!
//! The DX12 present-hook is rig-confirmed to render under Proton/vkd3d, and ships (always compiled;
//! the DLL statically links the C++ runtime so it's self-contained). `[debug] overlay` (default on)
//! is a recovery kill-switch if vkd3d ever breaks it.
//!
//! Threading: hudhook draws on the game's **Present** thread, a different thread than our frame tasks.
//! The rule (per OVERLAY-RENDERING.md): the overlay only **reads** shared app state (non-blocking) and
//! draws; it never mutates game state. Menu activations are handed to the game thread via
//! [`crate::actionq`] (a feature performs/surfaces them); the menu's own cursor state lives here and is
//! touched only on the Present thread. Installed once at `app::install`; like the task handles it stays
//! resident for the process lifetime (hudhook owns the global — no unhook on detach).

use std::ffi::c_void;

use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::imgui::{Condition, Context, FontId, FontSource, Io, Key, Ui, WindowFlags};
use hudhook::{Hudhook, ImguiRenderLoop, MessageFilter, RenderContext};
use log::Level;
use unseamless_core::config::Config;
use unseamless_core::menu::{Menu, MenuOutcome, SessionContext};
use unseamless_core::notifications::{Banner, Severity, Toast};
use unseamless_core::protocol::SessionAction;
use unseamless_core::settings::{Setting, registry};
use windows::Win32::Foundation::HINSTANCE;

/// Key that toggles the utility window: backtick / grave (`` ` ``). Unbound in Elden Ring and the
/// universal "console" key. Hardcoded for now; a config-bound key can come later.
const TOGGLE_KEY: Key = Key::GraveAccent;
/// Window title — carries the version (the watermark will show it too). Doubles as the imgui window id.
const WINDOW_TITLE: &str = concat!("unseamless-coop  v", env!("CARGO_PKG_VERSION"));
/// Crisp UI font: a printable-ASCII subset of Open Sans (OFL — see `assets/menu-font.OFL.txt`), with
/// the family renamed so we don't ship a modified face under its reserved name. Baked at this size
/// (not bitmap-scaled, which is why the default font looked blurry enlarged). Embedded so the DLL stays
/// self-contained.
const MENU_FONT: &[u8] = include_bytes!("../assets/menu-font.ttf");
const MENU_FONT_SIZE: f32 = 19.0;

// One palette, referenced everywhere, so the severity / log-level / provenance colours can't silently
// drift apart (they're the same swatches used in different contexts, on purpose).
const BLUE: [f32; 3] = [0.62, 0.80, 1.0];
const AMBER: [f32; 3] = [1.0, 0.82, 0.30];
const RED: [f32; 3] = [1.0, 0.45, 0.45];
const GREY: [f32; 3] = [0.80, 0.80, 0.80];
const TEAL: [f32; 3] = [0.55, 0.75, 0.85];
const DIM_GREY: [f32; 3] = [0.55, 0.55, 0.55];

/// Makes the atlas [`FontId`] (a raw pointer into the process-lifetime font atlas) shareable. hudhook
/// requires the render loop be `Send + Sync`; this id is only ever dereferenced on the Present thread
/// (in `render` via `push_font`) and the atlas lives for the whole process, so sharing it is sound.
struct SyncFontId(FontId);
// SAFETY: see the type doc — the id is read only on the Present thread; the pointee outlives the process.
unsafe impl Send for SyncFontId {}
unsafe impl Sync for SyncFontId {}

/// The render loop. Must be `Send + Sync + 'static` (hudhook calls it from the Present thread).
struct Overlay {
    /// Session-action menu (actions only; settings are shown read-only). Touched only on the Present
    /// thread (in `render`).
    menu: Menu,
    /// The settings registry, built once (instead of per frame) for the read-only Settings tab.
    settings: Vec<Setting>,
    /// Whether the utility window is open. Written in `render`, read in `message_filter` — both run on
    /// the Present thread (hudhook samples the filter in `prepare_render`), so a plain bool is sound.
    open: bool,
    /// Set if `render` ever panics: the overlay is then skipped for the rest of the session rather than
    /// re-entered on torn state (and never unwinds across hudhook's FFI boundary — see `render`).
    disabled: bool,
    /// Our crisp menu font, added to the atlas in `initialize` and pushed only for the utility window
    /// (toasts keep the compact default). `None` until `initialize` runs.
    font: Option<SyncFontId>,
    /// Last good config snapshot, refreshed non-blocking each frame; drawn from so a contended frame
    /// doesn't flicker.
    config: Config,
    /// Actions the user requested that we couldn't enqueue yet (queue momentarily locked by the game
    /// thread); retried next frame so a keypress is never lost.
    pending: Vec<SessionAction>,
}

impl Overlay {
    fn new() -> Self {
        Self {
            menu: Menu::actions_only(),
            settings: registry(),
            open: false,
            disabled: false,
            font: None,
            config: Config::default(),
            pending: Vec::new(),
        }
    }

    /// The actual per-frame work, run inside `render`'s panic firewall.
    fn render_inner(&mut self, ui: &Ui) {
        // Toggle on backtick (no-repeat: one open/close per physical press).
        if ui.is_key_pressed_no_repeat(TOGGLE_KEY) {
            self.open = !self.open;
            if self.open {
                self.menu.home(&session_context());
            }
        }
        // Refresh the config snapshot non-blocking; keep the last good one on contention.
        if let Some(cfg) = crate::state::try_snapshot() {
            self.config = cfg;
        }
        self.draw_notifications(ui);
        if self.open {
            self.draw_utility_window(ui);
        }
        // Retry any actions the queue refused last frame.
        self.flush_pending();
    }

    /// Draw the passive notifications (banners then fading toasts), coloured by severity, in a
    /// borderless, input-transparent, auto-sized corner window. Reads [`crate::notify`] non-blocking.
    fn draw_notifications(&self, ui: &Ui) {
        let Some((banners, toasts)) =
            crate::notify::try_read(|n| (n.banners().to_vec(), n.toasts().to_vec()))
        else {
            return;
        };
        if banners.is_empty() && toasts.is_empty() {
            return; // nothing to show — overlay invisible when idle
        }
        let flags = WindowFlags::NO_DECORATION
            | WindowFlags::ALWAYS_AUTO_RESIZE
            | WindowFlags::NO_INPUTS
            | WindowFlags::NO_SAVED_SETTINGS
            | WindowFlags::NO_FOCUS_ON_APPEARING;
        ui.window("##unseamless-notifications")
            .position([24.0, 24.0], Condition::Always)
            .bg_alpha(0.35)
            .flags(flags)
            .build(|| {
                draw_banners(ui, &banners);
                draw_toasts(ui, &toasts);
            });
    }

    /// Draw the toggleable utility window with its tabs.
    fn draw_utility_window(&mut self, ui: &Ui) {
        let ctx = session_context();
        ui.window(WINDOW_TITLE)
            .size([480.0, 380.0], Condition::FirstUseEver)
            .position([80.0, 80.0], Condition::FirstUseEver)
            // NO_NAV: we drive selection ourselves (arrow keys → the `Menu` cursor), so disable imgui's
            // own keyboard nav for this window — hudhook force-enables nav each frame, so a window flag
            // is the only reliable way to stop it double-handling arrows/Enter. Mouse clicks still work.
            .flags(WindowFlags::NO_SAVED_SETTINGS | WindowFlags::NO_COLLAPSE | WindowFlags::NO_NAV)
            .build(|| {
                // Push our crisp font for the whole window (incl. the log child); toasts keep the
                // compact default. Token held to closure end, then popped.
                let _font = self.font.as_ref().map(|f| ui.push_font(f.0));
                if let Some(_bar) = ui.tab_bar("##tabs") {
                    if let Some(_tab) = ui.tab_item("Actions") {
                        self.draw_actions_tab(ui, &ctx);
                    }
                    if let Some(_tab) = ui.tab_item("Settings") {
                        self.draw_settings_tab(ui);
                    }
                    if let Some(_tab) = ui.tab_item("Log") {
                        draw_log_tab(ui);
                    }
                }
            });
    }

    /// The interactive session-action list. Up/down move the cursor (skipping disabled rows), Enter or
    /// a click activates the selected action; the activated action is handed to the game thread.
    fn draw_actions_tab(&mut self, ui: &Ui, ctx: &SessionContext) {
        if ui.is_key_pressed(Key::DownArrow) {
            self.menu.select_next(ctx);
        }
        if ui.is_key_pressed(Key::UpArrow) {
            self.menu.select_prev(ctx);
        }
        let mut activate = enter_pressed(ui);

        // `rows` indices are 1:1 with the menu's items (actions-only, no filtering), so a row index is a
        // valid `select_index` target. `MenuRow.value` is always `None` here (no setting rows).
        let rows = self.menu.rows(&self.config, ctx);
        let mut clicked = None;
        for (i, row) in rows.iter().enumerate() {
            if row.enabled {
                // Action labels are unique, so each doubles as a stable imgui id.
                if ui.selectable_config(&row.label).selected(row.selected).build() {
                    clicked = Some(i);
                }
            } else {
                ui.text_disabled(&row.label);
            }
        }
        if let Some(i) = clicked {
            self.menu.select_index(i);
            activate = true;
        }

        if activate {
            // actions_only never mutates config on activate; pass a scratch clone (per activation, not
            // per frame) so the `&mut Config` signature is satisfied without touching our snapshot.
            let mut scratch = self.config.clone();
            if let MenuOutcome::Action(action) = self.menu.activate(&mut scratch, ctx) {
                self.request_action(action);
            }
        }
    }

    /// Read-only view of every setting and its current value, coloured by whether the host syncs it
    /// across the party (shared) or it's local to this machine. Editing happens in the config file.
    fn draw_settings_tab(&self, ui: &Ui) {
        ui.text_disabled("Read-only. Edit in unseamless_coop.toml, then relaunch.");
        ui.text_colored(rgba(BLUE, 1.0), "synced");
        ui.same_line();
        ui.text("= shared across the party,");
        ui.same_line();
        ui.text_colored(rgba(GREY, 1.0), "local");
        ui.same_line();
        ui.text("= just you");
        ui.separator();
        for s in &self.settings {
            let color = if s.id.is_shared() { BLUE } else { GREY };
            ui.text_colored(rgba(color, 1.0), s.label);
            ui.same_line();
            ui.text_disabled(format!("= {}", s.display_value(&self.config)));
        }
    }

    /// Hand an activated action to the game thread (via [`crate::actionq`]), retrying any the queue
    /// refused. Note: this pushes onto the local retry buffer; the cross-thread enqueue is `actionq`.
    fn request_action(&mut self, action: SessionAction) {
        self.pending.push(action);
        self.flush_pending();
    }

    /// Flush pending actions into the shared queue; keep any the (briefly locked) queue refused so they
    /// are retried next frame. Called unconditionally each frame, not just on activation.
    fn flush_pending(&mut self) {
        self.pending.retain(|&action| !crate::actionq::try_offer(action));
    }
}

impl ImguiRenderLoop for Overlay {
    fn initialize<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        // Called once before hudhook bakes the font atlas, so fonts added here get rasterized.
        let fonts = ctx.fonts();
        // Keep the compact default (ProggyClean) as the atlas default so the passive toasts stay small;
        // our crisp subset is an extra font pushed only for the utility window.
        fonts.add_font(&[FontSource::DefaultFontData { config: None }]);
        let id = fonts.add_font(&[FontSource::TtfData {
            data: MENU_FONT,
            size_pixels: MENU_FONT_SIZE,
            config: None,
        }]);
        self.font = Some(SyncFontId(id));
    }

    fn before_render<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        // Draw imgui's own cursor while the window is open — during gameplay the OS cursor is hidden,
        // so otherwise the mouse is invisible. (Whether the cursor can actually *move* during gameplay
        // is a separate, game-side capture problem — see `message_filter`; keyboard nav doesn't need it.)
        ctx.io_mut().mouse_draw_cursor = self.open;
    }

    fn message_filter(&self, _io: &Io) -> MessageFilter {
        // While the utility window is open, swallow keyboard/mouse/raw input so the game doesn't also
        // act on it while we navigate. hudhook feeds every message to imgui *before* consulting this, so
        // backtick-to-close still registers. Two caveats, both to confirm on the rig: (1) the filter is
        // sampled a frame late, so the first frame on open leaks one message to the game (harmless —
        // backtick is unbound in ER); (2) this blocks only WndProc input — if the game reads movement
        // out-of-band (DirectInput/XInput/GetAsyncKeyState), `MessageFilter` can't stop it, so verify
        // that movement/attack actually halt with the window open.
        if self.open {
            MessageFilter::InputAll
        } else {
            MessageFilter::empty()
        }
    }

    fn render(&mut self, ui: &mut Ui) {
        if self.disabled {
            return;
        }
        // FFI firewall: hudhook calls `render` from its `extern "system"` present hook with no catch of
        // its own, so a panic here would unwind across that boundary — UB under `panic = "unwind"` (a
        // bare `cargo build`), abort under the shipped `panic = "abort"`. Mirror app.rs's per-task
        // catch: on a render panic, disable the overlay for the session (the panic hook already logged
        // the backtrace) rather than risk the game.
        let ui: &Ui = ui;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.render_inner(ui))).is_err() {
            self.disabled = true;
            log::error!("overlay: render panicked; overlay disabled for the rest of the session");
        }
    }
}

/// The current session context for menu gating. Until the co-op/session layer lands (rig-gated) we
/// have no live session, so this is the not-in-session default — only Host / Join / Break-in enabled.
fn session_context() -> SessionContext {
    SessionContext::default()
}

/// Enter (main or keypad) pressed this frame, no key-repeat — one activation per physical press.
fn enter_pressed(ui: &Ui) -> bool {
    ui.is_key_pressed_no_repeat(Key::Enter) || ui.is_key_pressed_no_repeat(Key::KeypadEnter)
}

fn draw_banners(ui: &Ui, banners: &[Banner]) {
    for b in banners {
        ui.text_colored(rgba(severity_color(b.severity), 1.0), &b.message);
    }
}

fn draw_toasts(ui: &Ui, toasts: &[Toast]) {
    for t in toasts {
        // Fade alpha out as the toast expires. The model guarantees `duration > 0.0` and finite, so
        // this can't divide by zero.
        let alpha = (t.remaining / t.duration).clamp(0.0, 1.0);
        ui.text_colored(rgba(severity_color(t.severity), alpha), &t.message);
    }
}

/// Our own log tail, coloured by level. Copies the lines out under the non-blocking lock first, then
/// draws — never holding the log lock across the imgui draw loop (game-thread log pushes block on it).
fn draw_log_tab(ui: &Ui) {
    let Some(lines) = crate::logbuf::try_read(|lines| lines.iter().cloned().collect::<Vec<_>>()) else {
        return; // contended this frame; skip drawing the log
    };
    let avail = ui.content_region_avail();
    ui.child_window("##log").size([avail[0], avail[1].max(60.0)]).build(|| {
        // Wrap long lines at the child's right edge instead of overflowing horizontally.
        let _wrap = ui.push_text_wrap_pos();
        for line in &lines {
            ui.text_colored(rgba(level_color(line.level), 1.0), &line.text);
        }
        // Auto-scroll to the bottom only when already at the bottom, so scrolling up to read stays put.
        if ui.scroll_y() >= ui.scroll_max_y() - 1.0 {
            ui.set_scroll_here_y_with_ratio(1.0);
        }
    });
}

/// Pack an RGB swatch with an alpha into the RGBA imgui wants.
fn rgba(rgb: [f32; 3], alpha: f32) -> [f32; 4] {
    [rgb[0], rgb[1], rgb[2], alpha]
}

/// Palette for a notification severity (Info reads as informative blue).
fn severity_color(severity: Severity) -> [f32; 3] {
    match severity {
        Severity::Info => BLUE,
        Severity::Warning => AMBER,
        Severity::Error => RED,
    }
}

/// Palette for a log line's level (Info reads as neutral grey — distinct from severity-Info on purpose).
fn level_color(level: Level) -> [f32; 3] {
    match level {
        Level::Error => RED,
        Level::Warn => AMBER,
        Level::Info => GREY,
        Level::Debug => TEAL,
        Level::Trace => DIM_GREY,
    }
}

/// Install the DX12 present-hook overlay. `module` is our DLL's module handle (the `SELF_MODULE`
/// `usize`, reinterpreted as an `HINSTANCE`). Logs and returns on failure — the overlay is past the
/// install-critical path, so a hook failure degrades (no overlay) rather than aborting the game.
pub fn install(module: usize) {
    if module == 0 {
        // SELF_MODULE's default — only possible if DllMain never ran, which can't happen here since
        // install runs from it. Guard anyway, matching app::install's defensive posture.
        log::error!("overlay: no module handle; skipping hook install");
        return;
    }
    let hmodule = HINSTANCE(module as *mut c_void);
    match Hudhook::builder().with::<ImguiDx12Hooks>(Overlay::new()).with_hmodule(hmodule).build().apply() {
        Ok(()) => log::info!("overlay: DX12 present-hook installed; waiting for the swapchain"),
        Err(e) => log::error!("overlay: hook install failed ({e:?}); no overlay this session"),
    }
}
