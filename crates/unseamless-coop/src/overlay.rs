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
use hudhook::imgui::draw_list::DrawListMut;
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
const TABS: [&str; 4] = ["Actions", "Settings", "Debug", "Log"];
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

// Overhead nameplates: bright white text with a dark drop shadow (one pixel down-right) so a label
// stays legible over any part of the game world. The projected NDC points come from the game-thread
// feature (see [`crate::nameplates`]); the shadow is the same contrast trick the cursor orb uses.
// Per-label tint comes from the published `NameplateLabel::color` (the peer palette); the overlay only
// owns the shared alpha + the contrast shadow. The text is drawn semi-transparent so a label is present
// but unobtrusive over the world; the near-opaque shadow keeps it legible at that alpha.
const NAMEPLATE_ALPHA: f32 = 0.65;
const NAMEPLATE_SHADOW: [f32; 4] = [0.0, 0.0, 0.0, 0.85];
const NAMEPLATE_SHADOW_OFFSET: f32 = 1.0;
// Dot markers: the distance-LOD dot a far peer's plate degrades to, and the off-screen edge indicator
// dot. Both are a filled colored core ringed by a near-opaque dark outline (the same contrast trick the
// text shadow uses) so a small dot stays legible over any part of the world. The edge dot is a touch
// larger so an at-the-border "teammate is over here" marker reads at a glance. Tune at 2-player.
const NAMEPLATE_DOT_R: f32 = 3.0;
const NAMEPLATE_EDGE_DOT_R: f32 = 4.5;
const NAMEPLATE_DOT_OUTLINE: f32 = 1.5;

/// One inset, in pixels, from the viewport edge, shared by every overlay surface: the top-left
/// watermark (left + top), the top-right notifications (right + top), and the utility window's
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
    /// Whether the Settings tab reveals the session password (vs. masking it). Defaults masked — see
    /// [`draw_secret_row`]'s streamer-mode note — and is toggled by its own Reveal/Hide button.
    /// Present-thread only.
    password_revealed: bool,
    /// Controller→menu edge state: turns the raw pad snapshot into per-frame nav/activate/toggle edges.
    /// Updated once per frame in `render_inner`; Present-thread only (see [`crate::input::PadNav`]).
    pad: crate::input::PadNav,
    /// Whether the bottom-left **debug panel** is shown. Defaults to on in debug builds (`diag`
    /// profile), off in release; toggled from the Debug tab. Independent of `open` and of gameplay
    /// state — unlike the watermark, it stays up during play. Mirrored to [`crate::debug_panel`] each frame
    /// so the game-thread publisher only does work while it's shown. Present-thread only.
    show_debug: bool,
    /// Per-[`DEBUG_CATEGORIES`] toggles: whether each category's full detail is shown in the bottom-right
    /// detail pane. All off by default; set from the Debug tab. Only has visible effect while `show_debug`
    /// is on. Present-thread only.
    debug_details: [bool; DEBUG_CATEGORIES.len()],
    /// Cursor into the Debug tab's row list — the debug-panel on/off toggle at index 0, then one row per
    /// [`DEBUG_CATEGORIES`] entry (the detail toggles). Like [`actions_sel`], owns selection so arrow/d-pad
    /// nav spans the list. Present-thread only.
    debug_sel: usize,
    /// Cursor into the Actions tab's combined list — the menu's action rows followed by a trailing
    /// always-enabled "Export diagnostics" row (index `== menu rows`). Owns selection across all of them
    /// (the core `Menu`'s own cursor is only synced to it at activation), so arrow/d-pad nav spans the
    /// trailing row too.
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
    /// Our DLL's module handle, used to locate the install dir for the "Export diagnostics" button
    /// (which writes the shareable bundle next to the config/logs). Resolved fresh per export rather
    /// than cached as a path, so it can't go stale. Present-thread only.
    module: usize,
}

