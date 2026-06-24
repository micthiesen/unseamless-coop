//! Loading the typed [`Config`] from disk at startup.
//!
//! Parsing/validation lives in `unseamless-core` (host-tested); this does the file I/O against
//! the game's working directory. It runs *before* the logger is initialized (the logger needs
//! the config to pick its level and write the header), so instead of logging directly it returns
//! notes for [`crate::app::install`] to replay once logging is up.

use std::fs;
use std::path::PathBuf;

use log::Level;
use unseamless_core::config::Config;

/// Config path relative to the game's working directory (normally `ELDEN RING/Game/`).
const CONFIG_REL: &str = "unseamless-coop/unseamless_coop.toml";

/// A deferred log line: `(level, message)`, replayed after the logger initializes.
pub type Note = (Level, String);

/// Load the config, writing a default file if none exists. Always returns a usable [`Config`]
/// (defaults on any error) plus notes to log once the logger is up.
pub fn load() -> (Config, Vec<Note>) {
    let path = PathBuf::from(CONFIG_REL);
    let mut notes = Vec::new();

    match fs::read_to_string(&path) {
        Ok(text) => match Config::from_toml_str(&text) {
            Ok((cfg, warnings)) => {
                for w in &warnings {
                    notes.push((Level::Warn, format!("config: {w}")));
                }
                notes.push((Level::Info, format!("loaded config from {}", path.display())));
                (cfg, notes)
            }
            Err(e) => {
                notes.push((
                    Level::Error,
                    format!("config at {} is malformed ({e}); using defaults", path.display()),
                ));
                (Config::default(), notes)
            }
        },
        Err(e) => {
            notes.push((Level::Warn, format!("no config at {} ({e}); using defaults", path.display())));
            let cfg = Config::default();
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, cfg.to_toml_string()) {
                Ok(()) => notes.push((Level::Info, format!("wrote default config to {}", path.display()))),
                Err(e) => notes.push((Level::Warn, format!("couldn't write default config: {e}"))),
            }
            (cfg, notes)
        }
    }
}
