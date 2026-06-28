//! ASCII-art rendering tests for `ui::render`. The harness rasterizes a [`DrawList`] onto a character
//! grid so layouts read as human-legible pictures and bugs are obvious.
//!
//! ## How the rasterizer works (and why it's font-agnostic)
//! The grid is in **glyph cells**, not pixels: one column = one face advance, one row = one line
//! height, both pulled from [`metrics`]. A `Rect` colors every cell whose **center** it covers (so
//! cell-aligned fills map exactly, and sub-cell hairlines like borders simply don't show — those are
//! verified by direct command assertions instead). A `Text` command stamps its string one char per
//! cell at its pen position. Every expected coordinate below is derived from `metrics()` — never a
//! font-specific literal — so the in-flight Spleen→Proggy swap can't break these tests.
//!
//! Two color conventions make state legible: a rect's fill char comes from [`rect_char`] (`#` panel,
//! `*` accent/selection, severity `I`/`W`/`E`); and **dimmed** text (disabled rows) is stamped
//! lowercased, so a disabled row reads differently from an enabled one in the same picture.

use crate::bitmap_font::{metrics, Face};
use crate::notifications::{Severity, Toast};

use super::layout::{center, Align, Anchor, Length, Size, Sizing, Stack, Widget};
use super::primitives::{DrawCmd, DrawList, Rect};
use super::theme::Theme;
use super::widgets::{
    draw_border_for_test, toast_alpha, toast_stack, Banner, Divider, Label, List, Modal, Panel, Row,
    ScrollView, Tabs,
};

/// A test theme: the default palette, but **no border/divider hairlines** (`border_w = 0`) so every
/// picture's geometry stays exactly cell-aligned. Hairline rendering is tested separately via the
/// emitted commands. Padding is one menu cell each axis (advance × line height), which is cell-aligned.
fn test_theme() -> Theme {
    Theme { border_w: 0, ..Theme::default() }
}

/// The fill char for a rect of `color` under the test theme. Unknown colors surface as `?`.
fn rect_char(color: super::primitives::Rgba, theme: &Theme) -> char {
    match color {
        c if c == theme.panel => '#',
        c if c == theme.accent => '*',
        c if c == theme.bg => ' ',
        c if c == theme.info => 'I',
        c if c == theme.warning => 'W',
        c if c == theme.error => 'E',
        c if c == theme.border => '+',
        _ => '?',
    }
}

/// Rasterize a draw list onto a `cols × rows` glyph-cell grid (see module docs). `face` sets the cell
/// pitch; all commands in the list must use it. Returns the grid as newline-joined rows.
fn rasterize(list: &DrawList, face: Face, cols: i32, rows: i32, theme: &Theme) -> String {
    let m = metrics(face);
    let (adv, lh) = (m.advance, m.line_height);
    let mut grid = vec![vec!['.'; cols as usize]; rows as usize];

    // Rects: a cell takes the color of the last rect covering its center (painter's order).
    for row in 0..rows {
        for col in 0..cols {
            let (cx, cy) = (col * adv + adv / 2, row * lh + lh / 2);
            for cmd in list.cmds() {
                if let DrawCmd::Rect { rect, color } = cmd
                    && rect.x <= cx
                    && cx < rect.right()
                    && rect.y <= cy
                    && cy < rect.bottom()
                {
                    grid[row as usize][col as usize] = rect_char(*color, theme);
                }
            }
        }
    }

    // Text: stamp one char per cell from the pen position; `\n` drops a line and returns to the run's
    // start column (matching `shape`/`clip_text`); dimmed runs are lowercased so disabled rows read
    // distinctly.
    for cmd in list.cmds() {
        if let DrawCmd::Text { pos, text, color, .. } = cmd {
            let col0 = pos[0].div_euclid(adv);
            let dim = *color == theme.dim;
            for (line_idx, line) in text.split('\n').enumerate() {
                let row = pos[1].div_euclid(lh) + line_idx as i32;
                for (i, ch) in line.chars().enumerate() {
                    let col = col0 + i as i32;
                    if (0..cols).contains(&col) && (0..rows).contains(&row) {
                        grid[row as usize][col as usize] = if dim { ch.to_ascii_lowercase() } else { ch };
                    }
                }
            }
        }
    }

    grid.into_iter().map(|r| r.into_iter().collect::<String>()).collect::<Vec<_>>().join("\n")
}

