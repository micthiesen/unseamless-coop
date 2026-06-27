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
use std::time::Duration;

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

/// A structured, self-describing **runtime snapshot**, rendered as a delimited, greppable block in
/// the log. Where [`RunInfo`] is the boot-time header (version/build/config), a report captures the
/// *live* state at a moment — session FSM, roster, feature health — so a shared log answers "what
/// was actually happening when X went wrong" without the user/agent having to ask. Built on demand
/// (boot, a session change, a periodic probe tick, a feature panic) by the cdylib, which fills the
/// sections from live game state; this is the pure model + renderer, host-tested.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiagnosticReport {
    title: String,
    sections: Vec<ReportSection>,
}

/// One section of a [`DiagnosticReport`] — a titled, ordered list of `key = value` lines. Returned by
/// [`DiagnosticReport::section`] for chained [`field`](ReportSection::field) calls; fields are private
/// so it's append-only.
///
/// A section may also carry a shorter **summary** (via [`summary_line`](ReportSection::summary_line)) —
/// a condensed rollup of its full `fields` for a space-constrained renderer. The log [`render`] and the
/// overlay's detail pane always use the full `fields`; the overlay's concise panel prefers `summary`
/// when present (see [`has_summary`](ReportSection::has_summary)). A section with no summary is shown in
/// full everywhere — summaries are opt-in, set only on the verbose sections worth condensing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSection {
    title: String,
    fields: Vec<(String, String)>,
    summary: Vec<(String, String)>,
}

impl DiagnosticReport {
    pub fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), sections: Vec::new() }
    }

    /// Start a new section and return it for chained [`field`](ReportSection::field) calls.
    pub fn section(&mut self, title: impl Into<String>) -> &mut ReportSection {
        self.sections.push(ReportSection {
            title: title.into(),
            fields: Vec::new(),
            summary: Vec::new(),
        });
        self.sections.last_mut().expect("just pushed")
    }

    /// Render the delimited block. Keys are column-aligned within each section for scanability, and
    /// the markers (`==== … diagnostic`, `---- section ----`, `==== end …`) match [`RunInfo`]'s style
    /// so the same greps find both.
    pub fn render(&self) -> String {
        let mut out = format!("==== unseamless-coop diagnostic: {} ====\n", self.title);
        for section in &self.sections {
            out.push_str(&format!("---- {} ----\n", section.title));
            let width = section.fields.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
            for (k, v) in &section.fields {
                // Indent continuation lines of a multi-line value so they stay under their key.
                let v = v.replace('\n', "\n  ");
                out.push_str(&format!("{k:<width$} = {v}\n"));
            }
        }
        out.push_str("==== end diagnostic ====\n");
        out
    }

    /// The report's sections, in order, for a structured (non-text) renderer like the overlay's live
    /// debug panel (which draws the report itself rather than the delimited text [`render`] produces).
    pub fn sections(&self) -> &[ReportSection] {
        &self.sections
    }
}

impl ReportSection {
    /// Add a `key = value` line (any `Display` value). Returns `&mut self` for chaining.
    pub fn field(&mut self, key: impl Into<String>, value: impl std::fmt::Display) -> &mut Self {
        self.fields.push((key.into(), value.to_string()));
        self
    }

    /// The section title — for a structured renderer (the debug panel draws it as a header).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The section's `(key, value)` lines, in order — for a structured renderer like the debug panel.
    pub fn fields(&self) -> &[(String, String)] {
        &self.fields
    }

    /// Add a condensed `key = value` line to the section's **summary** (the rollup a space-constrained
    /// renderer shows in place of the full `fields`). Returns `&mut self` for chaining. Set only on
    /// verbose sections worth condensing; leaving it empty means the section renders in full everywhere.
    pub fn summary_line(&mut self, key: impl Into<String>, value: impl std::fmt::Display) -> &mut Self {
        self.summary.push((key.into(), value.to_string()));
        self
    }

