# Rig runbook — unblocking the co-op core

The point where Mac-side work hands off to the PC rig. Everything here needs the game running;
none of it can be done on the dev Mac. The goal of the first rig session is **not** to make
co-op work — it's to **observe the session state machine** so Layer 2 (see
[ARCHITECTURE.md](ARCHITECTURE.md)) can be designed against reality instead of guesses.

## Deploy

```bash
cargo build --release                 # on the Mac -> unseamless_coop.dll
# copy the DLL to the rig, then on the rig:
./scripts/deploy.sh                   # into ELDEN RING/Game/mods/  (Elden Mod Loader)
rm -f "ELDEN RING/Game/unseamless_coop.log"   # so a fresh load is unambiguous
```

Launch via the ERSC exe-swap path (so the mod loads outside EAC). The mod writes
`unseamless_coop.log` and a default `SeamlessCoop/unseamless_coop.ini` on first run.

## What to confirm first (harness sanity)

From the **title screen**, the log should show, in order:
1. `unseamless-coop loaded` + `cwd = …`
2. `registered feature 'session-observer' in FrameBegin`
3. `observer live; no CSSessionManager yet` heartbeats (every ~30s)

If those appear, the framework works end to end. (`FrameBegin` ticks in menus, so this needs no
save — same trick as er-crit-coop.)

## The observation run (the actual deliverable)

Drive this sequence and capture the `session change:` log lines at each step:

1. **Load a save (solo).** Expect a session snapshot with `players` small and a `limit`. Record
   `lobby`, `protocol`, `players`, `limit`, and the per-player roster (`host/local/cid`).
2. **Host a co-op session.** Watch the `lobby` transitions (`TryToCreateSession → Host`) and the
   `protocol` FSM.
3. **Have a second player join.** This is the key data point:
   - Does `players` go to 2? Is the local player included in `players.len()` or separate
     (`host_player`)? → settles the true "player count" for scaling.
   - What is `limit` (`session_player_limit`)? Expect 4 (open world).
   - Roster: which entry is `host`, which `local`, the `cid`s and steam_ids.
4. **Cross an area boundary / fast-travel together.** Watch whether `protocol` goes through
   `WaitReentryToMap` and whether the session persists or tears down. This is the heart of
   "seamless".
5. **Trigger a boss fight.** Note any state change (for boss-vs-enemy scaling targeting later).

Save the full `unseamless_coop.log` from this run — it's the spec for designing Layer 2.

## Follow-up experiments (once the FSM is understood)

In rough order, each a small, reversible probe:

- **Scaling application:** confirm the mechanism. Check whether ERSC-style scaling maps to
  `MultiPlayCorrectionParam` / `MultiSoulBonusRateParam` (idempotent, the likely-correct lever)
  vs raw `NpcParam` HP (which would compound if written per-frame). Prototype by setting the
  correction param once and observing enemy HP, using the multipliers the observer already logs.
- **Session limit:** try raising `session_player_limit` and see whether >4 players can connect,
  and what else gates it.
- **Side-channel:** register a `receive_packet` task on a custom packet type and a manual
  `broadcast_packet` send; confirm round-trip between two modded clients. This is the foundation
  for config sync and session actions.
- **Debug log forwarding:** once the side-channel round-trips, wire `ModMessage::Log` →
  `broadcast_packet` on clients with `[debug] forward_to_host`, and on the host feed received
  records into `diagnostics::LogBundle` and write the merged file. Then a friend's whole session
  lands in one place on the host. Model + message are already host-tested; this is just the
  transport glue. Rate-limit it so debug logging can't flood the channel.
- **Persistence:** find where the game tears down the session on map change and whether keeping
  it alive is viable.

## Menu overlay + settings application (rig-gated UI work)

The menu *model* and the settings *registry* are done and host-tested
(`unseamless-core/menu.rs`, `settings.rs`). What needs the game:

- **Overlay renderer** (`coop/features/menu_overlay.rs`, planned): hook the game's DX12 `Present`
  and draw `Menu::rows(&cfg, &ctx)` with egui (or similar). Pick the hooking approach with the
  game in front of you (this can't be designed blind). Toggle visibility with a hotkey, and read
  `CSFeMan` HUD state to optionally auto-show it when the pause menu is open.
- **Input → menu**: map keys to `select_next`/`select_prev`/`activate`/`adjust` (poll
  `GetAsyncKeyState`). Trivial wiring; just needs the overlay to exist to verify.
- **Building `SessionContext`**: fill `in_session`/`is_host` from `CSSessionManager`
  (lobby/protocol state + the local player's `is_host`) — confirm the mapping from the observer
  log first.
- **Applying `MenuOutcome`**: `SettingChanged` → persist config (add `coop/config.rs::save`) and
  re-apply effects (e.g. scaling); `Action(a)` → the session verb, which depends on the co-op
  core. So menu *navigation* can be demoed early; the *actions* light up as Layer 2 lands.

## Tooling on the rig (when needed)

For anything the observer log can't answer (e.g. *which* function relaxes a limit), set up
runtime instrumentation per [RUNTIME-RE.md](RUNTIME-RE.md): a diagnostic build of our own DLL
(preferred) or frida-gadget. Author those scripts on the Mac; run them here.

## Feeding results back

Record findings as behavioral notes **in our own words** (clean-room — see CLAUDE.md), update
FEATURES.md / ARCHITECTURE.md, and turn the confirmed mechanics into:
- host-tested types/state-machines in `unseamless-core`, then
- thin bindings in `unseamless-coop`.
