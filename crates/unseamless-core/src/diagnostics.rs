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

use std::collections::VecDeque;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};

/// Verbosity, mirroring `log`'s levels but serde-friendly and stable on the wire.
///
/// `#[repr(u8)]` with explicit discriminants pins the wire byte, and the conversions are
/// **derived** (`num_enum`): `u8::from(level)` encodes, `LogLevel::try_from(byte)` decodes, so the
/// two can't drift. Keep the values fixed and append new ones.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, IntoPrimitive, TryFromPrimitive,
)]
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
    /// Short build id (`<short-sha>` or `<short-sha>-dirty`), baked at compile time by the cdylib's
    /// build script. The version is the *release* identity; this is the *exact source* identity, so
    /// two friends on the same version but different builds are distinguishable, and a `-dirty` log
    /// is immediately flagged as an uncommitted build.
    pub build_id: String,
    /// e.g. `"windows-x86_64 (proton)"`.
    pub platform: String,
    /// Human/sortable start time supplied by the cdylib (core has no clock).
    pub started_at: String,
    pub role: SessionRole,
    /// Shared across all machines in one session once joined; lets logs be correlated.
    pub session_id: Option<String>,
    /// The effective config as TOML, **with secrets redacted** (see [`RunInfo::from_config`]).
    /// Private so the only way to populate it is the redacting constructor — a struct literal
    /// can't smuggle in an un-redacted config and leak the password into a shareable log.
    config_toml: String,
}

impl RunInfo {
    /// Build a header from the live config, **redacting secrets** (the session password) so the
    /// shareable log never carries them. The cdylib supplies the clock/OS/build strings (core has
    /// no clock); `role`/`session_id` default to unknown and may be set afterward.
    pub fn from_config(
        config: &crate::config::Config,
        run_id: String,
        mod_version: String,
        build_profile: String,
        build_id: String,
        platform: String,
        started_at: String,
    ) -> Self {
        Self {
            run_id,
            mod_version,
            build_profile,
            build_id,
            platform,
            started_at,
            role: SessionRole::Unknown,
            session_id: None,
            config_toml: config.to_redacted_toml_string(),
        }
    }

    /// Render the header block that prefixes a log file. Delimited and greppable.
    pub fn header_block(&self) -> String {
        let session = self.session_id.as_deref().unwrap_or("(not joined)");
        format!(
            "==== unseamless-coop run ====\n\
             run_id      = {}\n\
             mod_version = {}\n\
             build       = {}\n\
             build_id    = {}\n\
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
            self.build_id,
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
    // VecDeque so drop-oldest is O(1) — the cap defends against a flood, so eviction must not
    // itself be O(n) per add (which `Vec::remove(0)` would be).
    entries: VecDeque<(String, LogRecord)>,
}

/// Cap on retained records, so a peer flooding forwarded `Log` frames can't grow the bundle
/// (or its render cost) without bound. Oldest records are dropped first.
const MAX_BUNDLE_ENTRIES: usize = 50_000;

impl LogBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, peer: impl Into<String>, record: LogRecord) {
        if self.entries.len() >= MAX_BUNDLE_ENTRIES {
            self.entries.pop_front(); // O(1) drop-oldest; bounds memory against a hostile flood
        }
        self.entries.push_back((peer.into(), record));
    }

    /// Number of retained records (bounded by [`MAX_BUNDLE_ENTRIES`]).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
                // Indent continuation lines so a multi-line message (e.g. a forwarded backtrace)
                // stays visibly attributed to this peer and can't masquerade as a new peer header.
                let message = r.message.replace('\n', "\n        ");
                out.push_str(&format!("[{:>5}] {:<5} {}\n", r.seq, level_tag(r.level), message));
            }
        }
        out
    }
}

