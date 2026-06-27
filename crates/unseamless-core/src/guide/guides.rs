//! The committed registry of rig-testing guides, selectable via `[debug] guide = "<name>"` (empty =
//! off). Adding a guide is cheap: write a builder function and add one arm to [`by_name`] + one entry
//! to [`NAMES`]. Authoring API + conventions are in `.claude/skills/rig-guides/SKILL.md`; the engine
//! internals are in the parent [`crate::guide`] module and `docs/RIG-GUIDES.md`.

use super::{
    Advance, Guide, LobbyState, ProtocolState, after_secs, game_state_is, lobby_is, log_contains,
    players_at_least,
};
use crate::game_state::GameState;

/// Every committed guide's name, in registry order (for the "unknown guide" log + docs).
pub const NAMES: &[&str] = &["rung3-create-chart", "overlay-smoke", "two-player-join"];

/// Build the guide named `name`, or `None` if there's no such guide (the binding logs the available
/// [`NAMES`] and runs no guide). Each guide is rebuilt fresh on request — guides hold closures, so
/// they aren't cloneable; that's fine, one is built once per launch.
pub fn by_name(name: &str) -> Option<Guide> {
    match name {
        "rung3-create-chart" => Some(rung3_create_chart()),
        "overlay-smoke" => Some(overlay_smoke()),
        "two-player-join" => Some(two_player_join()),
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

/// A two-player join dogfood guide, showing role tagging: the host machine sees the host steps, the
/// joiner sees the joiner steps, both see the shared steps — all from this one committed guide (set
/// `[debug] rig_role` to `host` / `join` on each machine). The final step is a committed **stub**:
/// host-enforced settings-sync verification isn't executable until the sync core lands, so it's
/// documented now and revived later.
fn two_player_join() -> Guide {
    Guide::new("two-player-join")
        .step("both-boot", "Both players: load a character into the world, in the same area.")
        .done_when(game_state_is(GameState::InGame))
        .step("host-open", "HOST: open the menu (RB+L3+R3), pick Open World, and place your sign / host.")
        .role(super::Role::Host)
        .done_when(lobby_is(LobbyState::Host).or(lobby_is(LobbyState::TryToCreateSession)))
        .step("join-join", "JOINER: open the menu, pick Join World, and connect to the host.")
        .role(super::Role::Join)
        .done_when(lobby_is(LobbyState::Client).or(lobby_is(LobbyState::TryToJoinSession)))
        .step("confirm", "Both: confirm you can see each other in-world (roster should show 2 players).")
        .done_when(players_at_least(2).and(protocol_in_game()))
        .step("sync-check", "Verify the host's shared settings (scaling, max players) apply on the joiner.")
        .stub("pending the settings-sync core")
}

/// Small local helper: the protocol FSM is in the in-game multiplayer state. Kept here (not as a
/// public constructor) since it's only this guide's convenience.
fn protocol_in_game() -> super::Predicate {
    super::protocol_is(ProtocolState::Ingame)
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
}