    /// The section's condensed summary lines, in order — empty when the section has no summary (then the
    /// full [`fields`](ReportSection::fields) are the only representation). See [`has_summary`].
    pub fn summary(&self) -> &[(String, String)] {
        &self.summary
    }

    /// Whether this section carries a separate condensed summary (so a concise renderer should prefer it
    /// over the full `fields`).
    pub fn has_summary(&self) -> bool {
        !self.summary.is_empty()
    }
}

/// Tracks a contiguous range of boolean signals (event flags) and reports the ones that **changed**
/// between successive snapshots — the reusable "which flag flips when I do X in-game" finder (e.g.
/// resting at a Site of Grace, to locate the death-debuff cure flag). Pure: the cdylib reads the
/// flags from `CSEventFlagMan` and feeds them here; this owns the prior-state diff and noise control.
#[derive(Debug, Clone)]
pub struct FlagScanner {
    start: u32,
    prev: Vec<bool>,
}

impl FlagScanner {
    /// Watch `count` flags starting at flag id `start`, all assumed initially `false`.
    pub fn new(start: u32, count: usize) -> Self {
        Self { start, prev: vec![false; count] }
    }

    /// Number of flags watched.
    pub fn len(&self) -> usize {
        self.prev.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prev.is_empty()
    }

    /// Feed the current state of the watched flags, in id order from `start`. Returns `(flag_id,
    /// now_on)` for each flag whose value differs from the previous call, and records the new state.
    /// A `current` shorter or longer than the watched range only compares the overlapping prefix
    /// (defensive — never panics on a length mismatch).
    pub fn changes(&mut self, current: &[bool]) -> Vec<(u32, bool)> {
        let mut out = Vec::new();
        // `zip` stops at the shorter of the two, so a length mismatch only compares the overlap.
        for (i, (&cur, prev)) in current.iter().zip(self.prev.iter_mut()).enumerate() {
            if cur != *prev {
                *prev = cur;
                // saturating: `start` is unbounded config; a near-u32::MAX start must not overflow.
                out.push((self.start.saturating_add(i as u32), cur));
            }
        }
        out
    }
}

/// Protocol-version verdict for the [`ConnectReport`], decided once the handshake lands. Distinct from
/// "not reached" so a stuck link reads as *version mismatch* rather than a network problem at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VersionCheck {
    /// Handshake not reached yet — we haven't heard the partner's `Hello`, so there's nothing to check.
    #[default]
    Unknown,
    /// Majors compatible (the partner speaks a protocol we can talk to).
    Match,
    /// Majors differ — the link can establish but the side-channel won't agree; surfaced explicitly so
    /// this isn't mistaken for a NAT/receive failure.
    Mismatch,
}

impl VersionCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionCheck::Unknown => "unknown (handshake not reached)",
            VersionCheck::Match => "match",
            VersionCheck::Mismatch => "MISMATCH",
        }
    }
}

/// Which side of a rung-4 lobby-discovery attempt this machine played, for the [`ConnectReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyRole {
    /// We created the lobby and published the password data (the joiner finds us).
    Host,
    /// We filtered the lobby list by the password and joined a match.
    Joiner,
}

impl LobbyRole {
    pub fn as_str(self) -> &'static str {
        match self {
            LobbyRole::Host => "host (created the lobby)",
            LobbyRole::Joiner => "joiner (found + joined)",
        }
    }
}

/// Rung-4 lobby-discovery progress for a connection attempt (the [`ConnectReport`]'s `lobby` is
/// `None` only before any attempt this session). The joiner's `candidates` is the key
/// signal for an **empty-lobby-filter** failure: `Some(0)` means the password filter matched no lobby
/// (wrong password / host not up / different version tag), as distinct from "the list never returned".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyProgress {
    pub role: LobbyRole,
    /// Host: when `LobbyCreated_t` reported success (lobby exists, data published).
    pub created_at: Option<Duration>,
    /// Joiner: when `LobbyMatchList_t` returned (the filtered list came back).
    pub list_returned_at: Option<Duration>,
    /// Joiner: how many lobbies matched the password filter. `Some(0)` is the empty-filter case.
    pub candidates: Option<u32>,
    /// Joiner: when `LobbyEnter_t` reported we were in the lobby.
    pub joined_at: Option<Duration>,
    /// Whether the host's SteamID was read out of the lobby (the value that seeds rung 2).
    pub host_id_resolved: bool,
}

