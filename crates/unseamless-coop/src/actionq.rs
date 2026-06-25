//! Process-global queue of **session actions** requested from the overlay menu.
//!
//! The overlay runs on the Present thread and must never block on the game thread, so the producer
//! side is non-blocking ([`try_offer`]): an action that can't be enqueued this frame (the drain
//! momentarily holds the lock) is retried next frame by the caller, never lost or blocked on. The
//! game-thread consumer ([`drain`]) empties the queue and surfaces each action (see
//! [`crate::features::session_actions`]). Executing an action against the live session is the
//! rig-gated apply layer still ahead; this queue is the seam between the two threads.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, TryLockError};

use unseamless_core::protocol::SessionAction;

static QUEUE: OnceLock<Mutex<VecDeque<SessionAction>>> = OnceLock::new();

/// Hard cap on queued-but-undrained actions. The consumer ([`crate::features::session_actions`]) is a
/// frame task, and a feature that panics is permanently disabled (`app.rs`) — so without a cap a wedged
/// drain would let the queue grow for the rest of the session. Past the cap we drop the oldest: a stale
/// backlog of session verbs is worthless anyway, and dropping keeps both this queue and the producer's
/// retry buffer bounded. Generous, since a human can't enqueue many between frames.
const CAP: usize = 64;

/// Initialize the queue. Called once at install, before the overlay or the drain feature run.
pub fn init() {
    let _ = QUEUE.set(Mutex::new(VecDeque::new()));
}

/// Try to enqueue one action **without blocking**. Returns `false` only if uninitialized or the lock is
/// momentarily held by the draining game thread — the caller retries next frame so a keypress is never
/// dropped by contention. A *full* queue (drain wedged) instead drops the oldest and reports success, so
/// the producer's retry buffer can't grow unbounded either.
pub fn try_offer(action: SessionAction) -> bool {
    let Some(m) = QUEUE.get() else { return false };
    match m.try_lock() {
        Ok(mut q) => {
            push_capped(&mut q, action);
            true
        }
        Err(TryLockError::Poisoned(p)) => {
            push_capped(&mut p.into_inner(), action);
            true
        }
        Err(TryLockError::WouldBlock) => false,
    }
}

/// Append, evicting the oldest first if at [`CAP`].
fn push_capped(q: &mut VecDeque<SessionAction>, action: SessionAction) {
    while q.len() >= CAP {
        q.pop_front();
    }
    q.push_back(action);
}

/// Drain all queued actions on the game thread. The lock is held only to move the items out.
pub fn drain() -> Vec<SessionAction> {
    let Some(m) = QUEUE.get() else { return Vec::new() };
    let mut q = m.lock().unwrap_or_else(|p| p.into_inner());
    q.drain(..).collect()
}