impl Overlay {
    fn new(module: usize) -> Self {
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
            // stripped release, where it's an opt-in toggled from the Debug tab.
            show_debug: cfg!(debug_assertions),
            debug_details: [false; DEBUG_CATEGORIES.len()],
            debug_sel: 0,
            actions_sel: 0,
            home_dragging: false,
            last_win_pos: None,
            home_snap: false,
            module,
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
        // Toggle on backtick or the RB+L3+R3 chord (no-repeat: one open/close per press).
        if ui.is_key_pressed_no_repeat(TOGGLE_KEY) || pad.toggle {
            self.open = !self.open;
            if self.open {
                // Home the Actions cursor; `draw_actions_tab` repairs it to the first enabled row (0
                // can be a disabled action when opened mid-session). It owns nav, syncing the core
                // `Menu` only at activation, so we reset its cursor rather than the menu's here.
                self.actions_sel = 0;
            }
        }
        // Esc or B (Back/Cancel) closes the menu, but only while it's open — when closed they're game
        // inputs (Esc = pause, B = game action), and the input suppressor keeps the game from seeing
        // them while we're open. Esc is intentionally NOT advertised in the title hint (backtick is the
        // one documented toggle); it's just a familiar "get me out" key.
        if self.open && (pad.cancel || ui.is_key_pressed_no_repeat(Key::Escape)) {
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
        // Overhead peer nameplates: screen-space labels the game-thread feature
        // (`crate::features::nameplates`) projected and published to `crate::nameplates`. Drawn over
        // the world but behind our own windows; a no-op when nothing's published (off / no peers).
        self.draw_nameplates(ui);
        // Branded corner stamp — only off the playfield (title/main menu, character select, loading),
        // never a persistent in-play banner. The game-thread probe (`crate::features::playstate`)
        // publishes the flag; we read it non-blocking here. Suppressed whenever the debug panel is up:
        // it grows from the bottom-left and the watermark sits top-left, so off the playfield (where
        // both are visible) a tall panel overlaps the stamp. The panel wins — it's the live surface.
        if !crate::playstate::in_gameplay() && !self.show_debug {
            self.draw_watermark(ui);
        }
        // Live debug panel (bottom-left): shown whenever toggled on, including during gameplay (unlike
        // the watermark). Mirror its visibility to the game thread every frame so the publisher only
        // does work while it's shown; then draw from the snapshot it posts.
        crate::debug_panel::set_visible(self.show_debug);
        if self.show_debug {
            // One non-blocking snapshot read per frame, shared by both panes: halves the per-frame
            // clone and guarantees the concise panel and the detail pane render the same published
            // report (two independent reads could straddle a ~10 Hz publish and disagree by a frame).
            let report = crate::debug_panel::snapshot();
            self.draw_debug_panel(ui, report.as_ref());
            // Opt-in per-category detail, balanced into the opposite (bottom-right) corner.
            self.draw_debug_detail_pane(ui, report.as_ref());
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
        // Top-right, the opposite corner from the watermark (top-left). Anchored by the window's own
        // top-right corner (pivot 1,0) so it stays put regardless of the auto-sized content width.
        // Shares the right edge with Steam's bottom-right toasts but sits at the top, clear of them.
        let disp = ui.io().display_size;
        ui.window("##unseamless-notifications")
            .position([disp[0] - OVERLAY_MARGIN, OVERLAY_MARGIN], Condition::Always)
            .position_pivot([1.0, 0.0])
            .bg_alpha(PASSIVE_BG_ALPHA)
            .flags(passive_window_flags())
            .build(|| {
                // Right-align every line to the window's auto-sized right edge — the corner it's
                // pinned to — so the notifications hug the screen edge instead of ragged-left. The
                // target width is the widest line across banners + toasts (they share this window), so
                // each shorter line is offset right by the difference.
                let max_w = banners
                    .iter()
                    .map(|b| b.message.as_str())
                    .chain(toasts.iter().map(|t| t.message.as_str()))
                    .map(|m| ui.calc_text_size(m)[0])
                    .fold(0.0_f32, f32::max);
                draw_banners(ui, &banners, max_w);
                draw_toasts(ui, &toasts, max_w);
            });
    }

    /// Draw overhead peer nameplates from the projected labels the game-thread feature publishes
    /// ([`crate::nameplates`]). Reads them non-blocking; maps each NDC point to pixels with this
    /// frame's `display_size` (the projection deliberately stops at NDC — see
    /// [`unseamless_core::projection`]) and draws centered text with a drop shadow. Uses the
    /// **background** draw list so labels sit over the game world but behind our own windows (the
    /// utility menu stays on top). A no-op when the list is empty or momentarily contended, so it's
    /// cheap on the present hook.
    fn draw_nameplates(&self, ui: &Ui) {
        let Some(mut labels) = crate::nameplates::snapshot() else {
            return; // contended this frame, or not initialized — skip
        };
        if labels.is_empty() {
            return; // off, or no peers visible — nothing to draw
        }
        // Paint farthest-first so a nearer peer's label draws on top when two overlap on screen.
        labels.sort_by(|a, b| b.depth.total_cmp(&a.depth));
        let disp = ui.io().display_size;
        // Crisp menu font for the labels (held to function end); toasts keep the compact default.
        let _font = self.font.as_ref().map(|f| ui.push_font(f.0));
        // One background draw list, reused for every label and dropped at function end. It's a
        // different list from the foreground one `draw_cursor_marker`/`draw_title_hint` bind, and this
        // runs before the utility window's `draw_ghost_box` (the other background-list user), so the
        // imgui-rs one-live-instance rule isn't violated.
        let dl = ui.get_background_draw_list();
        for n in &labels {
            let p = unseamless_core::projection::ndc_to_screen(n.ndc, disp);
            let color = rgba(n.color, NAMEPLATE_ALPHA);
            match n.kind {
                // Off-screen peer: a colored dot pinned to the screen border, pointing toward them.
                crate::nameplates::NameplateKind::Edge => draw_nameplate_dot(&dl, p, NAMEPLATE_EDGE_DOT_R, color),
                // On-screen peer: full text up close, degrading to a colored dot past the LOD distance
                // (the `is_dot_lod` threshold) — switching representation rather than scaling the bitmap
                // font, which would turn mushy. The dot uses the same per-peer palette color.
                crate::nameplates::NameplateKind::Plate => {
                    if unseamless_core::projection::is_dot_lod(n.depth, unseamless_core::projection::DEFAULT_DOT_DISTANCE_M) {
                        draw_nameplate_dot(&dl, p, NAMEPLATE_DOT_R, color);
                    } else {
                        // Center the text horizontally on the projected point.
                        let x = p[0] - ui.calc_text_size(&n.text)[0] * 0.5;
                        dl.add_text([x + NAMEPLATE_SHADOW_OFFSET, p[1] + NAMEPLATE_SHADOW_OFFSET], NAMEPLATE_SHADOW, &n.text);
                        dl.add_text([x, p[1]], color, &n.text);
                    }
                }
            }
        }
    }

    /// Draw the branded corner stamp — mod name + version + the backtick hint — anchored to the
    /// top-left. Stands in for the vanilla "App Ver. / OFFLINE" version block (which we can't edit:
    /// its text is FMG, uncharted by the SDK at our pin). Sits top-left, the opposite corner from the
    /// notifications (top-right) and clear of Steam's own bottom-right notifications. Gated by the
    /// caller to off-the-playfield only. Borderless and input-transparent like the notifications
    /// surface; uses our crisp menu font.
    fn draw_watermark(&self, ui: &Ui) {
        // Anchor by the window's own top-left corner (default pivot 0,0) at a fixed inset from the
        // viewport's top-left.
        ui.window("##unseamless-watermark")
            .position([OVERLAY_MARGIN, OVERLAY_MARGIN], Condition::Always)
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
                ui.text_disabled("Press ` or RB+L3+R3 to open the menu");
            });
    }

