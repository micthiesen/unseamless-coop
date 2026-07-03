//! The session **toggle state** model — the host-authoritative bits behind the menu's collapsed
//! toggle rows (`world_locked`, `pvp_on`, `pvp_teams_on`, `friendly_fire_on`).
//!
//! Pure + host-tested, built ahead of the rung-3 game-side wiring (see COOP-CONNECTION.md > "The
//! pre-built session core"). The rest of the codebase treats these four bits as always-`false`
//! placeholders today ([`crate::menu::SessionContext`]); this module is the proper source: a tiny
//! state machine with **explicit transitions** driven by the [`SessionAction`] verbs, owned by
//! [`crate::peer::Peer`] so authority and distribution follow the side-channel's existing rules:
//!
//! - **The host is the authority.** Only the host's accepted actions mutate its state (it applies
//!   its own action at send time, [`crate::peer::Peer::session_action`]).
//! - **Joiners hold a replica that follows only host-confirmed transitions**: an inbound toggle
//!   action mutates the replica only if it arrived from the linked host and passed the sequence
//!   gate ([`crate::peer::Peer::handle`]) — a stranger or a fellow joiner can request all it wants,
//!   the state never moves. This is the same authorize-by-sender rule `is_host_only` already
//!   enforces, extended to the state the action produces.
//!
//! Each realized transition carries an ER-voiced toast ([`ToggleChange::message`]): a toggle is an
//! in-world *effect*, so per CLAUDE.md's message-voice rule the wording is lore-register and
//! value-free (the plain mechanical state still shows on the host's menu rows).

use crate::menu::SessionContext;
use crate::protocol::SessionAction;

/// The four session-wide toggle bits the host governs. `Default` is all-off — the state of a fresh
/// session (a session teardown resets state by dropping the owning `Peer`, so there's no explicit
/// reset transition to mis-order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionToggles {
    /// The world is locked against newcomers ([`SessionAction::LockWorld`]/`UnlockWorld`).
    pub world_locked: bool,
    /// PvP between cooperators is enabled ([`SessionAction::TogglePvp`]).
    pub pvp_on: bool,
    /// PvP teams are enabled ([`SessionAction::TogglePvpTeams`]).
    pub pvp_teams_on: bool,
    /// Friendly fire is enabled ([`SessionAction::ToggleFriendlyFire`]).
    pub friendly_fire_on: bool,
}

/// A realized toggle transition — which edge actually happened. Returned by
/// [`SessionToggles::apply`] so the caller can toast the matching message exactly once per real
/// change (a no-op apply returns `None` and stays silent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleChange {
    WorldLocked,
    WorldUnlocked,
    PvpOn,
    PvpOff,
    PvpTeamsOn,
    PvpTeamsOff,
    FriendlyFireOn,
    FriendlyFireOff,
}

impl ToggleChange {
    /// The ER-voiced toast for this transition. Lore register, identity- and value-free (like the
    /// presence messages in [`crate::peer`]): a toggle is an in-world effect, so it never shows raw
    /// mechanical values — the host's menu rows carry the literal on/off state.
    pub fn message(self) -> &'static str {
        match self {
            ToggleChange::WorldLocked => "The world is sealed against newcomers.",
            ToggleChange::WorldUnlocked => "The world lies open once more.",
            ToggleChange::PvpOn => "Cooperators may now cross blades.",
            ToggleChange::PvpOff => "Cooperators' blades are stayed.",
            ToggleChange::PvpTeamsOn => "Battle lines are drawn among cooperators.",
            ToggleChange::PvpTeamsOff => "The battle lines are dissolved.",
            ToggleChange::FriendlyFireOn => "Allied blows now draw blood.",
            ToggleChange::FriendlyFireOff => "Allied blows no longer draw blood.",
        }
    }
}

