//! unseamless-coop — a Rust rewrite of the Elden Ring Seamless Co-op (ERSC) mod, built on the
//! [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.
//!
//! `DllMain` inits logging and spawns an init thread that hands off to [`app::install`], which
//! waits for the game's task system, loads config, and registers each [`feature::Feature`] as a
//! recurring frame task. The platform-independent decision logic lives in the `unseamless-core`
//! crate (host-tested); this crate binds it to the live game. The upstream `ersc.dll` is kept
//! locally under `reference/` (gitignored) for behavioral study — never copied (see CLAUDE.md).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::BOOL;

mod actionq;
mod app;
#[cfg(feature = "bridge")]
mod bridge;
// The bridge is a loopback TCP listener; it must never ship. The rig builds it under the `diag`
// profile (debug-assertions ON); a release build has debug-assertions OFF, so enabling the feature
// there fails the build loudly instead of silently embedding a listener in the shipped DLL.
#[cfg(all(feature = "bridge", not(debug_assertions)))]
compile_error!("the `bridge` feature must not be enabled in a release build (use the `diag` profile)");
mod config;
mod feature;
mod features;
mod guard;
mod input;
mod logbuf;
mod logger;
mod mods;
mod notify;
mod overlay;
mod proxy;
mod sdk;
mod state;

/// Our own module handle, captured in `DllMain`, so the init thread can find the game folder (and
/// the `mods/` dir next to it) regardless of the process working directory.
pub(crate) static SELF_MODULE: AtomicUsize = AtomicUsize::new(0);

// Only DLL_PROCESS_ATTACH is handled, deliberately. We register tasks into the game's task pool
// that hold pointers and vtables into this DLL's image; the SDK has no way to unregister them,
// so the DLL must stay resident for the process lifetime. Do NOT add a DLL_PROCESS_DETACH
// cleanup path: unloading while a task is registered is a use-after-free.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(module: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        SELF_MODULE.store(module.0 as usize, Ordering::Relaxed);
        // EAC safety FIRST, synchronously: if our launcher didn't start the game, abort before
        // anything else runs (this does not return in that case). See `guard`.
        guard::ensure_launched_by_us_or_abort();
        // Off the loader lock and off the main thread: the init thread loads config, brings up
        // logging, loads other mods, waits for the task system, then registers per-frame tasks.
        // `wait_for_instance` must not run on the main thread (it blocks on main-thread init).
        std::thread::spawn(app::install);
    }
    true.into()
}
