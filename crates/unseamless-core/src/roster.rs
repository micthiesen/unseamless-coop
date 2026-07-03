//! The in-world session **roster** model: which peer identities are present in the game session,
//! and which in-world **phantom** each identity currently embodies.
//!
//! Pure + host-tested, built ahead of the rung-3 game-side wiring (see COOP-CONNECTION.md > "The
//! pre-built session core"). Two concerns live here because they share one consistency rule (a
//! departed peer's phantom bindings must die with it):
//!
//! - **Presence diffing** ([`Roster::observe`]): the binding layer samples the game's session
//!   roster (`CSSessionManager.players`' SteamIDs) each frame and hands the snapshot over; the
//!   model diffs it against the last one and reports the join/leave **edges**. The order of the
//!   snapshot is deliberately ignored — the game's roster reorders as players come and go, so
//!   identity is the key, never the index.
//! - **Phantom → identity mapping** ([`Roster::bind_phantom`]): maps an in-world phantom (keyed by
//!   an opaque stable handle — in production the phantom's `ChrIns` pointer) to the peer identity
//!   (SteamID) embodying it. This is what lets the nameplate dots color by **SteamID** instead of
//!   by transient pointer (NAMEPLATES.md > "color-by-SteamID"), and what a future overhead display
//!   keys per-player info on. *How* the binding correlates a phantom to a SteamID is the rung-3 RE;
//!   the model only keeps the answer queryable and consistent.
//!
//! [`crate::peer::Peer`] owns a `Roster` and ties the leave edge to side-channel eviction +
//! notification ([`crate::peer::Peer::observe_roster`]); this module stays mechanism-only so it can
//! be tested (and reused) in isolation.

use std::collections::{BTreeMap, BTreeSet};

use crate::transport::PeerId;

/// Consecutive [`Roster::observe`] snapshots a present peer must be absent from before its
/// [`RosterEvent::Left`] edge fires. Presence is definite (an id in the snapshot really is there,
/// so a join reports immediately), but absence is noisy — the game's roster vector can plausibly
/// read empty/partial for a few frames across a load or fast-travel transition — and a `Left`
/// edge is expensive downstream (a full side-channel eviction plus a departure toast), so absence
/// is debounced: a peer that reappears mid-streak was never gone. Denominated in `observe` calls;
/// the binding samples once per frame, so 30 ≈ half a second at 60 fps. Like
/// `peer::LIVENESS_TIMEOUT_TICKS` this is a conservative tuning value — revisit once the rig shows
/// how the live roster actually behaves across transitions.
pub const LEAVE_CONFIRM_SNAPSHOTS: u32 = 30;

/// Opaque handle identifying one loaded in-world phantom — in production the phantom's `ChrIns`
/// pointer, which is stable per loaded phantom across frames (NAMEPLATES.md). A newtype (not a bare
/// `u64`) so a handle can't be silently swapped with a [`PeerId`] at a call site — both are 64-bit,
/// and a swap would corrupt the mapping without a type error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhantomHandle(pub u64);

/// A presence edge from one [`Roster::observe`] diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterEvent {
    /// The peer appeared in the session roster.
    Joined(PeerId),
    /// The peer left the session roster (its phantom bindings were dropped with it).
    Left(PeerId),
}

/// The session roster: present peer identities + their phantom bindings. See the module docs.
#[derive(Debug, Default)]
pub struct Roster {
    /// Identities currently considered present. A peer stays here through an absence shorter than
    /// the [`LEAVE_CONFIRM_SNAPSHOTS`] debounce (it hasn't *left* until the absence is confirmed).
    present: BTreeSet<PeerId>,
    /// Consecutive-snapshot absence count per still-present peer (the leave debounce). Cleared on
    /// reappearance and on removal.
    absent_streaks: BTreeMap<PeerId, u32>,
    /// Phantom handle → the identity embodying it. Multiple handles may map to one peer
    /// mid-transition ([`phantom_of`](Roster::phantom_of) then returns the lowest handle); each
    /// handle maps to exactly one peer, and rebinding a reused handle overwrites.
    phantoms: BTreeMap<PhantomHandle, PeerId>,
}

impl Roster {
    pub fn new() -> Self {
        Self::default()
    }

