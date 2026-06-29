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
//! ## Session lifecycle events
//! Beyond the generic toast/banner primitives, [`SessionEvent`] is a typed surface for the co-op
//! session lifecycle (peer join/leave/return, version mismatch, connection lost/restored, auth
//! failure). It centralizes the *presentation* of each event — which register/voice it speaks in
//! (lore for in-world presence, plain for diagnostics), its [`Severity`], whether it's a transient
//! toast or a persistent banner, and (for banners) that it's [session-scoped](BannerScope) so
//! [`clear_session_banners`](Notifications::clear_session_banners) tears it all down at session end.
//! The emit sites that *fire* these events live in the co-op layer (`coop/coop.rs`) and the sibling
//! [`peer`] module; the per-variant seam — including the inline notification logic `peer` already
//! performs that adopting this surface would reconcile — is documented on [`SessionEvent`].
//!
//! ## Intentional non-features (so their absence reads as a decision)
//! - **Toasts are fire-and-forget**: no key, no update-in-place. (A future "progress" toast that
//!   updates would need a keyed variant.) Identical toasts *are* de-duplicated by content so a
//!   replaying error doesn't flood the screen.
//! - **Banners render in insertion order**, not by priority. A renderer with limited space can
//!   sort by [`Severity`] itself.

// The lore-voiced presence wording is single-sourced in the sibling `peer` module. [`SessionEvent`]
// references those `&str` constants directly rather than re-stating them, so the presence wording is
// drift-proof *by construction*. (Note: this single-sourcing guarantee is enforced only for presence;
// the diagnostic variants take a free-form `detail: String` the caller builds from `peer`'s message
// constructors, so for those it's a caller convention, not a type-level guarantee — see [`SessionEvent`].)
// `peer` already owns a `Notifications` and calls into it, so this is a mutual intra-crate dependency
// (an allowed module cycle), not a layer inversion — `peer` owns no rendering, only the strings.
use crate::peer;

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

/// Lifetime scope of a [`Banner`] — both persist on screen until cleared; this only decides whether
/// a bulk [`clear_session_banners`](Notifications::clear_session_banners) at session end sweeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BannerScope {
    /// Tied to the current co-op session (peer version/auth/liveness conditions, the session-state
    /// banner). Swept by [`clear_session_banners`](Notifications::clear_session_banners) when the
    /// session ends, so the caller doesn't track each id. This is the default because every banner
    /// the mod raises today is session-bound.
    #[default]
    Session,
    /// Outlives any one session (e.g. a global config-clamp condition). Survives a session-end
    /// sweep; only an explicit [`clear_banner`](Notifications::clear_banner) /
    /// [`clear_all_banners`](Notifications::clear_all_banners) removes it.
    Persistent,
}

/// A persistent message that stays until explicitly cleared, keyed by a stable `id` so the same
/// condition (e.g. "lost connection to host") updates in place rather than stacking. `id` is an
/// owned `String` so it can carry a runtime key (e.g. a peer tag). `scope` decides whether a
/// session-end sweep clears it (see [`BannerScope`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub id: String,
    pub message: String,
    pub severity: Severity,
    pub scope: BannerScope,
}

