//! Suppressing the **game's own input** while the overlay is open.
//!
//! Elden Ring reads keyboard/mouse through **DirectInput8** (`IDirectInputDevice8::GetDeviceState`,
//! *immediate* mode — confirmed on the rig), which is polled straight off the HID stack and never goes
//! through the window message queue, so hudhook's WndProc-level `message_filter` can't stop it and
//! WASD / clicks reach the game even with the overlay focused. We detour `GetDeviceState` and, while
//! the overlay is open, zero the state it returns, so the game sees no input. imgui still receives
//! input via hudhook's WndProc hook, so the menu stays usable. (`message_filter` covers the
//! WndProc/raw-input path; if ER ever read *buffered* DI via `GetDeviceData`, that path would need its
//! own hook.)
//!
//! It also detours user32 `SetCursorPos`/`ClipCursor` to release the game's cursor center-pin while
//! the overlay is open, so the mouse can move over the menu.
//!
//! **Controller** rides the same idea on a different API: ER polls the pad through
//! `xinput1_4!XInputGetState` (also immediate, also off the WndProc path, so `message_filter` can't
//! stop it either). We detour it to do two things at once: (1) **read** the live pad from the game's
//! own buffer into [`PAD_SNAPSHOT`] so the overlay can drive its menu (d-pad / left-stick to navigate,
//! A to confirm, B to close, and the **RB+L3+R3 chord** to toggle the overlay); and (2) **blank**
//! the pad while the overlay is open (zero the gamepad struct *and* bump `dwPacketNumber` — a game
//! that skips re-reading on an unchanged packet would otherwise reuse the pre-open input rather than
//! see the neutral one). The toggle is a chord of standard bits rather than the Guide/Home button on
//! purpose: Steam Input intercepts Guide for most players, but the plain `XInputGetState` reports the
//! chord, so there's nothing for it to eat and no need for the Guide-only `XInputGetStateEx`. The pure
//! menu-translation ([`PadNav`] → per-frame edges) lives in [`unseamless_core::pad`] (host-tested);
//! this module is just the OS binding that feeds it. (XInput is a flat C export, so unlike DirectInput
//! there's no COM-vtable probe.)
//!
//! Technique mirrors the `fromsoftware-rs` SDK's `debug` crate (public SDK — our reference per
//! CLAUDE.md > "Lean on the SDK"), reduced to a single "overlay open?" flag.
//!
//! Threading: [`set_blocked`] is called from the Present thread (overlay); the detours run on whatever
//! thread the game polls input on. Shared state is one [`AtomicBool`] (blocked), one
//! [`AtomicU64`](std::sync::atomic::AtomicU64) pad snapshot, and one
//! [`AtomicI32`](std::sync::atomic::AtomicI32) owner index; [`PadNav`] is Present-thread-only. The
//! hooks are installed once and never removed (process-lifetime, like our task handles — unhooking a
//! live input path is a use-after-free risk).

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_retn};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::{GUID, PCSTR, s};

use crate::hook::HookError;

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

// ---- Controller (XInput) --------------------------------------------------------------------------
//
// The pure menu-translation logic (edge/repeat state machine, button bits, thresholds) lives in
// `unseamless_core::pad`; this half is the OS binding — the `XInputGetState` hook and the atomic
// snapshot. Re-exported so the overlay keeps a single `crate::input::Pad*` import path.
pub use unseamless_core::pad::{PadEdges, PadNav};

