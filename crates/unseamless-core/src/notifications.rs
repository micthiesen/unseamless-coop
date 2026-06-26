//! In-game notification model: transient **toasts** (auto-expiring) and persistent **banners**
//! (stay until cleared). Pure and host-tested — a renderer draws [`Notifications::toasts`] and
//! [`Notifications::banners`] each frame.
//!
//! ## Rendering is separate
//! This model stays renderer-agnostic. The cdylib's overlay (`coop/overlay.rs`, hudhook DX12 + imgui)
//! draws these today; we could also forward them to the game's own on-screen announcement system if
//! that's reverse-engineered (ERSC's `YKNX3_*` messages). Features push notifications, and whatever
//! renderer exists consumes the active set.
//!
//! ## Ownership & cadence (decide at wiring time)
//! A single [`Notifications`] should live in the cdylib's app state and be `tick`ed **once per
//! frame** with `FD4TaskData::delta_time` — NOT once per feature, or toasts would age N× too fast
//! with N features. Features reach it through shared app state to push messages.
//!
//! ## Intentional non-features (so their absence reads as a decision)
//! - **Toasts are fire-and-forget**: no key, no update-in-place. (A future "progress" toast that
//!   updates would need a keyed variant.) Identical toasts *are* de-duplicated by content so a
//!   replaying error doesn't flood the screen.
//! - **Banners render in insertion order**, not by priority. A renderer with limited space can
//!   sort by [`Severity`] itself.

/// How prominent / what color a message is. The renderer maps these to styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl From<log::Level> for Severity {
    /// Map a log level to a toast severity (`Debug`/`Trace`/`Info` → `Info`), so the cdylib's
    /// buffered config-load notes (`Vec<(log::Level, String)>`) can be surfaced as toasts.
    fn from(level: log::Level) -> Self {
        match level {
            log::Level::Error => Severity::Error,
            log::Level::Warn => Severity::Warning,
            _ => Severity::Info,
        }
    }
}

/// A transient message that disappears on its own after [`remaining`](Toast::remaining) seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub message: String,
    pub severity: Severity,
    /// Seconds left before this toast expires.
    pub remaining: f32,
    /// The lifetime it started with, so a renderer can compute a fade progress
    /// (`1.0 - remaining / duration`).
    pub duration: f32,
}

