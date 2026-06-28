# Native UI Library (`unseamless_core::ui`)

A small, composable, **pure + host-tested** UI library that replaces the hudhook/imgui overlay. It
emits a renderer-agnostic **draw list** (rectangles + text runs) that the cdylib's
[`coop/native_draw.rs`](../crates/unseamless-coop/src/native_draw.rs) rasterizes via the game's own
`CSEzDraw` (no present-hook). This is the "base code everything else works off of": every native UI
surface (toasts, banners, the menu window, modals, the debug panel) is built from these components.

See [NAMEPLATES.md](NAMEPLATES.md) > native rendering for why we're off imgui and the per-primitive
`CSEzDraw` cost model that shapes the design (keep primitive counts down; merge; draw only what's shown).

## Layers

```
Layer 3  integration (cdylib, orchestrator-owned): feed viewport+metrics+app-state to ui::render,
         feed input events to ui::input, rasterize the returned DrawList via native_draw. Migrates
         each overlay surface off imgui. THIS IS NOT a worker lane — it's rig-coupled integration.
Layer 2  ui::input  (core, PURE): interaction/focus/navigation state -> selection + actions.
Layer 1  ui::render (core, PURE): primitives + layout + widgets + theme -> a DrawList.
Layer 0  native_draw (cdylib, EXISTS): draws a DrawList's rects/text via CSEzDraw screen-space.
```

**The view/controller split is the load-bearing design choice.** `ui::render` is the *view* (geometry
→ pixels); `ui::input` is the *controller* (input events → selection state + actions). They **share no
code** and never reference each other: they meet only at the integration layer, which passes plain data
between them (selected index, active tab, scroll offset — `usize`/`u32`/enums). This is what lets the
two be built in parallel.

## The draw-list contract (owned by `ui::render`)

`ui::render` produces a `DrawList` — a flat, ordered `Vec<DrawCmd>`:

- `DrawCmd::Rect { rect: Rect, color: Rgba }` — a filled rectangle (panels, highlights, dividers, the
  per-glyph quads of text if text is pre-rasterized, OR text stays a Text cmd — renderer's choice).
- `DrawCmd::Text { pos: [i32;2], text: String, face: Face, color: Rgba }` — a text run; the cdylib
  rasterizes it through `bitmap_font::shape`. (Prefer this over pre-rasterizing in core, so the draw
  list stays compact and native_draw owns the glyph→quad step it already has in `draw_text_screen`.)

Coordinates are **integer pixels, origin top-left, y-down** — matching `bitmap_font` and
`draw_text_screen`. native_draw maps a pixel rect/point to screen NDC via `ScreenSpace` (it already
does the aspect-correct mapping). A `Rect` is `{ x, y, w, h: i32 }`; `Rgba` is `[u8;4]`. The viewport
size (in pixels) is an input to layout; the integration layer picks it (e.g. a fixed 1920×1080 virtual
canvas that native_draw scales to the real viewport, like the overlay's font scaling).

## Component inventory (what we must support, and the overlay surface each replaces)

**Layout primitives (`ui::render`):**
- Stack (vertical / horizontal) with spacing.
- Padding / insets.
- Alignment (start / center / end) on each axis.
- Sizing: fixed, hug-contents, fill-parent.
- Anchoring to a viewport corner/edge (toasts top-right, watermark top-left, banners top-center).
- Centering (modals).
- Clip + scroll viewport (the log tab; offset + clip rect).

**Widgets (`ui::render`):**
| Widget | Replaces |
|---|---|
| `Label` (text run, face, color) | all text |
| `Panel` (bg fill + optional border + padding, optional title bar) | utility window, modal frame, toast/banner backgrounds |
| `Divider` (1px line/rect) | separators |
| `List` (rows; selected-row highlight; disabled-row dim; `key: value` rows) | Actions/Settings tabs, report groups |
| `Tabs` (tab strip + active-tab marker; content area) | the utility window tab bar |
| `Modal` (centered panel + options + selection) | the choice modal |
| `Banner` (top strip, severity-colored) | notification banners + rig-guide banner |
| `Toast` (corner stack, fade by remaining lifetime) | notification toasts (already native; re-express here) |
| `ScrollView` (clipped, offset content) | the log tab |
| `Marker`/dot | nameplate marker (world-space; may stay in native_draw — see note) |
| value/secret row (label + value, optional reveal) | settings rows, password row |

**Theme (`ui::render`):** one `Theme`/`Style` holding the palette (bg, panel, fg, accent/selected,
disabled/dim, and `Severity` → info/warning/error colors, reusing the spirit of
`unseamless_core::palette`), spacing/padding constants, border width, and the two faces
(`bitmap_font::Face::{Menu, Compact}`). Declare colors/metrics once; widgets read the theme.

**Interaction (`ui::input`):**
- Abstract input events: Up / Down / Left / Right / Activate / Cancel / NextTab / PrevTab (+ Page/Home
  if cheap). The cdylib maps keyboard/controller to these.
- A focus/selection cursor over a list of items that are enabled/disabled — **skips disabled**, wraps or
  clamps (pick + document).
- Tab switching across N tabs.
- A **modal focus stack**: while a modal is open it captures all input (the underlying menu doesn't move)
  until Activate/Cancel resolves it.
- Adjust (±) for range/numeric items (Left/Right on a selected range row).
- Scroll offset for a `ScrollView`.
- Output: the current selection state (selected index, active tab, scroll offset) as plain data, plus an
  `Action`/`Outcome` enum the app interprets. **Generalize the existing
  [`unseamless_core::menu`](../crates/unseamless-core/src/menu.rs)** (`select_next`/`select_prev`/
  `activate`/`adjust`/`MenuOutcome`/`action_rows`) — read it first; reuse its concepts, don't reinvent.

> **Nameplate marker note:** the overhead nameplate dot is *world-space* geometry (a billboarded disc in
> `native_draw`), not a screen-space UI widget. It stays in native_draw; the `Marker` widget here is only
> for any screen-space dot/icon use. Don't try to fold world-space nameplates into this 2D library.

## Won't-do (for now) — features we get free from imgui and are OK to drop

Michael confirmed these are droppable; we can revisit if we ever want them. Keeping them out is what
makes a small native lib tractable:

- **Window move / resize / snapping** — panels are **static, viewport-anchored** (fixed positions). No
  dragging, no resize handles, no the-main-menu home-snap. The layout system anchors to corners/edges
  and centers modals; it does not move windows.
- **Fade-in / fade-out animations** (and similar motion polish). Toasts may use a plain per-frame alpha
  derived from remaining lifetime (a static value, not an animation), but there is no animation/tween
  system, no easing, no transitions.
- **Other imgui polish** in that vein (hover states, drag affordances, ghost-box snap previews, etc.).

If we later want any of these, they layer on top of the static base without reshaping it.

## Conventions (both lanes)

- **Pure**: no game/OS/SDK deps. Lives under `crates/unseamless-core/src/ui/`. Runs on the host via
  `scripts/test-core.sh`.
- **Host-tested with ASCII-art**: like `bitmap_font`, tests must **render the output to an ASCII grid**
  (rasterize a `DrawList`'s rects + text onto a char canvas, e.g. `#`/`.`/glyph chars) and assert against
  an expected multi-line picture — so layouts and widgets are human-readable in test output and bugs are
  obvious. For `ui::input`, tests assert the selection/action sequence from an input-event sequence.
- ASCII strings only (the bitmap font covers printable ASCII; see OVERLAY-RENDERING.md).
- Keep `main` green: `scripts/test-core.sh`, `cargo build --release`, `cargo clippy --release -- -D warnings`.
- Depends on `unseamless_core::bitmap_font` for `Face` + text metrics (same crate).

## Parallel build lanes

Two **independent** workers (they share no code; module dirs are disjoint; the shared module files
`ui/mod.rs` + `lib.rs` are pre-committed so neither worker edits them):

- **`ui::render`** — `crates/unseamless-core/src/ui/render/` : primitives, DrawList, layout, theme, all
  widgets. Takes selection/active-tab/scroll as plain input *data*; emits a DrawList. Owns the
  draw-list contract above.
- **`ui::input`** — `crates/unseamless-core/src/ui/input/` : the interaction/focus/nav model. Pure
  logic over item counts/enabled-flags/tabs; emits selection state + actions. No geometry, no
  dependency on `ui::render`.

Integration (orchestrator): once both land, wire native_draw to rasterize `ui::render`'s DrawList and
the cdylib input to drive `ui::input`, then migrate overlay surfaces one at a time on the rig.
