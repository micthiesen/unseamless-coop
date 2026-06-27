//! The committed registry of rig-testing guides, selectable via `[debug] guide = "<name>"` (empty =
//! off). Adding a guide is cheap: write a builder function and add one arm to [`by_name`] + one entry
//! to [`NAMES`]. Authoring API + conventions are in `.claude/skills/rig-guides/SKILL.md`; the engine
//! internals are in the parent [`crate::guide`] module and `docs/RIG-GUIDES.md`.

use super::{Advance, Guide, LobbyState, Role, after_secs, game_state_is, lobby_is, log_contains};
use crate::game_state::GameState;

/// Every committed guide's name, in registry order (for the "unknown guide" log + docs).
pub const NAMES: &[&str] =
    &["rung3-create-chart", "overlay-smoke", "two-player-join", "rig-observation"];

/// Build the guide named `name`, or `None` if there's no such guide (the binding logs the available
/// [`NAMES`] and runs no guide). Each guide is rebuilt fresh on request — guides hold closures, so
/// they aren't cloneable; that's fine, one is built once per launch.
pub fn by_name(name: &str) -> Option<Guide> {
    match name {
        "rung3-create-chart" => Some(rung3_create_chart()),
        "overlay-smoke" => Some(overlay_smoke()),
        "two-player-join" => Some(two_player_join()),
        "rig-observation" => Some(rig_observation()),
        _ => None,
    }
}

/// **Flagship / dogfood guide** for the rung-3 create-session RE run (see
/// `docs/SESSION-RE-FINDINGS.md`). Drives the human steps and auto-detects the FSM signal; the
/// orchestrator-side ptrace write-watch is a separate concern.
///
/// Run it with `[debug] guide = "rung3-create-chart"` **and** `[debug.probes] session_probe = true`
/// (the auto-finish predicates read the probe's `session-probe:` log lines). The boot step finishes
/// on the real "a save is loaded" signal ([`GameState::InGame`]) rather than the probe's "FSM live"
/// line — per the findings doc the manager goes live at the *title* screen, so "FSM live" would fire
/// too early; we want a loaded character.
fn rung3_create_chart() -> Guide {
    Guide::new("rung3-create-chart")
        .step("boot", "Boot to a loaded save: load a character into the world.")
        .done_when(game_state_is(GameState::InGame))
        .step(
            "host",
            "Use a multiplayer item (e.g. a summon sign / Furlcalling Finger Remedy) to host a \
             session. Watching for the lobby FSM to move to TryToCreateSession.",
        )
        // Catch the transition either by reading the live FSM, or by the session-probe transition
        // line in the log (robust if the state is transient and the live read misses the exact frame).
        .done_when(lobby_is(LobbyState::TryToCreateSession).or(log_contains("->TryToCreateSession")))
        .branch(|ctx| {
            if ctx.state.lobby_state == LobbyState::TryToCreateSession
                || ctx.log_contains("->TryToCreateSession")
            {
                Advance::To("captured")
            } else {
                Advance::To("retry")
            }
        })
        // Skip (or a manual give-up) treats it as "hosting didn't move the FSM" -> the retry step.
        .default_branch(Advance::To("retry"))
        .step(
            "captured",
            "Captured: the lobby FSM reached TryToCreateSession (hosting drives the create FSM). \
             Note the frame from the session-probe line for the write-watch correlation.",
        )
        .step(
            "retry",
            "Hosting did not move the FSM to TryToCreateSession. Try placing or answering a summon \
             sign instead, then watch the session-probe log.",
        )
}

/// A tiny smoke guide to sanity-check the guide system itself on the rig (banner renders, controls
/// work, auto-advance fires, the done toast shows) without needing any session/RE state.
fn overlay_smoke() -> Guide {
    Guide::new("overlay-smoke")
        .step(
            "intro",
            "Rig-guide smoke test. This pinned banner is the current step. Hold the done control to \
             advance.",
        )
        .step("auto", "This step auto-advances after 3 seconds (watch the banner replace itself).")
        .done_when(after_secs(3.0))
        .step("manual", "Last step: hold the done control to finish and see the 'done testing' toast.")
}

