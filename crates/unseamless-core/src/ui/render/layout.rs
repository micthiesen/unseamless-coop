//! The layout engine: the [`Widget`] trait every renderable implements, the [`Stack`] flexbox-lite
//! container (vertical/horizontal, spacing, padding, per-axis alignment, fixed/hug/fill sizing),
//! viewport [`anchor`]ing + [`center`]ing, and [`clip`]ping for scroll/clip viewports.
//!
//! Everything is **static**: a widget is measured, placed into a `Rect`, and renders into it. There
//! is no dragging, resizing, or snapping (see `docs/UI-LIBRARY.md` > Won't-do) — placement is a pure
//! function of the viewport and the chosen anchor.

use crate::bitmap_font::metrics;

use super::primitives::{DrawCmd, DrawList, Insets, Rect};
use super::theme::Theme;

/// A widget's intrinsic pixel size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

impl Size {
    pub const fn new(w: i32, h: i32) -> Self {
        Self { w, h }
    }
}

/// Anything that can report a hug-contents size and paint itself into a bounds rect. The whole widget
/// set is built from this; composites (`Stack`, `Panel`, `Tabs`, `ScrollView`) hold `Box<dyn Widget>`
/// children.
pub trait Widget {
    /// The size this widget wants when hugging its contents (ignores fill/parent-imposed size).
    fn measure(&self, theme: &Theme) -> Size;

    /// Emit draw commands that fill `bounds`. `bounds` is chosen by the parent/anchor; the widget may
    /// paint a background over all of it and lay contents out within it.
    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList);
}

impl Widget for Box<dyn Widget> {
    fn measure(&self, theme: &Theme) -> Size {
        (**self).measure(theme)
    }
    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        (**self).render(theme, bounds, out)
    }
}

/// A convenience: render a widget into `bounds` and return a fresh [`DrawList`].
pub fn draw(widget: &dyn Widget, theme: &Theme, bounds: Rect) -> DrawList {
    let mut out = DrawList::new();
    widget.render(theme, bounds, &mut out);
    out
}

/// Stack orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    /// `(main, cross)` components of a size for this axis.
    fn main_cross(self, size: Size) -> (i32, i32) {
        match self {
            Axis::Horizontal => (size.w, size.h),
            Axis::Vertical => (size.h, size.w),
        }
    }

    /// Build a [`Size`] from `(main, cross)` components.
    fn size(self, main: i32, cross: i32) -> Size {
        match self {
            Axis::Horizontal => Size::new(main, cross),
            Axis::Vertical => Size::new(cross, main),
        }
    }

    /// Build a [`Rect`] from main/cross position + length.
    fn rect(self, main_pos: i32, cross_pos: i32, main_len: i32, cross_len: i32) -> Rect {
        match self {
            Axis::Horizontal => Rect::new(main_pos, cross_pos, main_len, cross_len),
            Axis::Vertical => Rect::new(cross_pos, main_pos, cross_len, main_len),
        }
    }

    /// `(main_start, cross_start, main_extent, cross_extent)` of a rect along this axis.
    fn parts(self, rect: Rect) -> (i32, i32, i32, i32) {
        match self {
            Axis::Horizontal => (rect.x, rect.y, rect.w, rect.h),
            Axis::Vertical => (rect.y, rect.x, rect.h, rect.w),
        }
    }

    /// `(main, cross)` [`Length`] of a sizing for this axis.
    fn main_cross_len(self, sizing: Sizing) -> (Length, Length) {
        match self {
            Axis::Horizontal => (sizing.width, sizing.height),
            Axis::Vertical => (sizing.height, sizing.width),
        }
    }
}

/// Alignment of content within available space, on one axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    /// The offset that positions `content` within `avail` per this alignment. Never negative (content
    /// larger than the space pins to the start).
    pub fn offset(self, avail: i32, content: i32) -> i32 {
        match self {
            Align::Start => 0,
            Align::Center => ((avail - content) / 2).max(0),
            Align::End => (avail - content).max(0),
        }
    }
}

/// How a child is sized along one axis within a [`Stack`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Length {
    /// Exactly this many pixels.
    Fixed(i32),
    /// The child's intrinsic (measured) size.
    #[default]
    Hug,
    /// Grow to fill the leftover space (shared equally among `Fill` siblings on the main axis; the
    /// full cross extent on the cross axis).
    Fill,
}

impl Length {
    fn is_fill(self) -> bool {
        matches!(self, Length::Fill)
    }

    /// Resolve to a pixel length given the child's `measured` intrinsic and the `avail`able extent
    /// (used for `Fill` on the **cross** axis; main-axis `Fill` is distributed separately).
    fn resolve(self, measured: i32, avail: i32) -> i32 {
        match self {
            Length::Fixed(n) => n,
            Length::Hug => measured,
            Length::Fill => avail,
        }
    }
}

