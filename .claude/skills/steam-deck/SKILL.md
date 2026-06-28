---
name: steam-deck
description: Drive a REMOTE rig (a Steam Deck / second Linux machine) over SSH as player 2 for two-player networking tests — apply the mod, seed config + save, launch/kill the game, and click into gameplay, all from this PC via scripts/deck.sh. Use when setting up or running the Steam Deck second player, testing rung-3 / real co-op, or anything "on the Deck". TRIGGER on "steam deck", "second player", "remote rig", "deck.sh", "two real machines", "apply the mod on the deck", "player 2".
---

# Remote rig over SSH (the Steam Deck — player 2)

The local PC rig (`scripts/rig.sh`, the [`/test-loop`](../test-loop/SKILL.md) skill) is **player 1**; the
Deck is **player 2** for two-player tests (see [FRIEND-TEST-RUNBOOK.md](../../../docs/FRIEND-TEST-RUNBOOK.md)).
You build the DLL here and push it; the Deck stays stateless (only the helper + applied mod/config/save).

- **`scripts/deck.sh`** — run this here; it builds, rsyncs, and drives the Deck over SSH.
- **`scripts/deck/deck-remote.sh`** — the on-Deck helper (`deck.sh` pushes + invokes it; don't call directly).

## Prerequisites

- **`DECK_HOST=user@host`** (required by every verb) + **`DECK_PORT`** if non-standard. This network's Deck:
  `export DECK_HOST=deck@10.10.1.57 DECK_PORT=2222`.
- **SSH key auth** to the Deck (no passwords; `deck.sh` uses `BatchMode`).
- ELDEN RING **installed** on the Deck. For `launch`/`dismiss`: the Deck **in Game Mode** (Steam running).

## Verbs (`scripts/deck.sh <verb>`)

| Verb | What it does |
|---|---|
| `setup` | One-time per Deck: push the helper, mark the host a throwaway rig (required by `apply`), seed the `uinput-tap` tapper, report deps. |
| `apply [--release] [--no-build] [--keep-config]` | Build the DLL (default `diag`) + rsync it, the launcher, and the seed config; install on the Deck (dll→`dinput8.dll`, launcher→`start_protected_game.exe`, config, marker; empties `mods/`). |
| `seed-save [file]` | Push a save into the Deck's Proton prefix as `ER0000.<ext>` (default source: the local rig's seeded save). |
| `launch` | Start the game via the Deck's running Steam (outside EAC, via the applied launcher). |
| `dismiss` | Tap Enter (`uinput-tap`) to clear the startup popups + select Continue → gameplay. |
| `kill` | Stop the game + launcher. |
| `cycle [apply-opts]` | `apply → kill → launch → wait-for-framework → dismiss`. The solo-on-Deck smoke test. |
| `log [-f]` | Print/follow the latest Deck log over SSH. |
| `pull-logs [dest]` | rsync the Deck's logs back here (default `.deck-logs/`). |
| `status` / `paths` / `check` | Applied state / resolved Deck paths / remote deps. |
| `seed-input` | (Re)build + push just the `uinput-tap` tapper (`setup`/`apply` already do this). |
| `shell` | SSH shell on the Deck. |

## Two-player run

`deck.sh` and `rig.sh` apply the **same** `scripts/rig/seed-config.toml`, so both machines share the
password + settings. Edit that file once to change both.

```bash
export DECK_HOST=deck@10.10.1.57 DECK_PORT=2222

# once per Deck:
scripts/deck.sh setup
scripts/deck.sh seed-save                 # after the game has run once (creates the save profile)

# each test:
scripts/rig.sh apply  && scripts/deck.sh apply
scripts/rig.sh cycle  && scripts/deck.sh cycle    # both into gameplay
# in-game: one Open World, the other Join world. Read logs:
scripts/rig.sh log -f                             # player 1
scripts/deck.sh pull-logs && less .deck-logs/*    # player 2
```

For the **rung-3 create-drive test**, set the probes in `seed-config.toml` and follow
[FRIEND-TEST-RUNBOOK.md](../../../docs/FRIEND-TEST-RUNBOOK.md) > "Part B — Rung-3 create-drive test".

## Paths & env (override per Deck)

Defaults derive from the resolved remote `$HOME`; override when they differ:

- `DECK_GAME_DIR` — default `<home>/.local/share/Steam/steamapps/common/ELDEN RING/Game`. **SD-card install:**
  set this **and** `DECK_SAVE_ROOT` to the card's library (`/run/media/<user>/<uuid>/steamapps/...`).
- `DECK_STEAM_ROOT` (default `<home>/.local/share/Steam`), `DECK_APPID` (`1245620`),
  `DECK_HELPER_DIR` (default `<home>/.local/share/unseamless-deck`), `DECK_SAVE_SRC` (local save file for `seed-save`).
- `DECK_STEAM_ID64` — set to create/select the save subdir on a fresh account (see below).
- Dismiss tuning: `DECK_DISMISS_PRESSES` (100), `DECK_DISMISS_KEY` (28=Enter), `DECK_DISMISS_INTERVAL_MS` (400).

Run `scripts/deck.sh paths` on a new Deck to confirm everything resolves.

## Notes

- **`apply` requires `setup` first** (the throwaway-rig mark) and writes into `DECK_GAME_DIR` with no backup
  — confirm the host/path with `deck.sh paths` before applying to a new Deck.
- **`seed-save` needs the save profile dir** (`…/EldenRing/<SteamID64>/`), which ELDEN RING only creates
  after running once under the account. Launch once first, or pass `DECK_STEAM_ID64=<id>`. It backs up any
  existing test save to `.deckbak` and never touches a `.sl2`.
- **`launch` needs Game Mode running** (it lifts the session env from the live Steam process and fires
  `steam://rungameid/<appid>`). It errors if no live session is found.
- **`dismiss` needs an active session** (writes `/dev/uinput`; no sudo). If a `cycle`'s dismiss fires before
  the popups appear (slow Proton cold-start), just run `scripts/deck.sh dismiss` again, or raise
  `DECK_DISMISS_PRESSES`.
- **diag build by default** (readable logs); `apply --release` for a shipping-profile run.
- **Build is local** — `gcc` (for `uinput-tap`) and the Rust cross-toolchain must be on this PC, not the Deck.

## Run the tooling against a non-Deck (no Steam/game)

All file/apply/config/save/log verbs work against any SSH box; point the env at a scratch dir:

```bash
export DECK_HOST=user@box DECK_GAME_DIR=/path/er/Game DECK_HELPER_DIR=/path/unseamless-deck \
       DECK_SAVE_ROOT=/path/er/EldenRing DECK_STEAM_ID64=76561190000000000
# create the fake Game dir with a touch'd eldenring.exe, then:
scripts/deck.sh setup && scripts/deck.sh apply --no-build && scripts/deck.sh status
```

`launch`/`dismiss` no-op there (no Steam / no writable `/dev/uinput`); the rest exercise the real paths.
