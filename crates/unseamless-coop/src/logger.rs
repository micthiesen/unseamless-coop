//! File logging, built for an agent to read after a session (see `unseamless-core::diagnostics`).
//!
//! Each run writes a timestamped, self-describing file under `unseamless-coop/logs/`: a [`RunInfo`]
//! header (mod version, build profile, platform, config) then the log. Old runs are kept (last
//! [`KEEP_LOGS`]) rather than truncated, so "the one from when it broke" survives. Verbosity
//! comes from `[debug]` config — off by default, so normal play only writes milestone lines.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use simplelog::{CombinedLogger, ConfigBuilder, SharedLogger, WriteLogger};
use unseamless_core::config::Config;
use unseamless_core::diagnostics::RunInfo;

/// Where logs go, relative to the install dir — next to the config, easy to zip and share.
const LOG_DIR: &str = "unseamless-coop/logs";
/// How many past run logs to keep.
const KEEP_LOGS: usize = 5;

/// Build profile string — tells a log reader whether panic backtraces will have symbols. Keyed
/// on `debug_assertions` (on for both the `dev` and `diag` profiles, off for `release`), so it
/// names the symbol status it can actually detect rather than a specific profile it can't.
const PROFILE: &str = if cfg!(debug_assertions) {
    "debug-assertions on (symbols)"
} else {
    "release (stripped)"
};

/// A log-file writer that forces each record to disk (`sync_all`) right after writing it. A **hard**
/// crash (the native-Windows overlay DX12 access violation is one — not a Rust panic, so the panic hook
/// can't flush it) loses whatever the OS write cache hasn't persisted; without this, the log tail before
/// such a crash is lost (3 of 4 logs from the first friend test died with the tail gone — see
/// `docs/OVERLAY-RENDERING.md`). `File::flush` is a no-op (writes already go straight to the OS), so
/// durability needs the fsync. **Debug builds only** (`#[cfg(debug_assertions)]`, on for `dev`/`diag`,
/// off for `release`): a normal player's log keeps the buffered/no-fsync default with no perf cost, while
/// a diag crash-diagnosis run captures the death point. The cost is self-limiting — the overlay crash
/// fires at the *first* hooked Present (early), so only a handful of records get fsync'd before it dies,
/// not a sustained per-frame tax.
#[cfg(debug_assertions)]
struct DurableLog(File);

