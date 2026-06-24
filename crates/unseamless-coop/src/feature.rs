//! The feature abstraction.
//!
//! A [`Feature`] is one unit of mod behavior that runs each frame in a chosen task phase. The
//! [`App`](crate::app) owns a set of features and drives them from the game's scheduler. Keeping
//! features small and self-contained is what lets the rewrite stay legible where ERSC is one
//! monolith.

use eldenring::cs::CSTaskGroupIndex;

/// Per-frame context handed to a [`Feature`]. `frame` counts this feature's own ticks; `delta`
/// is the frame's `FD4TaskData::delta_time` in seconds (for time-based cadence via
/// [`unseamless_core::util::Timer`]).
#[derive(Debug, Clone, Copy)]
pub struct Tick {
    pub frame: u64,
    // Consumed by time-based features via `unseamless_core::util::Timer`; the first such feature
    // is still ahead, so it's plumbed through but not yet read.
    #[allow(dead_code)]
    pub delta: f32,
}

pub trait Feature: Send {
    /// Stable short name, used in logs.
    fn name(&self) -> &'static str;

    /// Which frame phase this feature runs in. Default `FrameBegin` ticks every frame including
    /// menus/title (good for session-state work that exists before a save loads). Features that
    /// touch world/combat state should override with a phase ordered against that state (e.g.
    /// `ChrIns_PostPhysics`).
    fn phase(&self) -> CSTaskGroupIndex {
        CSTaskGroupIndex::FrameBegin
    }

    /// Called once per frame in [`phase`](Feature::phase). Runs on the game's main thread.
    fn on_frame(&mut self, tick: Tick);
}
