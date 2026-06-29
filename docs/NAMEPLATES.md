# Overhead Nameplates

A per-player colored **dot** over each co-op partner — who's where, at a glance. **This is shipped and
on by default.** It's drawn natively by the game's own `CSEzDraw` renderer (world-space, depth-tested,
**no overlay / no present-hook**), not as an imgui projected label. The feature is
`crates/unseamless-coop/src/features/native_nameplates.rs` and the draw substrate is
`coop/native_draw.rs` (`draw_billboard_disc`); it marks **your own head** too, so it's verifiable solo
on the rig with no session. Config: `[nameplates] enabled` (default `true`; surfaced in the settings
menu as "Overhead nameplates").

> **The imgui projected-label nameplates were removed (2026-06-28).** An earlier design rendered
> screen-space text labels over peers via a host-tested projection (`unseamless_core::projection` →
> `coop/features/nameplates.rs` → `coop/overlay.rs::draw_nameplates`). The decision is settled:
> nameplates are a native colored dot, not imgui labels. That whole pipeline (the projector feature,
> the `unseamless_core::projection` / `unseamless_core::nameplate` core modules, the `OverheadDisplay`
> text-content selector, and the overlay draw path) was deleted — it's in git history if ever revived.
> The **RE/projection insights** it produced are preserved below so a revival doesn't start cold.
>
> **Only remaining nameplate follow-up: color-by-SteamID.** The dot color is currently keyed off the
> phantom's `ChrIns` pointer (stable per loaded phantom across frames, so a peer keeps its color as the
> roster reorders) — a stand-in until the session core can map a phantom to a peer *identity*. Swapping
> the key to the SteamID is **rung-3-gated** (needs the co-op/session core — see
> [COOP-CONNECTION.md](COOP-CONNECTION.md)); the TODO is marked at the seam in `native_nameplates.rs`
> (`gather`).

## Native rendering (no overlay) — spike 2026-06-28

We spiked rendering nameplates (and eventually more UI) **natively** via the game's own `CSEzDraw`
debug renderer (`RendMan.debug_ez_draw`) from a frame task, instead of the hudhook/imgui present-hook.
Motivation: it sidesteps the present-hook entirely (so it renders even where the overlay crashes — the
native-Windows RTX-3080 case in OVERLAY-RENDERING.md), and it's a path toward dropping imgui. The
substrate is `coop/native_draw.rs`; the marker feature is `coop/features/native_nameplates.rs`, gated by
`[nameplates] enabled` (on by default — the shipped nameplate). *(At spike time this was a
config-only `native_spike` knob alongside the imgui overlay nameplates; both the knob and the overlay
nameplates are now gone — see the header.)*

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
      **Shipped, on by default** (`[nameplates] enabled`).
- [reverted] **Native toasts / banners / menu** → back to the imgui overlay. The `ui::render`/`ui::input`
      libraries + the bitmap-font/Proggy pipeline + the screen-space bits of `native_draw` were removed
      (in git history if revived). The CSEzDraw `draw_text` finding (RVA `0x264efd0`, font-dead in retail)
      is recorded above and in [RE-GAME-UI.md](RE-GAME-UI.md).

[`native_draw::draw_billboard_disc`]: ../crates/unseamless-coop/src/native_draw.rs

## Current state

- **Shipped: native colored dot, on by default.** A camera-facing filled disc per player
  (`native_draw::draw_billboard_disc`), drawn from `coop/features/native_nameplates.rs` at
  `ChrIns_PostPhysics`. Marks your own head + every fully-loaded phantom. World-space, depth-tested,
  fixed world radius (shrinks naturally with distance — no LOD by design). Config `[nameplates] enabled`.
- **Per-peer color — keyed off a stable handle.** Each player reads as a distinct palette color
  ([`unseamless_core::palette::peer_color_for_id`], a fixed set of high-value hues with headroom over
  the 6-player cap). The key is the phantom's `ChrIns` pointer (stable per loaded phantom across frames),
  so a peer keeps its color as the roster reorders.
- **Peer feed — rung-3-gated.** Solo, `player_chr_set` holds only the local player (your own dot still
  draws). Real phantoms appear in a live session; mapping one to a peer *identity* needs the co-op/session
  core (see [COOP-CONNECTION.md](COOP-CONNECTION.md)).

## Remaining follow-up: color-by-SteamID (rung-3-gated)

The **one** remaining nameplate follow-up. The dot color is keyed off the phantom pointer today; swap it
for the peer's **SteamID** so a given player keeps their color for the whole session across reconnects.
This needs the session core to map a phantom → identity, so it's **rung-3-gated** — the TODO is marked at
the seam in `native_nameplates.rs` (`gather`). Do not implement before the session layer lands.

## Projection insights — preserved from the removed imgui pipeline

The deleted imgui projected-label nameplates produced rig-validated RE results worth keeping if a
screen-space surface (e.g. richer per-peer info) is ever built. Recorded here (the code is in git
history):

- **Projection conventions — rig-confirmed (2026-06-26).** A solo self-label check validated every
  convention the camera→NDC math left open: `forward` points where the camera looks; `fov` is the
  engine's **vertical** fov and **in radians**; world **`+Y` is up** (a head-clearance offset lifts the
  marker above the head); the `right`-vector sign is correct (not mirrored); and aspect is width/height
  (no squash). The label was upright, correctly placed, and tracked the player on foot and on horseback —
  **no knobs needed**. The camera basis was read from `CSCamera.pers_cam_1` (the composited camera the
  game renders from) via the SDK's named `CSCamExt` basis accessors, null-guarding the sub-camera pointer
  (an unwired deref is a segfault `catch_unwind` can't catch). `native_nameplates.rs` still reads the same
  `pers_cam_1` basis to billboard the disc.
- **The native renderer's limits** (why the dot, not text) are in the spike + Outcome sections above:
  `CSEzDraw::draw_text` is font-dead in retail (RVA `0x264efd0`), screen-space CSEzDraw UI swims, and it's
  per-primitive-slow for dense content — so a colored world-space dot is the right native nameplate.

[`unseamless_core::palette::peer_color_for_id`]: ../crates/unseamless-core/src/palette.rs
