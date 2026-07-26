//! **In-world phantom presence probe** — does the session roster actually produce a *remote character
//! in the world*, does it *move*, and is the camera *looking at it*?
//!
//! Rung 3 reaches `players=2` in `CSSessionManager`'s session roster on two real machines
//! (docs/SESSION-DRIVE.md > "Native Transport and Two-Player Roster Result"). That's the *session*
//! layer. Whether the game then spawns the peer as a `PlayerIns` in `WorldChrMan::player_chr_set` —
//! an actual phantom standing in the world, at a position that tracks the peer's movement — was
//! previously only answerable by a human looking at the screen. This module makes it answerable from
//! a log.
//!
//! Two independent `[debug.probes]` levers, both off by default:
//!
//! - [`PresenceProbe`] (`presence_probe`) — pure observation. A ~1/sec roster line plus immediate
//!   spawn/despawn/status edge lines.
//! - [`LookAtPhantom`] (`look_at_phantom`) — points the game camera at a phantom, so a presence
//!   screenshot doesn't depend on where the save happens to leave the local character facing.
//!
//! Every line carries the greppable `presence-probe:` prefix, so a batched rig run (several lanes,
//! one game launch) can `grep presence-probe:` and read the whole presence story out of one log.
//! Lines are `info!` (so they land in a shared log) but the features aren't even *registered* when
//! their flag is off, so an off build logs nothing.
//!
//! ## Reading the log
//! **Correlate two machines' logs by the RFC3339 timestamp on each record, NOT by `@tick`** —
//! [`Tick::frame`] counts a *feature's own* ticks since registration, so a `@tick` here and a
//! `@frame` on an observer/session-probe line are different clocks. `@tick` is only good for
//! ordering lines from this feature against each other.
//!
//! **Positions are Havok space** (`pos_havok=`), which the SDK documents as AABB broadphase space
//! whose origin is tied to a block. One meter is one meter, so a *displacement within one block* is
//! meaningful — but a distance between two characters in **different blocks** is not, because the
//! two coordinates are in different frames. Both `dist=` and `moved=` therefore refuse to print a
//! number across a block change rather than print a plausible lie.
//!
//! ## Safety (the CLAUDE.md `characters()` load-status caveat, load-bearing here)
//! The probe deliberately reports **non-`Active`** entries too (a phantom mid-join sits in
//! `Initializing`/`NetworkInitializing`/`ReadyForActivation` — precisely the transition we want to
//! see), so it can't use `native_nameplates::active_characters`, which filters them out. Instead
//! [`walk_entries`] reads the `ChrSetEntry` array through **raw pointers**, never forming a
//! reference to the entry and never materializing the SDK's `ChrLoadStatus`/`ChrUpdateType` enums
//! (a half-initialized slot is exactly where an out-of-range discriminant would live, and a
//! diagnostic must not be the thing that breaks the run it's diagnosing — the raw `u8` is also the
//! more useful datum in an RE log). The `ChrIns` itself is dereferenced **only** when its entry
//! reads `Active` — the same gate `active_characters` applies. A mid-init entry's `modules`
//! pointers aren't wired, so reading `modules.physics.position` off one is a segfault
//! `catch_unwind` can't catch.
//!
//! [`Tick::frame`]: crate::feature::Tick::frame

use std::collections::BTreeMap;
use std::ptr::NonNull;

use eldenring::cs::{
    BlockId, CSCamExt, ChrCam, ChrCamType, ChrIns, ChrSet, CSTaskGroupIndex, WorldChrMan,
};
use fromsoftware_shared::Subclass; // `superclass()` on ChrIns subclasses
use unseamless_core::util::Timer;

use crate::feature::{Feature, Tick};

/// Periodic roster cadence, in seconds. ~1/sec: fast enough to see a walking phantom move between
/// samples (ER walk speed is a few m/s, so `moved=` reads in whole meters), slow enough that a
/// multi-minute rig capture stays readable.
const SAMPLE_SECS: f32 = 1.0;

/// While the roster is *empty* (the common case for most of a run), log the roster line only every
/// Nth sample. The rig log is shared with every other lane in a batched run, and a `phantoms=0` line
/// every second for twenty minutes buries them. The decimated line still proves the probe is alive.
const QUIET_SAMPLE_EVERY: u32 = 30;

/// Cap on edge lines emitted per sample interval. A phantom whose load status flaps — precisely the
/// mid-join behavior this probe exists to watch — could otherwise emit 60 lines/second, and in debug
/// builds each record is `sync_all`'d. Overflow is counted and reported on the next roster line, so
/// suppression is never silent.
const MAX_EDGES_PER_INTERVAL: u32 = 8;