/// A child's sizing on both axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sizing {
    pub width: Length,
    pub height: Length,
}

impl Sizing {
    /// Hug on both axes (the default leaf sizing).
    pub const HUG: Sizing = Sizing { width: Length::Hug, height: Length::Hug };
    /// Fill on both axes — grow to the parent's full extent each way. (For "stretch across the main
    /// axis but hug the cross," use [`Sizing::fill_width`].)
    pub const FILL: Sizing = Sizing { width: Length::Fill, height: Length::Fill };

    pub const fn new(width: Length, height: Length) -> Self {
        Self { width, height }
    }

    /// Fill the width, hug the height.
    pub const fn fill_width() -> Self {
        Self { width: Length::Fill, height: Length::Hug }
    }
}

struct Child {
    widget: Box<dyn Widget>,
    sizing: Sizing,
}

/// A flexbox-lite container: lays children along one [`Axis`] with `spacing` between them, inside
/// `pad`, aligning the content block (`main_align`) and each child on the cross axis (`cross_align`).
/// Children size via [`Sizing`] (fixed / hug / fill). This is the only layout container — padding,
/// alignment, and sizing are all expressed through it.
pub struct Stack {
    axis: Axis,
    spacing: i32,
    pad: Insets,
    main_align: Align,
    cross_align: Align,
    children: Vec<Child>,
}

impl Stack {
    /// A vertical stack (children top-to-bottom).
    pub fn vertical() -> Self {
        Self::new(Axis::Vertical)
    }

    /// A horizontal stack (children left-to-right).
    pub fn horizontal() -> Self {
        Self::new(Axis::Horizontal)
    }

    fn new(axis: Axis) -> Self {
        Self {
            axis,
            spacing: 0,
            pad: Insets::default(),
            main_align: Align::Start,
            cross_align: Align::Start,
            children: Vec::new(),
        }
    }

    /// Set the gap between children (pixels).
    pub fn spacing(mut self, spacing: i32) -> Self {
        self.spacing = spacing;
        self
    }

    /// Set the inner padding.
    pub fn pad(mut self, pad: Insets) -> Self {
        self.pad = pad;
        self
    }

    /// Align the content block along the main axis (only visible when no child fills it).
    pub fn main_align(mut self, align: Align) -> Self {
        self.main_align = align;
        self
    }

    /// Align each child along the cross axis.
    pub fn cross_align(mut self, align: Align) -> Self {
        self.cross_align = align;
        self
    }

    /// Add a hug-sized child.
    pub fn child(self, widget: impl Widget + 'static) -> Self {
        self.child_sized(widget, Sizing::HUG)
    }

    /// Add a child with explicit sizing.
    pub fn child_sized(mut self, widget: impl Widget + 'static, sizing: Sizing) -> Self {
        self.children.push(Child { widget: Box::new(widget), sizing });
        self
    }
}

/// One child resolved for a given parent bounds: its main length (`None` while it's a main-axis fill
/// awaiting leftover distribution) and its final cross length.
struct Resolved {
    main_fixed: Option<i32>,
    cross_len: i32,
}

impl Stack {
    /// Resolve every child's main/cross lengths against `inner` (the padded content box).
    fn resolve(&self, theme: &Theme, inner: Rect) -> Vec<Resolved> {
        let (_, _, _, cross_avail) = self.axis.parts(inner);
        self.children
            .iter()
            .map(|c| {
                let (mm, mc) = self.axis.main_cross(c.widget.measure(theme));
                let (main_len, cross_len) = self.axis.main_cross_len(c.sizing);
                Resolved {
                    main_fixed: (!main_len.is_fill()).then(|| main_len.resolve(mm, 0)),
                    cross_len: cross_len.resolve(mc, cross_avail),
                }
            })
            .collect()
    }
}

