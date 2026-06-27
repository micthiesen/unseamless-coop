//! The game-thread binding for the in-overlay **rig-testing guide** — the thin shell over the
//! host-tested engine ([`unseamless_core::guide`]). It gathers a per-frame [`GuideInput`] (read-only
//! game/session state, controller chords, new log lines), ticks the [`GuideRunner`], publishes the
//! pinned banner for the overlay to draw ([`crate::rig_guide`]), and fires the "done testing" toast
//! on completion. All decision logic lives in core; this module just samples and renders.
//!
//! Debug-only (`#[cfg(debug_assertions)]`): registered (via [`feature`]) only when `[debug] guide`
//! names a committed guide, so a normal/release build carries nothing. See `docs/RIG-GUIDES.md`.

use eldenring::cs::{CSSessionManager, CSTaskGroupIndex};
use unseamless_core::config::Config;
use unseamless_core::guide::{
    ChoiceInput, ControlHints, GuideInput, GuideRunner, LobbyState, ProtocolState, RigState, guides,
};
use unseamless_core::notifications::Severity;
use unseamless_core::pad::{XINPUT_DPAD_DOWN, XINPUT_DPAD_UP, XINPUT_LEFT_THUMB};

use crate::feature::{Feature, Tick};
use crate::rig_guide::{RigBanner, RigView};

// The two control chords (chosen here, trivially swappable — retuning these is expected). Made only
// of **standard** XInput bits (no Guide/Home button) so they survive Steam Input, the same reasoning
// the overlay toggle (RB+L3+R3) documents. Both are read from the live pad snapshot the overlay's
// XInput hook already captures. NOTE: these are 2-button combos and the read is non-consuming, so the
// chord also feeds the game (L3=sprint/click, D-pad=item switch); fine on a debug rig, but a future
// retune toward a 3-button gameplay-neutral combo would avoid the double-effect.
//   DONE = hold L3 + D-pad Up   (the engine applies the hold-to-confirm timer)
//   SKIP = press L3 + D-pad Down (the engine fires on the rising edge)
const DONE_CHORD: u16 = XINPUT_LEFT_THUMB | XINPUT_DPAD_UP;
const SKIP_CHORD: u16 = XINPUT_LEFT_THUMB | XINPUT_DPAD_DOWN;

/// The hint labels the engine appends to every banner — must describe [`DONE_CHORD`]/[`SKIP_CHORD`].
const HINTS: ControlHints = ControlHints { done: "L3 + Up", skip: "L3 + Down" };

/// Build the rig-guide feature for the configured guide, or an empty set when no guide is selected
/// (`[debug] guide` empty) or names one that doesn't exist. Mirrors the `probe_features` assembly in
/// [`crate::session_probe`] so `app::build_features` can just `extend` with it. A no-op for normal
/// play, where `[debug] guide` is empty.
pub fn feature(config: &Config) -> Vec<Box<dyn Feature>> {
    let name = config.debug.guide.trim();
    if name.is_empty() {
        return Vec::new();
    }
    match guides::by_name(name) {
        Some(guide) => {
            // The log tee is enabled lazily from `on_frame` (only while a step is actually showing),
            // not here — so pre-guide boot lines never reach the first step and a guide that starts
            // with no role-visible steps never leaves the tee on.
            log::info!(
                "rig-guide: running '{}' as role {:?} ({} steps); done = {}, skip = {}",
                guide.name(),
                config.debug.rig_role,
                guide.len(),
                HINTS.done,
                HINTS.skip,
            );
            vec![Box::new(RigGuideFeature {
                runner: GuideRunner::start(guide, config.debug.rig_role, HINTS),
            })]
        }
        None => {
            log::warn!(
                "rig-guide: no guide named '{name}' (available: {:?}); guide off",
                guides::NAMES
            );
            Vec::new()
        }
    }
}

/// Drives one [`GuideRunner`] each frame. Holds no game state of its own — every frame it re-samples
/// the world and hands a snapshot to the engine.
pub struct RigGuideFeature {
    runner: GuideRunner,
}

