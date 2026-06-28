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
- **`DECK_HOST=user@host`** exported. Required by every verb. **This network's Deck is `deck@10.10.1.57`
  on SSH port `2222`** — so `DECK_HOST=deck@10.10.1.57 DECK_PORT=2222`. (`DECK_PORT` defaults to 22; it
  flows to both ssh and rsync. The SteamOS default user is `deck`.) Confirmed reachable; recon showed
  Steam present + running, `/dev/uinput` writable by `deck`, rsync/setsid/pgrep present, ELDEN RING at the
  internal default path. **`setup` is done** (helper + `uinput-tap` seeded, host marked throwaway).
- The game **installed** on the Deck (we don't install ELDEN RING, just our mod over it).
- For `launch`: the Deck **logged into Game Mode** (a running Steam). `dismiss` needs nothing extra — the
  static `uinput-tap` key-tapper is bundled and seeded by `setup`/`apply` (see "Click into gameplay" below).

## Verbs (all `scripts/deck.sh <verb>`)

| Verb | What it does |
|---|---|
| `setup` | Push the on-Deck helper, **mark the host a throwaway rig** (the guard `apply` requires), and report deps. Run once per Deck (and after editing the helper). `deck.sh paths` shows the resolved paths. |
| `apply [--release] [--no-build] [--keep-config]` | Build the DLL here (default `diag`), rsync it + our launcher + the seed config, install on the Deck (dll→`dinput8.dll`, launcher→`start_protected_game.exe`, config, marker; empties `mods/`). Safe to repeat. |
| `seed-save [file]` | Push a save into the Deck's Proton prefix as `ER0000.<ext>` (ext from the seed config's `file_extension`). Default source: the local rig's seeded save; or pass a file. |
| `launch` | Start the game on the Deck via the running Steam (`steam://rungameid/<appid>`). Our applied launcher starts it **outside EAC** with the marker, same as the local rig. |
| `dismiss` | Tap Enter via the bundled `uinput-tap` to clear popups + select Continue → **lands in gameplay** (no daemon; same Enter-spam idea as the local rig). |
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
export DECK_HOST=deck@10.10.1.57 DECK_PORT=2222     # this network's Deck

# Player 2 (Deck) — once:
scripts/deck.sh setup
scripts/deck.sh seed-save        # same character as the rig (skip intros)

# Each test, both machines on the same build:
scripts/rig.sh apply             # player 1 (this PC)
scripts/deck.sh apply            # player 2 (Deck)
scripts/rig.sh cycle             # P1 into gameplay
scripts/deck.sh cycle            # P2 into gameplay
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
succeed.

### Click into gameplay (`uinput-tap`, no daemon)
`dismiss` taps Enter to clear ELDEN RING's modal startup popups and select **Continue** → gameplay. It
uses **`uinput-tap`** — a tiny self-contained static key-tapper (`scripts/deck/uinput-tap.c`) that writes
`/dev/uinput` directly. No daemon, no socket (vs ydotool): one process creates a virtual keyboard, taps
the key N times, and exits. `setup`/`apply` build it here and rsync it to `~/.local/share/unseamless-deck/bin/`
(or `deck.sh seed-input` to (re)push just it). **No sudo:** SteamOS's `60-cecd-uinput.rules` grants the
active-session user an ACL on `/dev/uinput`, so the tapper opens it unprivileged while Game Mode is up.

**Build gotcha (handled, but know it):** this dev box is CachyOS on the **x86-64-v4** repo, so its glibc +
crt objects use AVX-512 the Deck's **Zen 2** APU lacks. `deck.sh` therefore builds `uinput-tap`
**dynamic** (`-march=x86-64 -std=gnu11`, so it binds the Deck's own CPU-correct glibc 2.41 at runtime) and
**strips `.note.gnu.property`** (else the Deck's loader rejects it with "CPU ISA level is lower than
required" from the v4 marking on CachyOS's crt). Validated on the Deck: device create + event write +
destroy all succeed as `deck`. If you ever move to a different build host, re-confirm the tapper runs there.

Tuning: `DECK_DISMISS_KEY` (default 28 = Enter), `DECK_DISMISS_PRESSES` (**100** — a ~40s window, longer
than the local rig's 30 on purpose, since the Deck is unattended and Proton's cold-start shows the popups
late; extra taps after gameplay are harmless), `DECK_DISMISS_INTERVAL_MS` (400). `dismiss` warns (doesn't
crash) if `/dev/uinput` isn't writable (no active session).

> **What still needs a live Game-Mode run to confirm:** the actual `launch` (Steam URL handoff) and that
> the `dismiss` taps land in ELDEN RING to reach gameplay. The file/apply/config/save/log/SSH plumbing —
> and the `uinput-tap` mechanism itself — are validated end-to-end (against the Deck and a headless box).

## Validate without a Deck

Everything except the actual Steam launch + uinput key injection works against any SSH box — point the env at a scratch
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

`launch`/`dismiss` fail gracefully there (no Steam; no writable /dev/uinput without a session); `apply`/`seed-save`/`status`/`log`/`pull-logs`
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
