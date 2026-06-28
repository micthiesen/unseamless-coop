//! The widget set: every widget implements [`Widget`] and emits into a [`DrawList`]. Selection index,
//! active tab, scroll offset, and toast lifetimes are **plain input data** — this is the view half, so
//! it never touches input handling (that's `ui::input`). Widgets read all colors/metrics from the
//! [`Theme`]; text sizing comes only from `bitmap_font::{metrics, measure}`, never hardcoded cell
//! sizes, so a font swap can't break them.

use crate::bitmap_font::{measure, metrics, Face};
use crate::notifications::{Severity, Toast};

use super::layout::{anchor as place_anchor, Align, Anchor, Size, Widget};
use super::layout::{clip, Stack};
use super::primitives::{with_alpha, DrawList, Insets, Rect, Rgba};
use super::theme::Theme;

/// Mask character for an un-revealed secret value (e.g. the co-op password row).
const SECRET_MASK: char = '*';

/// A single line of text in one face/color. Defaults to the theme foreground.
pub struct Label {
    pub text: String,
    pub face: Face,
    pub color: Option<Rgba>,
}

impl Label {
    /// A menu-face label in the default foreground color.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), face: Face::Menu, color: None }
    }

    pub fn face(mut self, face: Face) -> Self {
        self.face = face;
        self
    }

    pub fn color(mut self, color: Rgba) -> Self {
        self.color = Some(color);
        self
    }
}

impl Widget for Label {
    fn measure(&self, _theme: &Theme) -> Size {
        let (w, h) = measure(&self.text, self.face);
        Size::new(w, h)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        out.text([bounds.x, bounds.y], self.text.clone(), self.face, self.color.unwrap_or(theme.fg));
    }
}

/// A thin separator line. Fills the cross axis of its bounds with `thickness` pixels, centered.
pub struct Divider {
    pub thickness: i32,
    pub color: Option<Rgba>,
    pub horizontal: bool,
}

impl Divider {
    /// A horizontal divider (a `Fill`-width row); thickness defaults from the theme at render time.
    pub fn horizontal() -> Self {
        Self { thickness: 0, color: None, horizontal: true }
    }

    /// A vertical divider (a `Fill`-height column).
    pub fn vertical() -> Self {
        Self { thickness: 0, color: None, horizontal: false }
    }

    pub fn thickness(mut self, thickness: i32) -> Self {
        self.thickness = thickness;
        self
    }

    pub fn color(mut self, color: Rgba) -> Self {
        self.color = Some(color);
        self
    }

    fn t(&self, theme: &Theme) -> i32 {
        if self.thickness > 0 { self.thickness } else { theme.border_w }
    }
}

impl Widget for Divider {
    fn measure(&self, theme: &Theme) -> Size {
        let t = self.t(theme);
        if self.horizontal { Size::new(0, t) } else { Size::new(t, 0) }
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        let t = self.t(theme);
        let color = self.color.unwrap_or(theme.border);
        // Center the line in its bounds via `Align::Center` (which clamps, so an oversized thickness
        // pins to the edge instead of escaping the box).
        let rect = if self.horizontal {
            Rect::new(bounds.x, bounds.y + Align::Center.offset(bounds.h, t), bounds.w, t)
        } else {
            Rect::new(bounds.x + Align::Center.offset(bounds.w, t), bounds.y, t, bounds.h)
        };
        out.rect(rect, color);
    }
}

/// A background panel: a filled rect with an optional border, optional title bar, and inner padding
/// around a single child. The frame for windows, modals, toasts, and banners. Static — no drag/resize.
pub struct Panel {
    child: Box<dyn Widget>,
    bg: Option<Rgba>,
    border: bool,
    title: Option<String>,
    pad: Option<Insets>,
}

impl Panel {
    pub fn new(child: impl Widget + 'static) -> Self {
        Self { child: Box::new(child), bg: None, border: false, title: None, pad: None }
    }

    /// Override the background fill (defaults to `theme.panel`).
    pub fn bg(mut self, bg: Rgba) -> Self {
        self.bg = Some(bg);
        self
    }

    /// Draw a border around the panel.
    pub fn border(mut self) -> Self {
        self.border = true;
        self
    }

