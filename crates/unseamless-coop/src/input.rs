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
//! stop it either). We detour it to do three things at once: (1) **blank** the pad while the overlay is
//! open (zero only the gamepad struct, never `dwPacketNumber` — the game treats an unchanged packet as
//! "reuse last input", which would *freeze* the pre-open input rather than neutralise it); (2) **read**
//! the live pad into [`PAD_SNAPSHOT`] so the overlay can drive its menu (d-pad / left-stick + A); and
//! (3) capture the **Guide/Home button** via the undocumented `XInputGetStateEx` (ordinal 100, exported
//! by both Windows and wine) — the plain `XInputGetState` masks that bit out, so ER never sees it,
//! which makes it the ideal overlay-toggle. The pure menu-translation ([`PadNav`] → per-frame edges)
//! lives in [`unseamless_core::pad`] (host-tested); this module is just the OS binding that feeds it.
//! (XInput is a flat C export, so unlike DirectInput there's no COM-vtable probe.)
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

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_retn};
use windows::Win32::Foundation::HMODULE;
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

// ---- Controller (XInput) --------------------------------------------------------------------------
//
// The pure menu-translation logic (edge/repeat state machine, button bits, thresholds) lives in
// `unseamless_core::pad`; this half is the OS binding — the `XInputGetState` hook, the atomic snapshot,
// reading the Guide bit. Re-exported so the overlay keeps a single `crate::input::Pad*` import path.
pub use unseamless_core::pad::{PadEdges, PadNav};

/// Latest pad sample, packed `buttons | (lx << 16) | (ly << 32)` so a frame reads consistently with one
/// atomic load (no tearing between buttons and stick). Written by the `XInputGetState` detour on the
/// game's input thread; read by [`pad_snapshot`] on the Present thread.
static PAD_SNAPSHOT: AtomicU64 = AtomicU64::new(0);
/// The controller index currently owning the snapshot (the pad driving menu nav), or `-1` for none.
/// Stops a second pad from stomping the first, and lets a disconnect of the owning pad reset the
/// snapshot to neutral so a direction held at disconnect can't auto-repeat in the menu forever.
static PAD_OWNER: AtomicI32 = AtomicI32::new(-1);
/// The real `XInputGetStateEx` (ordinal 100), resolved at install. Called by the detour to read the
/// Guide bit; `None` until installed (then the toggle/nav simply stay quiet).
static XINPUT_GET_STATE_EX: OnceLock<XInputGetStateExFn> = OnceLock::new();

type XInputGetStateExFn = unsafe extern "system" fn(u32, *mut XInputState) -> u32;

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

thread_local! {
    /// Re-entrancy guard for [`capture_pad`]. A third-party `xinput1_4.dll` shim (Steam Input, x360ce,
    /// …) can implement `XInputGetStateEx` by calling the (hooked) `XInputGetState`, which would
    /// otherwise recurse `detour → capture_pad → Ex → detour → …` straight to a stack-overflow abort
    /// (fatal under `panic = "abort"`). The guard caps the depth at one. Per-thread, since the recursion
    /// is same-thread.
    static IN_CAPTURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Clears [`IN_CAPTURE`] on scope exit (incl. early returns / a panic) so a bailed [`capture_pad`]
/// can't wedge the guard permanently on.
struct CaptureGuard;
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        IN_CAPTURE.with(|g| g.set(false));
    }
}