    /// Draw the live **debug panel** — a read-only, bottom-left passive surface rendering the
    /// diagnostic snapshot the game-thread publisher posts ([`crate::debug_panel`]). It's the same
    /// [`DiagnosticReport`] the log dumps produce, shown live (build / session / features / scaling /
    /// runtime).
    ///
    /// **Deliberately exempt from streamer-mode masking** (see [`draw_secret_row`]): this panel shows
    /// identifying info — the Steam ID and all — *in the clear*, because it's a diagnostics surface, not
    /// the always-available player UI, and it's opt-in on release builds (off by default there; on in
    /// debug builds or when toggled from the Debug tab via [`Overlay::show_debug`]). If you ever make
    /// this panel visible by default on release, revisit that — it would then leak identity on stream.
    ///
    /// Bottom-left is the one free corner (watermark top-left, notifications top-right,
    /// Steam's toasts bottom-right). Anchored by its own bottom-left corner (pivot 0,1) so it grows
    /// upward from a fixed inset and never runs off the bottom. Borderless + input-transparent like the
    /// other passive surfaces; renders the caller's shared snapshot and shows a "gathering" line before
    /// the first publish or on contention. Drawn in the compact default font (like the toasts) rather
    /// than the crisp menu font — the smaller type suits a dense, glanceable info panel.
    fn draw_debug_panel(&self, ui: &Ui, report: Option<&DiagnosticReport>) {
        let disp = ui.io().display_size;
        ui.window("##unseamless-debug")
            .position([OVERLAY_MARGIN, disp[1] - OVERLAY_MARGIN], Condition::Always)
            .position_pivot([0.0, 1.0])
            .bg_alpha(PASSIVE_BG_ALPHA)
            .flags(passive_window_flags())
            .build(|| {
                match report {
                    Some(report) => draw_groups(ui, &concise_groups(report)),
                    None => ui.text_disabled("debug panel: gathering..."),
                }
            });
    }