    /// Add a title bar (accent strip + title text) above the content.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Override the inner padding (defaults to `theme.pad`).
    pub fn pad(mut self, pad: Insets) -> Self {
        self.pad = Some(pad);
        self
    }

    fn pad_of(&self, theme: &Theme) -> Insets {
        self.pad.unwrap_or(theme.pad)
    }

    fn border_w(&self, theme: &Theme) -> i32 {
        if self.border { theme.border_w } else { 0 }
    }

    fn title_h(&self, theme: &Theme) -> i32 {
        if self.title.is_some() { metrics(theme.menu_face).line_height } else { 0 }
    }
}

impl Widget for Panel {
    fn measure(&self, theme: &Theme) -> Size {
        let pad = self.pad_of(theme);
        let bw = self.border_w(theme);
        let child = self.child.measure(theme);
        let title_w = self
            .title
            .as_deref()
            .map(|t| measure(t, theme.menu_face).0)
            .unwrap_or(0);
        let content_w = child.w.max(title_w);
        Size::new(
            content_w + pad.horizontal() + 2 * bw,
            child.h + pad.vertical() + self.title_h(theme) + 2 * bw,
        )
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        out.rect(bounds, self.bg.unwrap_or(theme.panel));

        let bw = self.border_w(theme);
        if self.border {
            draw_border(bounds, bw, theme.border, out);
        }

        let frame = bounds.inset(Insets::all(bw));
        let content_area = if let Some(title) = &self.title {
            let title_h = self.title_h(theme);
            let pad = self.pad_of(theme);
            let title_rect = Rect::new(frame.x, frame.y, frame.w, title_h);
            out.rect(title_rect, theme.accent);
            out.text([frame.x + pad.left, frame.y], title.clone(), theme.menu_face, theme.on_accent);
            // Clamp so a panel shorter than its title bar hands the child a zero-height (not
            // negative-extent) rect.
            Rect::new(frame.x, frame.y + title_h, frame.w, (frame.h - title_h).max(0))
        } else {
            frame
        };

        self.child.render(theme, content_area.inset(self.pad_of(theme)), out);
    }
}

/// Stroke a `width`-thick border just inside `bounds` (four edge rects).
fn draw_border(bounds: Rect, width: i32, color: Rgba, out: &mut DrawList) {
    if width <= 0 {
        return;
    }
    out.rect(Rect::new(bounds.x, bounds.y, bounds.w, width), color); // top
    out.rect(Rect::new(bounds.x, bounds.bottom() - width, bounds.w, width), color); // bottom
    out.rect(Rect::new(bounds.x, bounds.y, width, bounds.h), color); // left
    out.rect(Rect::new(bounds.right() - width, bounds.y, width, bounds.h), color); // right
}

/// Test hook: the border commands [`draw_border`] would emit, as a list (hairlines are sub-cell so
/// they don't show in the ASCII rasterizer — assert on the commands instead).
#[cfg(test)]
pub(crate) fn draw_border_for_test(bounds: Rect, width: i32, color: Rgba) -> Vec<super::primitives::DrawCmd> {
    let mut out = DrawList::new();
    draw_border(bounds, width, color, &mut out);
    out.0
}

/// One row of a [`List`] (also usable standalone). A plain text row (`value == None`), a `key: value`
/// row, or a secret row whose value is masked until `revealed`.
#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    pub value: Option<String>,
    pub secret: bool,
    pub revealed: bool,
    pub enabled: bool,
}

impl Row {
    /// A plain text row.
    pub fn text(label: impl Into<String>) -> Self {
        Self { label: label.into(), value: None, secret: false, revealed: false, enabled: true }
    }

    /// A `key: value` row (value drawn right-aligned).
    pub fn kv(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { value: Some(value.into()), ..Self::text(label) }
    }

    /// Mark this row as a secret value, masked unless `revealed`.
    pub fn secret(mut self, revealed: bool) -> Self {
        self.secret = true;
        self.revealed = revealed;
        self
    }

    /// Mark this row disabled (drawn dim; selection skips it in `ui::input`).
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// The value as it should be displayed (masked if a hidden secret).
    fn display_value(&self) -> Option<String> {
        self.value.as_ref().map(|v| {
            if self.secret && !self.revealed {
                SECRET_MASK.to_string().repeat(v.chars().count())
            } else {
                v.clone()
            }
        })
    }

