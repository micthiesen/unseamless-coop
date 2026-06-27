//! In-overlay **rig-testing guide** engine — a guided, on-screen sequence of test steps that drives
//! a tester through a rig run without the orchestrator having to round-trip "do X, tell me the
//! result, now do Y" over the wire. A guide is an ordered list of [`Step`]s; the engine shows the
//! current step as a pinned banner, advances when its finish signal fires (a manual controller press
//! or an auto [`Predicate`]), optionally [`branch`](Guide::branch)es on the result, and ends with a
//! hardcoded "done testing" toast. See [`docs/RIG-GUIDES.md`](../../../docs/RIG-GUIDES.md).
//!
//! ## Why this lives in core (host-tested)
//! All the decision logic — advance, branch, skip, finish-predicate evaluation, role filtering, the
//! done terminal — is **pure** and unit-tested here by feeding synthetic log lines + button events
//! (no game needed). The cdylib's `features::rig_guide` binding is a thin shell: it gathers a
//! per-frame [`GuideInput`] (read-only game/session state, controller chords, new log lines) and
//! renders the [`TickResult`]. That split is the same one the rest of the mod follows
//! (`docs/ARCHITECTURE.md` > core-vs-coop): logic is *verified* here, not just hoped.
//!
//! ## Debug-only
//! The whole subsystem is gated behind `#[cfg(debug_assertions)]` (on for the `dev`/test and `diag`
//! profiles, off for `release`), so the shipping build carries zero cost — there is no rig in a
//! player's hands. See `crate::lib` (the `#[cfg(debug_assertions)] pub mod guide;`).
//!
//! ## Voice
//! A guide is a **debug tool**, so its banners are PLAIN/diagnostic, never ER lore tone (CLAUDE.md >
//! "Message voice"). Authors write only the instruction text; the engine auto-appends the control
//! hints and the pending/stub markers.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::game_state::GameState;
use crate::util::push_capped;

pub mod guides;

/// How long the **done** control must be held continuously before a manual finish fires (seconds).
/// A hold (not a tap) so a fat-fingered press never advances a step by accident. Cleared the instant
/// the control is released, so it can't carry across steps.
const DONE_HOLD_SECS: f32 = 0.75;

/// Cap on the per-step accumulated log buffer that [`PredicateCtx::log_contains`] scans. A step
/// rarely needs more than the last few lines to recognize its finish signal; drop-oldest past this so
/// a chatty step can't grow it without bound (the shared [`push_capped`] discipline).
const STEP_LOG_CAP: usize = 256;

/// The **defined** banner colour for a stub (`[PENDING …]`) step — and the neutral fallback. Banner
/// colours are auto-assigned by the engine, never set in a guide; a regular step gets a deterministic
/// per-step palette hue ([`step_color`]) so consecutive steps read as visibly distinct, while a stub
/// gets this one fixed, muted amber-grey so a documentation banner reads as dim/secondary next to the
/// brightly-coloured live steps. RGB, each channel `0.0..=1.0`.
pub const PENDING_BANNER_COLOR: [f32; 3] = [0.72, 0.68, 0.55];

/// A deterministic, "random-looking" palette colour for a step, keyed off its `id` so it's **stable**
/// across frames (no flicker) and spread across the shared peer palette. Distinctness between adjacent
/// steps is likely but not guaranteed (it's a hash into an 8-entry palette, so collisions are possible);
/// the banner text changing is what actually signals an advance. Authors never choose colours.
fn step_color(id: &str) -> [f32; 3] {
    crate::palette::peer_color_for_id(fnv1a(id))
}

/// FNV-1a hash of a string to a `u64` seed for [`crate::palette::peer_color_for_id`] (which mixes it
/// again before folding to a palette entry). Cheap and deterministic — just enough to turn a step id
/// into a stable palette index.
fn fnv1a(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------------------------------
// Roles, and the core mirrors of the SDK session-FSM enums
// ---------------------------------------------------------------------------------------------------

/// Which machine a tester is on, so one shared guide runs everywhere and each machine sees only the
/// steps tagged for its role (an untagged step shows to all). Resolved from the `[debug] rig_role`
/// config field (default [`Solo`](Role::Solo)). This is what makes two-player testing easy: the host
/// machine sees host steps, the joiner sees joiner steps, from the same committed guide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The machine hosting the co-op session.
    Host,
    /// The machine joining the host's session.
    Join,
    /// A single machine testing alone (the default, and the unknown-value fallback — most RE probes
    /// are solo-capable). `#[serde(other)]` means a typo'd `rig_role` degrades to `Solo` rather than
    /// failing the whole config parse (which would fall back to an empty-password default and trip the
    /// startup password guard) — the same degrade-don't-fail posture as the `Unknown` enum arms below.
    #[default]
    #[serde(other)]
    Solo,
}

/// Core mirror of the SDK's `CSSessionManager::lobby_state` enum (`eldenring::cs::LobbyState`), so a
/// guide can reference session states (`lobby_is(LobbyState::TryToCreateSession)`) without `core`
/// taking a game dependency. The cdylib maps the live SDK value across via [`LobbyState::from_u32`].
/// Discriminants are pinned to the SDK's `repr(u32)` values (see `docs/SESSION-RE-FINDINGS.md`); a
/// future game/SDK bump must re-verify them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum LobbyState {
    #[default]
    None = 0,
    TryToCreateSession = 1,
    FailedToCreateSession = 2,
    Host = 3,
    TryToJoinSession = 4,
    FailedToJoinSession = 5,
    Client = 6,
    OnLeaveSession = 7,
    FailedToLeaveSession = 8,
    /// Any value the SDK adds that this mirror doesn't name yet — so a torn/garbage read or a future
    /// variant degrades to a well-defined "unknown" instead of fabricating a known state.
    Unknown = u32::MAX,
}

impl LobbyState {
    /// Map a raw SDK discriminant to this mirror; an unrecognized value becomes [`Unknown`](Self::Unknown).
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::None,
            1 => Self::TryToCreateSession,
            2 => Self::FailedToCreateSession,
            3 => Self::Host,
            4 => Self::TryToJoinSession,
            5 => Self::FailedToJoinSession,
            6 => Self::Client,
            7 => Self::OnLeaveSession,
            8 => Self::FailedToLeaveSession,
            _ => Self::Unknown,
        }
    }
}

/// Core mirror of the SDK's `CSSessionManager::protocol_state` enum (`eldenring::cs::ProtocolState`),
/// pinned to the SDK's `repr(u32)` values. Same role as [`LobbyState`] for the protocol half of the
/// session FSM; the cdylib maps the live value via [`ProtocolState::from_u32`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum ProtocolState {
    #[default]
    None = 0,
    JoinCheck = 1,
    WaitInitData = 2,
    WaitReloadWait = 3,
    WaitReload = 4,
    WaitReload2 = 5,
    Ingame = 6,
    WaitReentryToMap = 7,
    /// Any unrecognized/future value (see [`LobbyState::Unknown`]).
    Unknown = u32::MAX,
}

impl ProtocolState {
    /// Map a raw SDK discriminant to this mirror; an unrecognized value becomes [`Unknown`](Self::Unknown).
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::None,
            1 => Self::JoinCheck,
            2 => Self::WaitInitData,
            3 => Self::WaitReloadWait,
            4 => Self::WaitReload,
            5 => Self::WaitReload2,
            6 => Self::Ingame,
            7 => Self::WaitReentryToMap,
            _ => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// The per-frame snapshot predicates read
// ---------------------------------------------------------------------------------------------------

/// Read-only game/session state for the current frame, sampled by the cdylib binding and handed to
/// [`GuideRunner::tick`]. Predicates read it (through [`PredicateCtx`]); the engine never writes game
/// state. All-`Default` ("nothing up yet") is the correct pre-session / pre-load reading.
#[derive(Clone, Copy, Debug, Default)]
pub struct RigState {
    /// Coarse game lifecycle (booting / frontend / in-game), from `crate::playstate`.
    pub game_state: GameState,
    /// Session FSM lobby state (`None` until a session is forming), from `CSSessionManager`.
    pub lobby_state: LobbyState,
    /// Session FSM protocol state, from `CSSessionManager`.
    pub protocol_state: ProtocolState,
    /// Connected players in the session (`0` solo / pre-session).
    pub players: usize,
}

/// What a [`Predicate`] sees: the frame's [`RigState`], how long the current step has been showing,
/// and the log lines seen **since this step started** (so a `log_contains` check matches a line that
/// appeared on any earlier frame of the step, not only the exact frame it landed).
pub struct PredicateCtx<'a> {
    pub state: &'a RigState,
    /// Seconds the current step has been the active step.
    pub step_elapsed_secs: f32,
    /// Log lines accumulated since the current step started (newest last, capped). Private — read it
    /// through [`PredicateCtx::log_contains`] so the storage stays an implementation detail.
    step_log: &'a VecDeque<String>,
}

