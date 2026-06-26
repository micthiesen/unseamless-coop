//! Pure controller→menu translation: turn a frame's raw XInput pad sample into edge-triggered menu
//! intents ([`PadEdges`]). Host-tested. The OS-coupled half (hooking `XInputGetState`, the atomic
//! snapshot, reading the Guide bit) lives in the coop crate's `input` module, which samples the pad and
//! hands the unpacked `(buttons, lx, ly, dt)` here.
//!
//! Directions auto-repeat while held (an initial delay, then a fixed interval — keyboard-repeat feel);
//! the activate/toggle buttons fire once per physical press. Keeping the repeat/edge/threshold logic
//! here (not in the cdylib) makes it unit-testable on the host, per the project's core-vs-coop split.

/// XINPUT_GAMEPAD `wButtons` bits we read (a subset of the standard mask).
pub const XINPUT_DPAD_UP: u16 = 0x0001;
pub const XINPUT_DPAD_DOWN: u16 = 0x0002;
pub const XINPUT_DPAD_LEFT: u16 = 0x0004;
pub const XINPUT_DPAD_RIGHT: u16 = 0x0008;
/// Guide / "Home" button. Undocumented: the plain `XInputGetState` masks it out; only
/// `XInputGetStateEx` reports it — which is exactly why it's a safe overlay toggle (the game, calling
/// the plain API, is structurally blind to it).
pub const XINPUT_GUIDE: u16 = 0x0400;
pub const XINPUT_A: u16 = 0x1000;

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
/// held; `activate`/`toggle` fire once per physical press.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct PadEdges {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub activate: bool,
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

        // A / Guide: rising edge only (one action per press), like the keyboard's no-repeat keys.
        // Capture the previous mask before overwriting it so the edge test reads last frame's state.
        let prev = self.prev_buttons;
        self.prev_buttons = buttons;
        let pressed = |bit: u16| buttons & bit != 0 && prev & bit == 0;
        PadEdges {
            up: fire[UP],
            down: fire[DOWN],
            left: fire[LEFT],
            right: fire[RIGHT],
            activate: pressed(XINPUT_A),
            toggle: pressed(XINPUT_GUIDE),
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
    fn activate_and_toggle_are_one_shot_per_press() {
        let mut nav = PadNav::new();
        assert!(nav.update(XINPUT_A, 0, 0, FRAME_DT).activate);
        assert!(!nav.update(XINPUT_A, 0, 0, FRAME_DT).activate); // held: never auto-repeats
        assert!(!nav.update(0, 0, 0, FRAME_DT).activate); // released
        assert!(nav.update(XINPUT_A, 0, 0, FRAME_DT).activate); // re-press

        let mut nav = PadNav::new();
        assert!(nav.update(XINPUT_GUIDE, 0, 0, FRAME_DT).toggle);
        assert!(!nav.update(XINPUT_GUIDE, 0, 0, FRAME_DT).toggle);
    }

    #[test]
    fn dpad_drives_the_same_direction_as_the_stick() {
        assert!(PadNav::new().update(XINPUT_DPAD_RIGHT, 0, 0, FRAME_DT).right);
    }
}