    /// Draw the **debug detail pane** — a second passive surface, bottom-**right**, that appears only
    /// when the debug panel is on and at least one [`DEBUG_CATEGORIES`] toggle is enabled (from the Debug
    /// tab). It shows the *full* `fields` of each enabled category's sections, while the concise panel
    /// (bottom-left) keeps showing their rollups — so detail is opt-in and balanced into the opposite
    /// corner. Same passive/click-through styling, and renders the same shared snapshot the concise panel
    /// got; silently skips the frame when there's nothing enabled to show (so it never draws an empty box).
    fn draw_debug_detail_pane(&self, ui: &Ui, report: Option<&DiagnosticReport>) {
        if !self.debug_details.iter().any(|&on| on) {
            return; // no category enabled — nothing to expand
        }
        let Some(report) = report else {
            return; // not published yet or momentarily contended
        };
        let groups = detail_groups(report, &self.debug_details);
        if groups.is_empty() {
            return; // enabled categories have no matching section in this snapshot
        }
        // Bottom-right, anchored by its own bottom-right corner (pivot 1,1) so it grows up-and-left from a
        // fixed inset, mirroring the concise panel's bottom-left anchor. Shares the right edge with Steam's
        // bottom-right toasts, but it's opt-in (only while details are on), so the occasional overlap is fine.
        let disp = ui.io().display_size;
        ui.window("##unseamless-debug-detail")
            .position([disp[0] - OVERLAY_MARGIN, disp[1] - OVERLAY_MARGIN], Condition::Always)
            .position_pivot([1.0, 1.0])
            .bg_alpha(PASSIVE_BG_ALPHA)
            .flags(passive_window_flags())
            .build(|| draw_groups(ui, &groups));
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
                                // NO_NAV on the child too: every tab drives its own up/down (Actions
                                // moves its selection; Settings/Log scroll the view via `scroll_pane`),
                                // so imgui's own keyboard nav/scroll must not also fire on the arrows.
                                .flags(WindowFlags::NO_NAV)
                                .build(|| match label {
                                    "Actions" => self.draw_actions_tab(ui, &ctx, pad),
                                    "Settings" => self.draw_settings_tab(ui, pad), // &mut self: reveal toggle
                                    "Debug" => self.draw_debug_tab(ui, pad),
                                    "Log" => draw_log_tab(ui, pad),
                                    _ => {}
                                });
                        }
                    }
                }
            });
    }

    /// The interactive session-action list. Up/down (arrow keys or d-pad / left-stick) move the cursor
    /// across the action rows (skipping disabled ones); Enter, the controller A button, or a click
    /// activates the selected row, handing the action to the game thread.
    ///
    /// The overlay owns the cursor ([`Overlay::actions_sel`]) rather than the core `Menu`'s; the menu's
    /// own cursor is synced (via `select_index`) only at the moment of activation. (The debug-panel
    /// toggle and the Export-diagnostics action that used to trail this list now live in the Debug tab.)
    fn draw_actions_tab(&mut self, ui: &Ui, ctx: &SessionContext, pad: crate::input::PadEdges) {
        // `rows` indices are 1:1 with the menu's items (actions-only, no filtering), so a row index is
        // a valid `select_index` target. `MenuRow.value` is always `None` here (no setting rows). The
        // skip-disabled/wrap stepping reuses the core menu's host-tested helpers so this cursor can't
        // drift from `Menu`'s own nav.
        let rows = self.menu.rows(&self.config, ctx);
        let total = rows.len();
        let enabled = |i: usize| rows[i].enabled;

        // Repair the cursor if it's out of range or on a now-disabled row (e.g. the session context
        // changed since last frame), landing it on the first enabled row.
        if self.actions_sel >= total || (total > 0 && !enabled(self.actions_sel)) {
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
        if let Some(i) = clicked {
            self.actions_sel = i;
            activate = true;
        }

        if activate && self.actions_sel < total {
            // Sync the core menu's cursor to the activated action row, then fire it. actions_only never
            // mutates config on activate; pass a scratch clone (per activation, not per frame) so the
            // `&mut Config` signature is satisfied without touching our snapshot.
            self.menu.select_index(self.actions_sel);
            let mut scratch = self.config.clone();
            if let MenuOutcome::Action(action) = self.menu.activate(&mut scratch, ctx) {
                self.request_action(action);
            }
        }
    }

    /// The **Debug** tab: a navigable list mirroring the Actions tab. Row 0 turns the whole debug panel
    /// on/off (moved here from Actions — it's where it naturally belongs); rows 1..=N are one per
    /// [`DEBUG_CATEGORIES`] entry, each toggling whether that category's full detail shows in the
    /// bottom-right pane; the final row is "Export diagnostics" (also moved here from Actions). Detail
    /// rows are greyed (non-selectable) while the panel is off, since they have no effect then — but the
    /// panel toggle and Export stay enabled regardless, so a friend can export with the panel off. Up/down
    /// (arrows or d-pad / left-stick) move the cursor, skipping disabled rows; Enter / the A button / a
    /// click activates the selected row. Owns its own cursor ([`Overlay::debug_sel`]), like the Actions tab.
    fn draw_debug_tab(&mut self, ui: &Ui, pad: crate::input::PadEdges) {
        // Rows: 0 = panel on/off (always enabled); 1..=N = per-category detail toggles (enabled only while
        // the panel is on); the trailing `export_row` = Export diagnostics (always enabled). `show_debug`
        // is captured into a local so the `enabled` closure doesn't borrow `self` across the toggle
        // mutation below. Reuses the core menu's host-tested skip-disabled stepping.
        let show_debug = self.show_debug;
        let export_row = 1 + DEBUG_CATEGORIES.len();
        let total = export_row + 1;
        let enabled = |i: usize| i == 0 || i == export_row || show_debug;

        if self.debug_sel >= total || !enabled(self.debug_sel) {
            self.debug_sel = unseamless_core::menu::first_enabled(total, enabled);
        }
        if ui.is_key_pressed(Key::DownArrow) || pad.down {
            self.debug_sel = unseamless_core::menu::step_enabled(self.debug_sel, total, true, enabled);
        }
        if ui.is_key_pressed(Key::UpArrow) || pad.up {
            self.debug_sel = unseamless_core::menu::step_enabled(self.debug_sel, total, false, enabled);
        }
        let mut activate = enter_pressed(ui) || pad.activate;
        let mut clicked = None;

        // Panel on/off. State rides in the visible label; a fixed `###` id keeps imgui identity stable as
        // the text flips. "Debug panel" — not "overlay" — to avoid colliding with the `[debug] overlay`
        // config switch; this only controls the in-game panel.
        let panel_label =
            format!("Debug panel: {}###debug-panel-toggle", if show_debug { "on" } else { "off" });
        if ui.selectable_config(&panel_label).selected(self.debug_sel == 0).build() {
            clicked = Some(0);
        }

        ui.separator();
        if !show_debug {
            ui.text_disabled("Turn the panel on to enable detail.");
        }
        // One detail toggle per category; greyed (non-selectable) while the panel is off.
        for (i, cat) in DEBUG_CATEGORIES.iter().enumerate() {
            let row = i + 1;
            let label = format!(
                "Detail - {}: {}###debug-detail-{i}",
                cat.label,
                if self.debug_details[i] { "on" } else { "off" }
            );
            if show_debug {
                if ui.selectable_config(&label).selected(self.debug_sel == row).build() {
                    clicked = Some(row);
                }
            } else {
                ui.text_disabled(&label);
            }
        }

        // "Export diagnostics": writes the one-file shareable bundle (report + log tail, SteamIDs
        // scrubbed) next to the config/logs. The whole point is a single action a non-technical friend
        // can do — and one that works with NO peer connected, so it captures the failed-to-link case that
        // log-forwarding (which needs the link up) never can. Always enabled (independent of the panel),
        // and controller-navigable, so a friend on a pad can export with no mouse.
        ui.separator();
        if ui.selectable_config("Export diagnostics###export-diag").selected(self.debug_sel == export_row).build() {
            clicked = Some(export_row);
        }

        if let Some(i) = clicked {
            self.debug_sel = i;
            activate = true;
        }
        if activate && enabled(self.debug_sel) {
            if self.debug_sel == 0 {
                self.show_debug = !self.show_debug;
            } else if self.debug_sel == export_row {
                self.export_diagnostics();
            } else {
                self.debug_details[self.debug_sel - 1] ^= true;
            }
        }
    }

    /// Write the one-file shareable diagnostics bundle to the install's `unseamless-coop/` folder
    /// (next to the config and `logs/`), then toast the path. Everything it reads is Present-thread
    /// safe — the live config snapshot, the last published debug-panel report ([`crate::debug_panel`]),
    /// and the in-memory log tail ([`crate::logbuf`]) — so it deliberately does **not** read game
    /// singletons (that's the game thread's job) and does **not** depend on a live co-op transport.
    /// That's what makes it survive a non-link: the friend test most needs to capture the case where
    /// the side-channel never came up, which rung-2 log-forwarding (only live once linked) can't.
    ///
    /// The gather (config clone + module handle) happens here; the assemble + **disk write** + toast
    /// run on a short-lived detached thread (like [`crate::steam`]'s resolver), so the Present hook
    /// never blocks on a `create_dir_all`/`fs::write` (which Defender can stall on a fresh file) or on
    /// [`crate::notify`]'s blocking lock — the present thread must never block on the game thread (see
    /// notify.rs). Off the present hook there's no FFI boundary, so a panic in the worker just ends the
    /// worker (logged by the panic hook); no `catch_unwind` firewall is needed. Plain voice for this
    /// diagnostic message (per CLAUDE.md), not ER tone.
    fn export_diagnostics(&self) {
        // Snapshot the only present-thread-owned inputs, then hand off. `Config` is Clone, `module` is
        // Copy; everything else the worker reads is a process-global static reachable from any thread.
        let config = self.config.clone();
        let module = self.module;
        std::thread::spawn(move || export_bundle_to_disk(&config, module));
    }

    /// Read-only view of every setting and its current value, coloured by whether the host syncs it
    /// across the party (shared) or it's local to this machine. Editing happens in the config file.
    /// Up/down (arrow keys or d-pad / left-stick) scroll this read-only pane — it has no selectable
    /// rows of its own, so the directional input drives the view, unlike the Actions tab.
    fn draw_settings_tab(&mut self, ui: &Ui, pad: crate::input::PadEdges) {
        scroll_pane(ui, pad);
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

    /// The session password at the top of the Settings tab — the matchmaking key everyone in the party
    /// must match. Drawn through [`draw_secret_row`] so it's masked by default with its own Reveal and
    /// Copy buttons, identical to the Steam ID below. Amber so it stands out from the synced/local
    /// palette further down.
    fn draw_password_row(&mut self, ui: &Ui) {
        // Clone the small string out so the row's value borrow doesn't tangle with the `&mut` reveal flag.
        let pw = self.config.session.password.clone();
        draw_secret_row(ui, "Session password:", "session password", &pw, &mut self.password_revealed, AMBER);
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
        // FFI firewall (see `render`): hudhook calls this across its `extern "system"` present-hook
        // boundary with no catch of its own, so a panic here would unwind into vkd3d/the game — UB
        // under `panic = "unwind"`. Build the fonts inside the catch and only commit on success;
        // a panic disables the overlay for the session rather than risking the boundary.
        let baked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Called once before hudhook bakes the font atlas, so fonts added here get rasterized.
            let fonts = ctx.fonts();
            // Keep the compact default (ProggyClean) as the atlas default so the passive toasts stay
            // small; our crisp subset is an extra font pushed only for the utility window. Index 0
            // stays the compact 13px default; the menu font is Spleen 8x16 baked at its native 16px
            // with oversampling off + pixel snap, so it stays crisp.
            fonts.add_font(&[FontSource::DefaultFontData { config: None }]);
            fonts.add_font(&[FontSource::TtfData {
                data: MENU_FONT,
                size_pixels: MENU_FONT_SIZE,
                config: Some(FontConfig {
                    oversample_h: 1,
                    oversample_v: 1,
                    pixel_snap_h: true,
                    ..FontConfig::default()
                }),
            }])
        }));
        match baked {
            Ok(id) => self.font = Some(SyncFontId(id)),
            Err(_) => {
                self.disabled = true;
                // Contained log (see crate::logger::error_contained): the recovery arm runs in the same
                // present-hook frame, so a logging panic would unwind across hudhook's FFI boundary.
                crate::logger::error_contained(format_args!(
                    "overlay: initialize panicked; overlay disabled for the rest of the session"
                ));
            }
        }
    }

    fn before_render<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        if self.disabled {
            return; // a disabled overlay is fully inert (mirrors `render`'s guard)
        }
        // FFI firewall (see `render`): also called across hudhook's present-hook boundary. The body
        // can't realistically panic (one bool write), but catch it for the same soundness reason.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Keep imgui's own arrow cursor off — we draw our own marker (`draw_cursor_marker`) at the
            // mouse hotspot instead, which complements ER's cursor rather than clashing with a second.
            ctx.io_mut().mouse_draw_cursor = false;
        }))
        .is_err()
        {
            self.disabled = true;
            // Same input-suppression release as `render`'s disable arm: if we die here while the window
            // is open, `render` then early-returns on `disabled` and never runs its own cleanup, so the
            // DirectInput block + WndProc filter would stay latched for the session. Clear both here.
            self.open = false;
            crate::input::set_blocked(false);
            crate::logger::error_contained(format_args!(
                "overlay: before_render panicked; overlay disabled for the rest of the session"
            ));
        }
    }

    fn message_filter(&self, _io: &Io) -> MessageFilter {
        if self.disabled {
            return MessageFilter::empty(); // inert when disabled (mirrors `render`/`before_render`)
        }
        // FFI firewall: hudhook samples this in `prepare_render` on the Present thread (the same
        // `extern "system"` present-hook boundary as `render`), so a panic would unwind across it. It
        // only loads `self.open` (a bool — can't panic), but catch defensively and default to *not*
        // filtering (empty) so a panic can never strand window input.
        //
        // While the utility window is open, swallow keyboard/mouse/raw input so the game doesn't also
        // act on it while we navigate. hudhook feeds every message to imgui *before* consulting this, so
        // backtick-to-close still registers. Two caveats, both to confirm on the rig: (1) the filter is
        // sampled a frame late, so the first frame on open leaks one message to the game (harmless —
        // backtick is unbound in ER); (2) this blocks only WndProc input — if the game reads movement
        // out-of-band (DirectInput/XInput/GetAsyncKeyState), `MessageFilter` can't stop it, so verify
        // that movement/attack actually halt with the window open.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.open { MessageFilter::InputAll } else { MessageFilter::empty() }
        }))
        .unwrap_or_else(|_| MessageFilter::empty())
    }

    fn render(&mut self, ui: &mut Ui) {
        if self.disabled {
            return;
        }
        // FFI firewall: hudhook calls `render` from its `extern "system"` present hook with no catch of
        // its own, so a panic here would unwind across that boundary into vkd3d/the game — UB, and now
        // load-bearing in the player's build since every shipped profile is `panic = "unwind"` (see
        // docs/FFI-UNWIND-AUDIT.md). Mirror app.rs's per-task catch: on a render panic, disable the
        // overlay for the session (the panic hook already logged the backtrace) rather than risk the game.
        let ui: &Ui = ui;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.render_inner(ui))).is_err() {
            self.disabled = true;
            // Release both input-suppression paths if we died mid-frame while open: the DirectInput
            // block (set_blocked) AND the WndProc filter — `message_filter` reads `self.open`, so clear
            // it too, or it would keep swallowing all window input for the rest of the session.
            self.open = false;
            crate::input::set_blocked(false);
            crate::logger::error_contained(format_args!(
                "overlay: render panicked; overlay disabled for the rest of the session"
            ));
        }
    }
}

