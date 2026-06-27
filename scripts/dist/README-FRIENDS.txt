unseamless-coop test build for friends
======================================

Thanks for helping test! This is an in-development co-op mod for ELDEN RING. It installs
ALONGSIDE your existing setup and is fully reversible (Uninstall puts everything back).

This is NOT a finished mod. Use it only for our test sessions, and never on the official
servers (it loads outside EasyAntiCheat, same as Seamless Co-op).


WHAT YOU NEED
-------------
- ELDEN RING on Steam. You do NOT need to uninstall Seamless Co-op or your other mods: the
  installer backs them up (your other mods are disabled during the test session and restored
  on uninstall).


INSTALL (about 30 seconds)
--------------------------
1. UNBLOCK THE ZIP FIRST (important): right-click the .zip you downloaded -> Properties ->
   tick "Unblock" at the bottom -> OK. (Windows marks downloaded files as blocked; this
   clears it for everything inside in one step.)
2. Extract the zip somewhere (Desktop is fine).
3. Double-click  Install.cmd  inside the extracted folder.
4. It finds your ELDEN RING folder automatically, backs up your current setup, installs the
   test build, and checks the files copied correctly. Read the green lines; if anything is
   red, tell me.
5. Launch ELDEN RING from Steam normally (press Play).

To play together: we all just press Play. The shared password is already baked into the
config that came with this bundle, so there's nothing to set up.


UNINSTALL (back to your normal setup)
-------------------------------------
Double-click  Uninstall.cmd  in the same folder. It restores your original
start_protected_game.exe, dinput8.dll, and mods\ from the backup. Your saves are never
touched (we use a separate save file), so they stay as they are.


IF WINDOWS BLOCKS IT
--------------------
- "Windows protected your PC" (SmartScreen) when running Install.cmd: click "More info"
  -> "Run anyway". The files aren't code-signed, so Windows nags about anything new.
- Windows Defender quarantines a file: a DLL that loads into a game looks suspicious to
  antivirus (Seamless Co-op trips this too). If a file goes missing after install, restore
  it from quarantine or add the ELDEN RING\Game folder as a Defender exclusion, then run
  Install.cmd again. Ask me if unsure.
- If Install.cmd does nothing: right-click Install.ps1 -> "Run with PowerShell".


SENDING ME DIAGNOSTICS AFTER A SESSION
--------------------------------------
If something misbehaves (especially if we couldn't connect at all), the easiest way to send
me what I need is the in-game Export button. ONE click makes a single file to send:

1. In-game, press the  `  key (top-left of the keyboard, above Tab) to open the mod menu.
   (On a controller: press RB + click both sticks, L3 + R3, together.)
2. You'll be on the "Actions" tab. Move down to  Export diagnostics  and press Enter
   (or the A button on a controller).
3. A message pops up with the exact file it saved. It's here:

      <your ELDEN RING\Game>\unseamless-coop\unseamless-coop-diagnostics.txt

4. Send me that one file.

That file works even when we never managed to connect (that's the case I most need it for).
It does NOT contain your password (redacted) or your Steam account (the Steam IDs in it are
scrubbed), so it's safe to send or paste anywhere.

If the menu won't open or you can't find the file, fallback: there's a logs folder at
<your ELDEN RING\Game>\unseamless-coop\logs\ — zip the whole "logs" folder and send that
instead. Each log names the exact build you ran at the top (a "build_id" line), which is how
I confirm we were all on the same version.


WHAT GOT INSTALLED (for the curious)
------------------------------------
- dinput8.dll and start_protected_game.exe in your ELDEN RING\Game folder (the mod + its
  launcher).
- A config at Game\unseamless-coop\unseamless_coop.toml (the shared password).
- An empty mods\ folder (your mods were backed up and come back on uninstall).
Your original files are saved in  ELDEN RING\unseamless-coop-backup\  (don't delete that
until you've uninstalled).