    fn intrinsic(&self, face: Face) -> Size {
        let m = metrics(face);
        let label_w = measure(&self.label, face).0;
        // The masked value has the same width as the raw one (one mask glyph per char), so measure
        // the raw value directly — no need to allocate the masked string just to size it.
        let w = match &self.value {
            // At least one cell of gap between label and a right-aligned value.
            Some(v) => label_w + m.advance + measure(v, face).0,
            None => label_w,
        };
        Size::new(w, m.line_height)
    }
}

impl Widget for Row {
    fn measure(&self, theme: &Theme) -> Size {
        self.intrinsic(theme.menu_face)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        draw_row(self, theme, bounds, theme.menu_face, false, out);
    }
}

/// Draw a row's text into `bounds`. The caller paints any selection highlight first; `selected` only
/// picks the contrasting text color. Disabled rows draw dim.
fn draw_row(row: &Row, theme: &Theme, bounds: Rect, face: Face, selected: bool, out: &mut DrawList) {
    let color = if !row.enabled {
        theme.dim
    } else if selected {
        theme.on_accent
    } else {
        theme.fg
    };
    out.text([bounds.x, bounds.y], row.label.clone(), face, color);
    if let Some(value) = row.display_value() {
        let vw = measure(&value, face).0;
        // Right-align the value, but never let it overlap the label.
        let label_end = bounds.x + measure(&row.label, face).0 + metrics(face).advance;
        let x = (bounds.right() - vw).max(label_end);
        out.text([x, bounds.y], value, face, color);
    }
}

/// A vertical list of [`Row`]s with an optional selected-row highlight. Rows are one line tall, with
/// `row_gap` pixels between them. `selected` is plain input data from `ui::input`.
pub struct List {
    pub rows: Vec<Row>,
    pub selected: Option<usize>,
    pub face: Face,
    pub row_gap: i32,
}

impl List {
    pub fn new(rows: Vec<Row>) -> Self {
        Self { rows, selected: None, face: Face::Menu, row_gap: 0 }
    }

    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    pub fn face(mut self, face: Face) -> Self {
        self.face = face;
        self
    }

    pub fn row_gap(mut self, row_gap: i32) -> Self {
        self.row_gap = row_gap;
        self
    }

    fn row_h(&self) -> i32 {
        metrics(self.face).line_height
    }
}

impl Widget for List {
    fn measure(&self, _theme: &Theme) -> Size {
        let w = self.rows.iter().map(|r| r.intrinsic(self.face).w).max().unwrap_or(0);
        let n = self.rows.len() as i32;
        let h = if n == 0 { 0 } else { n * self.row_h() + self.row_gap * (n - 1) };
        Size::new(w, h)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        let row_h = self.row_h();
        let mut y = bounds.y;
        for (i, row) in self.rows.iter().enumerate() {
            let row_rect = Rect::new(bounds.x, y, bounds.w, row_h);
            let selected = self.selected == Some(i);
            if selected {
                out.rect(row_rect, theme.accent);
            }
            draw_row(row, theme, row_rect, self.face, selected, out);
            y += row_h + self.row_gap;
        }
    }
}

/// A tab strip (active tab highlighted) above a content area, separated by a divider. `active` is
/// plain input data; this widget renders the strip + the active page's content, nothing more.
pub struct Tabs {
    pub tabs: Vec<String>,
    pub active: usize,
    content: Box<dyn Widget>,
    pub face: Face,
    pub tab_gap: i32,
    pub tab_pad: i32,
}

impl Tabs {
    pub fn new(tabs: Vec<String>, active: usize, content: impl Widget + 'static) -> Self {
        Self { tabs, active, content: Box::new(content), face: Face::Menu, tab_gap: 0, tab_pad: 0 }
    }

    pub fn face(mut self, face: Face) -> Self {
        self.face = face;
        self
    }

    /// Gap between adjacent tabs (pixels).
    pub fn tab_gap(mut self, tab_gap: i32) -> Self {
        self.tab_gap = tab_gap;
        self
    }

    /// Horizontal padding inside each tab, so the active tab's highlight extends past its label.
    pub fn tab_pad(mut self, tab_pad: i32) -> Self {
        self.tab_pad = tab_pad;
        self
    }

    fn strip_h(&self) -> i32 {
        metrics(self.face).line_height
    }

