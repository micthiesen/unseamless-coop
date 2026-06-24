//! unseamless-coop launcher — ships as `start_protected_game.exe`, replacing the game's own
//! EasyAntiCheat bootstrapper so Steam's "Play" runs us instead.
//!
//! Its whole job: start `eldenring.exe` **directly** (which skips EAC) with two bits of context set:
//! - `UNSEAMLESS_LAUNCH=1` — the safety marker our `dinput8.dll` checks; without it the DLL refuses
//!   to run (so if a game update restores the original launcher, the mod won't run under EAC).
//! - the Steam app id — so the game's Steamworks (the P2P layer co-op rides) initializes as ELDEN
//!   RING even if we weren't launched from Steam's own process tree.
//!
//! It then waits for the game to exit, so Steam shows "Playing ELDEN RING" for the whole session.
//! This is Windows-only and rig-validated; there's nothing host-testable here.
#![windows_subsystem = "windows"]

use std::process::Command;

use unseamless_core::LAUNCH_MARKER;

/// ELDEN RING's Steam app id.
const STEAM_APP_ID: &str = "1245620";
const GAME_EXE: &str = "eldenring.exe";

fn main() {
    let dir = match std::env::current_exe().ok().and_then(|p| p.parent().map(ToOwned::to_owned)) {
        Some(d) => d,
        None => return fail("Could not determine the launcher's own location."),
    };

    let game = dir.join(GAME_EXE);
    if !game.exists() {
        return fail(&format!(
            "{GAME_EXE} was not found next to the launcher:\n{}\n\nPut the mod files in your \
             'ELDEN RING/Game' folder.",
            game.display()
        ));
    }

    // Start the game directly (no EAC), with the marker + Steam context, forwarding any args Steam
    // passed. `.status()` waits for exit so Steam tracks the session.
    let result = Command::new(&game)
        .current_dir(&dir)
        .args(std::env::args_os().skip(1))
        .env(LAUNCH_MARKER, "1")
        .env("SteamAppId", STEAM_APP_ID)
        .env("SteamGameId", STEAM_APP_ID)
        .status();

    if let Err(e) = result {
        fail(&format!("Failed to launch {GAME_EXE}:\n{e}"));
    }
}

/// Surface an error to the user. The launcher has no console (windows subsystem), so use a message
/// box, then exit non-zero.
fn fail(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    let text = HSTRING::from(message);
    let title = HSTRING::from("unseamless-coop launcher");
    unsafe {
        let _ = MessageBoxW(None, &text, &title, MB_OK | MB_ICONERROR);
    }
    std::process::exit(1);
}