/// Render a widget at the origin (sized to its own measure) and rasterize it to a grid that exactly
/// covers it. Cols/rows derive from `metrics(face)`, so no font literals leak in.
fn picture(widget: &dyn Widget, theme: &Theme, face: Face) -> String {
    let size = widget.measure(theme);
    let m = metrics(face);
    let cols = size.w / m.advance;
    let rows = size.h / m.line_height;
    let mut list = DrawList::new();
    widget.render(theme, Rect::new(0, 0, size.w, size.h), &mut list);
    rasterize(&list, face, cols, rows, theme)
}

#[test]
fn padded_titled_panel() {
    let theme = test_theme();
    // A panel with a title bar and one cell of padding around the content.
    let panel = Panel::new(Label::new("Hi")).title("Menu");
    // Title strip (accent `*`) with the title over it; content "Hi" inset one cell into the panel bg.
    assert_eq!(
        picture(&panel, &theme, Face::Menu),
        "\
*Menu*
######
#Hi###
######"
    );
}

#[test]
fn vstack_with_spacing_between_children() {
    let theme = test_theme();
    let m = metrics(Face::Menu);
    // Two labels with one blank line (one line-height) of spacing between them.
    let stack = super::layout::Stack::vertical()
        .spacing(m.line_height)
        .child(Label::new("AA"))
        .child(Label::new("BB"));
    assert_eq!(
        picture(&stack, &theme, Face::Menu),
        "\
AA
..
BB"
    );
}

#[test]
fn list_shows_selection_and_disabled_row() {
    let theme = test_theme();
    // Row 0 selected (accent fill past the text), row 1 disabled (stamped lowercase), row 2 key:value
    // with the value right-aligned.
    let list = List::new(vec![
        Row::text("Run"),
        Row::text("QUIT").disabled(),
        Row::kv("Vol", "50"),
    ])
    .selected(Some(0));
    assert_eq!(
        picture(&list, &theme, Face::Menu),
        "\
Run***
quit..
Vol.50"
    );
}

#[test]
fn tabs_highlight_the_active_tab() {
    let theme = test_theme();
    let m = metrics(Face::Menu);
    // Active tab 0 gets an accent cell on each side of its label (padding), tab 1 is plain. Content
    // sits one row below the (zero-width) divider.
    let tabs = Tabs::new(vec!["A".into(), "B".into()], 0, Label::new("Hi"))
        .tab_pad(m.advance)
        .tab_gap(m.advance);
    assert_eq!(
        picture(&tabs, &theme, Face::Menu),
        "\
*A*..B.
Hi....."
    );
}

#[test]
fn banner_fills_a_severity_strip_with_centered_text() {
    let theme = test_theme();
    // A warning banner: the whole strip is the warning color (`W`), the message centered within it.
    let banner = Banner::new("Hi", Severity::Warning);
    assert_eq!(
        picture(&banner, &theme, Face::Menu),
        "\
WWWW
WHiW
WWWW"
    );
}

#[test]
fn modal_centers_in_a_viewport() {
    let theme = test_theme();
    let m = metrics(Face::Menu);
    // A titled modal with two options, the second selected. Place it centered in a viewport and
    // rasterize the whole viewport so the centering (top-left origin offset) is visible.
    let modal = Modal::new(vec!["Yes".into(), "No".into()], 1).title("Quit?");
    let size = modal.measure(&theme);
    // Viewport two cells wider and two rows taller than the modal -> a one-cell/one-row margin.
    let vp = Rect::new(0, 0, size.w + 2 * m.advance, size.h + 2 * m.line_height);
    let bounds = center(size, vp);
    let mut list = DrawList::new();
    modal.render(&theme, bounds, &mut list);
    let cols = vp.w / m.advance;
    let rows = vp.h / m.line_height;
    assert_eq!(
        rasterize(&list, Face::Menu, cols, rows, &theme),
        "\
.........
.*Quit?*.
.#######.
.#Yes###.
.#No***#.
.#######.
........."
    );
}

#[test]
fn scrollview_clips_overflow_to_the_visible_window() {
    let theme = test_theme();
    let m = metrics(Face::Menu);
    // A 5-row list viewed through a 3-row window, scrolled down two rows: L0/L1 scroll off the top,
    // L2/L3/L4 remain.
    let rows: Vec<Row> = (0..5).map(|i| Row::text(format!("L{i}"))).collect();
    let list = List::new(rows);
    let scroll = ScrollView::new(list, 2 * m.line_height);
    let bounds = Rect::new(0, 0, 2 * m.advance, 3 * m.line_height);
    let mut dl = DrawList::new();
    scroll.render(&theme, bounds, &mut dl);
    assert_eq!(
        rasterize(&dl, Face::Menu, 2, 3, &theme),
        "\
L2
L3
L4"
    );
}

