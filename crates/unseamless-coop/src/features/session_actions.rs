//! Surfaces session actions the user requested from the overlay menu.
//!
//! The overlay (Present thread) enqueues a [`SessionAction`](unseamless_core::protocol::SessionAction)
//! when a menu row is activated; this feature drains the queue ([`crate::actionq`]) each frame on the
//! game thread and routes it. The lobby-lifecycle verbs (Open World / Join world / Leave world) drive
//! the co-op side-channel + lobby discovery ([`crate::coop`]); the host-only in-world toggles
//! (lock/PvP/…) are still the rig-gated apply layer ahead (rung 3) and toast a placeholder for now.
//!
//! The overlay only enqueues an action when its menu row is *enabled* (gated on Steam-readiness +
//! in-game + session state via [`unseamless_core::menu::SessionContext`]), so this layer trusts the
//! gating and doesn't re-check it.

use unseamless_core::notifications::{DEFAULT_TOAST_SECS, Severity};
use unseamless_core::protocol::SessionAction;

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct SessionActionsTick;

impl SessionActionsTick {
    pub fn new() -> Self {
        Self
    }
}

impl Feature for SessionActionsTick {
    fn name(&self) -> &'static str {
        "session-actions"
    }

    // Default phase (`FrameBegin`): runs every frame, including at menus/title where the overlay is
    // usable. Registered after `notifications` (the ager), so toasts it pushes age from next frame.

    fn on_frame(&mut self, _tick: Tick) {
        for action in crate::actionq::drain() {
            log::info!("menu requested session action {action:?}");
            match action {
                // Lobby lifecycle: start hosting / joining (reading the live config for the password),
                // or tear the session down. The co-op driver owns the progress banners + result toasts.
                SessionAction::OpenWorld => crate::coop::host(&crate::state::snapshot()),
                SessionAction::JoinWorld => crate::coop::join(&crate::state::snapshot()),
                SessionAction::LeaveWorld => crate::coop::leave(),
                // In-world toggles (host-only) are the rung-3 apply layer still ahead.
                SessionAction::LockWorld
                | SessionAction::UnlockWorld
                | SessionAction::TogglePvp
                | SessionAction::TogglePvpTeams
                | SessionAction::ToggleFriendlyFire => {
                    crate::notify::with_mut(|n| {
                        n.toast(
                            Severity::Info,
                            format!("{} (not wired up yet)", action.label()),
                            DEFAULT_TOAST_SECS,
                        )
                    });
                }
            }
        }
    }
}
