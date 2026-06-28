//! The in-game menu model. **Pure** — it owns no rendering and no game state, so it's fully
//! host-tested here. Two surfaces live in this module:
//!
//! - [`action_rows`] — the **dynamic Actions tab** the cdylib's overlay actually drives. It returns
//!   the context-appropriate session-action rows (paired verbs collapsed into one stateful row,
//!   inapplicable rows hidden); the overlay renders them and fires the selected row's
//!   [`ActionRow::action`]. The overlay owns its own cursor and drives skip-disabled/wrap nav through
//!   the shared [`step_enabled`]/[`first_enabled`] helpers here, so the navigation algorithm stays
//!   single-sourced and host-tested even though the overlay doesn't hold a [`Menu`].
//! - [`Menu`] — the **full static actions+settings layout** (`adjust`/setting-edit, [`Menu::rows`],
//!   [`Menu::select_index`]/[`Menu::activate`]) with its own cursor and the same shared nav helpers.
//!   Reserved for a future editable menu; not driven by the overlay today (see [`Menu::new`]).
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
    /// Steam is initialized and ready to host/join a lobby.
    pub steam_ready: bool,
    /// The player is loaded into the game world (not at the title/menu).
    pub in_game: bool,
    /// Current state of the host's collapsed toggle rows ([`action_rows`]): whether the world is
    /// locked, and whether PvP / PvP teams / friendly fire are on. These are sourced by the rung-3
    /// session FSM, which isn't tracked yet, so the overlay passes `false` for all four for now;
    /// they ride here (defaulting `false`) so the row label/action can flip once rung-3 wires them.
    pub world_locked: bool,
    pub pvp_on: bool,
    pub pvp_teams_on: bool,
    pub friendly_fire_on: bool,
}

/// One row in the menu.
pub enum MenuItem {
    /// A session verb (host/join/leave/…). `enabled` gates it on the session context; the label
    /// comes from [`SessionAction::label`] (one source of UI copy).
    Action {
        action: SessionAction,
        enabled: fn(&SessionContext) -> bool,
    },
    /// A tunable setting, addressed into [`crate::settings::registry`].
    Setting(SettingId),
}

/// The session-action rows, in display order, with their context gating. Shared by [`Menu::new`]
/// (actions + settings) and [`Menu::actions_only`].
fn action_items() -> Vec<MenuItem> {
    use SessionAction::*;

    // Sensible default gating. These are first-pass rules; the rig run may refine exactly when each
    // action is legal (see RIG-RUNBOOK.md), but the shape is right.
    //
    // Open/Join move the player from solo into a lobby, so they require Steam to be up and the
    // player to be in-game (not at the title/menu) — and only out of an existing session.
    let can_connect = |c: &SessionContext| c.steam_ready && c.in_game && !c.in_session;
    let in_session = |c: &SessionContext| c.in_session;
    let host_in_session = |c: &SessionContext| c.in_session && c.is_host;

    let action = |action, enabled: fn(&SessionContext) -> bool| MenuItem::Action { action, enabled };
    vec![
        action(OpenWorld, can_connect),
        action(JoinWorld, can_connect),
        action(LeaveWorld, in_session),
        action(LockWorld, host_in_session),
        action(UnlockWorld, host_in_session),
        action(TogglePvp, host_in_session),
        action(TogglePvpTeams, host_in_session),
        action(ToggleFriendlyFire, host_in_session),
    ]
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

/// One row of the **Actions tab**, ready to render and fire. Distinct from [`MenuRow`] (the full
/// static actions+settings layout): this is the *dynamic* session-action surface. Paired
/// positive/negative verbs collapse into a single stateful row whose `label` and `action` flip with
/// the current state (Lock⇄Unlock, the PvP toggles), and rows that don't apply to the current session
/// are omitted entirely rather than shown greyed. `enabled` then gates a *shown* row on readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRow {
    pub label: String,
    pub action: SessionAction,
    pub enabled: bool,
}

