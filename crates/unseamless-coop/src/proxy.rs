//! `dinput8.dll` proxy exports.
//!
//! Our shipped artifact is named `dinput8.dll` and dropped next to `eldenring.exe`. The game
//! imports `dinput8`, so Windows' DLL search order loads *ours* first — that's how we get our
//! `DllMain` to run with no separate mod loader (we become our own loader; see `mods.rs`). To not
//! break the game's actual DirectInput use, we re-export the same symbols and **forward** each to
//! the genuine `dinput8.dll` in the system directory, loaded lazily by name (never our own copy).
//!
//! In practice a game only calls `DirectInput8Create`; the rest exist so the import table resolves
//! and any COM use still works. None of this is reachable from the host, so it's validated by
//! cross-compiling + checking the export table (objdump), with the live forward confirmed on the rig.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::core::{GUID, HRESULT, PCSTR, PCWSTR, s};

/// `ERROR_MOD_NOT_FOUND` as an HRESULT — returned if the real `dinput8.dll` can't be loaded.
const E_MOD_NOT_FOUND: HRESULT = HRESULT(0x8007_007E_u32 as i32);
const S_FALSE: HRESULT = HRESULT(1);
const CLASS_E_CLASSNOTAVAILABLE: HRESULT = HRESULT(0x8004_0111_u32 as i32);

/// Cached handle to the genuine system `dinput8.dll` (as `usize` so it's `Send`/`Sync`).
static REAL: OnceLock<usize> = OnceLock::new();

/// Load the real `dinput8.dll` from the Windows system directory (explicitly, so we never recurse
/// into our own copy in the game folder). Cached for the process; null handle if unavailable.
fn real_dinput8() -> HMODULE {
    let h = *REAL.get_or_init(|| unsafe {
        let mut buf = [0u16; 320];
        let n = GetSystemDirectoryW(Some(&mut buf)) as usize;
        if n == 0 || n >= buf.len() {
            return 0;
        }
        let mut path: Vec<u16> = buf[..n].to_vec();
        path.extend("\\dinput8.dll".encode_utf16());
        path.push(0);
        match LoadLibraryW(PCWSTR(path.as_ptr())) {
            Ok(m) => m.0 as usize,
            Err(_) => 0,
        }
    });
    HMODULE(h as *mut c_void)
}

/// The untyped function pointer `GetProcAddress` hands back (the inner type of `FARPROC`).
type Proc = unsafe extern "system" fn() -> isize;

/// Resolve an export of the real `dinput8.dll` by name.
unsafe fn real_proc(name: PCSTR) -> Option<Proc> {
    let h = real_dinput8();
    if h.0.is_null() {
        return None;
    }
    unsafe { GetProcAddress(h, name) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DirectInput8Create(
    hinst: *mut c_void,
    dwversion: u32,
    riidltf: *const GUID,
    ppvout: *mut *mut c_void,
    punkouter: *mut c_void,
) -> HRESULT {
    type F = unsafe extern "system" fn(
        *mut c_void,
        u32,
        *const GUID,
        *mut *mut c_void,
        *mut c_void,
    ) -> HRESULT;
    unsafe {
        match real_proc(s!("DirectInput8Create")) {
            Some(p) => (std::mem::transmute::<Proc, F>(p))(hinst, dwversion, riidltf, ppvout, punkouter),
            None => E_MOD_NOT_FOUND,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    type F = unsafe extern "system" fn() -> HRESULT;
    unsafe {
        match real_proc(s!("DllCanUnloadNow")) {
            Some(p) => (std::mem::transmute::<Proc, F>(p))(),
            None => S_FALSE, // "do not unload" — the safe default for a resident proxy
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    type F = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
    unsafe {
        match real_proc(s!("DllGetClassObject")) {
            Some(p) => (std::mem::transmute::<Proc, F>(p))(rclsid, riid, ppv),
            None => CLASS_E_CLASSNOTAVAILABLE,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    type F = unsafe extern "system" fn() -> HRESULT;
    unsafe {
        match real_proc(s!("DllRegisterServer")) {
            Some(p) => (std::mem::transmute::<Proc, F>(p))(),
            None => E_MOD_NOT_FOUND,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    type F = unsafe extern "system" fn() -> HRESULT;
    unsafe {
        match real_proc(s!("DllUnregisterServer")) {
            Some(p) => (std::mem::transmute::<Proc, F>(p))(),
            None => E_MOD_NOT_FOUND,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn GetdfDIJoystick() -> *mut c_void {
    type F = unsafe extern "system" fn() -> *mut c_void;
    unsafe {
        match real_proc(s!("GetdfDIJoystick")) {
            Some(p) => (std::mem::transmute::<Proc, F>(p))(),
            None => std::ptr::null_mut(),
        }
    }
}
