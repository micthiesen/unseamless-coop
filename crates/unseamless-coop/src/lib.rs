//! unseamless-coop — a Rust rewrite of the Elden Ring Seamless Co-op (ERSC) mod, built on the
//! [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.
//!
//! This is the skeleton: `DllMain` inits logging and installs a recurring frame task (see
//! [`hook`]) that currently just heartbeats, proving the harness works. Reverse-engineered
//! Seamless Co-op behavior is built out from `hook::on_frame`. The upstream `ersc.dll` is kept
//! locally under `reference/` (gitignored) for inspection.

use std::ffi::c_void;

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::BOOL;

mod hook;
mod logger;

// Only DLL_PROCESS_ATTACH is handled, deliberately. We register a task into the game's task
// pool that holds a pointer and vtable into this DLL's image; the SDK has no way to unregister
// it, so the DLL must stay resident for the process lifetime. Do NOT add a DLL_PROCESS_DETACH
// cleanup path: unloading while the task is registered is a use-after-free.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(_: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        logger::init();
        // Off the loader lock and off the main thread: the init thread waits for the task
        // system, registers the per-frame task, and returns. `wait_for_instance` must not run
        // on the main thread (it blocks on main-thread initialization).
        std::thread::spawn(hook::install);
    }
    true.into()
}
