---
name: steam-deck
description: Drive a REMOTE rig (a Steam Deck / second Linux machine) over SSH as player 2 for two-player networking tests — apply the mod, seed config + save, launch/kill the game, and click into gameplay, all from this PC via scripts/deck.sh. Use when setting up or running the Steam Deck second player, testing rung-3 / real co-op, or anything "on the Deck". TRIGGER on "steam deck", "second player", "remote rig", "deck.sh", "two real machines", "apply the mod on the deck", "player 2".
---

# Remote rig over SSH (the Steam Deck second player)

The local PC rig ([`/test-loop`](../test-loop/SKILL.md), `scripts/rig.sh`) is **player 1**. This skill is
**player 2**: a Steam Deck (on a throwaway Steam account) or any Linux box, driven entirely over SSH so
you can run the **two-player networking tests** that rungs 2/4 and rung 3 need (see
[FRIEND-TEST-RUNBOOK.md](../../../docs/FRIEND-TEST-RUNBOOK.md)).

**Model — the Deck is nearly stateless.** We build the DLL **here** (the cross-compile toolchain lives
here; the Deck has none), then rsync the artifacts + config + save to the Deck and drive it through a
small on-Deck helper. No git clone, no toolchain, no backup/restore on the Deck (it's a throwaway
account — nothing to protect, unlike the local rig). The Deck holds only: the helper script, the applied
mod files, the config, and the save. Everything is re-pushable, so a wiped Deck is one `setup` + `apply`
away from ready.

## Pieces

