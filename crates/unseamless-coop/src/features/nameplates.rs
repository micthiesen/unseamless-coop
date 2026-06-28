//! Overhead peer **nameplates** — screen-space labels drawn over each co-op partner.
//!
//! Each frame this reads the live camera and every peer's world position, projects the position to
//! screen NDC with the host-tested [`unseamless_core::projection`] math, culls (behind camera /
//! off-screen / too far), and publishes the survivors to [`crate::nameplates`] for the overlay to
//! draw. Doing the projection here (game thread) keeps the overlay free of any game-state reads — it
//! only draws the published NDC (see `docs/OVERLAY-RENDERING.md` > "(Later) Overhead nameplates").
//!
//! ## Peer source (rig-gated)
//! There is **no live remote-peer roster** until the co-op/session core lands (rung 3). Phantoms do
//! live in `WorldChrMan.player_chr_set`, but mapping a phantom `ChrIns` to a peer *identity* (name,
//! ping, soul level, death count — the content [`OverheadDisplay`] selects) needs the session layer.
//! So today:
//!  - we iterate `player_chr_set` phantoms (the other players in a real session), skipping the local
//!    player, and build a [`PeerLabelData`](unseamless_core::nameplate::PeerLabelData) per peer whose
//!    `name` is a placeholder and whose stats are all `None`; the label text is formatted by the
//!    host-tested core ([`nameplate::nameplate_text`](unseamless_core::nameplate::nameplate_text)).
//!    **TODO: fill the real per-peer identity + ping/SL/death-count at that one seam** when the co-op
//!    core arrives;
//!  - solo, `player_chr_set` holds only the local player, so that yields nothing — the
//!    `[nameplates] show_self` debug knob draws a label over your *own* head so the projection + draw
//!    are exercisable solo on the rig before any session exists.
//!
//! ## Projection conventions — rig-confirmed (2026-06-26)
//! A solo `show_self` rig check validated every convention the math left open: `forward` points where
//! the camera looks, `fov` is vertical **and in radians**, world `+Y` is up (so [`HEAD_OFFSET_M`] lifts
//! the label above the head), the `right`-vector sign is correct (not mirrored), and aspect is right (no
//! squash). The label is upright, correctly placed, and tracks the player on foot and on horseback. No
//! knobs needed — see the rig note in [`docs/NAMEPLATES.md`](../../../docs/NAMEPLATES.md).
//!
//! ## Rendering behaviors wired here (geometry done; visual feel is 2-player-tuned)
//! The three rendering behaviors from docs/NAMEPLATES.md are now wired against the host-tested core math:
//!  - **Stable per-peer color** — the palette is keyed off a stable per-peer handle (the phantom's
//!    `ChrIns` pointer for now; SteamID once the session core lands), not roster iteration order, so a
//!    peer keeps its color as the roster reorders ([`peer_color_for_id`](unseamless_core::palette::peer_color_for_id)).
//!  - **Distance LOD** — a peer is published as a [`Plate`](NameplateKind::Plate) carrying its view
//!    depth; the overlay degrades it from text to a colored dot past
//!    [`is_dot_lod`](unseamless_core::projection::is_dot_lod)'s threshold.
//!  - **Off-screen edge indicator** — an off-screen / behind-camera peer is published as an
//!    [`Edge`](NameplateKind::Edge) at a border-clamped NDC ([`edge_indicator_ndc`](Camera::edge_indicator_ndc)).
//!
//! Still gated on the real peer feed (rung 3) + a 2-player rig: the real label **content** values
//! (name/ping/SL/death-count) — the *formatting* of those values is done and host-tested in
//! [`unseamless_core::nameplate`], so only the field values remain — swapping the color key from the
//! pointer to the SteamID, and tuning the LOD/edge thresholds so the transitions *feel* right with a
//! partner at a real distance.

