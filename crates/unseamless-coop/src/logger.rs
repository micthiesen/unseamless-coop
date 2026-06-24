//! File logging, built for an agent to read after a session (see `unseamless-core::diagnostics`).
//!
//! Each run writes a timestamped, self-describing file under `SeamlessCoop/logs/`: a [`RunInfo`]
//! header (mod version, build profile, platform, config) then the log. Old runs are kept (last
//! [`KEEP_LOGS`]) rather than truncated, so "the one from when it broke" survives. Verbosity
//! comes from `[debug]` config — off by default, so normal play only writes milestone lines.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use simplelog::{ConfigBuilder, WriteLogger};
use unseamless_core::config::Config;
use unseamless_core::diagnostics::{RunInfo, SessionRole};

/// Where logs go, relative to the game's working directory — next to the config, easy to zip
/// and share.
const LOG_DIR: &str = "SeamlessCoop/logs";
/// How many past run logs to keep.
const KEEP_LOGS: usize = 5;

/// Build profile string — also tells a log reader whether backtraces will have symbols.
const PROFILE: &str = if cfg!(debug_assertions) {
    "diag (symbols)"
} else {
    "release (stripped)"
};

/// Initialize logging from the loaded config. Picks the level (verbose only when
/// `debug.enabled`), opens a fresh run log with the [`RunInfo`] header, prunes old logs, and
/// installs a panic hook that captures a backtrace.
pub fn init(config: &Config) {
    let level = if config.debug.enabled {
        config.debug.level.to_level_filter()
    } else {
        log::LevelFilter::Info
    };

    let run_id = run_id();
    let info = RunInfo {
        run_id: run_id.clone(),
        mod_version: env!("CARGO_PKG_VERSION").to_string(),
        build_profile: PROFILE.to_string(),
        platform: platform(),
        started_at: run_id.clone(),
        role: SessionRole::Unknown,
        session_id: None,
        config_toml: config.to_toml_string(),
    };

    // Always set the panic hook, even if file logging fails to open.
    install_panic_hook();

    let dir = PathBuf::from(LOG_DIR);
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

    let log_config = ConfigBuilder::new().set_time_format_rfc3339().build();
    let _ = WriteLogger::init(level, log_config, file);

    log::info!("logging at {level} -> {}", path.display());
    if !config.debug.enabled {
        log::info!("debug logging off; set [debug] enabled = true to capture verbose logs");
    }
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // force_capture works regardless of RUST_BACKTRACE; symbol quality depends on the build
        // profile (use `--profile diag` for readable frames).
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!("PANIC: {info}\nbacktrace:\n{backtrace}");
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
