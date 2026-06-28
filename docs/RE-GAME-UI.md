# RE: The Game's Real Screen-Space UI Text (Feasibility for Native UI)

**Lane:** `worker:re-game-ui` — static RE over the clean `eldenring.exe`. No rig was driven;
every "needs confirming live" item is flagged for the orchestrator.

**The question.** Our native UI text is bitmap glyphs rasterized to world-space `CSEzDraw` quads
— it swims and is slow for dense text (see [NAMEPLATES.md](NAMEPLATES.md)). The game's own UI text
(HUD, item names, damage numbers, prompts, menus) is pixel-perfect and GPU-textured with a font
that *is* loaded in retail. Can we reach that path to draw an arbitrary wide string (and/or a
filled quad) at a screen position? If yes it fixes both problems at once (swim-free + efficient).

## TL;DR verdict

- **No generic "draw string at (x, y) with the loaded font" primitive is reachable.** The
  pixel-perfect path is **Scaleform (Flash/GFx)** — a *retained-mode* movie system. Text appears by
  setting **named fields of authored movies**, not by calling a glyph emitter with a position. There
  is no standalone `draw(font, x, y, wstr)` to call from a frame task, and the only low-level glyph
  renderers are render-pass/pipeline-locked or dead (below). **Arbitrary free-placed pixel-perfect
  text via a game call: not feasible.**

- **But there IS a high-value pragmatic win the original framing missed:** the game already renders
  several **pixel-perfect, GPU-textured, world-tracking text channels** whose *string contents are
  writable model fields the SDK already charts* on the `CSFeMan` singleton. We don't draw — we
  **populate the game's own HUD model and let Scaleform render it.** The standout is the **friendly
  character tag-HUD** (`ChrFriendTagEntry`), which is *exactly a co-op nameplate* (name + role + HP
  bar + world-projected screen position + off-screen edge clamp), plus the **summon/system message
  queue** and a couple of single-line banner channels. These are swim-free, batched (≈free vs. our
  per-quad cost), and need **zero RE** — they're typed SDK fields. Two costs: you get the game's
  positions and look, not yours; and the tag-HUD array is the game's *own* per-frame source, so the
  nameplate use is realistically **augmenting** the tags it already draws for phantoms, not
  free-placing arbitrary ones (rig-gated — see flags).

**Recommendation:** keep the bitmap-font→quads substrate for fully-custom surfaces (the tabbed
menu), but **retarget nameplates and system-style notifications onto the game's own tag-HUD /
message channels** for pixel-perfect, free, swim-free output. This is a model-write, not a draw
hook. Several behavioral assumptions need a quick rig confirpass (flagged at the end).

---

## The three retail text systems (what each is, and why it is/ isn't reachable)

RTTI in `eldenring.exe` confirms all three are present (found via `static.py vtable`/`ascii`):

| System | RTTI | What it is | Reachable as "draw at xy"? |
|---|---|---|---|
| **Scaleform GFx** | `CSScaleformSystem@CS`, `CSScaleformValue@CS`, `CSScaleformMovieDef@CS`, `Value@GFx@Scaleform` | The real HUD + menus. Flash movies, retained-mode. | **No** — you set authored textfields, not a position. |
| **GuiFramework** | `GUIFont@GuiFramework`, `GUIWindowBase`/`GUIListView`/`GUIEditBox`/… (`GuiFramework v1.48.0`) | FromSoft's in-house **dev/debug** GUI (the debug menus). | **No (practically)** — render-pass-coupled widget framework, debug-styled, retail font UNCONFIRMED. |
| **CSEzDraw debug text** | (SDK `CSEzDraw`) | Debug primitive text. | **Dead** — font not initialized in retail. Confirmed prior. |

### 1. Scaleform GFx — the pixel-perfect path, but retained-mode

The HUD and menus are Flash movies driven by `CSScaleformSystem`. The front-end manager
(`CSFeManImp`, SDK `cs/fe_man.rs`) owns the FrontEnd scene (its `front_end_view` field is the
"object containing all scaleform data for the FrontEnd scene") and **feeds it text by id or by
string into fixed, authored fields**:

