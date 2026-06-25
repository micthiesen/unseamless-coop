//! Suppressing the **game's own input** while the overlay is open.
//!
//! Elden Ring reads keyboard/mouse through **DirectInput8** (`IDirectInputDevice8::GetDeviceState`),
//! which is polled straight off the HID stack and never goes through the window message queue — so
//! hudhook's WndProc-level `message_filter` can't stop it, and WASD / clicks reach the game even with
//! the overlay focused. We detour `GetDeviceState` and, while the overlay is open, zero the state it
//! returns, so the game sees no input. imgui still receives input via hudhook's WndProc hook, so the
//! menu stays usable. (`message_filter` still covers the WndProc/raw-input path; the two together cover
//! both ways the game can read input.)
//!
//! Technique mirrors the `fromsoftware-rs` SDK's `debug` crate (public SDK — our reference per
//! CLAUDE.md > "Lean on the SDK"), reduced to a single "overlay open?" flag.
//!
//! Threading: [`set_blocked`] is called from the Present thread (overlay); the detour runs on whatever
//! thread the game polls input on. The shared state is one [`AtomicBool`]. The hooks are installed once
//! and never removed (process-lifetime, like our task handles — unhooking a live input path is a
//! use-after-free risk).

use std::sync::atomic::{AtomicBool, Ordering};

use ilhook::HookError;
use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_retn};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::{GUID, PCSTR, s};

/// True while the overlay wants the game to ignore input. Set each frame by the overlay; read by the
/// `GetDeviceState` detour on the game's input thread.
static BLOCKED: AtomicBool = AtomicBool::new(false);

/// Tell the input hook whether to swallow the game's input (overlay open ⇒ `true`). Cleared whenever
/// the overlay closes or disables itself, so the game can never get stuck unable to read input.
pub fn set_blocked(blocked: bool) {
    BLOCKED.store(blocked, Ordering::Relaxed);
}

fn is_blocked() -> bool {
    BLOCKED.load(Ordering::Relaxed)
}

const DIRECTINPUT_VERSION: u32 = 0x0800;
// IID_IDirectInput8W
const IID_IDIRECTINPUT8W: GUID =
    GUID::from_values(0xbf79_8031, 0x483a, 0x4da2, [0xaa, 0x99, 0x5d, 0x64, 0xed, 0x36, 0x97, 0x00]);
// GUID_SysKeyboard / GUID_SysMouse — the system keyboard/mouse device GUIDs.
const GUID_SYS_KEYBOARD: GUID =
    GUID::from_values(0x6f1d_2b61, 0xd5a0, 0x11cf, [0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00]);
const GUID_SYS_MOUSE: GUID =
    GUID::from_values(0x6f1d_2b60, 0xd5a0, 0x11cf, [0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00]);

// COM vtable slots: IUnknown::Release=2; IDirectInput8::CreateDevice=3; IDirectInputDevice8::GetDeviceState=9.
const VTBL_RELEASE: usize = 2;
const VTBL_CREATE_DEVICE: usize = 3;
const VTBL_GET_DEVICE_STATE: usize = 9;

type RawObj = *mut *const usize;
type DInput8CreateFn = unsafe extern "system" fn(usize, u32, *const GUID, *mut RawObj, usize) -> i32;
type CreateDeviceFn = unsafe extern "system" fn(RawObj, *const GUID, *mut RawObj, usize) -> i32;
type ReleaseFn = unsafe extern "system" fn(RawObj) -> u32;

unsafe fn vtable_fn<F: Copy>(obj: RawObj, slot: usize) -> F {
    unsafe { std::mem::transmute_copy(&*(*obj).add(slot)) }
}

/// The `GetDeviceState` detour: run the real call, then blank the returned state if blocked.
/// `IDirectInputDevice8W::GetDeviceState(this=rcx, cbData=rdx, lpvData=r8) -> HRESULT`.
fn get_state_detour(regs: *mut Registers, original: usize) -> usize {
    let (this, size, data) = unsafe { ((*regs).rcx, (*regs).rdx, (*regs).r8 as *mut u8) };
    let original: unsafe extern "system" fn(u64, u64, u64) -> usize =
        unsafe { std::mem::transmute(original) };
    let hr = unsafe { original(this, size, data as u64) };
    // Only blank a successful read (hr == 0 == DI_OK); leave error states alone.
    if hr == 0 && is_blocked() {
        unsafe { std::ptr::write_bytes(data, 0, size as usize) };
    }
    hr
}