/// The Actions-tab rows for `ctx`, in display order. The UX rule is **HIDE by session state, DISABLE
/// by readiness**:
///
/// - **Out of session** → `Open world` and `Join world`, always shown (so the player sees them even at
///   the title screen) but enabled only once Steam is up *and* the player is in-game. `Leave world` is
///   not shown.
/// - **In a session** → `Leave world` (always enabled); no Open/Join.
/// - **In a session, as host** → additionally the collapsed stateful toggles: Lock⇄Unlock, PvP, PvP
///   teams, Friendly fire. A joiner in a session sees only `Leave world`; a solo player sees only
///   Open/Join.
///
/// The toggle rows' label/action flip on their `ctx` state bit (e.g. `world_locked` →
/// `Unlock world`/[`UnlockWorld`](SessionAction::UnlockWorld)); the emitted action stays one of the
/// existing [`SessionAction`] variants (PvP rows emit the single `Toggle*` flip).
pub fn action_rows(ctx: &SessionContext) -> Vec<ActionRow> {
    use SessionAction::*;
    let row = |label: String, action, enabled| ActionRow { label, action, enabled };
    let on_off = |on: bool| if on { "on" } else { "off" };

    if !ctx.in_session {
        // Shown at the title screen too, but only enabled once Steam is up and we're in-game.
        let ready = ctx.steam_ready && ctx.in_game;
        return vec![
            row("Open world".into(), OpenWorld, ready),
            row("Join world".into(), JoinWorld, ready),
        ];
    }

    let mut rows = vec![row("Leave world".into(), LeaveWorld, true)];
    if ctx.is_host {
        rows.push(if ctx.world_locked {
            row("Unlock world".into(), UnlockWorld, true)
        } else {
            row("Lock world".into(), LockWorld, true)
        });
        rows.push(row(format!("PvP: {}", on_off(ctx.pvp_on)), TogglePvp, true));
        rows.push(row(format!("PvP teams: {}", on_off(ctx.pvp_teams_on)), TogglePvpTeams, true));
        rows.push(row(format!("Friendly fire: {}", on_off(ctx.friendly_fire_on)), ToggleFriendlyFire, true));
    }
    rows
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

/// Step a cursor from `from` to the next index in `0..total` for which `enabled` is true, moving
/// forward (`forward`) or backward, skipping disabled indices and wrapping. Returns `from` unchanged
/// if `total == 0` or no index is enabled. The single source of the "skip-disabled-and-wrap" nav used
/// by both [`Menu::move_selection`] and the overlay's combined Actions-tab cursor, so the algorithm
/// stays host-tested in one place rather than re-derived in the (untestable) cdylib.
pub fn step_enabled(from: usize, total: usize, forward: bool, enabled: impl Fn(usize) -> bool) -> usize {
    if total == 0 {
        return from;
    }
    let step = if forward { 1 } else { total - 1 }; // wrap-safe forward/backward by 1
    let mut idx = from;
    for _ in 0..total {
        idx = (idx + step) % total;
        if enabled(idx) {
            return idx;
        }
    }
    from // all disabled: leave the cursor where it was
}

/// The first index in `0..total` for which `enabled` is true, or `0` if none (or `total == 0`).
/// Pairs with [`step_enabled`] to home a cursor onto the first usable row.
pub fn first_enabled(total: usize, enabled: impl Fn(usize) -> bool) -> usize {
    (0..total).find(|&i| enabled(i)).unwrap_or(0)
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
    /// Build the default layout: session actions first, then every registered setting. The
    /// settings rows make the registry drive both the config file and the menu (ARCHITECTURE.md >
    /// Divergences). The overlay's Actions tab now drives off [`action_rows`] rather than this
    /// `Menu`, but this full layout stays host-tested and ready for a future editable menu.
    pub fn new() -> Self {
        let settings = registry();
        let mut items = action_items();
        items.extend(settings.iter().map(|s| MenuItem::Setting(s.id)));
        Menu { items, settings, selected: 0 }
    }

    /// Build an **actions-only** menu (no settings rows). The overlay's Actions tab now renders from
    /// [`action_rows`] instead, so this is exercised only by this module's own tests today; it stays
    /// as host-tested scaffolding for a future editable menu that wants the action rows without the
    /// settings block. Settings are presented read-only elsewhere, so editing them mid-session (with
    /// its boot-vs-live and host-enforcement questions) is deliberately out of this menu for now.
    pub fn actions_only() -> Self {
        Menu { items: action_items(), settings: Vec::new(), selected: 0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Point the cursor at `index` (used by the overlay when a row is clicked). Ignored if out of
    /// range; callers only target an enabled row.
    pub fn select_index(&mut self, index: usize) {
        if index < self.items.len() {
            self.selected = index;
        }
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

    /// Move the cursor one enabled row in the direction of `delta`'s sign (forward if `>= 0`),
    /// skipping disabled rows and wrapping. No-op if every row is disabled. Shares its stepping with
    /// the overlay's combined cursor via [`step_enabled`].
    pub fn move_selection(&mut self, delta: isize, ctx: &SessionContext) {
        self.selected = step_enabled(self.selected, self.items.len(), delta >= 0, |i| self.is_enabled(i, ctx));
    }

    /// Home the cursor onto the first enabled row for `ctx`. Call this when the menu opens or the
    /// session context changes, otherwise the initial selection (`OpenWorld`) can be a disabled
    /// row — when opened mid-session, or out of session before Steam is ready / while not in-game,
    /// since Open/Join are gated on both. A disabled first row is a dead first keypress and a
    /// highlighted-but-unusable row.
    pub fn home(&mut self, ctx: &SessionContext) {
        if self.is_enabled(self.selected, ctx) {
            return;
        }
        self.selected = first_enabled(self.items.len(), |i| self.is_enabled(i, ctx));
    }

    // `select_next`/`select_prev` (and `move_selection`/`home`) are the cursor API a future editable
    // menu would drive directly; the overlay's actions tab now owns its own combined cursor and calls
    // the shared `step_enabled`/`first_enabled` helpers instead, so these stay host-tested below.
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
            MenuItem::Action { action, enabled } => {
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
                MenuItem::Action { action, enabled } => MenuRow {
                    label: action.label().to_string(),
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
    fn step_enabled_skips_disabled_and_wraps_both_ways() {
        // Rows 1 and 3 enabled; 0, 2, 4 disabled.
        let enabled = |i: usize| matches!(i, 1 | 3);
        // Forward from 1 -> 3, then wraps 3 -> 1 (skipping 4, 0, 2).
        assert_eq!(step_enabled(1, 5, true, enabled), 3);
        assert_eq!(step_enabled(3, 5, true, enabled), 1);
        // Backward from 3 -> 1, then wraps 1 -> 3.
        assert_eq!(step_enabled(3, 5, false, enabled), 1);
        assert_eq!(step_enabled(1, 5, false, enabled), 3);
        // From a disabled row, forward finds the next enabled one.
        assert_eq!(step_enabled(0, 5, true, enabled), 1);
        assert_eq!(step_enabled(2, 5, false, enabled), 1);
    }

    #[test]
    fn step_enabled_degenerate_cases_leave_cursor_put() {
        // total == 0: no rows, returns `from` unchanged.
        assert_eq!(step_enabled(7, 0, true, |_| true), 7);
        // Every row disabled: bounded loop returns `from`, never spins.
        assert_eq!(step_enabled(2, 5, true, |_| false), 2);
        // Single enabled row: stepping lands back on it.
        assert_eq!(step_enabled(3, 5, true, |i| i == 3), 3);
    }

    #[test]
    fn first_enabled_finds_first_or_falls_back_to_zero() {
        assert_eq!(first_enabled(5, |i| i >= 2), 2);
        assert_eq!(first_enabled(5, |i| i == 0), 0);
        // None enabled (or empty) falls back to 0.
        assert_eq!(first_enabled(5, |_| false), 0);
        assert_eq!(first_enabled(0, |_| true), 0);
    }

    #[test]
    fn rows_cover_actions_and_settings() {
        let menu = Menu::new();
        let rows = menu.rows(&Config::default(), &SessionContext::default());
        // 8 actions + 23 settings.
        assert_eq!(rows.len(), 31);
        // Action rows have no value; setting rows do.
        assert!(rows[0].value.is_none());
        assert!(rows.iter().filter(|r| r.value.is_some()).count() == 23);
    }

    #[test]
    fn actions_only_has_no_setting_rows() {
        let menu = Menu::actions_only();
        let rows = menu.rows(&Config::default(), &SessionContext::default());
        assert_eq!(rows.len(), 8, "actions-only menu shows just the 8 session actions");
        assert!(rows.iter().all(|r| r.value.is_none()), "no setting rows (those carry a value)");

        // Navigation + activation still work; the homed cursor (ready, out of session) lands on
        // OpenWorld — which needs Steam up and the player in-game.
        let mut menu = Menu::actions_only();
        let mut cfg = Config::default();
        let ctx = SessionContext { steam_ready: true, in_game: true, ..Default::default() };
        menu.home(&ctx);
        assert_eq!(menu.activate(&mut cfg, &ctx), MenuOutcome::Action(SessionAction::OpenWorld));
    }

    #[test]
    fn select_index_points_the_cursor_and_ignores_out_of_range() {
        let mut menu = Menu::actions_only(); // 8 items, valid indices 0..=7
        menu.select_index(3);
        assert_eq!(menu.selected(), 3);
        menu.select_index(8); // == len: out of range, ignored (guards the `<` vs `<=` boundary)
        assert_eq!(menu.selected(), 3);
        menu.select_index(999);
        assert_eq!(menu.selected(), 3);
        // It does NOT respect the enabled predicate — callers target an enabled row, but the cursor
        // must be settable to any index (the overlay relies on this). LockWorld is disabled out of
        // session, yet select_index still moves there.
        let ctx = SessionContext::default();
        let lock = menu
            .items
            .iter()
            .position(|i| matches!(i, MenuItem::Action { action: SessionAction::LockWorld, .. }))
            .unwrap();
        assert!(!menu.rows(&Config::default(), &ctx)[lock].enabled);
        menu.select_index(lock);
        assert_eq!(menu.selected(), lock, "select_index sets the cursor regardless of enabled state");
    }

    #[test]
    fn navigation_skips_disabled_actions() {
        let mut menu = Menu::new();
        let ctx = SessionContext { in_session: false, is_host: false, steam_ready: true, in_game: true, ..Default::default() };
        // From row 0 (Open world, enabled), next-enabled skips Leave/Lock/etc (need a session) until
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
        let out_of_session = SessionContext { in_session: false, is_host: false, steam_ready: true, in_game: true, ..Default::default() };

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
    fn home_moves_off_a_disabled_first_row_when_opened_in_session() {
        let mut menu = Menu::new();
        let in_session = SessionContext { in_session: true, is_host: false, steam_ready: true, in_game: true, ..Default::default() };
        // Row 0 (OpenWorld) is disabled in-session; without home() the cursor sits on it.
        assert!(!menu.rows(&Config::default(), &in_session)[0].enabled);
        menu.home(&in_session);
        let rows = menu.rows(&Config::default(), &in_session);
        let sel = rows.iter().position(|r| r.selected).unwrap();
        assert!(rows[sel].enabled, "home() must land on an enabled row");
        // Ready and out of session, the first row (Open world) is already enabled, so home() is a no-op.
        let mut menu2 = Menu::new();
        menu2.home(&SessionContext { steam_ready: true, in_game: true, ..Default::default() });
        assert_eq!(menu2.selected(), 0);
    }

    #[test]
    fn host_only_actions_gated_by_context() {
        let menu = Menu::new();
        let guest = SessionContext { in_session: true, is_host: false, steam_ready: true, in_game: true, ..Default::default() };
        let host = SessionContext { in_session: true, is_host: true, steam_ready: true, in_game: true, ..Default::default() };
        let guest_rows = menu.rows(&Config::default(), &guest);
        let host_rows = menu.rows(&Config::default(), &host);
        let lock_guest = guest_rows.iter().find(|r| r.label == "Lock world").unwrap();
        let lock_host = host_rows.iter().find(|r| r.label == "Lock world").unwrap();
        assert!(!lock_guest.enabled, "guests can't lock the world");
        assert!(lock_host.enabled, "host can lock the world");
    }

    #[test]
    fn open_and_join_gated_on_steam_and_in_game() {
        let menu = Menu::new();
        let cfg = Config::default();

        let open_join_enabled = |ctx: &SessionContext| {
            let rows = menu.rows(&cfg, ctx);
            let open = rows.iter().find(|r| r.label == "Open world").unwrap();
            let join = rows.iter().find(|r| r.label == "Join world").unwrap();
            (open.enabled, join.enabled)
        };

        // Ready out of session: both enabled.
        let ready = SessionContext { steam_ready: true, in_game: true, ..Default::default() };
        assert_eq!(open_join_enabled(&ready), (true, true), "enabled when steam_ready && in_game && !in_session");

        // Steam not ready: both disabled even though in-game and out of session.
        let no_steam = SessionContext { steam_ready: false, in_game: true, ..Default::default() };
        assert_eq!(open_join_enabled(&no_steam), (false, false), "disabled when steam_ready is false");

        // Not in-game (at the title/menu): both disabled even with Steam up.
        let not_in_game = SessionContext { steam_ready: true, in_game: false, ..Default::default() };
        assert_eq!(open_join_enabled(&not_in_game), (false, false), "disabled when in_game is false");

        // Already in a session: both disabled even when ready.
        let in_session = SessionContext { steam_ready: true, in_game: true, in_session: true, is_host: true, ..Default::default() };
        assert_eq!(open_join_enabled(&in_session), (false, false), "disabled when already in a session");

        // The fully-default context (nothing ready) also disables both.
        assert_eq!(open_join_enabled(&SessionContext::default()), (false, false));
    }

    #[test]
    fn activating_open_join_is_noop_when_not_ready() {
        // The activate() path re-checks the predicate independently of rows(); pin that a disabled
        // Open/Join (Steam down, or not in-game) yields no action, not just a disabled-looking row.
        let mut menu = Menu::new();
        let mut cfg = Config::default();
        let open_idx = menu
            .items
            .iter()
            .position(|i| matches!(i, MenuItem::Action { action: SessionAction::OpenWorld, .. }))
            .unwrap();
        let join_idx = menu
            .items
            .iter()
            .position(|i| matches!(i, MenuItem::Action { action: SessionAction::JoinWorld, .. }))
            .unwrap();

        // Steam not ready (but in-game, out of session): both are no-ops.
        let no_steam = SessionContext { steam_ready: false, in_game: true, ..Default::default() };
        menu.select_index(open_idx);
        assert_eq!(menu.activate(&mut cfg, &no_steam), MenuOutcome::None, "Open is a no-op when Steam isn't ready");
        menu.select_index(join_idx);
        assert_eq!(menu.activate(&mut cfg, &no_steam), MenuOutcome::None, "Join is a no-op when Steam isn't ready");

        // Not in-game (Steam up): still no-ops.
        let not_in_game = SessionContext { steam_ready: true, in_game: false, ..Default::default() };
        menu.select_index(open_idx);
        assert_eq!(menu.activate(&mut cfg, &not_in_game), MenuOutcome::None, "Open is a no-op at the title/menu");

        // Ready + in-game + out of session: Open now fires.
        let ready = SessionContext { steam_ready: true, in_game: true, ..Default::default() };
        menu.select_index(open_idx);
        assert_eq!(menu.activate(&mut cfg, &ready), MenuOutcome::Action(SessionAction::OpenWorld));
    }

    #[test]
    fn home_skips_open_join_when_not_ready() {
        // The home() doc promises it lands on an enabled row even out of session when Open/Join are
        // gated off (Steam not ready / not in-game). With the full menu, row 0 (Open world) is then
        // disabled, so home() must skip past all 8 (disabled) actions onto the first setting row.
        let mut menu = Menu::new();
        let not_ready = SessionContext::default(); // steam_ready=false, in_game=false, out of session
        assert!(!menu.rows(&Config::default(), &not_ready)[0].enabled, "Open world is disabled when not ready");
        menu.home(&not_ready);
        let rows = menu.rows(&Config::default(), &not_ready);
        let sel = rows.iter().position(|r| r.selected).unwrap();
        assert!(rows[sel].enabled, "home() must land on an enabled row");
        assert!(rows[sel].value.is_some(), "with all 8 actions disabled, home() lands on the first setting");
    }

    #[test]
    fn actions_only_degrades_to_disabled_cursor_when_nothing_selectable() {
        // The real title-screen / Steam-not-up state: an actions-only menu where every row is gated
        // off. home()/first_enabled fall back to index 0 (a disabled row) and activate() is a no-op —
        // the same degenerate state the overlay's cursor-repair relies on.
        let mut menu = Menu::actions_only();
        let mut cfg = Config::default();
        let not_ready = SessionContext::default();
        assert!(menu.rows(&cfg, &not_ready).iter().all(|r| !r.enabled), "all actions disabled when not ready");
        menu.home(&not_ready);
        assert_eq!(menu.selected(), 0, "home() falls back to index 0 when no row is enabled");
        assert_eq!(menu.activate(&mut cfg, &not_ready), MenuOutcome::None, "activating the disabled row is a no-op");
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
        let before = cfg.gameplay.crit_coop;
        let outcome = menu.activate(&mut cfg, &ctx);
        assert!(matches!(outcome, MenuOutcome::SettingChanged(_)));
        assert_ne!(cfg.gameplay.crit_coop, before);
    }

    // ----- action_rows: the dynamic Actions-tab surface -----

    /// Collect `(label, action, enabled)` triples for terse assertions.
    fn triples(ctx: &SessionContext) -> Vec<(String, SessionAction, bool)> {
        action_rows(ctx).into_iter().map(|r| (r.label, r.action, r.enabled)).collect()
    }

    #[test]
    fn action_rows_solo_lists_exactly_open_and_join() {
        use SessionAction::*;
        // Solo (out of session), ready: exactly Open/Join, both enabled, no Leave / toggles.
        let ready = SessionContext { steam_ready: true, in_game: true, ..Default::default() };
        assert_eq!(
            triples(&ready),
            vec![
                ("Open world".into(), OpenWorld, true),
                ("Join world".into(), JoinWorld, true),
            ],
        );
    }

    #[test]
    fn action_rows_open_join_disabled_until_ready_and_in_game() {
        let labels = |ctx: &SessionContext| {
            let rows = action_rows(ctx);
            assert_eq!(rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["Open world", "Join world"]);
            (rows[0].enabled, rows[1].enabled)
        };
        // Both gates needed: missing either disables both; having both enables both.
        assert_eq!(labels(&SessionContext::default()), (false, false), "nothing ready");
        assert_eq!(
            labels(&SessionContext { steam_ready: true, in_game: false, ..Default::default() }),
            (false, false),
            "Steam up but at the title screen",
        );
        assert_eq!(
            labels(&SessionContext { steam_ready: false, in_game: true, ..Default::default() }),
            (false, false),
            "in-game but Steam not ready",
        );
        assert_eq!(
            labels(&SessionContext { steam_ready: true, in_game: true, ..Default::default() }),
            (true, true),
            "steam_ready && in_game",
        );
    }

    #[test]
    fn action_rows_in_session_host_lists_leave_plus_four_toggles_no_open_join() {
        use SessionAction::*;
        let host = SessionContext { in_session: true, is_host: true, steam_ready: true, in_game: true, ..Default::default() };
        assert_eq!(
            triples(&host),
            vec![
                ("Leave world".into(), LeaveWorld, true),
                ("Lock world".into(), LockWorld, true),
                ("PvP: off".into(), TogglePvp, true),
                ("PvP teams: off".into(), TogglePvpTeams, true),
                ("Friendly fire: off".into(), ToggleFriendlyFire, true),
            ],
        );
        // No connect verbs are shown while in a session.
        let labels: Vec<_> = action_rows(&host).into_iter().map(|r| r.label).collect();
        assert!(!labels.iter().any(|l| l == "Open world" || l == "Join world"));
    }

    #[test]
    fn action_rows_in_session_joiner_lists_only_leave() {
        use SessionAction::*;
        // Set every host-toggle state bit: a joiner must STILL see only Leave — the toggle block is
        // gated on `is_host`, not on the state bits, so this pins the host-only guarantee.
        let joiner = SessionContext {
            in_session: true,
            is_host: false,
            steam_ready: true,
            in_game: true,
            world_locked: true,
            pvp_on: true,
            pvp_teams_on: true,
            friendly_fire_on: true,
        };
        assert_eq!(triples(&joiner), vec![("Leave world".into(), LeaveWorld, true)]);
    }

    #[test]
    fn action_rows_toggle_rows_flip_label_and_action_with_state() {
        use SessionAction::*;
        // world_locked flips the Lock/Unlock row's label *and* its emitted action.
        let locked = SessionContext {
            in_session: true,
            is_host: true,
            world_locked: true,
            pvp_on: true,
            pvp_teams_on: true,
            friendly_fire_on: true,
            ..Default::default()
        };
        let rows = action_rows(&locked);
        // Leave, then the four toggles reflecting the "on"/locked state.
        assert_eq!(
            rows.iter().map(|r| (r.label.as_str(), r.action)).collect::<Vec<_>>(),
            vec![
                ("Leave world", LeaveWorld),
                ("Unlock world", UnlockWorld), // locked -> offers Unlock
                ("PvP: on", TogglePvp),        // action is the single flip regardless of state
                ("PvP teams: on", TogglePvpTeams),
                ("Friendly fire: on", ToggleFriendlyFire),
            ],
        );
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
