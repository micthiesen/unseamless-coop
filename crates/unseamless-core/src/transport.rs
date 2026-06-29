//! The side-channel transport seam.
//!
//! Co-op rides the game's own Steam P2P session (see ARCHITECTURE.md); our small `ModMessage`
//! side-channel rides *inside* one of its packet types. [`Transport`] is the byte-level interface
//! that the mod-coordination logic ([`crate::peer`]) sits on, so the same logic runs over two
//! backends:
//! - **production** (cdylib): wraps the game's `broadcast_packet` / `receive_packet`;
//! - **test** ([`Loopback`]): an in-memory bus that drives the host-side harness with no game.
//!
//! This is the seam that makes the side-channel testable on a laptop: the harness wires two
//! [`crate::peer::Peer`]s over a `Loopback` and exercises the whole handshake / config-sync /
//! action / log-forward flow without Steam or Elden Ring.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Identifies a peer. In production this is a Steam ID (`remote_identity`).
pub type PeerId = u64;

/// Injected delivery faults for the in-memory bus, so the harness can prove the side-channel
/// converges over a channel that **isn't** reliable or ordered — which the game's P2P broadcast may
/// well not be. All zero (the default) is a perfect channel. Rates are probabilities in `0.0..=1.0`,
/// applied independently per recipient (a broadcast can reach some peers and miss others).
#[derive(Debug, Clone, Copy, Default)]
pub struct FaultModel {
    /// Probability a frame is dropped before it reaches a given recipient.
    pub drop_rate: f64,
    /// Probability a delivered frame is also duplicated (delivered twice).
    pub duplicate_rate: f64,
    /// If set, each `poll` returns that peer's pending frames in a shuffled order.
    pub reorder: bool,
}

/// Tiny deterministic PRNG (xorshift64*), so fault injection is reproducible from a seed — a
/// failing soak run replays exactly. Not cryptographic; just a stable, dependency-free stream.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // xorshift64* state must be non-zero.
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// True with probability `p` (clamped to `0.0..=1.0`).
    fn chance(&mut self, p: f64) -> bool {
        if p <= 0.0 {
            return false;
        }
        if p >= 1.0 {
            return true;
        }
        // Map to [0,1): divide by 2^64 so 1.0 is unreachable, keeping p==1.0 the only sure path.
        (self.next_u64() as f64) / (2.0_f64.powi(64)) < p
    }
    /// Uniform in `0..n` (n > 0).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// A message-oriented broadcast channel between session peers.
pub trait Transport {
    /// Broadcast `bytes` (one encoded [`crate::protocol::ModMessage`]) to every other peer.
    fn send(&mut self, bytes: &[u8]);
    /// Drain frames received since the last poll, each tagged with its sender.
    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)>;
    /// This peer's own id.
    fn local_id(&self) -> PeerId;
}

/// Shared in-memory bus backing [`Loopback`]. Deliberately single-threaded (`Rc`/`RefCell`): the
/// harness and the game's task loop both drive peers on one thread.
struct Bus {
    inboxes: BTreeMap<PeerId, Vec<(PeerId, Vec<u8>)>>,
    faults: FaultModel,
    rng: Rng,
}

/// An in-memory [`Transport`] endpoint. Create a connected set with [`Loopback::mesh`].
#[derive(Clone)]
pub struct Loopback {
    id: PeerId,
    bus: Rc<RefCell<Bus>>,
}

impl Loopback {
    /// Create one connected endpoint per id, all sharing a **perfect** bus. A `send` from any
    /// endpoint lands in every other endpoint's inbox.
    pub fn mesh(ids: &[PeerId]) -> Vec<Loopback> {
        Self::mesh_with_faults(ids, FaultModel::default(), 0)
    }