impl PredicateCtx<'_> {
    /// Whether any log line seen since the current step started contains `needle` (substring match).
    pub fn log_contains(&self, needle: &str) -> bool {
        self.step_log.iter().any(|line| line.contains(needle))
    }
}

/// A finish condition over the read-only [`PredicateCtx`]. Composable via [`Predicate::and`] /
/// [`Predicate::or`]; the ready-made constructors ([`log_contains`], [`lobby_is`], [`after_secs`], …)
/// cover the common cases, and [`Predicate::new`] takes any closure so the set is trivial to extend.
pub struct Predicate(Box<dyn Fn(&PredicateCtx) -> bool + Send>);

impl Predicate {
    /// Wrap an arbitrary predicate closure. Use the named constructors below where one fits; reach
    /// for this to express a one-off condition without adding a constructor.
    pub fn new(f: impl Fn(&PredicateCtx) -> bool + Send + 'static) -> Self {
        Self(Box::new(f))
    }

    fn check(&self, ctx: &PredicateCtx) -> bool {
        (self.0)(ctx)
    }

    /// Satisfied only when both are (short-circuiting).
    pub fn and(self, other: Predicate) -> Predicate {
        Predicate::new(move |ctx| self.check(ctx) && other.check(ctx))
    }

    /// Satisfied when either is (short-circuiting).
    pub fn or(self, other: Predicate) -> Predicate {
        Predicate::new(move |ctx| self.check(ctx) || other.check(ctx))
    }
}

/// Finish when a log line containing `needle` has been seen since the step started. The flagship use:
/// catch a `session-probe:` transition line that proves an FSM edge fired.
pub fn log_contains(needle: &'static str) -> Predicate {
    Predicate::new(move |ctx| ctx.log_contains(needle))
}

/// Finish when the session lobby FSM equals `state` (e.g. `TryToCreateSession`).
pub fn lobby_is(state: LobbyState) -> Predicate {
    Predicate::new(move |ctx| ctx.state.lobby_state == state)
}

/// Finish when the session protocol FSM equals `state` (e.g. `Ingame`).
pub fn protocol_is(state: ProtocolState) -> Predicate {
    Predicate::new(move |ctx| ctx.state.protocol_state == state)
}

/// Finish when the coarse game lifecycle equals `state` (e.g. `InGame` for "a save is loaded").
pub fn game_state_is(state: GameState) -> Predicate {
    Predicate::new(move |ctx| ctx.state.game_state == state)
}

/// Finish once at least `n` players are connected — the "a peer joined" signal.
pub fn players_at_least(n: usize) -> Predicate {
    Predicate::new(move |ctx| ctx.state.players >= n)
}

/// Finish after the step has been showing for `secs` seconds (a dwell timer).
pub fn after_secs(secs: f32) -> Predicate {
    Predicate::new(move |ctx| ctx.step_elapsed_secs >= secs)
}

// ---------------------------------------------------------------------------------------------------
// Steps, guides, the fluent builder
// ---------------------------------------------------------------------------------------------------

/// Where to go when a step finishes (returned from a [`branch`](Guide::branch) closure, and the target
/// of a [`default_branch`](Guide::default_branch) used on skip). `To` addresses a step by its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Advance {
    /// The next step in declaration order (resolves to [`Done`](Advance::Done) past the last step).
    Next,
    /// Jump to the step with this id. If that step isn't visible to the active role, the engine lands
    /// on the next visible step at/after it (role filtering applies to jumps too), and an unknown id
    /// resolves to [`Done`](Advance::Done) (never panics). So branch to untagged steps unless you mean
    /// to role-skip.
    To(&'static str),
    /// End the guide (fires the "done testing" toast).
    Done,
}

type BranchFn = Box<dyn Fn(&PredicateCtx) -> Advance + Send>;

/// One test step. Built via the fluent [`Guide`] builder; defaults to **manual-finish, serial-next,
/// all-roles, executable**. Opt in with `.role(...)`, `.done_when(...)`, `.branch(...)`,
/// `.default_branch(...)`, `.stub(...)`.
pub struct Step {
    id: &'static str,
    instruction: String,
    role: Option<Role>,
    done_when: Option<Predicate>,
    branch: Option<BranchFn>,
    /// Where skip sends this step (and where a stub step advances). `None` ⇒ the engine's sensible
    /// default ([`Advance::Next`], which becomes `Done` past the last step), so skip never panics.
    skip_to: Option<Advance>,
    /// `Some(reason)` marks a **stub**: a not-yet-executable step that renders as committed
    /// documentation (a `[PENDING …]` banner) until the work behind it lands. A stub has no auto
    /// finish; it advances on done/skip like any manual step, so an all-stub guide still reaches the
    /// done toast cleanly. See `.stub(...)`.
    stub: Option<&'static str>,
    /// `Some(options)` marks a **choice step**: instead of the normal manual-done / auto-finish path,
    /// it shows a focused modal of preset options (each a label + the [`Advance`] confirming it takes)
    /// and waits for the tester to pick one (or skip). Selecting one captures the answer as a
    /// [`ChoiceMade`] (logged by the binding) and advances per that option's `Advance`. This is the
    /// **last resort after logging** (CLAUDE.md / the rig-guides skill): only for an irreducibly
    /// human-perceptual signal whose answer matters (it branches, or is worth recording). See
    /// [`Guide::choice`].
    choice: Option<Vec<(&'static str, Advance)>>,
    /// Whether a choice step also offers an optional free-form **note** field (keyboard-entered in the
    /// overlay). The captured note rides on the logged [`ChoiceMade`]. Only meaningful with `choice`.
    choice_note: bool,
}

impl Step {
    fn new(id: &'static str, instruction: String) -> Self {
        Step {
            id,
            instruction,
            role: None,
            done_when: None,
            branch: None,
            skip_to: None,
            stub: None,
            choice: None,
            choice_note: false,
        }
    }
}

/// An ordered guide: a name (for the registry / `[debug] guide`) and its steps. Build with the fluent
/// API — `Guide::new(name).step(id, text).done_when(pred).branch(f)…` — where each modifier applies to
/// the most recently added [`step`](Guide::step). Goal: writing a guide is "describe the steps".
pub struct Guide {
    name: &'static str,
    steps: Vec<Step>,
}

impl Guide {
    /// Start a new, empty guide with the registry `name`.
    pub fn new(name: &'static str) -> Self {
        Guide { name, steps: Vec::new() }
    }

    /// Append a step with a unique `id` and the tester-facing `instruction` text. Subsequent modifier
    /// calls (`.role`, `.done_when`, …) apply to this step until the next `.step(...)`.
    pub fn step(mut self, id: &'static str, instruction: impl Into<String>) -> Self {
        // Ids must be unique: `index_of`/`step_color` key off them, so a duplicate would silently
        // mis-route a branch `To(id)` to the first match and share its colour. Caught at build time
        // (every committed guide is constructed in a host test), never on the rig.
        debug_assert!(
            !self.steps.iter().any(|s| s.id == id),
            "guide '{}' has a duplicate step id '{id}'",
            self.name,
        );
        self.steps.push(Step::new(id, instruction.into()));
        self
    }

    /// Show this step only on machines whose [`Role`] matches (untagged ⇒ shown to all). Steps that
    /// don't match the active role are skipped over automatically during advancement.
    pub fn role(mut self, role: Role) -> Self {
        self.last_mut().role = Some(role);
        self
    }

    /// Auto-finish this step when `pred` holds (instead of waiting for a manual press). Manual finish
    /// still works as an override (hold the done control), so a never-firing predicate can't trap the
    /// tester.
    pub fn done_when(mut self, pred: Predicate) -> Self {
        let step = self.last_mut();
        debug_assert!(
            step.choice.is_none(),
            "step '{}' mixes .done_when() with .choice() — a choice supersedes the auto-finish path",
            step.id,
        );
        step.done_when = Some(pred);
        self
    }