/// Latest pad sample, packed `buttons | (lx << 16) | (ly << 32)` so a frame reads consistently with one
/// atomic load (no tearing between buttons and stick). Written by the `XInputGetState` detour on the
/// game's input thread; read by [`pad_snapshot`] on the Present thread.
static PAD_SNAPSHOT: AtomicU64 = AtomicU64::new(0);
/// The controller index currently owning the snapshot (the pad driving menu nav), or `-1` for none.
/// Stops a second pad from stomping the first, and lets a disconnect of the owning pad reset the
/// snapshot to neutral so a direction held at disconnect can't auto-repeat in the menu forever.
static PAD_OWNER: AtomicI32 = AtomicI32::new(-1);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XInputGamepad {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    thumb_lx: i16,
    thumb_ly: i16,
    thumb_rx: i16,
    thumb_ry: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XInputState {
    packet_number: u32,
    gamepad: XInputGamepad,
}

// A wrong field size/offset here is silent UB at runtime (the detour reads — and blanks — XInput-owned
// memory through these structs), so pin the layout against the documented `xinput.h` ABI
// (`XINPUT_GAMEPAD` = 12 bytes, `XINPUT_STATE` = 16) at compile time — `size_of`/`offset_of!` are
// const, so this fails the build (even cross-compiling) if the structs ever skew from the ABI.
const _: () = {
    assert!(size_of::<XInputGamepad>() == 12);
    assert!(size_of::<XInputState>() == 16);
    assert!(std::mem::offset_of!(XInputState, packet_number) == 0);
    assert!(std::mem::offset_of!(XInputState, gamepad) == 4);
};

/// Pack a pad sample into the snapshot word. The intermediate `as u16` casts are load-bearing: they
/// truncate to 16 bits *before* zero-extending to `u64`, so a negative stick value can't sign-extend
/// and clobber the neighbouring field. The top 16 bits (right stick) are intentionally unused.
fn pack_pad(buttons: u16, lx: i16, ly: i16) -> u64 {
    (buttons as u64) | ((lx as u16 as u64) << 16) | ((ly as u16 as u64) << 32)
}

/// Inverse of [`pack_pad`]; the `as u16 as i16` round-trip restores each stick value's sign.
fn unpack_pad(v: u64) -> (u16, i16, i16) {
    (v as u16, (v >> 16) as u16 as i16, (v >> 32) as u16 as i16)
}

/// This frame's pad sample `(buttons, left_x, left_y)` for the overlay's [`PadNav`]. Reads the snapshot
/// non-blocking; returns neutral `(0, 0, 0)` until the XInput hook is installed (or after a disconnect).
pub fn pad_snapshot() -> (u16, i16, i16) {
    unpack_pad(PAD_SNAPSHOT.load(Ordering::Relaxed))
}

/// Publish a connected pad's sample to [`PAD_SNAPSHOT`], claiming ownership for `user_index` unless
/// another connected pad already holds it (so a second controller never stomps the one driving the
/// menu; `release_pad` frees it on disconnect). The sample is read straight from the game's own
/// `XInputGetState` buffer in the detour — every bit we use (the RB+L3+R3 toggle chord, A, B,
/// d-pad, sticks) is standard, so there's no separate read and no need for the Guide-only
/// `XInputGetStateEx`.
fn capture_pad(user_index: u32, buttons: u16, lx: i16, ly: i16) {
    let idx = user_index as i32;
    let owner = PAD_OWNER.load(Ordering::Relaxed);
    if owner < 0 || owner == idx {
        PAD_OWNER.store(idx, Ordering::Relaxed);
        PAD_SNAPSHOT.store(pack_pad(buttons, lx, ly), Ordering::Relaxed);
    }
}

/// Reset the snapshot to neutral if the pad that just reported not-connected is the one driving the
/// menu, so a direction held at the moment of disconnect doesn't auto-repeat forever. No-op otherwise.
fn release_pad(user_index: u32) {
    if PAD_OWNER.load(Ordering::Relaxed) == user_index as i32 {
        PAD_OWNER.store(-1, Ordering::Relaxed);
        PAD_SNAPSHOT.store(0, Ordering::Relaxed);
    }
}

/// `DWORD XInputGetState(DWORD dwUserIndex = rcx, XINPUT_STATE* pState = rdx)`. Capture the pad for our
/// menu, then (while the overlay is open) blank what the game sees.
///
/// # Safety
/// `regs` must point at the saved registers for an `XInputGetState` call (the contract ilhook upholds
/// at the hook site): `rdx` is null or a writable `XInputState`. `original` must be the real
/// `XInputGetState` entry. Invoked only from the installed detour.
unsafe fn xinput_get_state_detour(regs: *mut Registers, original: usize) -> usize {
    let (user_index, state) = unsafe { ((*regs).rcx as u32, (*regs).rdx as *mut XInputState) };
    let original: unsafe extern "system" fn(u32, *mut XInputState) -> u32 =
        unsafe { std::mem::transmute(original) };
    let ret = unsafe { original(user_index, state) };
    if ret == 0 && !state.is_null() {
        // Connected (ERROR_SUCCESS): `state` holds this poll's real pad. Capture it for our menu (read
        // before any blanking), then blank what the game sees if the overlay is open.
        let g = unsafe { (*state).gamepad };
        capture_pad(user_index, g.buttons, g.thumb_lx, g.thumb_ly);
        if is_blocked() {
            unsafe {
                (*state).gamepad = XInputGamepad::default();
                // Also bump packet_number. A game that skips re-reading on an unchanged packet would
                // otherwise keep reusing the last *real* input — a steadily-held stick leaves the
                // device's packet constant, so zeroing the gamepad alone wouldn't be seen. Bumping
                // guarantees the game re-reads the now-neutral gamepad every blanked poll.
                (*state).packet_number = (*state).packet_number.wrapping_add(1);
            }
        }
    } else if ret != 0 {
        // Not connected (or error): if our nav pad vanished, drop to neutral so held dirs don't stick.
        release_pad(user_index);
    }
    ret as usize
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
///
/// # Safety
/// `regs` must point at the saved registers for a `GetDeviceState` call (ilhook's hook-site contract):
/// `r8`/`rdx` describe the device's output buffer and its size. `original` must be the real
/// `GetDeviceState` entry. Invoked only from the installed detour.
unsafe fn get_state_detour(regs: *mut Registers, original: usize) -> usize {
    let (this, size, data) = unsafe { ((*regs).rcx, (*regs).rdx, (*regs).r8 as *mut u8) };
    let original: unsafe extern "system" fn(u64, u64, u64) -> usize =
        unsafe { std::mem::transmute(original) };
    let hr = unsafe { original(this, size, data as u64) };
    // Only blank a successful read (hr == 0 == DI_OK), which guarantees the device wrote `cbData` bytes
    // at `lpvData` — so zeroing `size` bytes targets exactly that buffer. Guard the null edge anyway
    // (a misbehaving in-process DI consumer could return DI_OK with a null buffer) so the unsafe write
    // can never hit a null page.
    if hr == 0 && is_blocked() && !data.is_null() {
        unsafe { std::ptr::write_bytes(data, 0, size as usize) };
    }
    hr
}

// The game pins the OS cursor to the screen centre during gameplay (so the hidden cursor can't drift
// off-window while it reads relative mouse-look). The next two detours release that pin while the
// overlay is open, so the mouse can move over the menu. Both no-op when the overlay is closed.

/// `SetCursorPos(X = rcx, Y = rdx) -> BOOL`. While blocked, skip the recenter (return TRUE) so the
/// cursor stays where the user moved it.
///
/// # Safety
/// `regs` must point at the saved registers for a `SetCursorPos` call (ilhook's hook-site contract).
/// `original` must be the real `SetCursorPos` entry. Invoked only from the installed detour.
unsafe fn set_cursor_pos_detour(regs: *mut Registers, original: usize) -> usize {
    if is_blocked() {
        return 1; // TRUE — claim success without moving the cursor
    }
    let original: unsafe extern "system" fn(u64, u64) -> usize = unsafe { std::mem::transmute(original) };
    unsafe { original((*regs).rcx, (*regs).rdx) }
}

/// `ClipCursor(lpRect = rcx) -> BOOL`. While blocked, release the cursor (call with NULL) instead of
/// confining it to the game's rect.
///
/// # Safety
/// `regs` must point at the saved registers for a `ClipCursor` call (ilhook's hook-site contract).
/// `original` must be the real `ClipCursor` entry. Invoked only from the installed detour.
unsafe fn clip_cursor_detour(regs: *mut Registers, original: usize) -> usize {
    let lp_rect = if is_blocked() { 0 } else { unsafe { (*regs).rcx } };
    let original: unsafe extern "system" fn(u64) -> usize = unsafe { std::mem::transmute(original) };
    unsafe { original(lp_rect) }
}

/// Install one function-replacement detour and **forget it immediately**, so it's resident for the
/// process lifetime — never unhooked. Forgetting per-hook (rather than collecting handles and forgetting
/// at the end) means a *later* hook failing can't drop — and thus unpatch live code / free a closure a
/// concurrent call points into (UAF) — an already-installed detour. `detour` runs the original (via the
/// passed pointer) and may alter the result.
unsafe fn install_hook<T>(addr: usize, detour: T, what: &str) -> Result<(), HookError>
where
    T: Fn(*mut Registers, usize) -> usize + Send + Sync + 'static,
{
    // FFI firewall: ilhook invokes this detour from an `extern "win64"` trampoline with no catch of
    // its own, so a panic would unwind across that boundary into the game's input thread — UB under
    // `panic = "unwind"` (now every shipped profile; see docs/FFI-UNWIND-AUDIT.md). Wrap every detour
    // so a panic is contained: log it once (a repeat at input-poll rate would flood) and return 0.
    // The detour bodies are allocation-free with no `unwrap`/index/`panic!`, and the only fallible-
    // looking step (calling `original`) runs before any of our work — so this is soundness insurance
    // for an effectively-unreachable path. 0 reads as DI_OK / ERROR_SUCCESS / FALSE, all benign here.
    let what_log = what.to_string();
    let logged = AtomicBool::new(false);
    let guarded = move |regs: *mut Registers, original: usize| -> usize {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detour(regs, original))) {
            Ok(v) => v,
            Err(_) => {
                if !logged.swap(true, Ordering::Relaxed) {
                    // Contained log: we're already in the recovery arm, so a logging panic here would
                    // unwind across ilhook's trampoline — the boundary we just caught for.
                    crate::logger::error_contained(format_args!(
                        "input: '{what_log}' detour panicked; suppressed at the FFI boundary (returning 0)"
                    ));
                }
                0
            }
        }
    };
    let hook = unsafe { hook_closure_retn(addr, guarded, CallbackOption::None, HookFlags::empty()) }
        .map_err(|e| HookError::Install { what: what.to_string(), detail: format!("{e:?}") })?;
    std::mem::forget(hook);
    Ok(())
}

