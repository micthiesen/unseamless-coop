//! unseamless-coop — a Rust rewrite of the Elden Ring Seamless Co-op (ERSC) mod, built on the
//! [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.
//!
//! `DllMain` inits logging and spawns an init thread that hands off to [`app::install`], which
//! waits for the game's task system, loads config, and registers each [`feature::Feature`] as a
//! recurring frame task. The platform-independent decision logic lives in the `unseamless-core`
//! crate (host-tested); this crate binds it to the live game. The upstream `ersc.dll` is kept
//! locally under `reference/` (gitignored) for behavioral study — never copied (see CLAUDE.md).

use std::ffi::c_void;

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::BOOL;

mod app;
mod config;
mod feature;
mod features;
mod logger;

// Only DLL_PROCESS_ATTACH is handled, deliberately. We register tasks into the game's task pool
// that hold pointers and vtables into this DLL's image; the SDK has no way to unregister them,
// so the DLL must stay resident for the process lifetime. Do NOT add a DLL_PROCESS_DETACH
// cleanup path: unloading while a task is registered is a use-after-free.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(_: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        logger::init();
        // Off the loader lock and off the main thread: the init thread waits for the task
        // system, then registers per-frame tasks and returns. `wait_for_instance` must not run
        // on the main thread (it blocks on main-thread initialization).
        std::thread::spawn(app::install);
    }
    true.into()
}