/// The **canonical role-tagged two-player guide** and the best showcase of log/state auto-finish: it
/// drives the full friend-connect flow (rungs 4 + 2 — password-keyed lobby discovery + the Steam P2P
/// side-channel) from one committed guide. The host machine sees the host steps, the joiner sees the
/// joiner steps, both see the shared steps (set `[debug] rig_role` to `host` / `join` on each machine;
/// see `docs/FRIEND-TEST-RUNBOOK.md`).
///
/// **Every connect step auto-finishes off the run log**, so the result is captured in the shareable
/// (and host-forwarded) log instead of relayed as "did it connect?": the lobby-discovery resolve line
/// (per role), the rung-2 link milestone (`coop: linked`, logged by `coop::update_link_status`), and
/// the client's config adoption (`coop: adopted host config`, logged by `coop::adopt_host_config`).
/// The matched substrings are stable fragments those sites pin deliberately. The trailing **stub** is
/// the one piece not executable yet: verifying the host's settings take *effect* in-world needs the
/// apply layer + a real game session (rung 3).
fn two_player_join() -> Guide {
    Guide::new("two-player-join")
        // Same area isn't required for the side-channel link, but it sets up the in-world checks the
        // trailing stub will eventually cover.
        .step("both-boot", "Both players: load a character into the world (same area is ideal).")
        .done_when(game_state_is(GameState::InGame))
        // The shared password is the only pairing input (the startup guard already refused to launch
        // without one >= 8 chars, so by here it's set) — a reminder, not a gate, hence manual.
        .step("both-password", "Both: confirm you set the SAME co-op password before launch.")
        // HOST opens a world; auto-finish when lobby discovery resolves a joiner on our lobby (the
        // `coop: lobby discovery resolved partner … (we are the host)` line — the host-only fragment).
        .step("host-open", "HOST: open the overlay (backtick, or RB+L3+R3), Actions tab -> Open World.")
        .role(Role::Host)
        .done_when(log_contains("we are the host"))
        // JOINER joins; auto-finish when discovery finds the host's lobby (the joiner-only fragment).
        .step("join-join", "JOINER: open the overlay, Actions tab -> Join world (same password).")
        .role(Role::Join)
        .done_when(log_contains("we are the client"))
        // Shared: the side-channel handshake lands. BOTH machines log `coop: linked …` on the link
        // edge, so both advance off the same captured signal — no "it connected" relay. A version
        // mismatch is named on that same line (and bannered in-game).
        .step("linked", "Both: wait for the Steam P2P link (the 'Co-op partner connected' toast).")
        .done_when(log_contains("coop: linked"))
        // JOINER only: the client adopts the host's shared settings. The host is authoritative, so it
        // logs nothing to adopt — role-tag this to the joiner so the host doesn't wait on it.
        // Ordering note: `coop: adopted host config` is logged a config round-trip AFTER `coop: linked`,
        // so by the time we reach this step (entered when `linked` advanced) the adopt line is still to
        // come and its window catches it. If they ever landed in one drain batch, the `linked` step
        // would consume the batch first and this would fall back to manual (degrade, never trap).
        .step("config-adopt", "JOINER: confirm the host's settings synced (the 'settings synced' toast).")
        .role(Role::Join)
        .done_when(log_contains("coop: adopted host config"))
        // Shared: capture the run. A genuine human action, but the data it bundles is already in the
        // log — Export just packages it (and makes even a FAILED attempt diagnosable, per the runbook).
        .step("export", "Both: Actions tab -> Export diagnostics, then send the file back.")
        // Pending the apply layer + a real game session (rung 3): verifying the host's settings take
        // EFFECT in-world (enemy scaling, > 4 players) isn't executable yet. Committed as documentation.
        .step("sync-check", "Verify the host's shared settings take effect in-world (scaling, max players).")
        .stub("pending the settings-sync apply layer + a real game session (rung 3)")
}