    /// Diff a roster snapshot against the retained presence state and return the join/leave edges.
    /// The snapshot is treated as a **set** (order and duplicates are ignored); it must already
    /// exclude the local player — [`crate::peer::Peer::observe_roster`] does that filtering, since
    /// only it knows the local id. Joins report immediately; a leave fires only once the peer has
    /// been absent [`LEAVE_CONFIRM_SNAPSHOTS`] snapshots in a row (see the constant — a transient
    /// partial read must not mass-evict the party). A departing peer's phantom bindings are
    /// dropped with it, so a stale handle can never resolve to an identity that's no longer in the
    /// session.
    ///
    /// `pub(crate)`, not `pub`: presence moves only through [`crate::peer::Peer::observe_roster`]
    /// (which ties the leave edge to side-channel eviction + the departure toast). A binding-layer
    /// caller reaching this directly via `game_roster_mut()` would silently skip that tie — the
    /// mutable surface a binding legitimately needs is only the phantom bindings.
    pub(crate) fn observe(&mut self, snapshot: &[PeerId]) -> Vec<RosterEvent> {
        let now: BTreeSet<PeerId> = snapshot.iter().copied().collect();
        // Joins are definite: every snapshot id not yet present arrives now.
        let mut events: Vec<RosterEvent> =
            now.difference(&self.present).map(|&p| RosterEvent::Joined(p)).collect();

        // Absence is debounced: count the streak per missing peer; confirm the leave only at the
        // threshold, and forget the streak the moment the peer reappears.
        let mut left = Vec::new();
        for &peer in &self.present {
            if now.contains(&peer) {
                self.absent_streaks.remove(&peer);
            } else {
                let streak = self.absent_streaks.entry(peer).or_insert(0);
                *streak += 1;
                if *streak >= LEAVE_CONFIRM_SNAPSHOTS {
                    left.push(peer);
                }
            }
        }
        for &peer in &left {
            self.remove_peer(peer);
            events.push(RosterEvent::Left(peer));
        }
        self.present.extend(now);
        events
    }

    /// Whether `peer` is present in the session roster (as of the last snapshot).
    pub fn contains(&self, peer: PeerId) -> bool {
        self.present.contains(&peer)
    }

    /// Present peer identities, in stable (id) order.
    pub fn peers(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.present.iter().copied()
    }

    /// Number of present peers (excluding the local player, which is never in the roster).
    pub fn len(&self) -> usize {
        self.present.len()
    }

    pub fn is_empty(&self) -> bool {
        self.present.is_empty()
    }

    /// Bind an in-world phantom handle to the peer identity embodying it. Only a **present** peer
    /// can be bound (returns `false` otherwise — observe the roster before binding), so the map
    /// can't grow entries for identities the session doesn't contain. Rebinding an existing handle
    /// to a new peer overwrites (handles are pointers; the allocator can reuse one after a
    /// despawn).
    pub fn bind_phantom(&mut self, handle: PhantomHandle, peer: PeerId) -> bool {
        if !self.present.contains(&peer) {
            return false;
        }
        self.phantoms.insert(handle, peer);
        true
    }

    /// Drop one phantom binding (the phantom despawned). Returns whether it existed.
    pub fn unbind_phantom(&mut self, handle: PhantomHandle) -> bool {
        self.phantoms.remove(&handle).is_some()
    }

    /// The identity embodying `handle`, if bound — the color-by-SteamID lookup.
    pub fn phantom_identity(&self, handle: PhantomHandle) -> Option<PeerId> {
        self.phantoms.get(&handle).copied()
    }

    /// The phantom handle currently bound to `peer` (reverse lookup, e.g. for an overhead display
    /// walking the roster). First by handle order if a transition briefly leaves two.
    pub fn phantom_of(&self, peer: PeerId) -> Option<PhantomHandle> {
        self.phantoms.iter().find(|&(_, &p)| p == peer).map(|(&h, _)| h)
    }

    /// Retain only the bindings whose handle is in `live` — the per-frame sweep against the set of
    /// currently-loaded phantoms, so a despawned phantom's (reusable) pointer can't linger bound to
    /// the old identity. Returns how many stale bindings were dropped.
    pub fn retain_phantoms(&mut self, live: &[PhantomHandle]) -> usize {
        let live: BTreeSet<PhantomHandle> = live.iter().copied().collect();
        let before = self.phantoms.len();
        self.phantoms.retain(|h, _| live.contains(h));
        before - self.phantoms.len()
    }