/// Default lifetime for a toast when a caller doesn't specify one.
pub const DEFAULT_TOAST_SECS: f32 = 4.0;
/// Cap on simultaneously-queued toasts; the oldest is dropped when exceeded so a burst can't grow
/// unbounded or bury the screen.
const MAX_TOASTS: usize = 8;
/// Cap on simultaneously-active banners; the oldest is dropped when a *new* id would exceed it.
/// Banners are keyed by `id` and a peer drives some ids (e.g. a per-peer tag), so a peer cycling
/// through distinct ids could otherwise grow the banners `Vec` unbounded. Updating an existing id
/// never grows the `Vec`, so it isn't subject to this cap. Mirrors [`MAX_TOASTS`].
const MAX_BANNERS: usize = 8;

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

    /// Set (or replace, by `id`) a [session-scoped](BannerScope::Session) banner. Re-setting the
    /// same `id` updates it in place rather than adding a duplicate, so a recurring condition shows
    /// one banner. A *new* id past [`MAX_BANNERS`] evicts the oldest banner, so a peer cycling
    /// distinct ids can't grow the banners unbounded (an in-place update never grows the `Vec`, so
    /// it's exempt). For a banner that should outlive the session, use [`set_banner_scoped`] with
    /// [`BannerScope::Persistent`].
    ///
    /// [`set_banner_scoped`]: Notifications::set_banner_scoped
    pub fn set_banner(&mut self, id: impl Into<String>, severity: Severity, message: impl Into<String>) {
        self.set_banner_scoped(id, severity, message, BannerScope::Session);
    }

    /// Like [`set_banner`](Notifications::set_banner) but with an explicit [`BannerScope`]. Updating
    /// an existing `id` overwrites its scope too, so a banner can be re-classified in place.
    pub fn set_banner_scoped(
        &mut self,
        id: impl Into<String>,
        severity: Severity,
        message: impl Into<String>,
        scope: BannerScope,
    ) {
        let id = id.into();
        let message = message.into();
        if let Some(existing) = self.banners.iter_mut().find(|b| b.id == id) {
            existing.severity = severity;
            existing.message = message;
            existing.scope = scope;
        } else {
            self.banners.push(Banner { id, message, severity, scope });
            if self.banners.len() > MAX_BANNERS {
                // Drop the oldest *session* banner first. The cap exists to bound the peer-cycled,
                // per-peer ids (all session-scoped), so eviction must not silently remove a
                // [`Persistent`](BannerScope::Persistent) banner — that would break its "only an
                // explicit clear removes it" contract. Fall back to the oldest overall only if every
                // banner is persistent (a controlled, small set), keeping the cap a hard bound.
                let victim = self
                    .banners
                    .iter()
                    .position(|b| b.scope == BannerScope::Session)
                    .unwrap_or(0);
                self.banners.remove(victim); // MAX_BANNERS is tiny so the O(n) shift is fine
            }
        }
    }

    /// Remove a persistent banner by `id` (no-op if absent). Returns whether one was removed.
    pub fn clear_banner(&mut self, id: &str) -> bool {
        let before = self.banners.len();
        self.banners.retain(|b| b.id != id);
        self.banners.len() != before
    }

    /// Remove every banner regardless of scope. The blunt instrument; prefer
    /// [`clear_session_banners`](Notifications::clear_session_banners) at session end so a
    /// [persistent](BannerScope::Persistent) banner (e.g. a global config-clamp condition) survives.
    pub fn clear_all_banners(&mut self) {
        self.banners.clear();
    }

    /// Remove every [session-scoped](BannerScope::Session) banner — the disconnect / session-end
    /// teardown. Every session lifecycle banner ("connection lost", "version mismatch", auth
    /// failure, …) is torn down at once without the caller tracking each id, while any
    /// [`Persistent`](BannerScope::Persistent) banner is left in place. Returns how many were
    /// removed.
    pub fn clear_session_banners(&mut self) -> usize {
        let before = self.banners.len();
        self.banners.retain(|b| b.scope != BannerScope::Session);
        before - self.banners.len()
    }

    /// Present a co-op [`SessionEvent`]: route it to the right surface (transient toast vs. persistent
    /// banner) in the right voice/severity, with banners [session-scoped](BannerScope::Session) so a
    /// later [`clear_session_banners`](Notifications::clear_session_banners) tears them down. Dedup is
    /// inherited from [`toast`](Notifications::toast) / [`set_banner`](Notifications::set_banner): a
    /// repeated presence toast refreshes its timer instead of stacking, and a re-fired banner updates
    /// in place by its `key`. This is the single entry point the co-op layer calls per lifecycle edge.
    pub fn session_event(&mut self, event: SessionEvent) {
        let severity = event.severity();
        match event {
            // In-world presence is an *effect*, so it speaks in ER lore voice and carries no
            // mechanical values (no peer id) — the wording is single-sourced from `peer.rs` so it
            // can't drift from the rest of the presence surface. Transient toasts: presence is a
            // moment, not a standing condition.
            SessionEvent::PeerJoined => self.toast(severity, peer::PEER_ARRIVED_MESSAGE, DEFAULT_TOAST_SECS),
            SessionEvent::PeerLeft => self.toast(severity, peer::PEER_DEPARTED_MESSAGE, DEFAULT_TOAST_SECS),
            SessionEvent::PeerReturned => self.toast(severity, peer::PEER_RETURNED_MESSAGE, DEFAULT_TOAST_SECS),

            // Diagnostics speak plainly and *do* name the peer (the `detail`, built by the caller via
            // `peer.rs`'s single-sourced message constructors). They're standing conditions, so they
            // become session-scoped banners keyed per-peer: a flapping condition updates one banner
            // in place, and session end sweeps them all.
            SessionEvent::VersionMismatch { key, detail }
            | SessionEvent::ConnectionLost { key, detail }
            | SessionEvent::AuthFailed { key, detail } => self.set_banner(key, severity, detail),

            // Recovery: drop the matching "lost contact" banner and raise a brief plain confirmation.
            // (`PeerReturned` is the lore-voiced companion the co-op layer fires alongside this.)
            SessionEvent::ConnectionRestored { key } => {
                self.clear_banner(&key);
                self.toast(severity, CONNECTION_RESTORED_MESSAGE, DEFAULT_TOAST_SECS);
            }
        }
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

/// Plain-voiced confirmation that contact with a peer was re-established (the diagnostic counterpart
/// to the lore-voiced [`peer::PEER_RETURNED_MESSAGE`]). This is the one "gap" the presence/diagnostic
/// constructors in `peer.rs` don't already cover, so it's single-sourced here. Plain voice: a recovery
/// is a connection-status fact, not an in-world event.
pub const CONNECTION_RESTORED_MESSAGE: &str = "Contact re-established.";

/// A co-op session lifecycle event, modeled as a typed surface so the *presentation* of each edge —
/// voice (lore vs. plain), [`Severity`], toast-vs-banner, session scope, dedup — lives in one place
/// and the co-op layer just fires the event. Drive every variant through
/// [`Notifications::session_event`].
///
/// **Voice** (per CLAUDE.md): in-world *presence* (join/leave/return) is an effect → terse FromSoft
/// lore voice, **no** mechanical values (no peer id). Connection/version/auth are *diagnostics* →
/// plain literal voice that names the peer.
///
/// **The seam (wired by the co-op layer at rung 3 — emit sites, not this model):**
/// - [`PeerJoined`](SessionEvent::PeerJoined): `coop/coop.rs`, on the first link/handshake-landed
///   edge (where it raises the lore presence toast alongside the plain "connected" confirmation).
/// - [`PeerLeft`](SessionEvent::PeerLeft): `coop/coop.rs` / `peer.rs::sweep_liveness`, on the
///   liveness *lost* edge — additive to the `ConnectionLost` banner, not a replacement.
/// - [`PeerReturned`](SessionEvent::PeerReturned): same site, on the liveness *recovery* edge — the
///   lore companion fired alongside [`ConnectionRestored`](SessionEvent::ConnectionRestored).
/// - [`VersionMismatch`](SessionEvent::VersionMismatch): `peer.rs::verify_auth`; `key` is the
///   per-peer version banner key, `detail` from `peer::version_mismatch_message`.
/// - [`ConnectionLost`](SessionEvent::ConnectionLost): `peer.rs::sweep_liveness` /
///   `coop/coop.rs`; `key` is the per-peer liveness banner key, `detail` from
///   `peer::lost_contact_message`.
/// - [`ConnectionRestored`](SessionEvent::ConnectionRestored): same site, clearing the liveness
///   banner; `key` is that same liveness banner key.
/// - [`AuthFailed`](SessionEvent::AuthFailed): `peer.rs::verify_auth`; `key` is the per-peer auth
///   banner key, `detail` from `peer::auth_failed_message`.
///
/// On session end the co-op layer calls [`Notifications::clear_session_banners`] once to tear down
/// every banner these events raised.
///
/// **Adoption is a reconciling refactor, not a drop-in.** The sibling [`peer`] module *already*
/// raises these diagnostic banners inline through the [`Notifications`] primitives (`verify_auth`
/// sets the version/auth banners and clears auth on success; `sweep_liveness` sets/clears the
/// liveness banner). The presence toasts (`PEER_*_MESSAGE`) are new — currently unused in
/// production. Two behaviors don't map one-to-one and must be reconciled when the co-op layer
/// switches those call sites over, so they don't drift into two patterns:
/// - **The liveness *stale* edge is compound:** `peer` clears the auth+version banners *and* (only
///   for a linked peer) sets the liveness banner. [`ConnectionLost`](SessionEvent::ConnectionLost)
///   models only the liveness banner; the caller still issues the companion clears (or fires the
///   relevant events) around it.
/// - **Recovery toast is new behavior:** [`ConnectionRestored`](SessionEvent::ConnectionRestored)
///   raises [`CONNECTION_RESTORED_MESSAGE`] in addition to clearing the banner, whereas today
///   `sweep_liveness` clears silently. Pair it with [`PeerReturned`](SessionEvent::PeerReturned)
///   for the lore companion, and drop the redundant plain toast if the banner-clear alone is wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// A co-op partner's handshake landed (first link this session). Lore-voiced presence toast.
    PeerJoined,
    /// A linked partner fell silent (liveness *lost* edge). Lore-voiced presence toast.
    PeerLeft,
    /// A partner flagged silent was heard from again (liveness *recovery* edge). Lore-voiced toast.
    PeerReturned,
    /// A peer's mod major-version is incompatible. Plain diagnostic banner. `detail` is the
    /// single-sourced wording (`peer::version_mismatch_message`); `key` is the per-peer banner id.
    VersionMismatch { key: String, detail: String },
    /// Lost contact with a peer (liveness). Plain diagnostic banner. `detail` from
    /// `peer::lost_contact_message`; `key` is the per-peer liveness banner id.
    ConnectionLost { key: String, detail: String },
    /// Contact re-established with a peer — clears the matching [`ConnectionLost`] banner (by `key`)
    /// and raises a brief plain confirmation toast ([`CONNECTION_RESTORED_MESSAGE`]).
    ///
    /// [`ConnectionLost`]: SessionEvent::ConnectionLost
    ConnectionRestored { key: String },
    /// A peer presented the wrong co-op password (auth proof failed). Plain diagnostic banner.
    /// `detail` from `peer::auth_failed_message`; `key` is the per-peer auth banner id.
    AuthFailed { key: String, detail: String },
}