use eldenring::cs::{
    CSCamExt, CSCamera, CSTaskGroupIndex, ChrIns, ChrLoadStatus, ChrSet, WorldChrMan,
};
use fromsoftware_shared::Subclass;
use unseamless_core::config::OverheadDisplay;
use unseamless_core::nameplate::{self, PeerLabelData};
use unseamless_core::projection::Camera;
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};
use crate::nameplates::{NameplateKind, NameplateLabel};

/// Head clearance above a character's physics position (~feet/center), in meters along world up, so
/// the label floats above the head rather than at the navel. Rig-tunable (confirm `+Y` up and height).
const HEAD_OFFSET_M: f32 = 1.8;
/// NDC slop kept on every screen edge before culling — a label hanging slightly off the frame edge is
/// fine (its anchor can be just off-screen while the text is still partly visible).
const ON_SCREEN_MARGIN: f32 = 0.1;
/// NDC half-extent the off-screen edge indicator is pinned to — just under `1.0` so the dot insets from
/// the exact frame edge rather than sitting half off-screen. Tune at 2-player.
const EDGE_INSET: f32 = 0.95;
/// Color for the debug/solo `show_self` label. A warm near-white, distinct from the peer palette (which
/// is for *other* players) — it only ever shows during a solo rig check, never in real co-op.
const SELF_COLOR: [f32; 3] = [1.0, 0.95, 0.85];

/// Draws overhead nameplates over co-op peers. Config-gated (`[nameplates] enabled` +
/// [`OverheadDisplay`]); a no-op that publishes nothing while off.
pub struct Nameplates {
    /// Tracks the active/inactive transition so we log on change (debug) and clear stale labels once
    /// when switched off, rather than every frame.
    active: Latch<bool>,
}

impl Nameplates {
    pub fn new() -> Self {
        Self { active: Latch::new() }
    }
}

impl Feature for Nameplates {
    fn name(&self) -> &'static str {
        "nameplates"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // After physics so character positions are settled this frame (same reasoning as crit_coop /
        // death_debuffs). We only read positions + the camera; we never write game state. This phase
        // must also stay *after* `CameraStep` (it is, in the task order) so the camera matrix we read
        // is this frame's — moving this earlier would read a stale/half-updated camera.
        CSTaskGroupIndex::WorldChrMan_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        let (enabled, mode, max_dist, show_self) = crate::state::with(|c| {
            (c.nameplates.enabled, c.gameplay.overhead_display, c.nameplates.max_distance_m, c.nameplates.show_self)
        });
        // `OverheadDisplay::None` is the "show nothing overhead" choice, so it disables the feature
        // just as the master toggle does.
        let active = enabled && mode != OverheadDisplay::None;
        if self.active.changed(&active) {
            log::debug!("nameplates {}", if active { "enabled" } else { "disabled" });
            if !active {
                crate::nameplates::publish(Vec::new()); // clear any stale labels once
            }
        }
        if !active {
            return;
        }

        // Read the camera into a plain (SDK-free) `Camera`, so the projection below runs outside the
        // singleton borrow. `None` = no camera singleton yet (early boot / loading) — gather no labels,
        // but still publish empty below so any stale labels clear (don't leave frozen ones on screen).
        let camera = crate::sdk::with_instance::<CSCamera, _>(camera_from).flatten();

        let labels = camera
            .and_then(|camera| {
                crate::sdk::with_instance::<WorldChrMan, _>(|wcm| {
                    gather_labels(wcm, &camera, mode, max_dist, show_self)
                })
            })
            .unwrap_or_default();

        // Publish every frame, even when empty: peers (or the whole set, on a camera/world gap) that
        // went off-screen / out of range this frame must clear rather than freeze at a stale position.
        crate::nameplates::publish(labels);
    }
}

