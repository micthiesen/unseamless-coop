# RE: Can CSEzDraw Draw Screen-Space Geometry?

Static-RE investigation over the installed `eldenring.exe` (clean target;
base `0x140000000`), tooling `scripts/re/static.py` (capstone/numpy). All addresses are from
the build installed on 2026-06-28; a game update will shift them (re-derive via the method notes
below). Findings are in my own words from reading disassembly — no decompiler output is
reproduced here.

## Verdict

**NO.** The game's CSEzDraw debug renderer has **no screen-space mode for filled geometry**
(triangles and lines traced end to end; fans, spheres, capsules, wedges, dodecadrons inferred —
see scope note below). Every geometry primitive stores raw positions and is transformed by the
**single shared camera view-projection matrix** at render time. Only the **text** primitive honors a coordinate mode
(`text_coord_mode`); the geometry path never reads it. There is also no separate 2D / screen /
ortho / sprite draw primitive in the EzDraw family.

So the holy-grail "submit a screen-space triangle and skip world projection" does not exist as a
flag we can set. `native_draw.rs`'s current camera-locked billboard remains the correct approach.
There is exactly one theoretical lever to true screen-space geometry (overwrite the shared VP
matrix around a flush) — it is invasive and needs live validation; details at the end.

## How geometry is drawn (what I traced)

The pipeline has three stages: **enqueue** (from any thread, under a lock) → command stored in a
double-buffered command buffer → **render** (at flush) walks the commands and submits vertices.

### Enqueue (`draw_triangle` `0x14264fb10`, `draw_line` `0x14264fdc0`)

Both do the same shape of work:

1. Lock the EzDraw `command_queue_lock` (the `DLPlainLightMutex` at `CSEzDraw+0x28`).
2. Pick the writable command buffer via `current_buffer_index` (`CSEzDraw+0x20`).
3. Allocate a command record in that buffer.
4. Store a **render-function pointer** as the command's first field — `0x14269abf0` for a
   triangle, `0x14269af70` for a line. (These look like a vtable slot but are bare code
   addresses; the flush calls them directly.)
5. **Copy the raw positions verbatim** (`movaps`) into the command: the 48-byte `Triangle`
   (origin + edge1 + edge2) for a triangle, the two endpoints for a line.
6. Unlock.

There is **no coordinate-mode read, no scaling, no transform** at enqueue. The positions are
stored exactly as passed. (Contrast: `draw_text` `0x14264efd0` does not store a simple record — it
calls a text-build helper `0x14269c210` that is coord-mode aware.)

### Render (`0x14269abf0` triangle, `0x14269af70` line)

At flush the stored render-fn runs with the command as `this`. It reads the stored positions,
reconstructs the absolute corners (for a triangle, origin / origin+edge1 / origin+edge2), and
hands them to a per-primitive **submit** worker: `0x142697660` for triangles, `0x1426980f0` for
lines. (A flag at `EzDrawState+0x34`, the `unk34` / draw-flags bit 7, optionally drives a second
submit pass — this is a depth/through-walls variant, not a coordinate concern.)

### Submit (`0x142697660` triangle, `0x1426980f0` line) — the decisive evidence

The submit worker is where any coordinate decision would have to live, and it is where the
contrast with text is unambiguous. The triangle/line submit:

