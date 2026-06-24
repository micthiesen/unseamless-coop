//! Diagnostics: the model behind the shareable debug log.
//!
//! ## Primary consumer: a debugging *agent*, not a human tail-ing a console
//! After a play session with friends, the user points an assistant at a log folder/file and
//! asks "what happened?". So a log must be **self-describing and correlatable**: each file
//! starts with a [`RunInfo`] header naming the machine's mod version, build, role, the shared
//! session id, and the full config — so the reader never has to ask "which machine / version /
//! settings / session". Logs from different machines in one session share a `session_id`, so
//! they can be lined up. [`LogBundle`] merges forwarded records into one per-peer artifact when
//! log-forwarding is on (rig-gated wiring); without it, each machine's self-identifying file is
//! collected and read directly.
//!
//! This module is pure (the timestamp/run-id/platform strings are supplied by the cdylib, which
//! has the clock/OS); it's fully host-tested.

use serde::{Deserialize, Serialize};

/// Verbosity, mirroring `log`'s levels but serde-friendly and stable on the wire.
///
/// `#[repr(u8)]` with explicit discriminants pins the wire byte ([`to_u8`](LogLevel::to_u8) is
/// `self as u8`): reordering or inserting variants is then a visible, reviewable change instead
/// of a silent wire shift. Keep the values fixed and append new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl LogLevel {
    pub fn to_level_filter(self) -> log::LevelFilter {
        match self {
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Trace => log::LevelFilter::Trace,
        }
    }

    pub fn from_log_level(level: log::Level) -> Self {
        match level {
            log::Level::Error => LogLevel::Error,
            log::Level::Warn => LogLevel::Warn,
            log::Level::Info => LogLevel::Info,
            log::Level::Debug => LogLevel::Debug,
            log::Level::Trace => LogLevel::Trace,
        }
    }

    /// Stable wire byte.
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => LogLevel::Error,
            1 => LogLevel::Warn,
            2 => LogLevel::Info,
            3 => LogLevel::Debug,
            4 => LogLevel::Trace,
            _ => return None,
        })
    }
}

/// This machine's role in the session, for the log header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    Unknown,
    Solo,
    Host,
    Client,
}

impl SessionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionRole::Unknown => "unknown",
            SessionRole::Solo => "solo",
            SessionRole::Host => "host",
            SessionRole::Client => "client",
        }
    }
}

/// Self-describing header written at the top of every log file. Everything an agent needs to
/// interpret the rest of the log without asking the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInfo {
    /// Unique per process launch (the cdylib generates it, e.g. timestamp+pid).
    pub run_id: String,
    pub mod_version: String,
    /// Build profile string, e.g. `"release (stripped)"` or `"diag (symbols)"` — tells the
    /// reader whether panic backtraces in this log will have symbols.
    pub build_profile: String,
    /// e.g. `"windows-x86_64 (proton)"`.
    pub platform: String,
    /// Human/sortable start time supplied by the cdylib (core has no clock).
    pub started_at: String,
    pub role: SessionRole,
    /// Shared across all machines in one session once joined; lets logs be correlated.
    pub session_id: Option<String>,
    /// The full effective config (TOML), so settings are never in question.
    pub config_toml: String,
}

impl RunInfo {
    /// Render the header block that prefixes a log file. Delimited and greppable.
    pub fn header_block(&self) -> String {
        let session = self.session_id.as_deref().unwrap_or("(not joined)");
        format!(
            "==== unseamless-coop run ====\n\
             run_id      = {}\n\
             mod_version = {}\n\
             build       = {}\n\
             platform    = {}\n\
             started_at  = {}\n\
             role        = {}\n\
             session_id  = {}\n\
             ---- config ----\n\
             {}\n\
             ==== begin log ====\n",
            self.run_id,
            self.mod_version,
            self.build_profile,
            self.platform,
            self.started_at,
            self.role.as_str(),
            session,
            self.config_toml.trim_end(),
        )
    }
}

/// One forwarded log line. The sender's identity is known from the packet, so it isn't carried
/// here; the host attaches a peer label at aggregation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// Per-sender monotonic sequence, for ordering within a peer's stream.
    pub seq: u32,
    pub level: LogLevel,
    pub message: String,
}

/// Aggregates forwarded [`LogRecord`]s from multiple peers into one artifact (used by the host
/// when log-forwarding is enabled). Without forwarding, each machine's file is read directly;
/// this is the "everything in one place" convenience.
#[derive(Debug, Default)]
pub struct LogBundle {
    entries: Vec<(String, LogRecord)>,
}

