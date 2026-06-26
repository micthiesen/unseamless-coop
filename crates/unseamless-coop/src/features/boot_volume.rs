//! Boot master volume: set the game's master volume once at startup to the configured level, so the
//! game doesn't always come up at its saved volume (handy paired with `skip_splash_screens`, which
//! drops you onto the loud title fast).
//!
//! **Opt-in:** gated on [`boot_master_volume_enabled`](unseamless_core::config::Gameplay::boot_master_volume_enabled), off
//! by default. Off ⇒ we never touch the volume, so the player's own setting stands; forcing a volume
//! on every launch otherwise would override the in-game slider for people who never asked for it.
//!
//! The SDK charts master volume as `GameDataMan::game_settings.master_volume` (a `u8`, range 0..=10 —
//! same range as [`boot_master_volume`](unseamless_core::config::Gameplay::boot_master_volume),
//! so the config value maps straight through). We write it **once**, as soon as the `GameDataMan`
//! singleton is live, then become a no-op for the rest of the session — deliberately *not* a per-frame
//! hold like `world_time`/`seamless`, so the player can still change volume in the in-game options
//! menu afterward. `GameDataMan` isn't up at `app::install` time, hence a `Feature` that retries each
//! frame until it lands rather than a one-shot patch in `install`.
//!
//! The latch is per-session: with the opt-in off we keep polling cheaply (so toggling it **on**
//! mid-session via the overlay applies on the next frame), but once a write lands we're done for the
//! session — toggling off then on again won't re-apply, and toggling off won't restore the pre-boot
//! volume (the write is one-way).
//!
//! **Rig-TODO (two unknowns, confirm on the rig):**
//!  1. *Does the audio engine pick up a direct field write live*, or only when the options menu
//!     applies a change? If volume doesn't actually change, this needs to additionally poke whatever
//!     the options-apply path calls (a follow-up; the field write is the cheap first attempt).
//!  2. *Timing / clobber:* if the game loads the saved sound options into `game_settings` slightly
//!     *after* our first successful write, it would overwrite our value. If the rig shows that, switch
//!     from apply-once to re-asserting for a short window after the singleton appears, then releasing.

use eldenring::cs::GameDataMan;

use unseamless_core::config::MAX_MASTER_VOLUME;

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct BootVolume {
    /// Set once we've written the volume, so we stop touching it and leave the in-game slider free.
    applied: bool,
}

impl BootVolume {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Feature for BootVolume {
    fn name(&self) -> &'static str {
        "boot-volume"
    }

    // Default phase (`FrameBegin`): runs at the title/menu before a save loads, which is exactly when
    // a boot volume should land. Not frame-order-sensitive — it's a settings write, applied once.

    fn on_frame(&mut self, _tick: Tick) {
        if self.applied {
            return; // one-shot: leave the in-game volume option alone after the boot write
        }

        // The opt-in gate + clamp is host-tested decision logic in core. `None` ⇒ opt-in off, so leave
        // the player's own volume alone (and keep `applied` false so a mid-session toggle-on still lands).
        let Some(vol) = crate::state::with(|c| c.gameplay.boot_volume_to_apply()) else {
            return;
        };

        // Two not-ready signals, both ⇒ retry next frame without latching:
        //  - outer `None`: `GameDataMan` singleton not up yet (earliest boot).
        //  - `Some(false)`: singleton up but its `game_settings` `OwnedPtr` not wired yet. Guard the
        //    nested deref — the registry can surface a singleton before its members are populated (the
        //    CLAUDE.md unwired-pointer caveat), and writing through a null/half-init pointer corrupts.
        let landed = crate::sdk::with_instance_mut::<GameDataMan, _>(|gd| {
            if gd.game_settings.as_ptr().is_null() {
                return false;
            }
            gd.game_settings.master_volume = vol;
            true
        });
        if landed == Some(true) {
            self.applied = true;
            log::info!("boot master volume set to {vol}/{MAX_MASTER_VOLUME}");
        }
    }
}
