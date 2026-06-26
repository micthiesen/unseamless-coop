# Architecture

The design of the rewrite. This is the holistic view a step-by-step build wouldn't force us to
write down; it's also where the load-bearing decisions live. Read [FEATURES.md](FEATURES.md)
for *what* we're reproducing and [DEVELOPMENT.md](DEVELOPMENT.md) for the toolchain.

## Shape: a workspace split by verifiability

```
unseamless-core   (lib, pure Rust, NO game/OS deps)   -> host-testable on macOS
unseamless-coop   (cdylib, binds core to the game via fromsoftware-rs)
```

The split is deliberate and is the main rearchitecture vs ERSC's single C++ DLL: **push every
decision that can be expressed without the game into `unseamless-core`**, where `cargo test`
runs natively on the dev Mac. Config parsing, scaling math, the session/sync state model, and
(later) protocol message types all live there with real unit tests. The cdylib stays a thin,
mostly-mechanical binding layer: read SDK singletons, call core, write back.

Why it matters here specifically: we develop on a machine that can't run the game, so the more
logic is host-testable, the more of the project is *verified* rather than *hoped*. The cdylib's
correctness still needs the rig; the core's doesn't.

## Runtime spine (cdylib)

`DllMain` (attach only) → init thread → `app::install`:
1. `CSTaskImp::wait_for_instance` (off the main thread).
2. Load `Config` from `unseamless-coop/unseamless_coop.toml` (writes defaults if absent).
3. Build the `Vec<Box<dyn Feature>>` and register each as a recurring task in its `phase()`.