    /// On finish, decide the next step from the result (the read-only [`PredicateCtx`]) instead of
    /// going serially to the next step. Return an [`Advance`].
    pub fn branch(mut self, f: impl Fn(&PredicateCtx) -> Advance + Send + 'static) -> Self {
        let step = self.last_mut();
        debug_assert!(
            step.choice.is_none(),
            "step '{}' mixes .branch() with .choice() — a choice's options carry their own Advance",
            step.id,
        );
        step.branch = Some(Box::new(f));
        self
    }

    /// The branch a **skip** takes for this step (also the sensible default destination for a branching
    /// step when the tester gives up). Without this, skip uses [`Advance::Next`] (Done past the last
    /// step), so a step always has a skip target and the engine never panics.
    pub fn default_branch(mut self, advance: Advance) -> Self {
        self.last_mut().skip_to = Some(advance);
        self
    }

    /// Mark this step a **stub** documenting work that isn't executable yet (`reason` says what it's
    /// waiting on). It renders as a `[PENDING: reason]` banner and advances on done/skip with no auto
    /// finish — so a partially-built guide can be committed now as living documentation and revived
    /// when the RE catches up, and a tester is never trapped by it.
    pub fn stub(mut self, reason: &'static str) -> Self {
        let step = self.last_mut();
        debug_assert!(
            step.choice.is_none(),
            "step '{}' mixes .stub() with .choice() — a stub isn't an executable choice",
            step.id,
        );
        step.stub = Some(reason);
        self
    }

    /// Make this step a **choice step**: a focused modal presenting `options` (each a `(label,
    /// Advance)`), where selecting one logs the answer and advances per that option's [`Advance`].
    /// Use it as the **last resort after logging** — only for an irreducibly human-perceptual signal
    /// (the tester's eyes/judgement) whose answer *matters*, i.e. it branches or is worth recording;
    /// a plain "press to continue" stays a normal manual step, not a choice. The answer is always
    /// logged (captured, shareable), like every other guide signal. Pair with [`note`](Self::note)
    /// for an optional free-form annotation, and [`default_branch`](Self::default_branch) for where a
    /// skip lands.
    ///
    /// A choice step supersedes the manual-done / `done_when` / `branch` path — the option's own
    /// `Advance` is the branch. Skip still escapes it (logged as `skipped`, taking the skip target),
    /// so the never-trap rule holds.
    pub fn choice(mut self, options: &[(&'static str, Advance)]) -> Self {
        debug_assert!(!options.is_empty(), "a choice step needs at least one option");
        let step = self.last_mut();
        debug_assert!(
            step.done_when.is_none() && step.branch.is_none() && step.stub.is_none(),
            "step '{}' mixes .choice() with .done_when()/.branch()/.stub() — a choice supersedes them",
            step.id,
        );
        step.choice = Some(options.to_vec());
        self
    }

    /// Offer an optional free-form **note** field on a [`choice`](Self::choice) step — a keyboard-only
    /// text entry (the controller can only navigate the presets; the note needs a keyboard). Whatever
    /// the tester types is captured on the logged [`ChoiceMade`] alongside the chosen label. No-op
    /// styling on a non-choice step (the modal is what renders the field).
    pub fn note(mut self) -> Self {
        self.last_mut().choice_note = true;
        self
    }

    /// The guide's registry name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Number of steps (all roles).
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the guide has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    fn last_mut(&mut self) -> &mut Step {
        self.steps.last_mut().expect("a guide modifier (.role/.done_when/…) was called before .step()")
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.steps.iter().position(|s| s.id == id)
    }
}

// ---------------------------------------------------------------------------------------------------
// The runner — the per-frame state machine
// ---------------------------------------------------------------------------------------------------

/// The control hint labels appended to every banner (e.g. `done = "L3 + D-pad Up"`). Supplied by the
/// cdylib (which owns the actual button mapping) so the engine produces the full, host-tested banner
/// text without hard-coding button names from the coop layer.
#[derive(Clone, Copy, Debug)]
pub struct ControlHints {
    pub done: &'static str,
    pub skip: &'static str,
}

/// One frame of input to [`GuideRunner::tick`]: the frame delta, the read-only [`RigState`], the log
/// lines emitted since the last tick, and the raw held-state of the two control chords. The engine
/// turns the raw held bools into a held-to-confirm (done) and a one-shot edge (skip) itself, so that
/// logic is host-tested rather than living in the binding.
pub struct GuideInput<'a> {
    pub delta: f32,
    pub state: &'a RigState,
    pub new_log_lines: &'a [String],
    /// The **done** chord is held this frame (raw level; the engine applies the hold timer).
    pub done_held: bool,
    /// The **skip** chord is held this frame (raw level; the engine fires on the rising edge).
    pub skip_held: bool,
    /// Modal input for a **choice step** (no effect on a normal step). Gathered by the binding from the
    /// overlay menu input layer (the same up/down/confirm the Actions/Settings tabs use) and the
    /// keyboard note field — not the done/skip chords, which stay the normal-step controls.
    pub choice: ChoiceInput<'a>,
}

/// One frame of **choice-modal** input, fed through [`GuideInput`]. `up`/`down` are already-edged nav
/// intents (the overlay's [`PadEdges`](crate::pad::PadEdges) auto-repeat + keyboard arrows), `confirm`
/// a one-shot select, and `note` the current free-form text buffer (keyboard-entered in the overlay,
/// captured by the engine on confirm). All-default ("no modal input this frame") is the correct
/// reading on a normal step or an idle modal frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChoiceInput<'a> {
    /// Move the selection to the previous option (wraps).
    pub up: bool,
    /// Move the selection to the next option (wraps).
    pub down: bool,
    /// Confirm the selected option (one-shot): logs the answer and advances per its [`Advance`].
    pub confirm: bool,
    /// The current free-form note buffer, captured onto [`ChoiceMade`] when a choice resolves.
    pub note: &'a str,
}

/// What the binding should render after a [`tick`](GuideRunner::tick). `banner` is the pinned-banner
/// text for the current step (already including the control hints + any `[PENDING]` marker), or `None`
/// when the guide is finished/idle. `color` is the **auto-assigned** RGB to draw it in (a per-step
/// palette hue, or [`PENDING_BANNER_COLOR`] for a stub) — colours are never set in a guide. `finished_now`
/// is `true` on exactly the tick the guide completes, so the binding fires the "done testing" toast
/// once. `stub` flags that the current step is a stub, so the renderer can style it as documentation.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TickResult {
    pub banner: Option<String>,
    pub color: [f32; 3],
    pub finished_now: bool,
    pub stub: bool,
    /// `Some` while the current step is a **choice step**: the modal the binding should draw (and a
    /// signal that the pinned `banner` is `None` for this step — the modal replaces it). Carries the
    /// engine-held selection so the overlay just highlights it.
    pub choice: Option<ChoiceView>,
    /// `Some` on exactly the tick a choice **resolved** (a confirm or a skip), so the binding logs the
    /// captured answer once — the one source logging alone can't reach (the tester's judgement) turned
    /// into captured, shareable data.
    pub choice_made: Option<ChoiceMade>,
}

/// The render model for a **choice modal** — everything the overlay needs to draw it, with no decision
/// logic. The selection index is engine-held (the overlay only highlights `selected`); colours and the
/// skip hint are passed through so the overlay invents none.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceView {
    pub step_id: &'static str,
    /// The tester-facing prompt (the step's instruction text). Plain/diagnostic voice.
    pub prompt: String,
    /// The preset option labels, in declaration order; `selected` indexes into this.
    pub options: Vec<&'static str>,
    /// The engine-held selection index (already clamped in range).
    pub selected: usize,
    /// Whether to draw the optional free-form note field (keyboard-only).
    pub note_enabled: bool,
    /// Auto-assigned banner colour (a per-step palette hue) — never chosen in a guide.
    pub color: [f32; 3],
    /// The skip control label (from [`ControlHints`]) so the modal can show how to escape without the
    /// overlay hard-coding the chord.
    pub skip_hint: &'static str,
}

/// A resolved choice, surfaced once on [`TickResult::choice_made`] for the binding to log
/// (`rig-guide: '<id>' -> '<label>'`, plus `note = "<text>"` when free-form). `label` is the chosen
/// option's label, or `"skipped"` when the step was skipped.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceMade {
    pub step_id: &'static str,
    pub label: &'static str,
    pub note: String,
}

