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
#[derive(Default)]
struct Bus {
    inboxes: BTreeMap<PeerId, Vec<(PeerId, Vec<u8>)>>,
}

/// An in-memory [`Transport`] endpoint. Create a connected set with [`Loopback::mesh`].
#[derive(Clone)]
pub struct Loopback {
    id: PeerId,
    bus: Rc<RefCell<Bus>>,
}

impl Loopback {
    /// Create one connected endpoint per id, all sharing a bus. A `send` from any endpoint lands
    /// in every other endpoint's inbox.
    pub fn mesh(ids: &[PeerId]) -> Vec<Loopback> {
        let bus = Rc::new(RefCell::new(Bus::default()));
        {
            let mut b = bus.borrow_mut();
            for &id in ids {
                b.inboxes.insert(id, Vec::new());
            }
        }
        ids.iter().map(|&id| Loopback { id, bus: bus.clone() }).collect()
    }
}

impl Transport for Loopback {
    fn send(&mut self, bytes: &[u8]) {
        let mut bus = self.bus.borrow_mut();
        for (&pid, inbox) in bus.inboxes.iter_mut() {
            if pid != self.id {
                inbox.push((self.id, bytes.to_vec()));
            }
        }
    }

    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        let mut bus = self.bus.borrow_mut();
        bus.inboxes.get_mut(&self.id).map(std::mem::take).unwrap_or_default()
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
}