/// Create a throwaway DirectInput device for `guid` and read its `GetDeviceState` vtable address (the
/// real dinput8 implementation, shared by the game's own devices), then release the probe objects.
unsafe fn probe_get_device_state(
    di8_create: DInput8CreateFn,
    hinstance: usize,
    guid: &GUID,
) -> Result<usize, HookError> {
    let mut di8: RawObj = std::ptr::null_mut();
    let hr = unsafe { di8_create(hinstance, DIRECTINPUT_VERSION, &IID_IDIRECTINPUT8W, &mut di8, 0) };
    if hr != 0 || di8.is_null() {
        return Err(HookError::DInputProbe { what: "DirectInput8Create".to_string(), hr });
    }
    let create_device: CreateDeviceFn = unsafe { vtable_fn(di8, VTBL_CREATE_DEVICE) };
    let release_di8: ReleaseFn = unsafe { vtable_fn(di8, VTBL_RELEASE) };
    let mut device: RawObj = std::ptr::null_mut();
    let hr = unsafe { create_device(di8, guid, &mut device, 0) };
    if hr != 0 || device.is_null() {
        unsafe { release_di8(di8) };
        return Err(HookError::DInputProbe { what: format!("CreateDevice({guid:?})"), hr });
    }
    let addr = unsafe { *(*device).add(VTBL_GET_DEVICE_STATE) as usize };
    let release_device: ReleaseFn = unsafe { vtable_fn(device, VTBL_RELEASE) };
    unsafe { release_device(device) };
    unsafe { release_di8(di8) };
    Ok(addr)
}