/// The current session context for menu gating, assembled from the live process state (all reads are
/// non-blocking atomics, safe from the Present thread): the co-op session lifecycle
/// ([`crate::coop::session_context`] → `in_session`/`is_host`), Steam readiness
/// ([`crate::steam_ready::is_ready`]), and whether the player is loaded into the world
/// ([`crate::playstate::in_gameplay`]). Open World / Join world are enabled only when Steam is up, the
/// player is in-game, and no session is active; Leave only while in a session.
fn session_context() -> SessionContext {
    let flags = crate::coop::session_flags();
    SessionContext {
        in_session: flags.in_session,
        is_host: flags.is_host,
        steam_ready: crate::steam_ready::is_ready(),
        in_game: crate::playstate::in_gameplay(),
    }
}

/// Draw a nameplate marker dot — a filled colored core ringed by a near-opaque dark outline (the same
/// contrast trick the text shadow uses) so a small dot stays legible over any part of the world. Used
/// for both the distance-LOD dot a far plate degrades to and the off-screen edge indicator. Added to the
/// caller's already-bound background draw list (the same `dl` the text labels use), so it never binds a
/// second draw list — imgui-rs's one-live-instance rule stays satisfied.
fn draw_nameplate_dot(dl: &DrawListMut, center: [f32; 2], r: f32, color: [f32; 4]) {
    dl.add_circle(center, r + NAMEPLATE_DOT_OUTLINE, NAMEPLATE_SHADOW).filled(true).build();
    dl.add_circle(center, r, color).filled(true).build();
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
/// list, dropped at function end — never alive at the same time as `draw_cursor_marker`'s (the other
/// foreground-list user; sequential). `draw_ghost_box` uses the *background* list, so it never clashes.
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
/// on the **background** draw list (behind every imgui window) so the dragged window stays on top of
/// it: it reads as a "drop here" target the window sinks toward, not a box painted over the window.
/// Still above the game, since the background list draws after the game frame. Binds one background
/// draw list (a different list from the foreground one `draw_title_hint`/`draw_cursor_marker` use, so
/// no mutual-exclusion concern) and drops it at function end.
fn draw_ghost_box(ui: &Ui, pos: [f32; 2], size: [f32; 2]) {
    let max = [pos[0] + size[0], pos[1] + size[1]];
    let dl = ui.get_background_draw_list();
    dl.add_rect(pos, max, GHOST_FILL).filled(true).build();
    dl.add_rect(pos, max, GHOST_LINE).thickness(GHOST_LINE_THICKNESS).build();
}

/// Enter (main or keypad) pressed this frame, no key-repeat — one activation per physical press.
fn enter_pressed(ui: &Ui) -> bool {
    ui.is_key_pressed_no_repeat(Key::Enter) || ui.is_key_pressed_no_repeat(Key::KeypadEnter)
}

/// Horizontal gap, in pixels, added past the widest rendered line to set the right column's x, so it
/// starts at a fixed offset regardless of content width. Comfortably larger than the window padding
/// (~8px at the default style) that offsets the left column's content origin, so the columns stay clear
/// of each other — the effective inter-column gap is this minus that padding.
const DEBUG_PANEL_COL_GAP: f32 = 28.0;

/// A debug **detail category**: a label for the Debug tab's toggle list, and the report section titles
/// whose full `fields` it reveals in the bottom-right detail pane when enabled. Each named section is
/// rolled up in the always-on concise panel (so the summary stays visible) and expanded here on demand.
struct DebugCategory {
    label: &'static str,
    sections: &'static [&'static str],
}

