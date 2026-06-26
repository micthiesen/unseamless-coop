//! Client-side **debug-log forwarding** queue (rung 2 of [`docs/COOP-CONNECTION.md`]).
//!
//! When `[debug] forward_to_host` is on and we're a co-op *client*, the host aggregates everyone's
//! logs in one place (the host-tested [`Peer::forward_log`](unseamless_core::peer::Peer::forward_log)
//! → `LogBundle`). This module is the missing link the side-channel needed: a [`ForwardLogger`] tees
//! every emitted record into a bounded queue, and the co-op driver ([`crate::coop`]) drains it each
//! tick, mapping records through `Peer::forward_log` (which seq-stamps and rate-limits them) onto the
//! wire. Inert until the driver decides this session is a forwarding client.
//!
//! ## No feedback loop, no cross-thread stall
//! Forwarding a record triggers a Steam send, and a send that logs would re-enter the logger. Two
//! guards prevent amplification: (1) [`drain`] sets a **thread-local** "in forward" flag around the
//! send loop, so *anything* logged on the driver thread while forwarding — in any module or crate —
//! is dropped rather than re-enqueued (a target-agnostic loop breaker); (2) [`ForwardLogger::log`]
//! also drops records from our own networking modules outright (noise reduction — the host doesn't
//! need a client's view of its own side-channel). The bounded queue + the `Peer`'s rate limiter are
//! the remaining backstops. Critically, [`drain`] copies the queue out and **releases the lock
//! before** doing any network I/O, so a producer thread (a game-frame feature, the overlay) logging
//! a record never blocks on a Steam send.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, TryLockError};

use log::{LevelFilter, Log, Metadata, Record};
use simplelog::{Config as SlConfig, SharedLogger};
use unseamless_core::diagnostics::LogLevel;

/// Bounded backlog of records awaiting forward, so a logging burst can't grow it without bound. The
/// driver drains it ~10×/s and the `Peer` rate-limits the wire, so this is the only place records
/// could accumulate; drop-oldest past the cap (losing the *oldest* unsent line, like the ring buffer).
const CAP: usize = 256;

static QUEUE: OnceLock<Mutex<VecDeque<(LogLevel, String)>>> = OnceLock::new();
/// Off until the co-op driver finds it's a forwarding client (`!is_host && forward_to_host`). While
/// off, [`ForwardLogger::log`] does nothing past a relaxed atomic load, so a normal session pays
/// nothing for this logger being installed.
static ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Set on the driver thread for the duration of a [`drain`] send loop. While set, this thread's
    /// own log records are dropped by [`ForwardLogger::log`], so the act of forwarding can't enqueue
    /// more work to forward — regardless of which module/crate emitted the line.
    static IN_FORWARD: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard that marks the current thread "inside forwarding" (see [`IN_FORWARD`]) and clears it on
/// drop, so the flag is reset even if a forward closure panics.
struct ForwardGuard;

impl ForwardGuard {
    fn enter() -> Self {
        IN_FORWARD.with(|f| f.set(true));
        ForwardGuard
    }
}

impl Drop for ForwardGuard {
    fn drop(&mut self) {
        IN_FORWARD.with(|f| f.set(false));
    }
}

/// Create the shared queue. Call **before** [`forward_logger`] is installed (in `app::install`,
/// alongside `logbuf::init`, ahead of `logger::init`).
pub fn init() {
    let _ = QUEUE.set(Mutex::new(VecDeque::new()));
}

/// Turn forwarding on/off (the driver enables it for a forwarding client).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Drain every queued record, handing each to `f` (the driver maps it through `Peer::forward_log`).
/// Non-blocking: if a producer momentarily holds the lock, skip this tick and drain on the next.
///
/// `f` does network I/O (Steam sends), so the queue is copied out and the lock **released** before
/// any `f` call — otherwise a producer thread logging into [`push`] would block on a send. The send
/// loop runs under a [`ForwardGuard`] so any record `f`'s path logs (on this thread) is dropped, not
/// re-enqueued.
pub fn drain(mut f: impl FnMut(LogLevel, String)) {
    let Some(m) = QUEUE.get() else { return };
    let batch: Vec<(LogLevel, String)> = {
        let mut q = match m.try_lock() {
            Ok(q) => q,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        q.drain(..).collect()
    };
    let _guard = ForwardGuard::enter();
    for (level, message) in batch {
        f(level, message);
    }
}

/// Enqueue a record for forwarding, dropping the oldest when full.
fn push(level: LogLevel, message: String) {
    let Some(m) = QUEUE.get() else { return };
    let mut q = m.lock().unwrap_or_else(|p| p.into_inner());
    if q.len() == CAP {
        q.pop_front();
    }
    q.push_back((level, message));
}

/// A `simplelog::SharedLogger` that tees records into the forward queue — but only while [`ENABLED`]
/// and never our own networking modules' lines (the feedback guard). Installed alongside the file and
/// ring loggers via `CombinedLogger` (see [`crate::logger`]).
pub struct ForwardLogger {
    level: LevelFilter,
}

/// Build the forward logger at the given verbosity (match the file logger's level), boxed for
/// `CombinedLogger`.
pub fn forward_logger(level: LevelFilter) -> Box<ForwardLogger> {
    Box::new(ForwardLogger { level })
}

impl Log for ForwardLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        // Off (the common case) or a record above our level: nothing to do but a relaxed atomic load.
        // `IN_FORWARD` is the reentrancy guard: while this thread is inside a forward send loop, drop
        // everything it logs so forwarding can't enqueue more forwarding (target-/crate-agnostic).
        if !ENABLED.load(Ordering::Relaxed)
            || IN_FORWARD.with(|f| f.get())
            || !self.enabled(record.metadata())
        {
            return;
        }
        // Noise reduction (not the loop guard above): don't forward our own networking/forwarding
        // lines — the host doesn't need a client's view of its own side-channel chatter.
        let target = record.target();
        if target.starts_with("unseamless_coop::coop")
            || target.starts_with("unseamless_coop::steam")
            || target.starts_with("unseamless_coop::forward")
        {
            return;
        }
        push(LogLevel::from_log_level(record.level()), format!("{}", record.args()));
    }

    fn flush(&self) {}
}

impl SharedLogger for ForwardLogger {
    fn level(&self) -> LevelFilter {
        self.level
    }

    fn config(&self) -> Option<&SlConfig> {
        None
    }

    fn as_log(self: Box<Self>) -> Box<dyn Log> {
        self
    }
}
