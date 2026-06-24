//! In-game notification model: transient **toasts** (auto-expiring) and persistent **banners**
//! (stay until cleared). Pure and host-tested — a renderer draws [`Notifications::toasts`] and
//! [`Notifications::banners`] each frame.
//!
//! ## Rendering is separate (and not built yet)
//! There's no UI surface wired up yet. The planned egui overlay (rig-gated) will draw these, or
//! we forward them to the game's own on-screen announcement system once that's reverse-engineered
//! (ERSC's `YKNX3_*` messages). This model is agnostic to which: features push notifications, and
//! whatever renderer exists consumes the active set.
//!
//! Toast lifetimes are wall-clock seconds (driven by `FD4TaskData::delta_time` via the cdylib's
//! `Tick`), so they expire at the same rate regardless of framerate.

/// How prominent / what color a message is. The renderer maps these to styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// A transient message that disappears on its own after [`remaining`](Toast::remaining) seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub message: String,
    pub severity: Severity,
    /// Seconds left before this toast expires.
    pub remaining: f32,
}

/// A persistent message that stays until explicitly cleared, keyed by a stable `id` so the same
/// condition (e.g. "lost connection to host") updates in place rather than stacking.
#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub id: &'static str,
    pub message: String,
    pub severity: Severity,
}

/// Default lifetime for a toast when a caller doesn't specify one.
pub const DEFAULT_TOAST_SECS: f32 = 4.0;
/// Cap on simultaneously-queued toasts; the oldest is dropped when exceeded so a burst can't grow
/// unbounded or bury the screen.
const MAX_TOASTS: usize = 8;

/// Owns the active notifications. One instance lives in the cdylib's app state; features push to
/// it and the renderer reads it.
#[derive(Debug, Default)]
pub struct Notifications {
    toasts: Vec<Toast>,
    banners: Vec<Banner>,
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show a transient toast for `secs` seconds. Oldest toasts are evicted past [`MAX_TOASTS`].
    pub fn toast(&mut self, severity: Severity, message: impl Into<String>, secs: f32) {
        self.toasts.push(Toast {
            message: message.into(),
            severity,
            remaining: secs.max(0.0),
        });
        if self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0); // drop-oldest
        }
    }

    /// Convenience: an info/warning/error toast with the default lifetime.
    pub fn info(&mut self, message: impl Into<String>) {
        self.toast(Severity::Info, message, DEFAULT_TOAST_SECS);
    }
    pub fn warn(&mut self, message: impl Into<String>) {
        self.toast(Severity::Warning, message, DEFAULT_TOAST_SECS);
    }
    pub fn error(&mut self, message: impl Into<String>) {
        self.toast(Severity::Error, message, DEFAULT_TOAST_SECS);
    }

    /// Set (or replace, by `id`) a persistent banner. Re-setting the same `id` updates it in place
    /// rather than adding a duplicate, so a recurring condition shows one banner.
    pub fn set_banner(&mut self, id: &'static str, severity: Severity, message: impl Into<String>) {
        let message = message.into();
        if let Some(existing) = self.banners.iter_mut().find(|b| b.id == id) {
            existing.severity = severity;
            existing.message = message;
        } else {
            self.banners.push(Banner { id, message, severity });
        }
    }

    /// Remove a persistent banner by `id` (no-op if absent). Returns whether one was removed.
    pub fn clear_banner(&mut self, id: &str) -> bool {
        let before = self.banners.len();
        self.banners.retain(|b| b.id != id);
        self.banners.len() != before
    }

    /// Advance time by `delta` seconds: age toasts and drop any that have expired. Banners are
    /// persistent and unaffected.
    pub fn tick(&mut self, delta: f32) {
        for toast in &mut self.toasts {
            toast.remaining -= delta.max(0.0);
        }
        self.toasts.retain(|t| t.remaining > 0.0);
    }

    /// Active toasts, oldest first (the renderer can reverse for newest-on-top).
    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Active persistent banners, in insertion order.
    pub fn banners(&self) -> &[Banner] {
        &self.banners
    }

    /// Whether there's anything to draw (lets the renderer skip work when idle).
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty() && self.banners.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_expires_after_its_lifetime() {
        let mut n = Notifications::new();
        n.toast(Severity::Info, "saved", 1.0);
        assert_eq!(n.toasts().len(), 1);
        n.tick(0.6);
        assert_eq!(n.toasts().len(), 1, "still alive at 0.6s");
        n.tick(0.6);
        assert!(n.toasts().is_empty(), "expired past 1.0s");
    }

    #[test]
    fn toasts_are_fifo_and_capped() {
        let mut n = Notifications::new();
        for i in 0..(MAX_TOASTS + 3) {
            n.toast(Severity::Info, format!("msg{i}"), 10.0);
        }
        assert_eq!(n.toasts().len(), MAX_TOASTS, "capped");
        // The three oldest were evicted, so the front is msg3.
        assert_eq!(n.toasts()[0].message, "msg3");
    }

    #[test]
    fn convenience_helpers_set_severity_and_default_lifetime() {
        let mut n = Notifications::new();
        n.warn("careful");
        assert_eq!(n.toasts()[0].severity, Severity::Warning);
        assert_eq!(n.toasts()[0].remaining, DEFAULT_TOAST_SECS);
    }

    #[test]
    fn banner_replaces_in_place_by_id() {
        let mut n = Notifications::new();
        n.set_banner("conn", Severity::Warning, "connecting…");
        n.set_banner("conn", Severity::Error, "disconnected");
        assert_eq!(n.banners().len(), 1, "same id updates in place");
        assert_eq!(n.banners()[0].severity, Severity::Error);
        assert_eq!(n.banners()[0].message, "disconnected");
    }

    #[test]
    fn distinct_banner_ids_coexist_and_clear_individually() {
        let mut n = Notifications::new();
        n.set_banner("conn", Severity::Error, "down");
        n.set_banner("ver", Severity::Warning, "version mismatch");
        assert_eq!(n.banners().len(), 2);
        assert!(n.clear_banner("conn"));
        assert!(!n.clear_banner("conn"), "second clear is a no-op");
        assert_eq!(n.banners().len(), 1);
        assert_eq!(n.banners()[0].id, "ver");
    }

    #[test]
    fn tick_does_not_expire_persistent_banners() {
        let mut n = Notifications::new();
        n.set_banner("conn", Severity::Error, "down");
        n.tick(1000.0);
        assert_eq!(n.banners().len(), 1, "banners are persistent");
    }

    #[test]
    fn is_empty_reflects_both_kinds() {
        let mut n = Notifications::new();
        assert!(n.is_empty());
        n.info("hi");
        assert!(!n.is_empty());
        n.tick(DEFAULT_TOAST_SECS + 1.0);
        assert!(n.is_empty());
        n.set_banner("x", Severity::Info, "y");
        assert!(!n.is_empty());
    }
}