#[test]
fn toast_stack_anchors_top_right_with_severity_stripes() {
    let theme = test_theme();
    let m = metrics(theme.compact_face);
    // Two full-life toasts (alpha 255) right-anchored: each is `stripe # message`, right edges aligned
    // to the viewport's right edge. Info toast on top, error below.
    let toasts = vec![full_toast("Hi", Severity::Info), full_toast("Bye", Severity::Error)];
    let vp = Rect::new(0, 0, 10 * m.advance, 5 * m.line_height);
    let list = toast_stack(&theme, &toasts, vp, Anchor::TopRight, 0, 0);
    assert_eq!(
        rasterize(&list, theme.compact_face, 10, 2, &theme),
        "\
......I#Hi
.....E#Bye"
    );
}

/// A toast at full lifetime (no fade) for deterministic, opaque colors in the picture tests.
fn full_toast(message: &str, severity: Severity) -> Toast {
    Toast { message: message.into(), severity, remaining: 4.0, duration: 4.0 }
}

// --- Sub-cell / data-level assertions (hairlines + fade aren't visible at cell granularity) ---

#[test]
fn divider_emits_a_centered_hairline() {
    let theme = Theme::default();
    let divider = Divider::horizontal().thickness(2);
    let mut list = DrawList::new();
    divider.render(&theme, Rect::new(0, 0, 40, 16), &mut list);
    // One rect, full width, `thickness` tall, vertically centered, in the border color.
    assert_eq!(
        list.cmds(),
        &[DrawCmd::Rect { rect: Rect::new(0, 7, 40, 2), color: theme.border }]
    );
}

#[test]
fn panel_border_strokes_four_edges() {
    let theme = Theme::default(); // border_w = 2
    let cmds = draw_border_for_test(Rect::new(0, 0, 20, 10), theme.border_w, theme.border);
    assert_eq!(
        cmds,
        vec![
            DrawCmd::Rect { rect: Rect::new(0, 0, 20, 2), color: theme.border },   // top
            DrawCmd::Rect { rect: Rect::new(0, 8, 20, 2), color: theme.border },   // bottom
            DrawCmd::Rect { rect: Rect::new(0, 0, 2, 10), color: theme.border },   // left
            DrawCmd::Rect { rect: Rect::new(18, 0, 2, 10), color: theme.border },  // right
        ]
    );
}

#[test]
fn toast_fade_is_opaque_until_the_final_stretch() {
    let mk = |remaining| Toast { message: "x".into(), severity: Severity::Info, remaining, duration: 4.0 };
    assert_eq!(toast_alpha(&mk(4.0)), 255, "full life -> opaque");
    assert_eq!(toast_alpha(&mk(0.5)), 255, "fade begins exactly at the boundary");
    assert_eq!(toast_alpha(&mk(0.25)), 128, "halfway through the fade -> ~half alpha");
    assert_eq!(toast_alpha(&mk(0.0)), 0, "expired -> transparent");
}

#[test]
fn fill_children_split_leftover_space_evenly() {
    // Two fill rows share a fixed-height stack; the odd pixel goes to the first.
    let theme = test_theme();
    let stack = super::layout::Stack::vertical()
        .child_sized(Divider::horizontal().thickness(2).color(theme.info), Sizing::FILL)
        .child_sized(Divider::horizontal().thickness(2).color(theme.warning), Sizing::FILL);
    let mut list = DrawList::new();
    stack.render(&theme, Rect::new(0, 0, 10, 21), &mut list);
    // 21 px / 2 fills = 10 and 11; first fill spans rows [0,11), second [11,21). The dividers center
    // within those bands.
    let centers: Vec<i32> = list
        .cmds()
        .iter()
        .map(|c| match c {
            DrawCmd::Rect { rect, .. } => rect.y,
            _ => -1,
        })
        .collect();
    assert_eq!(centers, vec![(11 - 2) / 2, 11 + (10 - 2) / 2]);
}