impl SessionToggles {
    /// Apply one session action's state transition. Returns the transition that actually happened,
    /// or `None` when the action doesn't govern toggle state (the lobby verbs Open/Join/Leave) or
    /// was a no-op (`LockWorld` on an already-locked world) — so a re-delivered absolute verb never
    /// double-toasts. The `Toggle*` verbs are relative (they always flip), which is why ordering
    /// and exactly-once delivery matter: the caller must hand this only actions that passed the
    /// per-sender sequence gate (see [`crate::peer::Peer::handle`]).
    pub fn apply(&mut self, action: SessionAction) -> Option<ToggleChange> {
        use SessionAction::*;
        match action {
            LockWorld if !self.world_locked => {
                self.world_locked = true;
                Some(ToggleChange::WorldLocked)
            }
            UnlockWorld if self.world_locked => {
                self.world_locked = false;
                Some(ToggleChange::WorldUnlocked)
            }
            TogglePvp => {
                self.pvp_on = !self.pvp_on;
                Some(if self.pvp_on { ToggleChange::PvpOn } else { ToggleChange::PvpOff })
            }
            TogglePvpTeams => {
                self.pvp_teams_on = !self.pvp_teams_on;
                Some(if self.pvp_teams_on {
                    ToggleChange::PvpTeamsOn
                } else {
                    ToggleChange::PvpTeamsOff
                })
            }
            ToggleFriendlyFire => {
                self.friendly_fire_on = !self.friendly_fire_on;
                Some(if self.friendly_fire_on {
                    ToggleChange::FriendlyFireOn
                } else {
                    ToggleChange::FriendlyFireOff
                })
            }
            // Absolute verbs that already hold, and the lobby verbs (Open/Join/Leave): no state
            // transition here. Leave tears down the whole `Peer` (binding-owned), which is the reset.
            LockWorld | UnlockWorld | OpenWorld | JoinWorld | LeaveWorld => None,
        }
    }