impl LobbyProgress {
    pub fn new(role: LobbyRole) -> Self {
        Self {
            role,
            created_at: None,
            list_returned_at: None,
            candidates: None,
            joined_at: None,
            host_id_resolved: false,
        }
    }
}

/// Per-stage, timestamped record of a single co-op **connection attempt** — the "connect report".
///
/// A coarse phase atomic (off / linking / linked / lost) can't say *why* an attempt is stuck: a link
/// frozen at "linking" looks the same whether it's one-way NAT, a total receive failure, a protocol
/// version mismatch, or (rung 4) an empty lobby filter. This captures each stage and its timing so a
/// **single shared log** from a failed two-player test is fully diagnosable without a second run:
///
/// - `self_id` resolved? (rung 1 — if not, nothing downstream can run)
/// - `session_accepted`? (we proactively `AcceptSessionWithUser`'d the known peer)
/// - `messages` sent vs received — **the** NAT discriminator: `sent > 0, received = 0` is one-way.
/// - `handshake` reached? (the partner's `Hello` landed)
/// - `version` match vs mismatch
/// - (rung 4) `lobby` created / found / joined
///
/// **No raw SteamIDs live here** — identities are surfaced separately via [`peer_tag`] in the rung-2
/// `coop` status line, so this whole block is safe to share publicly. Pure model + renderer; the cdylib
/// fills it from the live driver and stamps the timings (core has no clock).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectReport {
    /// When our own SteamID resolved (rung 1). `None` = never (the side-channel can't start).
    pub self_id_at: Option<Duration>,
    /// When we proactively `AcceptSessionWithUser`'d the known peer (rung 2).
    pub session_accepted_at: Option<Duration>,
    /// Side-channel frames Steam accepted from us for delivery.
    pub messages_sent: u64,
    /// Side-channel frames received on our channel (counted **before** the peer-trust filter, so this
    /// answers "did *any* P2P traffic arrive" — the NAT/auth question — not "did the right peer reply").
    pub messages_received: u64,
    /// When we first heard the partner (their `Hello` — the handshake landed). `None` = never linked.
    pub handshake_at: Option<Duration>,
    /// Protocol-version verdict, once the handshake lands.
    pub version: VersionCheck,
    /// Rung-4 lobby discovery, if that path drove this attempt (else `None` — manual peer entry).
    pub lobby: Option<LobbyProgress>,
    /// Terminal failure reason in plain words, set when a stage gives up — the "why" the phase atomic
    /// can't carry. `None` while healthy or still trying.
    pub failure: Option<String>,
}

impl ConnectReport {
    /// A fresh, empty report (const so it can seed a `static` directly).
    pub const fn new() -> Self {
        Self {
            self_id_at: None,
            session_accepted_at: None,
            messages_sent: 0,
            messages_received: 0,
            handshake_at: None,
            version: VersionCheck::Unknown,
            lobby: None,
            failure: None,
        }
    }

