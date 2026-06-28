//! Application wiring: load config, bring up logging, wait for the game's task system, build the
//! feature set, and register each feature as a recurring task in its chosen phase.
//!
//! Features live behind a single global `Mutex<App>`. Each registered task locks it and ticks
//! exactly one feature; since tasks run on the game's main thread the lock is effectively
//! uncontended (it just satisfies the `Fn`/`'static` bounds the scheduler requires).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use eldenring::cs::CSTaskImp;
use eldenring::fd4::FD4TaskData;
use fromsoftware_shared::SharedTaskImpExt;

use crate::feature::{Feature, Tick};
use crate::features::boot_volume::BootVolume;
use crate::features::crit_coop::CritCoop;
use crate::features::death_debuffs::DeathDebuffsFeature;
use crate::features::nameplates::Nameplates;
use crate::features::native_nameplates::NativeNameplates;
use crate::features::native_toasts::NativeToasts;
use crate::features::notifications::NotificationsTick;
use crate::features::observer::SessionObserver;
use crate::features::playstate::PlayStateProbe;
use crate::features::scaling::ScalingFeature;
use crate::features::seamless::SeamlessRoam;
use crate::features::session_actions::SessionActionsTick;
use crate::features::session_limit::SessionLimit;
use crate::features::world_time::WorldTimeLock;

/// How long the init thread keeps trying for the task system before giving up.
const INIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Pause between retries while the game's singleton registry comes up (see
/// [`wait_for_task_system`]).
const TASK_RETRY_DELAY: Duration = Duration::from_millis(250);

struct App {
    features: Vec<Box<dyn Feature>>,
    /// Per-feature tick counters (index-aligned with `features`).
    frames: Vec<u64>,
}

static APP: OnceLock<Mutex<App>> = OnceLock::new();

/// One registered feature's stable identity + live health, kept in [`FEATURES`] **outside** the `APP`
/// mutex.
struct FeatureSlot {
    name: &'static str,
    /// Set when this feature's `on_frame` panics; it's then skipped on every later frame.
    disabled: AtomicBool,
}

/// Lock-free registry of every feature's identity + disabled state, index-aligned with `App.features`.
/// Deliberately separate from [`APP`] so reads never need that (tick-held) lock: [`tick`] checks it to
/// skip a panicked feature, [`disable_feature`] sets it, and [`feature_status`] reads it without
/// contending the tick lock. That decoupling is what keeps the live debug panel's `features` section
/// readable instead of perpetually "unavailable" when its publisher runs mid-tick.
static FEATURES: OnceLock<Vec<FeatureSlot>> = OnceLock::new();

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

    init_subsystems(&base, &config, notes);
    pre_task_startup(&config, &base);

    // No task system means the mod cannot install a single feature — there's no degraded mode to
    // fall back to, so fail like the other startup guards: a modal box, then close the game (rather
    // than leave it running silently unmodded). See CLAUDE.md > "Surfacing errors".
    let Some(cs_task) = wait_for_task_system() else {
        crate::guard::fatal(
            "unseamless-coop couldn't initialize: the game's task system never came up, so the \
             mod can't run.\n\nThis is unexpected — try launching again. Closing the game now.",
        );
    };

    // Build, then register the feature set. A `false` return means `install` somehow ran twice; bail
    // without re-registering or re-dumping.
    if !register_features(cs_task, build_features(&config)) {
        return;
    }

    // Boot snapshot: a full diagnostic report once everything's registered, so every shared log opens
    // with the live state at startup (most singletons are still pre-init here — that's expected and
    // itself informative). Periodic/panic snapshots add the evolving picture.
    crate::diag::dump("boot");

    // In-game overlay, deferred to its own thread until the frontend is up (see [`spawn_overlay`]).
    // Installed unless the runtime kill-switch is off, so a Proton/vkd3d render problem can be
    // disabled by editing the config (no rebuild).
    if config.debug.overlay {
        spawn_overlay(module);
    }
}

