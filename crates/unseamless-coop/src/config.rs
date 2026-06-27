//! Loading the typed [`Config`] from disk at startup.
//!
//! Parsing/validation lives in `unseamless-core` (host-tested); this does the file I/O against
//! the game's working directory. It runs *before* the logger is initialized (the logger needs
//! the config to pick its level and write the header), so instead of logging directly it returns
//! notes for [`crate::app::install`] to replay once logging is up.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use log::Level;
use unseamless_core::config::{Config, DEFAULT_PASSWORD_LEN, generate_password};
use unseamless_core::protocol::{AUTH_NONCE_LEN, AuthNonce};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};

/// Config path relative to the install dir (the folder our DLL lives in).
const CONFIG_REL: &str = "unseamless-coop/unseamless_coop.toml";

/// A deferred log line: `(level, message)`, replayed after the logger initializes.
pub type Note = (Level, String);

/// Load the config from `<base>/unseamless-coop/unseamless_coop.toml`, writing a default file if
/// none exists. `base` is the install dir (our DLL's folder), so config is found regardless of the
/// process working directory. Always returns a usable [`Config`] (defaults on any error) plus notes
/// to log once the logger is up.
pub fn load(base: &Path) -> (Config, Vec<Note>) {
    let path = base.join(CONFIG_REL);
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
            // Fresh install: seed a unique random session password so two unrelated installs that
            // never set one don't accidentally share a session (everyone in a party then sets the
            // same value). Generated here because core has no entropy source.
            let mut cfg = Config::default();
            cfg.session.password = generate_password(&random_bytes(DEFAULT_PASSWORD_LEN));
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, cfg.to_toml_string()) {
                Ok(()) => {
                    notes.push((Level::Info, format!("wrote default config to {}", path.display())));
                    // Never log the value — it lands in shareable logs. Point at the file instead.
                    notes.push((
                        Level::Info,
                        "generated a random co-op password; share it with friends (it's in the config file)".into(),
                    ));
                }
                Err(e) => notes.push((Level::Warn, format!("couldn't write default config: {e}"))),
            }
            (cfg, notes)
        }
    }
}

/// Cryptographically-random bytes from the OS CSPRNG (for the generated default password). Falls
/// back to a weak time/pid-derived seed only if BCrypt somehow fails, so we never end up writing an
/// empty password.
/// A fresh per-session handshake nonce from the OS CSPRNG — the same entropy source as the default
/// password. The peer auth proof ([`unseamless_core::peer`]) binds to this nonce, so its **freshness**
/// is what makes a captured `Auth` non-replayable; generate a new one for each session (every
/// `Peer::new`), never reuse one. Core can't do this (no entropy source), so the binding layer supplies it.
pub(crate) fn fresh_auth_nonce() -> AuthNonce {
    random_bytes(AUTH_NONCE_LEN)
        .try_into()
        .expect("random_bytes(AUTH_NONCE_LEN) returns exactly AUTH_NONCE_LEN bytes")
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    // No algorithm handle: the system-preferred-RNG flag selects the OS CSPRNG.
    let status = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_ok() {
        return buf;
    }
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1)
        ^ (std::process::id() as u128);
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (seed >> ((i % 16) * 8)) as u8 ^ (i as u8).wrapping_mul(31);
    }
    buf
}