    /// Width of one tab cell (label plus padding on both sides).
    fn tab_w(&self, label: &str) -> i32 {
        measure(label, self.face).0 + 2 * self.tab_pad
    }

    fn strip_w(&self) -> i32 {
        let labels: i32 = self.tabs.iter().map(|t| self.tab_w(t)).sum();
        let gaps = self.tab_gap * (self.tabs.len() as i32 - 1).max(0);
        labels + gaps
    }
}

impl Widget for Tabs {
    fn measure(&self, theme: &Theme) -> Size {
        let content = self.content.measure(theme);
        let w = self.strip_w().max(content.w);
        let h = self.strip_h() + theme.border_w + content.h;
        Size::new(w, h)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        let strip_h = self.strip_h();
        let mut x = bounds.x;
        for (i, tab) in self.tabs.iter().enumerate() {
            let tab_rect = Rect::new(x, bounds.y, self.tab_w(tab), strip_h);
            let active = i == self.active;
            if active {
                out.rect(tab_rect, theme.accent);
            }
            let color = if active { theme.on_accent } else { theme.fg };
            out.text([x + self.tab_pad, bounds.y], tab.clone(), self.face, color);
            x += tab_rect.w + self.tab_gap;
        }

        let divider_y = bounds.y + strip_h;
        out.rect(Rect::new(bounds.x, divider_y, bounds.w, theme.border_w), theme.border);

        let content_top = divider_y + theme.border_w;
        let content_rect = Rect::new(bounds.x, content_top, bounds.w, bounds.bottom() - content_top);
        self.content.render(theme, content_rect, out);
    }
}

/// A centered modal: a bordered, titled panel listing `options` with the `selected` option
/// highlighted. Build it, then [`center`](super::layout::center) it in the viewport. `selected` is
/// plain input data.
pub struct Modal {
    pub title: Option<String>,
    pub options: Vec<String>,
    pub selected: usize,
    pub face: Face,
}

impl Modal {
    pub fn new(options: Vec<String>, selected: usize) -> Self {
        Self { title: None, options, selected, face: Face::Menu }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn face(mut self, face: Face) -> Self {
        self.face = face;
        self
    }

    /// The panel this modal renders as (a bordered panel wrapping a selectable list).
    fn build(&self) -> Panel {
        let rows = self.options.iter().map(Row::text).collect();
        let list = List::new(rows).selected(Some(self.selected)).face(self.face);
        let panel = Panel::new(list).border();
        match &self.title {
            Some(t) => panel.title(t.clone()),
            None => panel,
        }
    }
}

impl Widget for Modal {
    fn measure(&self, theme: &Theme) -> Size {
        self.build().measure(theme)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        self.build().render(theme, bounds, out);
    }
}

/// A full-width severity-colored strip with a centered message — notification banners and the
/// rig-guide banner. Anchor it to a viewport edge (top-center for notifications).
pub struct Banner {
    pub message: String,
    pub severity: Severity,
    pub face: Face,
}

impl Banner {
    pub fn new(message: impl Into<String>, severity: Severity) -> Self {
        Self { message: message.into(), severity, face: Face::Menu }
    }

    pub fn face(mut self, face: Face) -> Self {
        self.face = face;
        self
    }
}

impl Widget for Banner {
    fn measure(&self, theme: &Theme) -> Size {
        let (tw, th) = measure(&self.message, self.face);
        Size::new(tw + theme.pad.horizontal(), th + theme.pad.vertical())
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        out.rect(bounds, theme.severity(self.severity));
        let (tw, th) = measure(&self.message, self.face);
        let x = bounds.x + Align::Center.offset(bounds.w, tw);
        let y = bounds.y + Align::Center.offset(bounds.h, th);
        out.text([x, y], self.message.clone(), self.face, theme.on_accent);
    }
}

/// Fade only kicks in over the final [`FADE_SECS`] of a toast's life — a plain per-frame alpha, not an
/// animation system (see `docs/UI-LIBRARY.md` > Won't-do).
const FADE_SECS: f32 = 0.5;

/// The alpha (`0..=255`) a toast should draw at given its remaining lifetime: fully opaque until the
/// last [`FADE_SECS`], then a linear fade to transparent. Pure function of `remaining`.
pub fn toast_alpha(toast: &Toast) -> u8 {
    let f = (toast.remaining / FADE_SECS).clamp(0.0, 1.0);
    (f * 255.0).round() as u8
}

/// One toast: a panel background with a left severity stripe and a one-line message, all drawn at
/// `alpha`. Built from a [`Toast`]'s data via [`toast_stack`]; exposed for standalone use.
pub struct ToastView {
    pub message: String,
    pub severity: Severity,
    pub alpha: u8,
    pub face: Face,
}

impl ToastView {
    /// Build a view from a notification [`Toast`], computing its fade alpha.
    pub fn from_toast(toast: &Toast, face: Face) -> Self {
        Self {
            message: toast.message.clone(),
            severity: toast.severity,
            alpha: toast_alpha(toast),
            face,
        }
    }