/// A short, stable, non-reversible tag for a peer's Steam ID, for use in **shareable** logs.
///
/// A raw 64-bit SteamID resolves directly to a person's Steam profile, so writing other players'
/// IDs into a log that gets handed to a host or an assistant leaks their identity. This hashes
/// the ID to a 32-bit tag (`peer-XXXXXXXX`) that's stable within and across a session's logs —
/// enough to correlate "the same player" across machines — without disclosing who they are. The
/// 32-bit space keeps collisions negligible for realistic party sizes (two distinct players
/// sharing a tag is possible but vanishingly unlikely). FNV-1a; not a security primitive, just
/// identity-obscuring for logs.
pub fn peer_tag(steam_id: u64) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in steam_id.to_le_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("peer-{:08x}", (hash & 0xffff_ffff) as u32)
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
            build_id: "a1b2c3d-dirty".into(),
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
            "build_id    = a1b2c3d-dirty",
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
            build_id: "nogit".into(),
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
        assert!(tag.starts_with("peer-") && tag.len() == 13, "32-bit tag: peer-XXXXXXXX");
        assert!(!tag.contains(&format!("{id:x}")), "must not embed the raw id");
        assert_ne!(peer_tag(id), peer_tag(id + 1), "distinct ids should usually differ");
    }

    #[test]
    fn from_config_redacts_password_in_the_header() {
        let mut cfg = crate::config::Config::default();
        cfg.session.password = "topsecret".into();
        let info = RunInfo::from_config(
            &cfg,
            "rid".into(),
            "0.1.0".into(),
            "release (stripped)".into(),
            "nogit".into(),
            "p".into(),
            "t".into(),
        );
        let header = info.header_block();
        assert!(!header.contains("topsecret"), "password leaked into header:\n{header}");
        assert!(header.contains("<redacted>"));
    }

    #[test]
    fn loglevel_serde_strings_round_trip() {
        // The serde (TOML) encoding is a *second* wire surface beside the protocol byte; pin it so
        // a variant rename is caught as the breaking config change it is.
        for (level, expected) in [
            (LogLevel::Error, "\"error\""),
            (LogLevel::Warn, "\"warn\""),
            (LogLevel::Info, "\"info\""),
            (LogLevel::Debug, "\"debug\""),
            (LogLevel::Trace, "\"trace\""),
        ] {
            let json_like = toml::to_string(&Wrap { level }).unwrap();
            assert!(json_like.contains(expected), "{level:?} -> {json_like}");
            let back: Wrap = toml::from_str(&json_like).unwrap();
            assert_eq!(back.level, level);
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrap {
        level: LogLevel,
    }

    #[test]
    fn bundle_caps_at_max_dropping_oldest() {
        let mut b = LogBundle::new();
        let overflow = 5;
        for seq in 0..(MAX_BUNDLE_ENTRIES + overflow) as u32 {
            b.add("p", LogRecord { seq, level: LogLevel::Info, message: String::new() });
        }
        assert_eq!(b.len(), MAX_BUNDLE_ENTRIES, "retained count is capped");
        // The oldest `overflow` records (seq 0..5) were evicted; the window is seq 5.. .
        let rendered = b.render();
        assert!(!rendered.contains("[    0]"), "oldest record dropped");
        assert!(rendered.contains(&format!("[{:>5}]", MAX_BUNDLE_ENTRIES + overflow - 1)), "newest kept");
    }

    #[test]
    fn render_indents_multi_line_messages_under_their_peer() {
        let mut b = LogBundle::new();
        b.add("peer-0001", LogRecord { seq: 1, level: LogLevel::Error, message: "boom\nat foo".into() });
        let out = b.render();
        // The second line must be indented (not flush-left, where it could look like a new header).
        assert!(out.contains("boom\n        at foo"), "continuation not indented:\n{out}");
        assert_eq!(out.matches("---- peer:").count(), 1);
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
            // Derived (num_enum) wire conversions: u8::from / try_from.
            assert_eq!(LogLevel::try_from(u8::from(l)).ok(), Some(l));
        }
        assert!(LogLevel::try_from(99u8).is_err());
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
