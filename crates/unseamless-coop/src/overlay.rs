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
    Condition, Context, FontConfig, FontId, FontSource, Io, Key, MouseButton, TabItemFlags, Ui,
    WindowFlags,
};
use hudhook::{Hudhook, ImguiRenderLoop, MessageFilter, RenderContext};
use log::Level;
use unseamless_core::config::Config;
use unseamless_core::diagnostics::DiagnosticReport;
use unseamless_core::menu::{Menu, MenuOutcome, SessionContext};
use unseamless_core::notifications::{Banner, Severity, Toast};
use unseamless_core::protocol::SessionAction;
use unseamless_core::settings::{Setting, registry};
use windows::Win32::Foundation::HINSTANCE;

/// Key that toggles the utility window: backtick / grave (`` ` ``). Unbound in Elden Ring and the
/// universal "console" key. Hardcoded for now; a config-bound key can come later.
const TOGGLE_KEY: Key = Key::GraveAccent;
/// Window title: just the mod name + version, with a stable `###` id so window identity (and its
/// remembered position) is independent of the visible label. The control hint is no longer baked in —
/// a short close hint is drawn right-aligned in the title bar instead (see [`draw_title_hint`]) — and
/// the build id now lives in the debug panel, not the title.
fn window_title() -> &'static str {
    concat!("unseamless-coop  v", env!("CARGO_PKG_VERSION"), "###unseamless-coop")
}
/// Crisp UI font: a printable-ASCII subset of **Spleen 8x16** (BSD-2 — see
/// `assets/menu-font.LICENSE.txt`), a pixel font with a 16px native size, baked at that size. A bitmap
/// font is only crisp at its native size, so we source one designed larger rather than scale the
/// 13px default (which blurs). Embedded so the DLL stays self-contained.
const MENU_FONT: &[u8] = include_bytes!("../assets/menu-font.otf");
const MENU_FONT_SIZE: f32 = 16.0;
/// The utility window's tabs, in order. Left/Right arrows cycle through them.
const TABS: [&str; 3] = ["Actions", "Settings", "Log"];
/// Short hint drawn right-aligned in the utility window's title bar: backtick (the toggle key) or B
/// (the controller cancel) closes it. ASCII (incl. backtick) so it renders in the default title font.
const CLOSE_HINT: &str = "` or B to close";
/// Right inset of the title-bar close hint from the window's right edge, in pixels.
const TITLE_HINT_INSET: f32 = 8.0;

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

// Home-snap ghost box: a white outline (with a faint fill) drawn over the window's home rectangle —
// its default position + size — while the window is dragged with the cursor inside that rectangle.
// Releasing the drag there snaps the window back to that exact spot and size. White so it reads as a
// neutral "drop here" target against ER's gold/UI palette.
const GHOST_FILL: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
const GHOST_LINE: [f32; 4] = [1.0, 1.0, 1.0, 0.85];
const GHOST_LINE_THICKNESS: f32 = 2.0;

// One palette, referenced everywhere, so the severity / log-level / provenance colours can't silently
// drift apart (they're the same swatches used in different contexts, on purpose).
const BLUE: [f32; 3] = [0.62, 0.80, 1.0];
const AMBER: [f32; 3] = [1.0, 0.82, 0.30];
const RED: [f32; 3] = [1.0, 0.45, 0.45];
const GREY: [f32; 3] = [0.80, 0.80, 0.80];
const TEAL: [f32; 3] = [0.55, 0.75, 0.85];
const DIM_GREY: [f32; 3] = [0.55, 0.55, 0.55];

/// Background alpha shared by the passive corner surfaces (notifications, watermark), so the two
/// can't drift apart.
const PASSIVE_BG_ALPHA: f32 = 0.35;

/// One inset, in pixels, from the viewport edge, shared by every overlay surface: the top-left
/// notifications (left + top), the top-right watermark (right + top), and the utility window's
/// default top edge. Tweak here to move them all together.
const OVERLAY_MARGIN: f32 = 24.0;

/// The utility window's default size. Also drives its default horizontal centering (so it opens
/// centered along the top), so it's a constant rather than an inline literal.
const WINDOW_DEFAULT_SIZE: [f32; 2] = [624.0, 380.0];