/// Stand up the in-process plumbing every feature depends on, then publish the live config. Ordering
/// is load-bearing: the log ring buffer (overlay Log tab) and the co-op forward queue must exist
/// before the logger tees into them, and all of this lands before any feature ticks or any task
/// registers. Finishes by replaying the config-load notes and publishing the global live config.
fn init_subsystems(
    base: &std::path::Path,
    config: &unseamless_core::config::Config,
    notes: Vec<crate::config::Note>,
) {
    crate::logbuf::init();
    crate::forward::init();
    // Rig-guide log tee queue (debug-only): must exist before the logger installs its `guide_logger`
    // sink, like the ring/forward queues above. Inert (drops every record) until a guide enables it.
    #[cfg(debug_assertions)]
    crate::guide_log::init();
    crate::logger::init(config, base);
    crate::notify::init();
    // Queue for menu-requested session actions (overlay → game thread). Before any feature ticks.
    crate::actionq::init();
    // Snapshot cell the debug-panel publisher posts into and the overlay reads. Before any tick.
    crate::debug_panel::init();
    // Label cell the nameplates feature posts projected peer labels into and the overlay draws.
    crate::nameplates::init();
    // Rig-testing guide pinned-banner cell the overlay reads (debug-only). Before any feature ticks
    // or the overlay draws; inert until a `[debug] guide` is configured (the banner stays `None`).
    #[cfg(debug_assertions)]
    crate::rig_guide::init();
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
}

/// Everything that must run before we block on the game's task system, in the order it has to happen.
/// Two of these are **fatal guards** (save isolation, co-op password) that close the game rather than
/// continue in a wrong state — see CLAUDE.md > "Surfacing errors".
fn pre_task_startup(config: &unseamless_core::config::Config, base: &std::path::Path) {
    // Resolve our own SteamID off-thread (the identity rung of the co-op connection plan). Steam comes
    // up after our early dinput8 load, so this polls until ready, then publishes the ID for the overlay
    // and logs it. Independent of the overlay kill-switch and of the task system below.
    crate::steam::start();

    // Steam-readiness gate: a one-shot probe that publishes Connecting -> Ready/Failed (off the rung-1
    // SteamID + networking resolve) and narrates it via a banner. The overlay gates the Open World /
    // Join world actions on this so the player can't try to host/join before Steam networking is up.
    crate::steam_ready::start();

    // Co-op save isolation, installed as early as possible — before the game opens its save (the
    // title/load screen is the first read). Redirects ER0000.sl2 -> ER0000.<ext> so co-op never
    // touches the player's single-player save. This is **safety-critical**: if the user wants
    // isolation (the default) and we can't install the hook, refuse to run rather than risk writing
    // co-op progress into their vanilla save. A disabled config (ext = sl2/empty) makes this a no-op.
    if let Err(e) = unsafe { crate::saves::install(&config.save.file_extension) } {
        log::error!("co-op saves: {e}");
        crate::guard::fatal(&format!(
            "unseamless-coop couldn't protect your save file ({e}).\n\n\
             To keep co-op from overwriting your single-player save, the game will now close. \
             Please try launching again."
        ));
    }

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
    apply_boot_patches(config);

    // Dev side-channel bridge: a loopback listener running a live `Session` so the harness can drive
    // the mod over a socket (the /test-loop skill's layer 3). Compiled in only with the `bridge`
    // feature (rig/diag builds) and inert unless a port is configured. Runs on its own thread.
    #[cfg(feature = "bridge")]
    if config.debug.bridge_port > 0 {
        crate::bridge::start(config.debug.bridge_port);
    }

    // The real side-channel (rung 2+4 of docs/COOP-CONNECTION.md) is no longer started at launch: a
    // private Steam P2P link to the partner resolved by password-keyed lobby discovery is triggered
    // on demand by the in-overlay Open World / Join world actions (`crate::coop::host`/`join`, routed
    // from `features::session_actions`), not automatically. A solo session pays nothing.

    // Rung-4 RunCallbacks probe (gated by `[debug.probes] lobby_callback_probe`, off by default):
    // register one harmless private `CreateLobby` and poll whether it completes under ER's own Steam
    // pump — the probe that settled the rung-4 design question. Pure diagnostic; runs on its own thread
    // and degrades (logs) on any binding miss. See docs/COOP-CONNECTION.md rung 4.
    if config.debug.probes.lobby_callback_probe {
        crate::steam::run_lobby_callback_probe();
    }

    // Rung-3 RE prep: install the session create/join logging hooks (gated by `[debug.probes]
    // session_probe`, off by default). Inert when off, and inert-but-announced when on until the
    // create/join AOBs are charted on the rig — see `coop/session_probe` + docs/SESSION-RE-RUNBOOK.md.
    // A pure diagnostic, so it degrades (logs) and never aborts.
    crate::session_probe::install_hooks(config);

    // Parent-loader: bring up other DLL mods from `mods/` before we block on the task system, so
    // they can hook game init as early as possible. We're our own `dinput8.dll`, so this is on us.
    crate::mods::load_mods(config, base);
}