    /// Forget one peer entirely: presence, absence streak, and phantom bindings. `pub(crate)` like
    /// [`observe`](Roster::observe): the only legitimate caller is [`crate::peer::Peer::evict`], so
    /// a peer evicted for side-channel reasons doesn't linger in the roster model. Returns whether
    /// the peer was known (present or bound).
    pub(crate) fn remove_peer(&mut self, peer: PeerId) -> bool {
        let was_present = self.present.remove(&peer);
        self.absent_streaks.remove(&peer);
        let had_bindings = {
            let before = self.phantoms.len();
            self.phantoms.retain(|_, p| *p != peer);
            before != self.phantoms.len()
        };
        was_present || had_bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PeerId = 11;
    const B: PeerId = 22;
    const C: PeerId = 33;
    const H1: PhantomHandle = PhantomHandle(0x1000);
    const H2: PhantomHandle = PhantomHandle(0x2000);

    /// Drive `observe(snapshot)` through the absence debounce: one leading call (whose events —
    /// immediate joins — are returned merged), then quiet interim calls, then the confirming call
    /// (whose leave edges are merged in). Panics if an interim call emits anything.
    fn observe_through_debounce(r: &mut Roster, snapshot: &[PeerId]) -> Vec<RosterEvent> {
        let mut events = r.observe(snapshot);
        for _ in 1..(LEAVE_CONFIRM_SNAPSHOTS - 1) {
            assert_eq!(r.observe(snapshot), vec![], "absence below the debounce must be quiet");
        }
        events.extend(r.observe(snapshot));
        events
    }

    #[test]
    fn observe_reports_join_and_leave_edges_not_steady_state() {
        let mut r = Roster::new();
        assert_eq!(r.observe(&[A, B]), vec![RosterEvent::Joined(A), RosterEvent::Joined(B)]);
        // Unchanged snapshot: no edges (the binding calls this every frame).
        assert_eq!(r.observe(&[A, B]), vec![]);
        // B leaves, C joins: the join reports on the first snapshot, the leave only once B's
        // absence outlasts the debounce.
        assert_eq!(
            observe_through_debounce(&mut r, &[A, C]),
            vec![RosterEvent::Joined(C), RosterEvent::Left(B)]
        );
        assert!(r.contains(A) && r.contains(C) && !r.contains(B));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn a_transient_absence_below_the_debounce_never_leaves() {
        // The load/fast-travel shape: the roster reads partial for a few frames, then recovers.
        // No Left edge may fire, presence and phantom bindings must survive, and the streak must
        // fully reset on reappearance (a second partial run starts counting from zero).
        let mut r = Roster::new();
        r.observe(&[A, B]);
        r.bind_phantom(H1, A);
        for _ in 0..(LEAVE_CONFIRM_SNAPSHOTS - 1) {
            assert_eq!(r.observe(&[B]), vec![], "sub-debounce absence is quiet");
        }
        assert_eq!(r.observe(&[A, B]), vec![], "reappearing mid-streak is not a join (never left)");
        assert!(r.contains(A));
        assert_eq!(r.phantom_identity(H1), Some(A), "bindings survive the blip");
        // The streak reset: another sub-debounce run stays quiet rather than resuming the count.
        for _ in 0..(LEAVE_CONFIRM_SNAPSHOTS - 1) {
            assert_eq!(r.observe(&[B]), vec![], "a fresh absence restarts the count");
        }
        assert!(r.contains(A));
    }

    #[test]
    fn observe_ignores_snapshot_order_and_duplicates() {
        // The game's roster reorders as players come and go; identity is the key, never the index.
        let mut r = Roster::new();
        r.observe(&[A, B]);
        assert_eq!(r.observe(&[B, A]), vec![], "a reorder is not an edge");
        assert_eq!(r.observe(&[B, B, A]), vec![], "a duplicated entry is not an edge");
    }

    #[test]
    fn empty_snapshots_before_a_session_are_quiet() {
        let mut r = Roster::new();
        assert_eq!(r.observe(&[]), vec![]);
        assert_eq!(r.observe(&[]), vec![]);
        assert!(r.is_empty());
    }

    #[test]
    fn phantom_binding_maps_handle_to_identity() {
        let mut r = Roster::new();
        r.observe(&[A, B]);
        assert!(r.bind_phantom(H1, A));
        assert!(r.bind_phantom(H2, B));
        assert_eq!(r.phantom_identity(H1), Some(A));
        assert_eq!(r.phantom_identity(H2), Some(B));
        assert_eq!(r.phantom_of(B), Some(H2));
        // The mapping is keyed by handle, so a roster reorder can't recolor anyone.
        r.observe(&[B, A]);
        assert_eq!(r.phantom_identity(H1), Some(A));
    }

    #[test]
    fn binding_an_absent_peer_is_refused() {
        let mut r = Roster::new();
        r.observe(&[A]);
        assert!(!r.bind_phantom(H1, B), "B is not in the roster");
        assert_eq!(r.phantom_identity(H1), None);
    }

    #[test]
    fn a_leaving_peer_takes_its_phantom_bindings_with_it() {
        let mut r = Roster::new();
        r.observe(&[A, B]);
        r.bind_phantom(H1, A);
        r.bind_phantom(H2, B);
        assert_eq!(observe_through_debounce(&mut r, &[B]), vec![RosterEvent::Left(A)]);
        assert_eq!(r.phantom_identity(H1), None, "the departed peer's binding is gone");
        assert_eq!(r.phantom_identity(H2), Some(B), "the remaining peer's binding survives");
        assert_eq!(r.phantom_of(A), None);
    }

    #[test]
    fn two_handles_on_one_peer_resolve_and_tie_break_by_lowest() {
        // Mid-transition a peer can briefly own two handles: both must resolve to it, and
        // phantom_of returns the lowest handle (the documented tie-break).
        let mut r = Roster::new();
        r.observe(&[A]);
        r.bind_phantom(H2, A);
        r.bind_phantom(H1, A);
        assert_eq!(r.phantom_identity(H1), Some(A));
        assert_eq!(r.phantom_identity(H2), Some(A));
        assert_eq!(r.phantom_of(A), Some(H1), "lowest handle wins the reverse lookup");
    }

    #[test]
    fn rebinding_a_reused_handle_overwrites() {
        // A ChrIns pointer can be reused after a despawn: rebinding must overwrite, not stack.
        let mut r = Roster::new();
        r.observe(&[A, B]);
        r.bind_phantom(H1, A);
        assert!(r.bind_phantom(H1, B));
        assert_eq!(r.phantom_identity(H1), Some(B));
        assert_eq!(r.phantom_of(A), None, "the old identity no longer claims the handle");
    }

    #[test]
    fn unbind_and_retain_drop_stale_bindings() {
        let mut r = Roster::new();
        r.observe(&[A, B]);
        r.bind_phantom(H1, A);
        r.bind_phantom(H2, B);
        assert!(r.unbind_phantom(H1));
        assert!(!r.unbind_phantom(H1), "already unbound");
        r.bind_phantom(H1, A);
        // Per-frame sweep: only H2 is still loaded; H1's binding is stale and dropped.
        assert_eq!(r.retain_phantoms(&[H2]), 1);
        assert_eq!(r.phantom_identity(H1), None);
        assert_eq!(r.phantom_identity(H2), Some(B));
        assert!(r.contains(A), "retain sweeps bindings, never presence");
        // Boundary returns: retaining everything drops 0; retaining nothing drops the rest.
        assert_eq!(r.retain_phantoms(&[H2]), 0, "a superset sweep is a counted no-op");
        assert_eq!(r.retain_phantoms(&[]), 1, "an empty live set drops every binding");
        assert_eq!(r.phantom_identity(H2), None);
    }

    #[test]
    fn remove_peer_forgets_presence_and_bindings() {
        let mut r = Roster::new();
        r.observe(&[A, B]);
        r.bind_phantom(H1, A);
        assert!(r.remove_peer(A));
        assert!(!r.contains(A));
        assert_eq!(r.phantom_identity(H1), None);
        assert!(!r.remove_peer(A), "already forgotten");
        // Removing is not a diff: the next snapshot containing A reports a fresh Joined edge.
        assert_eq!(r.observe(&[A, B]), vec![RosterEvent::Joined(A)]);
    }
}
