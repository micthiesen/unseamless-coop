//! Coarse, reliable game-lifecycle state — a small enum the rest of the mod gates on (overlay install
//! timing, "is the player actually in the game" checks) instead of each feature poking singletons ad
//! hoc.
//!
//! Pure + host-tested. The binding (coop `playstate`) samples the game's singletons each frame into
//! [`GameSignals`], asks [`GameState::classify`] for the state, and publishes it; readers on other
//! threads pull it non-blocking. Keeping the decision here makes the state machine unit-testable on the
//! host and keeps the binding a thin "read these pointers, hand them over" layer.
//!
//! Step 1 is deliberately coarse — three states we can read *reliably* today. Finer states
//! (character-creation vs title, an explicit `Loading`, paused-in-a-menu) are a later refinement as
//! features need them and the RE lands: add a variant, a [`GameSignals`] field, and a `classify` arm.

/// Where the game is in its lifecycle, at the coarsest reliable granularity.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum GameState {
    /// Earliest boot: the menu/UI system isn't up yet, so there's no stable swapchain or frontend.
    #[default]
    Booting = 0,
    /// The "frontend": title screen, main menu, character creation, and the pre-spawn loading before a
    /// character exists in the world. The UI system (and its swapchain) is up, but no player is loaded.
    Frontend = 1,
    /// A character is loaded in the world — active play, or an in-game pause/inventory/map screen.
    InGame = 2,
}

/// Raw presence signals the binding samples from the game each frame. Booleans only, so the decision
/// stays pure and host-testable (the binding does the `unsafe` singleton reads and fills this in).
#[derive(Clone, Copy, Debug, Default)]
pub struct GameSignals {
    /// The menu/UI manager (`CSMenuMan`) singleton is live — the frontend has come up.
    pub menu_system_up: bool,
    /// A main player exists in the world (`WorldChrMan.main_player.is_some()`).
    pub player_in_world: bool,
}

impl GameState {
    /// Classify the lifecycle state from this frame's raw signals. A loaded player wins (we're in the
    /// game even with a pause/inventory menu open); else the UI manager being up means we've reached the
    /// frontend; else we're still booting.
    pub fn classify(s: GameSignals) -> Self {
        if s.player_in_world {
            GameState::InGame
        } else if s.menu_system_up {
            GameState::Frontend
        } else {
            GameState::Booting
        }
    }

    /// A character is loaded in the world — the reliable "actually in the game" gate (e.g. don't host/
    /// join a session, don't apply a world effect, from the title or a loading screen).
    pub fn in_game(self) -> bool {
        matches!(self, GameState::InGame)
    }

    /// The frontend (and its swapchain) is up — past earliest boot. The overlay installs from here so
    /// hudhook's DX12 backend initializes against a live swapchain rather than the half-ready boot one.
    pub fn frontend_ready(self) -> bool {
        matches!(self, GameState::Frontend | GameState::InGame)
    }

    /// Stable `u8` tag for publishing through an atomic (the binding stores the state in an `AtomicU8`).
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Inverse of [`as_u8`](GameState::as_u8); an unknown tag falls back to [`GameState::Booting`] (the
    /// safe default — "nothing's up yet"), so a torn/garbage read never fabricates an in-game state.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => GameState::Frontend,
            2 => GameState::InGame,
            _ => GameState::Booting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(menu_system_up: bool, player_in_world: bool) -> GameSignals {
        GameSignals { menu_system_up, player_in_world }
    }

    #[test]
    fn classifies_the_three_states() {
        assert_eq!(GameState::classify(sig(false, false)), GameState::Booting);
        assert_eq!(GameState::classify(sig(true, false)), GameState::Frontend);
        assert_eq!(GameState::classify(sig(true, true)), GameState::InGame);
    }

    #[test]
    fn a_loaded_player_wins_even_without_the_menu_flag() {
        // In-game with an open pause menu still reads as InGame: the player presence is the strong
        // signal, so a momentarily-unobserved menu manager never demotes us off the playfield.
        assert_eq!(GameState::classify(sig(false, true)), GameState::InGame);
    }

    #[test]
    fn in_game_only_when_a_player_is_loaded() {
        assert!(!GameState::Booting.in_game());
        assert!(!GameState::Frontend.in_game());
        assert!(GameState::InGame.in_game());
    }

    #[test]
    fn frontend_ready_covers_frontend_and_in_game_but_not_booting() {
        assert!(!GameState::Booting.frontend_ready());
        assert!(GameState::Frontend.frontend_ready());
        assert!(GameState::InGame.frontend_ready());
    }

    #[test]
    fn u8_round_trips_and_unknown_is_booting() {
        for s in [GameState::Booting, GameState::Frontend, GameState::InGame] {
            assert_eq!(GameState::from_u8(s.as_u8()), s);
        }
        // Garbage/torn tag never fabricates an in-game state.
        assert_eq!(GameState::from_u8(3), GameState::Booting);
        assert_eq!(GameState::from_u8(255), GameState::Booting);
    }

    #[test]
    fn default_is_booting() {
        assert_eq!(GameState::default(), GameState::Booting);
    }
}