/// Build the feature set **in registration order**, which is semantic: same-phase tasks tick in this
/// (vec) order. So the order encodes behavior and must be preserved —
/// - `NotificationsTick` leads so it ages toasts once per frame *before* any producer pushes.
/// - `SessionLimit` precedes `SessionObserver` so the observer reads and logs the override the same
///   frame (a logging nicety: were that order to change, the observer would log it one frame late,
///   not wrong). Both read the live config (`crate::state`).
///
/// Appending is safe — the requestable diagnostic probes tack on at the end (empty in normal play).
fn build_features(config: &unseamless_core::config::Config) -> Vec<Box<dyn Feature>> {
    let mut features: Vec<Box<dyn Feature>> = vec![
        Box::new(NotificationsTick::new()),
        Box::new(SessionLimit::new()),
        // Hold the area-restriction lever so the party can roam the whole map (reads live config).
        Box::new(SeamlessRoam::new()),
        Box::new(SessionObserver::new()),
        // Drains overlay-requested session actions (a producer; after the ager above).
        Box::new(SessionActionsTick::new()),
        // Publishes the in-gameplay flag so the overlay shows its watermark only off the playfield.
        Box::new(PlayStateProbe::new()),
        // Stacking death penalty (reads HP after PostPhysics). Reads live config; no-op when off.
        Box::new(DeathDebuffsFeature::new()),
        // Clears crit-invuln so co-op partners can damage during crits (PostPhysics). No-op when off.
        Box::new(CritCoop::new()),
        // Projects peer positions to screen-space labels for the overlay to draw (PostPhysics, reads
        // camera + positions only). Reads live config; no-op (publishes nothing) when off.
        Box::new(Nameplates::new()),
        // Spike: overhead nameplate markers drawn natively via CSEzDraw (no overlay/present-hook).
        // Config-file-only (`[nameplates] native_spike`); no-op when off.
        Box::new(NativeNameplates::new()),
        // Spike: notification toasts drawn natively via CSEzDraw + the bitmap font (no overlay).
        // Same `native_spike` gate; no-op when off or when no toasts are active.
        Box::new(NativeToasts::new()),
        // Holds the time of day when locked (reads live config; no-op when off).
        Box::new(WorldTimeLock::new()),
        // Writes the per-player enemy/boss scaling curve into the multiplayer SpEffect rate rows
        // (reads live config; rig-verified, live by default) — see features::scaling.
        Box::new(ScalingFeature::new()),
        // Sets master volume once at boot, then leaves the in-game slider free (reads live config).
        Box::new(BootVolume::new()),
        // Feeds the overlay's live debug panel (a published snapshot), but only while that panel is
        // shown — otherwise a single atomic load per frame. Near-free when off; see crate::debug_panel.
        crate::diag::debug_panel_feature(),
    ];
    // Append any requestable diagnostic probes enabled in `[debug.probes]` (empty in normal play):
    // the diag probes, plus the rung-3 session probe's gated FSM rising-edge logger (every
    // lobby/protocol transition, under the `session-probe:` prefix; solo it just sits at `lobby=None`,
    // the transition machinery still running). Off by default; on for a create/join RE run.
    features.extend(crate::diag::probe_features(config));
    features.extend(crate::session_probe::probe_features(config));
    // Rig-testing guide (debug-only): one feature when `[debug] guide` names a committed guide, else
    // empty. Appended last, like the diagnostic probes — it only reads state + the pad and publishes a
    // banner, so its tick order doesn't matter.
    #[cfg(debug_assertions)]
    features.extend(crate::features::rig_guide::feature(config));
    // Auto-start a co-op session (`[debug] auto_session`), or nothing when off — the headless
    // Open/Join for a machine that can't use the overlay (e.g. native-Windows hudhook DX12 crash).
    features.extend(crate::features::auto_session::feature(config));
    features
}