impl SessionEvent {
    /// The severity this event renders at: presence is informational; lost-contact and version
    /// mismatch are warnings (degraded, recoverable); a failed auth is an error (the peer is
    /// rejected, never linked). Recovery confirmations are informational.
    pub fn severity(&self) -> Severity {
        match self {
            SessionEvent::PeerJoined
            | SessionEvent::PeerLeft
            | SessionEvent::PeerReturned
            | SessionEvent::ConnectionRestored { .. } => Severity::Info,
            SessionEvent::VersionMismatch { .. } | SessionEvent::ConnectionLost { .. } => {
                Severity::Warning
            }
            SessionEvent::AuthFailed { .. } => Severity::Error,
        }
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
    fn distinct_banner_ids_past_the_cap_evict_oldest_but_updates_do_not_grow() {
        let mut n = Notifications::new();
        for i in 0..(MAX_BANNERS + 3) {
            n.set_banner(format!("id{i}"), Severity::Info, format!("msg{i}"));
        }
        let ids: Vec<&str> = n.banners().iter().map(|b| b.id.as_str()).collect();
        // The three oldest distinct ids were evicted; survivors are id3..=id10 in insertion order.
        let expected: Vec<String> = (3..MAX_BANNERS + 3).map(|i| format!("id{i}")).collect();
        assert_eq!(ids, expected.iter().map(String::as_str).collect::<Vec<_>>());

        // Re-setting an existing id updates in place and must NOT grow the Vec past the cap.
        let before = n.banners().len();
        n.set_banner("id3", Severity::Error, "updated");
        assert_eq!(n.banners().len(), before, "updating an existing id never grows the banners");
        let updated = n.banners().iter().find(|b| b.id == "id3").expect("id3 still present");
        assert_eq!(updated.severity, Severity::Error);
        assert_eq!(updated.message, "updated");
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

    // --- SessionEvent model ---

    #[test]
    fn session_event_severity_matches_voice_and_register() {
        // Presence + recovery are informational; degraded-but-recoverable conditions warn; a
        // rejected peer (auth) is an error.
        assert_eq!(SessionEvent::PeerJoined.severity(), Severity::Info);
        assert_eq!(SessionEvent::PeerLeft.severity(), Severity::Info);
        assert_eq!(SessionEvent::PeerReturned.severity(), Severity::Info);
        assert_eq!(
            SessionEvent::ConnectionRestored { key: "k".into() }.severity(),
            Severity::Info
        );
        assert_eq!(
            SessionEvent::ConnectionLost { key: "k".into(), detail: "d".into() }.severity(),
            Severity::Warning
        );
        assert_eq!(
            SessionEvent::VersionMismatch { key: "k".into(), detail: "d".into() }.severity(),
            Severity::Warning
        );
        assert_eq!(
            SessionEvent::AuthFailed { key: "k".into(), detail: "d".into() }.severity(),
            Severity::Error
        );
    }

    #[test]
    fn presence_events_are_lore_voiced_transient_toasts() {
        let mut n = Notifications::new();
        n.session_event(SessionEvent::PeerJoined);
        n.session_event(SessionEvent::PeerLeft);
        n.session_event(SessionEvent::PeerReturned);
        // Presence is a moment, not a standing condition: toasts, no banners.
        assert!(n.banners().is_empty(), "presence must not leave a banner");
        let msgs: Vec<&str> = n.toasts().iter().map(|t| t.message.as_str()).collect();
        assert_eq!(
            msgs,
            [
                peer::PEER_ARRIVED_MESSAGE,
                peer::PEER_DEPARTED_MESSAGE,
                peer::PEER_RETURNED_MESSAGE
            ],
            "wording is single-sourced from peer.rs, in order"
        );
        // Lore voice carries no mechanical values (no peer id / digits). This guards the
        // single-sourced `peer::PEER_*_MESSAGE` wording the routing surfaces verbatim — a tripwire
        // if someone edits those constants to leak a value, not a property of the routing itself.
        for t in n.toasts() {
            assert!(t.severity == Severity::Info);
            assert!(!t.message.chars().any(|c| c.is_ascii_digit()), "lore voice shows no values: {:?}", t.message);
        }
        // They age out like any toast.
        n.tick(DEFAULT_TOAST_SECS);
        assert!(n.toasts().is_empty(), "presence toasts expire");
    }

    #[test]
    fn repeated_presence_event_refreshes_one_toast_not_stacks() {
        let mut n = Notifications::new();
        n.session_event(SessionEvent::PeerLeft);
        n.tick(DEFAULT_TOAST_SECS - 0.5); // remaining 0.5
        n.session_event(SessionEvent::PeerLeft); // same content -> refresh, not a 2nd toast
        assert_eq!(n.toasts().len(), 1, "a flapping presence edge must not stack toasts");
        assert_eq!(n.toasts()[0].remaining, DEFAULT_TOAST_SECS, "timer refreshed");
    }

    #[test]
    fn diagnostic_events_are_plain_keyed_session_banners_deduped_by_key() {
        let mut n = Notifications::new();
        let key = "liveness:42".to_string();
        n.session_event(SessionEvent::ConnectionLost {
            key: key.clone(),
            detail: "Lost contact with peer#42".into(),
        });
        assert_eq!(n.banners().len(), 1);
        assert_eq!(n.banners()[0].severity, Severity::Warning);
        assert_eq!(n.banners()[0].scope, BannerScope::Session);
        assert_eq!(n.banners()[0].message, "Lost contact with peer#42");

        // A re-fired condition under the same key updates the one banner in place, never stacks.
        n.session_event(SessionEvent::ConnectionLost {
            key: key.clone(),
            detail: "Lost contact with peer#42".into(),
        });
        assert_eq!(n.banners().len(), 1, "same key dedups to one banner");

        // Auth failure is a distinct keyed Error banner that coexists.
        n.session_event(SessionEvent::AuthFailed {
            key: "auth:42".into(),
            detail: "Authentication failed with peer#42 (wrong co-op password)".into(),
        });
        assert_eq!(n.banners().len(), 2);
        let auth = n.banners().iter().find(|b| b.id == "auth:42").expect("auth banner present");
        assert_eq!(auth.severity, Severity::Error);
    }

    #[test]
    fn connection_restored_clears_lost_banner_and_confirms() {
        let mut n = Notifications::new();
        let key = "liveness:7".to_string();
        n.session_event(SessionEvent::ConnectionLost { key: key.clone(), detail: "Lost contact with peer#7".into() });
        assert_eq!(n.banners().len(), 1);
        n.session_event(SessionEvent::ConnectionRestored { key: key.clone() });
        assert!(n.banners().is_empty(), "recovery clears the matching lost-contact banner");
        assert_eq!(
            n.toasts().iter().map(|t| t.message.as_str()).collect::<Vec<_>>(),
            [CONNECTION_RESTORED_MESSAGE],
            "and raises a plain confirmation toast"
        );
        // Restoring an unknown key is a harmless no-op clear plus the confirmation toast.
        n.session_event(SessionEvent::ConnectionRestored { key: "liveness:999".into() });
        assert!(n.banners().is_empty());
    }

    #[test]
    fn session_end_sweep_clears_session_banners_but_keeps_persistent() {
        let mut n = Notifications::new();
        // The session lifecycle banners every SessionEvent raises are session-scoped.
        n.session_event(SessionEvent::ConnectionLost { key: "liveness:1".into(), detail: "lost".into() });
        n.session_event(SessionEvent::VersionMismatch { key: "version:1".into(), detail: "mismatch".into() });
        n.session_event(SessionEvent::AuthFailed { key: "auth:1".into(), detail: "bad pw".into() });
        // A persistent, non-session banner (e.g. a global config-clamp condition) coexists.
        n.set_banner_scoped("config-clamp", Severity::Warning, "clamped a setting", BannerScope::Persistent);
        assert_eq!(n.banners().len(), 4);

        let cleared = n.clear_session_banners();
        assert_eq!(cleared, 3, "the three session banners are swept");
        let survivors: Vec<&str> = n.banners().iter().map(|b| b.id.as_str()).collect();
        assert_eq!(survivors, ["config-clamp"], "persistent banner survives session end");

        // A second sweep is a no-op once the session banners are gone.
        assert_eq!(n.clear_session_banners(), 0);
    }

    #[test]
    fn cap_eviction_spares_persistent_banners() {
        let mut n = Notifications::new();
        // A persistent banner set first would sit at index 0 and be the naive drop-oldest victim.
        n.set_banner_scoped("config-clamp", Severity::Warning, "clamped", BannerScope::Persistent);
        // Churn enough distinct session-scoped ids to push well past the cap.
        for i in 0..(MAX_BANNERS + 3) {
            n.set_banner(format!("liveness:{i}"), Severity::Warning, "lost");
        }
        assert_eq!(n.banners().len(), MAX_BANNERS, "still capped");
        assert!(
            n.banners().iter().any(|b| b.id == "config-clamp"),
            "the persistent banner must survive cap pressure (only session banners are evicted)"
        );
        assert_eq!(
            n.banners().iter().filter(|b| b.scope == BannerScope::Persistent).count(),
            1
        );
    }

    #[test]
    fn set_banner_defaults_to_session_scope() {
        let mut n = Notifications::new();
        n.set_banner("conn", Severity::Error, "down");
        assert_eq!(n.banners()[0].scope, BannerScope::Session, "the mod's banners are session-bound");
        // Re-classifying in place via the scoped setter overwrites the scope.
        n.set_banner_scoped("conn", Severity::Error, "down", BannerScope::Persistent);
        assert_eq!(n.banners()[0].scope, BannerScope::Persistent);
        assert_eq!(n.banners().len(), 1, "re-set by id updates in place");
    }
}
