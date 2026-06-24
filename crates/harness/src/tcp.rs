//! A real-socket [`Transport`] for the harness: two **separate OS processes** coordinate over a
//! localhost TCP connection, running the exact same `Peer`/`Session` logic as the in-memory loop.
//!
//! Why bother when `Loopback` already exercises the logic? Because this is a higher-fidelity rung:
//! real serialization, a real socket, real cross-process concurrency, and partial reads/writes —
//! the kinds of things an in-memory `Vec` bus can't surface. It's also the host half of the
//! planned layer-3 "debug bridge" (see the `/test-loop` skill): swap this `TcpTransport` for one
//! that speaks to a debug listener inside the live mod and the same scenarios drive a real game.
//!
//! Wire framing (its own, distinct from the `ModMessage` frame it carries): `[u32 len][u64
//! sender][payload]`, big-endian. `len` covers only `payload` (one encoded `ModMessage`); `sender`
//! conveys the peer id that the in-memory bus otherwise tracks out of band.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};

use unseamless_core::transport::{PeerId, Transport};

const HEADER_LEN: usize = 4 + 8; // u32 len + u64 sender
/// Reject a frame claiming more than this payload — a desynced or hostile peer otherwise grows
/// `inbuf` without bound while we wait for a frame that never completes. Side-channel `ModMessage`s
/// are tiny (a forwarded log caps at ~2 KiB), so 64 KiB is generous. This matters most when this
/// framing graduates to the layer-3 debug bridge, whose peer is the live mod.
const MAX_FRAME: usize = 64 * 1024;

pub struct TcpTransport {
    local_id: PeerId,
    stream: TcpStream,
    /// Bytes received but not yet parsed into whole frames.
    inbuf: Vec<u8>,
}

impl TcpTransport {
    /// Connect to a listening peer (the client side). Retries briefly so the connector can start
    /// before or alongside the listener without a race.
    pub fn connect(addr: &str, local_id: PeerId) -> std::io::Result<Self> {
        let mut last_err = None;
        for _ in 0..100 {
            match TcpStream::connect(addr) {
                Ok(stream) => {
                    stream.set_nodelay(true).ok();
                    stream.set_nonblocking(true)?;
                    return Ok(Self { local_id, stream, inbuf: Vec::new() });
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| std::io::Error::other("connect failed")))
    }

    /// Accept one inbound connection on `listener` (the host side).
    pub fn accept(listener: &TcpListener, local_id: PeerId) -> std::io::Result<Self> {
        let (stream, _peer) = listener.accept()?;
        stream.set_nodelay(true).ok();
        stream.set_nonblocking(true)?;
        Ok(Self { local_id, stream, inbuf: Vec::new() })
    }

    /// Write the whole buffer, handling partial writes / `WouldBlock` so framing can't corrupt.
    fn write_all_blocking(&mut self, buf: &[u8]) {
        let mut written = 0;
        while written < buf.len() {
            match self.stream.write(&buf[written..]) {
                Ok(0) => return, // peer closed
                Ok(n) => written += n,
                // Back off a touch instead of busy-spinning if the send buffer is full.
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_micros(200))
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

impl Transport for TcpTransport {
    fn send(&mut self, bytes: &[u8]) {
        let mut frame = Vec::with_capacity(HEADER_LEN + bytes.len());
        frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        frame.extend_from_slice(&self.local_id.to_be_bytes());
        frame.extend_from_slice(bytes);
        self.write_all_blocking(&frame);
    }

    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        // Drain whatever the socket has buffered.
        let mut tmp = [0u8; 8192];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => break, // closed
                Ok(n) => self.inbuf.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }

        // Parse as many complete `[len][sender][payload]` frames as are present.
        let mut out = Vec::new();
        let mut pos = 0;
        while self.inbuf.len() - pos >= HEADER_LEN {
            let len = u32::from_be_bytes(self.inbuf[pos..pos + 4].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                // Framing desync or a hostile peer — don't buffer unboundedly. Drop everything
                // buffered and resync from the next read; over a trusted localhost link this
                // shouldn't happen, so make it loud.
                eprintln!("[tcp] frame len {len} exceeds MAX_FRAME {MAX_FRAME}; dropping buffer");
                self.inbuf.clear();
                return out;
            }
            if self.inbuf.len() - pos < HEADER_LEN + len {
                break; // header says more payload than we've received yet
            }
            let sender = u64::from_be_bytes(self.inbuf[pos + 4..pos + 12].try_into().unwrap());
            let payload = self.inbuf[pos + HEADER_LEN..pos + HEADER_LEN + len].to_vec();
            out.push((sender, payload));
            pos += HEADER_LEN + len;
        }
        self.inbuf.drain(..pos);
        out
    }

    fn local_id(&self) -> PeerId {
        self.local_id
    }
}
