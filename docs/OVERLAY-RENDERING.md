# In-Game Overlay Rendering

How we draw our own 2D UI — the session-action menu ([`menu.rs`](../crates/unseamless-core/src/menu.rs)),
notification toasts/banners ([`notifications.rs`](../crates/unseamless-core/src/notifications.rs)), and
later overhead player nameplates — on top of Elden Ring by hooking the game's **DirectX 12** present
path, and getting that to work under **Proton/vkd3d**. This is the single biggest UI dependency: it's
the renderer those two host-tested models have always assumed but never had.

**Status: the renderer shipped and is verified on the rig (vkd3d/Proton).** `coop/overlay.rs` draws
the session-action menu, notification toasts/banners, the read-only settings view, and a live log tail
through **hudhook (DX12 present-hook) + Dear ImGui** (the decision this note worked through, now built
and wired). It renders correctly under vkd3d (rig baseline captured 2026-06-28). The open problem is
**native Windows**, where the present hook is fatal on NVIDIA hardware (see "Native-Windows Crash"
below). The game-internal and Proton claims below are grounded in the pinned `fromsoftware-rs` SDK
source (cited as such), in open-source overlay code we read and use (cited, license noted), or are
behavioral inferences (hedged). Per [CLAUDE.md](../CLAUDE.md) > Clean-room hygiene: we reimplement from
behavior + public SDK/open-source, never from ERSC's bytes (it's closed + Themida-packed, nothing to
copy here anyway; ERSC ships its own DX renderer hook we don't get to see).

> Why we own this at all: our session actions are an **overlay menu**, not ERSC's in-game items, and
> our notifications are toasts/banners, not (only) native game messages
> ([ARCHITECTURE.md](ARCHITECTURE.md) > Divergences). Both `menu.rs` and `notifications.rs` are written
> as pure models whose docs say "a renderer draws these each frame." That renderer is this doc.

## The Constraint That Drives Everything

Elden Ring renders with **DirectX 12**. On the rig (Linux + Steam + Proton) that D3D12 is translated
to **Vulkan by vkd3d-proton** (D3D12 → Vulkan). So an overlay that hooks the game's *D3D12* objects
isn't hooking real Direct3D — it's hooking vkd3d-proton's D3D12 *implementation*, which then drives
Vulkan underneath. That layering is the load-bearing risk: a DX12 present-hook that works natively on
Windows can still misbehave on vkd3d (timing, swapchain extensions, descriptor-heap paths). The
caveats are real but bounded — the standard Rust overlay crate explicitly targets Wine/Proton and the
most prominent ER tool built on it runs on Steam Deck (detailed below).

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
  survive translation. *Confirmed on rig (2026-06-28):* the scan found the CQ at offset **+0x8** under
  vkd3d, at **+0x138** on the WARP VM, and at **+0x140** on native NVIDIA (friend run 2026-07-01) —
  the offset-agnostic scan handled all three. On native it first *rejects* the game's second D3D12
  queue a few times (`Couldn't find command queue pointer` warns) — normal multi-queue noise, not
  failure; the failure tell is `Found command queue pointer` never appearing at all.
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
   `Severity`. Drive `tick(delta)` from a frame task, not the render loop. (The overlay is the sole
   notification surface; the once-planned `CSMenuManImp::display_status_message` complement is a
   **won't-do** — see "Fallbacks" #1 and [ROADMAP.md](ROADMAP.md) > Won't-do.)
5. **Render `menu.rs`.** Draw `Menu::rows(cfg, ctx)` as a list (selected row highlighted, disabled rows
   dimmed, settings showing `value`), and forward the toggle/nav keys to
   `select_next`/`select_prev`/`activate`/`adjust`. The model already returns `MenuOutcome`; the cdylib
   turns those into session actions / config writes. Home the cursor on open (`Menu::home`).
6. **Overhead nameplates — NOT on this overlay (shipped natively instead).** Nameplates are a per-player
   colored **dot** drawn by the game's own `CSEzDraw` renderer (world-space, depth-tested,
   present-hook-free — `coop/features/native_nameplates.rs`), not imgui projected labels. The earlier
   imgui screen-space projection path (`unseamless_core::projection` → `overlay.rs::draw_nameplates`) was
   removed; the only follow-up is color-by-SteamID (rung-3-gated). Full design + status:
   [NAMEPLATES.md](NAMEPLATES.md).

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