impl Widget for Stack {
    fn measure(&self, theme: &Theme) -> Size {
        let mut main = 0;
        let mut cross = 0;
        for c in &self.children {
            let (mm, mc) = self.axis.main_cross(c.widget.measure(theme));
            let (main_len, cross_len) = self.axis.main_cross_len(c.sizing);
            // A Fill child hugs (its measured size) for the stack's own intrinsic measure.
            main += match main_len {
                Length::Fixed(n) => n,
                _ => mm,
            };
            cross = cross.max(match cross_len {
                Length::Fixed(n) => n,
                _ => mc,
            });
        }
        if self.children.len() > 1 {
            main += self.spacing * (self.children.len() as i32 - 1);
        }
        let (pad_main, pad_cross) =
            self.axis.main_cross(Size::new(self.pad.horizontal(), self.pad.vertical()));
        self.axis.size(main + pad_main, cross + pad_cross)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        if self.children.is_empty() {
            return;
        }
        let inner = bounds.inset(self.pad);
        let (main_start, cross_start, main_avail, cross_avail) = self.axis.parts(inner);
        let resolved = self.resolve(theme, inner);

        let fill_count = resolved.iter().filter(|r| r.main_fixed.is_none()).count();
        let fixed_main: i32 = resolved.iter().filter_map(|r| r.main_fixed).sum();
        let spacing_total = self.spacing * (self.children.len() as i32 - 1);
        let leftover = (main_avail - fixed_main - spacing_total).max(0);
        let per_fill = if fill_count > 0 { leftover / fill_count as i32 } else { 0 };
        // Hand the integer remainder to the first fills so the row exactly fills the space.
        let mut fill_rem = if fill_count > 0 { leftover % fill_count as i32 } else { 0 };

        // Justify the content block on the main axis only when nothing fills the leftover.
        let mut cursor = main_start
            + if fill_count == 0 {
                self.main_align.offset(main_avail, fixed_main + spacing_total)
            } else {
                0
            };

        for (child, r) in self.children.iter().zip(&resolved) {
            // `.max(0)` so a stray negative `Fixed` can't drive the cursor backward into a prior
            // child (it just collapses to a zero-extent rect, which `DrawList::rect` skips).
            let main_len = match r.main_fixed {
                Some(v) => v.max(0),
                None => per_fill + if fill_rem > 0 { fill_rem -= 1; 1 } else { 0 },
            };
            let cross_pos = cross_start + self.cross_align.offset(cross_avail, r.cross_len);
            let rect = self.axis.rect(cursor, cross_pos, main_len, r.cross_len);
            child.widget.render(theme, rect, out);
            cursor += main_len + self.spacing;
        }
    }
}

/// Where in a viewport a fixed-size element sits. Toasts → `TopRight`, watermark → `TopLeft`, banners
/// → `TopCenter`, modals → `Center`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Anchor {
    /// The horizontal alignment this anchor implies (its left/center/right side). The single source
    /// for it — `toast_stack` and any other corner-stack consume this rather than re-deriving.
    pub(crate) fn horizontal(self) -> Align {
        match self {
            Anchor::TopLeft | Anchor::CenterLeft | Anchor::BottomLeft => Align::Start,
            Anchor::TopCenter | Anchor::Center | Anchor::BottomCenter => Align::Center,
            Anchor::TopRight | Anchor::CenterRight | Anchor::BottomRight => Align::End,
        }
    }

    fn vertical(self) -> Align {
        match self {
            Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight => Align::Start,
            Anchor::CenterLeft | Anchor::Center | Anchor::CenterRight => Align::Center,
            Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => Align::End,
        }
    }
}

/// Place a `size`-sized box inside `viewport` at `anchor`, keeping `margin` pixels from the edges it
/// hugs. The margin only applies to edges the anchor is pinned to (a centered box ignores it on that
/// axis). Pure — this is the whole of our "window positioning".
pub fn anchor(size: Size, viewport: Rect, anchor: Anchor, margin: i32) -> Rect {
    let avail_w = viewport.w - 2 * margin;
    let avail_h = viewport.h - 2 * margin;
    let x = viewport.x + margin + anchor.horizontal().offset(avail_w, size.w);
    let y = viewport.y + margin + anchor.vertical().offset(avail_h, size.h);
    Rect::new(x, y, size.w, size.h)
}

/// Center a `size`-sized box in `viewport` (modals). Equivalent to [`anchor`] with `Center` + no
/// margin.
pub fn center(size: Size, viewport: Rect) -> Rect {
    anchor(size, viewport, Anchor::Center, 0)
}

/// Clip a sequence of draw commands to `clip`, returning only what's visible:
/// - **Rects** are intersected to the pixel.
/// - **Text** is clipped at whole glyph-cell granularity (a cell shows iff it's fully inside `clip`),
///   so the result stays font-agnostic — a kept run is the original substring at its original pen
///   position. Cells straddling the clip edge are dropped, which is what a scroll viewport wants.
///
/// This is how [`super::widgets::ScrollView`] and any clip viewport stay inside their bounds without a
/// clip command in the draw-list contract. Font metrics come from each text run's own `face`.
pub fn clip(cmds: &[DrawCmd], clip: Rect) -> Vec<DrawCmd> {
    let mut out = Vec::new();
    for cmd in cmds {
        match cmd {
            DrawCmd::Rect { rect, color } => {
                if let Some(r) = rect.intersect(clip) {
                    out.push(DrawCmd::Rect { rect: r, color: *color });
                }
            }
            DrawCmd::Text { pos, text, face, color } => {
                clip_text(*pos, text, *face, *color, clip, &mut out);
            }
        }
    }
    out
}

