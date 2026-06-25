//! Surfaces session actions the user requested from the overlay menu.
//!
//! The overlay (Present thread) enqueues a [`SessionAction`](unseamless_core::protocol::SessionAction)
//! when a menu row is activated; this feature drains the queue ([`crate::actionq`]) each frame on the
//! game thread and, for now, logs + toasts the request. Actually performing the action against the
//! live session — broadcasting it over the side-channel / driving the game's session FSM — is the
//! rig-gated apply layer still ahead; this is the seam it will plug into.

use unseamless_core::notifications::{DEFAULT_TOAST_SECS, Severity};

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
            // Not yet executed — the apply layer (side-channel broadcast / session FSM) is rig-gated.
            log::info!("menu requested session action {action:?} (not wired up yet)");
            crate::notify::with_mut(|n| {
                n.toast(Severity::Info, format!("{} (not wired up yet)", action.label()), DEFAULT_TOAST_SECS)
            });
        }
    }
}
