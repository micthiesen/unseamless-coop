//! Debug side-channel **bridge** (dev only — behind the `bridge` cargo feature).
//!
//! Runs the real [`unseamless_core::peer::Session`] *inside the live mod* over a loopback TCP
//! socket, so the harness (`scripts/harness.sh bridge-host <port>`) can drive the side-channel
//! against the running game without a second game or Steam. This is layer 3 in the `/test-loop`
//! skill. The mod runs as the **client**: the harness is the authoritative host and pushes config,
//! which the mod applies into the live config ([`crate::state`]) — so the game-thread features see a
//! received `ConfigSync` and re-apply it (the apply path the bridge exists to test).
//!
//! It does **not** touch the game's own P2P (that's the rig-gated `GameTransport`, still ahead). The
//! `Session`/`Peer` logic is pure core types; the only cross-thread hand-off is the live config
//! (`crate::state`, a `Mutex`), written here and read by features. Loopback-only, off unless
//! `[debug] bridge_port > 0`, and compiled out of release builds entirely.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use unseamless_core::config::Config;
use unseamless_core::framing::{FrameDecoder, encode_frame};
use unseamless_core::peer::{Peer, Session};
use unseamless_core::protocol::PROTOCOL_VERSION;
use unseamless_core::transport::{PeerId, Transport};

/// In the bridge the **mod is a client**: the harness connects as the authoritative host (id 1) and
/// pushes config; the mod (id 2) applies it. That exercises the apply path — a received `ConfigSync`
/// changing live game state — which is what the bridge exists to test. Ids match the harness's
/// `HOST`/`CLIENT`, so its driver speaks to us unchanged.
const BRIDGE_HOST_ID: PeerId = 1;
const BRIDGE_CLIENT_ID: PeerId = 2;
/// Cadence for heartbeats / draining received frames while connected.
const TICK: Duration = Duration::from_millis(50);
/// Give up a single `write` after this many consecutive `WouldBlock`s (~10s at 200µs) so a peer
/// that stops reading can't pin this thread until process exit — drop it instead.
const MAX_WRITE_STALLS: u32 = 50_000;

/// Spawn the bridge listener thread. Non-blocking; logs and returns if it can't bind.
pub fn start(port: u16) {
    let _ = std::thread::Builder::new()
        .name("unseamless-bridge".into())
        .spawn(move || run(port));
}

fn run(port: u16) {
    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("bridge: could not bind {addr}: {e}; bridge disabled");
            return;
        }
    };
    log::info!("bridge listening on {addr} (dev side-channel; drive with `harness bridge-host {port}`)");
    // Serve one peer at a time, re-accepting so the bridge can be driven repeatedly.
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let peer_addr = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                log::info!("bridge: host connected ({peer_addr})");
                serve(stream);
                log::info!("bridge: host disconnected; awaiting next");
            }
            Err(e) => {
                log::warn!("bridge: accept failed: {e}; bridge stopping");
                break;
            }
        }
    }
}

/// Drive the mod as a client against the connected host until it disconnects, applying any config it
/// receives into the live config so the game-thread features pick it up.
fn serve(stream: TcpStream) {
    stream.set_nodelay(true).ok();
    if let Err(e) = stream.set_nonblocking(true) {
        log::warn!("bridge: set_nonblocking failed: {e}; dropping host");
        return;
    }
    let closed = Arc::new(AtomicBool::new(false));
    let transport = BridgeTransport {
        local_id: BRIDGE_CLIENT_ID,
        stream,
        decoder: FrameDecoder::new(),
        closed: closed.clone(),
    };
    // Seed from the current live config; the host's ConfigSync then overrides the shared subset.
    let mut session = Session::new(
        Peer::new(
            BRIDGE_CLIENT_ID,
            BRIDGE_HOST_ID,
            PROTOCOL_VERSION,
            crate::state::snapshot(),
            crate::config::fresh_auth_nonce(),
        ),
        transport,
    );
    session.connect();

    // Mirror the session's config into the live config only when it actually changes (a received
    // host `ConfigSync`), so the game-thread features pick it up. Comparing avoids a needless clone +
    // global write on every heartbeat tick (the host pings ~20×/s, but config rarely changes).
    let mut mirrored: Option<Config> = None;
    while !closed.load(Ordering::Relaxed) {
        session.maintain();
        session.pump();
        let cfg = session.peer().config();
        if mirrored.as_ref() != Some(cfg) {
            crate::state::set(cfg.clone());
            mirrored = Some(cfg.clone());
        }
        std::thread::sleep(TICK);
    }
}

/// Loopback-socket [`Transport`] for the bridge. Same wire framing as the harness `TcpTransport`
/// (the shared [`unseamless_core::framing`] codec); flips `closed` when the peer goes away so
/// [`serve`] can re-accept.
struct BridgeTransport {
    local_id: PeerId,
    stream: TcpStream,
    decoder: FrameDecoder,
    closed: Arc<AtomicBool>,
}

impl BridgeTransport {
    fn write_all_blocking(&mut self, buf: &[u8]) {
        let mut written = 0;
        let mut stalls = 0u32;
        while written < buf.len() {
            match self.stream.write(&buf[written..]) {
                Ok(0) => return self.closed.store(true, Ordering::Relaxed),
                Ok(n) => {
                    written += n;
                    stalls = 0;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    // A client that has stopped reading keeps the send buffer full forever; don't
                    // spin here indefinitely (it would wedge the single bridge thread). Drop it.
                    stalls += 1;
                    if stalls > MAX_WRITE_STALLS {
                        log::warn!("bridge: client not draining; dropping");
                        return self.closed.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return self.closed.store(true, Ordering::Relaxed),
            }
        }
    }
}

impl Transport for BridgeTransport {
    fn send(&mut self, bytes: &[u8]) {
        self.write_all_blocking(&encode_frame(self.local_id, bytes));
    }

    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        let mut tmp = [0u8; 8192];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => {
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
                Ok(n) => self.decoder.push(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => {
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
        match self.decoder.drain() {
            Ok(frames) => frames,
            Err(e) => {
                log::warn!("bridge: {e:?}; dropping client");
                self.closed.store(true, Ordering::Relaxed);
                Vec::new()
            }
        }
    }

    fn local_id(&self) -> PeerId {
        self.local_id
    }
}
