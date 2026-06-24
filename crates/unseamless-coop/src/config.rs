//! Loading the typed [`Config`] from disk at startup.
//!
//! The parsing/validation logic lives in `unseamless-core` (host-tested); this just does the
//! file I/O against the game's working directory and logs what `unseamless-core` reports.

use std::fs;
use std::path::PathBuf;

use unseamless_core::config::Config;

/// Config path relative to the game's working directory (normally `ELDEN RING/Game/`).
const CONFIG_REL: &str = "SeamlessCoop/unseamless_coop.ini";

/// Load the config, writing a default file if none exists. Always returns a usable [`Config`]
/// (defaults on any error), and logs warnings for malformed/unknown entries.
pub fn load() -> Config {
    let path = PathBuf::from(CONFIG_REL);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let (cfg, warnings) = Config::from_ini_str(&text);
            for w in &warnings {
                log::warn!("config: {w}");
            }
            log::info!("loaded config from {}", path.display());
            cfg
        }
        Err(e) => {
            log::warn!("no config at {} ({e}); using defaults", path.display());
            let cfg = Config::default();
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, cfg.to_ini_string()) {
                Ok(()) => log::info!("wrote default config to {}", path.display()),
                Err(e) => log::warn!("couldn't write default config: {e}"),
            }
            cfg
        }
    }
}