    /// Like [`mesh`](Loopback::mesh) but with injected delivery [`FaultModel`] driven by a seeded
    /// PRNG, so a lossy/unordered channel is reproducible. Use this to prove the side-channel still
    /// converges under drops/duplicates/reordering.
    pub fn mesh_with_faults(ids: &[PeerId], faults: FaultModel, seed: u64) -> Vec<Loopback> {
        let mut inboxes = BTreeMap::new();
        for &id in ids {
            inboxes.insert(id, Vec::new());
        }
        let bus = Rc::new(RefCell::new(Bus { inboxes, faults, rng: Rng::new(seed) }));
        ids.iter().map(|&id| Loopback { id, bus: bus.clone() }).collect()
    }
}

impl Transport for Loopback {
    fn send(&mut self, bytes: &[u8]) {
        let bus = &mut *self.bus.borrow_mut();
        let recipients: Vec<PeerId> =
            bus.inboxes.keys().copied().filter(|&pid| pid != self.id).collect();
        for pid in recipients {
            // Drop and duplicate are decided per recipient: a broadcast can land for some peers and
            // be lost for others, like a real unreliable channel.
            if bus.rng.chance(bus.faults.drop_rate) {
                continue;
            }
            let copies = if bus.rng.chance(bus.faults.duplicate_rate) { 2 } else { 1 };
            let inbox = bus.inboxes.get_mut(&pid).expect("recipient id came from inboxes");
            for _ in 0..copies {
                inbox.push((self.id, bytes.to_vec()));
            }
        }
    }

    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        let bus = &mut *self.bus.borrow_mut();
        let mut items = bus.inboxes.get_mut(&self.id).map(std::mem::take).unwrap_or_default();
        if bus.faults.reorder && items.len() > 1 {
            // Fisher–Yates with the bus PRNG: deterministic, so a reorder bug reproduces.
            for i in (1..items.len()).rev() {
                let j = bus.rng.below(i + 1);
                items.swap(i, j);
            }
        }
        items
    }

    fn local_id(&self) -> PeerId {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_reaches_others_but_not_self() {
        let mut ends = Loopback::mesh(&[1, 2, 3]);
        ends[0].send(b"hi");
        assert!(ends[0].poll().is_empty(), "sender doesn't receive its own broadcast");
        assert_eq!(ends[1].poll(), vec![(1, b"hi".to_vec())]);
        assert_eq!(ends[2].poll(), vec![(1, b"hi".to_vec())]);
    }

    #[test]
    fn poll_drains_the_inbox() {
        let mut ends = Loopback::mesh(&[1, 2]);
        ends[0].send(b"a");
        ends[0].send(b"b");
        assert_eq!(ends[1].poll().len(), 2);
        assert!(ends[1].poll().is_empty(), "second poll is empty");
    }

    #[test]
    fn drop_rate_one_delivers_nothing() {
        let faults = FaultModel { drop_rate: 1.0, ..Default::default() };
        let mut ends = Loopback::mesh_with_faults(&[1, 2], faults, 42);
        for _ in 0..50 {
            ends[0].send(b"x");
        }
        assert!(ends[1].poll().is_empty(), "every frame dropped");
    }

    #[test]
    fn duplicate_rate_one_delivers_each_frame_twice() {
        let faults = FaultModel { duplicate_rate: 1.0, ..Default::default() };
        let mut ends = Loopback::mesh_with_faults(&[1, 2], faults, 7);
        ends[0].send(b"x");
        assert_eq!(ends[1].poll(), vec![(1, b"x".to_vec()), (1, b"x".to_vec())]);
    }

    #[test]
    fn partial_drop_loses_some_but_not_all() {
        // A middling drop rate over many sends loses some and keeps some — the regime the
        // self-healing design has to survive.
        let faults = FaultModel { drop_rate: 0.5, ..Default::default() };
        let mut ends = Loopback::mesh_with_faults(&[1, 2], faults, 0xC0FFEE);
        for _ in 0..1000 {
            ends[0].send(b"x");
        }
        let delivered = ends[1].poll().len();
        assert!(delivered > 0 && delivered < 1000, "got {delivered} of 1000 — expected partial loss");
    }

    #[test]
    fn reorder_permutes_without_losing_frames() {
        let faults = FaultModel { reorder: true, ..Default::default() };
        let mut ends = Loopback::mesh_with_faults(&[1, 2], faults, 99);
        for i in 0u8..16 {
            ends[0].send(&[i]);
        }
        let got: Vec<u8> = ends[1].poll().into_iter().map(|(_, b)| b[0]).collect();
        assert_eq!(got.len(), 16, "reorder must not drop frames");
        let mut sorted = got.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0u8..16).collect::<Vec<_>>(), "same multiset, just reordered");
        assert_ne!(got, (0u8..16).collect::<Vec<_>>(), "this seed actually permutes");
    }

    #[test]
    fn partial_drop_is_independent_per_recipient() {
        // A broadcast's drop roll is made per recipient, so two receivers see DIFFERENT subsets of the
        // same sends — the realism the side-channel's self-heal is built against (a frame can reach one
        // peer and miss another in the same broadcast).
        let faults = FaultModel { drop_rate: 0.5, ..Default::default() };
        let mut ends = Loopback::mesh_with_faults(&[1, 2, 3], faults, 0xD1FF);
        for i in 0u8..200 {
            ends[0].send(&[i]);
        }
        let got2: Vec<u8> = ends[1].poll().into_iter().map(|(_, b)| b[0]).collect();
        let got3: Vec<u8> = ends[2].poll().into_iter().map(|(_, b)| b[0]).collect();
        assert!(!got2.is_empty() && !got3.is_empty(), "both recipients received some frames");
        assert!(got2.len() < 200 && got3.len() < 200, "both recipients lost some frames");
        assert_ne!(got2, got3, "the two recipients saw different subsets (per-recipient drop rolls)");
    }

    #[test]
    fn drop_and_duplicate_compose_on_the_same_channel() {
        // Both faults at once: each delivered frame may also be duplicated while others vanish. The
        // received multiset is a subset of the sends, each surviving frame present once or twice.
        let faults = FaultModel { drop_rate: 0.3, duplicate_rate: 0.5, reorder: false };
        let mut ends = Loopback::mesh_with_faults(&[1, 2], faults, 0xC0DE);
        for i in 0u8..100 {
            ends[0].send(&[i]);
        }
        let got: Vec<u8> = ends[1].poll().into_iter().map(|(_, b)| b[0]).collect();
        let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
        for b in &got {
            *counts.entry(*b).or_default() += 1;
        }
        assert!(counts.values().all(|&c| c == 1 || c == 2), "each surviving frame appears once or twice");
        assert!(counts.values().any(|&c| c == 2), "some frames were duplicated");
        assert!(counts.len() < 100, "some frames were dropped entirely");
    }

    #[test]
    fn reorder_only_shuffles_within_a_poll_not_across_them() {
        // Reordering permutes the frames drained in a single poll; frames sent after an intervening
        // poll land in a later batch and can't jump ahead of the earlier batch.
        let faults = FaultModel { reorder: true, ..Default::default() };
        let mut ends = Loopback::mesh_with_faults(&[1, 2], faults, 0x5151);
        for i in 0u8..8 {
            ends[0].send(&[i]);
        }
        let first: Vec<u8> = ends[1].poll().into_iter().map(|(_, b)| b[0]).collect();
        for i in 8u8..16 {
            ends[0].send(&[i]);
        }
        let second: Vec<u8> = ends[1].poll().into_iter().map(|(_, b)| b[0]).collect();
        assert_eq!(first.len(), 8, "first batch fully drained");
        assert_eq!(second.len(), 8, "second batch fully drained");
        assert!(first.iter().all(|&b| b < 8), "the first batch holds only the first 8 sends");
        assert!(second.iter().all(|&b| (8..16).contains(&b)), "later sends stay in the later batch");
    }

    #[test]
    fn same_seed_replays_identically() {
        let faults = FaultModel { drop_rate: 0.5, duplicate_rate: 0.3, reorder: true };
        let run = || {
            let mut ends = Loopback::mesh_with_faults(&[1, 2], faults, 2024);
            for i in 0u8..40 {
                ends[0].send(&[i]);
            }
            ends[1].poll()
        };
        assert_eq!(run(), run(), "a seeded fault run is reproducible");
    }
}
