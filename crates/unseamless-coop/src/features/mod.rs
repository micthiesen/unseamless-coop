//! Concrete features. Each module is one self-contained piece of mod behavior implementing
//! [`Feature`](crate::feature::Feature).

use unseamless_core::util::{Applied, Latch};

pub mod crit_coop;
pub mod death_debuffs;
pub mod notifications;
pub mod observer;
pub mod playstate;
pub mod seamless;
pub mod session_actions;
pub mod session_limit;
pub mod world_time;

/// Announce a config value just applied to a game field, with the shared policy of the
/// "hold a config value into an SDK field" features ([`session_limit`], [`seamless`], [`world_time`]):
/// classify the apply ([`Latch::classify`]) and **info-log on First/Changed, toast only on Changed**
/// (never the startup baseline), silent on a re-assert. The `info`/`toast` messages are built lazily,
/// so the steady-state (Reasserted) path allocates nothing. Returns the [`Applied`] classification so
/// a caller can add its own feature-specific debug re-assert line. Centralizing the "never toast the
/// baseline" decision keeps it from drifting across the (now three) call sites.
pub fn announce_held<T: Clone + PartialEq>(
    latch: &mut Latch<T>,
    value: T,
    info: impl FnOnce() -> String,
    toast: impl FnOnce() -> String,
) -> Applied {
    let applied = latch.classify(&value);
    if matches!(applied, Applied::First | Applied::Changed) {
        log::info!("{}", info());
        if applied == Applied::Changed {
            crate::notify::with_mut(|n| n.info(toast()));
        }
    }
    applied
}
