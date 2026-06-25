# Title Screen Offline Presentation

How the game presents its "you're offline" state at boot, and how we plan to replace that with
our own. Two related targets, one shared substrate:

1. **Suppress / replace the network-error popups** that fire at the title screen when the game
   can't reach FromSoft's servers (which is always, for us — we run outside EAC).
2. **Restyle the bottom-right watermark** — the muted `App Ver. …` / `OFFLINE` block — into our
   own `unseamless-coop <version>` stamp, the way ERSC stamps `Seamless Co-op X.Y.Z` there.

This is a **research note**, not an implemented feature. Everything game-internal below is either
grounded in the pinned `fromsoftware-rs` SDK source (cited as such) or is a behavioral
observation/inference to confirm on the rig. Per [CLAUDE.md](../CLAUDE.md) > Clean-room hygiene:
we reimplement from observed behavior + the public SDK, never from ERSC's code (it's closed and
Themida-packed — there's nothing to copy here even if we wanted to).

> Why we own this at all: we launch the game **outside EAC** (`launcher` crate +
> `coop/guard.rs`), so the vanilla online login always fails and these popups always fire. ERSC
> hides them and brands the corner so the user sees a clean co-op title screen instead of three
> error boxes. Same goal here.

## A. The offline popups

### What actually appears

Three distinct things, from different layers — only the middle one is ours to suppress in-process:

- **(a) The EAC launcher failure dialog** — a Windows/EAC bootstrap error shown *before* the game
  if you go through the EAC wrapper and it fails. **Not our problem:** our launcher starts
  `eldenring.exe` directly and never invokes the EAC wrapper, so this never appears (same trick the
  open-source offline launchers use — see "Prior art").
- **(b) The in-game network/login modal(s) at the title screen** — FromSoft FMG-keyed dialog
  boxes. **This is the target.** When the online login fails, the title flow queues one or more of
  these.
- **(c) Periodic "connection lost" banners / forced return to title** — fire mid-session if an
  established online state drops or FPS is flagged.

### The message IDs (datamined — verify on rig)

From the public datamine (`elden-ring-data/msg`, `engus/menu.msgbnd.dcx.json`). The startup/error
strings live in **`GR_System_Message_win64.fmg`**. Treat IDs as a starting map, not gospel —
confirm against the rig before relying on any one number:

| FMG ID | Text (abridged) | Class |
|---|---|---|
| 401170 | "Starting in offline mode. … select \"LOG IN\" from title menu." | (b) the canonical offline popup |
| 401201 | "Unable to connect to the network. Please check your network settings." | (b) |
| 401202 | "Network status check failed" | (b) |
| 401503 | "Failed to log in to the ELDEN RING game server. …" | (b) |
| 401731 | "Failed to initialize character info for online play. …" | (b) |
| 4102 | "The connection to the ELDEN RING game server was lost. Returning to title menu." | (c) |
| 4160 | "Connection to Steam interrupted. Returning to title menu." | (c) |
| 4161 | "Frame rate unsuitable for online play. Returning to title menu." | (c) |

(The Terms-of-Service accept/decline prompt and the in-world summon/invasion notifications are
*different* FMGs — `ToS_win64.fmg`, `NetworkMessage.fmg` — and are not error popups.)

The community "Inappropriate activity detected / offline mode" wording players associate with
running mods is a paraphrase of the EAC-off state; the real title-screen string is **401170**.

### What drives them, in SDK terms

The decision-to-show lives in the online/session layer; the render lives in the menu/FMG layer.
What the pinned SDK (rev `8c67a84`, under `crates/eldenring/src/cs/`) actually gives us:

