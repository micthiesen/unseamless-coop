# Overhead Nameplates

Screen-space labels over co-op partners — who's where, at a glance. This is the design + the build
order; the projection math is `crates/unseamless-core/src/projection.rs`, the feature is
`crates/unseamless-coop/src/features/nameplates.rs`, and the draw is in `coop/overlay.rs`
(`draw_nameplates`). See also [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md) for the overlay it draws on.

## Native rendering (no overlay) — spike 2026-06-28

We spiked rendering nameplates (and eventually more UI) **natively** via the game's own `CSEzDraw`
debug renderer (`RendMan.debug_ez_draw`) from a frame task, instead of the hudhook/imgui present-hook.
Motivation: it sidesteps the present-hook entirely (so it renders even where the overlay crashes — the
native-Windows RTX-3080 case in OVERLAY-RENDERING.md), and it's a path toward dropping imgui. The
substrate is `coop/native_draw.rs`; the marker feature is `coop/features/native_nameplates.rs`, gated by
`[nameplates] native_spike` (config-only, off by default, coexists with the overlay nameplates).

What the spike established (all rig-confirmed):

- **Native geometry works.** `CSEzDraw` draws untextured, depth-tested colored geometry (`draw_line`,
  `draw_sphere`, and **filled `draw_triangle`**, all confirmed in retail) that the game composites into
  the 3D scene. Lines/spheres/quads/discs all render.
- **Native *text* via `CSEzDraw::draw_text` is DEAD in retail.** We RE'd the call (RVA `0x264efd0`, see
  `native_draw.rs` for the derivation) and it enqueues fine, but the game **hard-faults at render** (a
  native access violation `catch_unwind` can't catch) because the debug text **font isn't initialized in
  the shipping build** — same in world- and screen-space coord modes; the whole `draw_text` path has a
  single caller and is debug-only. The real UI fonts (`FontRepository`, `GUIFont@GuiFramework`,
  `CSScaleformSystem`) are debug-only or menu/Scaleform-pipeline-locked, so there's no standalone
  "draw a string" primitive. **Conclusion: render text as a bitmap font rasterized to filled quads**
  (one+ solid rects per glyph), not via any game font.
- **Screen-space 2D works** via a near-plane billboard (`native_draw::ScreenSpace`): `CSEzDraw` geometry
  is world-space, so 2D UI (toasts/menus) is drawn on a plane locked just in front of the camera. Filled
  quads + bitmap-as-quads text render crisp and screen-locked (the blocky bitmap look Michael wants).
- **Cost model: per-primitive (~3µs/quad on the rig), linear, not fixable by caching** (it's the game's
  debug-renderer enqueue/render, likely unbatched). So: nameplate markers (a few discs) are ~free; small
  transient toasts are fine; a **dense always-redrawn menu** costs real frame time (~1.7k quads ≈ -12fps;
  ~11.5k ≈ 60→20fps). Mitigations: rect-merging (fewer quads), keep menus compact, gate work to when
  shown (the steady-state cost when nothing's drawn is one bool check).

### Outcome (2026-06-28): nameplate dots kept native; everything else reverted to imgui

After building native toasts/banners + a tabbed menu on a `CSEzDraw` + `ui::render` stack, we reverted
all of those to the imgui overlay and kept **only the native nameplate dot**. The deciding facts (full
rationale in [UI-LIBRARY.md](UI-LIBRARY.md) > OUTCOME): screen-space CSEzDraw UI **swims** (it's a
camera-billboard; no screen-space geometry mode — [RE-SCREENSPACE.md](RE-SCREENSPACE.md)) and is
**per-primitive-slow** for dense text; there's no reachable game text primitive except the `CSFeMan` HUD
channels, which work but we chose not to pursue ([RE-GAME-UI.md](RE-GAME-UI.md)); and imgui is simply the
right tool for a dense custom menu.

**Final state:**
- [x] **Native nameplate dots — KEPT.** A colored, camera-facing filled **disc** per player
      ([`native_draw::draw_billboard_disc`]) — world-space (so it doesn't swim), depth-tested,
      present-hook-free, no LOD; appropriate as a dot. The one surface where CSEzDraw is a good fit.
      Gated by `[nameplates] native_spike`.
- [reverted] **Native toasts / banners / menu** → back to the imgui overlay. The `ui::render`/`ui::input`
      libraries + the bitmap-font/Proggy pipeline + the screen-space bits of `native_draw` were removed
      (in git history if revived). The CSEzDraw `draw_text` finding (RVA `0x264efd0`, font-dead in retail)
      is recorded above and in [RE-GAME-UI.md](RE-GAME-UI.md).

[`native_draw::draw_billboard_disc`]: ../crates/unseamless-coop/src/native_draw.rs

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
      real `PeerLabelData` fields (today: placeholder name, every stat `None` → name-only labels).
      *Multi-line centering — done:* a multi-stat label is multiple lines joined with `\n`, and the overlay
      now centers **each line independently** on the projected point (`draw_nameplates` measures each line
      and places it via the host-tested [`projection::centered_line_origin`]), so the stat lines sit
      centered under the name rather than left-aligned within the widest line's block. Single-line
      (name-only) labels — everything drawn today — land pixel-identically (no regression). *Still
      2-player-gated:* the visual feel of a real multi-stat plate at distance.
- [ ] **Stable color by SteamID** — swap the per-peer color key from the phantom pointer to the SteamID.

Pure-logic pieces (palette, clamp, LOD threshold, edge-bearing) are host-tested in `unseamless-core` so
the remaining work is real content + visual tuning, not new math.

[`palette::peer_color_for_id`]: ../crates/unseamless-core/src/palette.rs
[`projection::centered_line_origin`]: ../crates/unseamless-core/src/projection.rs
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