/// Resolve an export to its address, mapping a missing one to an error string. `name` may be a string
/// (via `s!`) or an ordinal (`PCSTR(n as *const u8)`, the `MAKEINTRESOURCEA` convention).
fn resolve_proc(module: HMODULE, name: PCSTR, what: &'static str) -> Result<usize, HookError> {
    unsafe { GetProcAddress(module, name) }.map(|p| p as usize).ok_or(HookError::ExportNotFound(what))
}

/// Install the DirectInput `GetDeviceState` detours plus the controller (XInput) hook. Called once from
/// the deferred overlay-install thread (after the frontend is up; see `app::install`), off the game's
/// main/task threads. Returns a [`HookError`] (rather than panicking) so the caller can degrade —
/// without the hooks the overlay still draws, it just won't suppress game input while open.
///
/// # Safety
/// Standard hooking caveats: patches executable memory in the real `dinput8.dll`. Must run once, off
/// the main thread, before input is being polled concurrently.
pub unsafe fn install() -> Result<(), HookError> {
    // We are `dinput8.dll`; our proxy forwards `DirectInput8Create` to the real one (`proxy.rs`), so
    // the device we probe — and hook — is the real implementation the game also uses.
    let dinput8 = unsafe { GetModuleHandleA(s!("dinput8.dll")) }
        .map_err(|e| HookError::ModuleNotLoaded { module: "dinput8.dll", err: e })?;
    let di8_create: DInput8CreateFn =
        unsafe { std::mem::transmute(resolve_proc(dinput8, s!("DirectInput8Create"), "DirectInput8Create export")?) };
    let hinstance =
        unsafe { GetModuleHandleA(PCSTR::null()) }.map_err(HookError::ModuleHandle)?.0 as usize;

    let kb = unsafe { probe_get_device_state(di8_create, hinstance, &GUID_SYS_KEYBOARD)? };
    let ms = unsafe { probe_get_device_state(di8_create, hinstance, &GUID_SYS_MOUSE)? };

    // Keyboard and mouse devices often share one GetDeviceState implementation; hook each distinct
    // address once. (`install_hook` forgets each detour as it installs it — see its doc.)
    unsafe { install_hook(kb, |r, o| get_state_detour(r, o), "keyboard GetDeviceState")? };
    if ms != kb {
        unsafe { install_hook(ms, |r, o| get_state_detour(r, o), "mouse GetDeviceState")? };
    }

    // Cursor-unlock detours (user32), so the mouse can move over the menu while the overlay is open.
    let user32 = unsafe { GetModuleHandleA(s!("user32.dll")) }
        .map_err(|e| HookError::ModuleNotLoaded { module: "user32.dll", err: e })?;
    unsafe {
        install_hook(
            resolve_proc(user32, s!("SetCursorPos"), "SetCursorPos")?,
            |r, o| set_cursor_pos_detour(r, o),
            "SetCursorPos",
        )?
    };
    unsafe {
        install_hook(
            resolve_proc(user32, s!("ClipCursor"), "ClipCursor")?,
            |r, o| clip_cursor_detour(r, o),
            "ClipCursor",
        )?
    };

    // Controller hook is best-effort: a failure leaves keyboard/mouse suppression (the critical path)
    // intact, so log and continue rather than fail the whole install. Without it, the pad just isn't
    // blanked and can't drive the menu — the backtick key still toggles and navigates.
    match unsafe { install_xinput() } {
        Ok(()) => log::info!(
            "input: hooked XInput GetState (controller menu nav + RB+L3+R3 toggle; pad blanked while open)"
        ),
        Err(e) => log::warn!(
            "input: XInput hook not installed ({e}); controller won't drive the menu or be suppressed"
        ),
    }
    Ok(())
}