// Raw discriminants of the SDK enums we read as `u8` (see the module-level safety note). Values from
// `eldenring::cs::{ChrLoadStatus, ChrUpdateType}` at the pinned rev `8c67a84`; re-derive by reading
// those enums if a future SDK bump renumbers them.
const STATUS_ACTIVE: u8 = 2; // ChrLoadStatus::Active
const UPDATE_REMOTE: u8 = 4; // ChrUpdateType::Remote

/// One `player_chr_set` slot as the probe sees it. Entry-level fields are always readable; `detail`
/// is `Some` only for an `Active` entry (the only status at which dereferencing the `ChrIns` is
/// safe — see the module docs).
#[derive(Clone, Copy)]
struct Slot {
    /// Index into the `ChrSetEntry` array. Not an identity — a slot can be recycled by a different
    /// character (which is why [`Slot::ptr`], not this, keys the movement baseline).
    index: u32,
    /// Raw `ChrLoadStatus` discriminant.
    status: u8,
    /// Raw `ChrUpdateType` discriminant. `Local` vs `Remote` is the game's own "is this a networked
    /// character" marker, readable without touching the `ChrIns`. A real co-op phantom reads `Remote`.
    update_type: u8,
    /// Address of the entry's `ChrIns` — the phantom's identity for as long as it stays loaded.
    ptr: usize,
    detail: Option<Detail>,
}

/// The fields that need a live (`Active`) `ChrIns` deref.
#[derive(Clone, Copy)]
struct Detail {
    /// Settled physics position in **Havok space**, meters (see the module docs on coordinate frames).
    pos: (f32, f32, f32),
    /// Which map block the phantom is standing in — both "is the remote character in *my* world" at a
    /// glance, and the frame that makes a `pos` comparison meaningful or meaningless.
    block: BlockId,
}

impl Slot {
    fn tag(&self) -> String {
        phantom_tag(self.ptr)
    }

    /// `pos_havok=(…) block=…` when the entry is `Active`, else why it isn't readable — so a log line
    /// never silently omits the position.
    fn position_text(&self) -> String {
        match self.detail {
            Some(d) => format!(
                "pos_havok=({:.2}, {:.2}, {:.2}) block={}",
                d.pos.0, d.pos.1, d.pos.2, d.block
            ),
            None => "pos_havok=(not readable; entry not Active)".to_string(),
        }
    }

    /// The identity + state prefix shared by every line about a phantom.
    fn ident_text(&self) -> String {
        format!(
            "slot={} tag={} chr_ins={:#x} status={} update={}",
            self.index,
            self.tag(),
            self.ptr,
            status_text(self.status),
            update_text(self.update_type),
        )
    }

    fn is_active(&self) -> bool {
        self.status == STATUS_ACTIVE
    }

    fn is_remote(&self) -> bool {
        self.update_type == UPDATE_REMOTE
    }
}

/// Short, stable handle for a phantom, derived from its `ChrIns` address (constant for as long as
/// that character is loaded). Short enough to scan down a log column, and it survives a load-status
/// change, so a SPAWNED line and the ACTIVE line after it share a tag.
///
/// **Not globally unique:** a later, different phantom allocated at the same address reuses the tag,
/// and two live phantoms 256 MB apart collide. Every line that could introduce a *new* identity
/// (SPAWNED / REPLACED / the roster line / the aim line) therefore also prints the full `chr_ins=`.
fn phantom_tag(addr: usize) -> String {
    format!("p{:06x}", (addr >> 4) & 0xff_ffff)
}

/// Render a raw `ChrLoadStatus` as `2 (Active)`. An unnamed value is a genuine finding, not noise —
/// print it rather than hiding it behind a `Debug` impl that couldn't have been constructed.
fn status_text(raw: u8) -> String {
    let name = match raw {
        0 => "Unloaded",
        1 => "Initializing",
        2 => "Active",
        3 => "NetworkInitializing",
        4 => "ReadyForActivation",
        5 => "Unloading",
        _ => return format!("{raw} (UNKNOWN)"),
    };
    format!("{raw} ({name})")
}

/// Render a raw `ChrUpdateType` as `4 (Remote)`. See [`status_text`].
fn update_text(raw: u8) -> String {
    let name = match raw {
        0 => "Local",
        1 => "Unknown1",
        2 => "Unknown2",
        3 => "Unknown3",
        4 => "Remote",
        _ => return format!("{raw} (UNKNOWN)"),
    };
    format!("{raw} ({name})")
}

