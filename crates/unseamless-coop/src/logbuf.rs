//! In-memory **ring buffer** of recent log lines, for the overlay's Log panel.
//!
//! A [`RingLogger`] tees every emitted log record — alongside the file `WriteLogger`, via
//! `simplelog::CombinedLogger` (see [`crate::logger`]) — into a bounded [`VecDeque`], newest last.
//! The overlay's Present-thread Log tab reads it **non-blocking** ([`try_read`]) so you can watch our
//! own log live in-game without alt-tabbing to the file. Bounded to [`CAP`] lines, so it can't grow
//! without limit during a long session.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};
use simplelog::{Config as SlConfig, SharedLogger};

/// Most recent lines kept; older ones scroll off. A few hundred is plenty to see what just happened.
const CAP: usize = 400;

/// One captured line: its level (for colouring) and the rendered message.
#[derive(Clone)]
pub struct Line {
    pub level: Level,
    pub text: String,
}

static BUFFER: OnceLock<Mutex<VecDeque<Line>>> = OnceLock::new();

/// Initialize the shared buffer. Call **before** [`ring_logger`] is installed (in `app::install`,
/// ahead of `logger::init`).
pub fn init() {
    let _ = BUFFER.set(Mutex::new(VecDeque::with_capacity(CAP)));
}

/// Read the buffer **without blocking** — for the overlay's Present-thread render loop. Returns
/// `None` if uninitialized or momentarily contended (skip drawing the log this frame).
pub fn try_read<R>(f: impl FnOnce(&VecDeque<Line>) -> R) -> Option<R> {
    let m = BUFFER.get()?;
    match m.try_lock() {
        Ok(b) => Some(f(&b)),
        Err(TryLockError::Poisoned(p)) => Some(f(&p.into_inner())),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// Append a line, evicting the oldest when full. Called from arbitrary threads (any thread that
/// logs); the lock is held only for the push. Prefixes a fixed-width `HH:MM:SS` so order is obvious in
/// the viewer (the file log keeps its own full RFC3339 timestamp).
fn push(level: Level, text: String) {
    let Some(m) = BUFFER.get() else { return };
    let line = format!("{} {}", short_time(), text);
    let mut b = m.lock().unwrap_or_else(|p| p.into_inner());
    if b.len() == CAP {
        b.pop_front();
    }
    b.push_back(Line { level, text: line });
}

/// Wall-clock time-of-day as fixed-width `HH:MM:SS` (UTC, matching the file log's RFC3339), so log
/// lines stay aligned. Computed without a date library — order is what matters here, not the date.
fn short_time() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let tod = secs % 86_400;
    format!("{:02}:{:02}:{:02}", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// A `simplelog::SharedLogger` that mirrors records into the in-memory ring buffer at `level`.
pub struct RingLogger {
    level: LevelFilter,
}

/// Build the ring logger at the given verbosity (match the file logger's level), boxed for
/// `CombinedLogger`.
pub fn ring_logger(level: LevelFilter) -> Box<RingLogger> {
    Box::new(RingLogger { level })
}

impl Log for RingLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        push(record.level(), format!("{}", record.args()));
    }

    fn flush(&self) {}
}

impl SharedLogger for RingLogger {
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
