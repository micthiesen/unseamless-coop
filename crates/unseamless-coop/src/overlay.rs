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
use hudhook::imgui::{
    Condition, Context, FontConfig, FontId, FontSource, Io, Key, TabItemFlags, Ui, WindowFlags,
};
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
/// Window title: version, then a control hint, with a stable `###` id so the visible label can change
/// (and carry the hint) without changing the window's identity — which would reset its remembered
/// position. ASCII only: the title bar draws in the default font, which has no arrow glyphs.
const WINDOW_TITLE: &str = concat!(
    "unseamless-coop  v",
    env!("CARGO_PKG_VERSION"),
    "    |    Up/Dn: select    L/R: tabs    Enter: use    `: close",
    "###unseamless-coop",
);
/// Crisp UI font: a printable-ASCII subset of **Spleen 8x16** (BSD-2 — see
/// `assets/menu-font.LICENSE.txt`), a pixel font with a 16px native size, baked at that size. A bitmap
/// font is only crisp at its native size, so we source one designed larger rather than scale the
/// 13px default (which blurs). Embedded so the DLL stays self-contained.
const MENU_FONT: &[u8] = include_bytes!("../assets/menu-font.otf");
const MENU_FONT_SIZE: f32 = 16.0;
/// The utility window's tabs, in order. Left/Right arrows cycle through them.
const TABS: [&str; 3] = ["Actions", "Settings", "Log"];