/// Drives one [`Guide`] for one machine: tracks the current step, the per-step timers and log buffer,
/// and the control edges, advancing through the guide as finish signals fire. Created with
/// [`GuideRunner::start`]; ticked once per frame by the cdylib feature.
pub struct GuideRunner {
    guide: Guide,
    role: Role,
    hints: ControlHints,
    /// Index of the active step, or `None` once the guide is finished (or had no steps for this role).
    current: Option<usize>,
    step_elapsed: f32,
    /// Continuous hold time on the done chord (reset to 0 when released).
    done_hold: f32,
    /// Set once a hold has fired this press, cleared on release — so holding through an advance doesn't
    /// instantly finish the next step.
    done_consumed: bool,
    /// Last frame's skip-held level, for rising-edge detection.
    prev_skip: bool,
    /// Log lines accumulated since the current step started.
    step_log: VecDeque<String>,
    /// Set by [`advance`](GuideRunner::advance) on the tick the guide reaches its end, surfaced once
    /// via [`TickResult::finished_now`].
    just_finished: bool,
    /// Selection index within the current **choice step** (reset to 0 on every advance). Engine-held so
    /// the nav/clamp/wrap logic stays host-tested; the overlay only renders [`ChoiceView::selected`].
    choice_sel: usize,
    /// Set on the tick a choice resolves, surfaced once via [`TickResult::choice_made`] (recomputed
    /// each tick like [`just_finished`](Self::just_finished)).
    just_made_choice: Option<ChoiceMade>,
}

impl GuideRunner {
    /// Start running `guide` for `role`, with the control-hint labels `hints`. The active step is the
    /// first one visible to `role`; a guide with no role-visible steps starts already finished (idle,
    /// no toast — there was nothing to do).
    pub fn start(guide: Guide, role: Role, hints: ControlHints) -> Self {
        let mut runner = GuideRunner {
            current: None,
            guide,
            role,
            hints,
            step_elapsed: 0.0,
            done_hold: 0.0,
            done_consumed: false,
            prev_skip: false,
            step_log: VecDeque::new(),
            just_finished: false,
            choice_sel: 0,
            just_made_choice: None,
        };
        runner.current = runner.first_visible_from(0);
        runner
    }

    /// Advance one frame and return what to render. Precedence on a single frame: a **manual done**
    /// (completed hold) first, then a **skip** edge, then an **auto-finish** predicate. Done and an
    /// auto-finish take the step's branch; skip takes its skip target. Skip is honored above the
    /// auto-finish so it is always a live escape — a step whose predicate stays satisfied (or whose
    /// branch loops back to itself) can never starve skip and trap the tester.
    pub fn tick(&mut self, input: &GuideInput) -> TickResult {
        self.just_finished = false; // one-shot, recomputed each tick
        self.just_made_choice = None; // ditto — only set on the tick a choice resolves

        let Some(idx) = self.current else {
            return TickResult::default(); // finished/idle: nothing to draw, no toast
        };

        // A non-finite/negative delta (a bad engine frame) must not advance timers or extend a hold.
        let delta = if input.delta.is_finite() { input.delta.max(0.0) } else { 0.0 };
        self.step_elapsed += delta;
        for line in input.new_log_lines {
            push_capped(&mut self.step_log, line.clone(), STEP_LOG_CAP);
        }

        // Manual finish: the done chord held continuously past the threshold, firing once per press.
        if input.done_held {
            self.done_hold += delta;
        } else {
            self.done_hold = 0.0;
            self.done_consumed = false;
        }
        let done_fired = input.done_held && self.done_hold >= DONE_HOLD_SECS && !self.done_consumed;
        if done_fired {
            self.done_consumed = true;
        }

        // Skip: rising edge of the skip chord (a press, not a hold).
        let skip_fired = input.skip_held && !self.prev_skip;
        self.prev_skip = input.skip_held;

        // Choice step: driven by the modal nav/confirm (overlay menu layer) + the skip chord, not the
        // done/auto path. Handled here and returned, so a choice never consults `done_when`/`branch`.
        // Cloning the small options vec sidesteps a borrow tangle with `self.advance` below; it's a
        // handful of (`&str`, `Advance`) per frame a modal is up.
        if let Some(options) = self.guide.steps[idx].choice.clone() {
            let step_id = self.guide.steps[idx].id;
            let skip_to = self.guide.steps[idx].skip_to.clone();
            let note_enabled = self.guide.steps[idx].choice_note;
            let n = options.len();
            // Nav (wrap). Clamp first so a stale index (shouldn't happen — options are fixed) can't
            // index out of range below.
            if self.choice_sel >= n {
                self.choice_sel = 0;
            }
            if n > 0 {
                if input.choice.up {
                    self.choice_sel = (self.choice_sel + n - 1) % n;
                }
                if input.choice.down {
                    self.choice_sel = (self.choice_sel + 1) % n;
                }
            }
            // Resolve: skip escapes above confirm (never-trap — a choice step is always escapable). A
            // skip carries NO note — it's "I'm bailing", not an answer, and the cross-thread note buffer
            // may still hold a *prior* note-step's text (skip is read out-of-band and can fire before the
            // overlay redraws this modal). The note is captured only on a real confirm, and only for a
            // step that opted into `.note()`.
            let resolved: Option<(Advance, ChoiceMade)> = if skip_fired {
                let made = ChoiceMade { step_id, label: "skipped", note: String::new() };
                Some((skip_to.unwrap_or(Advance::Next), made))
            } else if input.choice.confirm && n > 0 {
                let note = if note_enabled { input.choice.note.to_string() } else { String::new() };
                let (label, advance) = options[self.choice_sel].clone();
                let made = ChoiceMade { step_id, label, note };
                Some((advance, made))
            } else {
                None
            };
            if let Some((advance, made)) = resolved {
                self.just_made_choice = Some(made);
                self.advance(advance, idx);
            }
            return self.render();
        }

        let ctx = PredicateCtx {
            state: input.state,
            step_elapsed_secs: self.step_elapsed,
            step_log: &self.step_log,
        };

        // A stub never auto-finishes (it isn't executable); only its done_when (if any) is consulted
        // for a real step.
        let step = &self.guide.steps[idx];
        let auto_finished =
            step.stub.is_none() && step.done_when.as_ref().is_some_and(|p| p.check(&ctx));

        // Precedence: manual done > skip > auto-finish. Skip sits above the auto-finish (but below a
        // completed manual done) so it's always a live escape — a sticky predicate or a self-looping
        // branch can never starve it. Done and auto take the branch; skip takes the skip target.
        let advance: Option<Advance> = if skip_fired && !done_fired {
            Some(step.skip_to.clone().unwrap_or(Advance::Next))
        } else if done_fired || auto_finished {
            Some(match &step.branch {
                Some(branch) => branch(&ctx),
                None => Advance::Next,
            })
        } else {
            None
        };
        if let Some(advance) = advance {
            self.advance(advance, idx);
        }

        self.render()
    }

    /// Build the [`TickResult`] for the (possibly newly-advanced) current step, including its
    /// auto-assigned colour (a per-step palette hue, or [`PENDING_BANNER_COLOR`] for a stub).
    fn render(&self) -> TickResult {
        // A resolved choice surfaces once, regardless of where it advanced to (incl. straight to Done).
        let choice_made = self.just_made_choice.clone();
        match self.current {
            Some(idx) => {
                let step = &self.guide.steps[idx];
                // A choice step renders as a modal, not a pinned banner: `banner` is `None`, `choice`
                // carries the model. The colour is a normal per-step hue (a choice is never a stub).
                if let Some(options) = step.choice.as_ref() {
                    let color = step_color(step.id);
                    let view = ChoiceView {
                        step_id: step.id,
                        prompt: step.instruction.clone(),
                        options: options.iter().map(|(label, _)| *label).collect(),
                        selected: self.choice_sel.min(options.len().saturating_sub(1)),
                        note_enabled: step.choice_note,
                        color,
                        skip_hint: self.hints.skip,
                    };
                    return TickResult {
                        banner: None,
                        color,
                        finished_now: false,
                        stub: false,
                        choice: Some(view),
                        choice_made,
                    };
                }
                let stub = step.stub.is_some();
                let color = if stub { PENDING_BANNER_COLOR } else { step_color(step.id) };
                TickResult {
                    banner: Some(self.banner_for(step)),
                    color,
                    finished_now: false,
                    stub,
                    choice: None,
                    choice_made,
                }
            }
            None => TickResult {
                banner: None,
                color: PENDING_BANNER_COLOR,
                finished_now: self.just_finished,
                stub: false,
                choice: None,
                choice_made,
            },
        }
    }