## Native-Windows Crash (Friend Tests 2026-06-27 + 2026-07-01)

> **Status: ROOT-CAUSED (mechanism) 2026-07-01 — see "WER Verdict" below.** The crash was never in
> the DX12 present path: the trace run showed the overlay initialize and render fine on native
> NVIDIA, and the friend's WER record then named an ACCESS_VIOLATION at **`XINPUT1_4.dll+0x9a65` =
> `XInputGetState+5`** — an **inline-hook collision** between our ilhook patch on `XInputGetState`
> (the overlay's controller capture, `input.rs`) and a second 5-byte hooker (likely Steam's
> gameoverlayrenderer). Fix direction: IAT hook instead. The older analysis below is kept for the
> record with refuted conclusions marked inline — "fatal at first hooked Present" and
> "NVIDIA-driver-specific" are both dead.

### Second Friend Run (2026-07-01, Trace Build): The Crash Is NOT At First Present

The exact run Part C of [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md) asked for: build
`eb61c07-dirty` v0.10.0, diag (debug-assertions + per-record flush, so the tail is trustworthy),
`level = "trace"`, overlay on, solo launch, same friend/NVIDIA box as the first test (RTX 3080 —
env re-confirm still open). One run, one crash. Raw bundle archived locally at
`reference/friend-crash-2026-07-01/` (gitignored; log `unseamless_coop-1782957572-6248.log`).

The timeline (t = seconds after first log line, 01:59:32.79Z):

```
t+0.0          logging up, config loaded, crashdump filter installed (t+0.03)
t+8.5          hudhook found IDXGISwapChain::Present (0x7ff939fed7e0); present hook installed
t+10.1         frame-1 gap: `Initialization context incomplete` + `Render error 0xFFFFFFFF`  <- same as rig
t+10.1..10.5   ~8x `Couldn't find command queue pointer ... (512 out of 512 readable)`       <- see below
t+10.5         `Found command queue pointer ... at offset +0x140` -> context Complete
t+12.7         overlay: hudhook initialize() reached (fonts baked -- ON NATIVE NVIDIA)
t+12.8         overlay: first render frame reached (render_inner); hooked presents flow
t+13.7         `ResizeBuffers` trampoline (mode switch; ~1.3s present pause, then presents resume)
t+15.0..15.56  steady hooked presents, ~25ms cadence (~40 Hz)
t+15.56        last `Call IDXGISwapChain::Present trampoline`
t+15.58        last log line (a SteamID retry, attempt 29/60) -- process dead, nothing further
```

What this run **proves**:

- **The first-present model (old hypothesis #1) is refuted on the friend's own machine.** The first
  hooked Present survived, the CQ matched, imgui init + font bake ran, our `render_inner` ran, and
  dozens of hooked presents flowed for ~3 seconds — including across a `ResizeBuffers`. The crash is
  a *later, separate* event, not the hook-activation fault the first-test logs suggested.
- **The CQ scan works on native NVIDIA: offset +0x140** (vs +0x8 vkd3d, +0x138 WARP — the
  offset-agnostic scan handled all three). The ~8 `Couldn't find command queue pointer` warns before
  the match are the scan rejecting the game's **second** D3D12 queue (`0x…fed20`), which also invokes
  `ExecuteCommandLists` on native — normal multi-queue rejection noise, *not* failure. The failure
  tell remains `Found command queue pointer` never appearing at all.
- **The first test's log-tail ambiguity is (probably) resolved.** The old v0.8.0 logs (info level, no
  flush, no breadcrumbs) ended right after the input-hook lines and we read that as "died at first
  present". Everything between the input hooks and this run's death is trace/debug-level or a
  breadcrumb that didn't exist in v0.8.0 — so those old logs are fully consistent with **this**
  timeline (init fine, died ~16s in). The "buffering vs died-earlier" fork below was likely a false
  dichotomy caused by log level.

The death signature — what did **not** appear (each absence is load-bearing):

- **No `crashdump:` line.** The `SetUnhandledExceptionFilter` handler (installed t+0.03) never fired.
- **No panic-hook output.** The global Rust panic hook (`logger.rs::install_panic_hook`) logs any
  in-process Rust panic; silence + per-record flush means **no Rust panic happened anywhere** (ours
  are additionally firewalled — `render_inner` is `catch_unwind`-wrapped and a caught panic logs).