A [`Feature`] is one unit of behavior with a `name`, a `phase` (`CSTaskGroupIndex`), and
`on_frame`. Features sit behind a single global `Mutex<App>`; each registered task locks it and
ticks one feature. Tasks run on the game's main thread, so the lock is effectively uncontended —
it exists to satisfy the scheduler's `Fn + 'static` bounds, not for real concurrency. The
no-DETACH / `mem::forget(handle)` invariants from er-crit-coop carry over unchanged (see
CLAUDE.md > "safety invariants").

This gives clean, independent feature modules instead of one monolith, and lets each feature run
in the frame phase ordered against the state it touches.

## The two layers

**Layer 1 — host-charted, buildable now.** Features whose game effect is a typed SDK
read/write: scaling (params via `SoloParamRepository::get_mut`), splash-skip, summons, event
flags, world time. The SDK *is* the contract; we build them on the Mac and batch-verify on the
rig. Risk is bounded and per-feature.

**Layer 2 — RE-gated, needs the rig.** The co-op core: relaxing session limits, persisting
sessions across area transitions, getting players into one another's worlds, and state sync.
We can't write this blind — not for lack of tools, but because we must *observe* how the game's
session machine behaves. The [session observer](../crates/unseamless-coop/src/features/observer.rs)
is Layer 2's first step: it logs the state machine live so the rig run produces the spec.

## Key decision: we drive the game's networking, not our own

The SDK inventory was decisive. The game already has a full P2P stack: `NetworkSessionVmt`
exposes `broadcast_packet` / `receive_packet` / `kick` / `remote_identity`; `CSSessionManager`
holds the lobby/protocol FSM, the player roster, the session player limit, and an AES
cipher; there are dedicated `TaskLineIdx_FrpgNet_*` and `NetFlushSendData` task phases.

So ERSC almost certainly does **not** implement its own transport — it **drives the game's
existing session/matchmaking** with the restrictions relaxed and the session made persistent.
We follow the same model. (Full per-subsystem inventory: [SDK-COVERAGE.md](SDK-COVERAGE.md).)

- **Transport = the game's** (Steam P2P, already encrypted). We do not reinvent it.
- **What the mod adds:** relax `session_player_limit` and area-boundary teardown; keep sessions
  alive across map transitions; coordinate mod state (config sync, session actions like
  open/lock/leave) over a **small mod-specific side-channel** — our own packet type(s) sent
  via `broadcast_packet` and read in a `receive_packet` task.
- **Interop scope:** because base networking is the game's own, vanilla multiplayer mechanisms
  come along for free. We make **no attempt** to interoperate with *vanilla ERSC's* side-channel
  packet format — every player runs *our* mod, so our side-channel is ours to define cleanly.
  (Recorded here so a future change of mind is a conscious one.)

This shrinks the hard RE surface from "an entire netcode" to "how the game's session FSM
behaves and where ERSC relaxes/persists it" — observable on the rig.

The end-to-end plan for *getting two players connected* — the offline/Steam distinction, the
private-Steam-channel-first build order, and the decided Steamworks integration approach (hand-bind
the flat C API at runtime, not the crate) — is its own doc: [COOP-CONNECTION.md](COOP-CONNECTION.md).

### Keeping it seamless: hold state invariants, don't patch call sites

ERSC byte-patches the many call sites that normally end a co-op session (boss death, fog gate, host
death, walking out of the host's area). We deliberately **don't** copy that. Two reasons it's the
wrong shape for us: every byte-patch is a potential **Arxan** landmine (Arxan restores `.text`, so a
patch can be reverted under us — it's why the offline-popup work is a **won't-do**, see
[OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md)), and we have a lever ERSC (a C++ DLL) leaned on
less: **typed SDK field writes from a per-frame task**.

So we model "stay connected" as a small set of **invariants re-asserted every frame**, not code we
carve out. Approaches, best to worst:

1. **Hold the state the teardown reads.** Arxan restores code, *not* runtime data, so a held field is
   immune. Existence proof: [`session_limit`](../crates/unseamless-coop/src/features/session_limit.rs)
   raises the player cap by writing one `u32` every frame and it sticks. The SDK already charts the
   prime levers for the roam/area half: `CSStayInMultiplayAreaWarpData::disable_multiplay_restriction`
   (documented "set true to go anywhere on the map") and `multiplay_start_area_id` ("set 0 to disable"
   the boss-area-mismatch warp). The first is now **shipped** as
   [`seamless-roam`](../crates/unseamless-coop/src/features/seamless.rs): it holds
   `disable_multiplay_restriction` to the host-enforced `gameplay.roam_anywhere` each frame (the roam
   *effect* awaits a party run; the held write is observable solo via the teardown probe).
2. **Hook the chokepoint and *decide*** (ilhook redirect, like `input.rs`, which sticks) — read the
   teardown *reason* and suppress only the unwanted ones, so a real quit-to-menu still works. Likely
   2-3 chokepoints (low-level net disconnect vs. high-level "area co-op ended"), not ERSC's dozen.
3. **Let teardown happen, reconnect** — safety net for genuinely unpreventable resets (a full area
   reload nuking the session object); has a blip + matchmaking latency.

Which lever each event needs is **empirical**, decided by a rig run, not from the armchair. The
[observer](../crates/unseamless-coop/src/features/observer.rs) is the probe: in a 2-player session,
trigger each event and read the log — it marks the roster shrink (`TEARDOWN`) loudly, frame-tags every
transition so the order of flips is recoverable, and watches the tether fields so a candidate state
lever shows itself. A field that flips *before* the roster drop is a lever (hold it); a drop with no
observable lead-up is atomic and needs a hook. We treat the ERSC trigger list as the *catalog of
events to test* (recovered behaviorally), never as the strategy to copy.

### The side-channel is self-healing (robust to an unknown delivery model)

We don't yet know whether `broadcast_packet` is reliable/ordered or best-effort — Steam P2P can be
either, and we'll only know from the rig. So the side-channel is designed to **converge regardless**,
which is robust to whatever the rig reveals:

- The host **re-asserts** its authoritative `ConfigSync` every `maintain()` tick, tagged with a
  monotonic **generation**. A dropped sync heals on the next tick; a stale/reordered one is ignored
  (generation guard); a duplicate is a no-op.
- Session actions and forwarded logs carry a per-sender **sequence** and are deduped (`SeqGate`), so
  a duplicated/reordered frame applies exactly once.
- A heartbeat `Ping` drives **liveness** (stale-peer banners). The timeout is tuned conservatively
  and is rig-dependent (loss rate), since liveness is itself lossy and role-asymmetric.

This whole layer is **host-testable** (`unseamless-core/{peer,transport}.rs` + the harness), with a
seeded `FaultModel` proving convergence under drop/duplicate/reorder — so it's verified on the Mac
before the rig, and the design holds whether the transport turns out reliable or not.

One thing is deliberately **deferred to the rig**: a host *restart/migration* resets the host's
generation counter, which the monotonic guard would stall on. Handling that needs a host-instance
epoch, and its shape depends on how the game's session FSM signals a host change — a Layer-2
observation, not something to guess at blind.

## Divergences from ERSC (deliberate — don't "fix" these back)

We are reimplementing ERSC's *effect*, not copying its design, and we intentionally differ in
several places. Recorded here so future work doesn't pattern-match ERSC and undo a choice:

1. **Config is TOML + serde, not ERSC's `ersc_settings.ini`.** Adding an option is a struct field
   (`unseamless-core/config.rs`); serde handles load/save and ignores unknown keys (forward/back
   compatible). We do **not** parse ERSC's `.ini` — no drop-in compat is needed since everyone
   runs our mod. Don't reintroduce a hand-written INI parser.
2. **Settings live in one declarative registry** (`unseamless-core/settings.rs`). Each option is
   described once (label, kind, get/set) and that single declaration powers *both* the config
   file and the in-game settings view. Don't hand-wire per-option UI. The in-game surface currently
   shows settings **read-only** (coloured synced-vs-local via `SettingId::is_shared`), edited in the
   TOML file; live editing from the UI is deliberately deferred (boot-vs-live + host-enforcement
   questions), so the registry still drives the *display*, just not an editable menu yet.
3. **Session actions are a menu, not items/hotkeys.** ERSC triggers host/join/leave via custom
   in-game **goods** (the `MODGOODS_*` items) and fixed hotkeys. We drive them through a menu
   model (`unseamless-core/menu.rs`, the actions-only `Menu::actions_only`) rendered as an **ImGui
   overlay** toggled by a hotkey (backtick) (via hudhook; see
   [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md)). We are **not** reproducing
   the custom goods, and we are **not** injecting a native pause-menu entry — the SDK exposes no
   API for that and it's heavy UI RE (an overlay is simpler and fully ours). If you see the
   `MODGOODS_*`/item machinery in FEATURES.md, it's catalogued for reference, not as a build target.
4. **Networking: drive the game's own session layer; add a small private side-channel.** No
   bespoke transport, and **no interop with vanilla-ERSC's** side-channel packet format (see the
   "Key decision" section above).
5. **We own the whole install — no Elden Mod Loader, no ERSC launcher.** The cdylib ships as the
   game's `dinput8.dll` (a search-order proxy the game auto-loads; `coop/proxy.rs` forwards the real
   exports), which makes this mod the **parent loader**: it also `LoadLibrary`s other DLL mods from
   `mods/` in a host-tested order (`unseamless-core/loader.rs` + `coop/mods.rs`). Our `launcher`
   crate ships as `start_protected_game.exe`, starting the game directly (outside EAC) with a
   `UNSEAMLESS_LAUNCH` marker; the DLL **aborts** if that marker is absent (`coop/guard.rs`), so a
   game update that reverts the launcher can't run the mod under anti-cheat. Config/logs live in our
   own `unseamless-coop/` folder, not ERSC's `SeamlessCoop/`. The no-installer install (overwrite
   `start_protected_game.exe`, restore via Steam "verify integrity") is the deliberate UX choice;
   don't reintroduce a separate loader dependency.

## Module map (current + planned)

| Path | Layer | Status |
|---|---|---|
| `unseamless-core/config.rs` (TOML/serde) | 1 | done, tested |
| `unseamless-core/settings.rs` (registry) | 1 | done, tested |
| `unseamless-core/scaling.rs` | 1 | done, tested |
| `unseamless-core/menu.rs` (menu model) | 1 | done, tested |
| `unseamless-core/notifications.rs` (toast/banner model) | 1 | done, tested; **wired** to the overlay renderer (`coop/notify.rs` + `coop/features/notifications.rs`) |
| `unseamless-core/protocol.rs` (side-channel, wire v2: generation/seq identity) | 2 | done, tested (wiring is rig-gated) |
| `unseamless-core/transport.rs` (`Transport` seam + `Loopback` + `FaultModel`) | 2 | done, tested |
| `unseamless-core/framing.rs` (length-prefixed wire codec, shared by `TcpTransport` + the bridge) | 2 | done, tested |
| `unseamless-core/peer.rs` (`Peer`/`Session`: handshake/config-sync/actions/log-forward/liveness, self-healing) | 2 | done, tested |
| `unseamless-core/loader.rs` (mod-load ordering policy) | 1 | done, tested |
| `harness` bin (in-memory + lossy + two-process TCP loops, no game) | — | done — see the `/test-loop` skill |
| `coop/proxy.rs` (dinput8 export forwarding) | — | done (export-table verified; live forward rig-gated) |
| `coop/guard.rs` (EAC launch-marker abort) | — | done (logic; abort behavior rig-gated) |
| `coop/mods.rs` (load `mods/` DLLs in order) | — | done (FS+LoadLibrary glue; rig-gated) |
| `launcher` bin (`start_protected_game.exe`) | — | done (rig-gated) |
| `unseamless-core/` player/world sync model | 2 | planned (the game's job; rig-gated) |
| `coop/app.rs`, `feature.rs` | — | done |
| `coop/config.rs` (disk load) | 1 | done |
| `coop/features/observer.rs` | 2 | done (read-only); logs `session_player_limit_override` |
| `coop/state.rs` (process-global live `Config`; features read each frame, bridge/menu write) | — | done |
| `coop/features/session_limit.rs` (write `session_player_limit_override` from live config) | 2 | done; apply rig-confirmed (incl. via a synced `ConfigSync`), >4-player effect needs a party |
| `coop/bridge.rs` (dev debug bridge: live `Session` over loopback, applies received config) | — | done; apply rig-confirmed (`bridge` feature, off in release) |
| `coop/overlay.rs` (hudhook DX12 present-hook → imgui) | — | **ships**: notifications (rig-confirmed) + a backtick-toggled utility window — interactive actions menu, read-only settings (synced/local), live log tail; deterministic input capture via hudhook `message_filter`. Self-contained DLL (static C++ runtime). See [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md) |
| `coop/logbuf.rs` (in-memory log ring buffer; `SharedLogger` teed via `CombinedLogger`) | — | done; overlay Log tab reads it non-blocking |
| `coop/actionq.rs` (overlay→game-thread queue of requested `SessionAction`s) | — | done; drained by `features/session_actions` (logs + toasts; execution rig-gated) |
| `coop/notify.rs` (process-global `Mutex<Notifications>`; features push, overlay reads non-blocking) | — | done |
| `coop/features/scaling.rs` (apply) | 1/2 | mechanism decided (SpEffect rate rows behind `MultiPlayCorrectionParam` — see [SCALING.md](SCALING.md)); row/ID map rig-gated |
| `coop/net/*` (session relax, side-channel, sync) | 2 | gated on observer findings |

## Where Mac-side work ends

When a piece needs to *observe or affect a live session* to proceed — the scaling application
mechanism, session-limit relaxation, the side-channel, sync — it's Layer 2 and moves to the rig.
The handoff artifact is the observer log; the plan for producing it is [RIG-RUNBOOK.md](RIG-RUNBOOK.md).
