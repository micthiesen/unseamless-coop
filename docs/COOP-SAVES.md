# Separate Co-op Save Files

How co-op gets its own save file so it can never touch the vanilla `.sl2` — the `save.file_extension`
feature ([FEATURES.md](FEATURES.md), config key already present as `save.file_extension = "co2"`). ERSC
does this so a character's co-op progress and its single-player progress live in two independent files;
we want the same isolation, and for us it is **safety-critical**: the failure mode is corrupting or
overwriting the player's vanilla save, which is the one thing this feature exists to prevent.

> **Status: implemented and rig-confirmed (2026-06).** The decision/transform is the host-tested
> [`unseamless_core::saves`](../crates/unseamless-core/src/saves.rs); the `CreateFileW` detour is
> [`coop/saves.rs`](../crates/unseamless-coop/src/saves.rs), installed early in
> [`app::install`](../crates/unseamless-coop/src/app.rs). A solo rig run confirmed the whole chain: the
> hook installs, reads redirect (`ER0000.sl2 -> ER0000.<ext>`), the backup redirects
> (`.sl2.bak -> .<ext>.bak`), a fresh `ER0000.<ext>` is **written**, and the vanilla `ER0000.sl2`
> (and the machine's real ERSC `.co2`) are left **untouched** — exactly the isolation this exists for.
> The rig seed config uses a distinct extension (`uco`) so testing can't write the real ERSC `.co2`.

Originally a research note; the mechanism below is what shipped. Game-internal claims are grounded in the
pinned `fromsoftware-rs` SDK (cited), in the **MIT-licensed `vswarte/alt-saves`** mod (the SDK author's
own save-location changer — read for *mechanism*, re-derived here per [CLAUDE.md](../CLAUDE.md) >
Clean-room), or are behavioral observations confirmed on the rig. ERSC itself is closed + Themida, so its
exact code is inference; the *behavior* (separate `.co2`) is well documented by its own FAQ.

> Hard project rule this feature must respect: the mod **must not touch `regulation.bin`**
> ([CLAUDE.md](../CLAUDE.md) > Safety/legitimacy). `alt-saves` ships a second, unrelated patch that
> clears a regulation-check flag (`regulation.rs`); that is **not** part of the save mechanism and we do
> **not** port it. Only the file-path interception below is in scope.

## How the Game Names and Locates Its Save

On a stock install the save lives at, per platform:

- **Windows:** `%AppData%\EldenRing\<SteamID64>\ER0000.sl2` (+ `ER0000.sl2.bak`).
  `%AppData%` = `C:\Users\<user>\AppData\Roaming`. `<SteamID64>` is the 17-digit account ID, so the path
  is per-account.
- **Linux + Proton (the rig):** the same path *inside the Proton prefix*. `AppData/Roaming` maps to
  `…/steamapps/compatdata/1245620/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing/<SteamID64>/`
  (ERSC's docs give the compatdata root; the `users/steamuser/AppData/Roaming/EldenRing/<SteamID64>`
  tail is the standard Wine layout). **Rig-only:** any verification of the written file happens here, not
  on the Mac.

What's confirmed about the format (relevant only so we understand what we must *not* break):

- `ER0000.sl2` is a **BND4 archive** of ~11 `USERDATA` sub-files (10 character slots + a
  system/profile file). Each `USERDATA` block is **AES-128-CBC encrypted** with a well-known fixed key,
  and the archive carries per-block checksums. The active `.sl2` and its `.sl2.bak` backup are the same
  format; the game writes both.
- The **SteamID gates two things**: the directory name (`…/EldenRing/<SteamID64>/`) and a copy of the ID
  embedded *inside* the save data (which is why third-party "save ID editors" exist to re-sign a save for
  a different account). **Inference:** the game derives the directory from the logged-in Steam account at
  runtime; the filename stem (`ER0000`) and extension (`.sl2`) are constants the engine appends. The
  extension being a constant string is the lever the whole feature turns on.

The crucial property for us: **the extension is the *only* thing that distinguishes a vanilla save from a
co-op save**. Same folder, same `ER0000` stem, same internal format — only `.sl2` vs `.co2`. The game
will only load `ER0000.<its-configured-extension>`, so two extensions = two fully independent saves for
the same character, in the same directory, with zero risk of one reading the other (confirmed behavior
per ERSC's FAQ and community docs).

## What to Intercept, and Where (The Mechanism)

The cleanest, most robust interception point is **not** an SDK field and **not** a game-function hook —
it's a **hook on the Win32 `kernel32!CreateFileW`** that the game ultimately calls to open the save file.
This is exactly what `alt-saves` does (MIT; mechanism re-derived here):

1. Detour `CreateFileW`. On every call, look at the wide-string path argument.
2. If it ends in `.sl2` **or** `.sl2.bak`, rewrite the extension to our co-op extension (`.co2` /
   `.co2.bak`) and pass the rewritten path through to the real `CreateFileW`. Otherwise pass it through
   untouched.
3. (Optional, `alt-saves` does it) also recognize an already-`.co2` path so a *second* mod stacking on
   top can redirect further. For us, the simpler `.sl2 → .co2` rewrite is enough.

Why this point and not the alternatives:

- **vs. patching the extension string in `.text`/data:** the game's `.sl2` literal is a UTF-16 constant;
  overwriting it in place is brittle (length-bounded, version-specific AOB) and would need RE for every
  game update. `CreateFileW` is stable, documented Win32 — version-proof.
- **vs. an SDK field/method:** there isn't one. The SDK's `cs/file.rs` charts `CSFileImp` /
  `CSFileRepository` (the resource/`FD4FileCap` loader — virtual + on-disk *asset* files) and the RVA
  bundle exposes only `csfile_repository_vmt` (a vtable pointer), with **no save-path API and no
  extension field**. `cs/game_man.rs` exposes save *state* (`save_slot`, `save_requested`, `save_state`,
  `requested_save_slot_load_index`) and `cs/game_data_man.rs` exposes save *versioning*, but **neither
  carries the path or extension**. So the SDK gives us the save *lifecycle* to observe, not a lever to
  change the filename. (Marked PARTIAL in [SDK-COVERAGE.md](SDK-COVERAGE.md), correctly.)
- **vs. renaming files on disk** (the manual "copy `ER0000.sl2` → `ER0000.co2`" trick, and what me3's
  newer `savefile` option does by seeding a copy): that's a *setup* convenience, not a live redirect, and
  it doesn't keep the two saves isolated *during play*. We want the in-process redirect so the running
  game writes to `.co2` and never opens `.sl2` at all.

The `CreateFileW` hook is **substrate we don't have yet.** `alt-saves` uses the `retour` crate's
`static_detour!` to build an inline trampoline detour. `er-crit-coop`'s `patch.rs` only does SDK-field
writes per frame, so this is a new capability for our toolchain (a function detour, distinct from the
AOB+NOP patcher [SKIP-INTROS.md](SKIP-INTROS.md) also wants — both are "we need a patch/hook utility"
gaps). `retour` is the obvious choice and is already battle-tested for exactly this hook.

**Cross-compile: validated (spike).** `retour` 0.3.1 (`features = ["static-detour"]`) compiles
cleanly to `x86_64-pc-windows-gnu` via mingw in both dev and shipping profiles (confirmed alongside
the hudhook spike on the rig). Note the stable line is **0.3.x** — `0.4` is alpha-only on crates.io,
so pin `retour = "0.3"`.

## How ERSC Does It (Behaviorally)

From ERSC's own FAQ and the community, re-stated in our words (its code is closed/Themida, so the
*mechanism* is inference; the *behavior* is documented):

- Co-op writes/reads `ER0000.co2` (config `save_file_extension = co2`); vanilla keeps `ER0000.sl2`. Same
  directory, same internal format. The two are independent because the game only ever opens the name with
  the extension it was told to use.
- The conversion between them is "**just rename the file**" — `ER0000.sl2` copied to `ER0000.co2` is a
  valid co-op save and vice-versa, since only the extension differs. (Third-party "save converters" do
  literally this.) This confirms changing the extension **fully isolates** the saves; there's no other
  per-file marker the game keys on.
- The `.bak` matters: the game writes `ER0000.sl2.bak` as its own rolling backup, so our hook must
  redirect **both** `.sl2` and `.sl2.bak` (→ `.co2` / `.co2.bak`). Miss the `.bak` and the game would
  still touch the vanilla backup file — a partial leak of co-op data into a vanilla-named file. `alt-saves`
  handles both extensions explicitly; we must too.
- **One-directional safety warning, surfaced to the user:** the community consensus is that converting
  *vanilla → co-op* is safe, but moving a *co-op save back to vanilla and going online* risks a ban
  (co-op saves can hold items/flags impossible in legit play). We never play vanilla online (we're
  outside EAC for co-op only), but our docs/README should carry the same warning since users will be
  tempted to rename files by hand.

## Steam Cloud Caveat

- **Elden Ring uses Steam Cloud** for its save. Cloud sync is **filename/pattern based** in the app's
  cloud config; a non-standard extension like `.co2` is **not** matched by the vanilla `ER0000.sl2`
  cloud rule. **Inference (high confidence, confirm on rig):** the `.co2` file is therefore **not
  cloud-synced** — it lives only on the local disk. This is mostly *fine* (it's exactly the isolation we
  want, and it's why the vanilla `.sl2` keeps syncing untouched), but it has two consequences worth
  stating: (a) co-op progress isn't backed up to the cloud, so a local disk loss loses it; (b) playing
  the same co-op character on two machines won't carry over via cloud.
- **Running outside EAC does not change cloud behavior** — cloud sync is a Steam-client concern keyed on
  the app and file patterns, independent of EAC. The only variable is the **extension**, per the point
  above.
- **Risk to call out:** if a user *renames* `ER0000.co2` back to `ER0000.sl2` to "get cloud backup," they
  (a) overwrite/clobber their vanilla save and (b) re-arm the ban risk above. Our messaging should steer
  them to back up the `.co2` manually (copy the `…/EldenRing/<SteamID64>/` folder, as `alt-saves`
  recommends) instead.

## What We Should Build, and When (Safety-First)

The single most important property: **the hook must be active before the game opens the save for the
first time.** `alt-saves`'s own README is blunt about this — *"if this mod loads too late it might start
writing to your vanilla save file first."* That is precisely the corruption we cannot allow.

- **Install timing: earliest possible, synchronously, in `DllMain` / very-early `install`.** We ship as
  the game's `dinput8.dll` proxy, so we already load **far earlier** than an Elden Mod Loader mod
  (`app.rs` documents this — the task system isn't even up yet when we run). That early load is an
  *advantage* here: install the `CreateFileW` detour **before** `wait_for_task_system()`, alongside the
  EAC guard, not as a per-frame `Feature` task. A `Feature` ticks on the game's scheduler, which only
  exists *after* init — far too late; the save can be opened during early boot. This is the key
  divergence from how our other features register.
- **Driven by config:** read `save.file_extension` (already in `unseamless-core/config.rs`, default
  `"co2"`, validated to 1..=120 alphanumerics by `Config::validate`). Build `.<ext>` and `.<ext>.bak`
  from it. An empty/invalid value is already clamped to `co2` before we'd see it.
- **Redirect both `.sl2` and `.sl2.bak`.** Non-negotiable (see ERSC `.bak` note above).
- **Match conservatively.** Only rewrite paths whose final component is `ER0000.sl2` / `ER0000.sl2.bak`
  (or, defensively, any path ending exactly in those extensions *under the EldenRing save dir*). A loose
  "contains .sl2" match could catch unrelated files; an ends-with on the known stem+extension is the safe
  shape. Confirm the exact path the game passes on the rig before finalizing the predicate.

### Failure Modes That Could Corrupt the Vanilla Save

1. **Hook installed too late** → game opens/writes `ER0000.sl2` before redirect is live. *Mitigation:*
   install in `DllMain`/pre-task, and on the rig confirm the **first** `CreateFileW` for a save path is
   already rewritten.
2. **`.bak` not redirected** → game writes `ER0000.sl2.bak` with co-op data, polluting the vanilla
   backup. *Mitigation:* handle both extensions; verify the `.bak` redirect explicitly.
3. **Hook failing silently** (detour didn't take, or `CreateFileW` not the path used) → game writes
   vanilla `.sl2` and nobody notices until it's overwritten. *Mitigation:* log every rewrite at
   `debug!`, and treat "save feature enabled but no rewrite ever logged across a save" as an alarm. Decide
   whether a *failed install of this specific hook* is fatal (`guard::fatal`, like the EAC guard, since
   continuing risks the vanilla save) vs. a loud toast. **Leaning fatal**: this hook's whole job is
   protecting the irreplaceable file; a half-working state is the dangerous state.
4. **Predicate too greedy** → some unrelated game file gets its extension mangled. *Mitigation:*
   ends-with on `ER0000.sl2`/`.sl2.bak`, not substring; rig-verify nothing else is caught.
5. **Auto-backup confusion** — the game's own `.bak` rotation now operates on `.co2.bak`. That's correct
   and desired, but means there's **no vanilla-named backup of co-op data**; users must back up manually.

### Verification Plan (Rig)

All of this is **rig-only** (Linux + Proton; see [RIG-RUNBOOK.md](RIG-RUNBOOK.md) and `/test-loop`):

1. **Baseline & backup.** Copy the whole `…/compatdata/1245620/pfx/.../EldenRing/<SteamID64>/` folder
   aside. Record `ER0000.sl2` size + mtime (and sha256).
2. **Launch modded, play, save** (rest at a grace / quit to menu so the game flushes a save).
3. **Confirm a `.co2` was written:** `ER0000.co2` (and `ER0000.co2.bak`) now exist in that folder.
4. **Confirm `.sl2` is untouched:** the vanilla `ER0000.sl2` size/mtime/sha256 are **identical** to the
   baseline — the single most important assertion of this feature.
5. **Confirm the redirect logs:** the mod's log shows the `CreateFileW` rewrite(s) for the save path
   (and the `.bak`), and the **first** save-path open was already rewritten.
6. **Round-trip sanity:** rename a copied `ER0000.sl2` → `ER0000.co2`, confirm it loads in co-op (proves
   extension-only isolation), without ever modifying the original `.sl2`.
7. (Optional) **Cloud check:** confirm Steam shows the `.sl2` syncing and the `.co2` staying local.

## Status / Next Steps

- [x] Config surface exists: `save.file_extension` (default `co2`, validated) in
      `unseamless-core/config.rs`. No config work needed.
- [ ] Add a **function-detour utility** (`retour`-based) to the cdylib's toolchain — new capability,
      shared with any future hook-based feature.
- [ ] Implement the **`CreateFileW` hook**: rewrite `ER0000.sl2`/`.sl2.bak` → `.<ext>`/`.<ext>.bak` from
      `save.file_extension`, installed **before** `wait_for_task_system()` in `app::install`
      (not a `Feature` task).
- [ ] Decide fatal-vs-toast if the hook **fails to install** (leaning `guard::fatal`, like the EAC guard,
      given the corruption risk). Implement accordingly.
- [ ] Rig: run the verification plan above; the load-bearing assertion is **vanilla `.sl2` byte-identical
      before/after**.
- [ ] Rig: confirm the exact save path string the game passes to `CreateFileW` under Proton (validate the
      predicate, and that `CreateFileW` — not some other API — is the open path).
- [ ] Rig: confirm Steam Cloud behavior for `.co2` (expected: not synced).
- [ ] README: user warning — never rename a `.co2` back to `.sl2` and play online; back up the `.co2`
      folder manually (no cloud backup).

## Sources

- Pinned SDK `fromsoftware-rs` rev `8c67a84` — `crates/eldenring/src/cs/file.rs` (`CSFileImp` /
  `CSFileRepository`, asset loader — no save path), `cs/game_man.rs` (`save_slot`/`save_requested`/
  `save_state`), `cs/game_data_man.rs` (save versioning), `rva/bundle.rs` (`csfile_repository_vmt` —
  vtable only, no save-path API). Read directly.
- [vswarte/alt-saves](https://github.com/vswarte/alt-saves) (MIT) — the `CreateFileW`-hook mechanism for
  changing the save extension; `src/file.rs` (`.sl2`/`.sl2.bak`/`.co2`/`.co2.bak` rewrite),
  `src/config.rs`, `README.md` (load-timing warning, backup advice). Mechanism re-derived per clean-room;
  its unrelated `regulation.rs` flag patch is deliberately **not** ported.
- [ERSC FAQ](https://ersc-docs.github.io/faq/) — `.co2` vs `.sl2`, save locations (Windows + Linux
  compatdata), "saves unaffected by updates/reinstall", manual-backup advice.
- [Souls Modding — SL2 Files](https://sites.google.com/view/soulsmods/file-formats/sl2-files) — BND4 +
  AES-128-CBC `USERDATA` format (what we must not break).
- [sabpprook/ERSaveIDEditor](https://github.com/sabpprook/ERSaveIDEditor) — confirms the SteamID is
  embedded inside the save (why directory + internal ID both matter).
- [me3 ModProfile reference](https://me3.help/en/latest/configuration-reference/) and
  [me3 issue #653](https://github.com/garyttierney/me3/issues/653) — the successor `savefile` option
  (seed-a-renamed-copy approach); contrast with our live in-process redirect.
- [ELDEN RING save-location community notes](https://steamcommunity.com/app/1245620/discussions/0/634541994529574595/)
  — `ER0000.sl2`/`.bak`, `%AppData%\EldenRing\<SteamID>\`, ban warning on co-op→vanilla→online.
</content>
</invoke>