impl Feature for RigGuideFeature {
    fn name(&self) -> &'static str {
        "rig-guide"
    }

    // FrameBegin: ticks every frame including title/menu, so the banner shows and advances from the
    // moment the guide starts (the boot step waits at the title, like the session observer).
    fn phase(&self) -> CSTaskGroupIndex {
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self, tick: Tick) {
        // New log lines emitted since the last tick (for `log_contains` predicates).
        let mut lines = Vec::new();
        crate::guide_log::drain(|line| lines.push(line));

        // Read-only session FSM, mapped to the core mirrors. `None`/`None`/`0` when no session is up
        // (solo / pre-session) — read the same shared way the observer + probe do, so we can't drift.
        let (lobby_state, protocol_state, players) =
            crate::sdk::with_instance::<CSSessionManager, _>(|s| {
                let view = crate::session::read(s);
                (
                    LobbyState::from_u32(view.lobby_state as u32),
                    ProtocolState::from_u32(view.protocol_state as u32),
                    view.players,
                )
            })
            .unwrap_or((LobbyState::None, ProtocolState::None, 0));

        let state = RigState {
            game_state: crate::playstate::current(),
            lobby_state,
            protocol_state,
            players,
        };

        // Controller chords from the live pad snapshot (an atomic read written by the overlay's XInput
        // hook, safe from the game thread). The engine turns the raw held bools into hold-to-confirm
        // (done) and a one-shot edge (skip). The skip chord still escapes a choice modal — it's read
        // here regardless of overlay state, so "skip always escapes" holds even with a modal up.
        let (buttons, _lx, _ly) = crate::input::pad_snapshot();
        let done_held = buttons & DONE_CHORD == DONE_CHORD;
        let skip_held = buttons & SKIP_CHORD == SKIP_CHORD;

        // Choice-modal input the overlay pushed back (menu nav up/down/confirm + the keyboard note
        // buffer). A quiet frame on a normal step; only a choice step reads it.
        let (choice_up, choice_down, choice_confirm, note) = crate::rig_guide::drain_modal_input();

        let result = self.runner.tick(&GuideInput {
            delta: tick.delta,
            state: &state,
            new_log_lines: &lines,
            done_held,
            skip_held,
            choice: ChoiceInput {
                up: choice_up,
                down: choice_down,
                confirm: choice_confirm,
                note: &note,
            },
        });

        // A resolved choice is captured/shareable like every other guide signal: log it. Plain voice
        // (a debug tool), and the note is quoted so a multi-word annotation reads cleanly. This is the
        // one datum logging alone can't reach — the tester's judgement, turned into a logged line.
        if let Some(made) = &result.choice_made {
            if made.note.is_empty() {
                log::info!("rig-guide: '{}' -> '{}'", made.step_id, made.label);
            } else {
                log::info!("rig-guide: '{}' -> '{}' note = \"{}\"", made.step_id, made.label, made.note);
            }
        }

        // Tee log lines into the guide queue only while a step is actually showing (a pinned banner OR
        // a choice modal): on once the first tick produces one, off the moment the guide finishes or is
        // idle. This keeps pre-guide boot lines out of step 1 and never leaves the tee running for a
        // guide that started already-finished (every step tagged for another role).
        let showing = result.banner.is_some() || result.choice.is_some();
        crate::guide_log::set_enabled(showing);

        // Copy the scalar outputs out before moving the banner/choice into the published view.
        let color = result.color;
        let finished_now = result.finished_now;
        let view = match (result.choice, result.banner) {
            (Some(choice), _) => Some(RigView::Choice(choice)),
            (None, Some(text)) => Some(RigView::Banner(RigBanner { text, color })),
            (None, None) => None,
        };
        crate::rig_guide::publish(view);

        if finished_now {
            log::info!("rig-guide: guide complete");
            // Plain/diagnostic voice (this is a debug tool, not gameplay); ASCII only (the overlay
            // font is an ASCII subset), per CLAUDE.md.
            crate::notify::toast(Severity::Info, "Rig guide complete. Testing done.");
        }
    }
}
