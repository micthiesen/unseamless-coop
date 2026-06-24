//! Mod-coordination logic for the side-channel: the part of "co-op" that is **ours** and is
//! transport-agnostic, so it's host-tested and driven by the harness with no game.
//!
//! [`Peer`] is pure: it consumes inbound [`ModMessage`]s and produces outbound ones; it never
//! touches a transport. [`Session`] glues a `Peer` to a [`Transport`] (the [`Loopback`] bus in
//! tests/harness, the game's `broadcast_packet` in production), so the same logic runs in both.
//!
//! What it covers (well-defined now): the version handshake, host→client config sync, session
//! action relay with **sender-role authorization**, and client→host debug-log forwarding. What it
//! does NOT cover: the game's own player/world sync (positions, HP) — that's the game's transport
//! and is reverse-engineering-gated (see RIG-RUNBOOK.md). Host identity comes from the game's
//! session manager out of band (here, passed to [`Peer::new`]); it is not carried in the wire
//! handshake.

use std::collections::BTreeMap;

use crate::config::Config;
use crate::diagnostics::{LogBundle, LogLevel, LogRecord, peer_tag};
use crate::notifications::{Notifications, Severity};
use crate::protocol::{ModMessage, SessionAction, SharedSettings};
use crate::transport::{PeerId, Transport};
use crate::util::Version;

pub struct Peer {
    id: PeerId,
    /// Who the host is (from the game's session manager). `is_host()` is `id == host_id`.
    host_id: PeerId,
    version: Version,
    config: Config,
    /// Versions advertised by other peers (from their `Hello`).
    peers: BTreeMap<PeerId, Version>,
    notifications: Notifications,
    /// Host-side aggregation of forwarded debug logs.
    log_bundle: LogBundle,
    /// Monotonic sequence for our own outbound log records.
    out_seq: u32,
    /// Last accepted session action (for harness/inspection).
    last_action: Option<(PeerId, SessionAction)>,
}

impl Peer {
    pub fn new(id: PeerId, host_id: PeerId, version: Version, config: Config) -> Self {
        Self {
            id,
            host_id,
            version,
            config,
            peers: BTreeMap::new(),
            notifications: Notifications::new(),
            log_bundle: LogBundle::new(),
            out_seq: 0,
            last_action: None,
        }
    }

    pub fn is_host(&self) -> bool {
        self.id == self.host_id
    }

    /// Messages to send on joining the session: announce our mod version.
    pub fn connect(&mut self) -> Vec<ModMessage> {
        vec![ModMessage::Hello { mod_version: self.version.to_u32() }]
    }

    /// Process one inbound message; return any outbound responses to broadcast.
    pub fn handle(&mut self, from: PeerId, msg: ModMessage) -> Vec<ModMessage> {
        match msg {
            ModMessage::Hello { mod_version } => {
                let theirs = Version::from_u32(mod_version);
                self.peers.insert(from, theirs);
                if !self.version.compatible_with(theirs) {
                    self.notifications.set_banner(
                        format!("version:{from}"),
                        Severity::Warning,
                        format!(
                            "Mod version mismatch with {}: they have {}, you have {}",
                            peer_tag(from),
                            fmt_version(theirs),
                            fmt_version(self.version),
                        ),
                    );
                }
                // The host brings a newcomer in sync with the current shared settings.
                if self.is_host() {
                    vec![ModMessage::ConfigSync(SharedSettings::from(&self.config))]
                } else {
                    vec![]
                }
            }
            ModMessage::ConfigSync(shared) => {
                if from == self.host_id {
                    shared.apply_to(&mut self.config);
                    self.notifications.info("Session settings synced from host");
                } else {
                    self.notifications
                        .warn(format!("Ignored ConfigSync from non-host {}", peer_tag(from)));
                }
                vec![]
            }
            ModMessage::SessionAction(action) => {
                // Authorize host-only actions by the SENDER's role (not the local UI).
                if action.is_host_only() && from != self.host_id {
                    self.notifications.warn(format!(
                        "Ignored host-only action {action:?} from non-host {}",
                        peer_tag(from)
                    ));
                } else {
                    self.last_action = Some((from, action));
                }
                vec![]
            }
            ModMessage::Ping { .. } => vec![],
            ModMessage::Log(record) => {
                // Only the host aggregates forwarded logs.
                if self.is_host() {
                    self.log_bundle.add(peer_tag(from), record);
                }
                vec![]
            }
        }
    }

    /// Host: broadcast the current shared settings (e.g. after the host changes a setting).
    pub fn broadcast_config(&self) -> Vec<ModMessage> {
        if self.is_host() {
            vec![ModMessage::ConfigSync(SharedSettings::from(&self.config))]
        } else {
            vec![]
        }
    }

    /// Client: forward a local log line to the host, if `[debug] forward_to_host` is on. No-op on
    /// the host or when forwarding is disabled.
    pub fn forward_log(&mut self, level: LogLevel, message: impl Into<String>) -> Vec<ModMessage> {
        if self.is_host() || !self.config.debug.forward_to_host {
            return vec![];
        }
        self.out_seq += 1;
        vec![ModMessage::Log(LogRecord { seq: self.out_seq, level, message: message.into() })]
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }
    pub fn notifications(&self) -> &Notifications {
        &self.notifications
    }
    pub fn log_bundle(&self) -> &LogBundle {
        &self.log_bundle
    }
    pub fn known_peers(&self) -> &BTreeMap<PeerId, Version> {
        &self.peers
    }
    pub fn last_action(&self) -> Option<(PeerId, SessionAction)> {
        self.last_action
    }
}