#[cfg(debug_assertions)]
impl Write for DurableLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.0.write(buf)?;
        let _ = self.0.sync_all(); // best-effort durability; logging must never fail loudly
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Initialize logging from the loaded config. Logs go under `<base>/unseamless-coop/logs/` (the
/// install dir, so they don't depend on the process cwd). Picks the level (verbose only when
/// `debug.enabled`), opens a fresh run log with the [`RunInfo`] header, prunes old logs, and
/// installs a panic hook that captures a backtrace.
pub fn init(config: &Config, base: &Path) {
    let level = if config.debug.enabled {
        config.debug.level.to_level_filter()
    } else {
        log::LevelFilter::Info
    };

    let run_id = run_id();
    // from_config redacts secrets internally — the header lands in a shareable log, so the type
    // makes it impossible to smuggle in an un-redacted config (and the password).
    let info = RunInfo::from_config(
        config,
        run_id.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        PROFILE.to_string(),
        // Baked by build.rs (see UNSEAMLESS_BUILD_ID there): the exact-source id, in every header.
        env!("UNSEAMLESS_BUILD_ID").to_string(),
        platform(),
        run_id.clone(),
    );

    // Always set the panic hook, even if file logging fails to open.
    install_panic_hook();

    let dir = base.join(LOG_DIR);
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("unseamless-coop: cannot create {LOG_DIR}: {e}");
        return;
    }
    prune_old_logs(&dir);

    let path = dir.join(format!("unseamless_coop-{run_id}.log"));
    let mut file = match File::create(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("unseamless-coop: cannot create log {}: {e}", path.display());
            return;
        }
    };
    let _ = file.write_all(info.header_block().as_bytes());

    // Debug builds fsync each record (see [`DurableLog`]) so a hard crash can't lose the log tail;
    // release keeps the plain buffered `File` (no per-record fsync, no perf cost).
    #[cfg(debug_assertions)]
    let file_writer = DurableLog(file);
    #[cfg(not(debug_assertions))]
    let file_writer = file;

    let log_config = ConfigBuilder::new().set_time_format_rfc3339().build();
    // Tee into three sinks at the same level (so they all agree): the shareable run file, the
    // in-memory ring buffer the overlay's Log tab reads live (`crate::logbuf`), and the co-op
    // forward queue (`crate::forward`) — inert unless we're a forwarding client, then drained onto
    // the side-channel so the host aggregates this client's log.
    // `mut` is only exercised by the debug-only push below; release strips that, so allow it there.
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut loggers: Vec<Box<dyn SharedLogger>> = vec![
        WriteLogger::new(level, log_config, file_writer),
        crate::logbuf::ring_logger(level),
        crate::forward::forward_logger(level),
    ];
    // Debug-only: tee into the rig-guide queue so a running guide's `log_contains` predicates can see
    // log output. Inert (drops every record) until a guide enables it; stripped from release entirely.
    #[cfg(debug_assertions)]
    loggers.push(crate::guide_log::guide_logger(level));
    let _ = CombinedLogger::init(loggers);

    log::info!("logging at {level} -> {}", path.display());
    if !config.debug.enabled {
        log::info!("debug logging off; set [debug] enabled = true to capture verbose logs");
    }
}

/// Emit an `error!` line from inside an FFI **recovery branch** with no chance of itself unwinding
/// across the boundary. The log sinks lock mutexes and allocate, so a poisoned/contended sink can
/// panic; in a recovery branch (a firewall's `Err` arm, running in the same foreign-invoked frame)
/// that panic would escape the very boundary the firewall just contained. Same guard the panic hook
/// uses, lifted into a helper for the firewalls in app.rs / overlay.rs / input.rs / saves.rs. Cheap:
/// one `catch_unwind` on an already-cold path.
pub fn error_contained(args: std::fmt::Arguments) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| log::error!("{args}")));
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // The hook itself must never panic: a panic-while-panicking escalates to an immediate
        // abort, which the FFI firewall's catch_unwind (app.rs) cannot intercept. Capturing a
        // backtrace, formatting, and writing to the log file all allocate and could fail, so
        // swallow any failure here — degrade to "no log line", never to an abort.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // force_capture works regardless of RUST_BACKTRACE; symbol quality depends on the
            // build profile (use `--profile diag` for readable frames).
            let backtrace = std::backtrace::Backtrace::force_capture();
            log::error!("PANIC: {info}\nbacktrace:\n{backtrace}");
        }));
    }));
}

/// Sortable run id: epoch seconds + process id (uniqueness across concurrent launches).
fn run_id() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}-{}", std::process::id())
}

fn platform() -> String {
    let base = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    // We're a Windows PE; if these Proton/Steam vars are present we're under Proton on Linux.
    let proton = std::env::var_os("STEAM_COMPAT_DATA_PATH").is_some()
        || std::env::var_os("WINEPREFIX").is_some();
    if proton {
        format!("{base} (proton)")
    } else {
        base
    }
}

/// Keep only the newest [`KEEP_LOGS`] run logs; remove the rest. Run-id filenames sort
/// chronologically, so lexical order is age order.
fn prune_old_logs(dir: &PathBuf) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("unseamless_coop-") && n.ends_with(".log"))
        })
        .collect();
    logs.sort();
    // Leave room for the new file we're about to create.
    let keep = KEEP_LOGS.saturating_sub(1);
    if logs.len() > keep {
        for old in &logs[..logs.len() - keep] {
            let _ = fs::remove_file(old);
        }
    }
}
