//! Ages the shared notifications once per frame.
//!
//! The [`Notifications`](unseamless_core::notifications::Notifications) model must be `tick`ed
//! **exactly once per frame** with the engine delta — not once per feature, or toasts would age N×
//! too fast with N features. A dedicated feature makes that "once per frame" unambiguous: it owns no
//! state, it just advances the shared notifications by this frame's `delta`.

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct NotificationsTick;

impl NotificationsTick {
    pub fn new() -> Self {
        Self
    }
}

impl Feature for NotificationsTick {
    fn name(&self) -> &'static str {
        "notifications"
    }

    // Default phase (`FrameBegin`): ticks every frame including menus/title, where toasts also show.

    fn on_frame(&mut self, tick: Tick) {
        crate::notify::with_mut(|n| n.tick(tick.delta));
    }
}