    /// Render as ordered `(key, value)` diagnostic lines for a report section — the per-stage connect
    /// picture. The fixed `self_id / session_accepted / messages / handshake / version` lines always
    /// appear; the `lobby_*` lines only when rung-4 discovery drove the attempt; `failure` only when set.
    pub fn fields(&self) -> Vec<(String, String)> {
        let mut f = vec![
            ("self_id".to_string(), stamp(self.self_id_at, "resolved", "not resolved")),
            ("session_accepted".to_string(), stamp(self.session_accepted_at, "yes", "no")),
            ("messages".to_string(), format!("sent {} / received {}", self.messages_sent, self.messages_received)),
            ("handshake".to_string(), stamp(self.handshake_at, "reached", "not reached")),
            ("version".to_string(), self.version.as_str().to_string()),
        ];
        if let Some(l) = &self.lobby {
            f.push(("lobby_role".to_string(), l.role.as_str().to_string()));
            match l.role {
                LobbyRole::Host => {
                    f.push(("lobby_created".to_string(), stamp(l.created_at, "yes", "no")));
                }
                LobbyRole::Joiner => {
                    let list = match l.candidates {
                        Some(0) => "0 matching (empty filter)".to_string(),
                        Some(n) => format!("{n} matching"),
                        None => "not returned".to_string(),
                    };
                    f.push(("lobby_list".to_string(), list));
                    f.push(("lobby_joined".to_string(), stamp(l.joined_at, "yes", "no")));
                }
            }
            f.push((
                "host_id".to_string(),
                if l.host_id_resolved { "resolved" } else { "not resolved" }.to_string(),
            ));
        }
        if let Some(why) = &self.failure {
            f.push(("failure".to_string(), why.clone()));
        }
        f
    }
}

/// Format a stage timestamp: `"<yes> (+1.23s)"` once reached, else the bare `<no>` label. The `+Ns` is
/// elapsed since the attempt's epoch (the cdylib supplies it; core has no clock), so stages line up in
/// time within one report.
fn stamp(at: Option<Duration>, yes: &str, no: &str) -> String {
    match at {
        Some(d) => format!("{yes} (+{:.2}s)", d.as_secs_f32()),
        None => no.to_string(),
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

/// The rung-4 lobby-discovery **password token** — the value a host publishes as the `usc_pw` lobby
/// datum and a joiner filters the lobby list by. This is the cross-implementation **contract**: the DLL
/// hand-bind ([`crate`]'s sibling `coop/steam.rs`) and the `harness` lobby prototype must produce the
/// **byte-identical** token or two players with the same password never find each other.
///
/// `token = lowercase_hex( SHA-256("unseamless-coop/lobby-discovery/v1\0" || password_bytes)[0..16] )`
///
/// Load-bearing details, each one a silent-discovery-break if violated:
/// - the domain-separator prefix ends with a **literal NUL** (`\0`) before the password bytes;
/// - the password is hashed **verbatim** — the caller must pass the raw configured bytes with **no**
///   trim, case-fold, or Unicode normalization (a stray normalize in the config layer breaks this);
/// - only the **first 16 bytes** of the digest are taken, rendered **lowercase** hex (32 chars).
///
/// SHA-256 here is for a stable, well-specified, collision-resistant keying of a shared secret into a
/// public lobby field — not a confidentiality primitive (lobby data is world-readable). Pinned by the
/// known-answer test below; the harness carries the matching KAT.
pub fn lobby_discovery_token(password: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"unseamless-coop/lobby-discovery/v1\0");
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();
    // First 16 bytes, lowercase hex (no external hex crate — format each byte).
    let mut token = String::with_capacity(32);
    for byte in &digest[..16] {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

/// Rewrite every raw 64-bit SteamID in `text` to its stable [`peer_tag`], so a captured bundle is
/// safe to share publicly — a raw SteamID64 resolves straight to a person's Steam profile.
///
/// Matches a **standalone run of exactly 17 ASCII digits starting `76561`** — the shape of an
/// individual SteamID64 (`76561197960265728 + account_id`), which is how the ID appears in our log
/// lines and diagnostic report (e.g. the `own_id` field). Bounded by non-digits on both sides so it
/// never rewrites a 17-digit slice of a longer number, and other numerics (counts, timestamps,
/// multipliers) are left untouched. Conservative by construction: a non-SteamID that happened to be
/// 17 digits *and* started `76561` is astronomically unlikely, and the worst case is one stray number
/// turned into a harmless tag.
///
/// This matches only the **full 17-digit decimal** form, which is how every Steam id currently reaches
/// a log/report (see `coop/diag.rs`'s `own_id`). It does NOT catch a Steam3 textual id (`[U:1:<acct>]`)
/// or a bare 32-bit `account_id` (trivially `+ 76561197960265728` back to the full id). When the
/// rig-gated co-op layer starts logging *peer* ids, emit them in full-decimal form (or extend this) so
/// they stay scrubbed in a "safe to share" bundle.
pub fn scrub_steam_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut digits = String::new();
    let flush = |out: &mut String, digits: &mut String| {
        if !digits.is_empty() {
            match steam_id_tag(digits) {
                Some(tag) => out.push_str(&tag),
                None => out.push_str(digits),
            }
            digits.clear();
        }
    };
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            flush(&mut out, &mut digits);
            out.push(ch);
        }
    }
    flush(&mut out, &mut digits);
    out
}