- **No hudhook `Render error`** after the structural frame-1 gap — rendering never *reported* failure.
- **The last present-path line is a trampoline trace, followed 26ms later by an unrelated Steam-retry
  line.** At the ~25ms present cadence, the *next* present had entered but never reached its
  trampoline trace. So *if* the death was on the present thread, it was inside hudhook's render work
  (`prepare_render` / `GetBuffer` / engine render — before the trampoline), **not** in the game's
  original Present. But a fail-fast on any other thread leaves the identical signature, so this
  localizer is suggestive, not conclusive.

Ways a Windows process dies without tripping our filter, the panic hook, or any log line — the
remaining candidate mechanisms:

1. **Fail-fast** (`__fastfail`, exception code `0xc0000409` — GS-cookie stack-buffer-overrun check,
   invalid-handle, CRT abort). Raises no catchable SEH: bypasses our filter *and* any vectored
   handler. A GS failure inside the game/driver/mingw code is silent everywhere in-process.
2. **Our filter was replaced.** `SetUnhandledExceptionFilter` is last-writer-wins; the game, the
   NVIDIA driver stack, or steamclient can install their own *after* our t+0.03 install. A plain AV
   then dies through *their* filter, invisibly to us.
3. **Deliberate exit** — e.g. the game detecting `DXGI_ERROR_DEVICE_REMOVED` on its own fence/wait
   path and error-boxing/exiting. (Would usually also surface as hudhook `Render error`s on
   subsequent presents — none seen — so ranked below the first two.)

~~**New prime suspect: the post-`ResizeBuffers` present path.**~~ **[Superseded the same evening by
the WER verdict below — the crash isn't in the render path at all.]** (For the record: hudhook
0.9.1's `ResizeBuffers` detour is a pure pass-through and its per-frame `GetBuffer` holds no stale
back-buffer refs; the resize timing correlation was real but indirect — see below.)

### WER Verdict (Same Evening): An Inline-Hook Collision On `XInputGetState`

The friend's Event Viewer **Event 1001 (WER APPCRASH)** for the run names the crash outright:

```
P1: eldenring.exe   P2: 2.6.2.0          P3: 69e21187
P4: XINPUT1_4.dll   P5: 10.0.26100.8521  P6: ce6efde6
P7: c0000005 (ACCESS_VIOLATION)
P8: 0000000000009a65   (fault offset into XINPUT1_4.dll)
```

**Not the DX12 present hook. Not NVIDIA. The faulting module is Windows' own `XINPUT1_4.dll` — the
DLL whose `XInputGetState` we inline-hook for the overlay's controller nav** (`input.rs::install_xinput`,
ilhook Retn detour). The DX12 path was healthy the whole run.

The fault offset is exact and damning. We pulled the byte-identical DLL build from the Microsoft
symbol server (winbindex → `msdl.microsoft.com/download/symbols/xinput1_4.dll/CE6EFDE61c000/` —
timestamp `ce6efde6` matches WER P6, sha256 matches winbindex; archived in
`reference/friend-crash-2026-07-01/`) and disassembled:

- `XInputGetState` export RVA = **`0x9a60`**; its first instruction is the classic 5-byte
  hot-patchable prologue `48 89 5c 24 08` (`mov [rsp+8], rbx`), so `+0x9a65` (`push rsi`) is the
  **second instruction — the exact jump-back target of a standard 5-byte `E9 rel32` inline hook's
  trampoline.**
- Fault RVA = **`0x9a65` = `XInputGetState + 5`**.

So: **a second XInput hooker with the 5-byte hook convention collided with our 14-byte ilhook patch
on the same entry.** Its trampoline executes the saved 5-byte first instruction, then jumps back to
`entry+5` — which, after our ilhook Retn patch (a 14-byte absolute `jmp [rip+0]`+addr), is the middle
of our patch bytes. Executing that garbage → AV with RIP = `XInputGetState+5`, exactly WER's P8.
(ilhook's glue itself was exonerated first: its prolog normalizes RSP parity and its own `movaps`
saves ran on every poll for 7s.)