    /// Render a step's pinned banner: a `[PENDING: reason]` prefix for a stub, the instruction text,
    /// then the auto-appended control hints (authors never write these). Plain/diagnostic voice.
    fn banner_for(&self, step: &Step) -> String {
        let hints = format!("(hold {} = done, {} = skip)", self.hints.done, self.hints.skip);
        // ASCII only: the overlay's pinned banner uses the Spleen ASCII-subset font, so a non-ASCII
        // glyph (em dash, etc.) would render blank — keep markers/separators plain.
        match step.stub {
            Some(reason) => format!("[PENDING: {reason}]\n{}\n{hints}", step.instruction),
            None => format!("{}\n{hints}", step.instruction),
        }
    }

    /// Resolve `advance` (from step `from`) to the next current step, honoring role visibility, and
    /// reset the per-step state. Reaching the end sets [`just_finished`](Self::just_finished).
    fn advance(&mut self, advance: Advance, from: usize) {
        let target = match advance {
            Advance::Next => self.first_visible_from(from + 1),
            // An unknown `To` id (e.g. a guide-authoring slip in a branch closure) degrades to Done
            // rather than panicking — the engine must never crash the game over an authoring typo.
            Advance::To(id) => self.guide.index_of(id).and_then(|i| self.first_visible_from(i)),
            Advance::Done => None,
        };

        self.current = target;
        self.step_elapsed = 0.0;
        self.done_hold = 0.0;
        self.choice_sel = 0; // a fresh step's modal (if any) starts on its first option
        // Keep `done_consumed` true if the done chord is still held (set by the caller on a manual
        // finish): the tester must release and re-hold to advance the next step. `prev_skip` likewise
        // stays so a still-held skip doesn't re-fire on the next step.
        self.step_log.clear();
        if target.is_none() {
            self.just_finished = true;
        }
    }

    /// Whether the step at `idx` is visible to this runner's role (untagged ⇒ visible to all).
    fn is_visible(&self, idx: usize) -> bool {
        match self.guide.steps[idx].role {
            None => true,
            Some(role) => role == self.role,
        }
    }

