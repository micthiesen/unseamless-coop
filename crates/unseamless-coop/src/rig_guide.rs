//! Process-global bridge between the game-thread rig-guide feature and the Present-thread overlay
//! draw — the pinned step-banner **and** choice-modal channel, plus the overlay→game modal-input
//! return path.
//!
//! Like [`crate::debug_panel`]: the overlay must never read game singletons or run engine logic, so
//! the game-thread feature ([`crate::features::rig_guide`]) ticks the host-tested
//! [`GuideRunner`](unseamless_core::guide::GuideRunner) and **publishes** what to draw here — either a
//! pinned [`RigBanner`] (a normal/stub step) or a [`ChoiceView`] modal (a choice step). The overlay
//! reads it non-blocking on the Present thread and draws it. A choice modal also needs input back from
//! the overlay (the menu nav layer + the keyboard note field), so this module carries a second cell in
//! the **opposite** direction (like [`crate::actionq`]): the overlay pushes [`ModalInput`], the game
//! thread drains it into the next `GuideInput`.
//!
//! Debug-only, like the rest of the guide subsystem (`#[cfg(debug_assertions)]` in `crate::lib`).

use std::sync::{Mutex, OnceLock, TryLockError};

use unseamless_core::guide::ChoiceView;

/// A pinned step banner: the text (instruction + auto-appended control hints, possibly a `[PENDING …]`
/// stub marker) and its auto-assigned RGB colour. Built by the game-thread feature from a
/// [`TickResult`](unseamless_core::guide::TickResult); colours are never chosen in a guide.
#[derive(Clone, Debug)]
pub struct RigBanner {
    pub text: String,
    pub color: [f32; 3],
}

/// What the overlay should draw for the current guide step: a pinned [`RigBanner`] for a normal/stub
/// step, or a [`ChoiceView`] **modal** for a choice step (visually distinct, focused, blocking). The
/// published value is `Option<RigView>` — `None` when no guide is active or it's finished.
#[derive(Clone, Debug)]
pub enum RigView {
    Banner(RigBanner),
    Choice(ChoiceView),
}

/// Latest published view (`None` = no guide active / finished), like [`crate::debug_panel`]'s
/// snapshot cell — a `Mutex<Option<_>>` read non-blocking from the Present thread.
static VIEW: OnceLock<Mutex<Option<RigView>>> = OnceLock::new();

/// One frame of overlay→game **choice-modal input**: edge-latched nav/confirm (OR-accumulated by the
/// overlay, reset by the game thread on drain, so a press can't be lost if Present outpaces the game
/// frame) plus the current free-form note buffer (overwritten each Present frame).
#[derive(Clone, Default)]
struct ModalInput {
    up: bool,
    down: bool,
    confirm: bool,
    note: String,
}

/// The overlay→game modal-input cell. Written by the overlay (Present thread) while a modal is up,
/// drained by the feature (game thread) each tick.
static MODAL_INPUT: OnceLock<Mutex<ModalInput>> = OnceLock::new();

/// Initialize both cells. Called once at install (in `app::install`), before any feature ticks or the
/// overlay renders.
pub fn init() {
    let _ = VIEW.set(Mutex::new(None));
    let _ = MODAL_INPUT.set(Mutex::new(ModalInput::default()));
}

/// Publish the current view (game thread). No-op before [`init`]. The lock is held only for the
/// move-assign, so the Present thread's [`snapshot`] (a `try_lock`) almost never contends. Publish
/// `None` to clear a stale view (guide finished / not running).
pub fn publish(view: Option<RigView>) {
    if let Some(m) = VIEW.get() {
        *m.lock().unwrap_or_else(|p| p.into_inner()) = view;
    }
}

/// A **non-blocking** clone of the latest view, for the overlay's Present thread (which must never
/// block on the game thread). The outer `Option` is `None` if uninitialized or momentarily contended
/// (skip drawing this frame); the inner `Option` is `None` when no guide view is active.
pub fn snapshot() -> Option<Option<RigView>> {
    let m = VIEW.get()?;
    match m.try_lock() {
        Ok(guard) => Some(guard.clone()),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner().clone()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// Push this frame's modal input from the overlay (Present thread). Nav/confirm are **OR-latched** so a
/// one-shot press survives until the game thread drains it (Present can run faster than the game frame);
/// `note` is the current buffer, overwritten each frame. No-op before [`init`].
///
/// NB: this **blocks** (`lock()`), unlike the non-blocking `snapshot()` discipline the rest of this
/// module follows. That's deliberate: `drain`'s critical section is tiny and non-nesting (three bool
/// reads and a small `String` clone), so the stall is bounded and can't deadlock, and a `try_lock` here
/// would be *worse* — a contended frame would silently drop the one-shot `confirm`/nav edge (it's
/// `false` next frame, so it's lost, not re-latched). Keep `lock()`.
pub fn push_modal_input(up: bool, down: bool, confirm: bool, note: &str) {
    if let Some(m) = MODAL_INPUT.get() {
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        g.up |= up;
        g.down |= down;
        g.confirm |= confirm;
        // Reuse the buffer's capacity rather than reallocating each frame.
        g.note.clear();
        g.note.push_str(note);
    }
}

/// Drain the latched modal edges for one game-thread tick, returning `(up, down, confirm, note)` and
/// resetting the edges (the note persists — the overlay overwrites it each frame). Returns a quiet
/// frame before [`init`]. The returned `note` is owned so the caller can borrow it into a `GuideInput`.
pub fn drain_modal_input() -> (bool, bool, bool, String) {
    let Some(m) = MODAL_INPUT.get() else {
        return (false, false, false, String::new());
    };
    let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
    let edges = (g.up, g.down, g.confirm, g.note.clone());
    g.up = false;
    g.down = false;
    g.confirm = false;
    edges
}