/// One frame's read of `player_chr_set` plus the local player.
struct Sample {
    phantoms: Vec<Slot>,
    own: Option<Detail>,
    /// `false` when `main_player` isn't wired, so the local player's *own* `player_chr_set` entry
    /// could not be excluded and may be counted as a phantom. Never let that pass silently — it's a
    /// false positive on the exact question the probe answers.
    exclusion_ok: bool,
}

/// Samples `WorldChrMan::player_chr_set` every frame for edges, and logs the full roster ~1/sec.
pub struct PresenceProbe {
    /// Last frame's roster, keyed by slot index, for edge detection.
    last: BTreeMap<u32, Slot>,
    /// Position + block at the previous *periodic* sample, keyed by the phantom's `ChrIns` address
    /// (its identity) rather than by slot index — a recycled slot must not inherit the previous
    /// character's baseline and fabricate a large `moved=`.
    last_sample: BTreeMap<usize, Detail>,
    sample: Timer,
    /// Seconds accumulated since the previous roster line, so `moved=` states the interval it covers
    /// (`Timer` drops backlog after a stall, and the clock only advances on in-world frames).
    elapsed: f32,
    /// Samples logged with an empty roster, for [`QUIET_SAMPLE_EVERY`] decimation.
    quiet_samples: u32,
    edges_emitted: u32,
    edges_suppressed: u32,
    /// One-shot: the probe is registered and ticking, but no `WorldChrMan` yet.
    waiting_announced: bool,
    /// One-shot: first successful `WorldChrMan` read.
    announced: bool,
}

impl PresenceProbe {
    pub fn new() -> Self {
        Self {
            last: BTreeMap::new(),
            last_sample: BTreeMap::new(),
            sample: Timer::every_secs(SAMPLE_SECS),
            elapsed: 0.0,
            quiet_samples: 0,
            edges_emitted: 0,
            edges_suppressed: 0,
            waiting_announced: false,
            announced: false,
        }
    }
}

impl Default for PresenceProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Feature for PresenceProbe {
    fn name(&self) -> &'static str {
        "presence-probe"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Same phase the nameplates draw from: physics is settled, so a position we read is this
        // frame's final one rather than a mid-update value. Matters for the `moved=` column.
        CSTaskGroupIndex::ChrIns_PostPhysics
    }

    fn on_frame(&mut self, tick: Tick) {
        let Some(sample) = crate::sdk::with_instance::<WorldChrMan, _>(gather) else {
            // No WorldChrMan (title screen / loading / world teardown). Announce once, so a run where
            // the probe never reaches the world is distinguishable by grep from one where the flag was
            // never set.
            if !self.waiting_announced {
                self.waiting_announced = true;
                log::info!(
                    "presence-probe: enabled and ticking; no WorldChrMan yet (title screen / loading). \
                     Roster lines start once the world is live."
                );
            }
            // Invalidate the roster: holding it across a load would emit DESPAWNED lines stamped long
            // after the phantom actually went away, or misreport a reoccupied slot as REPLACED. Say so
            // in the log rather than leaving the gap to be inferred.
            self.invalidate_roster();
            return;
        };

        if !self.announced {
            self.announced = true;
            log::info!(
                "presence-probe: live @tick {} — watching WorldChrMan::player_chr_set (phantoms exclude the \
                 local player); roster every {SAMPLE_SECS:.0}s plus spawn/despawn/status edges. \
                 Positions are Havok space (per-block origin): dist=/moved= print n/a across a block change. \
                 Correlate machines by timestamp, not @tick (it counts this feature's own ticks).",
                tick.frame,
            );
        }

        let now: BTreeMap<u32, Slot> = sample.phantoms.iter().map(|s| (s.index, *s)).collect();
        self.log_edges(&now, tick.frame);
        self.last = now;

        self.elapsed += tick.delta.max(0.0);
        if self.sample.tick(tick.delta) {
            self.log_roster(&sample, tick.frame);
        }
    }
}

impl PresenceProbe {
    /// Drop the remembered roster across a world gap, noting what was dropped so the log shows the
    /// discontinuity instead of implying the phantoms simply persisted.
    fn invalidate_roster(&mut self) {
        if self.last.is_empty() && self.last_sample.is_empty() {
            return;
        }
        log::info!(
            "presence-probe: world gone (no WorldChrMan) — roster invalidated, {} phantom(s) dropped without a \
             DESPAWNED edge",
            self.last.len(),
        );
        self.last.clear();
        self.last_sample.clear();
    }