    /// The first step index at or after `start` visible to this role, or `None` if there is none
    /// (⇒ the guide is finished for this machine).
    fn first_visible_from(&self, start: usize) -> Option<usize> {
        (start..self.guide.steps.len()).find(|&i| self.is_visible(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINTS: ControlHints = ControlHints { done: "DONE", skip: "SKIP" };
    /// A frame delta long enough to cross [`DONE_HOLD_SECS`] in one tick, for terse manual-finish tests.
    const HOLD_FRAME: f32 = DONE_HOLD_SECS + 0.1;

    /// Build a default input (no signals) over a borrowed state + log slice.
    fn input<'a>(state: &'a RigState, lines: &'a [String], delta: f32) -> GuideInput<'a> {
        GuideInput {
            delta,
            state,
            new_log_lines: lines,
            done_held: false,
            skip_held: false,
            choice: ChoiceInput::default(),
        }
    }

    /// Tick with no external signals (just time passing).
    fn idle_tick(runner: &mut GuideRunner, state: &RigState) -> TickResult {
        runner.tick(&input(state, &[], 1.0 / 60.0))
    }

    /// Simulate a full done **press**: a released frame (so the hold is a fresh edge — the engine
    /// requires release-before-refire) then a frame holding the chord past the threshold.
    fn done_tick(runner: &mut GuideRunner, state: &RigState) -> TickResult {
        idle_tick(runner, state); // release
        let mut i = input(state, &[], HOLD_FRAME);
        i.done_held = true;
        runner.tick(&i)
    }

    /// Simulate a full skip **press**: a released frame then a frame with skip held (a rising edge).
    fn skip_tick(runner: &mut GuideRunner, state: &RigState) -> TickResult {
        idle_tick(runner, state); // release, so the press is a rising edge
        let mut i = input(state, &[], 1.0 / 60.0);
        i.skip_held = true;
        runner.tick(&i)
    }

    fn linear_guide() -> Guide {
        Guide::new("linear").step("a", "step a").step("b", "step b").step("c", "step c")
    }

    #[test]
    fn serial_advance_through_manual_finishes_then_done_toast() {
        let mut r = GuideRunner::start(linear_guide(), Role::Solo, HINTS);
        let state = RigState::default();

        // First banner is step a, with the auto-appended hints.
        let first = idle_tick(&mut r, &state);
        assert!(first.banner.as_ref().unwrap().contains("step a"));
        assert!(first.banner.as_ref().unwrap().contains("hold DONE = done, SKIP = skip"));
        assert!(!first.finished_now);

        // Hold done -> step b, then step c.
        assert!(done_tick(&mut r, &state).banner.unwrap().contains("step b"));
        assert!(done_tick(&mut r, &state).banner.unwrap().contains("step c"));

        // Finishing the last step ends the guide: banner clears, finished_now fires exactly once.
        let end = done_tick(&mut r, &state);
        assert_eq!(end.banner, None);
        assert!(end.finished_now, "the done toast fires on the completing tick");
        // ...and never again.
        let after = idle_tick(&mut r, &state);
        assert!(!after.finished_now && after.banner.is_none());
    }

    #[test]
    fn manual_hold_requires_the_full_duration_and_fires_once_per_press() {
        let mut r = GuideRunner::start(linear_guide(), Role::Solo, HINTS);
        let state = RigState::default();

        // Held, but not yet past the threshold: still on step a.
        let mut held = input(&state, &[], DONE_HOLD_SECS - 0.1);
        held.done_held = true;
        assert!(r.tick(&held).banner.unwrap().contains("step a"));

        // Crossing the threshold advances to b...
        let mut held2 = input(&state, &[], 0.2);
        held2.done_held = true;
        assert!(r.tick(&held2).banner.unwrap().contains("step b"));

        // ...and holding *through* without releasing does NOT immediately finish b (one fire/press).
        let mut still = input(&state, &[], HOLD_FRAME);
        still.done_held = true;
        assert!(r.tick(&still).banner.unwrap().contains("step b"), "must release before re-firing");

        // Release, then hold again -> advances to c.
        idle_tick(&mut r, &state); // release
        assert!(done_tick(&mut r, &state).banner.unwrap().contains("step c"));
    }

    #[test]
    fn auto_finish_via_log_contains_predicate() {
        let guide = Guide::new("g")
            .step("wait", "wait for the marker")
            .done_when(log_contains("FSM live"))
            .step("next", "done");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();

        // No marker yet -> stays.
        assert!(idle_tick(&mut r, &state).banner.unwrap().contains("wait for the marker"));
        // A non-matching line doesn't trip it.
        assert!(
            r.tick(&input(&state, &["unrelated".into()], 0.1)).banner.unwrap().contains("wait")
        );
        // The marker (seen on an earlier frame) trips it.
        let lines = ["session-probe: FSM live @frame 51".to_string()];
        assert!(r.tick(&input(&state, &lines, 0.1)).banner.unwrap().contains("done"));
    }

    #[test]
    fn auto_finish_via_state_predicate_and_after_secs() {
        let guide = Guide::new("g")
            .step("load", "load a save")
            .done_when(game_state_is(GameState::InGame))
            .step("dwell", "wait 2s")
            .done_when(after_secs(2.0))
            .step("end", "end");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);

        let booting = RigState::default();
        assert!(idle_tick(&mut r, &booting).banner.unwrap().contains("load a save"));
        // Transition to InGame -> auto-advances to the dwell step.
        let ingame = RigState { game_state: GameState::InGame, ..Default::default() };
        assert!(idle_tick(&mut r, &ingame).banner.unwrap().contains("wait 2s"));
        // Dwell hasn't elapsed yet.
        assert!(r.tick(&input(&ingame, &[], 1.0)).banner.unwrap().contains("wait 2s"));
        // Crossing 2s total elapsed advances to end.
        assert!(r.tick(&input(&ingame, &[], 1.2)).banner.unwrap().contains("end"));
    }

    #[test]
    fn branch_on_state_chooses_the_target_step() {
        // A branching step: if captured -> "captured", else (manual override) -> "retry".
        let build = || {
            Guide::new("g")
                .step("host", "host a session")
                .done_when(lobby_is(LobbyState::TryToCreateSession))
                .branch(|ctx| {
                    if ctx.state.lobby_state == LobbyState::TryToCreateSession {
                        Advance::To("captured")
                    } else {
                        Advance::To("retry")
                    }
                })
                .default_branch(Advance::To("retry"))
                .step("captured", "captured the transition")
                .step("retry", "try a summon sign")
        };

        // Auto-finish path: the FSM moved -> branch sends us to "captured".
        let mut r = GuideRunner::start(build(), Role::Solo, HINTS);
        let creating = RigState { lobby_state: LobbyState::TryToCreateSession, ..Default::default() };
        assert!(idle_tick(&mut r, &creating).banner.unwrap().contains("captured the transition"));

        // Manual-override path: FSM never moved, tester holds done -> branch sends us to "retry".
        let mut r2 = GuideRunner::start(build(), Role::Solo, HINTS);
        let none = RigState::default();
        idle_tick(&mut r2, &none); // show the host step
        assert!(done_tick(&mut r2, &none).banner.unwrap().contains("try a summon sign"));
    }

    #[test]
    fn skip_serial_step_goes_to_next() {
        let mut r = GuideRunner::start(linear_guide(), Role::Solo, HINTS);
        let state = RigState::default();
        idle_tick(&mut r, &state); // step a
        assert!(skip_tick(&mut r, &state).banner.unwrap().contains("step b"));
    }

    #[test]
    fn skip_branching_step_takes_its_default_branch() {
        let guide = Guide::new("g")
            .step("host", "host")
            .done_when(lobby_is(LobbyState::TryToCreateSession))
            .branch(|_| Advance::To("captured"))
            .default_branch(Advance::To("retry"))
            .step("captured", "captured")
            .step("retry", "retry");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        idle_tick(&mut r, &state);
        // Skip uses default_branch -> "retry", NOT the branch closure's "captured".
        assert!(skip_tick(&mut r, &state).banner.unwrap().contains("retry"));
    }

    #[test]
    fn skip_dead_end_terminal_step_jumps_to_done() {
        // A single terminal step: skipping it (no next) ends the guide cleanly.
        let guide = Guide::new("g").step("only", "the only step");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        idle_tick(&mut r, &state);
        let end = skip_tick(&mut r, &state);
        assert_eq!(end.banner, None);
        assert!(end.finished_now, "skipping the last step reaches the done toast");
    }

    #[test]
    fn skip_fires_once_per_press_not_while_held() {
        let mut r = GuideRunner::start(linear_guide(), Role::Solo, HINTS);
        let state = RigState::default();
        idle_tick(&mut r, &state); // step a
        // Hold skip across two frames: advances once (a -> b), then stays on b while still held.
        let mut held = input(&state, &[], 1.0 / 60.0);
        held.skip_held = true;
        assert!(r.tick(&held).banner.unwrap().contains("step b"));
        let mut held2 = input(&state, &[], 1.0 / 60.0);
        held2.skip_held = true;
        assert!(r.tick(&held2).banner.unwrap().contains("step b"), "held skip doesn't re-fire");
    }

    #[test]
    fn role_filtering_shows_only_matching_or_untagged_steps() {
        let build = || {
            Guide::new("g")
                .step("both1", "both: boot")
                .step("host", "host: open world")
                .role(Role::Host)
                .step("join", "join: join world")
                .role(Role::Join)
                .step("both2", "both: confirm")
        };

        // Host machine: sees both1 -> host -> both2 (skips the join step entirely).
        let mut h = GuideRunner::start(build(), Role::Host, HINTS);
        let state = RigState::default();
        assert!(idle_tick(&mut h, &state).banner.unwrap().contains("both: boot"));
        assert!(done_tick(&mut h, &state).banner.unwrap().contains("host: open world"));
        assert!(done_tick(&mut h, &state).banner.unwrap().contains("both: confirm"));

        // Join machine: sees both1 -> join -> both2 (skips the host step).
        let mut j = GuideRunner::start(build(), Role::Join, HINTS);
        assert!(idle_tick(&mut j, &state).banner.unwrap().contains("both: boot"));
        assert!(done_tick(&mut j, &state).banner.unwrap().contains("join: join world"));
        assert!(done_tick(&mut j, &state).banner.unwrap().contains("both: confirm"));

        // Solo machine: sees only the untagged steps both1 -> both2.
        let mut s = GuideRunner::start(build(), Role::Solo, HINTS);
        assert!(idle_tick(&mut s, &state).banner.unwrap().contains("both: boot"));
        assert!(done_tick(&mut s, &state).banner.unwrap().contains("both: confirm"));
    }

    #[test]
    fn a_guide_with_no_visible_steps_starts_idle() {
        // Every step tagged for another role: this machine has nothing to do — idle, no toast.
        let guide = Guide::new("g").step("h", "host only").role(Role::Host);
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let out = idle_tick(&mut r, &RigState::default());
        assert_eq!(out.banner, None, "nothing to show on this machine");
        assert!(!out.finished_now, "an empty-for-this-role guide doesn't toast");
        assert!(!out.stub);
    }

    #[test]
    fn stub_step_renders_pending_and_advances_on_skip_without_auto_finish() {
        let guide = Guide::new("g")
            .step("real", "do a real thing")
            .step("pending", "verify the synced thing")
            .stub("pending the sync core")
            .step("end", "end");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        // A state that WOULD satisfy a predicate has no effect on a stub (it has none).
        let loaded = RigState { game_state: GameState::InGame, players: 4, ..Default::default() };

        // Finish the real step (manual) -> lands on the stub.
        let on_stub = done_tick(&mut r, &loaded);
        let banner = on_stub.banner.unwrap();
        assert!(banner.contains("[PENDING: pending the sync core]"), "stub shows a pending marker");
        assert!(banner.contains("verify the synced thing"), "stub still shows its doc text");
        assert!(on_stub.stub, "TickResult flags the stub so the renderer can grey it");

        // The stub doesn't auto-finish however much time/state passes; skip advances it.
        assert!(idle_tick(&mut r, &loaded).stub);
        assert!(skip_tick(&mut r, &loaded).banner.unwrap().contains("end"));
    }

    #[test]
    fn an_all_stub_guide_still_reaches_the_done_toast() {
        // Documentation-only guide (every step a stub): the tester is never trapped — skip walks it
        // straight to the done toast.
        let guide = Guide::new("doc")
            .step("s1", "future step one")
            .stub("rung-4")
            .step("s2", "future step two")
            .stub("rung-5");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        assert!(idle_tick(&mut r, &state).banner.unwrap().contains("future step one"));
        assert!(skip_tick(&mut r, &state).banner.unwrap().contains("future step two"));
        assert!(skip_tick(&mut r, &state).finished_now, "skipping the last stub completes the guide");
    }

    #[test]
    fn predicate_and_or_combinators() {
        let state_a = RigState { players: 2, ..Default::default() };
        let log = VecDeque::from(["hello world".to_string()]);
        let ctx = PredicateCtx { state: &state_a, step_elapsed_secs: 5.0, step_log: &log };

        assert!(players_at_least(2).and(log_contains("hello")).check(&ctx));
        assert!(!players_at_least(3).and(log_contains("hello")).check(&ctx));
        assert!(players_at_least(3).or(after_secs(4.0)).check(&ctx), "or short-circuits to the dwell");
        assert!(!players_at_least(3).or(after_secs(9.0)).check(&ctx));
    }

    #[test]
    fn non_finite_delta_does_not_advance_timers() {
        let guide = Guide::new("g").step("dwell", "wait 1s").done_when(after_secs(1.0)).step("end", "end");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        // A NaN/inf frame is treated as 0 elapsed, so the dwell predicate can't trip on garbage.
        assert!(r.tick(&input(&state, &[], f32::NAN)).banner.unwrap().contains("wait 1s"));
        assert!(r.tick(&input(&state, &[], f32::INFINITY)).banner.unwrap().contains("wait 1s"));
        // A real 1.1s frame then advances.
        assert!(r.tick(&input(&state, &[], 1.1)).banner.unwrap().contains("end"));
    }

    #[test]
    fn unknown_branch_target_ends_cleanly_without_panic() {
        let guide = Guide::new("g").step("a", "a").branch(|_| Advance::To("does-not-exist")).step("b", "b");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        idle_tick(&mut r, &state);
        let end = done_tick(&mut r, &state);
        assert!(end.finished_now, "an unknown To target degrades to Done");
    }

    #[test]
    fn banners_are_auto_coloured_stably_with_a_defined_stub_colour() {
        let guide = Guide::new("g")
            .step("alpha", "first")
            .step("beta", "second")
            .step("doc", "pending step")
            .stub("not yet");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();

        // A step's colour is a palette hue and is stable frame to frame (no flicker).
        let a1 = idle_tick(&mut r, &state).color;
        let a2 = idle_tick(&mut r, &state).color;
        assert_eq!(a1, a2, "a step's colour must not change between frames");
        assert!(crate::palette::peer_color_for_id(fnv1a("alpha")) == a1, "alpha gets its keyed hue");
        assert_ne!(a1, PENDING_BANNER_COLOR, "a live step is not the pending colour");

        // The next step gets its own keyed hue (here a distinct one).
        let b = done_tick(&mut r, &state).color;
        assert_eq!(b, crate::palette::peer_color_for_id(fnv1a("beta")));

        // The stub uses the fixed, defined pending colour, not a per-step hue.
        let s = done_tick(&mut r, &state);
        assert!(s.stub);
        assert_eq!(s.color, PENDING_BANNER_COLOR, "a stub banner uses the defined pending colour");
    }

    #[test]
    fn lobby_and_protocol_state_round_trip_from_u32() {
        // Every named variant must round-trip through its own discriminant, so a renumber/transpose
        // between the enum definition and `from_u32` is caught here rather than silently mismapping a
        // live SDK state.
        use LobbyState as L;
        for v in [
            L::None, L::TryToCreateSession, L::FailedToCreateSession, L::Host, L::TryToJoinSession,
            L::FailedToJoinSession, L::Client, L::OnLeaveSession, L::FailedToLeaveSession,
        ] {
            assert_eq!(L::from_u32(v as u32), v, "LobbyState {v:?} must round-trip");
        }
        use ProtocolState as P;
        for v in [
            P::None, P::JoinCheck, P::WaitInitData, P::WaitReloadWait, P::WaitReload, P::WaitReload2,
            P::Ingame, P::WaitReentryToMap,
        ] {
            assert_eq!(P::from_u32(v as u32), v, "ProtocolState {v:?} must round-trip");
        }
        assert_eq!(L::from_u32(999), L::Unknown);
        assert_eq!(P::from_u32(42), P::Unknown);
    }

    #[test]
    fn stale_log_does_not_carry_into_the_next_step() {
        // A marker emitted while on step "a" must NOT satisfy step "b"'s log_contains predicate:
        // `advance` clears the per-step log buffer, so each step only sees lines from its own window.
        let guide = Guide::new("g").step("a", "a").step("b", "b").done_when(log_contains("marker"));
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        let marker = vec!["...marker...".to_string()];
        // Emit "marker" during step a, then manually advance to b.
        r.tick(&input(&state, &marker, 1.0 / 60.0));
        assert!(done_tick(&mut r, &state).banner.unwrap().contains("b"));
        // No new lines: the stale "marker" must not auto-finish b.
        let still = idle_tick(&mut r, &state);
        assert!(still.banner.as_ref().unwrap().contains("b"), "stale log must not carry forward");
        assert!(!still.finished_now);
        // A fresh "marker" on b does finish it.
        assert!(r.tick(&input(&state, &marker, 1.0 / 60.0)).finished_now);
    }

    #[test]
    fn skip_escapes_a_self_looping_auto_finish() {
        // Pathological step: predicate always true + branch re-enters itself, so it auto-finishes into
        // itself every frame. Skip (above auto in precedence) must still advance it, so "never trap" holds.
        let guide = Guide::new("g")
            .step("loop", "loops")
            .done_when(after_secs(0.0))
            .branch(|_| Advance::To("loop"))
            .step("end", "end");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        assert!(idle_tick(&mut r, &state).banner.unwrap().contains("loops"));
        assert!(idle_tick(&mut r, &state).banner.unwrap().contains("loops"), "auto-finish self-loops");
        // Skip escapes despite the auto-finish firing the same frame.
        assert!(skip_tick(&mut r, &state).banner.unwrap().contains("end"));
    }

    // --- Choice / feedback modal --------------------------------------------------------------------

    /// A choice-modal input frame: nav up/down, confirm, and the current note buffer. Defaults to a
    /// quiet frame (no nav, no confirm, empty note) over a borrowed state.
    fn choice_input<'a>(
        state: &'a RigState,
        up: bool,
        down: bool,
        confirm: bool,
        note: &'a str,
    ) -> GuideInput<'a> {
        GuideInput {
            delta: 1.0 / 60.0,
            state,
            new_log_lines: &[],
            done_held: false,
            skip_held: false,
            choice: ChoiceInput { up, down, confirm, note },
        }
    }

