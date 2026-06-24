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
    /// Per-feature kill switch (index-aligned): set when a feature's `on_frame` panics, so it
    /// isn't re-ticked on possibly-torn internal state every subsequent frame.
    disabled: Vec<bool>,
}

static APP: OnceLock<Mutex<App>> = OnceLock::new();

/// Runs on the init thread spawned from `DllMain` (off the loader lock, off the main thread —
/// [`CSTaskImp::wait_for_instance`] blocks on main-thread init). Config load and logging come
/// first so we capture as much as possible; then features register as recurring tasks. The
/// registrations are permanent for the process lifetime.
pub fn install() {
    // Anchor config, logs, and mods to the install dir (our DLL's folder), not the process cwd —
    // Proton/Steam can set cwd elsewhere, and the user edits files next to the game. Fall back to
    // cwd (".") only if we can't locate our own module.
    let module = crate::SELF_MODULE.load(std::sync::atomic::Ordering::Relaxed);
    let base = crate::mods::self_dir(module).unwrap_or_else(|| std::path::PathBuf::from("."));

    // Config before logging: the logger picks its level and writes its header from the config.
    let (config, notes) = crate::config::load(&base);
    crate::logger::init(&config, &base);
    for (level, message) in notes {
        log::log!(level, "{message}");
    }
    if base == std::path::Path::new(".") {
        log::warn!("could not locate our own module dir; using the process cwd for config/logs/mods");
    }

    // Parent-loader: bring up other DLL mods from `mods/` before we block on the task system, so
    // they can hook game init as early as possible. We're our own `dinput8.dll`, so this is on us.
    crate::mods::load_mods(&config, &base);

    let cs_task = match CSTaskImp::wait_for_instance(INIT_TIMEOUT) {
        Ok(task) => task,
        Err(e) => {
            log::error!("CSTaskImp unavailable; mod not installed: {e:?}");
            return;
        }
    };

    let features: Vec<Box<dyn Feature>> = vec![Box::new(SessionObserver::new(config))];
    let frames = vec![0u64; features.len()];
    let disabled = vec![false; features.len()];

    // Snapshot (index, name, phase) before moving the app into the global.
    let registrations: Vec<(usize, &'static str, _)> = features
        .iter()
        .enumerate()
        .map(|(i, f)| (i, f.name(), f.phase()))
        .collect();

    if APP.set(Mutex::new(App { features, frames, disabled })).is_err() {
        log::error!("install() called twice; ignoring");
        return;
    }

    for (index, name, phase) in registrations {
        // Permanent registration: the SDK never unregisters (its `cancel()` is a no-op stub and
        // the task self-references). Forget the handle so its `Drop` can't flip the cancel flag.
        // The DLL must stay resident for the process lifetime — see the no-DETACH note in lib.rs.
        let handle = cs_task.run_recurring(
            move |data: &FD4TaskData| {
                // FFI firewall: a panic must NEVER unwind across the SDK's `extern "C"` task
                // boundary — that's UB. Under the shipped `panic = "abort"` profiles a panic
                // aborts before unwinding (so this is a no-op there), but a default `cargo build`
                // uses `panic = "unwind"`, and this catch is what keeps that build sound.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(index, data))).is_err()
                {
                    disable_feature(index);
                }
            },
            phase,
        );
        std::mem::forget(handle);
        log::info!("registered feature '{name}' in {phase:?}");
    }
}

/// Per-frame entry for the feature at `index`, in its phase. Skips features disabled by a prior
/// panic so they're not re-ticked on torn internal state.
fn tick(index: usize, data: &FD4TaskData) {
    let Some(app) = APP.get() else { return };
    // Recover from a poisoned lock: the `App` container (the Vecs) unwinds structurally intact, so
    // re-locking is memory-safe. The *feature* that panicked may have torn its own invariants, but
    // we never re-tick it — `disable_feature` flags it below.
    let mut app = app.lock().unwrap_or_else(|poison| poison.into_inner());
    if app.disabled.get(index).copied().unwrap_or(true) {
        return;
    }
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

/// Mark a feature as permanently disabled after its `on_frame` panicked (the panic hook already
/// logged the backtrace). Keeps one bad feature from wedging or spamming the rest.
fn disable_feature(index: usize) {
    let Some(app) = APP.get() else { return };
    let mut app = app.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(slot) = app.disabled.get_mut(index) {
        *slot = true;
    }
    let name = app.features.get(index).map(|f| f.name()).unwrap_or("?");
    log::error!("feature '{name}' (index {index}) panicked; disabled for the rest of the session");
}
