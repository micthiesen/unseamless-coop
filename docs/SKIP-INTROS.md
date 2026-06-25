# Skipping the Boot Logos / Intros

How to skip the logo/splash screens shown at boot before the title menu — the `skip_splash_screens`
feature ([FEATURES.md](FEATURES.md)). This is well-trodden ground in ER modding: the canonical
runtime method is a **one-branch, two-byte patch** to the boot/title flow, with the exact technique
in MIT-licensed open source. No movie files, no Bink hooking.

Research note, not implemented. Game-internal claims are grounded in the pinned `fromsoftware-rs`
SDK source (cited) or are behavioral observations to confirm on the rig. Clean-room posture per
[CLAUDE.md](../CLAUDE.md): reimplement from the mechanism, not from copied bytes (the reference is
MIT so reading it is fine, but the patch pattern is version-specific anyway — re-derive ours).

## The boot sequence

What plays on a stock boot, in order, before the "PRESS ANY BUTTON" title screen:

1. **White/black init flash** while the engine spins up — *engine-rendered*, not a movie. (QoL mods
   mask this with a separate black overlay; it's not removed by skipping movies.)
2. **EasyAntiCheat splash** — only when launched through the EAC wrapper. **Moot for us:** our
   launcher starts `eldenring.exe` directly outside EAC, so this never shows.
3. **FromSoftware / Bandai Namco / middleware-legal logos** — *Bink `.bk2` videos*. The "intro
   logos" the skip targets.
4. **Online-play / autosave notice card(s)** — likely engine-rendered notice screens (exact
   ordering not authoritatively documented).
5. Title screen / main menu.

The split that matters: **logos are videos; the white flash and notice cards are engine-rendered.**
That's why the file-delete trick kills logos but not the white screen, and why the open-source DLL
mod has two *separate* features (patch the logo flow + cover the white screen).

## The movie files (observed)

On this rig's install, `movie/` holds **loose, numeric-named `.bk2` files** (Bink 2 video):

```
movie/10010010.bk2  (~1.0 GB)   movie/13000050.bk2  (~220 MB)
movie/19000010.bk2  (~165 MB)   movie/19000030.bk2  (~576 MB)   movie/19000031.bk2 (~168 MB)
movie/19000060.bk2  (~165 MB)   movie/19000070.bk2  (~166 MB)   movie/19000080.bk2 (~166 MB)
movie/60510000.bk2  (~375 MB)   movie_dlc/20010020.bk2 (~382 MB, DLC opening)
```

The cluster of near-identical ~165 MB `19000xx0` files looks like a localized logo/warning reel;
`10010010` (1 GB) is plausibly the opening cinematic. **This mapping is inference** — exact
identification isn't publicly indexed and isn't needed for our approach (we patch the flow, not the
files). If we ever want the real names, read them off a UXM/legit dump on the rig; never
redistribute them.

## Two methods

### File-based (reference only — rejected)

Replace the logo `.bk2`s with empty/stub videos (Nexus "No Startup Videos"), or UXM-unpack and
delete them. Skips only the *video* logos, and requires shipping/overriding FromSoft assets — which
this project does not do ([CLAUDE.md](../CLAUDE.md) > clean-room). Understand it; don't use it.

### Runtime patch (the chosen approach)

A loaded DLL flips one conditional branch in the boot/title state machine — the branch that decides
"play the logo sequence" vs. "proceed to title" — so the game falls straight through to the menu.
**Behaviorally: NOP the gate, skip the logos.** It does *not* hook the Bink player and does *not*
simulate keypresses.

- **Open-source reference (MIT):** `techiew/EldenRingMods` → `SkipTheIntro/DllMain.cpp`. It
  AOB-scans for the gate, then overwrites a short conditional jump with two `NOP`s. The exact AOB
  and offset are in that file; they are **version-specific**, so re-derive against our rig's game
  version rather than transcribing.
- **DS3 precedent:** `bladecoding/DarkSouls3RemoveIntroScreens` — same idea, hardcoded per-version
  offsets instead of an AOB scan. Confirms this is the standard FromSoft-engine technique.
- **ERSC's `skip_splash_screens`** almost certainly does the equivalent boot-flow patch (closed +
  Themida, so inference) — there's no SDK movie API it could be calling instead.