/// Build the host-tested [`Camera`] from the live main camera's named `CSCam` accessors. Uses
/// `pers_cam_1` — the composited camera the game actually renders from (the `camera_mask` blends the
/// others into it). `None` when `pers_cam_1` isn't wired yet: a live `CSCamera` doesn't guarantee its
/// composited sub-camera pointer is set (early boot / loading), and an unwired deref is a segfault
/// `catch_unwind` can't catch — so we null-guard the `OwnedPtr` the same way [`crate::session::read`]
/// guards the tether warp-data pointer (reading the pointer's address is not a deref).
fn camera_from(cam: &CSCamera) -> Option<Camera> {
    if cam.pers_cam_1.as_ptr().is_null() {
        return None;
    }
    let c = &*cam.pers_cam_1;
    // Named SDK basis accessors (`CSCamExt`) rather than raw matrix indexing: rows 0/1/2 are the
    // right/up/forward basis, row 3 the world position, and the trait reads exactly those rows.
    // `fov` is the engine's **vertical** fov in **radians** and `aspect_ratio` is width/height — both
    // are rig-to-confirm (a degrees-vs-radians mismatch mis-projects everything, not just mirrors it).
    let (right, up, forward, position) = (c.right(), c.up(), c.forward(), c.position());
    Some(Camera {
        position: [position.0, position.1, position.2],
        right: [right.0, right.1, right.2],
        up: [up.0, up.1, up.2],
        forward: [forward.0, forward.1, forward.2],
        fov_y: c.fov,
        aspect_ratio: c.aspect_ratio,
    })
}

/// Iterate a `ChrSet` yielding only **fully loaded** (`ChrLoadStatus::Active`) characters. The SDK's
/// `ChrSet::characters()` yields a `ChrIns` *regardless* of load status (the CLAUDE.md UAF caveat), and
/// a `player_chr_set` phantom mid-join transits `Initializing`/`NetworkInitializing`/`ReadyForActivation`
/// with its `modules` pointers not yet wired — so reading `modules.physics.position` off such an entry
/// is a segfault `catch_unwind` can't catch. We gate on the **entry's** `chr_load_status` (the robust
/// form CLAUDE.md prescribes), not the in-`ChrIns` `is_active` flag, because the two aren't guaranteed
/// to flip in lockstep for a joining network peer (a rig-confirm item). Mirrors the SDK's own entry
/// walk, reading the status alongside the pointer.
pub(crate) fn active_characters<T>(set: &ChrSet<T>) -> impl Iterator<Item = &mut T> + '_
where
    T: Subclass<ChrIns> + 'static,
{
    let mut current = set.entries;
    let end = unsafe { current.add(set.capacity as usize) };
    std::iter::from_fn(move || {
        while current != end {
            let entry = unsafe { current.as_ref() };
            let (status, chr_ins) = (entry.chr_load_status, entry.chr_ins);
            current = unsafe { current.add(1) };
            if status != ChrLoadStatus::Active {
                continue;
            }
            if let Some(mut chr_ins) = chr_ins {
                return Some(unsafe { chr_ins.as_mut() });
            }
        }
        None
    })
}

