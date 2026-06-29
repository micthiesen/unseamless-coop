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

YOUR REAL SAVE IS SAFE. Co-op runs on its own separate save file, so your real character is
never read or written by the test build. So you don't have to test on a blank character, the
installer copies your most-recent character into that co-op save, and you play the test on the
copy. Your real save stays exactly as it is. (Uninstall removes the copy again.)

FOLLOW THE ON-SCREEN STEPS. When we're running a guided test, a pinned banner appears at the
top of the screen and walks all of us through the exact same sequence, one step at a time, so
there's nothing to coordinate over chat. Each step advances on its own once the game reaches
it; if one ever needs you to move it along by hand, the banner says so. Manual advance/skip is on
the CONTROLLER (advance = hold L3 + D-pad Up, i.e. press the left stick in while holding D-pad Up;
skip = L3 + D-pad Down), so have a controller handy for those few steps -- most advance on their
own. Just read the banner and do what it says.


UNINSTALL (back to your normal setup)
-------------------------------------
Double-click  Uninstall.cmd  in the same folder. It restores your original
start_protected_game.exe, dinput8.dll, and mods\ from the backup. Your real save is never
touched (co-op uses a separate save file), so it stays exactly as it is. The co-op test save
(the copy of your character) is removed too — keep it instead with:
  powershell -ExecutionPolicy Bypass -File Uninstall.ps1 -KeepCoopSave


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
The on-screen steps usually end by asking you to do this, but here's the whole of it: the
easiest way to send me what I need is the in-game Export button. Open the mod menu (the  `
key, top-left above Tab — or, on a controller, RB + click both sticks L3 + R3 together), and
on the "Actions" tab pick  Export diagnostics  (Enter, or the A button). It saves ONE file:

      <your ELDEN RING\Game>\unseamless-coop\unseamless-coop-diagnostics.txt

Send me that one file. It works even when we never managed to connect (that's the case I most
need it for). It does NOT contain your password (redacted) or your Steam account (the Steam IDs
in it are scrubbed), so it's safe to send or paste anywhere.

If the menu won't open or you can't find the file, fallback: there's a logs folder at
<your ELDEN RING\Game>\unseamless-coop\logs\ — zip the whole "logs" folder and send that
instead. Each log names the exact build you ran at the top (a "build_id" line), which is how
I confirm we were all on the same version.

IF THE GAME CRASHES (closes itself) on launch or right when the overlay would appear: that's
a known bug we're chasing, so it isn't your fault — please still send the logs, that's the
whole point. The in-game Export button won't be reachable (the overlay is what crashed), so
use the logs-folder fallback above: zip  <ELDEN RING\Game>\unseamless-coop\logs\  and send it.
The crash records itself there (look for a line starting "crashdump:") even though the game
closed.


WHAT GOT INSTALLED (for the curious)
------------------------------------
- dinput8.dll and start_protected_game.exe in your ELDEN RING\Game folder (the mod + its
  launcher).
- A config at Game\unseamless-coop\unseamless_coop.toml (the shared password).
- An empty mods\ folder (your mods were backed up and come back on uninstall).
- A co-op test save (a copy of your character) on a separate save file in your ELDEN RING save
  folder. Your real save (ER0000.sl2) is untouched; the copy is removed on uninstall.
Your original files are saved in  ELDEN RING\unseamless-coop-backup\  (don't delete that
until you've uninstalled).