// Software-cursor marker: a small faded orb drawn at the mouse hotspot (so it sits at the tip of ER's
// own cursor when both show, and reads as a position dot when ours is the only one). Three concentric
// discs — faint outer glow, dark contrast ring, bright core — so it stands out on any background.
// Cyan-ish to complement ER's gold cursor. Tweak freely.
/// Nudge the orb slightly right of the hotspot so it sits just under the tip of ER's cursor.
const CURSOR_OFFSET_X: f32 = 1.0;
const CURSOR_GLOW_R: f32 = 7.0;
const CURSOR_RING_R: f32 = 4.0;
const CURSOR_CORE_R: f32 = 2.5;
const CURSOR_GLOW: [f32; 4] = [0.55, 0.85, 1.0, 0.16];
const CURSOR_RING: [f32; 4] = [0.0, 0.0, 0.0, 0.50];
const CURSOR_CORE: [f32; 4] = [0.65, 0.90, 1.0, 0.95];

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
    /// Index of the active tab (into [`TABS`]). Tracks the visible tab (incl. mouse clicks) and is
    /// moved by Left/Right; we force-select the tab matching it the frame an arrow is pressed.
    tab: usize,
    /// A position to snap the window to next frame, set when it drifts out of the ER viewport so it
    /// stays "locked" inside the game window. `None` when it's in bounds (normal dragging).
    clamp_pos: Option<[f32; 2]>,
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
            tab: 0,
            clamp_pos: None,
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
        // Mirror the open-state into the input suppressor every frame: while open the game doesn't see
        // keyboard/mouse (but imgui still gets them via hudhook's WndProc hook), and closing the window
        // restores game input immediately.
        crate::input::set_blocked(self.open);
        // Refresh the config snapshot non-blocking; keep the last good one on contention.
        if let Some(cfg) = crate::state::try_snapshot() {
            self.config = cfg;
        }
        self.draw_notifications(ui);
        if self.open {
            self.draw_utility_window(ui);
            draw_cursor_marker(ui);
        }
        // Retry any actions the queue refused last frame.
        self.flush_pending();
    }

    /// Draw the passive notifications (banners then fading toasts), coloured by severity, in a
    /// borderless, input-transparent, auto-sized corner window. Reads [`crate::notify`] non-blocking.
    fn draw_notifications(&self, ui: &Ui) {
        // Copy out under the (non-blocking) lock, but skip the clone on the common idle frame — return
        // None from inside the closure when there's nothing to show, so we don't allocate two Vecs every
        // frame just to find them empty.
        let Some(Some((banners, toasts))) = crate::notify::try_read(|n| {
            (!n.banners().is_empty() || !n.toasts().is_empty())
                .then(|| (n.banners().to_vec(), n.toasts().to_vec()))
        }) else {
            return;
        };
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

    /// Keep the window fully inside the ER viewport ("lock it to the game window"). Reads the current
    /// geometry (valid inside the build closure) and, when out of bounds and not being dragged, queues a
    /// snap-back for next frame (applied via `position(Always)` before `build`). Only snapping on release
    /// avoids fighting an active drag (which jitters the window at the edge); an out-of-bounds window for
    /// another reason (e.g. the viewport shrank) snaps immediately. The frame already drew at the dragged
    /// spot, so there's one frame of overshoot, then it locks in.
    fn clamp_into_viewport(&mut self, ui: &Ui) {
        let (pos, size, disp) = (ui.window_pos(), ui.window_size(), ui.io().display_size);
        let clamped = [
            pos[0].clamp(0.0, (disp[0] - size[0]).max(0.0)),
            pos[1].clamp(0.0, (disp[1] - size[1]).max(0.0)),
        ];
        if clamped != pos && !ui.io().mouse_down[0] {
            self.clamp_pos = Some(clamped);
        }
    }

    /// Draw the toggleable utility window with its tabs.
    fn draw_utility_window(&mut self, ui: &Ui) {
        let ctx = session_context();
        // Left/Right cycle tabs. Compute the force-select target here (before the bar) so the arrow
        // lands on the new tab even though imgui otherwise owns tab state; the active tab writes back
        // `self.tab` below, so mouse clicks are tracked too and the next arrow moves from the right place.
        let mut force_tab = None;
        if ui.is_key_pressed(Key::RightArrow) {
            self.tab = (self.tab + 1) % TABS.len();
            force_tab = Some(self.tab);
        }
        if ui.is_key_pressed(Key::LeftArrow) {
            self.tab = (self.tab + TABS.len() - 1) % TABS.len();
            force_tab = Some(self.tab);
        }
        // If the window drifted out of the ER viewport last frame, snap it back this frame ("lock" it
        // to the game window). Taken into a local first so the build closure can re-borrow `self`.
        let clamp = self.clamp_pos.take();
        let mut win = ui
            .window(WINDOW_TITLE)
            .size([624.0, 380.0], Condition::FirstUseEver)
            // Floor the size so it can't be dragged down to a uselessly tiny box (max unbounded).
            .size_constraints([360.0, 240.0], [f32::MAX, f32::MAX])
            .position([80.0, 80.0], Condition::FirstUseEver)
            // NO_NAV: we drive selection ourselves (arrow keys → the `Menu` cursor / tabs), so disable
            // imgui's own keyboard nav for this window — hudhook force-enables nav each frame, so a
            // window flag is the only reliable way to stop it double-handling arrows. Clicks still work.
            // NO_SCROLLBAR: the tab bar stays fixed; each tab's content scrolls in its own child below.
            .flags(
                WindowFlags::NO_SAVED_SETTINGS
                    | WindowFlags::NO_COLLAPSE
                    | WindowFlags::NO_NAV
                    | WindowFlags::NO_SCROLLBAR,
            );
        if let Some(p) = clamp {
            win = win.position(p, Condition::Always);
        }
        win.build(|| {
                self.clamp_into_viewport(ui);
                // Push our crisp font for the whole window (incl. the log child); toasts keep the
                // compact default. Token held to closure end, then popped.
                let _font = self.font.as_ref().map(|f| ui.push_font(f.0));
                if let Some(_bar) = ui.tab_bar("##tabs") {
                    for (i, &label) in TABS.iter().enumerate() {
                        let flags = if force_tab == Some(i) {
                            TabItemFlags::SET_SELECTED
                        } else {
                            TabItemFlags::empty()
                        };
                        if let Some(_tab) = ui.tab_item_with_flags(label, None, flags) {
                            self.tab = i; // track the visible tab (incl. mouse clicks)
                            // Each tab's content lives in its own scrollable child (per-label id, so
                            // scroll state is independent), filling the space under the tab bar — so
                            // overflow scrolls the content, not the whole window / tab bar.
                            let avail = ui.content_region_avail();
                            ui.child_window(format!("##content-{label}"))
                                .size([avail[0], avail[1].max(60.0)])
                                .build(|| match label {
                                    "Actions" => self.draw_actions_tab(ui, &ctx),
                                    "Settings" => self.draw_settings_tab(ui),
                                    "Log" => draw_log_tab(ui),
                                    _ => {}
                                });
                        }
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
        // Index 0 stays the compact 13px default for the passive toasts. The menu font is Spleen 8x16
        // baked at its native 16px with oversampling off + pixel snap, so it stays crisp.
        fonts.add_font(&[FontSource::DefaultFontData { config: None }]);
        let id = fonts.add_font(&[FontSource::TtfData {
            data: MENU_FONT,
            size_pixels: MENU_FONT_SIZE,
            config: Some(FontConfig {
                oversample_h: 1,
                oversample_v: 1,
                pixel_snap_h: true,
                ..FontConfig::default()
            }),
        }]);
        self.font = Some(SyncFontId(id));
    }

    fn before_render<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        if self.disabled {
            return; // a disabled overlay is fully inert (mirrors `render`'s guard)
        }
        // Keep imgui's own arrow cursor off — we draw our own marker (`draw_cursor_marker`) at the mouse
        // hotspot instead, which complements ER's cursor rather than clashing with a second arrow.
        ctx.io_mut().mouse_draw_cursor = false;
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
            // Release both input-suppression paths if we died mid-frame while open: the DirectInput
            // block (set_blocked) AND the WndProc filter — `message_filter` reads `self.open`, so clear
            // it too, or it would keep swallowing all window input for the rest of the session.
            self.open = false;
            crate::input::set_blocked(false);
            log::error!("overlay: render panicked; overlay disabled for the rest of the session");
        }
    }
}

/// The current session context for menu gating. Until the co-op/session layer lands (rig-gated) we
/// have no live session, so this is the not-in-session default — only Host / Join / Break-in enabled.
fn session_context() -> SessionContext {
    SessionContext::default()
}

/// Draw our software cursor — a small faded orb at the mouse hotspot, on the foreground draw list (over
/// everything). At the same point as ER's cursor, so it lands at the tip of ER's arrow when both show,
/// and reads as a position dot when ours is the only cursor.
fn draw_cursor_marker(ui: &Ui) {
    let p = ui.io().mouse_pos;
    // imgui parks the mouse at a large-negative sentinel when it has no valid position; skip then.
    if !p[0].is_finite() || !p[1].is_finite() || p[0] < -1.0e4 || p[1] < -1.0e4 {
        return;
    }
    let p = [p[0] + CURSOR_OFFSET_X, p[1]];
    // One foreground draw list per frame: imgui-rs panics if a second instance is alive before this one
    // drops. We bind it once, reuse it for all three circles, and it drops at function end — and this is
    // called after the window has fully closed, so nothing else holds one. Keep it that way.
    let dl = ui.get_foreground_draw_list();
    dl.add_circle(p, CURSOR_GLOW_R, CURSOR_GLOW).filled(true).build();
    dl.add_circle(p, CURSOR_RING_R, CURSOR_RING).filled(true).build();
    dl.add_circle(p, CURSOR_CORE_R, CURSOR_CORE).filled(true).build();
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
    // The scrollable box is the per-tab child the caller created. Wrap long lines at its right edge.
    let _wrap = ui.push_text_wrap_pos();
    // Newest first: the ring buffer is oldest→newest, so render it reversed. The view sits at the top by
    // default, so the latest line is always in sight — and it's live, since the buffer is re-read each frame.
    for line in lines.iter().rev() {
        ui.text_colored(rgba(level_color(line.level), 1.0), &line.text);
    }
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