    fn three_option_guide() -> Guide {
        // A choice step branching three ways, then the three landing steps.
        Guide::new("g")
            .step("pick", "Pick one")
            .choice(&[
                ("Alpha", Advance::To("a")),
                ("Bravo", Advance::To("b")),
                ("Charlie", Advance::To("c")),
            ])
            .default_branch(Advance::To("c"))
            .step("a", "landed alpha")
            .step("b", "landed bravo")
            .step("c", "landed charlie")
    }

    #[test]
    fn choice_step_renders_a_modal_not_a_banner() {
        let mut r = GuideRunner::start(three_option_guide(), Role::Solo, HINTS);
        let state = RigState::default();
        let out = r.tick(&choice_input(&state, false, false, false, ""));
        assert!(out.banner.is_none(), "a choice step has no pinned banner — the modal replaces it");
        let view = out.choice.expect("a choice step surfaces a ChoiceView");
        assert_eq!(view.step_id, "pick");
        assert_eq!(view.prompt, "Pick one");
        assert_eq!(view.options, vec!["Alpha", "Bravo", "Charlie"]);
        assert_eq!(view.selected, 0, "selection starts on the first option");
        assert!(!view.note_enabled, "no .note() -> no free-form field");
        assert_eq!(view.skip_hint, HINTS.skip, "the modal passes through the skip hint");
        assert!(out.choice_made.is_none(), "nothing resolved yet");
    }

    #[test]
    fn choice_nav_wraps_in_both_directions() {
        let mut r = GuideRunner::start(three_option_guide(), Role::Solo, HINTS);
        let state = RigState::default();
        // down: 0 -> 1 -> 2 -> wraps to 0.
        assert_eq!(r.tick(&choice_input(&state, false, true, false, "")).choice.unwrap().selected, 1);
        assert_eq!(r.tick(&choice_input(&state, false, true, false, "")).choice.unwrap().selected, 2);
        assert_eq!(r.tick(&choice_input(&state, false, true, false, "")).choice.unwrap().selected, 0, "down wraps past the end");
        // up from 0 wraps to the last option.
        assert_eq!(r.tick(&choice_input(&state, true, false, false, "")).choice.unwrap().selected, 2, "up wraps past the start");
    }

    #[test]
    fn single_option_choice_nav_stays_put() {
        // The n==1 boundary of the wrap math: up and down both leave selection at 0 (no under/overflow).
        let guide = Guide::new("g").step("only", "one").choice(&[("Only", Advance::Next)]).step("end", "end");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        assert_eq!(r.tick(&choice_input(&state, false, false, false, "")).choice.unwrap().selected, 0);
        assert_eq!(r.tick(&choice_input(&state, true, false, false, "")).choice.unwrap().selected, 0, "up stays at 0 with one option");
        assert_eq!(r.tick(&choice_input(&state, false, true, false, "")).choice.unwrap().selected, 0, "down stays at 0 with one option");
    }

