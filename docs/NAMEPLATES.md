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

Gated on the **co-op/session core** (rung 3 — real peer feed + identity), then **2-player** to verify:
- [ ] **Stable per-peer color** — index the palette by SteamID, not iteration order (kills the flicker).
- [ ] **Real label content** — name + ping + soul level + death count on `NameplateLabel`, driven by
      [`OverheadDisplay`].
- [ ] **Distance LOD** — text→dot past a depth threshold (new tuning knob inside `max_distance_m`);
      tune the threshold + dot size at 2-player.
- [ ] **Off-screen edge indicator** — derive the behind-camera bearing, render the clamped dot
      (palette-colored), tune at 2-player.

Pure-logic pieces (palette, clamp) are host-tested in `unseamless-core` so the gated work is wiring +
visual tuning, not new math.

[`NameplateLabel`]: ../crates/unseamless-coop/src/nameplates.rs
[`OverheadDisplay`]: ../crates/unseamless-core/src/config.rs
[`palette::peer_color`]: ../crates/unseamless-core/src/palette.rs
[`unseamless_core::palette`]: ../crates/unseamless-core/src/palette.rs
[`projection::clamp_ndc_to_edge`]: ../crates/unseamless-core/src/projection.rs
[`unseamless_core::projection::clamp_ndc_to_edge`]: ../crates/unseamless-core/src/projection.rs