/// Window flags shared by the passive corner surfaces: borderless, auto-sized to content,
/// input-transparent, not persisted, and never stealing focus on appear. A function (not a `const`)
/// to avoid depending on `WindowFlags`' const bitops across imgui versions.
fn passive_window_flags() -> WindowFlags {
    WindowFlags::NO_DECORATION
        | WindowFlags::ALWAYS_AUTO_RESIZE
        | WindowFlags::NO_INPUTS
        | WindowFlags::NO_SAVED_SETTINGS
        | WindowFlags::NO_FOCUS_ON_APPEARING
}

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
    /// Whether the Settings tab reveals the session password (vs. masking it). Default masked so it
    /// isn't exposed on a stream/screenshot; toggled by the Reveal/Hide button. Present-thread only.
    password_revealed: bool,
    /// Controller→menu edge state: turns the raw pad snapshot into per-frame nav/activate/toggle edges.
    /// Updated once per frame in `render_inner`; Present-thread only (see [`crate::input::PadNav`]).
    pad: crate::input::PadNav,
    /// Whether the bottom-left **debug panel** is shown. Defaults to on in debug builds (`diag`
    /// profile), off in release; toggled from the Actions tab. Independent of `open` and of gameplay
    /// state — unlike the watermark, it stays up during play. Mirrored to [`crate::debug_panel`] each frame
    /// so the game-thread publisher only does work while it's shown. Present-thread only.
    show_debug: bool,
    /// Cursor into the Actions tab's combined list — the menu's action rows followed by the trailing
    /// debug-overlay toggle (index `== menu rows`). Owns selection across both (the core `Menu`'s own
    /// cursor is only synced to it at activation), so arrow/d-pad nav spans the toggle row too.
    actions_sel: usize,
    /// Home-snap affordance state. `home_dragging` latches true once an in-progress window move is
    /// detected (the window position changed while the left button is held) and clears on release —
    /// so it stays set if the drag pauses over the target, and a release can still be attributed to a
    /// drag the frame the button goes up. `last_win_pos` is last frame's window position, the baseline
    /// for that move detection. `home_snap` requests a snap-to-home (the window's default position +
    /// size) be applied next frame, via `position`/`size(Always)` before `build`. Present-thread only.
    home_dragging: bool,
    last_win_pos: Option<[f32; 2]>,
    home_snap: bool,
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
            password_revealed: false,
            pad: crate::input::PadNav::new(),
            // On by default in debug builds (the `diag` profile keeps debug-assertions); off in the
            // stripped release, where it's an opt-in toggled from the Actions tab.
            show_debug: cfg!(debug_assertions),
            actions_sel: 0,
            home_dragging: false,
            last_win_pos: None,
            home_snap: false,
        }
    }

    /// The actual per-frame work, run inside `render`'s panic firewall.
    fn render_inner(&mut self, ui: &Ui) {
        // Sample the controller once per frame (drives the toggle chord below and, while open, the menu
        // nav). Computed every frame regardless of open-state so the chord can *open* the menu, not just
        // navigate it. The raw snapshot comes from the XInput hook; the pure edge/repeat logic lives in
        // `unseamless_core::pad`.
        let (buttons, lx, ly) = crate::input::pad_snapshot();
        let pad = self.pad.update(buttons, lx, ly, ui.io().delta_time);
        // Toggle on backtick or the LB+RB+L3+R3 chord (no-repeat: one open/close per press).
        if ui.is_key_pressed_no_repeat(TOGGLE_KEY) || pad.toggle {
            self.open = !self.open;
            if self.open {
                // Home the Actions cursor; `draw_actions_tab` repairs it to the first enabled row (0
                // can be a disabled action when opened mid-session). It owns nav, syncing the core
                // `Menu` only at activation, so we reset its cursor rather than the menu's here.
                self.actions_sel = 0;
            }
        }
        // B closes the menu (Back/Cancel), but only while it's open — when closed, B is a game input.
        if self.open && pad.cancel {
            self.open = false;
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
        // Branded corner stamp — only off the playfield (title/main menu, character select, loading),
        // never a persistent in-play banner. The game-thread probe (`crate::features::playstate`)
        // publishes the flag; we read it non-blocking here.
        if !crate::playstate::in_gameplay() {
            self.draw_watermark(ui);
        }
        // Live debug panel (bottom-left): shown whenever toggled on, including during gameplay (unlike
        // the watermark). Mirror its visibility to the game thread every frame so the publisher only
        // does work while it's shown; then draw from the snapshot it posts.
        crate::debug_panel::set_visible(self.show_debug);
        if self.show_debug {
            self.draw_debug_panel(ui);
        }
        if self.open {
            self.draw_utility_window(ui, pad);
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
        // Top-left, the opposite corner from the watermark (top-right) and Steam's toasts (bottom-right).
        ui.window("##unseamless-notifications")
            .position([OVERLAY_MARGIN, OVERLAY_MARGIN], Condition::Always)
            .bg_alpha(PASSIVE_BG_ALPHA)
            .flags(passive_window_flags())
            .build(|| {
                draw_banners(ui, &banners);
                draw_toasts(ui, &toasts);
            });
    }

    /// Draw the branded corner stamp — mod name + version + the backtick hint — anchored to the
    /// top-right. Stands in for the vanilla "App Ver. / OFFLINE" version block (which we can't edit:
    /// its text is FMG, uncharted by the SDK at our pin), but sits top-right rather than the vanilla
    /// bottom-right so it doesn't overlap Steam's own bottom-right notifications. Gated by the caller
    /// to off-the-playfield only. Borderless and input-transparent like the notifications surface;
    /// uses our crisp menu font.
    fn draw_watermark(&self, ui: &Ui) {
        let disp = ui.io().display_size;
        // Anchor by the window's own top-right corner (pivot 1,0) at a fixed inset from the viewport's
        // top-right, so it stays put regardless of the auto-sized text width.
        ui.window("##unseamless-watermark")
            .position([disp[0] - OVERLAY_MARGIN, OVERLAY_MARGIN], Condition::Always)
            .position_pivot([1.0, 0.0])
            .bg_alpha(PASSIVE_BG_ALPHA)
            .flags(passive_window_flags())
            .build(|| {
                let _font = self.font.as_ref().map(|f| ui.push_font(f.0));
                ui.text_colored(rgba(BLUE, 1.0), concat!("unseamless-coop  v", env!("CARGO_PKG_VERSION")));
                // Debug builds show the baked build id here too, so the off-playfield watermark
                // self-identifies the build even when the menu is closed. Quiet on release.
                if cfg!(debug_assertions) {
                    ui.text_disabled(concat!("build ", env!("UNSEAMLESS_BUILD_ID")));
                }
                ui.text_disabled("Press ` or LB+RB+L3+R3 to open the menu");
            });
    }

    /// Draw the live **debug panel** — a read-only, bottom-left passive surface rendering the
    /// diagnostic snapshot the game-thread publisher posts ([`crate::debug_panel`]). It's the same
    /// [`DiagnosticReport`] the log dumps produce, shown live (build / session / features / scaling /
    /// runtime). Bottom-left is the one free corner (notifications top-left, watermark top-right,
    /// Steam's toasts bottom-right). Anchored by its own bottom-left corner (pivot 0,1) so it grows
    /// upward from a fixed inset and never runs off the bottom. Borderless + input-transparent like the
    /// other passive surfaces; reads the snapshot non-blocking and skips the frame before the first
    /// publish or on contention. Drawn in the compact default font (like the toasts) rather than the
    /// crisp menu font — the smaller type suits a dense, glanceable info panel.
    fn draw_debug_panel(&self, ui: &Ui) {
        let disp = ui.io().display_size;
        ui.window("##unseamless-debug")
            .position([OVERLAY_MARGIN, disp[1] - OVERLAY_MARGIN], Condition::Always)
            .position_pivot([0.0, 1.0])
            .bg_alpha(PASSIVE_BG_ALPHA)
            .flags(passive_window_flags())
            .build(|| {
                match crate::debug_panel::snapshot() {
                    Some(report) => draw_report(ui, &report),
                    None => ui.text_disabled("debug panel: gathering..."),
                }
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

    /// Home-snap affordance (called inside the window's build closure, so `is_window_focused` and the
    /// mouse state refer to this window). While the window is being dragged with the cursor inside its
    /// home rectangle (`home_pos` + `home_size`), draw a white ghost box over that rectangle; releasing
    /// the drag there requests a snap-to-home next frame (applied via `position`/`size(Always)`), which
    /// repositions *and* resizes the window back to exactly where it first opened.
    fn handle_home_snap(&mut self, ui: &Ui, home_pos: [f32; 2], home_size: [f32; 2]) {
        let pos = ui.window_pos();
        let mouse_down = ui.io().mouse_down[0];
        // Detect an in-progress window move by a position change while the button is held, then latch
        // it. This (unlike `is_any_item_active`, which imgui sets to the window's move-id during a
        // title-bar drag) distinguishes a window move from a selectable drag: only a move shifts the
        // window position. Latching keeps it true if the drag pauses over the target.
        if mouse_down && self.last_win_pos.is_some_and(|p| p != pos) {
            self.home_dragging = true;
        }
        // Trigger zone = the visual ghost: full width, top quarter of the home rect, anchored at the
        // home top-left. Hit-test and ghost use the same rect so the highlight is exactly where the
        // snap arms. The snap itself still restores the full window size (`WINDOW_DEFAULT_SIZE`).
        let snap_zone = [home_size[0], home_size[1] / 4.0];
        let in_home = point_in_rect(ui.io().mouse_pos, home_pos, snap_zone);
        if self.home_dragging && mouse_down && in_home {
            draw_ghost_box(ui, home_pos, snap_zone);
        }
        // Snap on release inside the box — checked before clearing the latch, since the release frame's
        // button is already up.
        if self.home_dragging && ui.is_mouse_released(MouseButton::Left) && in_home {
            self.home_snap = true;
        }
        if !mouse_down {
            self.home_dragging = false;
        }
        self.last_win_pos = Some(pos);
    }

    /// Draw the toggleable utility window with its tabs.
    fn draw_utility_window(&mut self, ui: &Ui, pad: crate::input::PadEdges) {
        let ctx = session_context();
        // Left/Right (arrow keys or d-pad / left-stick) cycle tabs. Compute the force-select target here
        // (before the bar) so the input lands on the new tab even though imgui otherwise owns tab state;
        // the active tab writes back `self.tab` below, so mouse clicks are tracked too and the next move
        // starts from the right place.
        let mut force_tab = None;
        if ui.is_key_pressed(Key::RightArrow) || pad.right {
            self.tab = (self.tab + 1) % TABS.len();
            force_tab = Some(self.tab);
        }
        if ui.is_key_pressed(Key::LeftArrow) || pad.left {
            self.tab = (self.tab + TABS.len() - 1) % TABS.len();
            force_tab = Some(self.tab);
        }
        // If the window drifted out of the ER viewport last frame, snap it back this frame ("lock" it
        // to the game window). Taken into a local first so the build closure can re-borrow `self`.
        let clamp = self.clamp_pos.take();
        // Whether to snap the window back to its home rectangle (default position + size) this frame —
        // requested last frame when the user released a drag with the cursor over the home box.
        let home_snap = std::mem::take(&mut self.home_snap);
        // Default placement (first open only): horizontally centered, a top-margin down from the top.
        // `.max(0.0)` only matters for a viewport narrower than the window. The clamp/drag logic owns
        // every later position; this is just where it first appears. Also the home rectangle the
        // home-snap affordance targets (position = `default_pos`, size = `WINDOW_DEFAULT_SIZE`).
        let disp = ui.io().display_size;
        let default_pos = [((disp[0] - WINDOW_DEFAULT_SIZE[0]) / 2.0).max(0.0), OVERLAY_MARGIN];
        let mut win = ui
            .window(window_title())
            .size(WINDOW_DEFAULT_SIZE, Condition::FirstUseEver)
            // Floor the size so it can't be dragged down to a uselessly tiny box (max unbounded).
            .size_constraints([360.0, 240.0], [f32::MAX, f32::MAX])
            .position(default_pos, Condition::FirstUseEver)
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
        // Home-snap wins over the edge clamp: it targets the in-bounds default rectangle, so applying
        // both position and size here puts the window exactly back where it first opened.
        if home_snap {
            win = win.position(default_pos, Condition::Always).size(WINDOW_DEFAULT_SIZE, Condition::Always);
        } else if let Some(p) = clamp {
            win = win.position(p, Condition::Always);
        }
        win.build(|| {
                self.clamp_into_viewport(ui);
                self.handle_home_snap(ui, default_pos, WINDOW_DEFAULT_SIZE);
                // Right-aligned close hint on the title bar, drawn here while the default font is still
                // active (matches the title text) and before the menu font is pushed.
                draw_title_hint(ui);
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
                                    "Actions" => self.draw_actions_tab(ui, &ctx, pad),
                                    "Settings" => self.draw_settings_tab(ui),  // &mut self: reveal toggle
                                    "Log" => draw_log_tab(ui),
                                    _ => {}
                                });
                        }
                    }
                }
            });
    }

    /// The interactive session-action list, plus a trailing **debug-overlay** toggle. Up/down (arrow
    /// keys or d-pad / left-stick) move the cursor across both (skipping disabled action rows); Enter,
    /// the controller A button, or a click activates the selected row. Activating an action hands it to
    /// the game thread; activating the toggle flips the local debug panel (no game round-trip).
    ///
    /// The overlay owns the combined cursor ([`Overlay::actions_sel`]) rather than the core `Menu`'s,
    /// so the toggle can live in the same nav list as the menu rows without that pure model knowing
    /// about an overlay-local UI control. The menu's own cursor is synced (via `select_index`) only at
    /// the moment of activation.
    fn draw_actions_tab(&mut self, ui: &Ui, ctx: &SessionContext, pad: crate::input::PadEdges) {
        // `rows` indices are 1:1 with the menu's items (actions-only, no filtering), so a row index is
        // a valid `select_index` target. `MenuRow.value` is always `None` here (no setting rows).
        let rows = self.menu.rows(&self.config, ctx);
        let n = rows.len();
        // Combined list = the `n` action rows, then the debug-panel toggle row at index `n` (always
        // enabled). The skip-disabled/wrap stepping reuses the core menu's host-tested helpers so this
        // cursor can't drift from `Menu`'s own nav.
        let total = n + 1;
        let enabled = |i: usize| if i < n { rows[i].enabled } else { true };

        // Repair the cursor if it's out of range or on a now-disabled row (e.g. the session context
        // changed since last frame), landing it on the first enabled row.
        if self.actions_sel >= total || !enabled(self.actions_sel) {
            self.actions_sel = unseamless_core::menu::first_enabled(total, enabled);
        }
        if ui.is_key_pressed(Key::DownArrow) || pad.down {
            self.actions_sel = unseamless_core::menu::step_enabled(self.actions_sel, total, true, enabled);
        }
        if ui.is_key_pressed(Key::UpArrow) || pad.up {
            self.actions_sel = unseamless_core::menu::step_enabled(self.actions_sel, total, false, enabled);
        }
        let mut activate = enter_pressed(ui) || pad.activate;

        let mut clicked = None;
        for (i, row) in rows.iter().enumerate() {
            if row.enabled {
                // Action labels are unique, so each doubles as a stable imgui id.
                if ui.selectable_config(&row.label).selected(i == self.actions_sel).build() {
                    clicked = Some(i);
                }
            } else {
                ui.text_disabled(&row.label);
            }
        }
        // The debug-panel toggle row, last. The on/off state rides in the visible label, but a fixed
        // `###` id keeps the row's imgui identity stable as that text flips (otherwise the id would
        // change each toggle). "Debug panel" — not "overlay" — to avoid colliding with the whole-
        // overlay `[debug] overlay` config switch; this only controls the bottom-left panel.
        ui.separator();
        let toggle_label = format!("Debug panel: {}###debug-panel-toggle", if self.show_debug { "on" } else { "off" });
        if ui.selectable_config(&toggle_label).selected(self.actions_sel == n).build() {
            clicked = Some(n);
        }
        if let Some(i) = clicked {
            self.actions_sel = i;
            activate = true;
        }

        if activate {
            if self.actions_sel == n {
                self.show_debug = !self.show_debug;
            } else {
                // Sync the core menu's cursor to the activated action row, then fire it. actions_only
                // never mutates config on activate; pass a scratch clone (per activation, not per
                // frame) so the `&mut Config` signature is satisfied without touching our snapshot.
                self.menu.select_index(self.actions_sel);
                let mut scratch = self.config.clone();
                if let MenuOutcome::Action(action) = self.menu.activate(&mut scratch, ctx) {
                    self.request_action(action);
                }
            }
        }
    }

    /// Read-only view of every setting and its current value, coloured by whether the host syncs it
    /// across the party (shared) or it's local to this machine. Editing happens in the config file.
    fn draw_settings_tab(&mut self, ui: &Ui) {
        self.draw_password_row(ui);
        ui.separator();
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

    /// The session password at the top of the Settings tab — the matchmaking key everyone in the
    /// party must match. Shown so it can be read off-screen without opening the config file, but
    /// masked behind a Reveal/Hide toggle so it isn't leaked on a stream or screenshot by default.
    /// Amber so it stands out from the synced/local palette below it. The mask uses ASCII `*` (the
    /// menu font is a printable-ASCII subset, so a Unicode bullet would render as a missing glyph).
    fn draw_password_row(&mut self, ui: &Ui) {
        let pw = &self.config.session.password;
        ui.text_colored(rgba(AMBER, 1.0), "Session password:");
        ui.same_line();
        if self.password_revealed {
            ui.text_colored(rgba(AMBER, 1.0), pw);
        } else {
            ui.text_colored(rgba(AMBER, 1.0), "*".repeat(pw.chars().count()));
        }
        ui.same_line();
        if ui.small_button(if self.password_revealed { "Hide" } else { "Reveal" }) {
            self.password_revealed = !self.password_revealed;
        }
        ui.text_disabled("Everyone in your party must match this.");
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
/// have no live session, so this is the not-in-session default — only Host / Join enabled.
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

/// Draw the right-aligned close hint on the utility window's title bar. imgui has no native
/// right-alignment for title text, so we place it manually: measure the text in the current (default)
/// font and put it flush to the title bar's right edge, vertically centered in the bar (`frame_height`).
/// Uses the foreground draw list because inside the build closure the window draw list is clipped to
/// the content region (the title bar is excluded), so a window-draw-list call there wouldn't show. Must
/// be called before the menu font is pushed so the hint matches the title's font. One foreground draw
/// list, dropped at function end — never alive at the same time as `draw_ghost_box`'s or
/// `draw_cursor_marker`'s (all sequential).
fn draw_title_hint(ui: &Ui) {
    let pos = ui.window_pos();
    let size = ui.window_size();
    let text = ui.calc_text_size(CLOSE_HINT);
    let x = pos[0] + size[0] - text[0] - TITLE_HINT_INSET;
    let y = pos[1] + (ui.frame_height() - text[1]) * 0.5;
    let dl = ui.get_foreground_draw_list();
    dl.add_text([x, y], rgba(DIM_GREY, 1.0), CLOSE_HINT);
}

/// Whether a point lies inside the axis-aligned rectangle at `pos` with `size` (inclusive edges).
fn point_in_rect(p: [f32; 2], pos: [f32; 2], size: [f32; 2]) -> bool {
    p[0] >= pos[0] && p[0] <= pos[0] + size[0] && p[1] >= pos[1] && p[1] <= pos[1] + size[1]
}

/// Draw the home-snap ghost box — a faint white fill under a white outline — over the home rectangle,
/// on the foreground draw list (above the dragged window) so it reads as a "drop here" target. Binds
/// one foreground draw list and drops it at function end; safe because `draw_cursor_marker` (the only
/// other foreground-list user) runs later, after the window has closed — never with this one alive.
fn draw_ghost_box(ui: &Ui, pos: [f32; 2], size: [f32; 2]) {
    let max = [pos[0] + size[0], pos[1] + size[1]];
    let dl = ui.get_foreground_draw_list();
    dl.add_rect(pos, max, GHOST_FILL).filled(true).build();
    dl.add_rect(pos, max, GHOST_LINE).thickness(GHOST_LINE_THICKNESS).build();
}

/// Enter (main or keypad) pressed this frame, no key-repeat — one activation per physical press.
fn enter_pressed(ui: &Ui) -> bool {
    ui.is_key_pressed_no_repeat(Key::Enter) || ui.is_key_pressed_no_repeat(Key::KeypadEnter)
}

/// Render a [`DiagnosticReport`] into the current window: each section title in blue, then its
/// `key = value` lines dimmed and indented under it. Values are ASCII (built that way in
/// [`crate::diag::build_report`]).
fn draw_report(ui: &Ui, report: &DiagnosticReport) {
    for (i, section) in report.sections().iter().enumerate() {
        if i > 0 {
            ui.spacing();
        }
        ui.text_colored(rgba(BLUE, 1.0), section.title());
        for (k, v) in section.fields() {
            ui.text_disabled(format!("  {k} = {v}"));
        }
    }
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