- Reads from the `FD4EzDrawState` (reached via the render context, `state = [ctx+0x18]`) **only**:
  - `[state+0x28]` = `fill_mode` (0 fill / 1 wireframe), and
  - `[state+0x2c]` (`unk2c`) —
  used purely to pick a **pipeline/topology variant** (triangle-list vs line-list, a blend/depth
  variant). Neither changes coordinate space. (The `[state+0x34]` flag mentioned above is read by
  the render-fn, not the submit worker — different functions, so don't tally them as one field set.)
- Builds vertices by copying the world positions straight in with `w = 1.0` (`0x3f800000`) and the
  current color, then submits them to the render device.
- Fetches the **view-projection matrix** via the shared helper `0x14268d010`, which copies a 4×4
  matrix from `[[ctx+0x10]+0xd0]` and binds it as the vertex shader constant.

It **never reads `[state+0x90]`** (`text_coord_mode`) or any of the text fields. The geometry is
therefore always world-space, projected by the camera. This is exactly why our billboard "swims".

### The text path, for contrast (`draw_text` submit `0x14268dad0`)

The text submit reads the same `FD4EzDrawState` but **does** load `text_coord_mode` at
`[state+0x90]`, compares it against 5 and falls through to a default for out-of-range values, then
switches over the six `EzDrawTextCoordMode` values
(`ScreenSpace0/1`, `HavokPosition2/3`, `Normalized1080p`, `Normalized4k`) — it also consumes the
text-only fields `text_color` (`+0x94`), `font_size` (`+0x98`), and the `text_pos_*_scale` fields
(`+0xa0`, etc.). This per-coord-mode branch is the screen-space mechanism, and it is **private to
the text primitive**. Nothing in the geometry submit shares it.

(Offsets cross-checked against the pinned SDK `crates/eldenring/.../cs/rend_man.rs`:
`fill_mode` `0x28`, `text_coord_mode` `0x90`, `text_color` `0x94`, `font_size` `0x98`,
`text_pos_height_scale` `0xa0`. The disassembly reads exactly these.)

## What I ruled out (and how)

- **A coord-mode field on the geometry path.** Disassembled the **triangle and line** submit
  workers (`0x142697660` / `0x1426980f0`) end to end; the only `EzDrawState` fields they touch are
  `0x28`/`0x2c`. Confirmed text reads `0x90`, these two do not. **Scope:** the other ~16 geometry
  primitives (sphere/capsule/fan/wedge/dodecadron/etc.) were **not** read submit-by-submit; their
  "no screen-space mode" status is *inferred* from (a) their enqueues reading no text field and
  (b) all 18 submit workers fetching the same shared VP matrix via `0x14268d010` (below). That is
  strong but not a per-worker proof — a thorough pass would read each of the remaining submits for
  a `0x90`-style read.
- **A coord-mode-aware geometry enqueue.** Enumerated all 66 `.pdata` functions in the EzDraw
  cluster (`0x14264e000`–`0x142651000`). Only four read the text fields (`0x14264f600`,
  `0x14264f6c0`, `0x14264f770`, `0x142650bb0` — touching `0x90/0x98/0xa0`); all are text helpers.
  Every geometry enqueue (`0x14264fb10` triangle, `0x14264fdc0` line, and the sphere/capsule/
  fan/wedge/dodecadron siblings around them) reads none.
- **A separate 2D / screen / ortho / sprite draw object.** Walked the EzDraw RTTI/string set:
  the family is `FD4::EzDraw`, `CS::EzDraw` (`CSEzDraw`), `EzDrawManager`, `EzDrawState`,
  `FD4HkEzDrawState`, `EzDrawCommandBuffer`, `EzDrawRigidBodyDispBufferManager`. No `2D`, `Screen`,
  `Ortho`, or `Sprite` EzDraw type exists. There is no alternate screen-space primitive.
- **A per-command matrix.** The geometry command stores only positions + the render-fn pointer.
  The transform is fetched fresh at flush from shared context state, not from the command, so it
  cannot be overridden per primitive at enqueue time.

## The one theoretical lever (needs live rig validation)

All 18 geometry submit workers fetch their transform through `0x14268d010`, which reads a 4×4 from
`[[render_ctx+0x10]+0xd0]` — a **single shared per-frame view-projection matrix** in a render-param
block (`render_ctx+0x10`). (That helper also has a branch on `[[ctx+0x10]+0x210] != 0` selecting an
alternate transform-build path — possibly an explicit-matrix override slot; unconfirmed.)

In principle, overwriting that matrix with an orthographic/identity projection would make all
geometry render in NDC/screen space. But:

- It is **read at flush time**, not at our enqueue time, so we cannot bracket it from a normal
  frame task. It would require **hooking the EzDraw flush** to set the matrix → draw our commands →
  restore.
- It is **all-or-nothing per frame** — it would also relocate the game's own debug geometry that
  frame, so the hook would have to isolate our draws.

Overwriting the shared matrix is strictly more invasive than the current billboard and buys little
over it, so I do **not** recommend pursuing *that* unless the billboard's residual swim proves
unacceptable.

The `+0x210` override branch is a **distinct, possibly lighter** option and shouldn't be dismissed
with the matrix-swap: if it really is a per-frame explicit-matrix slot the geometry transform path
honors, supplying an orthographic matrix through it could be cheaper than overwriting the shared VP
around a flush hook. It is unconfirmed and equally rig-gated, but worth probing on its own merits.

If we ever pursue either, the live probe is: dump `[[render_ctx+0x10]+0xd0]` (the 4×4) during a
frame and confirm it equals the camera VP (compare against the SDK camera matrix), confirm it is
shared/rebuilt per frame, and test whether the `+0x210` branch accepts an override matrix. **All of
that is rig work for the orchestrator** — I did static analysis only.

## Needs live validation by the orchestrator

- Nothing here was run against the live game; it is static disassembly. The verdict (no
  screen-space geometry mode) is high-confidence from the code, but the negative is worth a
  one-line sanity check if ever doubted.
- The "matrix swap" lever above (the `[[ctx+0x10]+0xd0]` VP matrix and the `+0x210` override
  branch) is entirely unverified at runtime and would need the rig.

## Re-derivation notes (after a game update)

- Geometry entries were reached from the RVAs in the task brief: `draw_triangle 0x264fb10`,
  `draw_line 0x264fdc0`, `draw_text 0x264efd0`. From each enqueue, the stored `lea rcx,[rip+...]`
  gives the render-fn (`0x14269abf0` / `0x14269af70`); the render-fn's final `call` reaches the
  submit worker (`0x142697660` / `0x1426980f0`).
- To re-confirm the verdict after an update: disassemble the geometry submit worker and check
  which `EzDrawState` offsets it reads. Screen-space geometry would mean a read of
  `text_coord_mode` (`+0x90`) or an equivalent coord field — absent in this build.
- The shared VP fetch (`0x14268d010`, copies `[[ctx+0x10]+0xd0]`) is called by all 18 geometry
  submit workers; that call-set is a good landmark for re-locating the cluster.