#[test]
fn fill_remainder_goes_to_leading_children() {
    // 3 fills over 11px -> bands of 4,4,3 (the first two children take the +1 remainder). A 1px
    // divider centers in each band, so its y reveals the band start.
    let theme = test_theme();
    let stack = Stack::vertical()
        .child_sized(Divider::horizontal().thickness(1), Sizing::FILL)
        .child_sized(Divider::horizontal().thickness(1), Sizing::FILL)
        .child_sized(Divider::horizontal().thickness(1), Sizing::FILL);
    let mut list = DrawList::new();
    stack.render(&theme, Rect::new(0, 0, 10, 11), &mut list);
    let ys: Vec<i32> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            DrawCmd::Rect { rect, .. } => Some(rect.y),
            _ => None,
        })
        .collect();
    // Band starts 0/4/8, each +(band_h-1)/2 to center the hairline.
    assert_eq!(ys, vec![(4 - 1) / 2, 4 + (4 - 1) / 2, 8 + (3 - 1) / 2]);
}

#[test]
fn empty_containers_are_inert() {
    let theme = test_theme();
    // Empty stack emits nothing even when handed real bounds.
    let mut dl = DrawList::new();
    Stack::vertical().spacing(theme.gap).render(&theme, Rect::new(0, 0, 40, 40), &mut dl);
    assert!(dl.is_empty(), "empty stack emits no commands");
    // Empty list measures to zero and draws nothing.
    let list = List::new(vec![]);
    assert_eq!(list.measure(&theme), Size::new(0, 0));
    let mut dl = DrawList::new();
    list.render(&theme, Rect::new(0, 0, 40, 40), &mut dl);
    assert!(dl.is_empty());
    // Empty toast stack is a no-op (guards the anchor/measure on an empty slice).
    assert!(toast_stack(&theme, &[], Rect::new(0, 0, 100, 100), Anchor::TopRight, 0, 0).is_empty());
}

#[test]
fn list_without_a_valid_selection_draws_no_highlight() {
    let theme = test_theme();
    // Neither `None` nor an out-of-range index highlights a row (the index is compared, never used to
    // index a slice).
    for sel in [None, Some(99)] {
        let list = List::new(vec![Row::text("AA"), Row::text("BB")]).selected(sel);
        assert_eq!(picture(&list, &theme, Face::Menu), "AA\nBB", "sel={sel:?}");
        let size = list.measure(&theme);
        let mut dl = DrawList::new();
        list.render(&theme, Rect::new(0, 0, size.w, size.h), &mut dl);
        assert!(
            !dl.cmds()
                .iter()
                .any(|c| matches!(c, DrawCmd::Rect { color, .. } if *color == theme.accent)),
            "sel={sel:?} drew an accent highlight"
        );
    }
}

#[test]
fn stack_cross_align_center_centers_narrow_children() {
    let theme = test_theme();
    // In a vertical stack the cross axis is horizontal: the short "X" centers over the wide "WIDE".
    let stack = Stack::vertical()
        .cross_align(Align::Center)
        .child(Label::new("X"))
        .child(Label::new("WIDE"));
    assert_eq!(
        picture(&stack, &theme, Face::Menu),
        "\
.X..
WIDE"
    );
}

#[test]
fn secret_row_masks_value_until_revealed() {
    let theme = test_theme();
    // A hidden secret renders one mask glyph per char; revealing shows the real value. Value is
    // right-aligned, so both sit in the last two cells.
    let hidden = List::new(vec![Row::kv("PW", "AB").secret(false)]);
    let shown = List::new(vec![Row::kv("PW", "AB").secret(true)]);
    assert_eq!(picture(&hidden, &theme, Face::Menu), "PW.**");
    assert_eq!(picture(&shown, &theme, Face::Menu), "PW.AB");
}

#[test]
fn rasterizer_and_label_handle_multiline_text() {
    let theme = test_theme();
    // An embedded newline measures as two lines and stamps onto two rows (carriage return to col 0).
    assert_eq!(picture(&Label::new("AB\nC"), &theme, Face::Menu), "AB\nC.");
}

#[test]
fn fixed_sizing_pins_an_exact_main_length() {
    // A `Fixed` main length is honored exactly; `Length` is part of the public sizing vocabulary.
    let theme = test_theme();
    let m = metrics(Face::Menu);
    let stack = Stack::vertical()
        .child_sized(Label::new("A"), Sizing::new(Length::Hug, Length::Fixed(m.line_height)))
        .child(Label::new("B"));
    // First child occupies exactly one line; "B" lands on row 1.
    assert_eq!(picture(&stack, &theme, Face::Menu), "A\nB");
}