    /// Log a line for every roster change since last frame: a phantom appearing, its load status or
    /// update type changing (the join transition), the slot being reused by a different character, or
    /// the phantom going away.
    fn log_edges(&mut self, now: &BTreeMap<u32, Slot>, tick: u64) {
        for (index, slot) in now {
            let line = match self.last.get(index) {
                None => Some(format!("SPAWNED {}  {}", slot.ident_text(), slot.position_text())),
                // Same slot, different character object: the game recycled the slot without us ever
                // seeing it empty. Its own line so a tag change isn't misread as a teleport.
                Some(prev) if prev.ptr != slot.ptr => Some(format!(
                    "REPLACED (was tag={} chr_ins={:#x}) {}  {}",
                    prev.tag(),
                    prev.ptr,
                    slot.ident_text(),
                    slot.position_text(),
                )),
                Some(prev) if prev.status != slot.status => Some(format!(
                    "STATUS {} -> {} {}  {}",
                    status_text(prev.status),
                    status_text(slot.status),
                    slot.ident_text(),
                    slot.position_text(),
                )),
                // A Local -> Remote flip is the "is this really a networked character" signal; without
                // this arm it would only surface on the next periodic line.
                Some(prev) if prev.update_type != slot.update_type => Some(format!(
                    "UPDATE-TYPE {} -> {} {}",
                    update_text(prev.update_type),
                    update_text(slot.update_type),
                    slot.ident_text(),
                )),
                Some(_) => None,
            };
            if let Some(line) = line {
                self.emit_edge(&line, tick);
            }
        }
        // `last` is only read here, so take the despawn lines out first and emit after the loop —
        // `emit_edge` needs `&mut self`.
        let gone: Vec<String> = self
            .last
            .iter()
            .filter(|(index, _)| !now.contains_key(*index))
            .map(|(_, prev)| {
                format!("DESPAWNED {}  last {}", prev.ident_text(), prev.position_text())
            })
            .collect();
        for line in &gone {
            self.emit_edge(line, tick);
        }
    }

    /// Emit one edge line, under the per-interval budget (see [`MAX_EDGES_PER_INTERVAL`]).
    fn emit_edge(&mut self, line: &str, tick: u64) {
        if self.edges_emitted >= MAX_EDGES_PER_INTERVAL {
            self.edges_suppressed += 1;
            return;
        }
        self.edges_emitted += 1;
        log::info!("presence-probe: EDGE @tick {tick} {line}");
    }

    /// The periodic roster: counts, the local player's own position (so a two-machine capture can be
    /// correlated), and one line per phantom with its movement since the previous sample.
    fn log_roster(&mut self, sample: &Sample, tick: u64) {
        let dt = std::mem::replace(&mut self.elapsed, 0.0);
        let (emitted, suppressed) = (self.edges_emitted, self.edges_suppressed);
        self.edges_emitted = 0;
        self.edges_suppressed = 0;

        // Decimate the empty-roster line: it's the same "still nothing" every second for most of a
        // run, and the log is shared with every other lane.
        if sample.phantoms.is_empty() {
            self.last_sample.clear();
            let quiet = self.quiet_samples;
            self.quiet_samples += 1;
            if !quiet.is_multiple_of(QUIET_SAMPLE_EVERY) {
                return;
            }
        } else {
            self.quiet_samples = 0;
        }

        let own_text = match sample.own {
            Some(d) => format!(
                "self pos_havok=({:.2}, {:.2}, {:.2}) block={}",
                d.pos.0, d.pos.1, d.pos.2, d.block
            ),
            None => "self (no active local player)".to_string(),
        };
        // The headline number is every occupied slot; `active`/`remote` are the ones that actually
        // answer "is a networked character standing in my world", so a single grep of the header
        // settles it without reading the per-phantom lines.
        let active = sample.phantoms.iter().filter(|s| s.is_active()).count();
        let remote = sample.phantoms.iter().filter(|s| s.is_remote()).count();
        let mut header = format!(
            "presence-probe: roster @tick {tick} dt={dt:.2}s: phantoms={} (active={active} remote={remote}) | {own_text}",
            sample.phantoms.len(),
        );
        if !sample.exclusion_ok {
            header.push_str(
                " | WARNING self-exclusion UNAVAILABLE (main_player not wired) — the local player's own \
                 entry may be counted as a phantom above",
            );
        }
        if suppressed > 0 {
            header.push_str(&format!(
                " | {suppressed} edge line(s) suppressed this interval (cap {MAX_EDGES_PER_INTERVAL}, {emitted} emitted)"
            ));
        }
        log::info!("{header}");

        let mut baseline = BTreeMap::new();
        for slot in &sample.phantoms {
            // Distance travelled since the last periodic sample, keyed on the phantom's identity.
            // Explicit about *why* a number is absent — "not moving" and "no baseline" are opposite
            // answers to the question this probe exists to settle, so they never share a rendering.
            let moved = match (slot.detail, self.last_sample.get(&slot.ptr)) {
                (Some(d), Some(prev)) if prev.block == d.block => {
                    format!("moved={:.2}m", distance(d.pos, prev.pos))
                }
                (Some(_), Some(_)) => "moved=n/a (block changed; Havok frames differ)".to_string(),
                (Some(_), None) => "moved=n/a (no baseline yet)".to_string(),
                (None, _) => "moved=n/a (position not readable)".to_string(),
            };
            // Distance from the local player. A peer that's loaded but far outside the local chunk
            // produces an empty screenshot; without this figure that's indistinguishable from the
            // phantom not existing at all.
            let dist = match (slot.detail, sample.own) {
                (Some(d), Some(o)) if d.block == o.block => {
                    format!("dist={:.2}m", distance(d.pos, o.pos))
                }
                (Some(d), Some(o)) => {
                    format!("dist=n/a (different block: {} vs self {})", d.block, o.block)
                }
                _ => "dist=n/a (position not readable)".to_string(),
            };
            log::info!(
                "presence-probe:   phantom {}  {} {dist} {moved}",
                slot.ident_text(),
                slot.position_text(),
            );
            if let Some(d) = slot.detail {
                baseline.insert(slot.ptr, d);
            }
        }
        self.last_sample = baseline;
    }
}

