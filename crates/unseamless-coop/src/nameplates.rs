//! Process-global bridge between the game-thread nameplate projector and the Present-thread draw.
//!
//! Overhead nameplates work the same two-halves way as [`crate::debug_panel`]: the overlay must never
//! read game singletons, so a game-thread feature ([`crate::features::nameplates`]) reads the camera +
//! peer positions, **projects** each to screen NDC ([`unseamless_core::projection`]), and publishes a
//! list of [`NameplateLabel`]s here; the overlay reads them non-blocking on the Present thread, maps
//! NDC→pixels (it's the side that knows the framebuffer size), and draws.
//!
//! Publishing NDC (not pixels) is deliberate — see [`unseamless_core::projection`]. When nameplates
//! are off or nothing is visible, the published list is empty and the overlay draws nothing.

use std::sync::{Mutex, OnceLock, TryLockError};

/// One projected nameplate ready to draw: where (screen NDC) and what (text). The game thread fills
/// these; the overlay converts [`ndc`](NameplateLabel::ndc) to pixels and draws [`text`](NameplateLabel::text).
///
/// Per-peer extras the design calls for (ping, soul level, death count)
/// are formatted into [`text`](NameplateLabel::text) by the host-tested core
/// ([`unseamless_core::nameplate`]) from a per-peer `PeerLabelData`, so this struct stays a flat
/// "where + what to draw" with no content logic; for now those stats are unavailable (rig-gated co-op
/// core) so a label is just the peer's name (or a placeholder). Keeping the projected NDC on this
/// struct means the overlay never touches the camera or any game state.
#[derive(Debug, Clone)]
pub struct NameplateLabel {
    /// Screen position in normalized device coords (`x,y ∈ [-1, 1]`, `+x` right, `+y` up). For a
    /// [`Plate`](NameplateKind::Plate) this is the peer's projected head; for an
    /// [`Edge`](NameplateKind::Edge) it's already clamped to the screen border by the projector.
    pub ndc: [f32; 2],
    /// View-space depth (meters from the camera) for a [`Plate`](NameplateKind::Plate), driving the
    /// text→dot distance LOD ([`unseamless_core::projection::is_dot_lod`]). For an
    /// [`Edge`](NameplateKind::Edge) marker it carries the peer's radial distance instead (the edge dot
    /// has no LOD); both are used only for farthest-first paint ordering across the set.
    pub depth: f32,
    /// RGB tint for this label and its dot, so each peer reads as a distinct, *stable* color
    /// ([`unseamless_core::palette::peer_color_for_id`]). The overlay applies the shared alpha at draw time.
    pub color: [f32; 3],
    /// The text drawn over the peer's head (a [`Plate`](NameplateKind::Plate) up close; ignored when the
    /// marker renders as a dot).
    pub text: String,
    /// How to draw this marker — a full overhead plate, or an off-screen edge indicator.
    pub kind: NameplateKind,
}

/// How a [`NameplateLabel`] renders, decided by the game-thread projector from the peer's screen
/// position. The overlay branches on this: a [`Plate`](NameplateKind::Plate) draws over the peer's head
/// (full text up close, a colored dot once [`is_dot_lod`](unseamless_core::projection::is_dot_lod)
/// trips); an [`Edge`](NameplateKind::Edge) draws only a colored dot pinned to the screen border,
/// pointing toward an off-screen peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameplateKind {
    /// On-screen: an overhead plate at [`ndc`](NameplateLabel::ndc) (text or distance-LOD dot).
    Plate,
    /// Off-screen: an edge-clamped indicator dot at [`ndc`](NameplateLabel::ndc) pointing at the peer.
    Edge,
}

/// Latest published label set, or `None` before the first publish. A `Mutex<Vec<_>>` (like
/// [`crate::debug_panel`]'s snapshot) read non-blocking from the Present thread.
static LABELS: OnceLock<Mutex<Vec<NameplateLabel>>> = OnceLock::new();

/// Initialize the label cell. Called once at install, before any feature ticks or the overlay renders.
pub fn init() {
    let _ = LABELS.set(Mutex::new(Vec::new()));
}

/// Publish the projected labels for this frame (game thread). No-op before [`init`]. The lock is held
/// only for the move-assign, so the Present thread's [`snapshot`] (a `try_lock`) almost never contends.
/// Publish an empty `Vec` to clear stale labels (e.g. when nameplates are disabled).
pub fn publish(labels: Vec<NameplateLabel>) {
    if let Some(m) = LABELS.get() {
        *m.lock().unwrap_or_else(|p| p.into_inner()) = labels;
    }
}

/// A **non-blocking** clone of the latest labels, for the overlay's Present thread (which must never
/// block on the game thread). `None` if uninitialized or momentarily contended — the overlay then
/// skips drawing nameplates this frame. Cloned out (rather than drawn under the lock) so an imgui draw
/// never holds the lock the game-thread publisher blocks on.
pub fn snapshot() -> Option<Vec<NameplateLabel>> {
    let m = LABELS.get()?;
    match m.try_lock() {
        Ok(guard) => Some(guard.clone()),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner().clone()),
        Err(TryLockError::WouldBlock) => None,
    }
}