/// The detail categories listed in the Debug tab, in order. Each mirrors a verbose section the concise
/// panel rolls up — toggling one expands that section's full detail into the bottom-right pane. Section
/// titles must match those built in [`crate::diag::build_report`].
const DEBUG_CATEGORIES: [DebugCategory; 3] = [
    DebugCategory { label: "Connection", sections: &["coop_connect", "session"] },
    DebugCategory { label: "Features", sections: &["features"] },
    DebugCategory { label: "Player status", sections: &["status"] },
];

/// A titled block of `key = value` rows for the two-column debug layout — one report section rendered as
/// either its condensed `summary` (concise panel) or its full `fields` (detail pane), the caller's
/// choice. Borrows from a live snapshot, so it's frame-scoped.
struct ReportGroup<'a> {
    title: &'a str,
    rows: &'a [(String, String)],
}

/// Render a list of [`ReportGroup`]s into the current window as a compact two-column layout. The groups
/// fill **column-major**: partitioned on group boundaries into a heavier LEFT column (filled first, top
/// to bottom) and a lighter RIGHT one, each drawn as its own vertical stack — so a group's rows never
/// split across columns. Both columns are **bottom-aligned** (the shorter right column is top-padded),
/// pooling all the unused space in the top-right corner. Halves the height versus one tall stack while
/// using the wide bottom-left space. Keys/values stay left-aligned; values are ASCII (built that way in
/// [`crate::diag::build_report`]). Shared by the concise panel and the detail pane.
fn draw_groups(ui: &Ui, groups: &[ReportGroup]) {
    if groups.is_empty() {
        return;
    }
    // A group's vertical extent in text lines: its title plus one per row.
    let group_lines = |g: &ReportGroup| 1 + g.rows.len();

    // Per-column rendered height: one line per group title + row, plus one inter-group gap per boundary.
    // Partition AND bottom-align use this same metric, so "heavier" and "taller" coincide by construction.
    let line_h = ui.text_line_height_with_spacing();
    // The per-boundary gap is the *measured* advance of `ui.spacing()`, not an assumed `item_spacing.y`:
    // `ui.spacing()` advances the cursor by its own rule, so deriving the gap from the style would leave
    // a residual that scales with each column's group count (the columns hold different group counts,
    // so it would NOT cancel). Probing the real advance — then restoring the cursor so the layout is
    // untouched — makes the per-boundary term identical to what's rendered, so it cancels exactly in the
    // top-pad difference and the columns bottom-align to the pixel no matter how the groups split.
    let gap = {
        let probe = ui.cursor_pos();
        ui.spacing();
        let advance = ui.cursor_pos()[1] - probe[1];
        ui.set_cursor_pos(probe); // undo the probe — measurement only, no layout shift
        advance
    };
    let column_height = |col: &[ReportGroup]| {
        let lines: usize = col.iter().map(group_lines).sum();
        lines as f32 * line_h + col.len().saturating_sub(1) as f32 * gap
    };

    // Partition on group boundaries by rendered height: the first split where the left column is at
    // least as tall as the right. Left height grows / right height shrinks as the split moves right, so
    // this is the most balanced split with left >= right — left is the heavier column and the pad below
    // is non-negative by construction. Falls back to `split == groups.len()` (everything left, a single
    // column) for one group, or the degenerate case where one group out-measures all the rest.
    let mut split = groups.len();
    for k in 1..groups.len() {
        if column_height(&groups[..k]) >= column_height(&groups[k..]) {
            split = k;
            break;
        }
    }
    let (left, right) = groups.split_at(split);

    // Right column's x = widest rendered line (title or `  key = value`) across every group, plus a
    // gap. One pitch measured over all groups keeps the right column clear of the left regardless of
    // which group is widest (see DEBUG_PANEL_COL_GAP on the window-padding caveat).
    let line_width = |text: &str| ui.calc_text_size(text)[0];
    let mut pitch = 0.0_f32;
    for g in groups {
        pitch = pitch.max(line_width(g.title));
        for (k, v) in g.rows {
            pitch = pitch.max(line_width(&format!("  {k} = {v}")));
        }
    }
    pitch += DEBUG_PANEL_COL_GAP;

    ui.group(|| draw_group_column(ui, left));
    if !right.is_empty() {
        ui.same_line_with_pos(pitch);
        ui.group(|| {
            // Top-pad the lighter right column by the height difference so its bottom lines up with the
            // left's — pushing the empty space into the top-right corner.
            let pad = (column_height(left) - column_height(right)).max(0.0);
            if pad > 0.0 {
                ui.dummy([0.0, pad]);
            }
            draw_group_column(ui, right);
        });
    }
}

