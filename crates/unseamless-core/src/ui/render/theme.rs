//! The one [`Theme`]: palette + spacing constants + the two faces. Declared once; every widget reads
//! it so colors and metrics aren't scattered across the widget set. See `docs/UI-LIBRARY.md` > Theme.
//!
//! The palette mirrors the spirit of [`crate::palette`] (clear, high-value hues) but lives here as
//! [`Rgba`] because UI chrome is screen-space 8-bit color, not the `[f32; 3]` per-peer nameplate
//! tints. Severity colors reuse [`crate::notifications::Severity`] so toasts/banners stay consistent
//! with the notification model.

use crate::bitmap_font::Face;
use crate::notifications::Severity;

use super::primitives::{rgb, Insets, Rgba};

/// Palette + spacing + faces. Widgets pull every color and metric from here, so restyling is a
/// single-struct edit. Construct [`Theme::default`] for the shipping look, or build one by hand in
/// tests with cell-aligned spacing derived from [`crate::bitmap_font::metrics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Screen/background fill behind everything.
    pub bg: Rgba,
    /// Panel/window/toast background.
    pub panel: Rgba,
    /// Primary text.
    pub fg: Rgba,
    /// Dimmed text/elements (disabled rows, secondary info).
    pub dim: Rgba,
    /// Selection / active-tab highlight fill.
    pub accent: Rgba,
    /// Text drawn on top of an [`accent`](Self::accent) highlight (for contrast).
    pub on_accent: Rgba,
    /// Border / divider color.
    pub border: Rgba,
    /// Severity → info color.
    pub info: Rgba,
    /// Severity → warning color.
    pub warning: Rgba,
    /// Severity → error color.
    pub error: Rgba,
    /// The interactive-menu face (windows, modals, lists).
    pub menu_face: Face,
    /// The compact glanceable face (toasts, dense info).
    pub compact_face: Face,
    /// Default inner padding for panels/banners, in pixels.
    pub pad: Insets,
    /// Default gap between stacked elements, in pixels.
    pub gap: i32,
    /// Border / divider thickness, in pixels.
    pub border_w: i32,
}

impl Theme {
    /// The color for a [`Severity`] — the single mapping toasts and banners share.
    pub fn severity(&self, severity: Severity) -> Rgba {
        match severity {
            Severity::Info => self.info,
            Severity::Warning => self.warning,
            Severity::Error => self.error,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        // Derive spacing from the menu face's live metrics so it stays one cell each axis after a
        // font swap — never freeze Spleen's 8×16 as a literal (font-agnosticism is a hard rule).
        let m = crate::bitmap_font::metrics(Face::Menu);
        Self {
            bg: rgb(18, 18, 22),
            panel: rgb(34, 36, 44),
            fg: rgb(232, 232, 238),
            dim: rgb(124, 128, 140),
            accent: rgb(82, 150, 232),
            on_accent: rgb(12, 14, 20),
            border: rgb(96, 100, 112),
            info: rgb(96, 168, 240),
            warning: rgb(240, 196, 72),
            error: rgb(236, 96, 96),
            menu_face: Face::Menu,
            compact_face: Face::Compact,
            // One menu cell of padding each axis (advance × line-height); cell-aligned by default.
            pad: Insets::symmetric(m.advance, m.line_height),
            gap: m.line_height,
            border_w: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_maps_to_distinct_colors() {
        let t = Theme::default();
        assert_eq!(t.severity(Severity::Info), t.info);
        assert_eq!(t.severity(Severity::Warning), t.warning);
        assert_eq!(t.severity(Severity::Error), t.error);
        assert_ne!(t.severity(Severity::Info), t.severity(Severity::Error));
    }
}
