//! Platform-independent core for **unseamless-coop**.
//!
//! Everything here is pure Rust with no dependency on the game, the `fromsoftware-rs` SDK, or
//! Windows — so it compiles and **runs its tests on the macOS dev host** (`scripts/test-core.sh`).
//! The game-bound `unseamless-coop` cdylib crate wires this logic into the live game.
//!
//! Keeping the decision logic here (config, scaling math, the session/sync model, protocol
//! message types) means the parts most prone to subtle bugs are the parts we can actually
//! verify locally; the cdylib stays a thin, mostly-mechanical binding layer.

pub mod config;
pub mod protocol;
pub mod scaling;
