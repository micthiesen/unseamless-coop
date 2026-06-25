//! Application wiring: load config, bring up logging, wait for the game's task system, build the
//! feature set, and register each feature as a recurring task in its chosen phase.
//!
//! Features live behind a single global `Mutex<App>`. Each registered task locks it and ticks
//! exactly one feature; since tasks run on the game's main thread the lock is effectively
//! uncontended (it just satisfies the `Fn`/`'static` bounds the scheduler requires).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use eldenring::cs::CSTaskImp;
use eldenring::fd4::FD4TaskData;
use fromsoftware_shared::SharedTaskImpExt;

use crate::feature::{Feature, Tick};
use crate::features::notifications::NotificationsTick;
use crate::features::observer::SessionObserver;
use crate::features::playstate::PlayStateProbe;
use crate::features::session_actions::SessionActionsTick;
use crate::features::session_limit::SessionLimit;

/// How long the init thread keeps trying for the task system before giving up.
const INIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Pause between retries while the game's singleton registry comes up (see
/// [`wait_for_task_system`]).
const TASK_RETRY_DELAY: Duration = Duration::from_millis(250);

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
    // The in-memory log ring buffer (overlay Log tab) must exist before the logger tees into it.
    crate::logbuf::init();
    crate::logger::init(&config, &base);
    crate::notify::init();
    // Queue for menu-requested session actions (overlay → game thread). Before any feature ticks.
    crate::actionq::init();
    for (level, message) in notes {
        log::log!(level, "{message}");
        // Surface only *actionable* config notes (a clamped value, a malformed file) as toasts —
        // the info-level "loaded config" chatter stays log-only so it doesn't toast on every launch.
        if level <= log::Level::Warn {
            let severity = unseamless_core::notifications::Severity::from(level);
            crate::notify::with_mut(|n| {
                n.toast(severity, message, unseamless_core::notifications::DEFAULT_TOAST_SECS * 2.0)
            });
        }
    }
    // The version is shown in the overlay window's title and the off-playfield watermark, so no
    // persistent banner — keep the notification surface for transient, actionable messages only.
    if base == std::path::Path::new(".") {
        log::warn!("could not locate our own module dir; using the process cwd for config/logs/mods");
    }

    // Publish the loaded config as the process-global live config: features read it each frame, and
    // the bridge writes it when it applies a received ConfigSync. Do this before anything reads it.
    crate::state::init(config.clone());

    // Reject an empty/too-short co-op password (the session key). The generated default always
    // passes, so this only fires on a deliberately-cleared password — fail loudly, like the EAC
    // guard, since a weak key risks accidental or trivially-joinable sessions.
    if !config.password_is_valid() {
        log::error!("co-op password too short; aborting");
        crate::guard::fatal(&format!(
            "Your co-op password is too short — it must be at least {} characters.\n\n\
             Set it in unseamless-coop/unseamless_coop.toml, then relaunch.",
            unseamless_core::config::MIN_PASSWORD_LEN
        ));
    }

    // Skip the boot logo videos, if enabled. A one-shot code patch (not a Feature/task), applied
    // early — before we block on the task system — so it lands before the logo gate fires. After the
    // password guard above so a fatal config never patches game memory then immediately aborts.
    // Fail-safe: a missed/ambiguous/drifted AOB just leaves the logos playing (logged), never aborts.
    apply_boot_patches(&config);

    // Dev side-channel bridge: a loopback listener running a live `Session` so the harness can drive
    // the mod over a socket (the /test-loop skill's layer 3). Compiled in only with the `bridge`
    // feature (rig/diag builds) and inert unless a port is configured. Runs on its own thread.
    #[cfg(feature = "bridge")]
    if config.debug.bridge_port > 0 {
        crate::bridge::start(config.debug.bridge_port);
    }

    // Parent-loader: bring up other DLL mods from `mods/` before we block on the task system, so
    // they can hook game init as early as possible. We're our own `dinput8.dll`, so this is on us.
    crate::mods::load_mods(&config, &base);

    // No task system means the mod cannot install a single feature — there's no degraded mode to
    // fall back to, so fail like the other startup guards: a modal box, then close the game (rather
    // than leave it running silently unmodded). See CLAUDE.md > "Surfacing errors".
    let cs_task = match wait_for_task_system() {
        Some(task) => task,
        None => crate::guard::fatal(
            "unseamless-coop couldn't initialize: the game's task system never came up, so the \
             mod can't run.\n\nThis is unexpected — try launching again. Closing the game now.",
        ),
    };

    // SessionLimit before the observer so that — since same-phase tasks tick in registration order
    // (the loop below registers in vec order) — the observer reads and logs the override we just
    // wrote, same frame. It's only a logging nicety: were that order to change, the observer would
    // log the override one frame late, not wrong. Both read the live config (`crate::state`).
    let features: Vec<Box<dyn Feature>> = vec![
        Box::new(NotificationsTick::new()), // ages toasts once per frame, before producers push
        Box::new(SessionLimit::new()),
        Box::new(SessionObserver::new()),
        // Drains overlay-requested session actions (a producer; after the ager above).
        Box::new(SessionActionsTick::new()),
        // Publishes the in-gameplay flag so the overlay shows its watermark only off the playfield.
        Box::new(PlayStateProbe::new()),
    ];
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

    // In-game overlay (hudhook DX12 present-hook). Installed unless the runtime kill-switch is off,
    // so a Proton/vkd3d render problem can be disabled by editing the config (no rebuild). Reads
    // shared app state; never mutates game state.
    if config.debug.overlay {
        crate::overlay::install(module);
        // Suppress the game's own DirectInput reads while the overlay is open — it polls keyboard/mouse
        // via DirectInput, which bypasses the window message queue, so the overlay's message filter
        // alone can't stop WASD/clicks reaching the game. Degrade (overlay still draws) if it fails.
        match unsafe { crate::input::install() } {
            Ok(()) => log::info!("input: hooked DirectInput GetDeviceState for overlay capture"),
            Err(e) => {
                log::error!(
                    "input: hook install failed ({e}); game input won't be suppressed while the overlay is open"
                );
                // Surface it to the player rather than failing silently (in-session problems degrade +
                // inform, per CLAUDE.md): the menu still works, but the game keeps reacting to input.
                crate::notify::with_mut(|n| {
                    n.set_banner(
                        "input-degraded",
                        unseamless_core::notifications::Severity::Warning,
                        "Heads up: the game still reacts to input while the menu is open (input hook failed)",
                    )
                });
            }
        }
    }
}