/// The **rig observation run** (docs/RIG-RUNBOOK.md): drive the session observer through the states we
/// want charted and read the `session change @frame …` snapshots out of the log. The solo legs
/// auto-finish off the observer's log line / live FSM where a fresh signal lands in the step's window
/// (else the manual advance covers it — e.g. the first `session change` may have already fired at the
/// title, and `TryToCreateSession` is transient); the multiplayer legs are committed as **stubs**
/// (Principle 3) since they need a real second player — revive them during the friend test
/// (FRIEND-TEST-RUNBOOK) once the apply layer lands. The full FSM create/join *capture* is its own
/// flagship (`rung3-create-chart`); this guide doesn't duplicate that procedure, only confirms the
/// create edge is reachable. Run it with `[debug.probes] session_probe = true` for the FSM log signals.
fn rig_observation() -> Guide {
    Guide::new("rig-observation")
        .step("boot", "Load a save solo. The session observer logs a 'session change' snapshot.")
        .done_when(game_state_is(GameState::InGame))
        .step(
            "solo-snapshot",
            "Read the solo snapshot in the log: expect lobby=None protocol=None players=0 and the \
             session player limit. Loading / fighting / dying should NOT drive the session FSM solo.",
        )
        .done_when(log_contains("session change"))
        // The create edge is solo-reachable (per SESSION-RE-FINDINGS); the full host/join FSM chart is
        // the flagship's job, so this only confirms the lobby FSM leaves None. TryToCreateSession is
        // transient, so the once-per-frame live read can miss it — the `->TryToCreateSession` log
        // fallback (which needs `[debug.probes] session_probe = true`) is the robust signal; without
        // the probe this degrades to a manual advance.
        .step(
            "host-create-edge",
            "Host a session (place / answer a summon sign). Watching the lobby FSM leave None. Enable \
             [debug.probes] session_probe for the log signal; full create/join capture is the \
             'rung3-create-chart' guide.",
        )
        .done_when(lobby_is(LobbyState::TryToCreateSession).or(log_contains("->TryToCreateSession")))
        // Everything below needs a live session with a real peer, so it ships as committed stubs.
        .step(
            "player-count",
            "With a peer joined: confirm the true player count (does `players` include the local \
             player?) and the in-session `session_player_limit` (expect 4 for open world).",
        )
        .stub("pending a real second player (FRIEND-TEST-RUNBOOK)")
        .step(
            "scaling-in-combat",
            "In a 2+ player fight: confirm enemy HP scales and is idempotent (no per-frame compounding).",
        )
        .stub("pending a real second player + the scaling apply layer")
        .step(
            "area-boundary",
            "Cross an area boundary / fast-travel together. Watch whether protocol passes through \
             WaitReentryToMap and whether the session persists (the heart of 'seamless').",
        )
        .stub("pending a real second player (session persistence is RE-gated)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_guide_builds_and_is_non_empty() {
        for name in NAMES {
            let guide = by_name(name).unwrap_or_else(|| panic!("NAMES lists '{name}' but by_name returned None"));
            assert_eq!(guide.name(), *name, "guide name must match its registry key");
            assert!(!guide.is_empty(), "'{name}' has no steps");
        }
    }

    #[test]
    fn unknown_guide_name_is_none() {
        assert!(by_name("nope").is_none());
        assert!(by_name("").is_none());
    }

    #[test]
    fn flagship_is_solo_runnable_to_completion() {
        // The create-RE flagship must be drivable on a Solo machine (create is solo-capable): every
        // step is untagged, so a Solo runner walks boot -> host -> captured/retry -> done.
        use crate::guide::{ControlHints, GuideRunner, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_chart(), Role::Solo, hints);
        let state = crate::guide::RigState::default();
        // boot -> host (skip), host -> retry (skip default branch), retry -> done (skip).
        let mut input = crate::guide::GuideInput {
            delta: 0.1,
            state: &state,
            new_log_lines: &[],
            done_held: false,
            skip_held: true,
        };
        // Three skips walk it to the end (each is a fresh rising edge after a release).
        for _ in 0..6 {
            let r1 = r.tick(&input);
            if r1.finished_now {
                return;
            }
            // release between presses so the next skip is a rising edge
            input.skip_held = !input.skip_held;
        }
        panic!("flagship did not reach the done toast via skips");
    }

    #[test]
    fn flagship_auto_finishes_to_captured_when_the_fsm_moves() {
        // The dogfood guide's whole reason to exist: when hosting moves the lobby FSM to
        // TryToCreateSession, the "host" step auto-finishes and branches to "captured" (not "retry").
        // Guards the predicate + branch wiring (incl. the `To("captured")` target) that the skip-walk
        // test can't reach.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyState, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_chart(), Role::Solo, hints);

        // boot auto-finishes on InGame -> the host step.
        let in_game = RigState { game_state: GameState::InGame, ..Default::default() };
        let boot = GuideInput { delta: 0.1, state: &in_game, new_log_lines: &[], done_held: false, skip_held: false };
        assert!(r.tick(&boot).banner.unwrap().contains("multiplayer item"), "boot -> host");

        // host: lobby reaches TryToCreateSession -> auto-finish -> branch to "captured".
        let hosting = RigState {
            game_state: GameState::InGame,
            lobby_state: LobbyState::TryToCreateSession,
            ..Default::default()
        };
        let host = GuideInput { delta: 0.1, state: &hosting, new_log_lines: &[], done_held: false, skip_held: false };
        let banner = r.tick(&host).banner.expect("should be on 'captured', not finished");
        assert!(banner.contains("Captured"), "host auto-finish must branch to 'captured', got: {banner}");
    }

    #[test]
    fn two_player_join_walks_each_role_to_done_through_the_stub() {
        // The role-tagged dogfood guide: a Host runner and a Join runner each reach the done toast
        // (past the trailing stub) from the one committed guide, never trapped. Exercises role
        // filtering + stub advance on the real guide, not a synthetic one.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, RigState, Role};
        let state = RigState::default();
        for role in [Role::Host, Role::Join] {
            let hints = ControlHints { done: "D", skip: "S" };
            let mut r = GuideRunner::start(two_player_join(), role, hints);
            let mut finished = false;
            for _ in 0..12 {
                // release frame, then a skip-press (rising edge) frame
                r.tick(&GuideInput { delta: 0.016, state: &state, new_log_lines: &[], done_held: false, skip_held: false });
                let press = GuideInput { delta: 0.016, state: &state, new_log_lines: &[], done_held: false, skip_held: true };
                if r.tick(&press).finished_now {
                    finished = true;
                    break;
                }
            }
            assert!(finished, "role {role:?} must reach the done toast (incl. past the stub)");
        }
    }

    #[test]
    fn two_player_join_joiner_auto_finishes_the_connect_chain_from_the_log() {
        // Principle 2 in action: the joiner's connect steps each auto-finish off a captured log line
        // (discovery resolve -> rung-2 link -> config adoption), so the flow self-detects the connect
        // rather than waiting on a manual "it connected" relay. Pins the substrings the guide matches
        // against the lines `coop.rs` actually emits.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(two_player_join(), Role::Join, hints);
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };
        let tick = |r: &mut GuideRunner, lines: &[String]| {
            r.tick(&GuideInput {
                delta: 0.1,
                state: &ingame,
                new_log_lines: lines,
                done_held: false,
                skip_held: false,
            })
        };

        // boot auto-finishes on InGame -> both-password.
        assert!(tick(&mut r, &[]).banner.unwrap().contains("SAME co-op password"), "boot -> password");
        // both-password is a manual reminder; skip it -> join-join.
        tick(&mut r, &[]); // release (prev_skip = false)
        let skip = GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: true };
        assert!(r.tick(&skip).banner.unwrap().contains("Join world"), "skip password -> join-join");
        // join-join auto-finishes on the joiner-only discovery line -> linked.
        let disc = ["coop: lobby discovery resolved partner peer-7 (we are the client); seeding rung 2".to_string()];
        assert!(tick(&mut r, &disc).banner.unwrap().contains("Steam P2P link"), "join-join -> linked");
        // linked auto-finishes on the rung-2 link milestone -> config-adopt.
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        assert!(tick(&mut r, &linked).banner.unwrap().contains("settings synced"), "linked -> config-adopt");
        // config-adopt auto-finishes on the adoption line -> export.
        let adopt = ["coop: adopted host config (settings synced)".to_string()];
        assert!(tick(&mut r, &adopt).banner.unwrap().contains("Export diagnostics"), "config-adopt -> export");
        // export is a manual step and sync-check is a stub; skip both to confirm the auto-finished run
        // actually TERMINATES (reaches the done toast), not just advances to export.
        let skip = |r: &mut GuideRunner| {
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: false });
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: true })
        };
        assert!(skip(&mut r).stub, "skip export -> the sync-check stub");
        assert!(skip(&mut r).finished_now, "the auto-finished joiner run reaches the done toast");
    }

    #[test]
    fn rig_observation_host_create_edge_auto_finishes_on_the_probe_log_fallback() {
        // The robust create-edge signal is the session-probe FSM log line — the live read of the
        // transient TryToCreateSession can miss the frame. Exercise the log fallback explicitly: a
        // realistic probe line, with the live state NOT showing TryToCreateSession, must still advance.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rig_observation(), Role::Solo, hints);
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };
        let tick = |r: &mut GuideRunner, lines: &[String]| {
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: lines, done_held: false, skip_held: false })
        };
        // Walk the solo legs to host-create-edge.
        tick(&mut r, &[]); // boot -> solo-snapshot
        let snap = ["session change @frame 120: lobby=None protocol=None players=0 limit=6 override=0 | tether: -".to_string()];
        assert!(tick(&mut r, &snap).banner.unwrap().contains("summon sign"), "-> host-create-edge");
        // The probe transition line advances it via the OR fallback, with the live state still None.
        let probe = ["session-probe: FSM @frame 240 lobby None->TryToCreateSession protocol None->None".to_string()];
        assert!(tick(&mut r, &probe).stub, "host-create-edge auto-finishes on the ->TryToCreateSession log line");
    }

    #[test]
    fn two_player_join_host_open_keys_on_the_host_only_discovery_fragment() {
        // The host/joiner discovery steps must key on role-distinct substrings: the host's `host-open`
        // must NOT advance on the joiner's line, only on its own. Guards the substring choice so a
        // host can't false-finish off the joiner's discovery line.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(two_player_join(), Role::Host, hints);
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };

        // boot -> both-password; skip the manual reminder -> host-open.
        r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: false });
        r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: false });
        let skip = GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: true };
        assert!(r.tick(&skip).banner.unwrap().contains("Open World"), "skip password -> host-open");

        // The joiner's line must NOT finish the host step...
        let joiner_line = ["coop: lobby discovery resolved partner peer-7 (we are the client); seeding rung 2".to_string()];
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &joiner_line, done_held: false, skip_held: false })
                .banner.unwrap().contains("Open World"),
            "host-open must ignore the joiner's discovery line",
        );
        // ...but the host's own line does, advancing to the shared linked step.
        let host_line = ["coop: lobby discovery resolved partner peer-7 (we are the host); seeding rung 2".to_string()];
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &host_line, done_held: false, skip_held: false })
                .banner.unwrap().contains("Steam P2P link"),
            "host-open -> linked on the host's own discovery line",
        );
    }

    #[test]
    fn rig_observation_solo_steps_auto_finish_then_stubs_skip_to_done() {
        // The observation run: the solo legs auto-finish off live state + the observer's log line, and
        // the 2-player-gated stubs skip-walk to the done toast without trapping the tester.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyState, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rig_observation(), Role::Solo, hints);

        // boot auto-finishes on InGame -> solo-snapshot.
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: false })
                .banner.unwrap().contains("solo snapshot"),
            "boot -> solo-snapshot",
        );
        // solo-snapshot auto-finishes on the observer's 'session change' line -> host-create-edge.
        let snap = ["session change @frame 120: lobby=None protocol=None players=0 limit=6 override=0 | tether: -".to_string()];
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &snap, done_held: false, skip_held: false })
                .banner.unwrap().contains("summon sign"),
            "solo-snapshot -> host-create-edge",
        );
        // host-create-edge auto-finishes on the (solo-reachable) create edge -> the first stub.
        let creating = RigState { game_state: GameState::InGame, lobby_state: LobbyState::TryToCreateSession, ..Default::default() };
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &creating, new_log_lines: &[], done_held: false, skip_held: false }).stub,
            "host-create-edge -> a stub",
        );
        // The 2-player stubs never auto-finish (state that would satisfy a predicate has no effect);
        // skip walks them to the done toast.
        let loaded = RigState { game_state: GameState::InGame, players: 4, ..Default::default() };
        let mut finished = false;
        for _ in 0..8 {
            r.tick(&GuideInput { delta: 0.016, state: &loaded, new_log_lines: &[], done_held: false, skip_held: false });
            let press = GuideInput { delta: 0.016, state: &loaded, new_log_lines: &[], done_held: false, skip_held: true };
            if r.tick(&press).finished_now {
                finished = true;
                break;
            }
        }
        assert!(finished, "the 2-player stubs must skip-walk to the done toast");
    }
}
