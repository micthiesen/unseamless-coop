# Overhead Nameplates

Screen-space labels over co-op partners — who's where, at a glance. This is the design + the build
order; the projection math is `crates/unseamless-core/src/projection.rs`, the feature is
`crates/unseamless-coop/src/features/nameplates.rs`, and the draw is in `coop/overlay.rs`
(`draw_nameplates`). See also [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md) for the overlay it draws on.

## Status

- **Projection — rig-confirmed (2026-06-26).** Solo `show_self` check on the rig: the label is upright,
  floats correctly above the head (on foot and on horseback), tracks the player, is **not** mirrored,
  and is crisp (no squash). That validates every convention the math left open: `forward` sign, `+Y`
  world-up, the `right`-vector sign, fov-is-vertical-**radians**, and aspect. No knobs needed.
- **Base styling — shipped.** Semi-transparent text (`NAMEPLATE_ALPHA = 0.65`) with a near-opaque
  contrast shadow so it stays legible, tinted per-label by [`palette::peer_color`]. Present but
  unobtrusive over the world.
- **Pure utilities — shipped + host-tested, ready to wire:** the per-peer color palette
  ([`unseamless_core::palette`]) and the off-screen edge-clamp math
  ([`unseamless_core::projection::clamp_ndc_to_edge`]). Built now so the 2-player work below is just
  wiring, not new math.
- **Peer feed — stub.** Solo, `player_chr_set` holds only the local player, so nothing draws unless
  `show_self` is on (a config-only debug knob — never in the menu, never on in real play). Mapping a
  phantom to a real peer *identity* needs the co-op/session core (rung 3, see
  [COOP-CONNECTION.md](COOP-CONNECTION.md)); until then peers get placeholder `Player N` labels.

## The Design (the full vision)

What a finished nameplate system should do, as discussed — most of it verifies only with two players
and rides on the real peer feed, so it's deliberately staged after the co-op core.

### Content: other players only
In real co-op a nameplate labels **other** players, never yourself. `show_self` exists purely to make
the projection + draw verifiable solo on the rig; it's a config-file-only knob (no settings-menu entry,
so zero menu bloat) and is off in normal play.