/// Apply one-shot boot-flow code patches gated by config. Currently just skip-intros: NOP the
/// conditional branch that gates the logo/splash video sequence so the game falls straight through
/// to the title screen. See `coop/patch.rs` and `docs/{SKIP-INTROS,CODE-PATCHING}.md`.
fn apply_boot_patches(config: &unseamless_core::config::Config) {
    if config.gameplay.skip_splash_screens {
        // Scan a distinctive sequence in the boot/title flow; the logo gate is a `74` (JZ rel8) a
        // fixed 60 bytes before it. NOP the 2-byte jump so the game falls through to the title.
        // Pattern adapted from the MIT techiew/SkipTheIntro signature and confirmed on the rig against
        // our pinned game version: it matches the *runtime* image (the on-disk exe is Arxan/Steam-
        // encrypted), which is what `Program::current()` scans. A miss/ambiguous/drifted match fails
        // safe — the logos just play, logged. See docs/SKIP-INTROS.md and docs/CODE-PATCHING.md.
        //
        // Re-derive after a game update: grab the current AOB from techiew/SkipTheIntro's DllMain.cpp
        // (the `c6 ? ? ? ? ? 01 ? 03 ...` landmark + `offset 60` to the `74` jump), translate to a
        // pelite pattern (one `?` = one wildcard byte), and confirm the log shows the `patched
        // 'skip_splash_screens'` line and the logos skip.
        const BOOT_LOGO_LANDMARK: &[pelite::pattern::Atom] =
            pelite::pattern!("C6 ? ? ? ? ? 01 ? 03 00 00 00 ? 8B ? E8 ? ? ? ? E9 ? ? ? ? ? 8D");
        crate::patch::nop_landmark("skip_splash_screens", BOOT_LOGO_LANDMARK, -60, 0x74, 2);
    }
}

/// Wait for the game's task system (`CSTaskImp`), tolerating our early DLL load.
///
/// We ship as `dinput8.dll`, so we initialize far earlier in process startup than an Elden Mod
/// Loader mod does (EML waits seconds before loading `mods/`). In that early window the game's
/// Dantelion2 (DLRF) singleton reflection — the registry that maps the name `"CSTask"` to the live
/// `CSTaskImp` — isn't populated yet, so the SDK can't find the instance.
///
/// The SDK's [`CSTaskImp::wait_for_instance`] only *polls* two cases (the global hINSTANCE not yet
/// set, and the instance pointer present-but-null); a singleton that isn't in the reflection
/// registry yet comes back as `NotFound`, which it reports as `InvalidRva` and returns
/// **immediately** rather than waiting. So we retry the whole call until the registry comes up — it
/// does once the game finishes early init (EML-loaded mods that bind a few seconds in prove it) —
/// bounded by [`INIT_TIMEOUT`]. This can't get stuck on a poisoned cache: `from-singleton` only
/// caches the singleton map permanently once some singleton is non-null (i.e. init has happened),
/// so an early miss doesn't spoil later lookups.
fn wait_for_task_system() -> Option<&'static CSTaskImp> {
    let deadline = Instant::now() + INIT_TIMEOUT;
    let mut announced = false;
    loop {
        // Short inner timeout: this polls the hINSTANCE wait, while our outer loop handles the
        // "registry not ready yet" (`InvalidRva`) case the SDK returns without waiting.
        match CSTaskImp::wait_for_instance(Duration::from_secs(1)) {
            Ok(task) => return Some(task),
            Err(e) => {
                if Instant::now() >= deadline {
                    log::error!("CSTaskImp unavailable after {INIT_TIMEOUT:?}: {e:?}");
                    return None;
                }
                if !announced {
                    log::debug!("task system not ready yet ({e:?}); retrying until the game finishes init");
                    announced = true;
                }
                std::thread::sleep(TASK_RETRY_DELAY);
            }
        }
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