/// Hook `xinput1_4!XInputGetState`. ER statically imports `XINPUT1_4.dll`, so it's already loaded by
/// the time install runs. The toggle chord (RB+L3+R3), A, B, d-pad and sticks are all standard bits
/// the plain `XInputGetState` reports, so the detour reads the game's own buffer — no `XInputGetStateEx`
/// / Guide-bit dependency, and nothing for Steam Input to intercept.
///
/// KNOWN FATAL on native Windows (friend crash 2026-07-01, WER: `c0000005` at
/// `XINPUT1_4.dll+0x9a65` = `XInputGetState+5`): this inline patch collides with other XInput
/// hookers (likely Steam's gameoverlayrenderer). `XInputGetState` opens with the 5-byte hot-patch
/// prologue, and a 5-byte-convention hooker's trampoline jumps back to `entry+5` — mid-our-14-byte
/// ilhook jmp — executing garbage. Works on the vkd3d rig only because Wine's xinput stack has no
/// colliding hooker. Fix direction: an IAT hook on `eldenring.exe`'s import (no function-body
/// bytes touched). Full analysis: docs/OVERLAY-RENDERING.md > "WER Verdict".
///
/// # Safety
/// Same hooking caveats as [`install`]: patches executable memory in the loaded `xinput1_4.dll`; run
/// once, off the main thread.
unsafe fn install_xinput() -> Result<(), HookError> {
    let xinput = unsafe { GetModuleHandleA(s!("xinput1_4.dll")) }
        .map_err(|e| HookError::ModuleNotLoaded { module: "xinput1_4.dll", err: e })?;
    let get_state = resolve_proc(xinput, s!("XInputGetState"), "XInputGetState export")?;
    unsafe { install_hook(get_state, |r, o| xinput_get_state_detour(r, o), "XInputGetState")? };
    Ok(())
}
