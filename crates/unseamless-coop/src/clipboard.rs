//! Writing text to the **Windows clipboard** from the overlay's Copy buttons.
//!
//! Why not just `ui.set_clipboard_text`: hudhook's `imgui-sys` is built with
//! `IMGUI_DISABLE_WIN32_FUNCTIONS` (its `build.rs` — "disabled due to linking issues"), so Dear ImGui's
//! default `SetClipboardTextFn` falls back to an *in-process* buffer that never touches the OS
//! clipboard. `ui.set_clipboard_text` then looks like it works but nothing is pasteable outside the game
//! — exactly what we hit on the rig. So we set the clipboard ourselves via the Win32 API (the path imgui
//! would have used if it weren't compiled out).
//!
//! Under Proton this writes Wine's clipboard, which Wine's clipboard driver mirrors to the host session
//! (so a friend can paste their SteamID into Discord). One environment caveat to confirm: a *nested*
//! gamescope session can fail to propagate Wine's clipboard to the outer compositor, so the rig's
//! gamescope wrapper may still show empty pastes even though a plain Proton launch works.
//!
//! Threading: called on the overlay's Present thread from a Copy button (not a hot path). Best-effort —
//! any failure logs and returns, since a copy that can't reach the clipboard shouldn't do anything
//! dramatic.

use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

/// `CF_UNICODETEXT` (the UTF-16 clipboard format). Hardcoded as the ancient, stable id `13` so we don't
/// pull in the large `Win32_System_Ole` feature just for the constant (it's `Ole::CF_UNICODETEXT`).
const CF_UNICODETEXT: u32 = 13;

/// Copy `text` to the Windows clipboard as `CF_UNICODETEXT`. Best-effort; logs on any failure.
pub fn set_text(text: &str) {
    // CF_UNICODETEXT is a NUL-terminated UTF-16 string.
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);

    // SAFETY: the standard Win32 clipboard hand-off. `OpenClipboard(None)` associates with the calling
    // thread's active window; once it succeeds we always `CloseClipboard`. The owned-handle dance (the
    // system takes ownership on a successful `SetClipboardData`, we free only on failure) lives in
    // `fill_open_clipboard`.
    unsafe {
        if OpenClipboard(None).is_err() {
            log::warn!("clipboard: OpenClipboard failed; nothing copied");
            return;
        }
        let ok = fill_open_clipboard(&wide);
        let _ = CloseClipboard();
        if !ok {
            log::warn!("clipboard: failed to set text; nothing copied");
        }
    }
}

/// Empty the (already-open) clipboard and install `wide` as `CF_UNICODETEXT`. Returns whether the data
/// was handed off. On any early return the allocated global is freed; on a successful `SetClipboardData`
/// the **system** owns the handle, so it must NOT be freed here (that would be a double-free).
///
/// # Safety
/// Call only with the clipboard already open (via [`set_text`]).
unsafe fn fill_open_clipboard(wide: &[u16]) -> bool {
    if unsafe { EmptyClipboard() }.is_err() {
        return false;
    }
    // A GMEM_MOVEABLE global holding the wide string + its NUL, the form SetClipboardData expects.
    let Ok(hmem) = (unsafe { GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(wide)) }) else {
        return false;
    };
    let dst = unsafe { GlobalLock(hmem) } as *mut u16;
    if dst.is_null() {
        let _ = unsafe { GlobalFree(Some(hmem)) };
        return false;
    }
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len()) };
    // GlobalUnlock returns Err on the normal "lock count reached 0" case (last-error is NO_ERROR), so its
    // result is deliberately ignored — that isn't a failure.
    let _ = unsafe { GlobalUnlock(hmem) };
    if unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hmem.0))) }.is_err() {
        // Ownership stayed with us — free it.
        let _ = unsafe { GlobalFree(Some(hmem)) };
        return false;
    }
    true
}