    /// Stripe + gap width to the left of the message: one cell each.
    fn text_x_offset(&self) -> i32 {
        2 * metrics(self.face).advance
    }
}

impl Widget for ToastView {
    fn measure(&self, _theme: &Theme) -> Size {
        let (tw, th) = measure(&self.message, self.face);
        Size::new(self.text_x_offset() + tw, th)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        out.rect(bounds, with_alpha(theme.panel, self.alpha));
        let stripe_w = metrics(self.face).advance;
        out.rect(
            Rect::new(bounds.x, bounds.y, stripe_w, bounds.h),
            with_alpha(theme.severity(self.severity), self.alpha),
        );
        out.text(
            [bounds.x + self.text_x_offset(), bounds.y],
            self.message.clone(),
            self.face,
            with_alpha(theme.fg, self.alpha),
        );
    }
}

/// Lay out a corner stack of toasts anchored in `viewport` (top-right by convention), with `gap`
/// pixels between them, and return the draw list. Each toast aligns to the anchor's horizontal side
/// within the stack so they share an edge; the newest-first ordering is the caller's
/// (`Notifications::toasts` is oldest-first, so reverse if newest-on-top is wanted).
pub fn toast_stack(
    theme: &Theme,
    toasts: &[Toast],
    viewport: Rect,
    anchor: Anchor,
    margin: i32,
    gap: i32,
) -> DrawList {
    let mut out = DrawList::new();
    if toasts.is_empty() {
        return out;
    }
    // Cross-axis alignment of each toast within the stack follows the anchor's horizontal side.
    let mut stack = Stack::vertical().spacing(gap).cross_align(anchor.horizontal());
    for toast in toasts {
        stack = stack.child(ToastView::from_toast(toast, theme.compact_face));
    }
    let bounds = place_anchor(stack.measure(theme), viewport, anchor, margin);
    stack.render(theme, bounds, &mut out);
    out
}

/// A clipped, vertically-offset view onto a taller child — the log tab. `offset` (pixels, clamped to
/// the content) is plain input data from `ui::input`; the child renders at `-offset` and everything is
/// clipped to the view bounds at whole-cell granularity.
///
/// Cost note: this renders the *whole* child each frame and clips afterward, so it's O(content), not
/// O(visible). Fine for bounded panels; for an unbounded log the integration layer should hand
/// `ScrollView` a child already windowed to the visible row range (it knows `offset`/view height)
/// rather than the full backlog.
pub struct ScrollView {
    child: Box<dyn Widget>,
    pub offset: i32,
}

impl ScrollView {
    pub fn new(child: impl Widget + 'static, offset: i32) -> Self {
        Self { child: Box::new(child), offset }
    }

    /// The maximum scroll offset for a content height inside a view height.
    pub fn max_offset(content_h: i32, view_h: i32) -> i32 {
        (content_h - view_h).max(0)
    }
}

impl Widget for ScrollView {
    fn measure(&self, theme: &Theme) -> Size {
        self.child.measure(theme)
    }

    fn render(&self, theme: &Theme, bounds: Rect, out: &mut DrawList) {
        let content_h = self.child.measure(theme).h.max(bounds.h);
        let off = self.offset.clamp(0, Self::max_offset(content_h, bounds.h));
        let content_rect = Rect::new(bounds.x, bounds.y - off, bounds.w, content_h);

        let mut content = DrawList::new();
        self.child.render(theme, content_rect, &mut content);
        for cmd in clip(content.cmds(), bounds) {
            out.0.push(cmd);
        }
    }
}