    #[test]
    fn a_second_choice_step_resets_selection_and_carries_no_stale_note() {
        // Two note-enabled choice steps: the second must start on option 0 (advance resets the index) and
        // carry NO note from the first (the engine reads the note fresh per resolve, never holding it).
        let guide = Guide::new("g")
            .step("first", "first")
            .choice(&[("A", Advance::Next), ("B", Advance::Next)])
            .note()
            .step("second", "second")
            .choice(&[("X", Advance::Next), ("Y", Advance::Next)])
            .note()
            .step("end", "end");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        r.tick(&choice_input(&state, false, true, false, "")); // first: down -> index 1
        let made1 = r.tick(&choice_input(&state, false, false, true, "looks off")).choice_made.unwrap();
        assert_eq!((made1.label, made1.note.as_str()), ("B", "looks off"));
        // second renders at option 0 (reset), and a confirm with an empty buffer logs no stale note.
        assert_eq!(r.tick(&choice_input(&state, false, false, false, "")).choice.unwrap().selected, 0, "second choice resets to option 0");
        let made2 = r.tick(&choice_input(&state, false, false, true, "")).choice_made.unwrap();
        assert_eq!((made2.label, made2.note.as_str()), ("X", ""), "no stale note carried from the first choice");
    }

    #[test]
    #[should_panic(expected = "supersedes")]
    fn choice_after_done_when_panics() {
        let _ = Guide::new("g").step("s", "x").done_when(after_secs(0.0)).choice(&[("A", Advance::Next)]);
    }

    #[test]
    #[should_panic(expected = "supersedes")]
    fn done_when_after_choice_panics() {
        let _ = Guide::new("g").step("s", "x").choice(&[("A", Advance::Next)]).done_when(after_secs(0.0));
    }

    #[test]
    #[should_panic(expected = "executable choice")]
    fn stub_after_choice_panics() {
        let _ = Guide::new("g").step("s", "x").choice(&[("A", Advance::Next)]).stub("nope");
    }

    #[test]
    #[should_panic(expected = "at least one option")]
    fn empty_choice_panics() {
        let _ = Guide::new("g").step("s", "x").choice(&[]);
    }

    #[test]
    fn confirm_resolves_the_selected_options_advance_and_emits_the_event() {
        let mut r = GuideRunner::start(three_option_guide(), Role::Solo, HINTS);
        let state = RigState::default();
        // Move to "Bravo" (index 1), then confirm -> branches to its To("b").
        r.tick(&choice_input(&state, false, true, false, ""));
        let out = r.tick(&choice_input(&state, false, false, true, ""));
        let made = out.choice_made.expect("confirm emits a choice_made event");
        assert_eq!(made.step_id, "pick");
        assert_eq!(made.label, "Bravo", "the chosen option's label is captured");
        assert_eq!(made.note, "", "no free-form note -> empty");
        assert_eq!(out.banner.unwrap(), {
            // landed on "b" with the auto-appended hints
            "landed bravo\n(hold DONE = done, SKIP = skip)".to_string()
        });
    }

    #[test]
    fn choice_made_event_fires_exactly_once() {
        let mut r = GuideRunner::start(three_option_guide(), Role::Solo, HINTS);
        let state = RigState::default();
        assert!(r.tick(&choice_input(&state, false, false, true, "")).choice_made.is_some(), "fires on resolve");
        assert!(r.tick(&choice_input(&state, false, false, false, "")).choice_made.is_none(), "not again next tick");
    }

    #[test]
    fn free_form_note_is_captured_and_surfaced() {
        let guide = Guide::new("g")
            .step("rate", "How did it look?")
            .choice(&[("Good", Advance::Next), ("Bad", Advance::Next)])
            .note()
            .step("end", "end");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        // The view advertises the note field.
        assert!(r.tick(&choice_input(&state, false, false, false, "")).choice.unwrap().note_enabled);
        // Confirm with a typed note -> it rides on the event.
        let made = r
            .tick(&choice_input(&state, false, false, true, "nameplate was 2px high"))
            .choice_made
            .expect("confirm with a note");
        assert_eq!(made.label, "Good");
        assert_eq!(made.note, "nameplate was 2px high");
    }

    #[test]
    fn a_choice_without_note_captures_no_note_even_if_the_buffer_is_nonempty() {
        // The note is captured only for a `.note()` step. Without it, a non-empty modal-input buffer
        // (which can linger in the cross-thread channel) must NOT leak onto the answer — core's capture
        // doesn't lean on the overlay having cleared the buffer.
        let mut r = GuideRunner::start(three_option_guide(), Role::Solo, HINTS); // no `.note()`
        let state = RigState::default();
        r.tick(&choice_input(&state, false, false, false, "stale buffer text"));
        let made = r
            .tick(&choice_input(&state, false, false, true, "stale buffer text"))
            .choice_made
            .expect("confirm resolves");
        assert_eq!(made.note, "", "a no-note choice carries no note regardless of the buffer");
    }

    #[test]
    fn skip_logs_skipped_and_takes_the_default_branch() {
        // A note-enabled choice with a non-empty buffer, to prove a skip carries NO note even so — a
        // skip is a bail-out, not an answer (and the out-of-band buffer may hold a prior step's text).
        let guide = Guide::new("g")
            .step("pick", "Pick one")
            .choice(&[("Alpha", Advance::To("a")), ("Bravo", Advance::To("b"))])
            .note()
            .default_branch(Advance::To("c"))
            .step("a", "landed alpha")
            .step("b", "landed bravo")
            .step("c", "landed charlie");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        // Show the modal, then skip via the chord (rising edge: released frame first).
        r.tick(&choice_input(&state, false, false, false, ""));
        let mut skip = choice_input(&state, false, false, false, "still typing");
        skip.skip_held = true;
        let out = r.tick(&skip);
        let made = out.choice_made.expect("skip still captures the answer — never thrown away");
        assert_eq!(made.label, "skipped", "a skip is logged as 'skipped'");
        assert_eq!(made.note, "", "a skip carries no note, even with a non-empty buffer");
        // default_branch(To("c")) -> landed charlie (not the selected/first option's To("a")).
        assert!(out.banner.unwrap().contains("landed charlie"), "skip takes the default branch");
    }

    #[test]
    fn choice_step_is_always_escapable_via_skip_even_without_confirm() {
        // never-trap: a choice with options that loop back to itself can't trap the tester — skip escapes.
        let guide = Guide::new("g")
            .step("loop", "loops?")
            .choice(&[("Stay", Advance::To("loop"))])
            .default_branch(Advance::To("out"))
            .step("out", "escaped");
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        // Confirming "Stay" re-enters the same modal...
        r.tick(&choice_input(&state, false, false, false, ""));
        assert!(r.tick(&choice_input(&state, false, false, true, "")).choice.is_some(), "Stay loops back to the modal");
        // ...but a skip still escapes to the default branch.
        let mut skip = choice_input(&state, false, false, false, "");
        skip.skip_held = true;
        assert!(r.tick(&skip).banner.unwrap().contains("escaped"), "skip always escapes a choice");
    }

    #[test]
    fn confirming_a_choice_to_done_reaches_the_toast_and_logs_the_answer() {
        let guide = Guide::new("g").step("last", "done?").choice(&[("Yes", Advance::Done)]);
        let mut r = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        r.tick(&choice_input(&state, false, false, false, ""));
        let out = r.tick(&choice_input(&state, false, false, true, ""));
        assert!(out.finished_now, "a choice that advances to Done fires the done toast");
        assert_eq!(out.choice_made.expect("answer logged on the completing tick").label, "Yes");
        assert!(out.choice.is_none() && out.banner.is_none());
    }

    #[test]
    fn role_filtering_skips_a_choice_tagged_for_another_machine() {
        // A choice step tagged Host is invisible to a Solo runner — role filtering applies to choices too.
        let guide = Guide::new("g")
            .step("intro", "both")
            .step("host-pick", "host only")
            .role(Role::Host)
            .choice(&[("X", Advance::Next)])
            .step("end", "end");
        let mut s = GuideRunner::start(guide, Role::Solo, HINTS);
        let state = RigState::default();
        assert!(idle_tick(&mut s, &state).banner.unwrap().contains("both"));
        // Manual done on the untagged intro skips the host choice entirely, landing on "end".
        assert!(done_tick(&mut s, &state).banner.unwrap().contains("end"));
    }

    #[test]
    fn done_chord_does_not_advance_a_choice_step() {
        // The done/skip chords are normal-step controls; on a choice step a held done is inert (you
        // confirm a selection or skip instead). Guards that holding done can't bypass the modal.
        let mut r = GuideRunner::start(three_option_guide(), Role::Solo, HINTS);
        let state = RigState::default();
        r.tick(&choice_input(&state, false, false, false, ""));
        let mut done = choice_input(&state, false, false, false, "");
        done.done_held = true;
        done.delta = HOLD_FRAME;
        assert!(r.tick(&done).choice.is_some(), "a completed done hold does not resolve a choice step");
    }
}
