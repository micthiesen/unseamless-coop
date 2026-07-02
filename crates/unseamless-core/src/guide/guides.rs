//! The committed registry of rig-testing guides, selectable via `[debug] guide = "<name>"` (empty =
//! off). Adding a guide is cheap: write a builder function and add one arm to [`by_name`] + one entry
//! to [`NAMES`]. Authoring API + conventions are in `.claude/skills/rig-guides/SKILL.md`; the engine
//! internals are in the parent [`crate::guide`] module and `docs/RIG-GUIDES.md`.

use super::{Advance, Guide, LobbyState, Role, after_secs, game_state_is, lobby_is, log_contains};
use crate::game_state::GameState;

/// Every committed guide's name, in registry order (for the "unknown guide" log + docs).
pub const NAMES: &[&str] = &[
    "rung3-create-chart",
    "overlay-smoke",
    "two-player-join",
    "rig-observation",
    "rung3-create-drive",
];

/// Build the guide named `name`, or `None` if there's no such guide (the binding logs the available
/// [`NAMES`] and runs no guide). Each guide is rebuilt fresh on request — guides hold closures, so
/// they aren't cloneable; that's fine, one is built once per launch.
pub fn by_name(name: &str) -> Option<Guide> {
    match name {
        "rung3-create-chart" => Some(rung3_create_chart()),
        "overlay-smoke" => Some(overlay_smoke()),
        "two-player-join" => Some(two_player_join()),
        "rig-observation" => Some(rig_observation()),
        "rung3-create-drive" => Some(rung3_create_drive()),
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
        // The offline-multiplayer-item re-enable patch (enable_offline_multiplayer) must have taken,
        // or there's no way to drive the FSM. Whether the items are greyed is a UI state the mod can't
        // read, so it's an irreducibly human-perceptual check — and it branches (no point hosting if
        // the patch didn't take), so it's a choice step. The answer is logged for the record.
        .step(
            "items-enabled",
            "Open your pouch/inventory and look at the online multiplayer items (Tarnished's Furled \
             Finger, Furlcalling Finger Remedy). Are they selectable — NOT greyed out?",
        )
        .choice(&[
            ("Yes — selectable", Advance::Next),
            ("No — still greyed", Advance::To("patch-failed")),
        ])
        // Look-first: the answer needs the tester to OPEN their inventory and look, so render as a
        // non-blocking banner first and only open the blocking modal on the done chord — otherwise the
        // modal grabs input the instant it's active and the tester can't go check.
        .look_first()
        .default_branch(Advance::To("patch-failed"))
        .step(
            "host",
            "Use TARNISHED'S FURLED FINGER to host (place your summon sign). Watching the lobby FSM \
             for TryToCreateSession. If a 'network error / return to title' popup appears instead of \
             the sign placing, SKIP this step (that's the broad-patch risk — tell the orchestrator).",
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
            "Captured: the lobby FSM reached TryToCreateSession (hosting drives the create FSM). The \
             orchestrator's write-watch now has the writing instruction — you're done.",
        )
        .step(
            "patch-failed",
            "Items still greyed: the offline re-enable patch didn't take. Tell the orchestrator to \
             check the boot log for the 'patched enable_offline_multiplayer' line.",
        )
        .step(
            "retry",
            "Hosting did not move the FSM (or a network error fired). Try placing/answering a summon \
             sign another way, or report the network-error popup to the orchestrator.",
        )
}

/// A tiny smoke guide to sanity-check the guide system itself on the rig (banner renders, controls
/// work, auto-advance fires, the **choice modal** renders + captures, the done toast shows) without
/// needing any session/RE state. The choice step is the worked example of the modal capability: a
/// human-perceptual question (does the modal render clearly?) whose answer is logged, with a free-form
/// note for *what* looked wrong — exactly the "last resort after logging" the modal is for.
fn overlay_smoke() -> Guide {
    Guide::new("overlay-smoke")
        .step(
            "intro",
            "Rig-guide smoke test. This pinned banner is the current step. Hold the done control to \
             advance.",
        )
        .step("auto", "This step auto-advances after 3 seconds (watch the banner replace itself).")
        .done_when(after_secs(3.0))
        // Choice-modal smoke test: a focused modal with preset options + an optional keyboard note.
        // Both options land on the same finish step (this is a render/capture check, not a real
        // branch); the logged answer + note prove the modal captured the tester's judgement.
        .step(
            "modal",
            "Choice-modal test: does this centered modal render clearly (options highlighted, controls \
             legible)? Pick one; if not, type what's off in the note.",
        )
        .choice(&[("Yes, looks right", Advance::To("manual")), ("No, something is off", Advance::To("manual"))])
        .note()
        .step("manual", "Last step: hold the done control to finish and see the 'done testing' toast.")
}

/// The **canonical role-tagged two-player guide** and the best showcase of log/state auto-finish: it
/// drives the full friend-connect flow (rungs 4 + 2 — password-keyed lobby discovery + the Steam P2P
/// side-channel) from one committed guide. Both machines run the same guide; each machine's role is
/// **derived** from its Open/Join action by the standard [`connect step`](Guide::connect_step) — the
/// host (Open World) sees the host steps, the joiner (Join world) the joiner steps, both the shared
/// ones. No per-machine `[debug] rig_role` needed (it's only an override/solo fallback now); see
/// `docs/FRIEND-TEST-RUNBOOK.md`.
///
/// **Every connect step auto-finishes off the run log**, so the result is captured in the shareable
/// (and host-forwarded) log instead of relayed as "did it connect?": the connect step resolves on the
/// Open/Join action itself, then the rung-2 link milestone (`coop: linked`, logged by
/// `coop::update_link_status`) and the client's config adoption (`coop: adopted host config`, logged by
/// `coop::adopt_host_config`) auto-finish the shared/joiner steps. The matched substrings are stable
/// fragments those sites pin deliberately. The trailing **stub** is the one piece not executable yet:
/// verifying the host's settings take *effect* in-world needs the apply layer + a real game session
/// (rung 3).
fn two_player_join() -> Guide {
    Guide::new("two-player-join")
        // Same area isn't required for the side-channel link, but it sets up the in-world checks the
        // trailing stub will eventually cover.
        .step("both-boot", "Both players: load a character into the world (same area is ideal).")
        .done_when(game_state_is(GameState::InGame))
        // The shared password is the only pairing input (the startup guard already refused to launch
        // without one >= 8 chars, so by here it's set) — a reminder, not a gate, hence manual.
        .step("both-password", "Both: confirm you set the SAME co-op password before launch.")
        // The standard connect step: each machine opens or joins a world, which DERIVES its role (Open
        // World -> Host, Join world -> Join) and auto-finishes on the action. Everything role-tagged
        // after it (config-adopt) then filters by the derived role. This single step replaces the old
        // hand-role-tagged host-open/join-join pair — guide writers no longer set the role by hand.
        .connect_step()
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

/// The **two-machine rung-3 create-drive run** (`docs/RUNG3-DRIVE-RUNBOOK.md`): with the drive
/// probes staged on BOTH machines (`[debug.probes] session_probe + drive_create +
/// force_netsession_ready`, `[gameplay] bypass_session_create_gate`), walk each tester through the
/// connect (rungs 4 + 2, roles derived by the standard connect step), then WATCH the one-shot driven
/// create and capture whether a real peer sizes leg B's session-slot array — the confirmed
/// capacity-0 root cause of the solo create failure (`docs/SESSION-DRIVE.md` > "Why a direct create
/// fails offline"). Every outcome step auto-finishes off `session-probe:` log lines and branches on
/// what the window captured, so the verdict lands in the shareable/forwarded log instead of being
/// relayed. Assumes the driver is re-timed to fire post-link (the runbook's prerequisite); if it
/// fired early the drive-watch step degrades to skip -> `inspect`, never traps.
fn rung3_create_drive() -> Guide {
    Guide::new("rung3-create-drive")
        .step("both-boot", "Both: load a character into the world.")
        .done_when(game_state_is(GameState::InGame))
        // The standard connect step: Open World derives Host, Join world derives Join. Both machines
        // run this same guide; everything role-tagged after here filters by the derived role.
        .connect_step()
        // BOTH machines log `coop: linked ...` on the rung-2 link edge (a stable fragment pinned at
        // coop::update_link_status), so both advance off the same captured signal.
        .step("linked", "Both: wait for the Steam P2P link (the 'Co-op partner connected' toast).")
        .done_when(log_contains("coop: linked"))
        // JOINER only: the client adopts the host's shared settings (logged by
        // coop::adopt_host_config); the host has nothing to adopt, so don't make it wait.
        // Caveat: that line only fires when the synced config DIFFERS from the joiner's own — with
        // both machines on the identical seed config (the Steam Deck path) it may never appear, and
        // this degrades to a manual skip/done (never traps). The runbook notes this.
        .step("config-adopt", "JOINER: wait for the host's settings to sync (the 'settings synced' toast).")
        .role(Role::Join)
        .done_when(log_contains("coop: adopted host config"))
        // The drive is automatic (the one-shot SessionCreateDriver, [debug.probes] drive_create) —
        // the tester only watches. Finish on the driver's terminal line ("drive-create returned
        // <bool> — lobby_state now ...", logged by coop/session_probe.rs::SessionCreateDriver) or on
        // the lobby FSM visibly leaving None, then branch on which outcome the window captured.
        .step(
            "drive-watch",
            "Nothing to press: the create drive fires by itself now that the link is up. Watching \
             for the drive-create result in the log.",
        )
        .done_when(
            log_contains("drive-create returned")
                // The driver declining to fire is a captured verdict too (`drive-create skipped —
                // lobby_state is ..., need None`, same emitter) — route it through the branch
                // (lands on inspect) instead of degrading to a manual skip.
                .or(log_contains("drive-create skipped"))
                .or(lobby_is(LobbyState::TryToCreateSession))
                .or(lobby_is(LobbyState::Host))
                .or(lobby_is(LobbyState::FailedToCreateSession)),
        )
        .branch(|ctx| {
            if ctx.log_contains("drive-create returned true")
                || ctx.state.lobby_state == LobbyState::TryToCreateSession
                || ctx.state.lobby_state == LobbyState::Host
            {
                Advance::To("pass")
            } else if ctx.log_contains("cap=0 ") {
                // The gate-trace legb-entry line in this window still reads `slot-array
                // [+0x20]cap=0 ` — the peer did NOT size the slot array. Logged by
                // session_probe.rs::log_legb_entry. The trailing space pins the exact zero token
                // defensively (a nonzero cap like `cap=10` can't substring-match `cap=0` anyway).
                Advance::To("cap-zero")
            } else if ctx.log_contains("drive-create returned false") {
                // The drive DID run in this window and failed, but no capacity-0 line accompanied
                // it: the runbook's PARTIAL signature (cap>0 yet the tail still rejected) — unless
                // the legb-entry tracer simply didn't hook, which the banner tells the tester to
                // check.
                Advance::To("partial")
            } else {
                Advance::To("inspect")
            }
        })
        .default_branch(Advance::To("inspect"))
        .step(
            "pass",
            "PASS: the driven create succeeded (lobby left None toward Host) - a real peer sizes \
             the session-slot array. Rung-3 create is unblocked; export next.",
        )
        .branch(|_| Advance::To("export"))
        .default_branch(Advance::To("export"))
        .step(
            "cap-zero",
            "Create still rejected with slot-array capacity 0 - a live lobby + peer alone does not \
             size it. A real (negative) result; export next.",
        )
        .branch(|_| Advance::To("export"))
        .default_branch(Advance::To("export"))
        .step(
            "partial",
            "PARTIAL: create failed past the capacity check (confirm the gate-trace line shows \
             cap > 0) - a new reject past everything charted. Export next.",
        )
        .branch(|_| Advance::To("export"))
        .default_branch(Advance::To("export"))
        .step(
            "inspect",
            "No create verdict in this window (the drive may have fired before this step, declined \
             to fire, or not fired at all). Details, if any, are in the log; export next.",
        )
        .step("export", "Both: Actions tab -> Export diagnostics, then send the file back.")
        // drive_create only drives CREATE; the join side (Call B: join the driven host's session by
        // SteamID) has no driver yet — committed as documentation, revived when that lever lands.
        .step("join-leg", "Drive the JOIN side (Call B) against the driven host session.")
        .stub("pending a join-side driver (drive_create drives create only)")
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
    fn overlay_smoke_choice_modal_captures_an_answer_and_reaches_done() {
        // The worked example of the choice capability: drive overlay-smoke to its choice step, confirm
        // an option with a free-form note, and check the answer surfaces (so the binding logs it) and
        // the guide finishes. Exercises the modal end-to-end on the committed example guide.
        use crate::guide::{ChoiceInput, ControlHints, GuideInput, GuideRunner, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(overlay_smoke(), Role::Solo, hints);
        let state = RigState::default();
        let frame = |r: &mut GuideRunner, choice: ChoiceInput, done: bool| {
            r.tick(&GuideInput {
                delta: 5.0, // long enough to cross the auto step's after_secs(3) in one tick
                state: &state,
                new_log_lines: &[],
                done_held: done,
                skip_held: false,
                choice,
            })
        };
        // intro (manual): release then a long done-hold advances it.
        frame(&mut r, ChoiceInput::default(), false);
        frame(&mut r, ChoiceInput::default(), true); // intro -> auto
        // auto auto-finishes on the 5s frame -> the choice modal.
        let at_modal = frame(&mut r, ChoiceInput::default(), false);
        let view = at_modal.choice.expect("overlay-smoke reaches its choice modal");
        assert_eq!(view.options.len(), 2);
        assert!(view.note_enabled, "the smoke choice offers a free-form note");
        // Confirm the first option with a note -> the answer is captured and we land on "manual".
        let confirm = ChoiceInput { up: false, down: false, confirm: true, note: "looked fine" };
        let out = frame(&mut r, confirm, false);
        let made = out.choice_made.expect("confirm captures the answer");
        assert_eq!(made.label, "Yes, looks right");
        assert_eq!(made.note, "looked fine");
        assert!(out.banner.unwrap().contains("Last step"), "choice -> manual");
        // manual (done) finishes the guide.
        frame(&mut r, ChoiceInput::default(), false); // release
        assert!(frame(&mut r, ChoiceInput::default(), true).finished_now, "reaches the done toast");
    }

    #[test]
    fn flagship_is_solo_runnable_to_completion() {
        // The create-RE flagship must be drivable on a Solo machine (create is solo-capable): every
        // step is untagged, so a Solo runner can walk the whole guide by skipping. Skip escapes a
        // choice step too (taking its default_branch), so the never-trap guarantee holds end to end.
        use crate::guide::{ControlHints, GuideRunner, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_chart(), Role::Solo, hints);
        let state = crate::guide::RigState::default();
        // Skip path: boot -> items-enabled -> patch-failed (choice default_branch) -> retry -> done.
        let mut input = crate::guide::GuideInput {
            delta: 0.1,
            state: &state,
            new_log_lines: &[],
            done_held: false,
            skip_held: true,
            choice: crate::guide::ChoiceInput::default(),
        };
        // Four skips walk it to the end (each is a fresh rising edge after a release).
        for _ in 0..10 {
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
        use crate::guide::{ChoiceInput, ControlHints, GuideInput, GuideRunner, LobbyState, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_chart(), Role::Solo, hints);

        // boot auto-finishes on InGame -> the items-enabled choice step. It's `.look_first()`, so it
        // renders FIRST as a non-blocking banner (the tester opens their inventory to check) — NOT the
        // modal yet — and the banner hints "answer" for the done chord.
        let in_game = RigState { game_state: GameState::InGame, ..Default::default() };
        let boot = GuideInput { delta: 0.1, state: &in_game, new_log_lines: &[], done_held: false, skip_held: false, choice: ChoiceInput::default() };
        let banner = r.tick(&boot).banner.expect("look_first: items-enabled shows a banner first, not a modal");
        assert!(banner.contains("greyed"), "items-enabled banner prompt, got: {banner}");
        assert!(banner.contains("= answer"), "look-first banner hints 'answer' for the done chord, got: {banner}");

        // The done chord OPENS the modal (a single long-held frame crosses the hold threshold).
        let open = GuideInput { delta: 1.0, state: &in_game, new_log_lines: &[], done_held: true, skip_held: false, choice: ChoiceInput::default() };
        let view = r.tick(&open).choice.expect("done chord opens the items-enabled modal");
        assert!(view.prompt.contains("greyed"), "opened modal carries the prompt, got: {}", view.prompt);

        // items-enabled: confirm the first option ("Yes — selectable") -> Advance::Next -> the host step.
        let yes = GuideInput { delta: 0.1, state: &in_game, new_log_lines: &[], done_held: false, skip_held: false, choice: ChoiceInput { up: false, down: false, confirm: true, note: "" } };
        assert!(r.tick(&yes).banner.unwrap().contains("summon sign"), "items-enabled (Yes) -> host");

        // host: lobby reaches TryToCreateSession -> auto-finish -> branch to "captured".
        let hosting = RigState {
            game_state: GameState::InGame,
            lobby_state: LobbyState::TryToCreateSession,
            ..Default::default()
        };
        let host = GuideInput { delta: 0.1, state: &hosting, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() };
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
                r.tick(&GuideInput { delta: 0.016, state: &state, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() });
                let press = GuideInput { delta: 0.016, state: &state, new_log_lines: &[], done_held: false, skip_held: true, choice: crate::guide::ChoiceInput::default() };
                if r.tick(&press).finished_now {
                    finished = true;
                    break;
                }
            }
            assert!(finished, "role {role:?} must reach the done toast (incl. past the stub)");
        }
    }

    #[test]
    fn two_player_join_joiner_derives_join_then_auto_finishes_the_rest_from_the_log() {
        // The retrofit in action: start UNRESOLVED (Solo), and the standard connect step DERIVES the
        // joiner role from the Join action (lobby_intent = Join). The role-tagged config-adopt step is
        // then visible (post-derivation filtering), and the shared/joiner steps auto-finish off captured
        // log lines (rung-2 link -> config adoption) — the flow self-detects rather than relaying.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyIntent, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(two_player_join(), Role::Solo, hints);
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };
        let joining =
            RigState { game_state: GameState::InGame, lobby_intent: LobbyIntent::Join, ..Default::default() };
        let tick = |r: &mut GuideRunner, state: &RigState, lines: &[String]| {
            r.tick(&GuideInput {
                delta: 0.1,
                state,
                new_log_lines: lines,
                done_held: false,
                skip_held: false,
                choice: crate::guide::ChoiceInput::default(),
            })
        };

        // boot auto-finishes on InGame -> both-password.
        assert!(tick(&mut r, &ingame, &[]).banner.unwrap().contains("SAME co-op password"), "boot -> password");
        // both-password is a manual reminder; skip it -> the connect step.
        tick(&mut r, &ingame, &[]); // release (prev_skip = false)
        let skip = GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: true, choice: crate::guide::ChoiceInput::default() };
        assert!(r.tick(&skip).banner.unwrap().contains("Open World to host or Join"), "skip password -> connect step");
        // The Join action resolves the intent -> connect derives Join and advances to linked.
        assert!(tick(&mut r, &joining, &[]).banner.unwrap().contains("Steam P2P link"), "connect (Join) -> linked");
        // linked auto-finishes on the rung-2 link milestone -> config-adopt (visible: role derived Join).
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        assert!(tick(&mut r, &joining, &linked).banner.unwrap().contains("settings synced"), "linked -> config-adopt");
        // config-adopt auto-finishes on the adoption line -> export.
        let adopt = ["coop: adopted host config (settings synced)".to_string()];
        assert!(tick(&mut r, &joining, &adopt).banner.unwrap().contains("Export diagnostics"), "config-adopt -> export");
        // export is a manual step and sync-check is a stub; skip both to confirm the auto-finished run
        // actually TERMINATES (reaches the done toast), not just advances to export.
        let skip = |r: &mut GuideRunner| {
            r.tick(&GuideInput { delta: 0.1, state: &joining, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() });
            r.tick(&GuideInput { delta: 0.1, state: &joining, new_log_lines: &[], done_held: false, skip_held: true, choice: crate::guide::ChoiceInput::default() })
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
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: lines, done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() })
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
    fn two_player_join_derives_host_and_skips_the_joiner_only_config_adopt() {
        // The host machine: starting UNRESOLVED, the Open World action (lobby_intent = Host) derives Host
        // via the connect step, and the joiner-only config-adopt step is then filtered out — the host
        // goes linked -> export directly. Guards both derivation and post-derivation role filtering on
        // the real guide. (The connect step finishes on the action itself, not a discovery log line.)
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyIntent, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(two_player_join(), Role::Solo, hints);
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };
        let hosting =
            RigState { game_state: GameState::InGame, lobby_intent: LobbyIntent::Host, ..Default::default() };

        // boot -> both-password; release, then skip the manual reminder -> the connect step.
        r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() });
        r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() });
        let skip = GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: true, choice: crate::guide::ChoiceInput::default() };
        assert!(r.tick(&skip).banner.unwrap().contains("Open World to host or Join"), "skip password -> connect step");

        // The Open World action resolves the intent -> connect derives Host and advances to linked.
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &hosting, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() })
                .banner.unwrap().contains("Steam P2P link"),
            "connect (Open World) -> linked, role derived Host",
        );
        // linked auto-finishes on the rung-2 link milestone; config-adopt is Join-only, so the host
        // skips straight to export.
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &hosting, new_log_lines: &linked, done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() })
                .banner.unwrap().contains("Export diagnostics"),
            "host skips the joiner-only config-adopt -> export",
        );
    }

    #[test]
    fn rung3_create_drive_walks_each_role_to_done_through_the_stub() {
        // The two-machine drive guide must never trap: a Host runner and a Join runner each
        // skip-walk the whole guide (through the drive-watch default branch, the inspect step, and
        // the trailing join-leg stub) to the done toast. Also proves it's solo-walkable (a pinned
        // role with no peer signals still escapes every step).
        use crate::guide::{ControlHints, GuideInput, GuideRunner, RigState, Role};
        let state = RigState::default();
        for role in [Role::Host, Role::Join, Role::Solo] {
            let hints = ControlHints { done: "D", skip: "S" };
            let mut r = GuideRunner::start(rung3_create_drive(), role, hints);
            let mut finished = false;
            for _ in 0..16 {
                // release frame, then a skip-press (rising edge) frame
                r.tick(&GuideInput { delta: 0.016, state: &state, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() });
                let press = GuideInput { delta: 0.016, state: &state, new_log_lines: &[], done_held: false, skip_held: true, choice: crate::guide::ChoiceInput::default() };
                if r.tick(&press).finished_now {
                    finished = true;
                    break;
                }
            }
            assert!(finished, "role {role:?} must reach the done toast (incl. past the stub)");
        }
    }

    #[test]
    fn rung3_create_drive_host_auto_finishes_to_pass_on_the_drive_success_line() {
        // The run's whole point: starting UNRESOLVED, the connect step derives Host from the Open
        // World action, the linked step auto-finishes on the rung-2 milestone (the joiner-only
        // config-adopt is filtered out), and the drive-watch step auto-finishes on the driver's
        // terminal line — branching to "pass" when it reports success. Guards the predicate + branch
        // wiring against the real log-line formats.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyIntent, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_drive(), Role::Solo, hints);
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };
        let hosting =
            RigState { game_state: GameState::InGame, lobby_intent: LobbyIntent::Host, ..Default::default() };
        let tick = |r: &mut GuideRunner, state: &RigState, lines: &[String]| {
            r.tick(&GuideInput {
                delta: 0.1,
                state,
                new_log_lines: lines,
                done_held: false,
                skip_held: false,
                choice: crate::guide::ChoiceInput::default(),
            })
        };

        // boot auto-finishes on InGame -> the connect step.
        assert!(tick(&mut r, &ingame, &[]).banner.unwrap().contains("Open World to host or Join"), "boot -> connect");
        // Open World resolves the intent -> Host derived -> linked.
        assert!(tick(&mut r, &hosting, &[]).banner.unwrap().contains("Steam P2P link"), "connect (Open World) -> linked");
        // linked auto-finishes on the rung-2 milestone; config-adopt is Join-only, so the host lands
        // straight on drive-watch.
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        assert!(tick(&mut r, &hosting, &linked).banner.unwrap().contains("drive-create result"), "host: linked -> drive-watch");
        // The driver's terminal line (real format, success shape) auto-finishes and branches to pass.
        let drove = [
            "session-probe: gate-trace legb-entry REACHED — NetworkSession=0x143dcdad0 reject#1 [+0x10]=1 slot-array [+0x20]cap=6 [+0x24]count=0 (cap 0 => leg B tail can't store the session)".to_string(),
            "session-probe: drive-create returned true — lobby_state now TryToCreateSession (TryToCreateSession=driven OK; FailedToCreateSession=internal gate rejected)".to_string(),
        ];
        let banner = tick(&mut r, &hosting, &drove).banner.expect("drive-watch -> pass");
        assert!(banner.contains("PASS"), "success line must branch to 'pass', got: {banner}");
        // pass finishes (manual done) to export via its constant branch — the outcome steps must not
        // fall through into each other serially.
        tick(&mut r, &hosting, &[]); // release
        let done = GuideInput { delta: 1.0, state: &hosting, new_log_lines: &[], done_held: true, skip_held: false, choice: crate::guide::ChoiceInput::default() };
        assert!(r.tick(&done).banner.unwrap().contains("Export diagnostics"), "pass -> export");
    }

    #[test]
    fn rung3_create_drive_joiner_walks_the_log_path_to_cap_zero_and_on_to_export() {
        // The joiner leg end to end, on the negative verdict: the connect step derives Join, the
        // joiner-only config-adopt auto-finishes on the adoption line, and a failed drive whose
        // gate-trace window still shows `cap=0 ` branches to cap-zero (not the generic inspect) —
        // then cap-zero's own constant branch converges on export (guarding that the outcome steps
        // don't fall through into each other serially). Fixtures match the real emitter formats.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyIntent, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_drive(), Role::Solo, hints);
        let joining =
            RigState { game_state: GameState::InGame, lobby_intent: LobbyIntent::Join, ..Default::default() };
        let tick = |r: &mut GuideRunner, lines: &[String]| {
            r.tick(&GuideInput {
                delta: 0.1,
                state: &joining,
                new_log_lines: lines,
                done_held: false,
                skip_held: false,
                choice: crate::guide::ChoiceInput::default(),
            })
        };
        // boot -> connect; the Join action derives the role -> linked.
        tick(&mut r, &[]); // boot -> connect
        assert!(tick(&mut r, &[]).banner.unwrap().contains("Steam P2P link"), "connect (Join) -> linked");
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        assert!(tick(&mut r, &linked).banner.unwrap().contains("settings synced"), "joiner: linked -> config-adopt");
        let adopt = ["coop: adopted host config (settings synced)".to_string()];
        assert!(tick(&mut r, &adopt).banner.unwrap().contains("drive-create result"), "config-adopt -> drive-watch");
        // Failure shape: the legb-entry trace still reads cap=0 and the driver returns false.
        let failed = [
            "session-probe: gate-trace legb-entry REACHED — NetworkSession=0x143dcdad0 reject#1 [+0x10]=1 slot-array [+0x20]cap=0 [+0x24]count=0 (cap 0 => leg B tail can't store the session)".to_string(),
            "session-probe: drive-create returned false — lobby_state now FailedToCreateSession (TryToCreateSession=driven OK; FailedToCreateSession=internal gate rejected)".to_string(),
        ];
        let banner = tick(&mut r, &failed).banner.expect("drive-watch -> cap-zero");
        assert!(banner.contains("capacity 0"), "cap=0 failure must branch to 'cap-zero', got: {banner}");
        // cap-zero (manual done) must converge on export via its constant branch.
        tick(&mut r, &[]); // release
        let done = GuideInput { delta: 1.0, state: &joining, new_log_lines: &[], done_held: true, skip_held: false, choice: crate::guide::ChoiceInput::default() };
        assert!(r.tick(&done).banner.unwrap().contains("Export diagnostics"), "cap-zero -> export");
    }

    #[test]
    fn rung3_create_drive_drive_watch_finishes_on_live_lobby_state_alone() {
        // The live-state fallback arms: with NO drive-create log line in the window, the lobby FSM
        // visibly sitting at Host must still auto-finish drive-watch and branch to pass (the log
        // drain could miss/reword; the FSM read is the backstop). Guards the lobby_is(...) arms of
        // done_when and the state half of the branch, which the log-driven tests leave dead.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyIntent, LobbyState, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_drive(), Role::Solo, hints);
        let hosting =
            RigState { game_state: GameState::InGame, lobby_intent: LobbyIntent::Host, ..Default::default() };
        let tick = |r: &mut GuideRunner, state: &RigState| {
            r.tick(&GuideInput {
                delta: 0.1,
                state,
                new_log_lines: &[],
                done_held: false,
                skip_held: false,
                choice: crate::guide::ChoiceInput::default(),
            })
        };
        tick(&mut r, &hosting); // boot -> connect
        tick(&mut r, &hosting); // connect (Open World) -> linked
        // linked still needs its log line — feed it, landing on drive-watch.
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        let on_watch = r.tick(&GuideInput { delta: 0.1, state: &hosting, new_log_lines: &linked, done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() });
        assert!(on_watch.banner.unwrap().contains("drive-create result"), "-> drive-watch");
        // Live FSM at Host, empty log window -> auto-finish -> pass.
        let live_host = RigState {
            game_state: GameState::InGame,
            lobby_intent: LobbyIntent::Host,
            lobby_state: LobbyState::Host,
            ..Default::default()
        };
        let banner = tick(&mut r, &live_host).banner.expect("drive-watch -> pass on live state");
        assert!(banner.contains("PASS"), "live lobby Host must branch to 'pass', got: {banner}");
    }

    #[test]
    fn rung3_create_drive_discriminates_partial_from_cap_zero() {
        // A failed drive WITHOUT a capacity-0 line (the gate-trace shows a sized array) is the
        // runbook's PARTIAL verdict — it must land on the partial step, not cap-zero and not the
        // generic inspect. Also proves a sized `cap=6` line can't satisfy the pinned `cap=0 `
        // fragment.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyIntent, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_drive(), Role::Solo, hints);
        let hosting =
            RigState { game_state: GameState::InGame, lobby_intent: LobbyIntent::Host, ..Default::default() };
        let tick = |r: &mut GuideRunner, lines: &[String]| {
            r.tick(&GuideInput {
                delta: 0.1,
                state: &hosting,
                new_log_lines: lines,
                done_held: false,
                skip_held: false,
                choice: crate::guide::ChoiceInput::default(),
            })
        };
        tick(&mut r, &[]); // boot -> connect
        tick(&mut r, &[]); // connect (Open World) -> linked
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        assert!(tick(&mut r, &linked).banner.unwrap().contains("drive-create result"), "-> drive-watch");
        let failed_sized = [
            "session-probe: gate-trace legb-entry REACHED — NetworkSession=0x143dcdad0 reject#1 [+0x10]=1 slot-array [+0x20]cap=6 [+0x24]count=0 (cap 0 => leg B tail can't store the session)".to_string(),
            "session-probe: drive-create returned false — lobby_state now FailedToCreateSession (TryToCreateSession=driven OK; FailedToCreateSession=internal gate rejected)".to_string(),
        ];
        let banner = tick(&mut r, &failed_sized).banner.expect("drive-watch -> partial");
        assert!(banner.contains("PARTIAL"), "cap=6 + returned false must branch to 'partial', got: {banner}");
        // partial (manual done) must converge on export via its constant branch, like its siblings.
        tick(&mut r, &[]); // release
        let done = GuideInput { delta: 1.0, state: &hosting, new_log_lines: &[], done_held: true, skip_held: false, choice: crate::guide::ChoiceInput::default() };
        assert!(r.tick(&done).banner.unwrap().contains("Export diagnostics"), "partial -> export");
    }

    #[test]
    fn rung3_create_drive_routes_a_skipped_drive_to_inspect() {
        // The driver declining to fire (`drive-create skipped — lobby_state is ..., need None`,
        // logged by session_probe.rs::SessionCreateDriver) is a captured verdict: drive-watch must
        // auto-finish on it and land on inspect via the branch's else arm — not strand the tester
        // on a step waiting for a result line that will never come.
        use crate::guide::{ControlHints, GuideInput, GuideRunner, LobbyIntent, RigState, Role};
        let hints = ControlHints { done: "D", skip: "S" };
        let mut r = GuideRunner::start(rung3_create_drive(), Role::Solo, hints);
        let hosting =
            RigState { game_state: GameState::InGame, lobby_intent: LobbyIntent::Host, ..Default::default() };
        let tick = |r: &mut GuideRunner, lines: &[String]| {
            r.tick(&GuideInput {
                delta: 0.1,
                state: &hosting,
                new_log_lines: lines,
                done_held: false,
                skip_held: false,
                choice: crate::guide::ChoiceInput::default(),
            })
        };
        tick(&mut r, &[]); // boot -> connect
        tick(&mut r, &[]); // connect (Open World) -> linked
        let linked = ["coop: linked with partner peer-7 (rung 2); versions match".to_string()];
        assert!(tick(&mut r, &linked).banner.unwrap().contains("drive-create result"), "-> drive-watch");
        let skipped = ["session-probe: drive-create skipped — lobby_state is Host, need None (already in/at a session)".to_string()];
        let banner = tick(&mut r, &skipped).banner.expect("drive-watch -> inspect");
        assert!(banner.contains("No create verdict"), "a skipped drive must land on 'inspect', got: {banner}");
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
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() })
                .banner.unwrap().contains("solo snapshot"),
            "boot -> solo-snapshot",
        );
        // solo-snapshot auto-finishes on the observer's 'session change' line -> host-create-edge.
        let snap = ["session change @frame 120: lobby=None protocol=None players=0 limit=6 override=0 | tether: -".to_string()];
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &ingame, new_log_lines: &snap, done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() })
                .banner.unwrap().contains("summon sign"),
            "solo-snapshot -> host-create-edge",
        );
        // host-create-edge auto-finishes on the (solo-reachable) create edge -> the first stub.
        let creating = RigState { game_state: GameState::InGame, lobby_state: LobbyState::TryToCreateSession, ..Default::default() };
        assert!(
            r.tick(&GuideInput { delta: 0.1, state: &creating, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() }).stub,
            "host-create-edge -> a stub",
        );
        // The 2-player stubs never auto-finish (state that would satisfy a predicate has no effect);
        // skip walks them to the done toast.
        let loaded = RigState { game_state: GameState::InGame, players: 4, ..Default::default() };
        let mut finished = false;
        for _ in 0..8 {
            r.tick(&GuideInput { delta: 0.016, state: &loaded, new_log_lines: &[], done_held: false, skip_held: false, choice: crate::guide::ChoiceInput::default() });
            let press = GuideInput { delta: 0.016, state: &loaded, new_log_lines: &[], done_held: false, skip_held: true, choice: crate::guide::ChoiceInput::default() };
            if r.tick(&press).finished_now {
                finished = true;
                break;
            }
        }
        assert!(finished, "the 2-player stubs must skip-walk to the done toast");
    }
}