- **`CSNetMan`** (`net_man.rs`, singleton `"CSNetMan"`) exposes named bools that clearly gate the
  **runtime (c) banners**: `server_connection_lost`, `low_fps_penalty` (commented "True if fps is
  low, prevents you from online play"), and `freeze_game`. These map naturally onto FMG 4102/4161.
- **`CSSessionManager`** (`session_manager.rs`) exposes the `LobbyState` and `ProtocolState` FSMs
  (`None`/`Host`/`Client`/`TryToJoin…`/etc.). This is the state machine whose "failed/none" landing
  precedes the **(b) startup popup**.
- **The (b) startup popup trigger itself is NOT charted.** No SDK function ties "login failed" →
  "queue dialog 401170". That call chain (community-named around `CSNetMan` / a `SprjOnline`-style
  login stepper) is **inference**; finding it is a rig-RE task.
- **The modal-dialog renderer is also NOT charted.** The SDK has the *banner* system
  (`CSMenuManImp::display_status_message`, below) and `MenuType::GenericDialog = 15` (`menu_type.rs`)
  as a *name*, but no "show message box / yes-no dialog by FMG id" function. `MsgRepository`
  (`msg_repository.rs`) is a **bare marker singleton at our pin** — no `get_msg`. (Newer
  `fromsoftware-rs` revs add the FMG read/write API; ours doesn't — see "SDK gaps" below.)

### How to suppress — two mechanism families

1. **Suppress the trigger (preferred, fits our architecture).** Stop the offline/login-failure
   path from ever queueing the dialog — either by NOP-ing the call that raises it, or (cleaner) by
   driving the session/online state so the game never concludes it's in the error state. We already
   commit to "drive the game's own session layer" ([ARCHITECTURE.md](ARCHITECTURE.md) >
   Divergences); bringing the FSM up into a valid-looking state is the same lever ERSC almost
   certainly pulls. Needs the rig to find *where* the startup popup is raised and *what* state
   gates it.
2. **Intercept the display.** Hook the dialog renderer and drop messages by FMG id. More surgical
   but needs the (uncharted) renderer RVA, and a blanked dialog can still flash. Fallback if (1)
   proves hard.

For the **runtime (c) banners specifically**, we may not need any RE: `CSNetMan.server_connection_lost`
/ `low_fps_penalty` are writable bools at our pin. Clamping them off each frame (a `Feature` task,
same pattern as `er-crit-coop`'s `patch.rs`) is a cheap first experiment — confirm on the rig that
clearing them actually suppresses 4102/4161 and doesn't fight the game writing them back.

### First rig step (free observation)

Our mod **doesn't suppress these yet**, so launching as-is *shows* them — which is exactly the
observation we want. Extend `features/observer.rs` to log `CSSessionManager.lobby_state` /
`protocol_state` and `CSNetMan.{server_connection_lost,low_fps_penalty,freeze_game}` each frame
across the boot→title transition. The state right before each popup names the trigger condition.
See [RIG-RUNBOOK.md](RIG-RUNBOOK.md) and the `/test-loop` skill for the deploy/launch/log loop.

## B. The bottom-right version/status watermark

### What it shows

A muted label+value stack in the title screen's bottom-right corner. Labels are FMG strings in
**`GR_MenuText.fmg`** (datamined IDs, verify on rig):

| FMG ID | Label |
|---|---|
| 401320 | `App Ver.` |
| 401321 | `Server Ver.` |
| 401322 | `Calibrations Ver.` (regulation/"calibrations") |
| 401310 | `ONLINE` |
| 401311 | `OFFLINE` |

Crucial detail: the **labels** are FMG; the **version numbers** are formatted in at runtime (they
appear in no FMG). So "App Ver. 1.16" = FMG label `App Ver.` + a runtime-injected number. The game
picks `ONLINE` (401310) vs `OFFLINE` (401311) by current network state. ERSC overwrites this block
with `Seamless Co-op X.Y.Z`; we'd want `unseamless-coop <version>` (+ maybe git short-sha/build).

### How to change it

- **FMG text override** of `GR_MenuText` (e.g. 401311 `OFFLINE` → mod name; 401320 `App Ver.` line
  → version). This is the clean DLL-side path: locate `MsgRepository`, fetch the entry by
  `(category, id)`, and overwrite its UTF-16 buffer in place (null-terminated, **clamped to the
  existing buffer length** — `OFFLINE` is short, so writing a longer mod string means overwriting a
  longer existing entry or repointing the entry). Restore on teardown.
  - **Open-source reference for the exact pattern:** `Dasaav-dsv/erfps2` `src/tutorial.rs`
    (Apache-2.0) does precisely this with `MsgRepository::get_msg_disjoint_mut([(cat, id), …])`,
    UTF-16 encode + `copy_from_slice`, and a `Drop` that restores the original bytes. It's the
    clean-room recipe; reimplement from the behavior, not the bytes.
- **Replacing the version *number*** (not just the `OFFLINE`/label text) likely needs hooking the
  corner-block draw/format, since the number isn't FMG. If we only need to swap `OFFLINE` → mod
  name and tack a version onto a label, FMG override alone may suffice. Decide after seeing the
  live layout on the rig.
- **NOT** the UXM/`menu.msgbnd` asset-repack route — we don't ship or override FromSoft assets
  ([CLAUDE.md](../CLAUDE.md) > Clean-room: don't redistribute upstream bytes). Runtime mutation
  only.

## Shared substrate & SDK gaps (at our pin)

- **`MsgRepository` is a marker singleton at rev `8c67a84`** — no FMG read/write. The FMG-override
  approach (B, and the display-intercept variant of A) needs either: (a) **bump the SDK pin** to a
  rev that exposes `get_msg`/`get_msg_disjoint_mut` (then re-verify *all* struct layouts —
  [CLAUDE.md](../CLAUDE.md) > "pin both crates to the same commit"), or (b) RE the `get_msg` RVA
  ourselves. Bumping the pin is the lower-risk path; check whether the SDK's other named fields we
  rely on shifted first.
- **RVA-backed calls require the matching game version.** The SDK's RVA bundle (`rva.rs`) only
  resolves for ER **2.6.2.0 (WW) / 2.6.2.1 (JP)**; it panics otherwise. Any `display_status_message`
  / future `get_msg` call assumes the rig's game is on that version. Confirm the installed version
  before leaning on RVA calls.
- **We already have a native on-screen message API** for "replace their popups with our own":
  `CSMenuManImp::display_status_message(i32)` is **charted and callable at our pin** (`menu_man.rs`,
  RVA `cs_menu_man_imp_display_status_message`). It drives the big center banners via the
  `STATUS_MESSAGE_*` constants (`…_MENU_TEXT = 41`, etc.). This is the natural backend for our
  notifications model (`unseamless-core/notifications.rs`) when we want a *native* banner rather
  than the planned overlay. Note: this is the **banner** system, distinct from the **modal dialog**
  that shows the offline errors — useful for our messaging, not for suppressing theirs.

## Prior art (offline launchers — confirm the launch half)

`techiew/EldenRingEacToggler` (Nexus mods/90) and the other "Offline launcher (No EAC)" mods just
set `SteamAppId=1245620` and `ShellExecute` `eldenring.exe` directly — no memory patching, no
network hook. They get you offline but **do not** suppress popup 401170; they accept it. Takeaway:
**launching outside EAC is necessary but not sufficient** — our `launcher` already does the launch
half; the in-process suppression (A) is the part only a loaded DLL can do, and the part no public
source documents for ERSC.

## Status & next actions

- [ ] Rig observation: log session/net state across boot→title; record exactly which popups fire
      under our launch and the state that precedes each.
- [ ] Cheap experiment: clamp `CSNetMan.{server_connection_lost,low_fps_penalty}` off in a
      `Feature` task; confirm it suppresses the (c) banners.
- [ ] Locate the (b) startup-popup trigger / the dialog renderer (rig RE) — decide suppress-trigger
      vs intercept-display.
- [ ] Decide SDK-pin bump vs RVA-RE for FMG access (gates the watermark restyle and display-intercept).
- [ ] Watermark: once FMG access exists, overwrite `GR_MenuText` 401311/version line; evaluate
      whether the runtime version number needs a draw hook.

## Sources

- Pinned SDK `fromsoftware-rs` rev `8c67a84` — `crates/eldenring/src/cs/{net_man,session_manager,
  menu_man,fe_man,msg_repository,menu_type}.rs`, `rva.rs`, `rva/bundle.rs` (read directly).
- [elden-ring-data/msg](https://github.com/elden-ring-data/msg) — FMG IDs/strings (datamine).
- [Dasaav-dsv/erfps2 `src/tutorial.rs`](https://github.com/Dasaav-dsv/erfps2/blob/main/src/tutorial.rs)
  — runtime `MsgRepository` mutation pattern.
- [techiew/EldenRingEacToggler](https://github.com/techiew/EldenRingEacToggler) — offline-launch
  reference; [Nexus mods/90](https://www.nexusmods.com/eldenring/mods/90).
- [ERSC docs/FAQ](https://ersc-docs.github.io/faq/) — "version … title screen, bottom right".
- [LukeYui/EldenRingSeamlessCoopRelease](https://github.com/LukeYui/EldenRingSeamlessCoopRelease)
  — binary-only (no source); mechanism is inference, not copied.
</content>
</invoke>