/// Register each built feature as a recurring task in its phase and publish the lock-free health
/// registry. Returns `false` if `install` somehow ran twice (the global `APP` was already set), so the
/// caller can bail without re-registering.
fn register_features(cs_task: &'static CSTaskImp, features: Vec<Box<dyn Feature>>) -> bool {
    let frames = vec![0u64; features.len()];

    // Snapshot (index, name, phase) before moving the app into the global.
    let registrations: Vec<(usize, &'static str, _)> = features
        .iter()
        .enumerate()
        .map(|(i, f)| (i, f.name(), f.phase()))
        .collect();

    // Publish the lock-free feature registry (identity + health) before any task can fire, so both
    // `tick`'s skip check and the diagnostic snapshot read it without the `APP` lock.
    let _ = FEATURES.set(
        registrations.iter().map(|&(_, name, _)| FeatureSlot { name, disabled: AtomicBool::new(false) }).collect(),
    );

    if APP.set(Mutex::new(App { features, frames })).is_err() {
        log::error!("install() called twice; ignoring");
        return false;
    }

    for (index, name, phase) in registrations {
        // Permanent registration: the SDK never unregisters (its `cancel()` is a no-op stub and
        // the task self-references). Forget the handle so its `Drop` can't flip the cancel flag.
        // The DLL must stay resident for the process lifetime — see the no-DETACH note in lib.rs.
        let handle = cs_task.run_recurring(
            move |data: &FD4TaskData| {
                // FFI firewall: a panic must NEVER unwind across the SDK's `extern "C"` task
                // boundary — that's UB. Every shipped profile is now `panic = "unwind"` (release
                // and diag alike, after the FFI-unwind audit — see docs/FFI-UNWIND-AUDIT.md), so
                // this catch is load-bearing in the player's build, not just a debug aid: a
                // panicking feature is caught, disabled, and toasted instead of crashing the game.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(index, data))).is_err()
                {
                    // The recovery runs in this same SDK-invoked task closure, so it must ALSO never
                    // unwind across the `extern "C"` boundary. Both steps are fallible under unwind:
                    // `disable_feature` logs + toasts (locks), and `diag::dump` reads live game
                    // singletons that — right after a feature panic, with state most likely torn —
                    // can themselves panic on an unwired pointer. Wrap the whole branch in its own
                    // firewall so a second panic is contained, not propagated. (Lock-safe: the
                    // panicked tick's APP lock already unwound, and the dump/notify paths use
                    // independent, poison-recovering locks, so there's no re-lock to deadlock on.)
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        disable_feature(index);
                        crate::diag::dump("feature-panic");
                    }));
                }
            },
            phase,
        );
        std::mem::forget(handle);
        log::info!("registered feature '{name}' in {phase:?}");
    }
    true
}

/// Spawn the in-game overlay (hudhook DX12 present-hook) install on its own short-lived thread.
///
/// Deferred until the frontend is up (title/menu reached — `GameState::frontend_ready`), not
/// installed at boot. Installing at boot gave hudhook's DX12 backend a half-ready init (the
/// `Initialization context incomplete` error in the logs) against a swapchain that wasn't up yet,
/// and that fragile init crashed at the first big transition (character creation / intro). Waiting
/// for the frontend means `apply()` runs once a real swapchain is presenting, so hudhook initializes
/// cleanly — and the watermark shows from the title screen, which is where a new player learns the
/// toggle key. (Rig-validated floor: installing at first *gameplay* is crash-free; if the frontend
/// init still proves fragile through character creation on the rig, bump this predicate to
/// `current().in_game()` to fall back to that proven point.) The wait + install run on their own
/// short-lived thread, like the init thread, so hudhook's `apply()` and the input-hook install stay
/// off the game's task-scheduler thread. hudhook patches the shared swapchain vtable, so hooking an
/// already-running swapchain takes effect on the next present (the standard injector path).
fn spawn_overlay(module: usize) {
    std::thread::spawn(move || {
        while !crate::playstate::current().frontend_ready() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        crate::overlay::install(module);
        // Suppress the game's own DirectInput reads while the overlay is open — it polls keyboard/
        // mouse via DirectInput, which bypasses the window message queue, so the overlay's message
        // filter alone can't stop WASD/clicks reaching the game. Degrade (overlay still draws) if it
        // fails.
        match unsafe { crate::input::install() } {
            Ok(()) => log::info!("input: hooked DirectInput GetDeviceState for overlay capture"),
            Err(e) => {
                log::error!(
                    "input: hook install failed ({e}); game input won't be suppressed while the overlay is open"
                );
                // Surface it to the player rather than failing silently (in-session problems degrade
                // + inform, per CLAUDE.md): the menu still works, but the game keeps reacting to input.
                crate::notify::with_mut(|n| {
                    n.set_banner(
                        "input-degraded",
                        unseamless_core::notifications::Severity::Warning,
                        "Heads up: the game still reacts to input while the menu is open (input hook failed)",
                    )
                });
            }
        }
    });
}