/// Clip a single text run to `clip` at glyph-cell granularity, pushing the surviving runs. Each kept
/// maximal run of consecutive visible cells becomes one `Text` cmd at its starting pen position.
fn clip_text(
    pos: [i32; 2],
    text: &str,
    face: crate::bitmap_font::Face,
    color: super::primitives::Rgba,
    clip: Rect,
    out: &mut Vec<DrawCmd>,
) {
    let m = metrics(face);
    for (line_idx, line) in text.split('\n').enumerate() {
        let line_y = pos[1] + line_idx as i32 * m.line_height;
        // A cell's vertical band must sit fully within the clip for the line to show at all.
        let vertical_ok = line_y >= clip.y && line_y + m.line_height <= clip.bottom();
        if !vertical_ok {
            continue;
        }
        // Walk cells, grouping consecutive visible ones into runs.
        let mut run_start: Option<usize> = None; // char index where the current run began
        let mut run = String::new();
        let chars: Vec<char> = line.chars().collect();
        let flush = |run: &mut String, run_start: &mut Option<usize>, out: &mut Vec<DrawCmd>| {
            if let Some(start_col) = *run_start
                && !run.is_empty()
            {
                let x = pos[0] + start_col as i32 * m.advance;
                out.push(DrawCmd::Text { pos: [x, line_y], text: std::mem::take(run), face, color });
            }
            *run_start = None;
        };
        for (col, &ch) in chars.iter().enumerate() {
            let cell_x = pos[0] + col as i32 * m.advance;
            let visible = cell_x >= clip.x && cell_x + m.advance <= clip.right();
            if visible {
                if run_start.is_none() {
                    run_start = Some(col);
                }
                run.push(ch);
            } else {
                flush(&mut run, &mut run_start, out);
            }
        }
        flush(&mut run, &mut run_start, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitmap_font::Face;
    use crate::ui::render::primitives::rgb;

    #[test]
    fn align_offset_never_negative() {
        assert_eq!(Align::Start.offset(10, 4), 0);
        assert_eq!(Align::Center.offset(10, 4), 3);
        assert_eq!(Align::End.offset(10, 4), 6);
        assert_eq!(Align::Center.offset(4, 10), 0, "content larger than space pins to start");
    }

    #[test]
    fn anchor_pins_to_corners_with_margin() {
        let vp = Rect::new(0, 0, 100, 100);
        let sz = Size::new(20, 10);
        assert_eq!(anchor(sz, vp, Anchor::TopLeft, 5), Rect::new(5, 5, 20, 10));
        assert_eq!(anchor(sz, vp, Anchor::TopRight, 5), Rect::new(75, 5, 20, 10));
        assert_eq!(anchor(sz, vp, Anchor::BottomRight, 5), Rect::new(75, 85, 20, 10));
        assert_eq!(center(sz, vp), Rect::new(40, 45, 20, 10));
    }

    #[test]
    fn clip_intersects_rects_and_drops_outside_cells() {
        let m = metrics(Face::Menu);
        // Clip window two cells wide starting at the origin.
        let clip_rect = Rect::new(0, 0, m.advance * 2, m.line_height);
        let cmds = vec![
            DrawCmd::Rect { rect: Rect::new(-5, 0, 10, m.line_height), color: rgb(1, 2, 3) },
            DrawCmd::Text { pos: [0, 0], text: "ABCD".into(), face: Face::Menu, color: rgb(4, 5, 6) },
        ];
        let clipped = clip(&cmds, clip_rect);
        // Rect intersected to the clip's left edge.
        assert_eq!(clipped[0], DrawCmd::Rect { rect: Rect::new(0, 0, 5, m.line_height), color: rgb(1, 2, 3) });
        // Only the first two cells ("AB") survive; "CD" fall outside the 2-cell window.
        assert_eq!(
            clipped[1],
            DrawCmd::Text { pos: [0, 0], text: "AB".into(), face: Face::Menu, color: rgb(4, 5, 6) }
        );
    }

    #[test]
    fn clip_keeps_a_middle_run_and_splits_around_a_hole() {
        // A clip window covering cells [1,4): the run "BCD" survives, "A" (col 0) and "E" (col 4) drop.
        let m = metrics(Face::Menu);
        let clip_rect = Rect::new(m.advance, 0, m.advance * 3, m.line_height);
        let cmds = vec![DrawCmd::Text {
            pos: [0, 0],
            text: "ABCDE".into(),
            face: Face::Menu,
            color: rgb(7, 8, 9),
        }];
        let clipped = clip(&cmds, clip_rect);
        assert_eq!(
            clipped,
            vec![DrawCmd::Text {
                pos: [m.advance, 0],
                text: "BCD".into(),
                face: Face::Menu,
                color: rgb(7, 8, 9)
            }]
        );
    }
}