/// Project every peer (and optionally the local player) into the on-screen, in-range label set.
fn gather_labels(
    wcm: &WorldChrMan,
    camera: &Camera,
    mode: OverheadDisplay,
    max_dist: u32,
    show_self: bool,
) -> Vec<NameplateLabel> {
    let mut labels = Vec::new();

    // Identify the local player so we can skip it in the phantom roster (it's also an entry there) and
    // optionally label it via `show_self`. Pointer identity, not a name — we have no identity yet.
    let main_ptr =
        wcm.main_player.as_ref().map(|p| std::ptr::from_ref::<ChrIns>((**p).superclass()) as usize);

    // Remote-peer roster (real session only). The label *content* is formatted by the host-tested core
    // ([`nameplate::nameplate_text`]) from a per-peer [`PeerLabelData`]; today we can only fill `name`
    // (a `Player N` placeholder) and leave every stat `None`, so labels degrade to name-only.
    // TODO(co-op core): fill the real `name` + `ping_ms`/`soul_level`/`death_count` at this one seam
    // once the session layer can map a phantom to an identity — the formatting is already done.
    let mut peer_n = 0;
    // `active_characters` already skips any entry whose `chr_load_status` isn't `Active`, so each
    // `base` here is safe to deref (its modules are wired) — see the helper's docs for why that's the
    // load-bearing guard rather than the in-`ChrIns` `is_active` flag.
    for chr in active_characters(&wcm.player_chr_set) {
        let base = chr.superclass();
        let ptr = std::ptr::from_ref(base) as usize;
        // Skip the local player here; it's handled by `show_self` below.
        if main_ptr == Some(ptr) {
            continue;
        }
        // Stable per-peer color: key the palette off a *stable identity*, not iteration order, so a
        // peer keeps its color even as the roster reorders on join/leave (the flicker docs/NAMEPLATES.md
        // calls out). Until the session core hands us SteamIDs, the phantom's `ChrIns` pointer is the
        // stable per-peer handle — constant for a loaded phantom across frames, unlike `peer_n`.
        // TODO(co-op core): swap `ptr` for the peer's SteamID once the session layer maps phantom→identity.
        let color = unseamless_core::palette::peer_color_for_id(ptr as u64);
        peer_n += 1;
        // TODO(co-op core): populate the stats here once the session layer attaches an identity.
        let peer = PeerLabelData::named(format!("Player {peer_n}"));
        // `nameplate_text` is `None` only for `OverheadDisplay::None`, which already disables the whole
        // feature upstream (`active` in `on_frame`), so this skip is defensive — a peer always has text
        // by the time we get here today.
        if let Some(text) = nameplate::nameplate_text(mode, &peer) {
            push_label(&mut labels, camera, base, max_dist, text, color);
        }
    }

    // Debug/solo self-nameplate: makes the projection + draw verifiable with no session (the only case
    // that produces a visible nameplate solo). Off in normal play — you don't label yourself. It deliberately
    // bypasses the per-peer content formatter (you label yourself by name only, never with stats).
    if show_self
        && let Some(p) = wcm.main_player.as_ref()
    {
        let base = (**p).superclass();
        if base.chr_flags1c8.is_active() {
            push_label(&mut labels, camera, base, max_dist, "You".to_string(), SELF_COLOR);
        }
    }

    labels
}

/// Project one character's head position and, if it survives culling, push its label — either an
/// on-screen [`Plate`](NameplateKind::Plate) or an off-screen [`Edge`](NameplateKind::Edge) indicator.
/// Reads the physics-module world position (after `PostPhysics`, so it's this frame's settled value).
fn push_label(
    labels: &mut Vec<NameplateLabel>,
    camera: &Camera,
    base: &ChrIns,
    max_dist: u32,
    text: String,
    color: [f32; 3],
) {
    let pos = base.modules.physics.position;
    let world = [pos.0, pos.1 + HEAD_OFFSET_M, pos.2];
    // Cull by *radial* distance so the one threshold governs both a plate and an off-screen edge dot
    // (a behind-camera peer has no forward depth to cull on). `as f32` is lossless — `max_dist` is
    // clamped to a small integer-meter range. A non-finite distance (a torn position read gives NaN)
    // is culled here so a NaN never reaches the projector / draw list, the way `on_screen` used to
    // backstop it before this path existed.
    let dist = camera.distance_to(world);
    if !dist.is_finite() || dist > max_dist as f32 {
        return; // too far, or a bad position — draw nothing at all
    }
    // On-screen and in front → a full overhead plate at the projected point; otherwise (off the frame
    // edges, or behind the camera) → an edge indicator pinned to the screen border pointing at the peer.
    match camera.project(world) {
        Some(p) if p.on_screen(ON_SCREEN_MARGIN) => {
            labels.push(NameplateLabel { ndc: p.ndc, depth: p.depth, color, text, kind: NameplateKind::Plate });
        }
        _ => {
            let ndc = camera.edge_indicator_ndc(world, EDGE_INSET);
            labels.push(NameplateLabel { ndc, depth: dist, color, text, kind: NameplateKind::Edge });
        }
    }
}
