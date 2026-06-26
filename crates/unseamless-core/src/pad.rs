//! Pure controller→menu translation: turn a frame's raw XInput pad sample into edge-triggered menu
//! intents ([`PadEdges`]). Host-tested. The OS-coupled half (hooking `XInputGetState`, the atomic
//! snapshot, reading the Guide bit) lives in the coop crate's `input` module, which samples the pad and
//! hands the unpacked `(buttons, lx, ly, dt)` here.
//!
//! Directions auto-repeat while held (an initial delay, then a fixed interval — keyboard-repeat feel);
//! the activate (A), cancel (B), and toggle (the RB+L3+R3 chord) intents fire once per press.
//! Keeping the repeat/edge/threshold logic here (not in the cdylib) makes it unit-testable on the
//! host, per the project's core-vs-coop split.

/// XINPUT_GAMEPAD `wButtons` bits we read (a subset of the standard mask).
pub const XINPUT_DPAD_UP: u16 = 0x0001;
pub const XINPUT_DPAD_DOWN: u16 = 0x0002;
pub const XINPUT_DPAD_LEFT: u16 = 0x0004;
pub const XINPUT_DPAD_RIGHT: u16 = 0x0008;
pub const XINPUT_LEFT_THUMB: u16 = 0x0040; // left stick click (L3)
pub const XINPUT_RIGHT_THUMB: u16 = 0x0080; // right stick click (R3)
pub const XINPUT_LEFT_SHOULDER: u16 = 0x0100; // LB
pub const XINPUT_RIGHT_SHOULDER: u16 = 0x0200; // RB
pub const XINPUT_A: u16 = 0x1000; // confirm / activate
pub const XINPUT_B: u16 = 0x2000; // cancel / close

/// The overlay-toggle chord: RB + L3 + R3 held together. Deliberately awkward so it's never hit by
/// accident, and — unlike the Guide/Home button — made of standard bits the plain `XInputGetState`
/// reports, so it survives Steam Input (which intercepts Guide for most players). **LB is deliberately
/// excluded**: in Elden Ring LB fires the *left-hand* armament — with a catalyst equipped that casts a
/// spell, so folding LB into the chord would burn FP/mana every time you open the menu. The remaining
/// three don't touch FP: RB is a right-hand attack (a wasted swing at worst, no resource cost) and
/// L3/R3 are the stick-clicks (crouch / reset-camera), so tapping them together to open is harmless.
const TOGGLE_COMBO: u16 = XINPUT_RIGHT_SHOULDER | XINPUT_LEFT_THUMB | XINPUT_RIGHT_THUMB;

/// Per-direction auto-repeat tuning (seconds): the initial delay before a held direction starts
/// repeating, then one step per interval.
const REPEAT_DELAY: f32 = 0.35;
const REPEAT_INTERVAL: f32 = 0.12;
/// Left-stick deflection past which an axis counts as a d-pad press. Well above XInput's own resting
/// deadzone (~7800) so a centred stick never nudges the menu.
const STICK_THRESHOLD: i16 = 16000;

// Direction indices into the per-direction arrays, named so the four parallel arrays and the
// `PadEdges` build below can't silently drift if one is reordered.
const UP: usize = 0;
const DOWN: usize = 1;
const LEFT: usize = 2;
const RIGHT: usize = 3;

/// One frame's edge-triggered controller intents for the overlay menu. Directions auto-repeat while
/// held; `activate`/`cancel`/`toggle` fire once per physical press.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct PadEdges {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    /// A — confirm the selected action.
    pub activate: bool,
    /// B — close the overlay (a Back/Cancel; only acted on while it's open).
    pub cancel: bool,
    /// The RB+L3+R3 chord — open/close the overlay.
    pub toggle: bool,
}

/// Edge/repeat state for one input source. One instance lives on the overlay (Present thread);
/// [`update`](PadNav::update) is called once per frame with that frame's raw pad sample.
pub struct PadNav {
    prev_buttons: u16,
    held: [bool; 4],
    timer: [f32; 4],
}

