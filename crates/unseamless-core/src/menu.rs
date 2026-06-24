//! The in-game menu model: a flat list of rows (session actions + tunable settings), a cursor,
//! and the navigation/edit logic. **Pure** — it owns no rendering and no game state, so it's
//! fully host-tested here. The cdylib's overlay just draws [`Menu::rows`] and forwards key
//! presses to [`Menu::select_next`]/`select_prev`/`activate`/`adjust`.
//!
//! ## Divergence from ERSC (intentional)
//! ERSC drives session actions (host/join/leave/…) through **in-game items** and fixed hotkeys.
//! We drive them through this menu instead (rendered as an overlay; see
//! `docs/ARCHITECTURE.md` > Divergences). Settings come straight from [`crate::settings`], so the
//! same registry powers the config file and the menu.

use crate::config::Config;
use crate::protocol::SessionAction;
use crate::settings::{Setting, SettingId, registry};

/// What's true about the current session, used to enable/disable action rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionContext {
    pub in_session: bool,
    pub is_host: bool,
}

/// One row in the menu.
pub enum MenuItem {
    /// A session verb (host/join/leave/…). `enabled` gates it on the session context.
    Action {
        action: SessionAction,
        label: &'static str,
        enabled: fn(&SessionContext) -> bool,
    },
    /// A tunable setting, addressed into [`crate::settings::registry`].
    Setting(SettingId),
}

/// A row prepared for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRow {
    pub label: String,
    /// Present for settings (the current value); `None` for action rows.
    pub value: Option<String>,
    pub enabled: bool,
    pub selected: bool,
}

/// Result of acting on the selected row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOutcome {
    /// Nothing happened (e.g. activating a disabled action, or a range on `activate`).
    None,
    /// A session action should be performed.
    Action(SessionAction),
    /// A setting's value changed; the caller should persist config / re-apply effects.
    SettingChanged(SettingId),
}

pub struct Menu {
    items: Vec<MenuItem>,
    settings: Vec<Setting>,
    selected: usize,
}

impl Default for Menu {
    fn default() -> Self {
        Self::new()
    }
}

