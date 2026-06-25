//! In-game **overlay** — hudhook's DX12 present-hook driving imgui.
//!
//! This is the renderer that [`menu.rs`](unseamless_core::menu) and
//! [`notifications.rs`](unseamless_core::notifications) have always assumed: pure models whose docs
//! say "a renderer draws these each frame". Milestone 1 (this module) draws a static box, to prove a
//! DX12 present-hook renders under Proton/vkd3d on the rig — the make-or-break UI test. It's gated
//! behind the `overlay` cargo feature (and a `[debug] overlay` runtime kill-switch) until confirmed;
//! then it becomes a shipping feature and we wire the real models.
//!
//! Threading: hudhook draws on the game's **Present** thread, a different thread than our frame
//! tasks. The rule (per OVERLAY-RENDERING.md): the overlay only **reads** shared app state and draws;
//! it never mutates game state (game writes stay in tasks). Installed once at `app::install`; like
//! the task handles it stays resident for the process lifetime (hudhook owns the global — no unhook
//! on detach, which would be a use-after-free on a live present path).

use std::ffi::c_void;

use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::imgui::{Condition, Ui};
use hudhook::{Hudhook, ImguiRenderLoop};
use windows::Win32::Foundation::HINSTANCE;

/// The render loop. Milestone 1: one static window. Must be `Send + Sync + 'static` (hudhook calls
/// it from the Present thread).
struct Overlay;

impl ImguiRenderLoop for Overlay {
    fn render(&mut self, ui: &mut Ui) {
        ui.window("unseamless-coop")
            .size([320.0, 90.0], Condition::FirstUseEver)
            .build(|| {
                ui.text("unseamless-coop overlay alive");
                ui.separator();
                ui.text(format!("v{} — DX12 present-hook OK", env!("CARGO_PKG_VERSION")));
            });
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
    match Hudhook::builder().with::<ImguiDx12Hooks>(Overlay).with_hmodule(hmodule).build().apply() {
        Ok(()) => log::info!("overlay: DX12 present-hook installed; waiting for the swapchain"),
        Err(e) => log::error!("overlay: hook install failed ({e:?}); no overlay this session"),
    }
}