/// Create a throwaway DirectInput device for `guid` and read its `GetDeviceState` vtable address (the
/// real dinput8 implementation, shared by the game's own devices), then release the probe objects.
unsafe fn probe_get_device_state(
    di8_create: DInput8CreateFn,
    hinstance: usize,
    guid: &GUID,
) -> Result<usize, String> {
    let mut di8: RawObj = std::ptr::null_mut();
    let hr = unsafe { di8_create(hinstance, DIRECTINPUT_VERSION, &IID_IDIRECTINPUT8W, &mut di8, 0) };
    if hr != 0 || di8.is_null() {
        return Err(format!("DirectInput8Create failed: {hr:#010x}"));
    }
    let create_device: CreateDeviceFn = unsafe { vtable_fn(di8, VTBL_CREATE_DEVICE) };
    let release_di8: ReleaseFn = unsafe { vtable_fn(di8, VTBL_RELEASE) };
    let mut device: RawObj = std::ptr::null_mut();
    let hr = unsafe { create_device(di8, guid, &mut device, 0) };
    if hr != 0 || device.is_null() {
        unsafe { release_di8(di8) };
        return Err(format!("CreateDevice({guid:?}) failed: {hr:#010x}"));
    }
    let addr = unsafe { *(*device).add(VTBL_GET_DEVICE_STATE) as usize };
    let release_device: ReleaseFn = unsafe { vtable_fn(device, VTBL_RELEASE) };
    unsafe { release_device(device) };
    unsafe { release_di8(di8) };
    Ok(addr)
}

/// Install the DirectInput `GetDeviceState` detours. Call once, on the init thread (single-threaded
/// context). Returns an error string (rather than panicking) so the caller can degrade — without the
/// hook the overlay still draws, it just won't suppress game input while open.
///
/// # Safety
/// Standard hooking caveats: patches executable memory in the real `dinput8.dll`. Must run once, off
/// the main thread, before input is being polled concurrently.
pub unsafe fn install() -> Result<(), String> {
    // We are `dinput8.dll`; our proxy forwards `DirectInput8Create` to the real one (`proxy.rs`), so
    // the device we probe — and hook — is the real implementation the game also uses.
    let dinput8 =
        unsafe { GetModuleHandleA(s!("dinput8.dll")) }.map_err(|e| format!("dinput8.dll not loaded: {e}"))?;
    let proc = unsafe { GetProcAddress(dinput8, s!("DirectInput8Create")) }
        .ok_or_else(|| "DirectInput8Create export not found".to_string())?;
    let di8_create: DInput8CreateFn = unsafe { std::mem::transmute(proc) };
    let hinstance =
        unsafe { GetModuleHandleA(PCSTR::null()) }.map_err(|e| format!("GetModuleHandle failed: {e}"))?.0
            as usize;

    let kb = unsafe { probe_get_device_state(di8_create, hinstance, &GUID_SYS_KEYBOARD)? };
    let ms = unsafe { probe_get_device_state(di8_create, hinstance, &GUID_SYS_MOUSE)? };

    let hook = |addr: usize, what: &str| -> Result<_, String> {
        unsafe { hook_closure_retn(addr, get_state_detour, CallbackOption::None, HookFlags::empty()) }
            .map_err(|e: HookError| format!("hooking {what} GetDeviceState: {e:?}"))
    };

    // Keyboard and mouse devices often share one GetDeviceState implementation; hook each distinct
    // address once.
    let mut hooks = vec![hook(kb, "keyboard")?];
    if ms != kb {
        hooks.push(hook(ms, "mouse")?);
    }
    // Never unhooked — resident for the process lifetime, like our task handles.
    std::mem::forget(hooks);
    Ok(())
}