/// Read the phantom roster (every `player_chr_set` entry that isn't the local player) plus the local
/// player's own position, in one `WorldChrMan` borrow.
fn gather(wcm: &WorldChrMan) -> Sample {
    // Address of the local player's `PlayerIns`, so its own `player_chr_set` entry can be excluded by
    // pointer identity. `OwnedPtr::as_ptr` reads the pointer *value* without forming a `&PlayerIns`,
    // which would assert its (possibly not-yet-wired) module pointers are valid.
    let main_ptr = wcm.main_player.as_ref().map(|p| p.as_ptr() as usize);

    let own = wcm.main_player.as_ref().and_then(|p| {
        let base = (**p).superclass();
        // Same guard the nameplates use on the local player: skip a mid-load/teardown half-wired
        // `ChrIns` rather than reading its unwired modules.
        base.chr_flags1c8.is_active().then(|| detail(base))
    });

    let mut phantoms = Vec::new();
    for (index, status, update_type, ptr) in walk_entries(&wcm.player_chr_set) {
        let addr = ptr.as_ptr() as usize;
        if Some(addr) == main_ptr {
            continue;
        }
        // ONLY here — an `Active` entry — is dereferencing the `ChrIns` safe (module pointers wired).
        // See the module-level safety note; this is the same gate `active_characters` applies.
        let detail =
            (status == STATUS_ACTIVE).then(|| detail(unsafe { ptr.as_ref() }.superclass()));
        phantoms.push(Slot { index, status, update_type, ptr: addr, detail });
    }
    Sample { phantoms, own, exclusion_ok: main_ptr.is_some() }
}

// -------------------------------------------------------------------------------------------------
// Look-at-phantom camera lever
// -------------------------------------------------------------------------------------------------

