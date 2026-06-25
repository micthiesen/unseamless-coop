//! Seamless roaming: let the party explore the whole map together instead of being tethered to the
//! host's multiplay area — the defining "seamless" behavior.
//!
//! The game keeps co-op phantoms inside the host's area via `CSStayInMultiplayAreaWarpData`, warping
//! anyone who steps out back. That struct exposes `disable_multiplay_restriction`, which the SDK
//! documents as completely disabling multiplay area restrictions so the player can go anywhere on the
//! map. We hold it to [`roam_anywhere`](unseamless_core::config::Gameplay::roam_anywhere) each frame —
//! a single-`bool` **state write**, Arxan-immune (Arxan restores code, not runtime data), the same
//! low-risk lever shape as [`session_limit`](crate::features::session_limit). Write-if-different +
//! self-healing, since the warp data re-initializes when a session forms.
//!
//! Reads the **live** config (`crate::state`) so a `ConfigSync` from the host re-applies here without
//! rebuilding the feature. The roam *effect* needs a live multiplayer session to observe (deferred to
//! a rig/party run); the write itself is visible solo via the session observer's change log, which
//! prints `restriction_disabled` whenever the session state changes.

use eldenring::cs::CSSessionManager;
use unseamless_core::util::{Applied, Latch};

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct SeamlessRoam {
    /// Classifies each apply (first / re-assert / change). The classification is the host-tested
    /// shared bit (`Latch::classify`); the debug/info/toast mapping below stays local.
    latch: Latch<bool>,
}

impl SeamlessRoam {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Feature for SeamlessRoam {
    fn name(&self) -> &'static str {
        "seamless-roam"
    }

    // Default phase (`FrameBegin`): this is session config, not frame-order-sensitive game state, and
    // must be held before/while a session is up — the same reasoning as `session_limit`.

    fn on_frame(&mut self, _tick: Tick) {
        let desired = crate::state::with(|c| c.gameplay.roam_anywhere);

        // `Some(true)` = we just wrote it; `Some(false)` = already correct (or the warp data isn't
        // wired yet); `None` = no session manager live yet — retry next frame either way.
        let wrote = crate::sdk::with_instance_mut::<CSSessionManager, _>(|s| {
            // `session::tether_mut` null-guards the warp-data `OwnedPtr` (it may be unwired pre-session;
            // reading the address is not a deref) — the one place that guard lives, shared with the read
            // side (observer + diag report). `None` -> skip this frame. (Deref soundness once non-null
            // is rig-gated, like the read side.)
            let Some(warp) = crate::session::tether_mut(s) else {
                return false;
            };
            if warp.disable_multiplay_restriction == desired {
                return false;
            }
            warp.disable_multiplay_restriction = desired;
            true
        });

        if wrote == Some(true) {
            // Shared announce policy (info on First/Changed, toast only on Changed); the self-heal
            // debug line is local since it fires only on an actual write here.
            let applied = crate::features::announce_held(
                &mut self.latch,
                desired,
                || format!("seamless roam set to {desired} (disable_multiplay_restriction)"),
                || {
                    if desired { "Roaming enabled" } else { "Roaming disabled (vanilla area tether)" }.to_string()
                },
            );
            if applied == Applied::Reasserted {
                log::debug!("re-applied roam_anywhere = {desired}");
            }
        }
    }
}