impl Menu {
    /// Build the default layout: session actions first, then every registered setting.
    pub fn new() -> Self {
        use SessionAction::*;

        // Sensible default gating. These are first-pass rules; the rig run may refine exactly
        // when each action is legal (see RIG-RUNBOOK.md), but the shape is right.
        let not_in_session = |c: &SessionContext| !c.in_session;
        let in_session = |c: &SessionContext| c.in_session;
        let host_in_session = |c: &SessionContext| c.in_session && c.is_host;

        let action = |action, label, enabled: fn(&SessionContext) -> bool| MenuItem::Action {
            action,
            label,
            enabled,
        };

        let mut items = vec![
            action(OpenWorld, "Host / open world", not_in_session),
            action(JoinWorld, "Join world", not_in_session),
            action(BreakInWorld, "Break into world", not_in_session),
            action(LeaveWorld, "Leave world", in_session),
            action(LockWorld, "Lock world", host_in_session),
            action(UnlockWorld, "Unlock world", host_in_session),
            action(TogglePvp, "Toggle PvP", host_in_session),
            action(TogglePvpTeams, "Toggle PvP teams", host_in_session),
            action(ToggleFriendlyFire, "Toggle friendly fire", host_in_session),
            action(ToggleDriedFinger, "Toggle dried finger", host_in_session),
            action(GiveEmber, "Give ember", in_session),
        ];
        items.extend(registry().iter().map(|s| MenuItem::Setting(s.id)));

        Menu { items, settings: registry(), selected: 0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    fn setting(&self, id: SettingId) -> &Setting {
        self.settings
            .iter()
            .find(|s| s.id == id)
            .expect("menu references a setting that exists in the registry")
    }

    /// Is the row at `index` selectable in this context? Settings always are; actions depend on
    /// their `enabled` predicate.
    fn is_enabled(&self, index: usize, ctx: &SessionContext) -> bool {
        match &self.items[index] {
            MenuItem::Action { enabled, .. } => enabled(ctx),
            MenuItem::Setting(_) => true,
        }
    }

    /// Move the cursor by `delta` rows (usually ±1), skipping disabled rows and wrapping. No-op
    /// if every row is disabled.
    pub fn move_selection(&mut self, delta: isize, ctx: &SessionContext) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let step = if delta >= 0 { 1 } else { n - 1 }; // wrap-safe forward/backward by 1
        let mut idx = self.selected;
        for _ in 0..n {
            idx = (idx + step) % n;
            if self.is_enabled(idx, ctx) {
                self.selected = idx;
                return;
            }
        }
        // all disabled: leave cursor where it was
    }

    pub fn select_next(&mut self, ctx: &SessionContext) {
        self.move_selection(1, ctx);
    }

    pub fn select_prev(&mut self, ctx: &SessionContext) {
        self.move_selection(-1, ctx);
    }

    /// Activate the selected row: fire an enabled action, or toggle/cycle a setting. Ranges do
    /// nothing on activate (use [`adjust`](Menu::adjust) with left/right).
    pub fn activate(&mut self, cfg: &mut Config, ctx: &SessionContext) -> MenuOutcome {
        match &self.items[self.selected] {
            MenuItem::Action { action, enabled, .. } => {
                if enabled(ctx) {
                    MenuOutcome::Action(*action)
                } else {
                    MenuOutcome::None
                }
            }
            MenuItem::Setting(id) => {
                let id = *id;
                let setting = self.setting(id);
                match &setting.kind {
                    crate::settings::SettingKind::Range { .. } => MenuOutcome::None,
                    _ => {
                        setting.adjust(cfg, true);
                        MenuOutcome::SettingChanged(id)
                    }
                }
            }
        }
    }

    /// Adjust the selected setting left/right (`forward` = right/increase). No-op on action rows.
    pub fn adjust(&mut self, cfg: &mut Config, forward: bool) -> MenuOutcome {
        match &self.items[self.selected] {
            MenuItem::Setting(id) => {
                let id = *id;
                self.setting(id).adjust(cfg, forward);
                MenuOutcome::SettingChanged(id)
            }
            MenuItem::Action { .. } => MenuOutcome::None,
        }
    }

    /// Render all rows for display, given the current config and session context.
    pub fn rows(&self, cfg: &Config, ctx: &SessionContext) -> Vec<MenuRow> {
        self.items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                MenuItem::Action { label, enabled, .. } => MenuRow {
                    label: (*label).to_string(),
                    value: None,
                    enabled: enabled(ctx),
                    selected: i == self.selected,
                },
                MenuItem::Setting(id) => {
                    let s = self.setting(*id);
                    MenuRow {
                        label: s.label.to_string(),
                        value: Some(s.display_value(cfg)),
                        enabled: true,
                        selected: i == self.selected,
                    }
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_cover_actions_and_settings() {
        let menu = Menu::new();
        let rows = menu.rows(&Config::default(), &SessionContext::default());
        // 11 actions + 14 settings.
        assert_eq!(rows.len(), 25);
        // Action rows have no value; setting rows do.
        assert!(rows[0].value.is_none());
        assert!(rows.iter().filter(|r| r.value.is_some()).count() == 14);
    }

    #[test]
    fn navigation_skips_disabled_actions() {
        let mut menu = Menu::new();
        let ctx = SessionContext { in_session: false, is_host: false };
        // From row 0 (Host, enabled), next-enabled skips Leave/Lock/etc (need a session) until
        // the first action that's valid out of session or the first setting.
        menu.select_next(&ctx);
        let rows = menu.rows(&Config::default(), &ctx);
        let sel = rows.iter().position(|r| r.selected).unwrap();
        assert!(rows[sel].enabled, "cursor must land on an enabled row");
        // "Leave world" (in_session only) must never be the selection out of session.
        assert_ne!(rows[sel].label, "Leave world");
    }

    #[test]
    fn activating_disabled_action_is_noop() {
        let mut menu = Menu::new();
        let mut cfg = Config::default();
        let out_of_session = SessionContext { in_session: false, is_host: false };

        // Point directly at "Leave world", which requires being in a session.
        menu.selected = menu
            .items
            .iter()
            .position(|i| matches!(i, MenuItem::Action { action: SessionAction::LeaveWorld, .. }))
            .unwrap();
        assert_eq!(menu.activate(&mut cfg, &out_of_session), MenuOutcome::None);

        // The enabled host action does fire.
        menu.selected = 0;
        assert_eq!(
            menu.activate(&mut cfg, &out_of_session),
            MenuOutcome::Action(SessionAction::OpenWorld),
        );
    }

    #[test]
    fn host_only_actions_gated_by_context() {
        let menu = Menu::new();
        let guest = SessionContext { in_session: true, is_host: false };
        let host = SessionContext { in_session: true, is_host: true };
        let guest_rows = menu.rows(&Config::default(), &guest);
        let host_rows = menu.rows(&Config::default(), &host);
        let lock_guest = guest_rows.iter().find(|r| r.label == "Lock world").unwrap();
        let lock_host = host_rows.iter().find(|r| r.label == "Lock world").unwrap();
        assert!(!lock_guest.enabled, "guests can't lock the world");
        assert!(lock_host.enabled, "host can lock the world");
    }

    #[test]
    fn activating_a_toggle_setting_changes_config_and_reports() {
        let mut menu = Menu::new();
        let mut cfg = Config::default();
        let ctx = SessionContext::default();
        // Walk the cursor to the first setting row (the toggles start right after the actions).
        while !matches!(menu.items[menu.selected], MenuItem::Setting(_)) {
            menu.selected += 1;
        }
        let before = cfg.gameplay.allow_invaders;
        let outcome = menu.activate(&mut cfg, &ctx);
        assert!(matches!(outcome, MenuOutcome::SettingChanged(_)));
        assert_ne!(cfg.gameplay.allow_invaders, before);
    }

    #[test]
    fn adjusting_a_range_setting_steps_value() {
        let mut menu = Menu::new();
        let mut cfg = Config::default();
        // Point at the enemy-health range setting directly.
        menu.selected = menu
            .items
            .iter()
            .position(|i| matches!(i, MenuItem::Setting(SettingId::EnemyHealth)))
            .unwrap();
        cfg.scaling.enemy_health = 35;
        assert_eq!(menu.adjust(&mut cfg, true), MenuOutcome::SettingChanged(SettingId::EnemyHealth));
        assert_eq!(cfg.scaling.enemy_health, 40);
        // Activate is a no-op on ranges.
        assert_eq!(menu.activate(&mut cfg, &SessionContext::default()), MenuOutcome::None);
        assert_eq!(cfg.scaling.enemy_health, 40);
    }
}
