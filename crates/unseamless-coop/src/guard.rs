//! EAC safety guard.
//!
//! This DLL must only run when the game was launched by **our** launcher, which starts
//! `eldenring.exe` directly and so bypasses EasyAntiCheat. The dangerous case: an ELDEN RING
//! update restores the original `start_protected_game.exe` while our `dinput8.dll` is still in the
//! folder — pressing Play then boots EAC *with a mod present*, the exact state that gets accounts
//! banned. We can't fix the launch from inside the DLL, but we can refuse to run.
//!
//! The signal is a positive launch marker: our launcher sets an environment variable before
//! starting the game; its **absence** means we weren't started by the trusted path, so we abort.
//! That's version-proof (no dependency on EAC's module names) and fail-safe (anything but our
//! launch path dies). See README "After an ELDEN RING update".

use unseamless_core::LAUNCH_MARKER;
use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SYSTEMMODAL, MessageBoxW};
use windows::core::{HSTRING, w};

/// Show a fatal startup error to the user and kill the process — for an unrecoverable condition
/// (wrong launch path, invalid config). **Never returns.** Safe from `DllMain` or the init thread:
/// the `MessageBox` is courtesy (if it can't display this early, we still die), and after
/// `TerminateProcess` we `abort()` as a guaranteed stop so this can never fall through to running
/// the game.
pub fn fatal(message: &str) -> ! {
    let text = HSTRING::from(message);
    unsafe {
        let _ = MessageBoxW(None, &text, w!("unseamless-coop"), MB_OK | MB_ICONERROR | MB_SYSTEMMODAL);
        let _ = TerminateProcess(GetCurrentProcess(), 1);
    }
    std::process::abort();
}

/// Proceed only if our launcher started us. Otherwise show a message and **terminate the process**
/// (this does not return). Called first thing in `DllMain`, synchronously, so the game is frozen at
/// our DLL's load — before it initializes anti-cheat or networking — until we kill it.
///
/// Note: the `TerminateProcess` is the actual safety; the `MessageBox` is only courtesy, so even if
/// it fails to display this early in process init, we still abort.
pub fn ensure_launched_by_us_or_abort() {
    if std::env::var_os(LAUNCH_MARKER).is_some() {
        return;
    }
    fatal(
        "unseamless-coop was not started by its launcher.\n\n\
         An ELDEN RING update may have reverted the mod launcher. Re-copy the mod files \
         (see the README) before playing.\n\n\
         Closing the game now to protect your account from anti-cheat.",
    );
}
