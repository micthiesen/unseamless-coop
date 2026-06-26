//! Thin helpers over the SDK's singleton access, to centralize the `unsafe` and kill the
//! repeated `match unsafe { X::instance() } { Ok(..) => .., Err(_) => return }` boilerplate.

use eldenring::cs::{ChrInsExt, PlayerIns, WorldChrMan};
use fromsoftware_shared::{FromStatic, Subclass};

/// Run `f` with a shared reference to singleton `T`, or return `None` if it isn't live yet.
///
/// # Safety contract
/// Wraps the SDK's `unsafe` instance access. Only call from the game's main thread (i.e. inside
/// a task callback); the returned reference must not escape `f`.
pub fn with_instance<T, R>(f: impl FnOnce(&T) -> R) -> Option<R>
where
    T: FromStatic + 'static,
{
    match unsafe { T::instance() } {
        Ok(t) => Some(f(t)),
        Err(_) => None,
    }
}

/// Like [`with_instance`] but a mutable reference. Same main-thread requirement.
// The standard accessor for mutating features (e.g. scaling via SoloParamRepository). First used by
// `features::session_limit`, which writes `CSSessionManager::session_player_limit_override`.
pub fn with_instance_mut<T, R>(f: impl FnOnce(&mut T) -> R) -> Option<R>
where
    T: FromStatic + 'static,
{
    match unsafe { T::instance_mut() } {
        Ok(t) => Some(f(t)),
        Err(_) => None,
    }
}

/// Run `f` with the local player, but only when it's fully active — skip a mid-load/teardown
/// half-wired `ChrIns` (`chr_flags1c8.is_active()`), per the CLAUDE.md load-status caveat. Returns
/// `None` if there's no live, active main player. Game-thread only (call from a feature `on_frame`).
pub fn with_active_main_player<R>(f: impl FnOnce(&mut PlayerIns) -> R) -> Option<R> {
    with_instance_mut::<WorldChrMan, _>(|w| {
        let player = w.main_player.as_mut()?;
        if !player.superclass().chr_flags1c8.is_active() {
            return None;
        }
        Some(f(player))
    })
    .flatten()
}

/// Apply a SpEffect to the local player by row id (`dont_sync = true` keeps it local, not networked —
/// what per-player effects like death debuffs / rune-arc want). Returns whether it was
/// applied (i.e. a live, active player was present). The shared lever behind several features.
pub fn apply_speffect_to_main_player(id: i32, dont_sync: bool) -> bool {
    with_active_main_player(|p| p.apply_speffect(id, dont_sync)).is_some()
}

/// Remove a SpEffect from the local player by row id. Returns whether a live, active player was present.
pub fn remove_speffect_from_main_player(id: i32) -> bool {
    with_active_main_player(|p| p.remove_speffect(id)).is_some()
}