/// If `run` (a maximal digit run) looks like an individual SteamID64, return its [`peer_tag`].
fn steam_id_tag(run: &str) -> Option<String> {
    if run.len() == 17 && run.starts_with("76561") {
        return run.parse::<u64>().ok().map(peer_tag);
    }
    None
}

/// Assemble the **one-file shareable diagnostics bundle** the overlay's "Export diagnostics" button
/// writes — and scrub every raw SteamID64 to a [`peer_tag`] so the whole thing is safe to post
/// publicly. Pure (host-tested) so the assembly + scrubbing live where they can be verified; the
/// cdylib gathers the live pieces and supplies them.
///
/// `header` is a [`RunInfo::header_block`] (already password-redacted), `live_report` the latest live
/// [`DiagnosticReport`] snapshot rendered to text (`None` if none was captured this session — the log
/// tail still carries the boot snapshot), and `log_tail` the recent log lines, oldest first.
///
/// Designed to **survive a non-link**: every input is read locally, so the bundle captures exactly the
/// failed-to-connect case that log-forwarding (which needs the link up) can't.
pub fn export_bundle(header: &str, live_report: Option<&str>, log_tail: &str) -> String {
    let mut out = String::with_capacity(header.len() + log_tail.len() + 256);
    out.push_str(header.trim_end());
    out.push_str("\n\n==== live snapshot ====\n");
    match live_report {
        Some(report) => out.push_str(report.trim_end()),
        None => out.push_str("(no live snapshot captured this session; the boot snapshot is in the log tail below)"),
    }
    out.push_str("\n\n==== recent log tail (newest last) ====\n");
    out.push_str(log_tail.trim_end());
    out.push('\n');
    // One scrub over the whole assembled bundle so no raw SteamID survives in any section.
    scrub_steam_ids(&out)
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
    fn section_summary_is_optional_and_independent_of_full_fields() {
        let mut report = DiagnosticReport::new("t");
        let plain = report.section("plain");
        plain.field("a", 1).field("b", 2);
        let verbose = report.section("verbose");
        verbose.field("x", 1).field("y", 2).field("z", 3);
        verbose.summary_line("rollup", "3 items");

        let sections = report.sections();
        // A section with no summary reports none; its full fields are the only representation.
        assert!(!sections[0].has_summary());
        assert!(sections[0].summary().is_empty());
        assert_eq!(sections[0].fields().len(), 2);
        // A summary is a separate, shorter view — it doesn't disturb the full fields.
        assert!(sections[1].has_summary());
        assert_eq!(sections[1].summary(), &[("rollup".to_string(), "3 items".to_string())]);
        assert_eq!(sections[1].fields().len(), 3);
    }

    #[test]
    fn render_always_uses_full_fields_not_the_summary() {
        let mut report = DiagnosticReport::new("t");
        report.section("verbose").field("x", 1).field("y", 2).summary_line("rollup", "2 items");
        let text = report.render();
        // The log dump is the authoritative full record: it shows every field and never the summary.
        assert!(text.contains("x = 1"), "{text}");
        assert!(text.contains("y = 2"), "{text}");
        assert!(!text.contains("rollup"), "summary must not leak into the log render:\n{text}");
    }

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
    fn scrub_replaces_steam_ids_and_leaves_other_numbers() {
        let id = 76561197960287930u64; // a real-shaped individual SteamID64 (17 digits, 76561...)
        let raw = format!("own_id = {id}\nframe = 12345  players = 2  hp x1.35");
        let scrubbed = scrub_steam_ids(&raw);
        // The SteamID is gone, replaced by its stable tag; the tag round-trips with peer_tag.
        assert!(!scrubbed.contains(&id.to_string()), "raw SteamID leaked:\n{scrubbed}");
        assert!(scrubbed.contains(&peer_tag(id)), "expected the peer tag:\n{scrubbed}");
        // Ordinary numbers (frame counter, party size, the multiplier) are untouched.
        assert!(scrubbed.contains("frame = 12345"));
        assert!(scrubbed.contains("players = 2"));
        assert!(scrubbed.contains("hp x1.35"));
    }

    #[test]
    fn scrub_only_matches_the_steam_id_shape() {
        // 17 digits but not a SteamID prefix: left alone.
        let not_steam = "12345678901234567";
        assert_eq!(scrub_steam_ids(not_steam), not_steam);
        // 76561-prefixed but the wrong length (18 digits): not a SteamID64, left alone.
        let too_long = "765611979602879300";
        assert_eq!(scrub_steam_ids(too_long), too_long);
        // A non-ASCII char adjacent to digits must not corrupt the output (char-based scan).
        assert_eq!(scrub_steam_ids("× 2 ×"), "× 2 ×");
    }

    #[test]
    fn export_bundle_has_all_sections_and_scrubs_ids() {
        let id = 76561197960287930u64;
        let header = format!("==== unseamless-coop run ====\nown_id = {id}\n");
        let report = format!("==== unseamless-coop diagnostic: debug ====\nown_id = {id}\n");
        let tail = format!("00:00:01 connecting to peer {id}\n00:00:02 connect failed");
        let bundle = export_bundle(&header, Some(&report), &tail);
        // Every section header is present and in order.
        let snap = bundle.find("==== live snapshot ====").expect("live snapshot section");
        let log = bundle.find("==== recent log tail").expect("log tail section");
        assert!(snap < log, "sections out of order:\n{bundle}");
        // The failed-connect log line and its surrounding context survive.
        assert!(bundle.contains("connect failed"));
        // No raw SteamID anywhere in the assembled bundle — header, report, and tail are all scrubbed.
        assert!(!bundle.contains(&id.to_string()), "raw SteamID leaked into bundle:\n{bundle}");
        assert!(bundle.contains(&peer_tag(id)));
    }

    #[test]
    fn export_bundle_keeps_the_password_redacted_through_the_real_header_path() {
        // Defense-in-depth check on the actual composition the cdylib uses: a config with a password,
        // through RunInfo::from_config (the redacting path) into export_bundle. The password must never
        // surface in the shareable bundle.
        let mut cfg = crate::config::Config::default();
        cfg.session.password = "hunter2-very-secret".into();
        let header = RunInfo::from_config(
            &cfg,
            "export".into(),
            "0.6.0".into(),
            "release (stripped)".into(),
            "nogit".into(),
            "windows-x86_64".into(),
            "t".into(),
        )
        .header_block();
        let bundle = export_bundle(&header, None, "00:00:01 a log line");
        assert!(!bundle.contains("hunter2-very-secret"), "password leaked into bundle:\n{bundle}");
        assert!(bundle.contains("<redacted>"));
    }

    #[test]
    fn export_bundle_notes_a_missing_live_snapshot() {
        let bundle = export_bundle("header\n", None, "some log line");
        assert!(bundle.contains("no live snapshot captured this session"));
        assert!(bundle.contains("some log line"));
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
    fn report_renders_aligned_self_describing_sections() {
        let mut r = DiagnosticReport::new("snapshot");
        r.section("session").field("lobby", "Ingame").field("players", 2);
        r.section("features").field("death-debuffs", "ok");
        let out = r.render();
        assert!(out.starts_with("==== unseamless-coop diagnostic: snapshot ====\n"));
        assert!(out.contains("---- session ----"));
        // Keys column-aligned to the widest in the section ("players" = 7), so "lobby" is padded.
        assert!(out.contains("lobby   = Ingame"), "aligned key:\n{out}");
        assert!(out.contains("players = 2"));
        assert!(out.contains("---- features ----"));
        assert!(out.trim_end().ends_with("==== end diagnostic ===="));
    }

    #[test]
    fn report_sections_render_in_insertion_order() {
        let mut r = DiagnosticReport::new("t");
        r.section("first").field("a", 1);
        r.section("second").field("b", 2);
        let out = r.render();
        assert!(out.find("---- first ----").unwrap() < out.find("---- second ----").unwrap());
    }

    #[test]
    fn empty_report_and_empty_section_render_safely() {
        // No sections: just the title + end marker (no panic from an empty sections loop).
        let bare = DiagnosticReport::new("bare").render();
        assert_eq!(bare, "==== unseamless-coop diagnostic: bare ====\n==== end diagnostic ====\n");
        // A section with no fields: the `unwrap_or(0)` width branch — header present, no field lines.
        let mut r = DiagnosticReport::new("t");
        r.section("empty");
        let out = r.render();
        assert!(out.contains("---- empty ----\n==== end diagnostic ===="), "{out}");
    }

    #[test]
    fn report_uses_a_fixed_two_space_continuation_indent() {
        // Continuation lines get a constant 2-space indent (not column alignment under the value).
        let mut r = DiagnosticReport::new("t");
        r.section("s").field("trace", "line1\nline2");
        assert!(r.render().contains("trace = line1\n  line2"), "{}", r.render());
    }

    #[test]
    fn flag_scanner_count_zero_is_empty_and_never_reports() {
        let mut s = FlagScanner::new(5, 0);
        assert!(s.is_empty());
        assert_eq!(s.changes(&[true, true]), []); // nothing watched -> nothing reported
    }

    #[test]
    fn flag_scanner_reports_only_changes_with_ids() {
        let mut s = FlagScanner::new(1000, 4);
        assert_eq!(s.len(), 4);
        // First snapshot: flags 1001 and 1003 are on -> two rising edges at their real ids.
        assert_eq!(s.changes(&[false, true, false, true]), [(1001, true), (1003, true)]);
        // No change -> nothing.
        assert_eq!(s.changes(&[false, true, false, true]), []);
        // 1001 clears, 1000 sets -> a falling and a rising edge.
        assert_eq!(s.changes(&[true, false, false, true]), [(1000, true), (1001, false)]);
    }

    #[test]
    fn flag_scanner_tolerates_length_mismatch() {
        let mut s = FlagScanner::new(0, 3);
        // Shorter input only compares the overlap; never panics.
        assert_eq!(s.changes(&[true]), [(0, true)]);
        // Longer input ignores the tail beyond the watched range.
        assert_eq!(s.changes(&[true, true, false, true]), [(1, true)]);
    }

    #[test]
    fn lobby_discovery_token_matches_the_pinned_contract() {
        // Known-answer test: these must match the harness's KAT byte-for-byte (the DLL hand-bind and
        // the harness both call this fn, but the values are pinned independently so a future edit to
        // the domain string / digest slice / hex casing is caught as the discovery-breaking change it
        // is). Values computed from SHA-256("unseamless-coop/lobby-discovery/v1\0" || password)[0..16].
        assert_eq!(lobby_discovery_token("swordfish"), "e1ae25ea4eab35799470c31622b014b8");
        assert_eq!(lobby_discovery_token(""), "997351a38b7ef8eecef4d5c57de65ff4");
        assert_eq!(lobby_discovery_token("hunter2"), "1ad477bb65bcc83f7235160ee4b63883");
        // 16 bytes -> 32 lowercase hex chars, and the password is keyed verbatim (case-sensitive).
        assert_eq!(lobby_discovery_token("hunter2").len(), 32);
        assert_ne!(lobby_discovery_token("hunter2"), lobby_discovery_token("Hunter2"));
    }

    #[test]
    fn connect_report_default_shows_every_stage_unreached() {
        let f = ConnectReport::new().fields();
        let map: std::collections::HashMap<_, _> = f.into_iter().collect();
        assert_eq!(map["self_id"], "not resolved");
        assert_eq!(map["session_accepted"], "no");
        assert_eq!(map["messages"], "sent 0 / received 0");
        assert_eq!(map["handshake"], "not reached");
        assert_eq!(map["version"], "unknown (handshake not reached)");
        // No lobby / failure lines until those paths populate them.
        assert!(!map.contains_key("lobby_role"));
        assert!(!map.contains_key("failure"));
    }

    #[test]
    fn connect_report_distinguishes_one_way_nat_from_no_receive() {
        // One-way NAT: we sent, nothing came back, handshake never reached.
        let mut r = ConnectReport::new();
        r.self_id_at = Some(Duration::from_millis(500));
        r.session_accepted_at = Some(Duration::from_millis(600));
        r.messages_sent = 12;
        r.messages_received = 0;
        let map: std::collections::HashMap<_, _> = r.fields().into_iter().collect();
        assert_eq!(map["self_id"], "resolved (+0.50s)");
        assert_eq!(map["session_accepted"], "yes (+0.60s)");
        assert_eq!(map["messages"], "sent 12 / received 0"); // the one-way signal
        assert_eq!(map["handshake"], "not reached");
    }

    #[test]
    fn connect_report_surfaces_version_mismatch_and_failure() {
        let mut r = ConnectReport::new();
        r.handshake_at = Some(Duration::from_secs(2));
        r.version = VersionCheck::Mismatch;
        r.failure = Some("partner speaks protocol v2; we speak v1".into());
        let map: std::collections::HashMap<_, _> = r.fields().into_iter().collect();
        assert_eq!(map["handshake"], "reached (+2.00s)");
        assert_eq!(map["version"], "MISMATCH");
        assert_eq!(map["failure"], "partner speaks protocol v2; we speak v1");
    }

    #[test]
    fn connect_report_joiner_empty_filter_is_legible() {
        let mut r = ConnectReport::new();
        let mut l = LobbyProgress::new(LobbyRole::Joiner);
        l.list_returned_at = Some(Duration::from_secs(1));
        l.candidates = Some(0); // password filter matched nothing
        r.lobby = Some(l);
        let map: std::collections::HashMap<_, _> = r.fields().into_iter().collect();
        assert_eq!(map["lobby_role"], "joiner (found + joined)");
        assert_eq!(map["lobby_list"], "0 matching (empty filter)");
        assert_eq!(map["lobby_joined"], "no");
        assert_eq!(map["host_id"], "not resolved");
    }

    #[test]
    fn connect_report_host_lobby_branch_renders() {
        let mut r = ConnectReport::new();
        let mut l = LobbyProgress::new(LobbyRole::Host);
        l.created_at = Some(Duration::from_millis(800));
        l.host_id_resolved = true;
        r.lobby = Some(l);
        let map: std::collections::HashMap<_, _> = r.fields().into_iter().collect();
        assert_eq!(map["lobby_role"], "host (created the lobby)");
        assert_eq!(map["lobby_created"], "yes (+0.80s)");
        assert_eq!(map["host_id"], "resolved");
        // The joiner-only lines must not appear on the host branch.
        assert!(!map.contains_key("lobby_list"));
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
