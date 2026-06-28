//! Native utility-window **menu** — the tabbed interactive menu (Actions / Settings / Log / Debug),
//! drawn by the game's own `CSEzDraw` via the native UI library instead of the imgui overlay. Built
//! from `unseamless_core::ui::render` widgets (Tabs / List / Modal), navigated by
//! `unseamless_core::ui::input` (Navigator), and rasterized through [`crate::native_draw::draw_list`].
//!
//! (Skeleton — implemented by the `native-menu` worker lane; registered in `app.rs` at integration.)
