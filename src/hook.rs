//! Skeleton frame hook: registers a recurring task on the game's own scheduler and logs a
//! heartbeat. This proves the harness end-to-end — `wait_for_instance` → `run_recurring` →
//! per-frame firing — which is the part that's verifiable solo (without a loaded save). The
//! reverse-engineered Seamless Co-op behavior gets built out from here, in `on_frame`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use eldenring::cs::{CSTaskGroupIndex, CSTaskImp};
use eldenring::fd4::FD4TaskData;
use fromsoftware_shared::SharedTaskImpExt;

/// How long the init thread waits for the game's task system before giving up.
const INIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Phase to run in each frame. `FrameBegin` ticks every frame including menus/title, which
/// makes the heartbeat observable without loading a save. Real game-state work should move to
/// a phase ordered against the state it touches (e.g. `WorldChrMan_PostPhysics`).
const PHASE: CSTaskGroupIndex = CSTaskGroupIndex::FrameBegin;

static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Runs on a short-lived init thread spawned from `DllMain`: wait for the task system, then
/// register the per-frame task and return. Must NOT run on the main thread, since
/// [`CSTaskImp::wait_for_instance`] blocks on main-thread initialization.
pub fn install() {
    let cs_task = match CSTaskImp::wait_for_instance(INIT_TIMEOUT) {
        Ok(task) => task,
        Err(e) => {
            log::error!("CSTaskImp unavailable; hook not installed: {e:?}");
            return;
        }
    };

    // Registration is permanent: the SDK never unregisters (its `cancel()` is a no-op stub and
    // the task keeps an internal self-reference). Forget the handle so its `Drop` can't flip
    // the cancel flag or leave a dangling task. The DLL must stay resident for the process
    // lifetime — see the no-DETACH note in `lib.rs`. Do NOT store/drop this handle.
    let handle = cs_task.run_recurring(|_: &FD4TaskData| on_frame(), PHASE);
    std::mem::forget(handle);

    log::info!("hook installed: heartbeat task running in {PHASE:?}");
}

/// Per-frame. Skeleton: heartbeat only (first tick, then ~every 10s at 60fps).
fn on_frame() {
    let f = FRAMES.fetch_add(1, Ordering::Relaxed);
    if f == 0 || f.is_multiple_of(600) {
        log::info!("frame task live (frame {f})");
    }
}