- **`scripts/deck.sh`** — the local driver (run it here). Builds, rsyncs, and drives the Deck over SSH.
- **`scripts/deck/deck-remote.sh`** — the on-Deck helper. Pushed by `setup`/`apply` (re-pushed every
  apply, so it's never stale), owns all Deck-path resolution + process control. You rarely call it
  directly; `deck.sh` invokes its verbs over SSH.

## Prerequisites

- **SSH key auth** to the Deck (set up out of band — `deck.sh` uses `BatchMode`, no passwords).
- **`DECK_HOST=user@host`** exported. Required by every verb. **This network's Deck is at a static
  `10.10.1.57`**, so `DECK_HOST=deck@10.10.1.57` (the SteamOS default user is `deck`; adjust if changed).
- The game **installed** on the Deck (we don't install ELDEN RING, just our mod over it).
- For `launch`: the Deck **logged into Game Mode** (a running Steam). For `dismiss`/click-into-gameplay:
  **ydotool** available on the Deck (see "ydotool on SteamOS" below).

## Verbs (all `scripts/deck.sh <verb>`)

| Verb | What it does |
|---|---|
| `setup` | Push the on-Deck helper, **mark the host a throwaway rig** (the guard `apply` requires), and report deps. Run once per Deck (and after editing the helper). `deck.sh paths` shows the resolved paths. |
| `apply [--release] [--no-build] [--keep-config]` | Build the DLL here (default `diag`), rsync it + our launcher + the seed config, install on the Deck (dll→`dinput8.dll`, launcher→`start_protected_game.exe`, config, marker; empties `mods/`). Safe to repeat. |
| `seed-save [file]` | Push a save into the Deck's Proton prefix as `ER0000.<ext>` (ext from the seed config's `file_extension`). Default source: the local rig's seeded save; or pass a file. |
| `launch` | Start the game on the Deck via the running Steam (`steam://rungameid/<appid>`). Our applied launcher starts it **outside EAC** with the marker, same as the local rig. |
| `dismiss` | ydotool the startup popups away and select Continue → **lands in gameplay** (same Enter-spam approach as the local rig's `dismiss`). |
| `kill` | Stop the game + launcher (bracket-trick `pkill`, SIGTERM→SIGKILL). |
| `cycle [apply-opts]` | `apply → kill → launch → wait-for-framework → dismiss`. The solo-on-Deck smoke test. |
| `log [-f]` | Print/follow the latest Deck log over SSH. |
| `pull-logs [dest]` | rsync the Deck's `unseamless-coop/logs/` back here (default `.deck-logs/`) so you can read results. |
| `status` / `paths` / `check` | Applied state / resolved Deck paths / remote deps. |
| `shell` | Open an interactive SSH shell on the Deck. |

## The two-player loop (what this is for)

Player 1 is the local rig, player 2 is the Deck. The seed config (`scripts/rig/seed-config.toml`) is
**shared** — `deck.sh` and `rig.sh` both apply it, so both machines get the **same password** (the
pairing key) and settings by default. Typical run:

```bash
# Player 2 (Deck) — once:
DECK_HOST=deck@10.10.1.57 scripts/deck.sh setup
DECK_HOST=deck@10.10.1.57 scripts/deck.sh seed-save        # same character as the rig (skip intros)

# Each test, both machines on the same build:
scripts/rig.sh apply                                            # player 1 (this PC)
DECK_HOST=deck@10.10.1.57 scripts/deck.sh apply            # player 2 (Deck)
scripts/rig.sh cycle                                            # P1 into gameplay
DECK_HOST=deck@10.10.1.57 scripts/deck.sh cycle            # P2 into gameplay
# In-game: one Opens a world, the other Joins (rung-4 lobby + rung-2 side-channel link).
# Read both logs:  scripts/rig.sh log -f   |   scripts/deck.sh pull-logs && less .deck-logs/*
```

For the **rung-3 create-drive test** (the current frontier — does a real peer unblock create?), set the
probes on **both** machines and follow
[FRIEND-TEST-RUNBOOK.md](../../../docs/FRIEND-TEST-RUNBOOK.md) > "Part B — Rung-3 create-drive test".
Because `deck.sh apply` ships the same `seed-config.toml`, editing that file once flips the probes on both
sides.

## Deck-specific setup notes

### Paths (SteamOS defaults, override per Deck)
`deck.sh`/`deck-remote.sh` default to the SteamOS layout. Override via env when they differ:

- `DECK_GAME_DIR` — default `<remote-home>/.local/share/Steam/steamapps/common/ELDEN RING/Game` (derived
  from the resolved remote `$HOME`, so it's right for any Deck user). **On an SD card** it's
  `/run/media/<user>/<uuid>/steamapps/common/ELDEN RING/Game` — set `DECK_GAME_DIR` **and**
  `DECK_SAVE_ROOT` (the Proton prefix/compatdata is on the same SD library, not under the internal
  `DECK_STEAM_ROOT`; setting only `DECK_GAME_DIR` would seed the save into the wrong prefix).
- `DECK_STEAM_ROOT` — default `~/.local/share/Steam`. Drives the compatdata/save root.
- `DECK_APPID` — `1245620`.
- `DECK_HELPER_DIR` — default `<home>/.local/share/unseamless-deck` (resolved via the remote `$HOME`).

`deck.sh paths` prints everything resolved — run it first on a new Deck to confirm.

### The save subdir (run the game once first)
ELDEN RING creates `…/EldenRing/<SteamID64>/` only after running once under an account, so on a fresh
throwaway account `seed-save` can't find the dir. Either **launch the game once** (let it reach the title,
which creates the profile), then `seed-save`; or pass **`DECK_STEAM_ID64=<id>`** to create/select the
subdir explicitly. `deck.sh seed-save` backs up any existing test save (`.deckbak`) before writing and
never touches a vanilla `.sl2`.

### Launch over SSH (needs Game Mode running)
Over SSH we don't inherit the Deck's graphical session, so `deck-remote.sh launch` lifts the session env
(`XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS`/`WAYLAND_DISPLAY`/`DISPLAY`/`XAUTHORITY`) out of the
**running Steam process's** `/proc/<pid>/environ` (newest match) and fires `steam://rungameid/<appid>` at
it, detached (`setsid`) so the closing SSH session can't kill the handoff. So Steam must already be
running (Game Mode logged in). If launch can't resolve a live session, it errors instead of pretending to
succeed. (`dismiss` lifts the same env so its ydotool socket guess is right.)

### ydotool on SteamOS (the click-into-gameplay piece)
`dismiss` injects key presses via **ydotool** (uinput-level virtual input, so it reaches the game under
gamescope). SteamOS has an immutable root and ships no ydotool, so seed it into the (writable) helper
dir — this keeps the Deck stateless-ish and survives SteamOS updates better than `pacman` on the
unlocked root:

1. Put a **statically-linked `ydotool` + `ydotoold`** in `~/.local/share/unseamless-deck/bin/` on the
   Deck (build static on this PC, or grab a static release; musl static binaries run on SteamOS).
2. Run `ydotoold` (needs `/dev/uinput` access — `sudo` once, or a udev rule granting the `deck` user;
   document whichever the Deck ends up using).
3. Point `dismiss` at it: `DECK_YDOTOOL_SOCKET=<socket>` and ensure the `bin/` dir is on `PATH` for the SSH
   command (or extend `deck-remote.sh` to prefer `$DECK_HELPER_DIR/bin`).

Until ydotool is set up, `dismiss` warns and no-ops; drive the menu by hand (or with the controller) that
run. The dismiss key is `DECK_DISMISS_KEY` (default 28 = Enter, same as the local rig); tune
`DECK_DISMISS_PRESSES`/`DECK_DISMISS_INTERVAL` if the Deck's popup timing differs.

> **This is the one part that needs the real hardware to finalize** — launch and ydotool can only be
> validated on a Deck in Game Mode. The file/apply/config/save/log/SSH plumbing is validated end-to-end
> against a plain headless Linux box (see below).

## Validate without a Deck

Everything except the actual Steam launch + ydotool works against any SSH box — point the env at a scratch
dir:

```bash
export DECK_HOST=user@somebox \
       DECK_GAME_DIR=/home/user/working/er/Game \
       DECK_HELPER_DIR=/home/user/working/unseamless-deck \
       DECK_SAVE_ROOT=/home/user/working/er/EldenRing \
       DECK_STEAM_ID64=76561190000000000
mkdir -p ... (a fake Game dir with a touch'd eldenring.exe)
scripts/deck.sh setup && scripts/deck.sh apply --no-build && scripts/deck.sh status
scripts/deck.sh seed-save /path/to/any/ER0000.uco
```

`launch`/`dismiss` fail gracefully there (no Steam/ydotool); `apply`/`seed-save`/`status`/`log`/`pull-logs`
all exercise the real code paths. (This is exactly how the tooling was first validated, against
`michael@10.10.1.100:/home/michael/working`.)

## Gotchas

- **`DECK_HOST` points the gun** — `apply` writes into `DECK_GAME_DIR` on whatever host you set. There's no
  backup (throwaway account), so double-check the host/dir on a new Deck with `deck.sh paths` before `apply`.
- **Build here, not there.** Always `apply` from this repo so the Deck runs the same commit as the rig.
  `apply --no-build` reuses the last local build (faster when both machines need the same artifact).
- **Spaces in paths** (`ELDEN RING`) are handled (rsync `--protect-args`); don't hand-quote `DECK_GAME_DIR`.
- **Logs come back via `pull-logs`** (or `log`/`log -f` to stream). The Deck's overlay **Export** button
  still works for a self-describing diagnostics bundle; grab it from the Deck if you want the scrubbed report.
- **diag build by default** (readable logs, like the rig). `apply --release` for a shipping-profile run.