The leading candidate for the other hooker is **Steam's `gameoverlayrenderer64.dll`** (hooks
`XInputGetState` for overlay/Steam-Input controller support, and can engage late — plausibly around
the t+13.7s fullscreen `ResizeBuffers`, which is what made the resize look like the trigger). The
~7s of healthy hooked polling before death is consistent with the other detour fast-pathing (e.g.
"no controller"/emulated state) and only taking its jump-back trampoline path on a later event
(controller arrival / overlay engage). Actor + install order are unconfirmed pending the WER
report's module list; the *mechanism* is confirmed by the address arithmetic.

This retroactively explains every platform datum: native Windows + Steam crashes (collision);
the vkd3d rig survives (Wine's different xinput + overlay stack, no colliding hooker); the WARP
`dx12-harness` ran clean because **it never installs the input hooks at all** — it only ever tested
the present hook + font bake, which were never the problem. "NVIDIA-driver-specific" was a red
herring. It also likely explains the **first (2026-06-27) crashes**: same build path, same hook,
same machine — not a first-present fault.

**The fix direction: stop inline-hooking `XInputGetState`; hook the game's import (IAT) instead.**
We only need to observe/blank what *the game* reads — `eldenring.exe` statically imports
`XINPUT1_4.dll`, so patching its IAT entry for `XInputGetState` gives us the same interpose with
**no function-body bytes touched**: immune to third-party inline hookers by construction (they
patch the function body; we'd own only the game's call slot), a plain typed calling convention (no
ilhook register glue), and trivially reversible. The DirectInput `GetDeviceState` hooks are vtable-
probed COM methods (different mechanism, no evidence of conflict) and can stay.

**Next steps (in order):**

1. **Reimplement the XInput capture/blank as an IAT hook** on `eldenring.exe`'s
   `XINPUT1_4.dll!XInputGetState` import (fix above). Interim mitigation for testers remains
   `[debug] overlay = false` (skips the input hooks entirely).
2. **Friend-side confirmation, no new run:** the WER report folder
   (`C:\ProgramData\Microsoft\Windows\WER\ReportArchive\AppCrash_eldenring.exe_…`) — its
   `Report.wer` lists the **loaded modules**, confirming/denying `gameoverlayrenderer64.dll` (or
   another XInput hooker: NVIDIA App, RTSS, DS4Windows/HidHide). Also ask: controller plugged in /
   turned on around the crash? Steam running + logged in (the log's 16s of `ISteamUser` null is
   still unexplained)?
3. **Harden `crashdump.rs`:** the AV *was* a plain SEH exception that reached WER, yet our
   `SetUnhandledExceptionFilter` handler logged nothing — so something replaced our filter after
   t+0.03s. Periodically re-assert it and **log when it was found replaced**.
4. **Local repro if wanted** (validation, not discovery): the Win11 VM + `dx12-harness` grown an
   XInput phase — install the same ilhook detour, then a second 5-byte hook over it, poll — should
   AV at `+5` deterministically; then flip to the IAT hook and watch it not care.

---

The remainder of this section is the **first-test (2026-06-27) analysis**, kept for the record with
refuted parts marked.

### What happened

The mod loaded and ran perfectly on the friend's machine through Steam init, the session probe,
scaling, every feature — then died the instant the overlay's present hook activated. (The friend's
environment, reported **out-of-band, not derivable from the logs**: native Windows, NVIDIA RTX 3080,
driver 32.0.15.9649, DX12 / WDDM 2.7. The unusual WDDM-2.7-with-a-2024-driver pairing is worth a
re-confirm, since hypotheses #1/#2 below lean on the native stack being characterized correctly.)

All four crash logs (`unseamless_coop-*.log`, build `0f1c99f` = v0.8.0, **older than current
`7c8c746`**, so no breadcrumbs) share the same fatal *neighbourhood* but **do not end identically**:

```
[INFO]  overlay: DX12 present-hook installed; waiting for the swapchain   <- all 4
[INFO]  input: hooked XInput GetState ...                                 <- all 4
[INFO]  input: hooked DirectInput GetDeviceState for overlay capture      <- all 4, then 3/4 END here
[ERROR] Initialization context incomplete                                 <- hudhook dx12.rs:176 (1/4 only)
[ERROR] Render error: Error { code: HRESULT(0xFFFFFFFF), message: "" }    <- hudhook dx12.rs:235 (1/4 only)
                                                                          <- log ENDS; process dies
```

**Only one of the four runs (`…764-9676`) captured the two hudhook `[ERROR]` lines; the other three
end after the input-hook install with no hudhook present output at all.** Two readings, both pointing
the same way:

- **Buffering:** our `simplelog` file sink isn't flushed per record, so a hard crash drops unflushed
  tail lines — the errors were emitted in all four but only survived to disk in one.
- **Died-even-earlier:** in three runs the first hooked Present faulted *before* hudhook's `render()`
  logged anything (the detour entry / MinHook itself), and the one survivor got one frame further.

Either way: **the file tail is not a reliable last-line death oracle**, the crash clusters tightly at
first-present activation, and the trace diagnostic below must force per-record flushing to be trusted.

> **[Resolved 2026-07-01 — a third reading won.]** The trace run showed everything between the
> input-hook lines and the actual death (~16s in) is trace/debug-level or a breadcrumb v0.8.0 didn't
> have, so these logs never implied a first-present death at all — the "clusters tightly at
> first-present activation" conclusion was a log-level artifact. See the 2026-07-01 subsection above.

### What the log proves — and what it does *not*

- **Both `[ERROR]` lines are hudhook's, not ours** (`dx12.rs:176`, `:235`), and they are *the exact
  two lines our rig prints harmlessly on the first frame or two* before the command queue is captured.
  So **they are not the crash** — they're the last buffered lines before it. The old `install()`
  comment calling them a "known-harmless startup artifact, confirmed on the rig" was right *for
  vkd3d* and dangerously misleading for native; it's now corrected to point here.
- **"Initialization context incomplete"** = hudhook's state machine is still `WithSwapChain` (it has
  the swapchain but hasn't matched a command queue to it yet). This is the *expected* state on the
  first hooked Present — see the gap analysis below.
- **The CQ scan did not *fail* — it simply hadn't run yet.** hudhook logs `Couldn't find command
  queue pointer …` at **`warn`** (≥ info, so it *would* be in this log) when `check_command_queue`
  rejects a queue. It is **absent**, so on native the scan never even rejected a queue before death.
  This is pure first-frame ordering, identical to the rig — not a layout/offset mismatch.
- **We cannot yet tell whether our code ran.** The `initialize() reached` / `first render frame
  reached` breadcrumbs (added in `7c8c746`) aren't in these older logs. Since the context never
  completed, our `initialize`/`render` almost certainly never ran — a fresh native run on the current
  build will confirm.

### Anatomy of the fatal frame

hudhook's `dxgi_swap_chain_present_impl` (`dx12.rs:220`) does, every present:

1. `INIT_STATE.insert_swap_chain(&swap_chain)` → state becomes `WithSwapChain`.
2. `render(&swap_chain)` → `init_pipeline()` → `INIT_STATE.get()` returns `None` (not `Complete`) →
   logs **"Initialization context incomplete"**, returns `Err(HRESULT(-1))`.
3. `render` returns `Err` → `util::print_dxgi_debug_messages()` → logs **"Render error …
   0xFFFFFFFF"**. (Our render did *nothing* — it bailed before any GPU work.)
4. `trace!("Call IDXGISwapChain::Present trampoline")` then **calls the original Present**.
5. `<process dies here on native; survives on vkd3d>`.

**The structural 1-frame gap (why the error always appears, even on the rig):** the swapchain only
becomes known to hudhook *at Present time*, by which point that same frame's `ExecuteCommandLists`
(where the queue is captured) has already passed. So frame 1 is *always* incomplete; frame 2's
`ExecuteCommandLists` runs with state `WithSwapChain`, matches the queue → `Complete`, and frame 2's
Present renders. This is inherent to hudhook 0.9.x and **cannot be closed from our side**. It is why
Michael's rig logs the same two errors and recovers — and why removing those errors is not a fix.

**Therefore the native crash is not the incomplete context.** It is whatever runs *after* it on the
first hooked present — overwhelmingly likely **the call into the game's original Present trampoline**
(step 4→5) on native NVIDIA DXGI, where our detour did nothing but `AddRef` the swapchain.

> **[Refuted 2026-07-01.]** The trace run survived the first hooked present *and* the trampoline
> (every present logged its trampoline trace for ~3s); the death is a later event. The frame-1-gap
> anatomy above remains correct and useful; the "fatal at step 4→5" conclusion is dead.

### Why native NVIDIA dies where vkd3d tolerates it (ranked hypotheses)

> **Narrowed by the local Windows VM harness (`dx12-harness`, 2026-06-28).** A clean WARP run (below)
> **rules out the hardware-independent parts of #1 and all of #3.** The crash is now pinned to
> something **NVIDIA-driver-specific**: the present-threading *trigger* of #1, or the #2 interposer.
>
> **[Re-ranked 2026-07-01.]** The friend trace run refuted #1 outright (first hooked Present, CQ
> match, init, and render all succeeded on the same machine) and left #2 open-but-unobserved. The
> live ranking is in the 2026-07-01 subsection above: post-`ResizeBuffers` present path first, then
> filter-replacement/fail-fast mechanisms.

1. **First hooked Present faults under MinHook on native — NVIDIA-driver-specific (most likely).** The
   detour is applied to a live swapchain vtable while presents are in flight; native NVIDIA's
   frames-in-flight / present threading hits a window vkd3d's different scheduling dodges. **The VM
   harness reproduced the *mechanism* (off-thread MinHook detour on an already-presenting swapchain)
   on a real Windows loader and it did NOT crash — so the mechanism alone is fine; the fatal part is
   the NVIDIA driver's present path specifically, which WARP doesn't share.** Confirm on the friend's
   machine / a GPU-passthrough VM.
2. **A swapchain proxy on native shifts the present target / CQ layout.** Anything interposing a DXGI
   swapchain proxy (NVIDIA App / GeForce overlay, an FPS/RTSS overlay, DLSS Streamline) would diverge
   the detour target or the CQ scan. ER ships DLSS *super-resolution* (no swapchain proxy), not
   frame-gen, so rank this below #1 — but **ruling out other in-game overlays on the friend's machine
   is a cheap test.** Not reproducible in the VM (no such interposer), so still open.
3. **~~The fault is later than the log suggests — imgui's DX12 font upload faults on native.~~ RULED
   OUT (VM harness, 2026-06-28).** The harness bakes + GPU-uploads the *identical* font atlas via
   imgui's DX12 backend, and it completed cleanly on a real Windows loader (`initialize() reached` →
   `first render frame reached`, 5690 hooked-Present frames, no crash). The font-upload path is not
   hardware-dependent and is not the crash.

### Version reality (no bump available)

- We are on **hudhook 0.9.1 — the latest release** (0.9.1 May 2024; no 0.9.2/0.10). Its `dx12.rs` is
  **byte-identical to 0.9.0**, so the command-queue-capture logic is unchanged across the only two
  candidate revs. **A "vetted hudhook bump" is not an available fix.**
- *Correction to the brief:* the dep does **not** resolve to a git checkout (`53bbf50`). `Cargo.toml`
  pins `hudhook = { version = "0.9", … }` and `Cargo.lock` resolves it to **crates.io 0.9.1**. The
  git checkout in `~/.cargo` is just an old artifact. (Source is identical, so the analysis holds —
  but there's no git override to hunt for.)

### The diagnostic lever we already have

hudhook's `tracing` dep is `features = ["log"]`, and we install **no** tracing subscriber, so its
events forward straight to the `log` crate — that's how its `error!` lines reached our file. But its
**localizing** breadcrumbs sit below `info` and were suppressed (the friend ran at `info`):

| hudhook line | level | what it tells us |
|---|---|---|
| `ID3D12CommandQueue::ExecuteCommandLists(...) invoked` | trace | the CQ hook is firing at all |
| `Found command queue pointer in swap chain struct at offset +0x…` | debug | CQ matched → context will complete; gives the native offset |
| `Couldn't find command queue pointer …` | warn | CQ scan rejected a queue (would already show at info) |
| `Call IDXGISwapChain::Present trampoline` | trace | **the decisive line** — see below |

A single run at `[debug] enabled = true, level = "trace"` should surface all of these: our `simplelog`
sinks set no target filter, `CombinedLogger` raises `log::max_level` to `Trace`, and `tracing`'s `log`
feature honours it (the `error`-level bridge is *verified* — those lines reached our file; the
trace-level path is *expected*, exercised for the first time by this run, as hudhook sets no static
`max_level` feature). **Caveat:** the tail is buffering-dependent (3 of 4 first runs lost their last
lines), so this is only trustworthy with per-record flushing forced. **The decisive observation:** if
`Call IDXGISwapChain::Present trampoline` is the *last* line before death, the crash is in the game's
original Present (hypothesis #1); if it's *absent* (flushing on), the crash is just after the
Render-error log, in the `trace!` eval or detour glue *before* the trampoline call.
(`print_dxgi_debug_messages` runs at `dx12.rs:234`, before the Render-error line, so its survival is
implied whenever that line is present.) The crash handler (above) now answers this more directly by
naming the faulting module, so treat the trace-line tell as a cross-check.

### Validation plan (baseline needs no Windows box)

Michael's key insight: **the rig prints the same two errors, it just doesn't crash** — so most of the
loop is runnable here.

1. **Rig baseline (orchestrator, vkd3d) — CAPTURED 2026-06-28.** Ran the rig (diag build, current
   `main`) at `level = "trace"`, boot to title (no in-game input needed — the present hook fires as
   soon as frames flow). The *healthy* sequence, in order, is now the reference to diff native against:

   ```
   hudhook::hooks::dx12: IDXGISwapChain::Present = 0x…                (dx12.rs:380, present hook found)
   overlay: DX12 present-hook installed; waiting for the swapchain
   [ERROR] Initialization context incomplete                          (dx12.rs:176 — frame-1 gap)
   [ERROR] Render error: Error { code: HRESULT(0xFFFFFFFF) … }        (dx12.rs:235 — same gap)
   Call IDXGISwapChain::Present trampoline                             (dx12.rs:238 — DECISIVE: survives)
   ID3D12CommandQueue::ExecuteCommandLists(…) invoked                  (dx12.rs:272 — CQ hook firing)
   Found command queue pointer in swap chain struct at offset +0x8    (vkd3d's CQ offset)
   Found command queue matching swap chain … (context will Complete)
   overlay: hudhook initialize() reached (baking fonts)               (our init ran)
   overlay: first render frame reached (render_inner)                 (our render ran)
   …then `Call … Present trampoline` every present, steady, no crash.
   ```

   Three baseline facts this nails down: (a) **both `[ERROR]` lines print on the rig too** at frame 1,
   then recovery — confirms they are the harmless structural gap (§"Anatomy"), not the crash; (b) on
   vkd3d the CQ is found at **offset +0x8** (native NVIDIA may differ — hudhook's scan is
   offset-agnostic, so a *different* native offset is fine, but `Found command queue pointer` never
   appearing is the tell); (c) `Call … Present trampoline` prints on **every** present and the process
   lives — so on native, if that line is the *last* before death, the crash is in the game's original
   Present (hypothesis #1); if `initialize() reached` / `first render frame reached` are absent on
   native, the context never completed (matching the friend's older logs). The breadcrumb build +
   per-record fsync (`#[cfg(debug_assertions)]`, already shipped) make the native run's tail trustworthy.
2. **Local Windows VM harness (`crates/dx12-harness`, the `/windows-test` skill) — RAN CLEAN
   2026-06-28.** A minimal D3D12 app that presents then injects the *same* hudhook present-hook + imgui
   font bake into its own live swapchain mid-flight, driven into the existing quickemu Win11 VM via
   `scripts/win.sh`. The VM is a **real native Windows DXGI/D3D12 loader + present path** (not vkd3d)
   but runs **WARP, not the NVIDIA driver**. **Result:** swapchain up → hook injected mid-flight → CQ
   found at offset **+0x138** (vs the rig's +0x8 — the offset-agnostic scan handled it) → `initialize()
   reached (baking fonts)` → `first render frame reached` → **5690 hooked-Present frames, no crash.**
   So the MinHook-on-a-live-swapchain *mechanism* (hyp #1) and the imgui DX12 font upload (hyp #3) are
   **ruled out as hardware-independent causes** — the crash is NVIDIA-driver-specific, which WARP can't
   reproduce and the VM can't validate. The VM was the cheap filter that did its job (narrowed the
   space); it can't be the verdict. (Run gotchas it surfaced, now baked into `win.sh`: the harness
   needs WARP forced + an Interactive scheduled task with a logged-in desktop, since an SSH session has
   no window station for a DXGI swapchain — see the `/windows-test` skill.)
3. **Friend native run (current build, breadcrumbs on), `level = "trace"`, once — with per-record log
   flushing forced** — **CAPTURED 2026-07-01** (the subsection above). Answers: `initialize()
   reached` / `first render frame reached` **do** appear on native; `Call … Present trampoline` is
   **not** the last line; `Found command queue pointer` **does** appear (+0x140). The divergence from
   the rig baseline is not at hook activation at all — it's a silent process death ~16s in.
4. **A code fix is only "validated" if the VM harness reproduced the crash and then stopped after the
   fix; otherwise it stays UNVALIDATED on native until a subsequent friend session.** Flag it as such.

### Candidate fixes (ranked)

1. **Immediate mitigation — no code, shippable now: `[debug] overlay = false`.** The logs prove the
   *entire* mod runs perfectly without the overlay; only the in-game menu is lost. Co-op host/join can
   be driven headless via `auto_session` / the rig-guide actions (`7c8c746`) for the next test.
   Zero vkd3d impact. **Recommended so the friend can play + we can still exercise co-op.**
2. **Diagnostic trace run — no code (the §"validation plan" runs).** Cheapest path to an actual fix;
   do this before changing any render/hook code.
3. **Install-timing change — RULED OUT (Michael, 2026-06-27): we will NOT do this.** The idea was to
   defer overlay install from `frontend_ready` to `in_game()`. It does **not** change the
   first-hooked-present dynamic (the crash is at the first hooked Present whenever that is, title or
   in-game), so it's a guess that mainly *moves* where it crashes rather than fixing it, and it would
   make the overlay unavailable at the title/menu (where it's wanted). Don't pursue it; it's recorded
   here only so it isn't re-proposed.
4. **Fork hudhook with a defensive `dx12.rs` — judgment call (needs sign-off), only after breadcrumbs
   implicate a specific step.** If the original-Present call is the faulting step, options are thin
   (it's the game's own present). If the CQ scan ever matches a *wrong* queue (false positive in the
   512-pointer scan), tighten `check_command_queue`. **Risk: forking the render path can regress the
   working vkd3d overlay — must re-run the rig baseline after.** Do not do this blind.
5. **Not a fix:** a version bump (none newer exists); "removing the incomplete-context error" (it's
   structural and harmless, and present on the rig too).

**Recommendation:** don't touch render/hook code blind. Ship mitigation #1 so the friend can play and
we can still drive co-op headless; gather the §"validation plan" data (rig baseline + one friend trace
run); decide #3/#4 with Michael from that data.

## Status & Next Steps

- [x] Rig milestone #1: hudhook added to the cdylib, DX12 hook installed, overlay renders over the
      running game under the rig's Proton/vkd3d (rig baseline captured 2026-06-28 — the make-or-break
      test passed).
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
- [x] Overhead nameplates: **shipped as native `CSEzDraw` dots** (`coop/features/native_nameplates.rs`),
      not on this overlay. The imgui world→screen projection path was removed — see [NAMEPLATES.md](NAMEPLATES.md).
- [ ] ⚠️ **Native-Windows overlay crash — ROOT-CAUSED (mechanism) 2026-07-01, fix pending.** The
      trace run proved the DX12 present path healthy on native NVIDIA (CQ at +0x140, fonts baked,
      presents flowing); the friend's WER Event 1001 then pinned the death: `c0000005` at
      **`XINPUT1_4.dll+0x9a65` = `XInputGetState+5`** — an inline-hook collision between our ilhook
      patch on `XInputGetState` and a second 5-byte hooker (likely Steam's gameoverlayrenderer)
      whose trampoline jumps back to `entry+5`, mid-our-patch. Full analysis: "WER Verdict" above.
      **Next:** reimplement the XInput capture as an **IAT hook** on `eldenring.exe`'s import (no
      function-body patching); get the friend's `Report.wer` module list (confirms the other
      hooker); harden `crashdump.rs` (re-assert the filter, log replacement — ours was bypassed).
      Mitigate meanwhile with `[debug] overlay = false`.
- [x] **Crash handler staged (2026-06-29):** `unseamless-coop/src/crashdump.rs` (in the cdylib *and* the
      harness) installs an unhandled-exception filter that logs the **faulting module+offset** + AV
      target + registers on a hard fault. Verified on WARP via `DX12_HARNESS_FORCE_CRASH=1` (caught the
      AV, resolved `dx12-harness.exe+0x2fa0`). Read-a-report recipe is in the `/windows-test` skill.
      **Caveat (2026-07-01): the real friend crash bypassed it** — either a fail-fast (no SEH) or the
      filter was replaced after install (`SetUnhandledExceptionFilter` is last-writer-wins). Hardening
      is a listed next step; friend-side Event Viewer is the fallback datum.

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