/// Draw one column of the debug layout: each group's title in blue, then its `key = value` lines dimmed
/// and indented, with a blank gap between groups.
fn draw_group_column(ui: &Ui, groups: &[ReportGroup]) {
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            ui.spacing();
        }
        ui.text_colored(rgba(BLUE, 1.0), group.title);
        for (k, v) in group.rows {
            ui.text_disabled(format!("  {k} = {v}"));
        }
    }
}

/// Concise view of a report: every section, condensed to its `summary` where it has one (verbose
/// sections — features / status / coop_connect / session) and shown in full otherwise. This is the
/// always-on bottom-left panel; the per-category full detail lives in the bottom-right pane.
fn concise_groups(report: &DiagnosticReport) -> Vec<ReportGroup<'_>> {
    report
        .sections()
        .iter()
        .map(|s| ReportGroup {
            title: s.title(),
            rows: if s.has_summary() { s.summary() } else { s.fields() },
        })
        .collect()
}

/// Full detail for the enabled categories: each present section named by an enabled [`DEBUG_CATEGORIES`]
/// entry, in full `fields`. Empty when nothing is enabled or no matching section is in the snapshot
/// (e.g. `coop_connect` only exists during a connect attempt) — then the detail pane isn't drawn.
fn detail_groups<'a>(report: &'a DiagnosticReport, enabled: &[bool]) -> Vec<ReportGroup<'a>> {
    let mut groups = Vec::new();
    for (cat, &on) in DEBUG_CATEGORIES.iter().zip(enabled) {
        if !on {
            continue;
        }
        for &title in cat.sections {
            if let Some(s) = report.sections().iter().find(|s| s.title() == title) {
                groups.push(ReportGroup { title: s.title(), rows: s.fields() });
            }
        }
    }
    groups
}

/// Draw a **sensitive identity row**: a label, the value masked behind `*` by default, a Reveal/Hide
/// toggle, and a Copy button (which copies the *real* value even while masked). Shared by the session
/// password and the Steam ID so the two behave identically — DRY, and one place to enforce the policy.
///
/// ## Streamer-mode-by-default (the overlay's only mode)
/// Treat every identifying value a *player-facing* overlay surface shows as if a stream or screenshot is
/// always capturing it: mask it by default, reveal only on an explicit per-row Reveal click. Copy still
/// copies the real value while masked, so a player can share their ID without ever putting it on screen.
/// Any new identifying field on a player-facing surface (this utility window, notifications) should go
/// through this helper, or follow the same mask-by-default rule — never drawn in the clear.
///
/// The **debug panel is the deliberate exception**: it renders the full diagnostic report (Steam ID and
/// all) unobscured, because it's a diagnostics surface rather than a player-facing one, and it's opt-in
/// on release builds — off by default there, on only in debug builds or when explicitly toggled (see
/// [`Overlay::draw_debug_panel`] / [`Overlay::show_debug`]). So the "don't show identifying info" rule is
/// about the always-available player UI, not the debug-gated panel.
///
/// `noun` is the human name used in the copy toast and (as a slug) to keep each row's imgui button ids
/// unique. The mask uses ASCII `*` — the menu font is a printable-ASCII subset, so a Unicode bullet would
/// render as a missing glyph.
fn draw_secret_row(ui: &Ui, label: &str, noun: &str, value: &str, revealed: &mut bool, color: [f32; 3]) {
    ui.text_colored(rgba(color, 1.0), label);
    ui.same_line();
    if *revealed {
        ui.text_colored(rgba(color, 1.0), value);
    } else {
        // Mask allocation-free: slice a fixed `*` run to the value's length rather than rebuilding a
        // `String` every frame (this row redraws at ~60Hz on the Settings tab). The cap dwarfs any
        // real secret (Steam IDs are 17 chars, passwords a handful); an implausibly long value just
        // masks up to the cap, which still hides it.
        const MASK: &str = "****************************************************************"; // 64 '*'
        let count = value.chars().count().min(MASK.len());
        ui.text_colored(rgba(color, 1.0), &MASK[..count]);
    }
    ui.same_line();
    // `###`-suffixed ids: the visible label flips (Reveal/Hide) without changing imgui identity, and the
    // per-noun slug keeps the password and Steam-ID buttons from colliding on a shared label.
    if ui.small_button(format!("{}###reveal-{noun}", if *revealed { "Hide" } else { "Reveal" })) {
        *revealed = !*revealed;
    }
    ui.same_line();
    if ui.small_button(format!("Copy###copy-{noun}")) {
        // Copy the real value even while masked — sharing without showing it on stream is the whole point.
        // Goes through our own Win32 clipboard, not imgui's: hudhook's imgui-sys disables the Win32
        // clipboard impl, so `ui.set_clipboard_text` would only write an in-process buffer (see
        // `crate::clipboard`).
        crate::clipboard::set_text(value);
        // Toast so the click feels responsive — a silent copy reads as a no-op.
        crate::notify::with_mut(|n| n.info(format!("Copied {noun} to clipboard")));
    }
}

fn draw_banners(ui: &Ui, banners: &[Banner], max_w: f32) {
    for b in banners {
        text_right_aligned(ui, rgba(severity_color(b.severity), 1.0), max_w, &b.message);
    }
}

