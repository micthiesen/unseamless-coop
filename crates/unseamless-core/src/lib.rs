//! Platform-independent core for **unseamless-coop**.
//!
//! Everything here is pure Rust with no dependency on the game, the `fromsoftware-rs` SDK, or
//! Windows — so it compiles and **runs its tests natively on the host** (`scripts/test-core.sh`).
//! The game-bound `unseamless-coop` cdylib crate wires this logic into the live game.
//!
//! Keeping the decision logic here (config, scaling math, the session/sync model, protocol
//! message types) means the parts most prone to subtle bugs are the parts we can actually
//! verify locally; the cdylib stays a thin, mostly-mechanical binding layer.

/// Environment variable our launcher sets before starting the game, and the cdylib's EAC guard
/// requires (absent → the mod aborts the process). Defined here, in the dependency both the
/// `launcher` and the `unseamless-coop` cdylib share, so the two can't drift out of sync — a
/// mismatch would silently abort every legitimate launch. It must only ever be set per-launch by
/// the launcher, never as a persistent user environment variable (that would disarm the guard).
pub const LAUNCH_MARKER: &str = "UNSEAMLESS_LAUNCH";

pub mod bitmap_font;
pub mod config;
pub mod crypto;
pub mod death_debuffs;
pub mod diagnostics;
pub mod framing;
pub mod game_state;
/// The in-overlay rig-testing guide engine. Debug-only (`dev`/test/`diag` profiles): the shipping
/// `release` build strips it entirely, since there is no rig in a player's hands.
#[cfg(debug_assertions)]
pub mod guide;
pub mod loader;
pub mod menu;
pub mod nameplate;
pub mod notifications;
pub mod pad;
pub mod palette;
pub mod peer;
pub mod projection;
pub mod protocol;
pub mod saves;
pub mod scaling;
pub mod settings;
pub mod transport;
pub mod ui;
pub mod util;