A label's content grows from a placeholder name today to the peer's **name + ping + soul level + death
count** ([`OverheadDisplay`] already selects what's shown) once the session layer can attach an identity
to a phantom. Those become fields on [`NameplateLabel`].

### Per-player colors
Each partner reads as a distinct, clear color — name **and** dot — from a fixed palette of high-value
hues ([`palette::peer_color`], 8 colors, headroom over the 6-player cap). The palette + lookup ship now.

The open piece is **stable assignment**: the color must key off a *stable peer identity* (the SteamID),
so a given player keeps their color for the whole session. Today the cdylib indexes by roster
*iteration order* (`peer_n`), which shifts as peers join/leave — so a color can currently flicker. The
fix lands with the real peer feed (rung 3): index the palette by a stable per-peer key, not iteration
order.

### Distance LOD: text up close, a dot far away
Don't scale the bitmap font with distance (it turns mushy). Instead, **switch representation by depth**:
the full nameplate (colored text) up close, and past a depth threshold a small **colored dot** that
reads as the same marker but takes almost no screen space. `NameplateLabel::depth` already carries the
view distance for this; the threshold is a new tuning knob (a second distance, inside the existing
`max_distance_m` hard cull). The dot uses the peer's palette color.

Verifying the transition "feels right" needs a peer at a real distance → 2-player.

### Off-screen indicator: clamp a dot to the screen edge
When a partner is outside your view, pin a small colored **dot to the screen border** in their
direction — a lightweight "teammate is over here" indicator, like a co-op compass. The pure clamp math
([`projection::clamp_ndc_to_edge`]) ships now: an off-screen NDC point is scaled onto the `±limit`
border along its bearing from center (with an inset so the dot isn't half off-screen), and an on-screen
point passes through unchanged.

Two pieces remain, both for the wiring step: (1) a peer **behind** the camera has no valid NDC
(`project` returns `None`), so the indicator must derive its bearing from the peer's view-space
direction before clamping; (2) the dot rendering itself. Verifying it helps (vs. clutters) needs a real
off-screen peer → 2-player.

## Build Order / TODO

Ship-ready now (done): projection, base styling, palette, clamp math.

**Rendering geometry — wired (host-tested math + cdylib draw).** The three rendering behaviors below are
built against the core math and draw solo (against the placeholder peer set / `show_self`); what's left
is the real peer **content** and tuning the *feel* with a partner at a real distance (2-player):
- [x] **Stable per-peer color** — palette keyed off a stable per-peer handle
      ([`palette::peer_color_for_id`]), not iteration order, so a peer's color can't flicker as the
      roster reorders. *Still TODO:* swap the handle (the phantom `ChrIns` pointer today) for the SteamID
      once the session core maps phantom→identity.
- [x] **Distance LOD** — a peer publishes as a `Plate` carrying its view depth; the overlay degrades it
      from text to a colored dot past [`projection::is_dot_lod`]'s threshold
      ([`projection::DEFAULT_DOT_DISTANCE_M`], a constant inside the `max_distance_m` hard cull — the
      rendering lane doesn't own the config surface, so it's not a config knob yet). *Tune the threshold
      + dot size at 2-player.*
- [x] **Off-screen edge indicator** — an off-screen / behind-camera peer publishes as an `Edge` at a
      border-clamped NDC ([`Camera::edge_indicator_ndc`], which derives the behind-camera bearing from
      the view-space lateral offset and clamps it); the overlay draws the palette-colored dot. *Tune at
      2-player.*

Still gated on the **co-op/session core** (rung 3 — real peer feed + identity), then **2-player**:
- [ ] **Real label content** — name + ping + soul level + death count, driven by [`OverheadDisplay`]. The
      *formatting* is done + host-tested: [`nameplate::nameplate_text`] turns a per-peer `PeerLabelData`
      (name + optional ping/SL/death-count) into the drawable label, and the cdylib already renders peers
      through that seam ([`features::nameplates::gather_labels`]). All that's left at rung 3 is filling the
      real `PeerLabelData` fields (today: placeholder name, every stat `None` → name-only labels). *Draw
      gap:* a multi-stat label is multiple lines joined with `\n`; the overlay's `draw_nameplates` centers
      a plate by its *widest* line (one `add_text` call), so the stat lines under the name will sit
      left-aligned within that block, not each individually centered. Single-line (name-only) labels —
      everything drawn today — are unaffected; revisit per-line centering when stats actually land.
- [ ] **Stable color by SteamID** — swap the per-peer color key from the phantom pointer to the SteamID.

Pure-logic pieces (palette, clamp, LOD threshold, edge-bearing) are host-tested in `unseamless-core` so
the remaining work is real content + visual tuning, not new math.

[`palette::peer_color_for_id`]: ../crates/unseamless-core/src/palette.rs
[`projection::is_dot_lod`]: ../crates/unseamless-core/src/projection.rs
[`projection::DEFAULT_DOT_DISTANCE_M`]: ../crates/unseamless-core/src/projection.rs
[`Camera::edge_indicator_ndc`]: ../crates/unseamless-core/src/projection.rs

[`NameplateLabel`]: ../crates/unseamless-coop/src/nameplates.rs
[`OverheadDisplay`]: ../crates/unseamless-core/src/config.rs
[`nameplate::nameplate_text`]: ../crates/unseamless-core/src/nameplate.rs
[`features::nameplates::gather_labels`]: ../crates/unseamless-coop/src/features/nameplates.rs
[`palette::peer_color`]: ../crates/unseamless-core/src/palette.rs
[`unseamless_core::palette`]: ../crates/unseamless-core/src/palette.rs
[`projection::clamp_ndc_to_edge`]: ../crates/unseamless-core/src/projection.rs
[`unseamless_core::projection::clamp_ndc_to_edge`]: ../crates/unseamless-core/src/projection.rs
