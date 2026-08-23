# Local Two-Player Rig

`scripts/local-duo.sh` stages a faster two-player feedback loop on the CachyOS desktop. It runs two
real ELDEN RING clients under Proton, but renders P2 into a tiny displayless gamescope backend. P1 is
a small visible window by default and can also be displayless. This removes the Steam Deck transfer,
second-device startup, physical input, and manual “can I see the peer?” observation from most RE
iterations.

It is not a server-mode approximation. Each side has a distinct Steam account, Steam home, library,
game installation, Proton prefix, save, config, process tag, nested display, and log. Both clients run
the real game simulation and Steam networking. That makes it a strong development loop, while the
desktop plus Deck remains the acceptance gate for machine-boundary and real-hardware behavior.

P2 also uses Steam's internal `-master_ipc_name_override` so its client and the Steamworks API do not
fall back to P1's identity. This mechanism is intentionally part of the CachyOS evaluation gate because
Valve does not document it as a supported public interface and Steam client updates can change it.

## Requirements

- CachyOS/Linux with `steam`, `gamescope`, Python 3, and `python-xlib`.
- Two Steam accounts that may run ELDEN RING concurrently, with a usable license/copy for each.
- Enough CPU, RAM, and VRAM for two clients. Defaults are 960x540 at 30 Hz for P1 and displayless
  640x360 at 30 Hz for P2.
- The normal P1 install plus a separate P2 install. On btrfs, the helper can make the latter as a
  cheap copy-on-write reflink.

## One-Time Setup

```bash
scripts/local-duo.sh setup
```

The command creates a machine-local config at
`~/.config/unseamless-coop/local-duo.env`. If P1 is not in the default Steam library, uncomment and
set `UNSEAMLESS_DUO_P1_LIBRARY` there. Other supported overrides are visible at the top of
`scripts/local-duo.sh`.

Next:

1. Put the printed P1 wrapper command in the main account's ELDEN RING launch options.
2. Run `scripts/local-duo.sh steam-p2`, sign into the second account once, add the P2 library, and
   put the printed P2 wrapper command in that account's launch options.
3. Install ELDEN RING into that library, or run `scripts/local-duo.sh clone-p2`, then let P2 Steam
   verify it once.
4. Launch each account once manually if Steam needs to finish its Proton prefix or Cloud/save setup.
5. Run `scripts/local-duo.sh check`.

The two launch-option commands differ only by the stable `p1`/`p2` role. The wrapper exports that
role into the game process, which lets `status`, `kill`, and input injection target one instance
without touching unrelated Wine games.

## Automated Loop

```bash
scripts/local-duo.sh cycle
```

`cycle` performs one build, safely applies the same artifact to two independent game directories,
writes `auto_session=host` for P1 and `auto_session=join` for P2, launches each isolated Steam
client, clears the offline dialogs inside each nested display, enters the latest save, and waits for
the automatic session actions. It then evaluates both current-run logs.

The pass condition is deliberately stronger than `players=2`: each log must report at least one
active remote phantom. On failure the command distinguishes “the game rosters reached two but no
remote `ChrIns` appeared” from “the roster never reached two.” Both logs are copied to
`~/.local/share/unseamless-duo/evidence/` for immediate comparison.

For iteration after a build made elsewhere:

```bash
scripts/local-duo.sh cycle --no-build
scripts/local-duo.sh verify
scripts/local-duo.sh logs p1 -f
scripts/local-duo.sh logs p2 -f
scripts/local-duo.sh status
scripts/local-duo.sh kill all
scripts/local-duo.sh restore
```

The current session driver is process-lifetime one-shot, so a fresh session attempt still requires a
game restart. The improvement here is that the restart, deployment, input, two-sided observation,
and evidence collection are one unattended command. A future in-process debug action queue could
make the pair warm-retry without restarting, but adding that before the local pair is proven would
mix a new control surface into the behavior being measured.

## What Still Needs CachyOS Validation

- Steam permits both isolated clients to stay online concurrently on this desktop.
- The displayless gamescope backend exposes a nested Xwayland display that accepts XTEST input.
- Both independent prefixes see the expected Steam identities and test saves.
- GPU/VRAM use is stable at the default low resolutions.
- `cycle` reaches the known `players=2, join_wait=true` baseline before a spawn fix, then becomes a
  passing remote-phantom assertion once that fix lands.