    /// Project this state onto the menu's [`SessionContext`] toggle bits — the seam that replaces
    /// the overlay's always-`false` placeholders (`coop/overlay.rs` `session_context`). Writes only
    /// the four bits this model owns; the session/readiness flags stay the caller's.
    pub fn write_context(&self, ctx: &mut SessionContext) {
        ctx.world_locked = self.world_locked;
        ctx.pvp_on = self.pvp_on;
        ctx.pvp_teams_on = self.pvp_teams_on;
        ctx.friendly_fire_on = self.friendly_fire_on;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::action_rows;

    #[test]
    fn lock_and_unlock_are_absolute_and_idempotent() {
        let mut t = SessionToggles::default();
        assert_eq!(t.apply(SessionAction::LockWorld), Some(ToggleChange::WorldLocked));
        assert!(t.world_locked);
        // Re-applying the held state is a silent no-op — a duplicate-delivered absolute verb (or a
        // re-assert) must not double-toast.
        assert_eq!(t.apply(SessionAction::LockWorld), None);
        assert!(t.world_locked);
        assert_eq!(t.apply(SessionAction::UnlockWorld), Some(ToggleChange::WorldUnlocked));
        assert!(!t.world_locked);
        assert_eq!(t.apply(SessionAction::UnlockWorld), None);
    }

    #[test]
    fn toggles_flip_each_time_with_matching_edges() {
        let mut t = SessionToggles::default();
        for (action, on, off) in [
            (SessionAction::TogglePvp, ToggleChange::PvpOn, ToggleChange::PvpOff),
            (SessionAction::TogglePvpTeams, ToggleChange::PvpTeamsOn, ToggleChange::PvpTeamsOff),
            (
                SessionAction::ToggleFriendlyFire,
                ToggleChange::FriendlyFireOn,
                ToggleChange::FriendlyFireOff,
            ),
        ] {
            assert_eq!(t.apply(action), Some(on), "{action:?} first flip turns on");
            assert_eq!(t.apply(action), Some(off), "{action:?} second flip turns off");
        }
        assert_eq!(t, SessionToggles::default(), "each pair of flips returns to default");
    }

    #[test]
    fn each_toggle_flips_only_its_own_bit() {
        // Flipping one toggle must not perturb the other three (catches a copy-paste transition
        // writing the wrong field).
        let cases: [(SessionAction, fn(&SessionToggles) -> bool); 4] = [
            (SessionAction::LockWorld, |t| t.world_locked),
            (SessionAction::TogglePvp, |t| t.pvp_on),
            (SessionAction::TogglePvpTeams, |t| t.pvp_teams_on),
            (SessionAction::ToggleFriendlyFire, |t| t.friendly_fire_on),
        ];
        for (action, read) in cases {
            let mut t = SessionToggles::default();
            t.apply(action);
            assert!(read(&t), "{action:?} sets its own bit");
            let mut expected = SessionToggles::default();
            match action {
                SessionAction::LockWorld => expected.world_locked = true,
                SessionAction::TogglePvp => expected.pvp_on = true,
                SessionAction::TogglePvpTeams => expected.pvp_teams_on = true,
                SessionAction::ToggleFriendlyFire => expected.friendly_fire_on = true,
                _ => unreachable!(),
            }
            assert_eq!(t, expected, "{action:?} touches only its own bit");
        }
    }

    #[test]
    fn lobby_verbs_do_not_govern_toggle_state() {
        let mut t = SessionToggles::default();
        t.apply(SessionAction::TogglePvp);
        let before = t;
        for action in [SessionAction::OpenWorld, SessionAction::JoinWorld, SessionAction::LeaveWorld]
        {
            assert_eq!(t.apply(action), None, "{action:?} is not a toggle transition");
            assert_eq!(t, before, "{action:?} must not perturb toggle state");
        }
    }

    #[test]
    fn toggle_messages_are_er_voiced_value_free_and_distinct() {
        // Same contract the presence messages pin: a toggle is an in-world effect, so its toast is
        // lore-voiced — non-empty, no raw mechanical values (no digits), and each edge reads
        // differently (a shared on/off line would make the pair ambiguous in the toast history).
        let all = [
            ToggleChange::WorldLocked,
            ToggleChange::WorldUnlocked,
            ToggleChange::PvpOn,
            ToggleChange::PvpOff,
            ToggleChange::PvpTeamsOn,
            ToggleChange::PvpTeamsOff,
            ToggleChange::FriendlyFireOn,
            ToggleChange::FriendlyFireOff,
        ];
        for change in all {
            let msg = change.message();
            assert!(!msg.is_empty(), "{change:?} must say something");
            assert!(
                !msg.chars().any(|c| c.is_ascii_digit()),
                "lore voice carries no raw values: {msg:?}"
            );
        }
        let unique: std::collections::BTreeSet<_> = all.iter().map(|c| c.message()).collect();
        assert_eq!(unique.len(), all.len(), "every transition message must be distinct");
    }

    #[test]
    fn write_context_feeds_the_menus_collapsed_toggle_rows() {
        // End to end into the menu surface: a populated state projected onto the SessionContext
        // must flip the host's collapsed rows (label AND emitted action) — the exact placeholder
        // this model replaces.
        let mut t = SessionToggles::default();
        t.apply(SessionAction::LockWorld);
        t.apply(SessionAction::TogglePvp);

        // Start the context with a STALE `true` on a bit the state holds `false`: write_context
        // must overwrite (plain assignment), not OR — a reused context would otherwise latch on.
        let mut ctx = SessionContext {
            in_session: true,
            is_host: true,
            steam_ready: true,
            in_game: true,
            pvp_teams_on: true, // stale; state says false → must be cleared
            ..Default::default()
        };
        t.write_context(&mut ctx);
        assert!(ctx.world_locked && ctx.pvp_on && !ctx.pvp_teams_on && !ctx.friendly_fire_on);

        let rows: Vec<(String, SessionAction)> =
            action_rows(&ctx).into_iter().map(|r| (r.label, r.action)).collect();
        assert!(
            rows.contains(&("Unlock world".into(), SessionAction::UnlockWorld)),
            "a locked world offers Unlock: {rows:?}"
        );
        assert!(
            rows.contains(&("PvP: on".into(), SessionAction::TogglePvp)),
            "the PvP row shows the live state: {rows:?}"
        );

        // The projection writes only the four toggle bits; the session/readiness flags survive.
        assert!(ctx.in_session && ctx.is_host && ctx.steam_ready && ctx.in_game);
    }
}