/// Read the full pad state (incl. the Guide bit) via `XInputGetStateEx` and publish it to
/// [`PAD_SNAPSHOT`], claiming ownership for `user_index` unless another connected pad already holds it.
/// Called only for a game-confirmed-connected index. This is a second (stateless) XInput poll per
/// connected pad on every game poll — a permanent overhead, the cost of reading the Guide bit the plain
/// API hides; small, but not free.
fn capture_pad(user_index: u32) {
    if IN_CAPTURE.with(|g| g.replace(true)) {
        return; // re-entered via a shim's Ex→GetState; bail rather than recurse (outer call owns the guard)
    }
    let _guard = CaptureGuard; // resets IN_CAPTURE on return
    let Some(ex) = XINPUT_GET_STATE_EX.get() else { return };
    let mut st = XInputState::default();
    // ERROR_SUCCESS == 0; anything else means it raced to disconnected between the game's read and ours.
    if unsafe { ex(user_index, &mut st) } != 0 {
        return;
    }
    let idx = user_index as i32;
    let owner = PAD_OWNER.load(Ordering::Relaxed);
    // Claim the snapshot if it's free or already ours, so a second controller never stomps the pad
    // currently driving the menu. (`release_pad` frees it when the owner disconnects.)
    if owner < 0 || owner == idx {
        PAD_OWNER.store(idx, Ordering::Relaxed);
        let g = st.gamepad;
        PAD_SNAPSHOT.store(pack_pad(g.buttons, g.thumb_lx, g.thumb_ly), Ordering::Relaxed);
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
fn xinput_get_state_detour(regs: *mut Registers, original: usize) -> usize {
    let (user_index, state) = unsafe { ((*regs).rcx as u32, (*regs).rdx as *mut XInputState) };
    let original: unsafe extern "system" fn(u32, *mut XInputState) -> u32 =
        unsafe { std::mem::transmute(original) };
    let ret = unsafe { original(user_index, state) };
    if ret == 0 {
        // Connected (ERROR_SUCCESS): `state` was written and the index is real. Capture for our menu,
        // then blank if the overlay is open.
        capture_pad(user_index);
        if is_blocked() && !state.is_null() {
            unsafe {
                (*state).gamepad = XInputGamepad::default();
                // Also bump packet_number. A game that skips re-reading on an unchanged packet would
                // otherwise keep reusing the last *real* input — a steadily-held stick leaves the
                // device's packet constant, so zeroing the gamepad alone wouldn't be seen. Bumping
                // guarantees the game re-reads the now-neutral gamepad every blanked poll.
                (*state).packet_number = (*state).packet_number.wrapping_add(1);
            }
        }
    } else {
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
fn get_state_detour(regs: *mut Registers, original: usize) -> usize {
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
fn set_cursor_pos_detour(regs: *mut Registers, original: usize) -> usize {
    if is_blocked() {
        return 1; // TRUE — claim success without moving the cursor
    }
    let original: unsafe extern "system" fn(u64, u64) -> usize = unsafe { std::mem::transmute(original) };
    unsafe { original((*regs).rcx, (*regs).rdx) }
}

/// `ClipCursor(lpRect = rcx) -> BOOL`. While blocked, release the cursor (call with NULL) instead of
/// confining it to the game's rect.
fn clip_cursor_detour(regs: *mut Registers, original: usize) -> usize {
    let lp_rect = if is_blocked() { 0 } else { unsafe { (*regs).rcx } };
    let original: unsafe extern "system" fn(u64) -> usize = unsafe { std::mem::transmute(original) };
    unsafe { original(lp_rect) }
}

/// Install one function-replacement detour and **forget it immediately**, so it's resident for the
/// process lifetime — never unhooked. Forgetting per-hook (rather than collecting handles and forgetting
/// at the end) means a *later* hook failing can't drop — and thus unpatch live code / free a closure a
/// concurrent call points into (UAF) — an already-installed detour. `detour` runs the original (via the
/// passed pointer) and may alter the result.
unsafe fn install_hook<T>(addr: usize, detour: T, what: &str) -> Result<(), String>
where
    T: Fn(*mut Registers, usize) -> usize + Send + Sync + 'static,
{
    let hook = unsafe { hook_closure_retn(addr, detour, CallbackOption::None, HookFlags::empty()) }
        .map_err(|e| format!("hooking {what}: {e:?}"))?;
    std::mem::forget(hook);
    Ok(())
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

/// Resolve an export to its address, mapping a missing one to an error string. `name` may be a string
/// (via `s!`) or an ordinal (`PCSTR(n as *const u8)`, the `MAKEINTRESOURCEA` convention).
fn resolve_proc(module: HMODULE, name: PCSTR, what: &str) -> Result<usize, String> {
    unsafe { GetProcAddress(module, name) }.map(|p| p as usize).ok_or_else(|| format!("{what} not found"))
}

/// Install the DirectInput `GetDeviceState` detours plus the controller (XInput) hook. Called once from
/// the deferred overlay-install thread (after the frontend is up; see `app::install`), off the game's
/// main/task threads. Returns an error string (rather than panicking) so the caller can degrade —
/// without the hooks the overlay still draws, it just won't suppress game input while open.
///
/// # Safety
/// Standard hooking caveats: patches executable memory in the real `dinput8.dll`. Must run once, off
/// the main thread, before input is being polled concurrently.
pub unsafe fn install() -> Result<(), String> {
    // We are `dinput8.dll`; our proxy forwards `DirectInput8Create` to the real one (`proxy.rs`), so
    // the device we probe — and hook — is the real implementation the game also uses.
    let dinput8 =
        unsafe { GetModuleHandleA(s!("dinput8.dll")) }.map_err(|e| format!("dinput8.dll not loaded: {e}"))?;
    let di8_create: DInput8CreateFn =
        unsafe { std::mem::transmute(resolve_proc(dinput8, s!("DirectInput8Create"), "DirectInput8Create export")?) };
    let hinstance =
        unsafe { GetModuleHandleA(PCSTR::null()) }.map_err(|e| format!("GetModuleHandle failed: {e}"))?.0
            as usize;

    let kb = unsafe { probe_get_device_state(di8_create, hinstance, &GUID_SYS_KEYBOARD)? };
    let ms = unsafe { probe_get_device_state(di8_create, hinstance, &GUID_SYS_MOUSE)? };

    // Keyboard and mouse devices often share one GetDeviceState implementation; hook each distinct
    // address once. (`install_hook` forgets each detour as it installs it — see its doc.)
    unsafe { install_hook(kb, get_state_detour, "keyboard GetDeviceState")? };
    if ms != kb {
        unsafe { install_hook(ms, get_state_detour, "mouse GetDeviceState")? };
    }

    // Cursor-unlock detours (user32), so the mouse can move over the menu while the overlay is open.
    let user32 =
        unsafe { GetModuleHandleA(s!("user32.dll")) }.map_err(|e| format!("user32.dll not loaded: {e}"))?;
    unsafe { install_hook(resolve_proc(user32, s!("SetCursorPos"), "SetCursorPos")?, set_cursor_pos_detour, "SetCursorPos")? };
    unsafe { install_hook(resolve_proc(user32, s!("ClipCursor"), "ClipCursor")?, clip_cursor_detour, "ClipCursor")? };

    // Controller hook is best-effort: a failure leaves keyboard/mouse suppression (the critical path)
    // intact, so log and continue rather than fail the whole install. Without it, the pad just isn't
    // blanked and can't drive the menu — the backtick key still toggles and navigates.
    match unsafe { install_xinput() } {
        Ok(()) => log::info!(
            "input: hooked XInput GetState (controller menu nav + Home-button toggle; pad blanked while open)"
        ),
        Err(e) => log::warn!(
            "input: XInput hook not installed ({e}); controller won't drive the menu or be suppressed"
        ),
    }
    Ok(())
}

/// Hook `xinput1_4!XInputGetState` and resolve `XInputGetStateEx` (ordinal 100). ER statically imports
/// `XINPUT1_4.dll`, so it's already loaded by the time install runs.
///
/// Note: the Guide/Home toggle depends on that button reaching the game's XInput layer. Under
/// Proton/Steam, Steam Input commonly intercepts Guide for its own overlay, so the toggle may be inert
/// on the rig even when this installs cleanly — that's an accepted limitation (the backtick key always
/// toggles). Verify on the rig rather than assuming it works.
///
/// # Safety
/// Same hooking caveats as [`install`]: patches executable memory in the loaded `xinput1_4.dll`; run
/// once, off the main thread.
unsafe fn install_xinput() -> Result<(), String> {
    let xinput = unsafe { GetModuleHandleA(s!("xinput1_4.dll")) }
        .map_err(|e| format!("xinput1_4.dll not loaded: {e}"))?;
    let get_state = resolve_proc(xinput, s!("XInputGetState"), "XInputGetState export")?;
    // XInputGetStateEx is exported by ordinal 100 only (no name on real Windows); resolve it via
    // MAKEINTRESOURCEA(100) — i.e. a PCSTR whose pointer value *is* the ordinal — so it works on both
    // Windows and wine/Proton. It returns the Guide bit the plain API masks; that's our toggle.
    let ex_addr = resolve_proc(xinput, PCSTR(100 as *const u8), "XInputGetStateEx (ordinal 100)")?;
    let ex: XInputGetStateExFn = unsafe { std::mem::transmute(ex_addr) };
    // First (and only) writer wins; install runs once. Ignore the Result — a redundant set is harmless.
    let _ = XINPUT_GET_STATE_EX.set(ex);
    unsafe { install_hook(get_state, xinput_get_state_detour, "XInputGetState")? };
    Ok(())
}
