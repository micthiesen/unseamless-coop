//! Application wiring: load config, bring up logging, wait for the game's task system, build the
//! feature set, and register each feature as a recurring task in its chosen phase.
//!
//! Features live behind a single global `Mutex<App>`. Each registered task locks it and ticks
//! exactly one feature; since tasks run on the game's main thread the lock is effectively
//! uncontended (it just satisfies the `Fn`/`'static` bounds the scheduler requires).

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use eldenring::cs::CSTaskImp;
use eldenring::fd4::FD4TaskData;
use fromsoftware_shared::SharedTaskImpExt;

use crate::feature::{Feature, Tick};
use crate::features::observer::SessionObserver;

/// How long the init thread waits for the task system before giving up.
const INIT_TIMEOUT: Duration = Duration::from_secs(60);

struct App {
    features: Vec<Box<dyn Feature>>,
    /// Per-feature tick counters (index-aligned with `features`).
    frames: Vec<u64>,
}

static APP: OnceLock<Mutex<App>> = OnceLock::new();

/// Runs on the init thread spawned from `DllMain` (off the loader lock, off the main thread —
/// [`CSTaskImp::wait_for_instance`] blocks on main-thread init). Config load and logging come
/// first so we capture as much as possible; then features register as recurring tasks. The
/// registrations are permanent for the process lifetime.
pub fn install() {
    // Config before logging: the logger picks its level and writes its header from the config.
    let (config, notes) = crate::config::load();
    crate::logger::init(&config);
    for (level, message) in notes {
        log::log!(level, "{message}");
    }

    let cs_task = match CSTaskImp::wait_for_instance(INIT_TIMEOUT) {
        Ok(task) => task,
        Err(e) => {
            log::error!("CSTaskImp unavailable; mod not installed: {e:?}");
            return;
        }
    };

    let features: Vec<Box<dyn Feature>> = vec![Box::new(SessionObserver::new(config))];
    let frames = vec![0u64; features.len()];

    // Snapshot (index, name, phase) before moving the app into the global.
    let registrations: Vec<(usize, &'static str, _)> = features
        .iter()
        .enumerate()
        .map(|(i, f)| (i, f.name(), f.phase()))
        .collect();

    if APP.set(Mutex::new(App { features, frames })).is_err() {
        log::error!("install() called twice; ignoring");
        return;
    }

    for (index, name, phase) in registrations {
        // Permanent registration: the SDK never unregisters (its `cancel()` is a no-op stub and
        // the task self-references). Forget the handle so its `Drop` can't flip the cancel flag.
        // The DLL must stay resident for the process lifetime — see the no-DETACH note in lib.rs.
        let handle = cs_task.run_recurring(move |data: &FD4TaskData| tick(index, data), phase);
        std::mem::forget(handle);
        log::info!("registered feature '{name}' in {phase:?}");
    }
}

/// Per-frame entry for the feature at `index`, in its phase.
fn tick(index: usize, data: &FD4TaskData) {
    let Some(app) = APP.get() else { return };
    let Ok(mut app) = app.lock() else { return }; // poisoned only if a feature panicked
    let Some(frame) = app.frames.get_mut(index).map(|f| {
        *f += 1;
        *f
    }) else {
        return;
    };
    let tick = Tick { frame, delta: data.delta_time.time };
    if let Some(feature) = app.features.get_mut(index) {
        feature.on_frame(tick);
    }
}