/// Apply one-shot boot-flow code patches, each gated by its own config flag: **skip-intros** (NOP
/// the conditional branch that gates the logo/splash video sequence so the game falls straight
/// through to the title), **enable-offline-multiplayer** (neutralize the game's "is offline"
/// predicate), and **force-online-menu-mode** (force `IsEnableOnlineMode()` true) — the latter two
/// both candidates for un-greying the online multiplayer items offline. See `coop/patch.rs` and
/// `docs/{SKIP-INTROS,CODE-PATCHING,OFFLINE-ITEMS-FINDINGS}.md`.
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

    if config.gameplay.enable_offline_multiplayer {
        // Re-enable the online multiplayer items offline by neutralizing the game's central
        // "is offline" predicate so it always reports *not* offline. That tiny leaf function is
        //   sub rsp,0x28 ; call <get_network_mode> ; cmp eax,2 ; sete al ; add rsp,0x28 ; ret
        // i.e. `return network_mode == 2` where the network-mode enum global is 2 when launched
        // offline / outside EAC (1 = online, 0 = initializing). ~45 sites gate online features on
        // this predicate — including multiplayer-item availability — so forcing it false ungreys the
        // items (and lets an item-use drive the session FSM). We overwrite `sete al` (0F 94 C0) with
        // `xor eax,eax ; nop` (31 C0 90) so the predicate returns 0; the dead `cmp eax,2` is left
        // in place. A miss/ambiguous/drifted scan fails safe (items stay greyed, logged), like
        // skip-intros above. Full RE write-up + risks + rig recipe: docs/OFFLINE-ITEMS-FINDINGS.md.
        //
        // Re-derive after a game update: find the unique tiny function whose body is
        // `cmp eax,2 ; sete al` bracketed by `sub rsp,0x28 ; call …` and `add rsp,0x28 ; ret`
        // (that's `is_offline()`); the bare tail alone is NOT unique, so the landmark spans the
        // whole 20-byte function (the 4 `?` are the call rel32). offset +12 is the `sete al`.
        const IS_OFFLINE_LANDMARK: &[pelite::pattern::Atom] =
            pelite::pattern!("48 83 EC 28 E8 ? ? ? ? 83 F8 02 0F 94 C0 48 83 C4 28 C3");
        crate::patch::overwrite_landmark(
            "enable_offline_multiplayer",
            IS_OFFLINE_LANDMARK,
            12,
            0x0F,
            &[0x31, 0xC0, 0x90],
        );
    }

    if config.gameplay.force_online_menu_mode {
        // EXPERIMENTAL / UNVERIFIED follow-up to enable_offline_multiplayer (which rig-proved that
        // forcing `is_offline()` false does NOT ungrey the multiplayer items — the item gate reads a
        // *different* signal). This patch tries the next candidate: force the game's
        // `IsEnableOnlineMode()` getter (reads the `Menu.IsEnableOnlineMode` config bool) to always
        // return true. The getter lazily initializes a cached bool and all its code paths converge on
        //   movzx eax, byte [<cached bool 0x144588afc>]   (0F B6 05 B1 26 73 03)
        // as the single return site, so overwriting that with `mov eax,1 ; nop ; nop` forces every
        // call to report online-mode enabled regardless of the cached value. This is the cheapest
        // end-to-end test of whether `IsEnableOnlineMode` is (part of) the real item gate — flip the
        // flag on the rig and watch whether the items ungrey. A miss/ambiguous/drifted scan fails safe
        // (no-op, logged), like the patches above. Full write-up + the rig recipe and the live address
        // to read (0x144588afc) instead of patching: docs/OFFLINE-ITEMS-FINDINGS.md.
        //
        // Site note: unlike skip-intros (whose safety rests on patching *before* its boot-logo path
        // first runs), this getter is lazily initialized and may already be live when install runs.
        // The single 7-byte overwrite is not atomic, so a concurrent execution during the write is a
        // theoretical torn read — in practice install runs very early (init thread, pre-title) and the
        // getter only inits once a menu touches online state, so the risk matches the is_offline patch
        // above. See patch.rs's "reason per-site" caveat.
        //
        // Re-derive after a game update: find the getter referenced by the UTF-16 string
        // "Menu.IsEnableOnlineMode" (the unique `lea rdx, [string]`); the function's return path ends
        // in a unique `movzx eax, byte [<cached bool>]`. Take that 7-byte movzx as the landmark
        // (offset 0, expect 0x0F) and overwrite it with `mov eax,1` padded with NOPs (B8 01 00 00 00
        // 90 90). The cached-bool disp is what makes the movzx unique.
        const IS_ENABLE_ONLINE_MODE_RET: &[pelite::pattern::Atom] =
            pelite::pattern!("0F B6 05 B1 26 73 03");
        crate::patch::overwrite_landmark(
            "force_online_menu_mode",
            IS_ENABLE_ONLINE_MODE_RET,
            0,
            0x0F,
            &[0xB8, 0x01, 0x00, 0x00, 0x00, 0x90, 0x90],
        );
    }

    if config.gameplay.bypass_session_create_gate {
        // EXPERIMENTAL / UNVERIFIED rung-3 lever. A direct call to the create-session wrapper
        // (0x140cad4c0) returns false offline and the FSM moves `lobby_state None ->
        // FailedToCreateSession` synchronously — even with `enable_offline_multiplayer` applied. Static
        // RE (docs/SESSION-DRIVE.md > "Why a direct create fails offline") found the create inner
        // (0x140cb1f70) and join inner (0x140cb2470) both call a shared, Arxan-encrypted availability
        // gate `0x140cb4b50(this)` *before* building params and bail to FailedToCreate/FailedToJoin if
        // it returns false. The gate takes only `this` and runs before `is_offline()` (which lives in
        // the params builder and only sets fields, never rejects) — which is why forcing `is_offline()`
        // false is insufficient. At the create call site the gate's bool feeds:
        //   0x140cb202b  lea  rcx, [rsp+0x30]      (48 8D 4C 24 30)
        //   0x140cb2030  test al, al              (84 C0)
        //   0x140cb2032  jne  0x140cb203b         (75 07)   <- success edge; fall-through = fail
        // Flipping that `jne` (75) to an unconditional `jmp` (EB) makes create always take the success
        // path, so the gate still *runs* (its side effects, whatever they are, are preserved) but its
        // `false` verdict no longer fails the create — control proceeds to the network-session create.
        // If the gate was the reject, the FSM now reaches `TryToCreateSession`; if the gate was
        // load-bearing for network-create readiness, the failure simply moves to the network create
        // (still `FailedToCreateSession`) — the orchestrator's re-drive + a write-watch on `[G]+0x24`
        // distinguishes these (see the doc's runtime-verify recipe). The landmark spans the gate's
        // call rel32 (`E8 26 2B 00 00`, create-specific — the join site's rel32 differs, so this stays
        // unique to create), then `nop` + the lea/test/jne; the `jne` is at offset 13. Fail-safe
        // (no-op + logged) on miss/ambiguous/drift, like the patches above.
        //
        // Re-derive after a game update: the create inner is the `mov [this+0xc], 1` function in the
        // CSSessionManager method block; the gate is the bool-returning call it makes right after the
        // `lobby_state` guards and before the params builder (0x140cb20d0). Take the call + the
        // following `nop; lea rcx,[rsp+0x30]; test al,al; jne rel8` as the landmark (the concrete call
        // rel32 keeps it create-specific) and flip the `75` at offset 13 to `EB`.
        const CREATE_GATE_CALLSITE: &[pelite::pattern::Atom] =
            pelite::pattern!("E8 26 2B 00 00 90 48 8D 4C 24 30 84 C0 75 07");
        crate::patch::overwrite_landmark(
            "bypass_session_create_gate",
            CREATE_GATE_CALLSITE,
            13,
            0x75,
            &[0xEB],
        );
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
    // Skip a feature disabled by a prior panic (read lock-free from FEATURES). An out-of-range or
    // pre-registration index reads as disabled, so we never tick something we can't account for.
    if feature_disabled(index) {
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

/// Whether the feature at `index` has been disabled by a prior panic. Lock-free (reads [`FEATURES`]),
/// so it's safe to call from inside a feature tick. An out-of-range or pre-registration index reads as
/// disabled.
fn feature_disabled(index: usize) -> bool {
    FEATURES.get().and_then(|f| f.get(index)).map(|s| s.disabled.load(Ordering::Relaxed)).unwrap_or(true)
}

/// Snapshot of each registered feature's name and whether it's been disabled (panicked), for the
/// diagnostic report + live debug panel. Reads the lock-free [`FEATURES`] registry, so — unlike a read
/// of the `APP` mutex — it stays available even when called from *inside* a feature tick (the periodic
/// snapshot + debug-panel publishers), which is what keeps the panel's `features` section live. `None`
/// only before registration completes (earliest boot).
pub fn feature_status() -> Option<Vec<(&'static str, bool)>> {
    FEATURES.get().map(|f| f.iter().map(|s| (s.name, s.disabled.load(Ordering::Relaxed))).collect())
}

/// Mark a feature as permanently disabled after its `on_frame` panicked (the panic hook already
/// logged the backtrace). Keeps one bad feature from wedging or spamming the rest. Lock-free: sets the
/// [`FEATURES`] flag without touching `APP`.
fn disable_feature(index: usize) {
    let Some(slot) = FEATURES.get().and_then(|f| f.get(index)) else { return };
    slot.disabled.store(true, Ordering::Relaxed);
    log::error!("feature '{}' (index {index}) panicked; disabled for the rest of the session", slot.name);
    // Tell the player a feature went away — the game keeps running. This is a *diagnostic* message, so
    // PLAIN voice, not ER lore (CLAUDE.md > "Surfacing errors"). Lock-safe: the panicked tick already
    // unwound its `APP` lock, and `notify` owns an independent, poison-recovering Mutex, so this can't
    // deadlock against the lock the panic released. Panic-safety is provided by the caller — the task
    // firewall wraps this whole recovery branch in its own `catch_unwind` (see `register_features`) —
    // so a (re-)panic from logging or toasting here is contained, not propagated across the boundary.
    crate::notify::with_mut(|n| {
        n.warn(format!(
            "A feature was disabled after an error; the game will continue. ({})",
            slot.name
        ))
    });
    // Clear the nameplates overlay surface. A disabled feature never publishes again, so a feature
    // that fed a *world-locked* overlay would otherwise leave its last labels frozen at stale screen
    // positions while the camera/world move under them (a visible glitch — unlike an aging toast or a
    // static debug panel). nameplates is the only such surface; clearing it here is the graceful-
    // degrade that "a panicking feature is disabled, the game keeps running" intends. Cheap + idempotent
    // for every other feature's disable, and inside the caller's firewall so a re-panic stays contained.
    crate::nameplates::publish(Vec::new());
    // Same rationale, higher stakes: if the rig-guide feature died with a CHOICE modal published, the
    // stale `Choice` view keeps the overlay's `set_blocked`/input-focus latched, stranding the game's
    // keyboard/mouse blocked (the overlay toggle can't clear it, and skip can't rescue since the dead
    // feature no longer ticks). Clear the view so a dead modal releases input focus. Debug-only (the
    // whole guide subsystem is `#[cfg(debug_assertions)]`); idempotent for non-guide feature disables.
    #[cfg(debug_assertions)]
    crate::rig_guide::publish(None);
}
