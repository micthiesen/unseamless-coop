# In-Game Overlay Rendering

How we draw our own 2D UI — the session-action menu ([`menu.rs`](../crates/unseamless-core/src/menu.rs)),
notification toasts/banners ([`notifications.rs`](../crates/unseamless-core/src/notifications.rs)), and
later overhead player nameplates — on top of Elden Ring by hooking the game's **DirectX 12** present
path, and getting that to work under **Proton/vkd3d**. This is the single biggest UI dependency: it's
the renderer those two host-tested models have always assumed but never had.

**Status: the renderer shipped.** `coop/overlay.rs` draws the session-action menu, notification
toasts/banners, the read-only settings view, and a live log tail through **hudhook (DX12 present-hook)
+ Dear ImGui** — the decision this note worked through, now built and wired. What remains unverified is
the **Proton/vkd3d rig behavior**: the game-internal and Proton claims below are either grounded in the
pinned `fromsoftware-rs` SDK source (cited as such), in open-source overlay code we read and use (cited,
license noted), or are behavioral inferences to confirm on the rig (hedged) — that rig confirmation
(does the box render under vkd3d, does input capture feel right) is the open part, not the renderer
choice. Per [CLAUDE.md](../CLAUDE.md) > Clean-room hygiene: we reimplement from behavior + public
SDK/open-source, never from ERSC's bytes (it's closed + Themida-packed — there's nothing to copy here
anyway; ERSC ships its own DX renderer hook we don't get to see).

> Why we own this at all: our session actions are an **overlay menu**, not ERSC's in-game items, and
> our notifications are toasts/banners, not (only) native game messages
> ([ARCHITECTURE.md](ARCHITECTURE.md) > Divergences). Both `menu.rs` and `notifications.rs` are written
> as pure models whose docs say "a renderer draws these each frame." That renderer is this doc.

## The Constraint That Drives Everything

Elden Ring renders with **DirectX 12**. On the rig (Linux + Steam + Proton) that D3D12 is translated
to **Vulkan by vkd3d-proton** (D3D12 → Vulkan). So an overlay that hooks the game's *D3D12* objects
isn't hooking real Direct3D — it's hooking vkd3d-proton's D3D12 *implementation*, which then drives
Vulkan underneath. That layering is the load-bearing risk: a DX12 present-hook that works natively on
Windows can still misbehave on vkd3d (timing, swapchain extensions, descriptor-heap paths). The good
news, detailed below, is that the standard Rust overlay crate explicitly targets Wine/Proton and the
most prominent ER tool built on it reports working on Steam Deck. The caveats are real but bounded.

## hudhook: The Standard Choice

[`hudhook`](https://github.com/veeenu/hudhook) (by veeenu, who also wrote the Elden Ring practice
tool) is the de-facto Rust overlay-hook framework, and **the SDK we already depend on uses it.** The
`fromsoftware-rs` `debug` crate declares `hudhook.workspace = true` and its `DebugDisplay` trait
renders widgets through `hudhook::imgui::Ui` (`crates/debug/src/display.rs` at our pin). So hudhook is
already in the SDK's dependency graph and is the path of least resistance.

What it is, from its own docs:

- **Backends:** "Currently supports DirectX 9, DirectX 11, DirectX 12 and OpenGL 3. **Runs on Windows
  and Wine/Proton.**" DX12 is the one we need; the Wine/Proton line is the one that matters most for
  the rig. (No Vulkan backend — it hooks the *D3D12* side, which vkd3d then translates. That's fine
  for us; see Proton section.)