/// **Point the camera at a remote phantom** — `[debug.probes] look_at_phantom`, off by default.
///
/// The screenshot half of the presence check is worthless unless the camera is actually looking at
/// the remote player, and we don't want to depend on where a save happens to leave the local
/// character standing. This lever removes that dependency: while a remote phantom is loaded, aim the
/// camera at it, every frame, deterministically.
///
/// Mechanism is [`crate::features::spectate`]'s, reused rather than reinvented: `WorldChrMan.chr_cam`
/// exposes `death_cam_target: Option<NonNull<ChrIns>>` and `camera_type: ChrCamType::DeathCam`; we
/// write both and re-assert each frame from `WorldChrMan_PostPhysics` (after the game's `CameraStep`).
/// Target selection is the first `Active` **and** `Remote` non-local `player_chr_set` entry in slot
/// order — deterministic, so two machines' logs are comparable and a rerun picks the same phantom.
/// Nothing about spectate's own death-triggered behavior or its [`unseamless_core::spectate`] policy
/// is touched.
///
/// **This lever doubles as the verification for `docs/SPECTATE.md` > "Rig asks" #1** — whether writing
/// `death_cam_target` + forcing `DeathCam` actually makes the camera follow that character, which is
/// currently unverified because spectate's own path needs a 2-player session *and* a local death. Here
/// it needs only a loaded phantom, and the CONFIRM line answers it **from the log** rather than
/// deferring to a human: it reports (a) the `camera_type` the game left in place before our re-assert,
/// (b) whether our `death_cam_target` pointer is still installed, and (c) the actual camera geometry —
/// where the camera is, how far the phantom is, and the angle between the camera's forward vector and
/// the direction to the phantom. A small angle plus a plausible follow distance *is* the answer.
///
/// Caveat for a rig run: don't enable this together with `gameplay.always_spectate_on_death` — both
/// drive the same two `chr_cam` fields (`Config::validate` warns).
pub struct LookAtPhantom {
    /// The `ChrIns` **address** we're currently aiming at (and therefore the pointer we installed in
    /// `death_cam_target`), so a target change logs once rather than per-frame, and the read-back can
    /// tell "still ours" from "the game replaced it". An address, not a `NonNull`, for the same reason
    /// spectate keeps a `u64`: it's only ever compared, never dereferenced, and it keeps the feature
    /// `Send` without an `unsafe impl`.
    current: Option<usize>,
    /// Frames since acquiring the current target, to time the read-back confirm.
    since_acquire: u64,
    /// Whether the CONFIRM line has fired for the current target.
    confirmed: bool,
    /// Consecutive frames with no eligible target, for release hysteresis.
    missing: u64,
    /// Whether we're currently driving the camera.
    aiming: bool,
    /// One-shot "registered and waiting", so a run where no phantom ever loads is distinguishable by
    /// grep from one where the flag was never set.
    announced: bool,
}

impl LookAtPhantom {
    const PREFIX: &'static str = "presence-probe: look-at";
    /// Frames to wait after acquiring before reading back what the game left in place. ~1s at 60fps —
    /// several of the game's own `CameraStep` updates, so a revert has had every chance to show.
    const CONFIRM_AFTER_FRAMES: u64 = 60;
    /// Consecutive target-less frames before releasing the camera. A phantom routinely drops out of
    /// `Active` for a frame or two during a peer's load/fast-travel; releasing on the first such frame
    /// would churn `request_camera_reset` and the log with aim/release pairs.
    const RELEASE_AFTER_FRAMES: u64 = 30;

    pub fn new() -> Self {
        Self {
            current: None,
            since_acquire: 0,
            confirmed: false,
            missing: 0,
            aiming: false,
            announced: false,
        }
    }
}

impl Default for LookAtPhantom {
    fn default() -> Self {
        Self::new()
    }
}

impl Feature for LookAtPhantom {
    fn name(&self) -> &'static str {
        "look-at-phantom"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Spectate's phase, for the same reason: it runs after the game's `CameraStep`, so our write
        // re-asserts on top of the game's own camera update each frame (1-frame lag, fine).
        CSTaskGroupIndex::WorldChrMan_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        if !self.announced {
            self.announced = true;
            log::info!(
                "{}: enabled — will aim the camera at the first Active+Remote phantom in player_chr_set \
                 and report whether the write sticks (docs/SPECTATE.md > \"Rig asks\" #1).",
                Self::PREFIX,
            );
        }
        crate::sdk::with_instance_mut::<WorldChrMan, _>(|wcm| self.tick(wcm));
    }
}