fn draw_toasts(ui: &Ui, toasts: &[Toast], max_w: f32) {
    for t in toasts {
        // Fade alpha out as the toast expires. The model guarantees `duration > 0.0` and finite, so
        // this can't divide by zero.
        let alpha = (t.remaining / t.duration).clamp(0.0, 1.0);
        text_right_aligned(ui, rgba(severity_color(t.severity), alpha), max_w, &t.message);
    }
}

/// Draw one notification line right-aligned within `max_w` (the widest line in the corner window), by
/// pushing the cursor right by the line's shortfall before drawing. Keeps the top-right notifications
/// flush with the screen edge they're pinned to rather than left-aligned ragged.
fn text_right_aligned(ui: &Ui, color: [f32; 4], max_w: f32, text: &str) {
    let off = (max_w - ui.calc_text_size(text)[0]).max(0.0);
    if off > 0.0 {
        let pos = ui.cursor_pos();
        ui.set_cursor_pos([pos[0] + off, pos[1]]);
    }
    ui.text_colored(color, text);
}

/// Scroll the current window vertically from up/down input — keyboard arrows or the controller d-pad /
/// left stick (both auto-repeat while held, so holding scrolls continuously). Called *inside* a tab's
/// scrollable child so `scroll_y`/`set_scroll_y` act on that child. Used by the read-only Log and
/// Settings panes, which have nothing to select, so up/down scrolls the view — whereas the Actions tab
/// instead spends up/down moving its selection cursor (see [`Overlay::draw_actions_tab`]). Clamped to
/// `[0, scroll_max_y]` so it can't overscroll past either end.
fn scroll_pane(ui: &Ui, pad: crate::input::PadEdges) {
    let down = ui.is_key_pressed(Key::DownArrow) || pad.down;
    let up = ui.is_key_pressed(Key::UpArrow) || pad.up;
    if down == up {
        return; // neither pressed (or both at once) — nothing to do
    }
    // Two text lines per tick: brisk enough to traverse a long log while a direction is held, fine
    // enough to read line-by-line with taps.
    let step = ui.text_line_height_with_spacing() * 2.0;
    let delta = (i32::from(down) - i32::from(up)) as f32 * step;
    let target = (ui.scroll_y() + delta).clamp(0.0, ui.scroll_max_y());
    ui.set_scroll_y(target);
}

/// Our own log tail, coloured by level. Copies the lines out under the non-blocking lock first, then
/// draws — never holding the log lock across the imgui draw loop (game-thread log pushes block on it).
/// Up/down (arrow keys or d-pad / left-stick) scroll the tail, like the Settings tab.
fn draw_log_tab(ui: &Ui, pad: crate::input::PadEdges) {
    scroll_pane(ui, pad);
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

/// Assemble the shareable diagnostics bundle and write it to the install's `unseamless-coop/` folder.
/// Runs on a detached worker thread off the present hook (see [`Overlay::export_diagnostics`]), so the
/// blocking disk write and the [`crate::notify`] toast can't hitch rendering. All inputs are either
/// passed in (`config`, `module`) or read from process-global statics, so this is freely callable from
/// any thread.
fn export_bundle_to_disk(config: &Config, module: usize) {
    use unseamless_core::diagnostics::{RunInfo, export_bundle};

    // Build profile string mirrors logger.rs's PROFILE: keyed on debug-assertions (on for dev/diag,
    // off for release), so it names the symbol status it can actually detect.
    const PROFILE: &str = if cfg!(debug_assertions) {
        "debug-assertions on (symbols)"
    } else {
        "release (stripped)"
    };
    // Self-describing header from the live config — RunInfo::from_config is the *redacting* path (the
    // session password never reaches the bundle); the final scrub in export_bundle then also strips the
    // raw SteamID it carries. `started_at` is wall-clock epoch seconds (core has no clock); run_id
    // "export" marks this as an on-demand capture, not a fresh process launch.
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    let header = RunInfo::from_config(
        config,
        "export".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
        PROFILE.to_string(),
        env!("UNSEAMLESS_BUILD_ID").to_string(),
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        started_at,
    )
    .header_block();

    // Latest live snapshot, if the debug panel was ever shown this session (the publisher only runs
    // while it's visible). `None` is fine — the boot snapshot is always in the log tail below.
    let live = crate::debug_panel::snapshot().map(|r| r.render());

    // Recent log lines, oldest first, read non-blocking (the same source the Log tab draws). On
    // momentary contention we still export — just without the in-memory tail (the boot dump and run
    // header still carry the essentials), rather than failing the whole capture.
    let tail = crate::logbuf::try_read(|lines| {
        lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n")
    })
    .unwrap_or_default();

    let bundle = export_bundle(&header, live.as_deref(), &tail);

    // The install's unseamless-coop/ folder (config + logs live here too), mirroring app.rs's
    // self-dir-or-cwd fallback so the file always lands somewhere findable. Create the folder in case
    // logging never opened it.
    let dir = crate::mods::self_dir(module)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("unseamless-coop");
    let path = dir.join("unseamless-coop-diagnostics.txt");
    match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &bundle)) {
        Ok(()) => {
            log::info!("exported diagnostics bundle to {}", path.display());
            crate::notify::with_mut(|n| n.info(format!("Diagnostics saved to {}", path.display())));
        }
        Err(e) => {
            log::error!("failed to export diagnostics to {}: {e}", path.display());
            crate::notify::with_mut(|n| n.error(format!("Couldn't save diagnostics: {e}")));
        }
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
    // NB: hudhook logs `Render error HRESULT(0xFFFFFFFF)` / `Initialization context incomplete` on the
    // first frame or two before the swapchain is fully wired — a known-harmless hudhook/imgui startup
    // artifact (confirmed on the rig), not a real failure. Don't chase it; the overlay renders fine after.
    match Hudhook::builder().with::<ImguiDx12Hooks>(Overlay::new(module)).with_hmodule(hmodule).build().apply() {
        Ok(()) => log::info!("overlay: DX12 present-hook installed; waiting for the swapchain"),
        Err(e) => log::error!("overlay: hook install failed ({e:?}); no overlay this session"),
    }
}
