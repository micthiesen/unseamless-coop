//! Thin helpers over the SDK's singleton access, to centralize the `unsafe` and kill the
//! repeated `match unsafe { X::instance() } { Ok(..) => .., Err(_) => return }` boilerplate.

use fromsoftware_shared::FromStatic;

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
