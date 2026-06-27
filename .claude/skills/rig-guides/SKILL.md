---
name: rig-guides
description: How to write an in-overlay rig-testing GUIDE for unseamless-coop — the authoring API (the fluent builder, ready-made finish predicates, role tagging, stub steps, the defaults). Use when adding or editing a guide in crates/unseamless-core/src/guide/guides.rs, or when someone asks to "make a guide" / "add a test guide" / "walk the tester through X". This is authoring only; the engine internals + wiring are docs/RIG-GUIDES.md.
---

# Writing a rig-testing guide

A **guide** is an ordered list of on-screen test steps. The engine pins the current step as a banner,
advances when its finish signal fires, optionally branches on the result, and ends with a "done
testing" toast. You write **only the steps and their finish conditions** — the engine auto-appends the
control hints, auto-colours the banners, filters by machine role, and handles skip/done/done-toast.
You never write rendering, input, colour, or boilerplate.

Guides live in `crates/unseamless-core/src/guide/guides.rs`. They're **debug-only** (stripped from
release). To add one: write a builder function, then add a `by_name` arm and a `NAMES` entry. Test on
the host with `scripts/test-core.sh`.

## The shape

```rust
fn my_guide() -> Guide {
    Guide::new("my-guide")
        .step("boot", "Boot to a loaded save.")
            .done_when(game_state_is(GameState::InGame))   // auto-finish; else it's manual
        .step("do-the-thing", "Trigger the behaviour under test, then watch the banner.")
        .step("confirm", "Confirm the effect happened.")
}
```

Then register it:

```rust
pub const NAMES: &[&str] = &[/* … */, "my-guide"];
pub fn by_name(name: &str) -> Option<Guide> {
    match name {
        // …
        "my-guide" => Some(my_guide()),
        _ => None,
    }
}
```

Run it with `[debug] guide = "my-guide"` in the config (or `scripts/rig/seed-config.toml`).

## Defaults (so a step is one line)

Each `.step(id, text)` defaults to **manual-finish, serial-next, all-roles, executable**. You only add
a modifier to opt out of a default. The modifiers chain after the `.step(...)` they apply to:

| Modifier | Effect |
|---|---|
| `.done_when(pred)` | **Auto-finish** when `pred` holds (instead of waiting for a manual press). Manual finish still works as an override. |
| `.role(Role::Host\|Join\|Solo)` | Show this step only on that machine's role. Untagged ⇒ shown to all. |
| `.branch(\|ctx\| …)` | On finish, choose the next step from the result (return an `Advance`). Default is serial `Next`. |
| `.default_branch(advance)` | Where **skip** sends this step (and the sensible default for a branching step). Default `Next`. |
| `.stub("reason")` | Mark a not-yet-executable step (renders as `[PENDING: reason]`; advances on done/skip, never auto). |

The `id` is a short stable string, unique within the guide; `.branch(...)`/`Advance::To` address steps
by it.

## Finish predicates (the auto path — preferred)

Pass one of these to `.done_when(...)`. They read a read-only context; compose with `.and` / `.or`.

| Predicate | Finishes when |
|---|---|
| `game_state_is(GameState::InGame)` | The coarse game lifecycle matches (e.g. a save is loaded). |
| `lobby_is(LobbyState::TryToCreateSession)` | The session lobby FSM equals that state. |
| `protocol_is(ProtocolState::Ingame)` | The session protocol FSM equals that state. |
| `players_at_least(2)` | At least N players are connected (a peer joined). |
| `after_secs(3.0)` | The step has shown for N seconds (a dwell timer). |
| `log_contains("session-probe: …->TryToCreateSession")` | Any log line **since this step started** contains the substring. |

Combine: `.done_when(lobby_is(LobbyState::Host).or(lobby_is(LobbyState::TryToCreateSession)))`.

Need something not listed? Add a constructor next to these in `guide.rs` returning a `Predicate`
(`Predicate::new(|ctx| …)` takes any closure over `ctx.state` / `ctx.step_elapsed_secs` /
`ctx.log_contains(...)`). Keep new predicates in core so they stay host-tested.

## Branching on a result

A branching step finishes (auto or manual) and then its `.branch` closure picks the next step:

```rust
.step("host", "Host a session via a multiplayer item.")
    .done_when(lobby_is(LobbyState::TryToCreateSession).or(log_contains("->TryToCreateSession")))
    .branch(|ctx| {
        if ctx.state.lobby_state == LobbyState::TryToCreateSession
            || ctx.log_contains("->TryToCreateSession")
        {
            Advance::To("captured")          // the transition fired
        } else {
            Advance::To("retry")             // manual override: it didn't move
        }
    })
    .default_branch(Advance::To("retry"))    // where SKIP goes
.step("captured", "Captured the transition.")
.step("retry", "Try a summon sign instead.")
```

`Advance` is `Next` (the following step), `To("id")` (a named step), or `Done` (end the guide). An
unknown `To` id ends the guide cleanly rather than panicking — but keep ids correct.

## Roles (two-player guides)

Tag steps so one committed guide drives both machines. Set `[debug] rig_role` to `host` / `join` on
each machine (default `solo`); each sees only its own steps plus the untagged shared ones.

```rust
.step("both-boot", "Both: load a save in the same area.").done_when(game_state_is(GameState::InGame))
.step("host-open", "HOST: Open World and place your sign.").role(Role::Host).done_when(lobby_is(LobbyState::Host))
.step("join-join", "JOINER: Join World.").role(Role::Join).done_when(lobby_is(LobbyState::Client))
.step("confirm", "Both: confirm you see each other.").done_when(players_at_least(2))
```

## Stub steps (commit a guide before it's executable)

When a step can't be auto-detected yet (the RE/feature hasn't landed), commit it as a **stub** so the
guide is living documentation now and revivable later:

```rust
.step("sync-check", "Verify the host's shared settings apply on the joiner.")
    .stub("pending the settings-sync core")
```

A stub renders as `[PENDING: pending the settings-sync core]` in a dimmed colour and advances on
done/skip — it never auto-finishes and never traps the tester. A guide that's **all** stubs still
reaches the done toast via skip. When the work lands, drop `.stub(...)` and add a `.done_when(...)`.

## Controls & rendering (you don't write these)

- The tester advances with **hold `L3 + D-pad Up`** (done) and skips with **press `L3 + D-pad Down`**
  (skip). The engine appends `(hold L3 + Up = done, L3 + Down = skip)` to every banner — don't write
  hints into your text.
- Colours are auto-assigned (a distinct per-step hue; a fixed dim colour for stubs). **Never** put a
  colour in a guide.
- Voice is **plain/diagnostic** (a guide is a debug tool, not gameplay) — terse, literal instructions.
  No ER lore tone, no raw mechanical values dressed up.

## Checklist

- [ ] Builder function + `by_name` arm + `NAMES` entry in `guide/guides.rs`.
- [ ] Prefer `.done_when(...)` (auto) over manual where a signal exists; manual is the fallback.
- [ ] Branching step has a `.default_branch(...)` (so skip is sensible).
- [ ] Role-tag steps for two-player guides; leave shared steps untagged.
- [ ] Stub anything not yet executable rather than leaving it out.
- [ ] `scripts/test-core.sh` green (the registry test builds every named guide).
