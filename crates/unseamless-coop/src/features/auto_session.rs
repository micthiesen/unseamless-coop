//! Config-driven **auto-start** of a co-op session — the headless equivalent of the overlay's Open
//! World / Join world actions. With `[debug] auto_session = "host" | "join"`, this fires the matching
//! [`crate::coop`] action **once**, as soon as Steam networking is ready and we're in gameplay, with
//! no overlay interaction. It exists for a machine that can't use the overlay (e.g. the hudhook DX12
//! present-hook crashing on some native-Windows GPUs): set `[debug] overlay = false` + an
//! `auto_session`, and the machine still connects. `off` (the default) registers nothing.
//!
//! It gates on exactly the same preconditions the overlay menu uses to *enable* Open/Join (Steam
//! networking ready, in gameplay, and not already in a session), so it can't fire `host`/`join` before
//! they'd work. The derived-role rig guide composes with it: `auto_session` sets the lobby intent the
//! guide's connect step reads, so a guide on the *host* machine still derives `Host` from the
//! auto-triggered Open World.

use eldenring::cs::CSTaskGroupIndex;
use unseamless_core::config::{AutoSession, Config};

use crate::feature::{Feature, Tick};

/// Build the auto-session feature for `[debug] auto_session`, or an empty set when it's `off`. Mirrors
/// the `probe_features` / `rig_guide::feature` assembly so `app::build_features` can just `extend`.
pub fn feature(config: &Config) -> Vec<Box<dyn Feature>> {
    match config.debug.auto_session {
        AutoSession::Off => Vec::new(),
        intent => vec![Box::new(AutoSessionFeature { intent, fired: false })],
    }
}

/// Fires the configured Open/Join once the gate opens, then stays inert (`fired`).
pub struct AutoSessionFeature {
    intent: AutoSession,
    fired: bool,
}

impl Feature for AutoSessionFeature {
    fn name(&self) -> &'static str {
        "auto-session"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self, _tick: Tick) {
        if self.fired {
            return;
        }
        // Same gate the overlay uses to enable Open/Join: don't fire until Steam networking is up AND
        // we're in gameplay. Just wait (stay un-fired) on any frame they don't both hold.
        if crate::steam_ready::status() != crate::steam_ready::Status::Ready {
            return;
        }
        if !crate::playstate::in_gameplay() {
            return;
        }
        // Already in a session (a manual Open/Join beat us to it) — nothing to do; latch and stop.
        if crate::coop::session_flags().in_session {
            self.fired = true;
            return;
        }
        let cfg = crate::state::snapshot();
        match self.intent {
            AutoSession::Host => {
                log::info!("auto-session: opening world (host) [debug.auto_session]");
                crate::coop::host(&cfg);
            }
            AutoSession::Join => {
                log::info!("auto-session: joining world (join) [debug.auto_session]");
                crate::coop::join(&cfg);
            }
            AutoSession::Off => {} // unreachable (Off registers no feature), but keep the match total
        }
        self.fired = true;
    }
}