fn fmt_version(v: Version) -> String {
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}

/// Binds a [`Peer`] to a [`Transport`]: encodes the peer's outbound messages onto the wire and
/// feeds decoded inbound frames back into it. The harness uses `Session<Loopback>`; the cdylib
/// will use `Session<GameTransport>` over `broadcast_packet`.
pub struct Session<T: Transport> {
    peer: Peer,
    transport: T,
}

impl<T: Transport> Session<T> {
    pub fn new(peer: Peer, transport: T) -> Self {
        Self { peer, transport }
    }

    /// Announce ourselves to the session (sends `Hello`).
    pub fn connect(&mut self) {
        let out = self.peer.connect();
        self.broadcast(out);
    }

    /// One network step: deliver every inbound frame to the peer and broadcast its responses.
    /// Returns the number of frames processed (0 = quiescent), so a driver can loop to convergence.
    pub fn pump(&mut self) -> usize {
        let inbound = self.transport.poll();
        let count = inbound.len();
        for (from, bytes) in inbound {
            // Malformed/foreign frames are dropped — the decoder already rejects hostile input.
            if let Ok(msg) = ModMessage::decode(&bytes) {
                let out = self.peer.handle(from, msg);
                self.broadcast(out);
            }
        }
        count
    }

    /// Encode and broadcast a batch of messages (e.g. from `peer.broadcast_config()`).
    pub fn broadcast(&mut self, messages: Vec<ModMessage>) {
        for m in messages {
            self.transport.send(&m.encode());
        }
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }
    pub fn peer_mut(&mut self) -> &mut Peer {
        &mut self.peer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Loopback;

    const HOST: PeerId = 1;
    const CLIENT: PeerId = 2;

    /// Build a host+client pair over a shared loopback, each at the given version.
    fn pair(host_v: Version, client_v: Version) -> (Session<Loopback>, Session<Loopback>) {
        let ends = Loopback::mesh(&[HOST, CLIENT]);
        let mut it = ends.into_iter();
        let host = Session::new(Peer::new(HOST, HOST, host_v, Config::default()), it.next().unwrap());
        let client =
            Session::new(Peer::new(CLIENT, HOST, client_v, Config::default()), it.next().unwrap());
        (host, client)
    }

    /// Drive both sessions to convergence (no frames left in flight).
    fn run(sessions: &mut [&mut Session<Loopback>]) {
        for _ in 0..100 {
            let mut activity = 0;
            for s in sessions.iter_mut() {
                activity += s.pump();
            }
            if activity == 0 {
                return;
            }
        }
        panic!("did not converge");
    }

    #[test]
    fn handshake_exchanges_versions() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);
        assert_eq!(host.peer().known_peers().get(&CLIENT), Some(&v));
        assert_eq!(client.peer().known_peers().get(&HOST), Some(&v));
        // Compatible versions => no version banner.
        assert!(client.peer().notifications().banners().is_empty());
    }

    #[test]
    fn incompatible_major_raises_a_banner() {
        let (mut host, mut client) = pair(Version::new(1, 0, 0), Version::new(2, 0, 0));
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);
        let banners = client.peer().notifications().banners();
        assert_eq!(banners.len(), 1);
        assert!(banners[0].message.contains("version mismatch"));
    }

    #[test]
    fn host_config_change_converges_to_the_client() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        host.peer_mut().config_mut().scaling.boss_health = 250;
        host.peer_mut().config_mut().gameplay.allow_invaders = false;

        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]); // handshake triggers the host's ConfigSync

        assert_eq!(client.peer().config().scaling.boss_health, 250);
        assert!(!client.peer().config().gameplay.allow_invaders);
        assert!(
            client.peer().notifications().toasts().iter().any(|t| t.message.contains("synced")),
            "client should be notified of the sync"
        );
    }

    #[test]
    fn host_only_action_from_non_host_is_rejected() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);

        // Client (non-host) tries to lock the world: rejected on the host.
        host.peer_mut().last_action(); // (none yet)
        client.broadcast(vec![ModMessage::SessionAction(SessionAction::LockWorld)]);
        run(&mut [&mut host, &mut client]);
        assert_eq!(host.peer().last_action(), None, "host-only action from a client is dropped");
        assert!(host.peer().notifications().toasts().iter().any(|t| t.message.contains("host-only")));

        // A non-host-only action (GiveEmber) from the client is accepted.
        client.broadcast(vec![ModMessage::SessionAction(SessionAction::GiveEmber)]);
        run(&mut [&mut host, &mut client]);
        assert_eq!(host.peer().last_action(), Some((CLIENT, SessionAction::GiveEmber)));
    }

    #[test]
    fn client_forwards_logs_into_the_host_bundle() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        client.peer_mut().config_mut().debug.forward_to_host = true;
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);

        let out = client.peer_mut().forward_log(LogLevel::Warn, "something looked off");
        client.broadcast(out);
        run(&mut [&mut host, &mut client]);

        let rendered = host.peer().log_bundle().render();
        assert!(rendered.contains("something looked off"));
        assert!(rendered.contains(&peer_tag(CLIENT)), "attributed to the client's pseudonym");
    }
}
