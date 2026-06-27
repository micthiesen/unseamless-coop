//! A **log TEE → drain queue** feeding new log lines to the rig-guide engine each tick.
//!
//! The guide engine's auto-finish predicates can match on log output (e.g. `log_contains` over the
//! `session-probe:` transition lines). To do that the game-thread feature needs "the log lines emitted
//! since my last tick" — a drain queue, not a ring buffer. This mirrors [`crate::forward`]'s tee:
//! a [`GuideLogger`] pushes every emitted record's message into a bounded [`VecDeque`], and the
//! feature [`drain`]s it each frame into [`GuideRunner::tick`](unseamless_core::guide::GuideRunner).
//!
//! Inert until a guide is active: [`ENABLED`] starts `false`, so [`GuideLogger::log`] does nothing
//! past a relaxed atomic load when no guide runs (the common case, and the only case in non-rig play).
//! The feature flips it on when it starts a guide. Debug-only (`#[cfg(debug_assertions)]`), like the
//! rest of the subsystem.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, TryLockError};

use log::{LevelFilter, Log, Metadata, Record};
use simplelog::{Config as SlConfig, SharedLogger};
use unseamless_core::util::push_capped;

/// Bounded backlog of un-drained lines. The feature drains every frame, so this only fills if the
/// game stalls; drop-oldest past the cap (the shared [`push_capped`] discipline).
const CAP: usize = 512;

static QUEUE: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
/// Off until a guide is running, so a normal session pays nothing for this logger being installed.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Create the shared queue. Call **before** [`guide_logger`] is installed (in `app::install`,
/// alongside `logbuf::init`, ahead of `logger::init`).
pub fn init() {
    let _ = QUEUE.set(Mutex::new(VecDeque::new()));
}

/// Turn the tee on/off (the rig-guide feature enables it while a step is showing). Disabling also
/// drains the queue so a finished/idle guide leaves no residual lines behind (best-effort; a momentary
/// lock contention just leaves the bounded backlog to be drop-oldest'd as before).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        return;
    }
    // Disabling: drain the queue so a finished/idle guide leaves no residual lines behind.
    let Some(m) = QUEUE.get() else { return };
    match m.try_lock() {
        Ok(mut q) => q.clear(),
        Err(TryLockError::Poisoned(p)) => p.into_inner().clear(),
        Err(TryLockError::WouldBlock) => {}
    }
}

/// Drain every queued line into `f` (the feature feeds them to the engine). Non-blocking: if a
/// producer momentarily holds the lock, skip this tick and drain on the next.
///
/// Copies the batch out and **releases the lock before** calling `f`, exactly like [`crate::forward`]'s
/// drain: a `GuideLogger` shares this same `QUEUE` mutex (non-reentrant), so running `f` under the lock
/// would self-deadlock if `f` ever logged. The current caller doesn't, but matching the safe pattern
/// keeps it that way.
pub fn drain(mut f: impl FnMut(String)) {
    let Some(m) = QUEUE.get() else { return };
    let batch: Vec<String> = {
        let mut q = match m.try_lock() {
            Ok(q) => q,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        q.drain(..).collect()
    };
    for line in batch {
        f(line);
    }
}

/// Enqueue a line, dropping the oldest when full.
fn push(message: String) {
    let Some(m) = QUEUE.get() else { return };
    let mut q = m.lock().unwrap_or_else(|p| p.into_inner());
    push_capped(&mut q, message, CAP);
}

/// A `simplelog::SharedLogger` that tees record messages into the guide queue while [`ENABLED`].
/// Installed alongside the file/ring/forward loggers via `CombinedLogger` (see [`crate::logger`]),
/// but only in debug builds.
pub struct GuideLogger {
    level: LevelFilter,
}

/// Build the guide logger at the given verbosity (match the file logger's level), boxed for
/// `CombinedLogger`.
pub fn guide_logger(level: LevelFilter) -> Box<GuideLogger> {
    Box::new(GuideLogger { level })
}

impl Log for GuideLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        // Off (the common case) or above our level: nothing but a relaxed atomic load.
        if !ENABLED.load(Ordering::Relaxed) || !self.enabled(record.metadata()) {
            return;
        }
        push(record.args().to_string());
    }

    fn flush(&self) {}
}

impl SharedLogger for GuideLogger {
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