- Boss name, area-welcome popup, blinking message → **FMG text *ids*** (`i32`), not strings:
  `boss_health_displays[i].fmg_id`, `subarea_name_popup_message_id`, `blinking_message_id`. You can
  only show an *existing* localized entry, in *its* authored position.
- Other fields are **`MenuString`** — a custom string type whose own doc-comment says *"Custom
  string type used to interact with the scaleform."* These hold arbitrary text but still land in a
  **fixed authored field** (a specific on-screen slot/style), not an arbitrary x,y.

So Scaleform is the loaded-font, GPU-textured, swim-free renderer we'd want — but it is **retained
mode**: there is no exposed "rasterize this string at this pixel." To free-place text you'd have to
author/inject your own `.gfx` movie + textfield (or hijack an existing field and move it via the
`GFx::Value`/ActionScript API) and drive it inside the Scaleform advance/render pass. That is a
large, fragile, pipeline-locked effort with no SDK support — **not a practical primitive.**

### 2. GuiFramework / GUIFont — a dev tool, render-pass-coupled

`GuiFramework v1.48.0` (version string at `0x142bcf770`; its lib is statically linked into the
binary's *second* `.text` section at `0x144c0e000+`) is FromSoft's in-house developer GUI — the
thing behind the debug menus (`DbgMenu`, the `CSStageDebugGui*`/`GUIWindowBase`/`GUIListView`/
`GUIComboTweaker`/`GUIEditBox` widget zoo). **`GUIFont@GuiFramework` is not a glyph atlas — it's a
small style/handle object** (its ctor at `0x141da9380` builds a ~0x34-byte struct holding only a
default font size `0xc`=12 and color `0xffffffff`). The actual glyph rendering is a GuiFramework
routine that consumes a `GUIFont` + string + position **during the framework's own render pass**,
walking registered widgets/windows.

Why this is not a clean win:
- It is a **retained widget framework**, not a free `draw(font,x,y,str)`. Using it means the
  framework is initialized + ticking and you either register `GUIWidget`s or call its draw inside
  its render pass with a valid draw context — i.e. you'd be **hooking the debug-GUI render pass**,
  not calling a standalone function from a frame task.
- Output is **debug-styled chrome**, not game-styled UI.
- **Whether GuiFramework's render pass and its font are live in retail when no debug window is open
  is UNCONFIRMED** (rig item). The community "debug menu" mods suggest *some* GuiFramework text
  renders in retail when a debug window is force-enabled, but that's a different thing from "callable
  whenever we like."

Net: even in the best case this is a render-pass hook + widget model with debug styling — strictly
worse than option (3) below for our needs. Not recommended.

### 3. CSEzDraw debug text — dead (unchanged)

Confirmed by the prior spike and not revisited: `CSEzDraw::draw_text` (RVA `0x264efd0`) enqueues but
**hard-faults at render because its debug font isn't initialized in the shipping build**. See
`native_draw.rs`. Dead end; left as-is.

---

## The pragmatic win: write the game's own HUD model, let Scaleform render it

The original lane framing assumed we'd need to *call a draw function*. The better lever is that the
game's Scaleform HUD is **model-driven**, and the model lives on the **`CSFeMan` singleton, already
charted by the SDK** (`#[shared::singleton("CSFeMan")]` on `CSFeManImp` in `cs/fe_man.rs`). We can
get that singleton from a frame task and write its text channels; the game's existing update task
copies them into the FrontEndView and hands them to Scaleform. Result: **pixel-perfect, GPU-textured,
font-loaded, swim-free, batched text — with no RE and no render hook.** The tradeoff is we inherit
the game's positions and styling for each channel.

The writable channels (all typed SDK fields on `CSFeManImp` / `FrontEndViewValues`):

**A. Friendly character tag-HUD — this is a co-op nameplate, already built by the game.**
`pub friendly_chr_tag_displays: [ChrFriendTagEntry; 7]`. Each entry the game already renders as a
floating tag over a friendly/summon character: `name_string: DLString`, `role_string: DLString`
(the role line — the propagated `TagHudData.role_string` is documented "eg. 'Duelist'"),
`screen_pos: F32Vector4` (X, Y, depth — **the game computes world→screen itself**,
including a line-of-sight raycast and an **off-screen→left-edge clamp** via `is_line_of_sight_blocked`
/ `is_not_on_screen`), `role_name_color` (1=white friend, 2=red), HP bar fields, `has_rune_arc`
icon, `voice_chat_state`, keyed by `field_ins_handle`. This is **everything our nameplate feature
reimplements with bitmap quads** — name, color, HP, world tracking, edge-clamp — rendered
pixel-perfect by the game. (Damage numbers, an explicit lane target, are the same subsystem:
`ChrEnemyTagEntry.damage_taken` and `TagHudData.last_damage_taken` drive the on-tag damage number.)
**Important caveat:** the SDK marks this array as the game's *own source* of truth ("Data used for
the friendly character tags / Will be copied to the friendly character tags in FrontEndView"),
populated from the summon roster — so the realistic use is **augmenting the tags the game already
draws for co-op phantoms**, not free-placing arbitrary labels for non-summon handles (rig items
1–3 decide which).

**B. Summon / system message queue — arbitrary string, system-message slot.**
`pub summon_msg_queue: SummonMsgQueue` (struct: `{ current: SummonMsgData, list:
CSFixedList<SummonMsgData, 4> }` — you push to `.list` / set `.current`, not write a bare entry) →
`SummonMsgData { priority: i16, force_play: bool, text: MenuString }`. The bottom "summoning…/has
arrived" system-message channel takes an **arbitrary `MenuString`** — a natural home for ER-voiced co-op session toasts (join/leave/return) at the game's
own system-message position and styling.

**C. Single-line banner channels.** `FrontEndViewValues.sword_arts_name_string: MenuString` (the
ash-of-war name popup) is arbitrary text in a fixed slot; `subarea_name_popup_message_id` /
`blinking_message_id` are arbitrary *FMG ids* (existing localized lines only) for the center-screen
area-name banner. Lower value but cheap repurposable surfaces.

### Concrete sketch (option A — nameplates via the friendly tag-HUD)

```rust
// frame task (PostPhysics-style phase). PSEUDOCODE — fields/singleton are real SDK; the
// write-discipline + lifetime are the rig-validation unknowns (see flags).
let fe = CSFeMan::instance()?;                 // #[shared::singleton("CSFeMan")]
for (i, peer) in peers.iter().take(7).enumerate() {
    let tag = &mut fe.friendly_chr_tag_displays[i];
    tag.field_ins_handle = peer.chr_handle;    // (assumed) game projects + LOS-checks this handle
    tag.name_string.set("Player 2");           // DLString — pixel-perfect, GPU text
    tag.role_string.set("");                   // or ping/SL/death-count
    tag.role_name_color = 1;                    // white
    tag.is_visible = true;
}
```

We'd stop drawing our own bitmap nameplates and instead keep these entries populated. The game owns
projection, edge-clamp, HP bar, and font — our nameplate `projection.rs` math becomes unnecessary
for this path (kept only for any fully-custom overlay).

---

## Reachability summary

| Goal | Path | Verdict |
|---|---|---|
| Arbitrary wide string at arbitrary screen x,y, pixel-perfect, via a game call | (none) | **Not feasible** — Scaleform is retained-mode; GuiFramework is render-pass-coupled debug; CSEzDraw text dead. |
| Pixel-perfect **co-op nameplates** (name/role/HP/color, world-tracked) | Write `CSFeMan.friendly_chr_tag_displays[]` | **Likely augment-only, no RE** — game owns the source array; clobber/registration risk, *rig-gated* (items 1–3). |
| Pixel-perfect **system/session toast** (arbitrary text, fixed slot) | Write `CSFeMan.summon_msg_queue` (`MenuString`) | **Promising, no RE** — *rig-confirm write discipline.* |
| Center-screen **area banner** (existing localized lines only) | `subarea_name_popup_message_id` (FMG id) | **Feasible, no RE**, FMG-id-limited. |
| Fully-custom tabbed **menu** at arbitrary layout | — | **No game path** — keep bitmap-font→quads. |
| Filled quad at screen pos | `CSEzDraw` screen-space (already shipped) | Already have it; the GPU-texture win only exists *inside* Scaleform, which we can't drive standalone. |

---

## Flag for live rig validation (orchestrator)

The model-write path is statically sound but its *runtime discipline* is unverified — these decide
whether option A/B actually work and should be probed on the rig before building on them:

1. **Repopulation / ownership.** Does the game's update task **overwrite** `friendly_chr_tag_displays`
   (and `summon_msg_queue`) every frame from its own source? If so we must write **after** that task
   each frame (phase ordering), or our strings get clobbered. Confirm which phase wins. *Lead:* the
   SDK exposes `CSFeManImp.disable_updates` ("Don't update intermediate `frontend_values` data each
   frame in the `CSMenuMan` update task") — a candidate switch to suppress the clobber, but note it
   guards the `frontend_values`/`FrontEndViewValues` channels (sword-arts banner etc.), **not** the
   `friendly_chr_tag_displays` / `summon_msg_queue` source arrays, so it doesn't cover the nameplate
   path. Verify scope on the rig.
2. **Do co-op phantoms already get friendly tags natively?** In real co-op the network summons are
   friendly characters — the game may *already* render these tags for them, which would make our
   custom nameplates redundant (just style/augment the existing ones). Needs 2-player.
3. **Does writing an entry for an arbitrary `field_ins_handle` render**, or only for handles the game
   has registered as summons? If the latter, free-placed labels for non-summon entities won't work.
4. **`DLString`/`MenuString` set semantics** from our side (allocation ownership, the
   `static_string` vs `allocated_string` split in `MenuString`) — confirm a safe write that the
   game frees/handles correctly (no UAF/leak).
5. **HUD-hidden / state interplay** — `hud_state`/`CSFeManHudState` and whether these channels are
   suppressed in menus or when HUD is off.

## How these results were derived (for re-derivation after a game patch)

- RTTI located with `scripts/re/static.py vtable '.?AV<Name>@<Ns>@@'` and `ascii`/`utf16` searches:
  `FontRepositoryImp@CS`, `FontResCap@CS`, `GUIFont@GuiFramework`, `CSScaleformSystem@CS`,
  `CSScaleformMovieDef@CS`, `CSScaleformValue@CS`. `GuiFramework v1.48.0` version string at
  `0x142bcf770`; GuiFramework lib sits in the **second `.text`** (`0x144c0e000+`).
- `GUIFont` is a style/handle object, not a glyph atlas: ctor `0x141da9380` initializes only a
  default size (12) + color (0xffffffff) — read from raw disasm, not decompiler output.
- The model-write channels are **typed SDK fields** (no offsets to re-derive): `cs/fe_man.rs`
  `CSFeManImp` (`#[shared::singleton("CSFeMan")]`) → `friendly_chr_tag_displays`,
  `enemy_chr_tag_displays`, `boss_health_displays`, `summon_msg_queue`, `subarea_name_popup_message_id`,
  `blinking_message_id`; `FrontEndViewValues.sword_arts_name_string`; `ChrFriendTagEntry`,
  `TagHudData`, `MenuString`. If a game update shifts layouts, re-verify against the SDK pin (same
  commit for `eldenring` + `fromsoftware-shared`).

**Clean-room note:** all findings above are written from reading raw disassembly + the public
`fromsoftware-rs` SDK and describing *behavior*. No decompiler/disassembler pseudocode was copied
into this doc or the repo.