- **UI toolkit:** **Dear ImGui only.** "At the moment, the only UI toolkit supported is dear imgui,
  but there are plans to support egui in the future." So today, using hudhook means imgui, not egui
  (see "egui vs imgui" below — this is a real divergence from the project's stated egui lean).
- **Maintained:** version **0.9.1** (released May 2026), 18 releases, active. Not abandonware.
- **Used by real Souls tools:** `eldenring-practice-tool` (DX12), `darksoulsiii-practice-tool` (DX11),
  and others all build on it. The ER tool is the closest precedent to us: same game, same DX12, same
  hudhook, and it ships a `dinput8.dll` proxy install — the exact slot we occupy.

### Cross-compile: validated (spike)

`hudhook` 0.9.1 (`default-features = false, features = ["dx12"]`) **cross-compiles cleanly to
`x86_64-pc-windows-gnu` via mingw** — confirmed in a throwaway cdylib on the Linux rig, in both the
dev profile and the shipping profile (`lto = true`, `opt-level = "z"`, `strip`; shipping is now
`panic = "unwind"` — see docs/FFI-UNWIND-AUDIT.md), producing a valid stripped PE32+ DLL. It resolves `windows` **0.62.2**, which matches the cdylib's
existing `windows = "0.62"` pin (no version split). So the overlay is buildable on our normal
Linux cross-toolchain — no native-Windows build host needed.

**Hard placement constraint this surfaced:** hudhook vendors **minhook (C)** and **imgui-sys (C++)**,
both compiled via `cc`/`cc`-`g++` for the *Windows target only* (a native-host build of hudhook
fails, as expected). So hudhook must live **only in the windows-only `unseamless-coop` cdylib, never
in `unseamless-core`** — adding it to core would break `scripts/test-core.sh` (host build/test).
This aligns with the existing crate split (core = no OS/SDK deps); just don't violate it for the
overlay. Mind the build-time cost too: hudhook pulls in `tracing-subscriber`/`regex`/`imgui` and two
C/C++ vendored libs.

### How It Hooks (DX12)

hudhook installs **function detours** on the rendering path. For DX12 it detours three functions
(confirmed from `src/hooks/dx12.rs`):

1. **`IDXGISwapChain3::Present`** — the per-frame "frame is done, show it" call. This is where the
   overlay draws each frame.
2. **`IDXGISwapChain3::ResizeBuffers`** — to rebuild render targets on resolution / fullscreen change.
3. **`ID3D12CommandQueue::ExecuteCommandLists`** — needed to *find the command queue*, which is the
   classic DX12-hook problem: unlike DX11, the command queue isn't reachable from the swapchain. The
   crate solves it with a small state machine — on `ExecuteCommandLists` it scans up to ~512 pointers
   of the swapchain's memory looking for the queue pointer, logging "Found command queue pointer in
   swap chain struct at offset +0x…". Once it has both swapchain and matching queue, it builds its own
   render target/descriptor heap and starts drawing in the `Present` detour.

The detours themselves are installed by finding the vtable entries for these COM methods (hudhook
creates a dummy device/swapchain to read the real function addresses, then trampolines them). We don't
have to implement any of this — it's the crate's job.

### Input Capture

The overlay needs to *steal* mouse/keyboard while open (so navigating the menu doesn't also move the
character / swing the sword), then give input back when closed. hudhook does this the standard
ImGui-overlay way: it **hooks the window procedure (`WndProc`)** via the game's HWND, feeds messages
to ImGui, and consults ImGui's `WantCaptureMouse` / `WantCaptureKeyboard` / `WantTextInput` to decide
whether to swallow a message or pass it to the game. It exposes bitflags for which message types to
filter. The well-known gotcha (keyboard leaking to the game while the mouse is correctly captured) is
an ImGui-integration issue the crate handles, but it's worth a rig check that, say, WASD doesn't move
the character while our menu is focused.

This dovetails with the SDK: `CSWindowImp` (`cs/window.rs`) exposes `window_handle` (the HWND), so if
we ever need the window handle ourselves it's a named field — but hudhook finds the window on its own.

## Does It Work Under Proton / vkd3d?

**Short answer: yes, with caveats, and there's strong precedent.** The closest real-world data point:
the **Elden Ring practice tool "fully supports Linux and should run on Steam Deck seamlessly"**, run
via `protontricks-launch --appid 1245620 jdsd_er_practice_tool.exe`. That tool is hudhook + DX12 +
this exact game, so the core "DX12 present-hook overlay over vkd3d-proton" path is proven on the same
title we target. hudhook's own README claiming "Runs on Windows and Wine/Proton" is corroborated by
that.

Where the friction lives (all to verify on the rig, where the game actually runs):

- **The command-queue scan over vkd3d's structs.** hudhook finds the command queue by scanning the
  *swapchain object's* memory layout. That layout is **vkd3d-proton's**, not Microsoft's DXGI, so the
  offset differs from Windows — but the scan is offset-agnostic (it searches a range), so this should
  survive translation. *Inference, confirm on rig:* watch for the "Found command queue pointer…" log
  line; its absence means the scan failed under vkd3d.
- **vkd3d swapchain-extension churn.** Recent vkd3d-proton/Proton combos have produced black-but-
  running DX12 screens fixed by disabling swapchain extensions, e.g.
  `VKD3D_DISABLE_EXTENSIONS=VK_KHR_present_id,VK_KHR_present_wait %command%` or disabling
  `EXT_swapchain_maintenance1`. If the *game itself* renders black with the overlay injected, this
  class of env-var workaround is the first thing to try (it's a vkd3d issue, not a hudhook bug).
- **GPU/driver-specific ImGui-DX12 crashes.** There are upstream reports of ImGui's DX12 backend
  crashing the GPU when initialized from a present-hook on some driver/GPU combos (notably newer AMD;
  NVIDIA/Intel less so). Our rig is **NVIDIA (RTX 5080, nvidia-open)** per the host notes, which is on
  the safer side, but it's a known failure mode to keep in mind if init crashes.
- **Pin the Proton/vkd3d versions that work.** DX12-over-Proton regressions come and go with Proton
  releases. Once a combination renders the overlay cleanly, record the exact Proton + game version in
  [RIG-RUNBOOK.md](RIG-RUNBOOK.md) and treat a Proton bump like a game bump (re-verify).

### If the DX12 Hook Proves Too Fragile (Fallbacks)

Ranked by how much we'd want them (we almost certainly stay with hudhook; these are the escape hatches):

1. **`CSMenuManImp::display_status_message(i32)` — the native, no-overlay path.** This is **charted and
   callable at our pin** (`cs/menu_man.rs`, RVA `cs_menu_man_imp_display_status_message`), and it drives
   the game's own big center banners via `STATUS_MESSAGE_*` constants. It needs **no renderer at all** —
   it's the game drawing for us. It can't render an interactive menu or styled toasts, but it's the
   natural backend for *simple* notifications (and is discussed in
   [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md)). **Decided won't-do (2026-06-26):** we will *not*
   ship this as a degraded notification fallback — it's not worth the added surface for a path the
   hudhook overlay already covers (see [ROADMAP.md](ROADMAP.md) > Won't-do). This entry stays as the
   RE record that the call is charted/callable, kept only as a genuine last-ditch escape hatch if the
   DX12 hook itself ever proves unshippable on the rig — not as a planned complement.
2. **`CSEzDraw` (SDK, `cs/rend_man.rs`) — world-space debug primitives.** `RendMan.debug_ez_draw`
   exposes `draw_line` / `draw_sphere` / `draw_capsule` / etc. via charted RVAs. This is a **3D
   world-space** debug drawer (lines/shapes in the scene), **not a 2D screen-space UI layer** — wrong
   tool for a menu or toasts. But it's plausibly relevant *later* for **overhead nameplates** if we
   project a marker into the world rather than screen-space text. Note as a maybe for nameplates only.
3. **A separate transparent overlay window (the MangoHud-style / external route).** Instead of hooking
   the game's renderer, draw into our own borderless transparent always-on-top window, or ship a Vulkan
   layer (`VK_LAYER_*`) like MangoHud. Under Proton this is awkward: a separate Win32 window inside the
   prefix fights the game's fullscreen/focus, and a Vulkan layer is an external-to-our-DLL deployment
   that can't read game state in-process the way a frame task can. Treat as last resort; it loses the
   tight "draw using state I just read this frame" coupling that the hook gives us.

## egui vs imgui

**Resolved: imgui via hudhook, shipped.** Early notes leaned egui, but **hudhook is imgui-only** (egui
is on its roadmap, not shipped as of 0.9.1), so the overlay ships on Dear ImGui. The reasoning that
settled it:

- **Use hudhook → use imgui.** Lowest risk by far: it's what the crate ships, what the SDK's debug
  tooling already uses (`hudhook::imgui::Ui`), and what the ER practice tool proves on Proton. `menu.rs`
  and `notifications.rs` are renderer-agnostic (`MenuRow`/`Toast`/`Banner` are plain data), so wiring
  them to imgui widgets was mechanical — imgui-rs's `ui.window().build(|| ui.text(...))` maps directly
  onto a list of rows and a stack of toasts. **This is what we did.**
- **Insist on egui → you leave hudhook's paved path.** Options are a separate egui-DX12 hook crate
  (e.g. `egui_hooks` / an egui-d3d12 renderer) bolted onto a hand-rolled present detour, or wait for
  hudhook's egui support. Both mean owning more of the render/hook plumbing and re-proving Proton
  compatibility from scratch — exactly the fragile part hudhook already solved. Not worth it for a
  menu + toasts.

The core models stayed renderer-agnostic, so the choice remains swappable if hudhook ever ships egui
support and there's a concrete reason — but there's no plan to revisit. (ARCHITECTURE.md's Divergences
already describe the shipped surface as an "ImGui overlay … via hudhook.")

## Injection Fit (Coexisting With Our Loader + Task System)

We already load as the game's **`dinput8.dll` proxy** and register **frame tasks** via the SDK
(`CSTaskImp::run_recurring`). An overlay hook coexists with that cleanly — they touch different
machinery:

- **No proxy conflict.** The ER practice tool's own proxy-DLL install *is* `dinput8.dll`. We can't run
  two `dinput8.dll` proxies, but we don't need to — **we install the present hook ourselves from inside
  our existing DLL**, the same way we install everything else. hudhook is a library; we call its hook
  setup at install time. There's no second DLL.
- **Set up the hook at install, alongside task registration.** In `coop/app.rs` `install` (on the
  short-lived init thread, off the loader lock — [CLAUDE.md](../CLAUDE.md) > safety invariants), after
  we get `CSTaskImp`, also kick off hudhook's hook installation. hudhook runs its draw on the **Present
  detour**, which is a *different thread/phase* than our `CSTaskGroupIndex` frame tasks. That's fine and
  even desirable: tasks **mutate game state** in a chosen frame phase (the safety model is frame
  ordering); the overlay only **reads our own app-state models** (`menu.rs`/`notifications.rs`) and
  draws. Keep the rule that the overlay never writes game state from the Present detour — game mutations
  stay in tasks, drawing stays in the present hook.
- **Shared app state is the seam.** The `Notifications`/`Menu` live in app state (per their module
  docs: "one instance lives in the cdylib's app state"). Frame tasks push into them; the present-detour
  render loop reads them. That cross-thread read needs a `Mutex` (or equivalent) around shared app
  state — a `try_lock` in the render loop that skips a frame on contention is the conservative choice
  (never block the present thread). `notifications.rs::tick` should be driven by a **frame task** with
  `FD4TaskData::delta_time` (per its docs), *not* by the render loop, so toast aging stays on the game
  clock.
- **Same `mem::forget` discipline.** Like the task handle, the overlay hook must stay resident for the
  process lifetime; don't unhook on `DLL_PROCESS_DETACH` (we never get a clean detach anyway, and
  unhooking a live present path is a use-after-free risk). Install once, leave it.

## Minimal Plan (First Overlay Milestone)

Goal: **"a box with text drawn over the game" on the rig**, then layer the real models on. Each step is
rig-verifiable via the log + a screenshot ([RIG-RUNBOOK.md](RIG-RUNBOOK.md), `/test-loop`).

1. **Add hudhook to the cdylib.** `hudhook` as a dependency of `unseamless-coop` (the cdylib only — the
   core crate stays game/OS-free). Pin it; the SDK already pins a hudhook in its workspace, so prefer a
   compatible version to avoid two imgui builds.
2. **Static box.** In `coop/app.rs install`, install hudhook's DX12 hook with a trivial
   `ImguiRenderLoop` that draws one window with `ui.text("unseamless-coop overlay alive")`. Log when the
   hook installs and (hudhook logs) when the command queue is found. **Milestone: see the box over the
   running game on the rig.** This is the make-or-break Proton test — if it renders, the hard part is
   done.
3. **Toggle + input capture.** Bind a key to show/hide, and confirm on the rig that while the overlay is
   open, movement/attack input does *not* reach the game (WASD doesn't move the character), and that
   closing it returns control. (ER practice tool uses a hold-`RShift` reveal; pick our own.)
4. **Render `notifications.rs`.** Wire the present-detour render loop to read shared `Notifications` (via
   `try_lock`) and draw `toasts()` as a corner stack + `banners()` as a top strip, colored by
   `Severity`. Drive `tick(delta)` from a frame task, not the render loop. (Originally this step also
   planned a `CSMenuManImp::display_status_message` complement for plain messages; that native-banner
   path is now a **won't-do** — see [ROADMAP.md](ROADMAP.md) > Won't-do — so the overlay is the sole
   notification surface.)
5. **Render `menu.rs`.** Draw `Menu::rows(cfg, ctx)` as a list (selected row highlighted, disabled rows
   dimmed, settings showing `value`), and forward the toggle/nav keys to
   `select_next`/`select_prev`/`activate`/`adjust`. The model already returns `MenuOutcome`; the cdylib
   turns those into session actions / config writes. Home the cursor on open (`Menu::home`).
6. **Overhead nameplates (projection rig-confirmed 2026-06-26).** Screen-space text from projected peer
   world coordinates on this same imgui draw surface (a background draw list), via the host-tested
   `unseamless_core::projection` (camera data is the SDK `cs/camera.rs`). Base styling is shipped; the
   richer design (per-peer colors, distance LOD to a dot, off-screen edge indicator, real
   name/ping/SL/death) rides on the co-op core. Full design + status: [NAMEPLATES.md](NAMEPLATES.md).

## Shipped UI Behavior (Final State)

How the rendered surfaces actually behave now, beyond the milestone plan above.

### Actions tab: dynamic, collapsed, hidden-not-greyed

The Actions tab renders from `unseamless_core::menu::action_rows(ctx) -> Vec<ActionRow>` (host-tested),
**not** the static `Menu`. The rule is **hide by session state, disable by readiness**:

- **Paired verbs collapse into one stateful row.** Lock⇄Unlock is a single row whose label *and* emitted
  action flip on `ctx.world_locked`; PvP / PvP teams / Friendly fire each show `on`/`off` and emit the
  single `Toggle*` action. So the player sees one row per concept, reflecting current state.
- **Inapplicable rows are hidden, not greyed.** Solo (out of session) → `Open world` / `Join world`
  (shown even at the title screen, enabled only once Steam is ready and in-game). In a session → `Leave
  world`; as host, additionally the four collapsed toggles; a joiner sees only `Leave world`.
- The toggle rows' state comes from `SessionContext.{world_locked, pvp_on, pvp_teams_on,
  friendly_fire_on}`, which are **always-`false` placeholders** until rung 3 sources them from the
  session FSM (see [COOP-CONNECTION.md](COOP-CONNECTION.md)). The actions themselves are still inert
  pending rung 3.

### Debug tab: detail panes independent of the summary

The debug report is published by a game-thread feature only when the overlay says one is **wanted**
(`crate::debug_panel::report_wanted`, true when the summary panel **or any detail pane** is showing),
so the summary panel and a detail pane each light up the feed on their own; a detail pane no longer
depends on the summary panel being open. The published snapshot is cached **per publish-version**: the
publisher bumps a monotonic counter at its ~10 Hz cadence and the Present-thread overlay deep-clones a
new report only when that counter advances, turning a per-frame clone of the whole report into a
per-publish one.

### Ailment display (rig-confirmed)

The status section shows accrued ailment **buildup**, computed as `gauge_max - gauge` because
`PlayerGameData.resistance_gauges[i]` is the resistance *remaining* (full at rest, depleting as buildup
accrues), not the buildup itself. **Rig-confirmed:** a clean player reads "none building or active", and
applying one ailment makes its buildup climb 0 → max. Only ailments that are building or procced are
listed, so the panel stays quiet when clean.

### Rendered strings are ASCII-only (font constraint)

The overlay's crisp UI font is a **printable-ASCII subset of Spleen 8x16**, with no glyph for the em
dash (`—`) or the ellipsis character (`…`), so either renders as a missing-glyph `?`. So **every
user-facing banner/toast string must stay ASCII** (use `...`, not `…`; commas/parens, not em dashes).
This is a standing constraint for anyone adding notification copy or diagnostic fields, and the
diagnostic report is already built ASCII-only for the same reason (`crate::diag::build_report`).

## SDK Angle (At Pin `8c67a84`)

- **No swapchain / Present / D3D12 / imgui / egui surface in the game-struct SDK** — confirmed by grep
  over `crates/eldenring/src`. The overlay is necessarily **external** (hudhook), not an SDK field. The
  only render-adjacent things the SDK charts are `CSEzDraw` (world-space debug primitives, above) and
  `CSWindowImp.window_handle` (the HWND), neither of which is a 2D UI layer.
- **The SDK's `debug` crate already depends on hudhook** and uses `hudhook::imgui::Ui` — so adopting
  hudhook + imgui aligns us with the SDK's own tooling rather than introducing a new dependency family.
- **`CSMenuManImp::display_status_message(i32)` is charted/callable** (`cs/menu_man.rs`) — the native
  banner path, no overlay required. RVA-backed, so it assumes the rig's game version matches the SDK's
  RVA bundle (ER 2.6.2.0 WW / 2.6.2.1 JP); confirm before relying on it. (We decided **not** to ship it
  as a notification fallback — see [ROADMAP.md](ROADMAP.md) > Won't-do; this is RE record only.)

## Status & Next Steps

- [ ] Rig milestone #1: add hudhook to the cdylib, install the DX12 hook, draw a static text box over
      the running game. Confirm it renders under the rig's Proton/vkd3d (the make-or-break test).
- [ ] Capture the working Proton + vkd3d + game version in RIG-RUNBOOK.md; record any
      `VKD3D_DISABLE_EXTENSIONS` / swapchain-extension workaround needed.
- [x] **Input capture solved deterministically** via hudhook's `message_filter`: while the utility
      window is open we return `MessageFilter::InputAll` (game ignores movement/attack); closed, we
      return `empty()`. hudhook always feeds imgui *before* consulting the filter, so backtick-to-close
      still registers — the keyboard-leak gotcha doesn't apply. Rig-verify the actual in-game feel.
- [~] **Won't-do:** ship `CSMenuManImp::display_status_message` as a degraded notification fallback.
      Dropped 2026-06-26 — not worth the surface for a path the overlay already covers
      ([ROADMAP.md](ROADMAP.md) > Won't-do). The call stays charted/callable (above) as a last-ditch
      escape hatch only.
- [x] Wire `notifications.rs` into the render loop (shared state via `try_lock`; `tick` on a frame task).
- [x] Wire the Actions tab into the render loop + input (nav/activate → `MenuOutcome` → `actionq` →
      game thread). The tab renders from the dynamic `menu::action_rows(ctx)` (paired verbs collapsed,
      inapplicable rows hidden; see "Shipped UI Behavior" above), not the static `Menu`. Settings are
      shown read-only (synced/local); live editing deferred. Plus a live Log tab (`logbuf`). **Backtick**
      toggles the window; text enlarged via `set_window_font_scale`.
- [x] Decided egui vs imgui: **imgui via hudhook** (egui is hudhook-roadmap-only). Shipped on imgui;
      ARCHITECTURE.md's Divergences describe it as an "ImGui overlay … via hudhook."
- [ ] (Later) Overhead nameplates: world→screen projection (`cs/camera.rs`) or `CSEzDraw` markers.

## Sources

- [veeenu/hudhook](https://github.com/veeenu/hudhook) — backends ("DirectX 9/11/12 and OpenGL 3"),
  "Runs on Windows and Wine/Proton", imgui-only ("plans to support egui in the future"), v0.9.1.
- [hudbook (hudhook docs)](https://veeenu.github.io/hudhook/) — DX12 hook via detouring
  `IDXGISwapChain::Present`; the `ImguiRenderLoop`/`render(&mut self, ui)` example; egui-is-future note.
- [hudhook `src/hooks/dx12.rs`](https://github.com/veeenu/hudhook/blob/main/src/hooks/dx12.rs) — detours
  `IDXGISwapChain3::Present` + `ResizeBuffers` + `ID3D12CommandQueue::ExecuteCommandLists`; command-queue
  pointer scan over the swapchain struct.
- [hudhook proxy-DLL guide](https://veeenu.github.io/hudhook/advanced/proxy-dll.html) — `dinput8.dll`
  proxy pattern (the same slot we use).
- [veeenu/eldenring-practice-tool](https://github.com/veeenu/eldenring-practice-tool) /
  [DeepWiki](https://deepwiki.com/veeenu/eldenring-practice-tool) — hudhook + DX12 + this game;
  `dinput8.dll` proxy install; "fully supports Linux and should run on Steam Deck seamlessly" via
  `protontricks-launch --appid 1245620`; AOB version detection.
- [vkd3d-proton](https://github.com/HansKristian-Work/vkd3d-proton) and issues
  [#1878 (black/frozen DX12)](https://github.com/HansKristian-Work/vkd3d-proton/issues/1878),
  [#2872 (descriptor-heap black screen)](https://github.com/HansKristian-Work/vkd3d-proton/issues/2872)
  — swapchain-extension / `VKD3D_DISABLE_EXTENSIONS` workarounds.
- [ocornut/imgui #7207](https://github.com/ocornut/imgui/issues/7207) — ImGui DX12 present-hook GPU
  crash on some driver/GPU combos (AMD-leaning). [imgui #5674](https://github.com/ocornut/imgui/issues/5674)
  — keyboard-leak input-capture gotcha.
- Pinned SDK `fromsoftware-rs` rev `8c67a84` — `crates/debug/{Cargo.toml,src/display.rs}` (hudhook +
  `hudhook::imgui::Ui`), `crates/eldenring/src/cs/{rend_man.rs (CSEzDraw), window.rs (CSWindowImp),
  menu_man.rs (display_status_message), camera.rs}`, and a render-primitive grep over
  `crates/eldenring/src` (no swapchain/Present/imgui surface). Read directly.
</content>
</invoke>
