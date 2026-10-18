//! Rendering, split by panel. Pieces several panels share live here.

mod bottom;
pub(in crate::app) mod chrome;
mod db_browser;
mod debugger;
mod files;
mod workspace;

use gpui::{
    App, ClickEvent, InteractiveElement as _, MouseButton, ParentElement as _, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};
use gpui_component::{ActiveTheme as _, h_flex, tooltip::Tooltip};

use super::PgGuiApp;

impl PgGuiApp {
    /// The height of a tree row.
    ///
    /// Fixed rather than derived from the label, because a row inside a
    /// `uniform_list` is laid out three times per frame, not once: on top of
    /// the real pass, `UniformList::measure_item` renders and lays out a row
    /// as its own taffy root from `request_layout` and again from `prepaint`,
    /// both at `MaxContent` width and `MinContent` height. Every dimension
    /// the row leaves to its content makes taffy walk the subtree again to
    /// resolve it, on each of those passes.
    ///
    /// Expressed in the theme font size rather than as a constant: the
    /// window's rem size follows `Theme::font_size` (gpui-component's `Root`
    /// sets it every frame) and `apply_zoom` scales that. The value
    /// reproduces what `ListItem` used to derive — a `text_base` line height
    /// at gpui's default `phi()` leading, plus its `py_1` above and below —
    /// so rows keep the height they had at any zoom.
    pub(in crate::app) fn tree_row_height(cx: &App) -> Pixels {
        let font_size = cx.theme().font_size;
        (font_size * 1.618_034).round() + font_size * 0.5
    }

    /// Keep a tree row's label on one line, clipped at the row's width.
    ///
    /// Deliberately not `truncate()`. That sets `text_overflow`, and in
    /// `TextLayout::layout` a set `truncate_width` both disqualifies the
    /// measured-layout cache ("if `truncate_width` is Some, we need to
    /// re-layout") and adds a whole extra `shape_text` of the string just to
    /// decide whether truncation is needed, before the real shaping. A row is
    /// measured three times a frame inside a `uniform_list`, so that lands as
    /// two uncached shapings per pass: profiling the ellipsis version against
    /// this one showed `shape_text` at 2.3x the allocations — more than the
    /// layout savings it was paired with. `whitespace_nowrap` gets the
    /// single-line behaviour without ever setting `text_overflow`; the cost is
    /// a hard clip instead of an "…".
    pub(in crate::app) fn clip_label(label_row: gpui::Div) -> gpui::Div {
        label_row.overflow_hidden().whitespace_nowrap()
    }

    /// The hover-revealed action at the end of a folder row: "+" in the files
    /// panel, "↻" in the object browser.
    ///
    /// A plain div rather than a `Button`, which nests four more nodes (its
    /// base, the content flex, the label wrapper and the label itself) to
    /// draw a single glyph — and a tree row is laid out three times a frame,
    /// so each of those is paid for three times. The slot is fixed-width for
    /// the same reason: pairing an auto-width child with `justify_between`
    /// leaves the width content-derived, which is what sends taffy down
    /// `determine_content_based_container_width`.
    pub(in crate::app) fn tree_row_action(
        id: SharedString,
        group: &'static str,
        glyph: &'static str,
        tooltip: &'static str,
        cx: &App,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let size = cx.theme().font_size * 1.25;
        h_flex()
            .id(id)
            .w(size)
            .h(size)
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(cx.theme().radius)
            .invisible()
            .group_hover(group, gpui::Styled::visible)
            .text_color(cx.theme().muted_foreground)
            .hover(|this| this.bg(cx.theme().tokens.list_hover))
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            // The tree toggles a row on mouse-down; swallow it here so
            // pressing the action doesn't also collapse the folder.
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                on_click(event, window, cx);
            })
            .child(glyph)
    }
}
