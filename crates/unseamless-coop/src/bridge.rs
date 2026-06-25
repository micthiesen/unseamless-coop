//! Debug side-channel **bridge** (dev only — behind the `bridge` cargo feature).
//!
//! Runs the real [`unseamless_core::peer::Session`] *inside the live mod* as host, over a loopback
//! TCP socket, so the harness (`scripts/harness.sh bridge-probe <port>`) can drive the whole
//! side-channel — handshake, config-sync, actions, log-forward — against the running game without a
//! second game or Steam. This is layer 3 in the `/test-loop` skill.
//!
//! It does **not** touch the game's own P2P (that's the rig-gated `GameTransport`, still ahead) or
//! any game memory: the `Session`/`Peer` logic is pure core types over its own config clone, so it
//! runs on this background thread with no game-thread requirement. Loopback-only, off unless
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

/// The mod is the host peer on the bridge; the harness connects as the client (id 2). This matches
/// the harness's `HOST`, so its existing client speaks to us unchanged.
const BRIDGE_HOST_ID: PeerId = 1;
/// Cadence for re-asserting config / heartbeats while a client is connected.
const TICK: Duration = Duration::from_millis(50);
/// Give up a single `write` after this many consecutive `WouldBlock`s (~10s at 200µs) so a client
/// that stops reading can't pin this thread until process exit — drop it instead.
const MAX_WRITE_STALLS: u32 = 50_000;

/// Spawn the bridge listener thread. Non-blocking; logs and returns if it can't bind.
pub fn start(config: Config, port: u16) {
    let _ = std::thread::Builder::new()
        .name("unseamless-bridge".into())
        .spawn(move || run(config, port));
}

fn run(config: Config, port: u16) {
    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("bridge: could not bind {addr}: {e}; bridge disabled");
            return;
        }
    };
    log::info!("bridge listening on {addr} (dev side-channel; drive with `harness bridge-probe {port}`)");
    // Serve one client at a time, re-accepting so the bridge can be probed repeatedly.
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let client_addr = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                log::info!("bridge: client connected ({client_addr})");
                serve(&config, stream);
                log::info!("bridge: client disconnected; awaiting next");
            }
            Err(e) => {
                log::warn!("bridge: accept failed: {e}; bridge stopping");
                break;
            }
        }
    }
}

/// Drive a `Session` host against one connected client until it disconnects.
fn serve(config: &Config, stream: TcpStream) {
    stream.set_nodelay(true).ok();
    if let Err(e) = stream.set_nonblocking(true) {
        log::warn!("bridge: set_nonblocking failed: {e}; dropping client");
        return;
    }
    let closed = Arc::new(AtomicBool::new(false));
    let transport = BridgeTransport {
        local_id: BRIDGE_HOST_ID,
        stream,
        decoder: FrameDecoder::new(),
        closed: closed.clone(),
    };
    let mut host = Session::new(
        Peer::new(BRIDGE_HOST_ID, BRIDGE_HOST_ID, PROTOCOL_VERSION, config.clone()),
        transport,
    );
    host.connect();
    // Eager config push so a client that connects gets our settings immediately, without waiting to
    // round-trip its own `Hello` first (the handshake would also sync it; this just front-runs).
    let initial_sync = host.peer_mut().mark_config_changed();
    host.broadcast(initial_sync);

    while !closed.load(Ordering::Relaxed) {
        host.maintain();
        host.pump();
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
