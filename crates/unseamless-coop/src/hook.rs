//! Typed errors for the low-level OS-hook / code-patch binding layer (`input`, `saves`, `patch`).
//!
//! Every site here shares one caller contract: **log and degrade** (or, for the save-isolation
//! guard, fatal) — never `unwrap`. This enum just replaces the old stringly-typed
//! `Result<(), String>` with named variants so a failure mode is legible at the type level. The
//! `Display` text is what lands in the shared log, so it stays as informative as the old format
//! strings the callers used to build.

use thiserror::Error;

/// A failure while resolving, placing, or probing an OS hook or in-place code patch.
#[derive(Debug, Error)]
pub enum HookError {
    /// A module we need to hook into wasn't loaded (e.g. `dinput8.dll`, `xinput1_4.dll`,
    /// `kernel32.dll`, `user32.dll`).
    #[error("{module} not loaded: {err}")]
    ModuleNotLoaded { module: &'static str, err: windows::core::Error },

    /// `GetModuleHandle(NULL)` for the process base image failed.
    #[error("GetModuleHandle failed: {0}")]
    ModuleHandle(windows::core::Error),

    /// A required export wasn't present in an otherwise-loaded module.
    #[error("{0} not found")]
    ExportNotFound(&'static str),

    /// ilhook couldn't place the detour. `detail` is ilhook's `Debug` form (its error type isn't part
    /// of our public surface, so we capture it as text rather than nesting it).
    #[error("hooking {what}: {detail}")]
    Install { what: String, detail: String },

    /// A DirectInput probe call (`DirectInput8Create` / `CreateDevice`) returned a failing `HRESULT`.
    #[error("{what} failed: {hr:#010x}")]
    DInputProbe { what: String, hr: i32 },

    /// A Win32 memory operation (`VirtualProtect`, …) for an in-place code patch failed.
    #[error(transparent)]
    Win32(#[from] windows::core::Error),
}