impl LookAtPhantom {
    fn tick(&mut self, wcm: &mut WorldChrMan) {
        // Require a wired `main_player`. Without it the local-player exclusion below is a no-op, and
        // we'd cheerfully force the death cam onto the *local* player and log it as a phantom — the
        // most misleading thing this lever could do. It's also the load/teardown window spectate
        // refuses to act in, for the UAF reason in its module docs.
        let main = wcm.main_player.as_ref().map(|p| (p.as_ptr() as usize, (**p).superclass()));
        let local = main.and_then(|(addr, base)| {
            base.chr_flags1c8.is_active().then(|| (addr, detail(base)))
        });

        // First `Active` + `Remote` non-local entry, in slot order. `walk_entries` reads only
        // entry-level fields; the deref below is gated on `Active` (module-level safety note).
        let mut target = None;
        if let Some((main_addr, _)) = local {
            for (_, status, update_type, ptr) in walk_entries(&wcm.player_chr_set) {
                let addr = ptr.as_ptr() as usize;
                if addr == main_addr || status != STATUS_ACTIVE || update_type != UPDATE_REMOTE {
                    continue;
                }
                let base = unsafe { ptr.as_ref() }.superclass();
                target = Some((addr, NonNull::from(base), detail(base)));
                break;
            }
        }

        let Some(mut cam_ptr) = wcm.chr_cam else {
            return; // camera not wired yet (early boot / loading)
        };
        // SAFETY: `chr_cam` is the live per-character camera singleton pointer; non-null here. We read
        // and write plain fields on it and never dereference `death_cam_target` ourselves.
        let cam = unsafe { cam_ptr.as_mut() };

        let Some((addr, ptr, phantom)) = target else {
            self.missing += 1;
            // Release only after sustained absence, and exactly once per aim cycle.
            if self.aiming && self.missing >= Self::RELEASE_AFTER_FRAMES {
                // Clear the target (never leave a pointer installed the game could deref after the
                // `ChrIns` is freed), un-force `DeathCam`, and ask for the default follow position
                // back. Same restore set spectate uses on revive.
                cam.death_cam_target = None;
                cam.camera_type = ChrCamType::Unk0;
                cam.request_camera_reset = true;
                self.aiming = false;
                self.current = None;
                log::info!(
                    "{}: released camera — no Active+Remote phantom for {} frames",
                    Self::PREFIX,
                    self.missing,
                );
            }
            return;
        };
        self.missing = 0;

        // Read back what the game left in place *before* this frame's re-assert — the whole point of
        // the confirm line below.
        let observed_type = camera_type_raw(cam);
        let held = cam.death_cam_target.map(|h| h.as_ptr() as usize);
        // Camera geometry off the same untouched view matrix (our write doesn't move it this frame —
        // the game recomputes it next `CameraStep`). The SDK documents camera position as Havok space,
        // the same frame the phantom position is in.
        let camera_pos = cam.position();
        let camera_fwd = cam.forward();

        cam.death_cam_target = Some(ptr);
        cam.camera_type = ChrCamType::DeathCam;
        self.aiming = true;

        if self.current != Some(addr) {
            self.current = Some(addr);
            self.since_acquire = 0;
            self.confirmed = false;
            log::info!(
                "{}: aiming at phantom tag={} chr_ins={addr:#x} block={}; camera_type was {} before the write, \
                 now forced {}. A CONFIRM line follows in ~{} frames.",
                Self::PREFIX,
                phantom_tag(addr),
                phantom.block,
                camera_type_text(observed_type),
                camera_type_text(ChrCamType::DeathCam as u32),
                Self::CONFIRM_AFTER_FRAMES,
            );
            return;
        }

        self.since_acquire += 1;
        if self.confirmed || self.since_acquire < Self::CONFIRM_AFTER_FRAMES {
            return;
        }
        self.confirmed = true;
        let observed = camera_type_text(observed_type);

        // Did our pointer survive the game's own camera update?
        let target_held = match held {
            Some(h) if h == addr => "target_held=yes".to_string(),
            Some(h) => format!("target_held=no (game holds {h:#x})"),
            None => "target_held=no (game cleared it)".to_string(),
        };
        // The geometric answer: where the camera actually is relative to the phantom. Havok space is
        // per-block, so this is only meaningful when camera and phantom share a block — the camera
        // sits on the local player, so compare against the local player's block.
        let geometry = match local {
            Some((_, own)) if own.block == phantom.block => {
                let cam_xyz = (camera_pos.0, camera_pos.1, camera_pos.2);
                let d = distance(cam_xyz, phantom.pos);
                let fwd = (camera_fwd.0, camera_fwd.1, camera_fwd.2);
                match aim_error_degrees(fwd, cam_xyz, phantom.pos) {
                    Some(deg) => format!("cam_to_phantom={d:.2}m aim_error={deg:.1}deg"),
                    None => format!("cam_to_phantom={d:.2}m aim_error=n/a (degenerate vector)"),
                }
            }
            Some((_, own)) => format!(
                "cam_to_phantom=n/a (phantom block {} != local block {})",
                phantom.block, own.block
            ),
            None => "cam_to_phantom=n/a (local player not readable)".to_string(),
        };
        log::info!(
            "{} CONFIRM: tag={} chr_ins={addr:#x} after {} frames of re-asserting — the game left \
             camera_type={observed} {target_held} {geometry}. Reading it: camera_type=7 and target_held=yes mean our \
             write survives the game's own camera update; a small aim_error (<~15deg) at a plausible \
             cam_to_phantom means the camera really is framing the phantom, which answers \
             docs/SPECTATE.md > \"Rig asks\" #1 YES. camera_type=0 or target_held=no means the game is \
             reverting us and this phase/field set is the wrong lever (next thing to try: register in \
             CameraStep).",
            Self::PREFIX,
            phantom_tag(addr),
            Self::CONFIRM_AFTER_FRAMES,
        );
    }
}

