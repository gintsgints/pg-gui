//! The bottom panel: result table, message log and plan flame graph.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_base::SelectableText;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    chart::{FlameGraph, FlamePath},
    h_flex,
    table::DataTable,
    v_flex,
};

use crate::app::{BottomView, PgGuiApp};
use crate::plan;

impl PgGuiApp {
    /// A small outline button that switches the bottom panel to `view`. The
    /// icon is a text glyph, like the rest of the app's small controls.
    fn bottom_view_button(
        view: BottomView,
        glyph: &'static str,
        tooltip: &'static str,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        Button::new(id)
            .outline()
            .small()
            .label(glyph)
            .tooltip(tooltip)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.bottom_view != view {
                    this.bottom_view = view;
                    cx.notify();
                }
            }))
    }

    /// The Plan switch button, present only once the tab holds a plan — the
    /// view has nothing to show before the first `EXPLAIN`.
    fn plan_view_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        self.tabs
            .get(self.active_tab)?
            .result
            .plan
            .as_ref()
            .map(|_| {
                Self::bottom_view_button(
                    BottomView::Plan,
                    "🔥",
                    "Show plan",
                    "bottom-view-plan",
                    cx,
                )
            })
    }

    /// The row of buttons switching between the result sets a multi-statement
    /// run produced, one per statement that returned rows. `None` when the run
    /// produced at most one.
    fn render_result_selector(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let result = &self.tabs.get(self.active_tab)?.result;
        if result.results.len() < 2 {
            return None;
        }
        let selected = result.selected;
        Some(
            h_flex().gap_1().flex_wrap().children(
                result
                    .results
                    .iter()
                    .enumerate()
                    .map(|(ix, set)| {
                        let button = Button::new(("result-set", ix))
                            .small()
                            .label(format!("#{} ({})", set.statement, set.rows.len()))
                            .tooltip(set.label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_result_set(ix, cx);
                            }));
                        if ix == selected {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
        )
    }

    /// The results table between the result-set selector (when a run produced
    /// more than one) and a bottom row holding the pager (when present) and
    /// the Log switch button.
    pub(in crate::app) fn render_data_view(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // The table measures its column viewport in a layout pass, so a
        // stretch to fill it lands on the frame after the pane resizes.
        self.results.update(cx, |table, cx| {
            let viewport = table.horizontal_scroll_handle.bounds().size.width;
            if table.delegate_mut().set_viewport(viewport) {
                table.refresh(cx);
            }
        });

        v_flex()
            .size_full()
            .p_2()
            .gap_1()
            .children(self.render_result_selector(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(DataTable::new(&self.results))
                    .children(self.render_no_rows_hint(cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .children(self.render_results_pager(cx))
                    .children(self.render_row_selection_count(cx))
                    .child(div().flex_1())
                    .children(self.plan_view_button(cx))
                    // The Log switch stays visible so the log is always one
                    // click away, even before a run produced any messages.
                    .child(Self::bottom_view_button(
                        BottomView::Log,
                        "☰",
                        "Show log",
                        "bottom-view-log",
                        cx,
                    )),
            )
    }

    /// A muted "0 rows" line over the table body when the shown result set
    /// has columns but no rows — otherwise a query that matched nothing looks
    /// exactly like one whose output has not arrived. Drawn over the table
    /// rather than in its place so the column header stays: which columns
    /// came back is still the answer. It has no listeners, so clicks reach
    /// the header underneath.
    fn render_no_rows_hint(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let set = self.tabs.get(self.active_tab)?.result.selected()?;
        if set.columns.is_empty() || !set.rows.is_empty() {
            return None;
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("0 rows — the statement returned no rows"),
        )
    }

    /// Whether the Log view is scrolled to its last line — which it also is
    /// when the log is short enough not to scroll at all, and before the
    /// view has ever been laid out.
    ///
    /// The offset runs negative downwards and `max_offset` is the positive
    /// distance the content can travel, so the two cancel out at the bottom;
    /// the tolerance covers the sub-pixel drift between the two measurements.
    pub(in crate::app) fn log_at_bottom(&self) -> bool {
        self.log_scroll.max_offset().y + self.log_scroll.offset().y <= px(2.)
    }

    /// The message log (one line per statement from the last query/script)
    /// over a Data switch button at the bottom.
    pub(in crate::app) fn render_log(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let body = if self.log.is_empty() {
            v_flex().child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No messages yet."),
            )
        } else {
            // One run for the whole log rather than one per line: a plain
            // `div` child takes no part in window selection, and runs sharing
            // a selection handle each overwrite the others' text, so a drag
            // across several lines (and its cmd-c) needs them in one element.
            v_flex().w_full().child(SelectableText::with_handle(
                "log-text",
                self.log_selection.clone(),
                SharedString::from(self.log.join("\n")),
            ))
        };
        v_flex()
            .size_full()
            .p_2()
            .gap_1()
            .child(
                div()
                    .id("log-view")
                    .track_focus(&self.log_focus)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| window.focus(&this.log_focus, cx)),
                    )
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.log_scroll)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .child(body),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("clear-log")
                            .outline()
                            .small()
                            .label("🗑")
                            .tooltip("Clear log")
                            .disabled(self.log.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                // The view is a mirror of the tab's own log;
                                // clearing only the mirror would bring every
                                // line back on the next run.
                                if let Some(tab) = this.tabs.get_mut(this.active_tab) {
                                    tab.result.log.clear();
                                }
                                this.log.clear();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .children(self.plan_view_button(cx))
                    // The Data switch stays visible so the table is always one
                    // click away, even when the last run returned no rows.
                    .child(Self::bottom_view_button(
                        BottomView::Data,
                        "▦",
                        "Show data",
                        "bottom-view-data",
                        cx,
                    )),
            )
    }

    /// The last `EXPLAIN`'s plan as a flame graph: each node as wide as its
    /// cost (or, under ANALYZE, the time it spent), its children packed
    /// inside it. Clicking a frame zooms to it, clicking one of the ancestor
    /// rows above zooms back out.
    pub(in crate::app) fn render_plan_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.result.plan.as_ref())
        else {
            return v_flex()
                .size_full()
                .p_2()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .text_color(cx.theme().muted_foreground)
                        .child("No plan yet — cmd-e explains the statement at the cursor."),
                )
                .child(Self::render_plan_footer(None, cx))
                .into_any_element();
        };

        let focus = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.result.plan_focus.clone());
        let header = format!(
            "{} — {}, {} node(s), {} deep · {}",
            if plan.analyze {
                "explain analyze"
            } else {
                "explain"
            },
            plan.unit.caption(),
            plan.nodes,
            plan.depth,
            plan.summary,
        );

        v_flex()
            .size_full()
            .p_2()
            .gap_1()
            .child(
                v_flex()
                    .gap_0p5()
                    .child(div().text_color(cx.theme().muted_foreground).child(header))
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(cx.theme().mono_font_size)
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(plan.statement.clone()),
                    ),
            )
            .child(Self::render_plan_chart(plan, focus.clone(), cx))
            .child(Self::render_plan_footer(Some(focus.is_some()), cx))
            .into_any_element()
    }

    /// The flame graph itself, in the scroll container that gives it room for
    /// the whole stack: the chart fills the area it is handed and clips what
    /// does not fit, so a plan deeper than the pane scrolls rather than
    /// squeezing its rows.
    fn render_plan_chart(
        plan: &plan::Plan,
        focus: Option<FlamePath>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let unit = plan.unit;
        let chart = FlameGraph::shared(plan.roots.clone())
            .id("explain-flame")
            .children(|node: &plan::PlanNode| node.children.as_slice())
            .value(|node: &plan::PlanNode| node.value)
            .label(|node: &plan::PlanNode| node.label.clone())
            .format(move |value| unit.format(value).into())
            .focus(focus)
            .row_height(cx.theme().mono_font_size + px(8.))
            .on_click(cx.listener(|this, path: &FlamePath, _, cx| {
                let active = this.active_tab;
                if let Some(tab) = this.tabs.get_mut(active) {
                    tab.result.plan_focus = Some(path.clone());
                }
                cx.notify();
            }));
        let height = chart.height(plan.depth);
        div()
            .id("plan-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(div().w_full().h(height).child(chart))
    }

    /// The plan view's bottom row: the switches back to the table and the log,
    /// plus — once there is a plan to zoom — the way out of a zoom. `zoomed`
    /// is `None` before the first `EXPLAIN`, when there is nothing to reset.
    fn render_plan_footer(
        zoomed: Option<bool>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        h_flex()
            .gap_2()
            .children(zoomed.map(|zoomed| {
                Button::new("plan-reset-zoom")
                    .outline()
                    .small()
                    .label("Reset zoom")
                    .tooltip("Show the whole plan again")
                    .disabled(!zoomed)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let active = this.active_tab;
                        if let Some(tab) = this.tabs.get_mut(active) {
                            tab.result.plan_focus = None;
                        }
                        cx.notify();
                    }))
            }))
            .child(div().flex_1())
            .child(Self::bottom_view_button(
                BottomView::Data,
                "▦",
                "Show data",
                "plan-view-data",
                cx,
            ))
            .child(Self::bottom_view_button(
                BottomView::Log,
                "☰",
                "Show log",
                "plan-view-log",
                cx,
            ))
    }

    /// "N rows selected" beside the pager while a multi-row selection is
    /// up, since it can span pages the grid is not showing.
    fn render_row_selection_count(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let count = self.results.read(cx).delegate().selected_row_count();
        (count > 0).then(|| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if count == 1 {
                    "1 row selected".to_string()
                } else {
                    format!("{count} rows selected")
                })
        })
    }

    /// The "Prev / Next / Page x of y" bar under the results table; `None`
    /// when everything fits on one page.
    fn render_results_pager(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let delegate = self.results.read(cx).delegate();
        let (page, page_count, total_rows) = (
            delegate.page(),
            delegate.page_count(),
            delegate.total_rows(),
        );
        let has_more = self.active_has_more();
        if page_count <= 1 && !has_more {
            return None;
        }

        Some(
            h_flex()
                .gap_2()
                .child(
                    Button::new("prev-page")
                        .outline()
                        .small()
                        .label("‹ Prev")
                        .disabled(page == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change_results_page(false, cx);
                        })),
                )
                .child(
                    Button::new("next-page")
                        .outline()
                        .small()
                        .label("Next ›")
                        .disabled(page + 1 >= page_count)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change_results_page(true, cx);
                        })),
                )
                .children(has_more.then(|| {
                    Button::new("fetch-more")
                        .outline()
                        .small()
                        .label("Fetch more")
                        .disabled(self.active_running())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.fetch_more_rows(window, cx);
                        }))
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Page {} of {page_count} · {total_rows}{} rows",
                            page + 1,
                            if has_more { "+" } else { "" }
                        )),
                ),
        )
    }
}