/// A persistent message that stays until explicitly cleared, keyed by a stable `id` so the same
/// condition (e.g. "lost connection to host") updates in place rather than stacking. `id` is an
/// owned `String` so it can carry a runtime key (e.g. a peer tag).
#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub id: String,
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

    /// Show a transient toast for `secs` seconds. A non-positive or non-finite `secs` is treated
    /// as "use the default lifetime" / "don't show" respectively (a 0s toast would otherwise flash
    /// for one frame). If an identical live toast (same message + severity) exists, its timer is
    /// refreshed instead of stacking a duplicate. Oldest toasts are evicted past [`MAX_TOASTS`].
    pub fn toast(&mut self, severity: Severity, message: impl Into<String>, secs: f32) {
        let secs = if secs.is_finite() { secs } else { DEFAULT_TOAST_SECS };
        if secs <= 0.0 {
            return;
        }
        let message = message.into();

        // De-dup by content: a replaying error or a flapping condition refreshes one toast rather
        // than filling all the slots and evicting everything else.
        if let Some(existing) = self
            .toasts
            .iter_mut()
            .find(|t| t.severity == severity && t.message == message)
        {
            existing.remaining = existing.remaining.max(secs);
            existing.duration = existing.duration.max(secs);
            return;
        }

        self.toasts.push(Toast { message, severity, remaining: secs, duration: secs });
        if self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0); // drop-oldest; MAX_TOASTS is tiny so the O(n) shift is fine
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
    pub fn set_banner(&mut self, id: impl Into<String>, severity: Severity, message: impl Into<String>) {
        let id = id.into();
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

    /// Remove every persistent banner — e.g. on disconnect / session end, where every
    /// session-scoped banner ("connection lost", "version mismatch", …) should be torn down at
    /// once without the caller tracking each id.
    pub fn clear_all_banners(&mut self) {
        self.banners.clear();
    }

    /// Advance time by `delta` seconds: age toasts and drop any that have expired. A non-finite
    /// delta (a bad engine frame) is ignored. Banners are persistent and unaffected.
    pub fn tick(&mut self, delta: f32) {
        if !delta.is_finite() {
            return;
        }
        let delta = delta.max(0.0); // a negative delta must not extend lifetimes
        for toast in &mut self.toasts {
            toast.remaining -= delta;
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
    fn toast_expires_exactly_at_its_lifetime_boundary() {
        let mut n = Notifications::new();
        n.toast(Severity::Info, "saved", 1.0);
        n.tick(0.999);
        assert_eq!(n.toasts().len(), 1, "alive just before the boundary");
        let mut n = Notifications::new();
        n.toast(Severity::Info, "saved", 1.0);
        n.tick(1.0); // remaining hits exactly 0.0
        assert!(n.toasts().is_empty(), "remaining == 0.0 must drop (not >= 0.0)");
    }

    #[test]
    fn non_positive_or_non_finite_lifetime_is_handled() {
        let mut n = Notifications::new();
        n.toast(Severity::Info, "flash", 0.0);
        assert!(n.toasts().is_empty(), "0s toast must not show (would flash one frame)");
        n.toast(Severity::Info, "neg", -5.0);
        assert!(n.toasts().is_empty(), "negative lifetime is a no-op");
        n.toast(Severity::Info, "inf", f32::INFINITY);
        assert_eq!(n.toasts()[0].remaining, DEFAULT_TOAST_SECS, "non-finite -> default lifetime");
    }

    #[test]
    fn identical_toasts_are_deduplicated_and_refreshed() {
        let mut n = Notifications::new();
        n.toast(Severity::Warning, "config error", 2.0);
        n.tick(1.5); // remaining 0.5
        n.toast(Severity::Warning, "config error", 2.0); // same -> refresh, not a 2nd toast
        assert_eq!(n.toasts().len(), 1);
        assert_eq!(n.toasts()[0].remaining, 2.0, "timer refreshed");
        assert!(
            n.toasts()[0].remaining <= n.toasts()[0].duration,
            "refresh keeps remaining <= duration (progress fraction stays in [0,1])"
        );
        // Different severity is a distinct toast.
        n.toast(Severity::Error, "config error", 2.0);
        assert_eq!(n.toasts().len(), 2);
    }

    #[test]
    fn toasts_are_fifo_and_capped_in_order() {
        let mut n = Notifications::new();
        for i in 0..(MAX_TOASTS + 3) {
            n.toast(Severity::Info, format!("msg{i}"), 10.0);
        }
        let messages: Vec<&str> = n.toasts().iter().map(|t| t.message.as_str()).collect();
        // The three oldest were evicted; survivors are msg3..=msg10 in order.
        let expected: Vec<String> = (3..MAX_TOASTS + 3).map(|i| format!("msg{i}")).collect();
        assert_eq!(messages, expected.iter().map(String::as_str).collect::<Vec<_>>());
    }

    #[test]
    fn negative_delta_does_not_extend_lifetimes() {
        let mut n = Notifications::new();
        n.toast(Severity::Info, "x", 1.0);
        n.tick(-100.0);
        assert_eq!(n.toasts()[0].remaining, 1.0, "negative delta clamped to no-op");
        n.tick(f32::NAN);
        assert_eq!(n.toasts()[0].remaining, 1.0, "non-finite delta ignored");
    }

    #[test]
    fn convenience_helpers_set_severity_and_default_lifetime() {
        let mut n = Notifications::new();
        n.warn("careful");
        assert_eq!(n.toasts()[0].severity, Severity::Warning);
        assert_eq!(n.toasts()[0].remaining, DEFAULT_TOAST_SECS);
    }

    #[test]
    fn severity_maps_from_log_level() {
        assert_eq!(Severity::from(log::Level::Error), Severity::Error);
        assert_eq!(Severity::from(log::Level::Warn), Severity::Warning);
        assert_eq!(Severity::from(log::Level::Info), Severity::Info);
        assert_eq!(Severity::from(log::Level::Debug), Severity::Info);
    }

    #[test]
    fn banner_replaces_in_place_by_id_and_supports_dynamic_ids() {
        let mut n = Notifications::new();
        let peer = format!("peer-{:08x}", 0xdeadbeefu32); // a runtime-computed id
        n.set_banner(peer.clone(), Severity::Warning, "connecting…");
        n.set_banner(peer.clone(), Severity::Error, "disconnected");
        assert_eq!(n.banners().len(), 1, "same id updates in place");
        assert_eq!(n.banners()[0].severity, Severity::Error);
        assert_eq!(n.banners()[0].message, "disconnected");
    }

    #[test]
    fn distinct_banner_ids_coexist_clear_individually_and_clear_all() {
        let mut n = Notifications::new();
        n.set_banner("conn", Severity::Error, "down");
        n.set_banner("ver", Severity::Warning, "version mismatch");
        assert_eq!(n.banners().len(), 2);
        assert!(n.clear_banner("conn"));
        assert!(!n.clear_banner("conn"), "second clear is a no-op");
        assert_eq!(n.banners().len(), 1);
        n.set_banner("conn", Severity::Error, "down again");
        n.clear_all_banners();
        assert!(n.banners().is_empty(), "clear_all tears down everything");
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