/// Angle in degrees between the camera's forward vector and the direction from the camera to the
/// phantom. `0` = pointing straight at it. `None` if either vector is degenerate (camera exactly on
/// the phantom, or an unwired zero matrix), so a division by zero can't print as a confident `0deg`.
fn aim_error_degrees(
    forward: (f32, f32, f32),
    from: (f32, f32, f32),
    to: (f32, f32, f32),
) -> Option<f32> {
    let to_target = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
    let (fl, tl) = (norm(forward), norm(to_target));
    if fl <= f32::EPSILON || tl <= f32::EPSILON {
        return None;
    }
    let dot = forward.0 * to_target.0 + forward.1 * to_target.1 + forward.2 * to_target.2;
    Some((dot / (fl * tl)).clamp(-1.0, 1.0).acos().to_degrees())
}

fn norm(v: (f32, f32, f32)) -> f32 {
    (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt()
}

/// Read `chr_cam.camera_type` as a raw `u32`. The raw number is the more useful datum in an RE log
/// (`Unk3` tells you nothing `3` doesn't), and keeping it raw means an unexpected value gets *printed*
/// rather than being a value the SDK enum couldn't have represented.
///
/// (This is not a soundness guard: the enclosing `as_mut()` has already produced a `&mut ChrCam`,
/// which asserts every field is valid — the same assumption every SDK-using feature makes.)
fn camera_type_raw(cam: &ChrCam) -> u32 {
    cam.camera_type as u32
}

/// Render a `camera_type` value with its SDK name. `0` and `7` are annotated because they're the two
/// that carry the verdict: `7` is the mode we force, `0` is the normal follow cam we'd be reverted to.
fn camera_type_text(raw: u32) -> String {
    match raw {
        0 => "0 (Unk0, normal follow cam)".to_string(),
        7 => "7 (DeathCam)".to_string(),
        other => format!("{other}"),
    }
}

/// Euclidean distance between two positions **in the same Havok frame** (same block), in meters.
/// Callers must check the block first — see the module docs on coordinate frames.
fn distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    norm((a.0 - b.0, a.1 - b.1, a.2 - b.2))
}

/// Copy the readable fields off a live `ChrIns`.
fn detail(base: &ChrIns) -> Detail {
    let p = base.modules.physics.position;
    Detail { pos: (p.0, p.1, p.2), block: base.block_id }
}

/// Walk a `ChrSet`'s entry array yielding `(index, raw load status, raw update type, ChrIns pointer)`
/// for every **occupied** slot, at **any** load status and **without dereferencing** the `ChrIns`.
///
/// This is deliberately not `native_nameplates::active_characters`: that one hands back a `&mut T`,
/// which is only sound for `Active` entries, and so filters the mid-join statuses this probe exists
/// to observe. Here the caller gets the raw pointer and the raw status, and dereferences only when the
/// status says it may (the CLAUDE.md load-status caveat).
///
/// Fields are read through `addr_of!` off the entry pointer rather than via `&ChrSetEntry`, and the
/// two `#[repr(u8)]` enums are kept as raw `u8`. A never-initialized or torn-down slot is exactly
/// where an out-of-range discriminant would live, and it's read here *before* the occupancy check —
/// materializing the enum there would be UB in the one code path whose job is to survive weird state.
fn walk_entries<T>(
    set: &ChrSet<T>,
) -> impl Iterator<Item = (u32, u8, u8, NonNull<T>)> + '_
where
    T: Subclass<ChrIns> + 'static,
{
    let mut current = set.entries;
    let mut index = 0u32;
    let end = unsafe { current.add(set.capacity as usize) };
    std::iter::from_fn(move || {
        while current != end {
            let p = current.as_ptr();
            // SAFETY: `current` is in `[entries, entries + capacity)`, so it points at a live
            // `ChrSetEntry<T>`. Each read is of an initialized, correctly-aligned field; the two enum
            // fields are read as their `#[repr(u8)]` underlying type, and `Option<NonNull<T>>` is
            // valid for every bit pattern (null reads as `None`).
            let (chr_ins, status, update_type) = unsafe {
                (
                    std::ptr::read(std::ptr::addr_of!((*p).chr_ins)),
                    std::ptr::read(std::ptr::addr_of!((*p).chr_load_status).cast::<u8>()),
                    std::ptr::read(std::ptr::addr_of!((*p).chr_update_type).cast::<u8>()),
                )
            };
            let i = index;
            current = unsafe { current.add(1) };
            index += 1;
            // An empty slot has no `ChrIns` at all — nothing to report, at any status.
            if let Some(chr_ins) = chr_ins {
                return Some((i, status, update_type, chr_ins));
            }
        }
        None
    })
}