impl Default for PadNav {
    fn default() -> Self {
        Self::new()
    }
}

impl PadNav {
    pub const fn new() -> Self {
        Self { prev_buttons: 0, held: [false; 4], timer: [0.0; 4] }
    }

    /// Compute this frame's menu edges from the raw pad sample. `buttons` is the XInput button mask
    /// (incl. the Guide bit), `lx`/`ly` the left stick, `dt` the frame delta (seconds) driving repeat.
    /// A neutral sample (`0, 0, 0`) releases everything — the binding passes that on controller
    /// disconnect, so a direction held at disconnect can't auto-repeat forever.
    pub fn update(&mut self, buttons: u16, lx: i16, ly: i16, dt: f32) -> PadEdges {
        // XInput thumb-Y is positive-up, matching "up = previous item". A direction is active on either
        // the d-pad bit or the stick past the threshold.
        let mut dirs = [false; 4];
        dirs[UP] = buttons & XINPUT_DPAD_UP != 0 || ly > STICK_THRESHOLD;
        dirs[DOWN] = buttons & XINPUT_DPAD_DOWN != 0 || ly < -STICK_THRESHOLD;
        dirs[LEFT] = buttons & XINPUT_DPAD_LEFT != 0 || lx < -STICK_THRESHOLD;
        dirs[RIGHT] = buttons & XINPUT_DPAD_RIGHT != 0 || lx > STICK_THRESHOLD;

        let mut fire = [false; 4];
        for i in 0..4 {
            if dirs[i] {
                if !self.held[i] {
                    fire[i] = true; // initial press
                    self.timer[i] = REPEAT_DELAY;
                } else {
                    self.timer[i] -= dt;
                    if self.timer[i] <= 0.0 {
                        fire[i] = true; // repeat tick
                        self.timer[i] = REPEAT_INTERVAL;
                    }
                }
            }
            self.held[i] = dirs[i];
        }

        // A / B / the toggle chord: rising edge only (one action per press), like the keyboard's
        // no-repeat keys. Capture the previous mask before overwriting it so the edge tests read last
        // frame's state.
        let prev = self.prev_buttons;
        self.prev_buttons = buttons;
        let pressed = |bit: u16| buttons & bit != 0 && prev & bit == 0;
        // The chord toggles once, on the frame its full set first becomes held (not while it's holding).
        let combo_held = |b: u16| b & TOGGLE_COMBO == TOGGLE_COMBO;
        PadEdges {
            up: fire[UP],
            down: fire[DOWN],
            left: fire[LEFT],
            right: fire[RIGHT],
            activate: pressed(XINPUT_A),
            cancel: pressed(XINPUT_B),
            toggle: combo_held(buttons) && !combo_held(prev),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_DT: f32 = 1.0 / 60.0;

    #[test]
    fn neutral_sample_fires_nothing() {
        let mut nav = PadNav::new();
        assert_eq!(nav.update(0, 0, 0, FRAME_DT), PadEdges::default());
    }

    #[test]
    fn dpad_up_fires_once_then_waits_for_the_repeat_delay() {
        let mut nav = PadNav::new();
        assert!(nav.update(XINPUT_DPAD_UP, 0, 0, FRAME_DT).up); // initial press
        assert!(!nav.update(XINPUT_DPAD_UP, 0, 0, 0.1).up); // held, before the delay elapses
        assert!(nav.update(XINPUT_DPAD_UP, 0, 0, REPEAT_DELAY).up); // delay crossed → repeat fires
    }

    #[test]
    fn releasing_then_pressing_fires_a_fresh_edge() {
        let mut nav = PadNav::new();
        assert!(nav.update(XINPUT_DPAD_DOWN, 0, 0, FRAME_DT).down);
        assert!(!nav.update(0, 0, 0, FRAME_DT).down); // released
        assert!(nav.update(XINPUT_DPAD_DOWN, 0, 0, FRAME_DT).down); // re-press
    }

    #[test]
    fn stick_past_threshold_navigates_with_the_right_sign() {
        // Up is positive Y (matching "up = previous"); down negative; left negative X; right positive X.
        assert!(PadNav::new().update(0, 0, STICK_THRESHOLD + 1, FRAME_DT).up);
        assert!(PadNav::new().update(0, 0, -(STICK_THRESHOLD) - 1, FRAME_DT).down);
        assert!(PadNav::new().update(0, -(STICK_THRESHOLD) - 1, 0, FRAME_DT).left);
        assert!(PadNav::new().update(0, STICK_THRESHOLD + 1, 0, FRAME_DT).right);
    }

    #[test]
    fn stick_inside_the_deadzone_does_nothing() {
        let mut nav = PadNav::new();
        let e = nav.update(0, STICK_THRESHOLD - 1, STICK_THRESHOLD - 1, FRAME_DT);
        assert_eq!(e, PadEdges::default());
    }

    #[test]
    fn activate_and_cancel_are_one_shot_per_press() {
        let mut nav = PadNav::new();
        assert!(nav.update(XINPUT_A, 0, 0, FRAME_DT).activate);
        assert!(!nav.update(XINPUT_A, 0, 0, FRAME_DT).activate); // held: never auto-repeats
        assert!(!nav.update(0, 0, 0, FRAME_DT).activate); // released
        assert!(nav.update(XINPUT_A, 0, 0, FRAME_DT).activate); // re-press

        let mut nav = PadNav::new();
        assert!(nav.update(XINPUT_B, 0, 0, FRAME_DT).cancel);
        assert!(!nav.update(XINPUT_B, 0, 0, FRAME_DT).cancel); // held: one-shot
    }

    #[test]
    fn toggle_chord_fires_once_when_the_set_completes() {
        let full = XINPUT_RIGHT_SHOULDER | XINPUT_LEFT_THUMB | XINPUT_RIGHT_THUMB;
        let mut nav = PadNav::new();
        // A partial chord (and any single member) never toggles.
        assert!(!nav.update(XINPUT_RIGHT_SHOULDER | XINPUT_LEFT_THUMB, 0, 0, FRAME_DT).toggle);
        assert!(!nav.update(full & !XINPUT_RIGHT_THUMB, 0, 0, FRAME_DT).toggle);
        // Completing the set fires exactly once; holding it does not re-fire.
        assert!(nav.update(full, 0, 0, FRAME_DT).toggle);
        assert!(!nav.update(full, 0, 0, FRAME_DT).toggle);
        // Releasing a member and re-completing fires again.
        assert!(!nav.update(full & !XINPUT_LEFT_THUMB, 0, 0, FRAME_DT).toggle);
        assert!(nav.update(full, 0, 0, FRAME_DT).toggle);
    }

    #[test]
    fn toggle_chord_excludes_lb_so_it_doesnt_fire_a_spell() {
        // LB is intentionally not part of the chord (it casts a spell / wastes mana in-game). The chord
        // is exactly RB+L3+R3, so it completes without LB ever being pressed.
        assert_eq!(TOGGLE_COMBO, XINPUT_RIGHT_SHOULDER | XINPUT_LEFT_THUMB | XINPUT_RIGHT_THUMB);
        assert_eq!(TOGGLE_COMBO & XINPUT_LEFT_SHOULDER, 0, "LB must not be required by the toggle chord");

        // RB+L3+R3 with no LB toggles on completion.
        let chord = XINPUT_RIGHT_SHOULDER | XINPUT_LEFT_THUMB | XINPUT_RIGHT_THUMB;
        assert!(PadNav::new().update(chord, 0, 0, FRAME_DT).toggle);
        // An incidental LB held alongside is ignored (extra bits don't gate the chord) — still one toggle.
        assert!(PadNav::new().update(chord | XINPUT_LEFT_SHOULDER, 0, 0, FRAME_DT).toggle);
    }

    #[test]
    fn dpad_drives_the_same_direction_as_the_stick() {
        assert!(PadNav::new().update(XINPUT_DPAD_RIGHT, 0, 0, FRAME_DT).right);
    }
}
