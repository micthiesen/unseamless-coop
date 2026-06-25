//! Seamless roaming: let the party explore the whole map together instead of being tethered to the
//! host's multiplay area — the defining "seamless" behavior.
//!
//! The game keeps co-op phantoms inside the host's area via `CSStayInMultiplayAreaWarpData`, warping
//! anyone who steps out back. That struct exposes `disable_multiplay_restriction`, documented in the
//! SDK as "set true to completely disable multiplay area restrictions, allowing the player to go
//! anywhere on the map". We hold it to [`roam_anywhere`](unseamless_core::config::Gameplay::roam_anywhere)
//! each frame — a single-`bool` **state write**, Arxan-immune (Arxan restores code, not runtime data),
//! the same low-risk lever shape as [`session_limit`](crate::features::session_limit). Write-if-different
//! + self-healing, since the warp data re-initializes when a session forms.
//!
//! Reads the **live** config (`crate::state`) so a `ConfigSync` from the host re-applies here without
//! rebuilding the feature. The roam *effect* needs a live multiplayer session to observe (deferred to
//! a rig/party run); the write itself is visible solo via the session observer's teardown probe, which
//! logs `restriction_disabled`.

use eldenring::cs::CSSessionManager;

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct SeamlessRoam {
    /// Last value logged at `info`, so steady-state re-asserts (per-session re-init) stay at `debug`
    /// but a genuine change (config-driven) logs loudly and toasts.
    last_logged: Option<bool>,
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

        // `Some(true)` = we just wrote it; `Some(false)` = already correct; `None` = no session
        // manager live yet (pre-init / between loads) — retry next frame.
        let wrote = crate::sdk::with_instance_mut::<CSSessionManager, _>(|s| {
            let warp = &mut s.stay_in_multiplay_area_warp_data;
            if warp.disable_multiplay_restriction == desired {
                return false;
            }
            warp.disable_multiplay_restriction = desired;
            true
        });

        if wrote == Some(true) {
            if self.last_logged == Some(desired) {
                log::debug!("re-applied roam_anywhere = {desired}");
            } else {
                log::info!("seamless roam set to {desired} (disable_multiplay_restriction)");
                // Toast only a genuine *change* (e.g. a host ConfigSync), not the startup baseline
                // (`last_logged == None`, every launch) nor the per-session self-heal re-asserts.
                if self.last_logged.is_some() {
                    let msg = if desired { "Roaming enabled" } else { "Roaming disabled (vanilla area tether)" };
                    crate::notify::with_mut(|n| n.info(msg));
                }
                self.last_logged = Some(desired);
            }
        }
    }
}