- **White-screen overlay (optional, separable):** the reference also spawns a topmost black Win32
  popup window over the game for a few seconds to mask the init flash. Pure Win32, no memory patch.
  Treat as a distinct sub-feature we can add later.

## SDK angle (at pin `8c67a84`)

The SDK gives us **no named handle for movie playback or intro-skip** — confirming this is an
AOB/RE feature, not an SDK-field write (unlike scaling/flags). Breadcrumbs that exist but are *not*
skip levers:

- `CSTaskGroupIndex::MovieStep` (`cs/task.rs`) — a frame-task phase associated with movie playback.
  Tells us *when* movie work runs, not how to skip it.
- `pre_opening_movie_wait_sec` (a param field in `param/generated.rs`) — a timing knob around the
  opening movie, not a logo-skip toggle.

So implementing this needs **byte-pattern scanning + a code patch**, which our current toolchain
doesn't have yet:

- `er-crit-coop`'s `patch.rs` pattern is *SDK-field writes per frame* — not applicable here.
- We do have RVA→VA resolution (`pelite` + `Program::current().rva_to_va`, as the SDK uses for
  `display_status_message`). For a stable cross-version patch we'd add a small **AOB scanner** over
  the game module's `.text` (designed in [CODE-PATCHING.md](CODE-PATCHING.md) — pelite already ships
  the scanner, as the SDK's `arxan.rs` shows), find the gate once at install, and NOP it.
  Alternatively, hardcode the VA per game version (brittle; the SDK already pins to ER 2.6.2.0
  WW / 2.6.2.1 JP, so a version-gated offset is viable but needs re-checking on every game update).

## How it maps to our project

- A **one-shot patch at install**, on the init thread (after the module is mapped), not a recurring
  task — apply the NOP once and leave it. Like the task handle, the patch is permanent; nothing
  unwinds it (we stay resident).
- Driven by the existing **`skip_splash_screens` setting** in the registry
  (`unseamless-core/settings.rs`) — apply only when enabled.
- **Reclassify in FEATURES.md:** this was listed as **E** (param write). It's actually **M** —
  needs its own AOB/RE and a memory patch, not an SDK field. (Updated.)

## Open questions / next steps

- [ ] On the rig: launch unmodified (outside EAC) and record the exact boot sequence we get — which
      logos actually play, and whether the EAC splash is already gone. This sets the real target.
- [ ] Derive our own AOB (or version-gated VA) for the boot-flow gate against the rig's game
      version; confirm the 2-byte NOP skips logos and lands cleanly on the title screen.
- [ ] Implement the shared AOB-scan + patch utility per [CODE-PATCHING.md](CODE-PATCHING.md)
      (pelite `finds_code` over `code_range()` + `VirtualProtect`), then NOP the boot-flow gate
      through it.
- [ ] Decide whether to also ship the white-screen black-overlay sub-feature.

## Sources

- Pinned SDK `fromsoftware-rs` rev `8c67a84` — `crates/eldenring/src/cs/task.rs` (`MovieStep`),
  `param/generated.rs` (`pre_opening_movie_wait_sec`), `rva.rs` (no movie RVA). Read directly.
- [techiew/EldenRingMods — SkipTheIntro](https://github.com/techiew/EldenRingMods/blob/master/SkipTheIntro/DllMain.cpp)
  (MIT) — the canonical runtime AOB+NOP reference.
- [bladecoding/DarkSouls3RemoveIntroScreens](https://github.com/bladecoding/DarkSouls3RemoveIntroScreens)
  — DS3 per-version-offset variant.
- [Nexus mods/421](https://www.nexusmods.com/eldenring/mods/421) (DLL skip) and
  [mods/91](https://www.nexusmods.com/eldenring/mods/91) (file replacement).
- [ERSC FAQ — `skip_splash_screens`](https://ersc-docs.github.io/faq/).
- [ER.BDT.Tool](https://github.com/Ekey/ER.BDT.Tool) — for reading `.bk2` names off a dump if ever needed.
</content>
</invoke>