impl LogBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, peer: impl Into<String>, record: LogRecord) {
        self.entries.push((peer.into(), record));
    }

    /// Render grouped by peer, each peer's lines ordered by `seq`. Stable and easy to scan.
    pub fn render(&self) -> String {
        // Collect distinct peers in first-seen order.
        let mut peers: Vec<&str> = Vec::new();
        for (peer, _) in &self.entries {
            if !peers.contains(&peer.as_str()) {
                peers.push(peer);
            }
        }

        let mut out = String::new();
        for peer in peers {
            out.push_str(&format!("---- peer: {peer} ----\n"));
            let mut records: Vec<&LogRecord> = self
                .entries
                .iter()
                .filter(|(p, _)| p == peer)
                .map(|(_, r)| r)
                .collect();
            records.sort_by_key(|r| r.seq);
            for r in records {
                out.push_str(&format!(
                    "[{:>5}] {:<5} {}\n",
                    r.seq,
                    level_tag(r.level),
                    r.message
                ));
            }
        }
        out
    }
}

/// A short, stable, non-reversible tag for a peer's Steam ID, for use in **shareable** logs.
///
/// A raw 64-bit SteamID resolves directly to a person's Steam profile, so writing other players'
/// IDs into a log that gets handed to a host or an assistant leaks their identity. This hashes
/// the ID to a 16-bit tag (`peer-XXXX`) that's stable within and across a session's logs — enough
/// to correlate "the same player" across machines — without disclosing who they are. (FNV-1a;
/// not a security primitive, just identity-obscuring for logs.)
pub fn peer_tag(steam_id: u64) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in steam_id.to_le_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("peer-{:04x}", (hash & 0xffff) as u16)
}

fn level_tag(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "ERROR",
        LogLevel::Warn => "WARN",
        LogLevel::Info => "INFO",
        LogLevel::Debug => "DEBUG",
        LogLevel::Trace => "TRACE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_block_contains_everything_an_agent_needs() {
        let info = RunInfo {
            run_id: "20260624-100000-1234".into(),
            mod_version: "0.1.0".into(),
            build_profile: "diag".into(),
            platform: "windows-x86_64 (proton)".into(),
            started_at: "2026-06-24T10:00:00Z".into(),
            role: SessionRole::Host,
            session_id: Some("abc123".into()),
            config_toml: "[scaling]\nenemy_health = 35\n".into(),
        };
        let header = info.header_block();
        for needle in [
            "run_id      = 20260624-100000-1234",
            "mod_version = 0.1.0",
            "build       = diag",
            "role        = host",
            "session_id  = abc123",
            "enemy_health = 35",
            "==== begin log ====",
        ] {
            assert!(header.contains(needle), "header missing {needle:?}:\n{header}");
        }
    }

    #[test]
    fn header_marks_unjoined_session() {
        let info = RunInfo {
            run_id: "r".into(),
            mod_version: "0.1.0".into(),
            build_profile: "release".into(),
            platform: "p".into(),
            started_at: "t".into(),
            role: SessionRole::Unknown,
            session_id: None,
            config_toml: String::new(),
        };
        assert!(info.header_block().contains("session_id  = (not joined)"));
    }

    #[test]
    fn peer_tag_is_stable_and_hides_the_raw_id() {
        let id = 0x1100_0011_4514_1919u64;
        let tag = peer_tag(id);
        assert_eq!(tag, peer_tag(id), "must be deterministic for correlation");
        assert!(tag.starts_with("peer-") && tag.len() == 9);
        assert!(!tag.contains(&format!("{id:x}")), "must not embed the raw id");
        assert_ne!(peer_tag(id), peer_tag(id + 1), "distinct ids should usually differ");
    }

    #[test]
    fn level_round_trips_and_orders() {
        for l in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            assert_eq!(LogLevel::from_u8(l.to_u8()), Some(l));
        }
        assert_eq!(LogLevel::from_u8(99), None);
        assert!(LogLevel::Error < LogLevel::Trace);
    }

    #[test]
    fn bundle_groups_by_peer_and_orders_by_seq() {
        let mut b = LogBundle::new();
        b.add("alice", LogRecord { seq: 2, level: LogLevel::Info, message: "second".into() });
        b.add("bob", LogRecord { seq: 1, level: LogLevel::Warn, message: "bob-first".into() });
        b.add("alice", LogRecord { seq: 1, level: LogLevel::Info, message: "first".into() });
        let out = b.render();

        // alice seen first; her lines ordered first<second despite insertion order.
        let alice = out.find("peer: alice").unwrap();
        let bob = out.find("peer: bob").unwrap();
        assert!(alice < bob);
        let first = out.find("first").unwrap();
        let second = out.find("second").unwrap();
        assert!(first < second);
        assert!(out.contains("[    1] INFO  first"));
        assert!(out.contains("[    1] WARN  bob-first"));
    }
}
