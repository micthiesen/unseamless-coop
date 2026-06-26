//! Boot master volume: set the game's master volume at startup to the configured level, so the game
//! doesn't always come up at its saved volume (handy paired with `skip_splash_screens`, which drops
//! you onto the loud title fast).
//!
//! **Opt-in:** gated on [`boot_master_volume_enabled`](unseamless_core::config::Gameplay::boot_master_volume_enabled), off
//! by default. Off ⇒ we never touch the volume, so the player's own setting stands; forcing a volume
//! on every launch otherwise would override the in-game slider for people who never asked for it.
//!
//! The SDK charts master volume as `GameDataMan::game_settings.master_volume` (a `u8`, range 0..=10 —
//! same range as [`boot_master_volume`](unseamless_core::config::Gameplay::boot_master_volume),
//! so the config value maps straight through). `GameDataMan` isn't up at `app::install` time, hence a
//! `Feature` that retries each frame until the write lands rather than a one-shot patch in `install`.
//!
//! **Why re-assert instead of write-once (rig-confirmed):** a single boot write *does* take effect
//! (the rig heard the title go silent under a forced 0), but the game's saved sound-options load runs
//! a few seconds later — at **main-menu entry** — and clobbers `master_volume` back to the saved
//! value. A one-shot write is therefore lost. So we re-assert our value every frame for a window that
//! covers main-menu entry ([`REASSERT_SECS`]); the rig confirmed that re-asserting *through* the
//! clobber makes the value stick permanently afterward, so once the window closes we stop and leave
//! the in-game slider free again (toggling the option off doesn't restore the pre-boot value — the
//! write is one-way). The window is time-based (frame delta), so it's the same wall-clock span
//! regardless of frame rate.
//!
//! Re-derive after a game update: if a forced boot volume stops sticking, the clobber timing
//! (main-menu entry) likely shifted; widen [`REASSERT_SECS`] or, if it moved a lot, re-confirm on the
//! rig with a long window (`scripts/rig.sh`) and watch the `boot volume:` log lines below.

use eldenring::cs::GameDataMan;

use unseamless_core::config::MAX_MASTER_VOLUME;

use crate::feature::{Feature, Tick};

/// How long to re-assert the boot volume after the first write. The saved-options load clobbers our
/// value once, at main-menu entry (~10s after boot on the rig); re-asserting across that window
/// defeats the clobber, after which the engine holds our value. 60s gives comfortable margin over the
/// observed ~10s clobber (and over the rig-proven ~50s window) for slower cold loads, then releases
/// the slider. Tunable: the only cost of a longer window is that a player adjusting the in-game volume
/// *during* it gets overridden — acceptable at the title/main-menu, where this feature applies.
const REASSERT_SECS: f32 = 60.0;

#[derive(Default)]
pub struct BootVolume {
    /// Set once the first write lands; the re-assert window then runs until [`done`](Self::done).
    applied: bool,
    /// The value we wrote, re-asserted each frame across the window (and compared against on re-read
    /// to spot the options-load clobber for the log).
    target: u8,
    /// Seconds elapsed since the first write, accumulated from frame delta — drives the window close.
    elapsed: f32,
    /// Whether we've logged the one-time options-load clobber this session (so it's logged once).
    corrected: bool,
    /// Window closed: stop touching the volume and leave the in-game slider free.
    done: bool,
}

impl BootVolume {
    pub fn new() -> Self {
        Self::default()
    }

    /// Write `vol` into `GameDataMan::game_settings.master_volume`, returning the value that was there
    /// *before* the write. Nested `Option`: outer `None` ⇒ `GameDataMan` singleton not up yet; inner
    /// `None` ⇒ singleton up but its `game_settings` `OwnedPtr` not wired (the CLAUDE.md unwired-pointer
    /// caveat — writing through a null/half-init pointer corrupts, so we guard the deref).
    fn write_volume(vol: u8) -> Option<Option<u8>> {
        crate::sdk::with_instance_mut::<GameDataMan, _>(|gd| {
            if gd.game_settings.as_ptr().is_null() {
                return None;
            }
            let before = gd.game_settings.master_volume;
            gd.game_settings.master_volume = vol;
            Some(before)
        })
    }
}

impl Feature for BootVolume {
    fn name(&self) -> &'static str {
        "boot-volume"
    }

    // Default phase (`FrameBegin`): runs at the title/menu before and through main-menu entry, which
    // is exactly the window the saved-options clobber falls in. Not frame-order-sensitive.

    fn on_frame(&mut self, tick: Tick) {
        if self.done {
            return; // window closed: leave the in-game volume option alone for the rest of the session
        }

        // The opt-in gate + clamp is host-tested decision logic in core. `None` ⇒ opt-in off, so leave
        // the player's own volume alone (and keep `applied` false so a mid-session toggle-on still lands).
        let Some(vol) = crate::state::with(|c| c.gameplay.boot_volume_to_apply()) else {
            return;
        };

        if !self.applied {
            // First write. Both not-ready signals (outer/inner `None`) ⇒ retry next frame without latching.
            if let Some(Some(before)) = Self::write_volume(vol) {
                self.applied = true;
                self.target = vol;
                log::info!(
                    "boot master volume set to {vol}/{MAX_MASTER_VOLUME} (was {before}); re-asserting across main-menu entry"
                );
            }
            return;
        }

        // Re-assert window: keep writing our target so the saved-options load (at main-menu entry)
        // can't win, then release. Log the one-time clobber when we see it (debug — hot path).
        self.elapsed += tick.delta;
        if let Some(Some(before)) = Self::write_volume(self.target)
            && before != self.target
            && !self.corrected
        {
            self.corrected = true;
            log::debug!(
                "boot volume: options-load clobbered master_volume to {before}; re-asserted {}",
                self.target
            );
        }
        if self.elapsed >= REASSERT_SECS {
            self.done = true;
            log::debug!("boot volume: re-assert window closed; leaving the in-game slider free");
        }
    }
}
