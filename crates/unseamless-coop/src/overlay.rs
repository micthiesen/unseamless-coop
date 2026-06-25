//! In-game **overlay** — hudhook's DX12 present-hook driving imgui.
//!
//! This is the renderer that [`menu.rs`](unseamless_core::menu) and
//! [`notifications.rs`](unseamless_core::notifications) have always assumed: pure models whose docs
//! say "a renderer draws these each frame". It draws the live notifications ([`crate::notify`]) —
//! banners + toasts — and the menu later. The DX12 present-hook is rig-confirmed to render under
//! Proton/vkd3d, and ships (always compiled; the DLL statically links the C++ runtime so it's
//! self-contained). `[debug] overlay` (default on) is a recovery kill-switch if vkd3d ever breaks it.
//!
//! Threading: hudhook draws on the game's **Present** thread, a different thread than our frame
//! tasks. The rule (per OVERLAY-RENDERING.md): the overlay only **reads** shared app state and draws;
//! it never mutates game state (game writes stay in tasks). Installed once at `app::install`; like
//! the task handles it stays resident for the process lifetime (hudhook owns the global — no unhook
//! on detach, which would be a use-after-free on a live present path).

use std::ffi::c_void;

use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::imgui::{Condition, Ui, WindowFlags};
use hudhook::{Hudhook, ImguiRenderLoop};
use unseamless_core::notifications::{Banner, Severity, Toast};
use windows::Win32::Foundation::HINSTANCE;

/// The render loop. Must be `Send + Sync + 'static` (hudhook calls it from the Present thread). It
/// holds no state — it reads the shared [`crate::notify`] each frame and draws it.
struct Overlay;

impl ImguiRenderLoop for Overlay {
    fn render(&mut self, ui: &mut Ui) {
        // Copy the active set out under the (non-blocking) lock, then draw from the owned copy — so
        // we never hold the notifications lock across the imgui draw (the game thread's pushes/tick
        // block on that lock; holding it across a foreign UI library is the wrong boundary). The set
        // is tiny and bounded (a few banners + ≤8 toasts), so the clone is cheap. `None` = the lock
        // was momentarily held by the game thread; skip drawing this frame.
        let Some((banners, toasts)) = crate::notify::try_read(|n| (n.banners().to_vec(), n.toasts().to_vec()))
        else {
            return;
        };
        if banners.is_empty() && toasts.is_empty() {
            return; // nothing to show — overlay invisible when idle
        }
        // Shared reborrow so the `window().build(|| ui.text(..))` borrow pattern type-checks.
        let ui: &Ui = ui;
        draw(ui, &banners, &toasts);
    }
}

/// Draw persistent banners then transient toasts (fading), colored by severity, in a borderless,
/// input-transparent, auto-sized corner window.
fn draw(ui: &Ui, banners: &[Banner], toasts: &[Toast]) {
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
            for b in banners {
                ui.text_colored(severity_color(b.severity, 1.0), &b.message);
            }
            for t in toasts {
                // Fade alpha out as the toast expires. The model guarantees `duration > 0.0` and
                // finite (`Notifications::toast` rejects otherwise), so this can't divide by zero.
                let alpha = (t.remaining / t.duration).clamp(0.0, 1.0);
                ui.text_colored(severity_color(t.severity, alpha), &t.message);
            }
        });
}

/// imgui RGBA color for a severity, at the given alpha.
fn severity_color(severity: Severity, alpha: f32) -> [f32; 4] {
    let [r, g, b] = match severity {
        Severity::Info => [0.62, 0.80, 1.0],
        Severity::Warning => [1.0, 0.82, 0.30],
        Severity::Error => [1.0, 0.45, 0.45],
    };
    [r, g, b, alpha]
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
    match Hudhook::builder().with::<ImguiDx12Hooks>(Overlay).with_hmodule(hmodule).build().apply() {
        Ok(()) => log::info!("overlay: DX12 present-hook installed; waiting for the swapchain"),
        Err(e) => log::error!("overlay: hook install failed ({e:?}); no overlay this session"),
    }
}
