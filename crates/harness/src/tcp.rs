//! A real-socket [`Transport`] for the harness: two **separate OS processes** coordinate over a
//! localhost TCP connection, running the exact same `Peer`/`Session` logic as the in-memory loop.
//!
//! Why bother when `Loopback` already exercises the logic? Because this is a higher-fidelity rung:
//! real serialization, a real socket, real cross-process concurrency, and partial reads/writes —
//! the kinds of things an in-memory `Vec` bus can't surface. It's also the host half of the
//! planned layer-3 "debug bridge" (see the `/test-loop` skill): swap this `TcpTransport` for one
//! that speaks to a debug listener inside the live mod and the same scenarios drive a real game.
//!
//! The wire framing is the shared [`unseamless_core::framing`] codec (`[u32 len][u64 sender]
//! [payload]`); this file is just the socket I/O around it.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};

use unseamless_core::framing::{FrameDecoder, encode_frame};
use unseamless_core::transport::{PeerId, Transport};

pub struct TcpTransport {
    local_id: PeerId,
    stream: TcpStream,
    /// Reassembles received bytes into whole frames.
    decoder: FrameDecoder,
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
                    return Ok(Self { local_id, stream, decoder: FrameDecoder::new() });
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
        Ok(Self { local_id, stream, decoder: FrameDecoder::new() })
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
        self.write_all_blocking(&encode_frame(self.local_id, bytes));
    }

    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        // Drain whatever the socket has buffered into the decoder.
        let mut tmp = [0u8; 8192];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => break, // closed
                Ok(n) => self.decoder.push(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        match self.decoder.drain() {
            Ok(frames) => frames,
            // Over a trusted localhost link a desync shouldn't happen; the decoder already cleared
            // its buffer to resync, so just surface it loudly and yield nothing this poll.
            Err(e) => {
                eprintln!("[tcp] {e:?}; dropped framing buffer");
                Vec::new()
            }
        }
    }

    fn local_id(&self) -> PeerId {
        self.local_id
    }
}
