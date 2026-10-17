//! The workspace layout: side panels, editor and bottom panel, split.

use gpui::{AnyElement, Context, Entity, IntoElement, ParentElement as _, Styled as _, div, px};
use gpui_component::{
    ActiveTheme as _,
    input::Editor,
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    v_flex,
};

use crate::app::{BottomView, PgGuiApp};

impl PgGuiApp {
    /// The editor (with its tab bar) over the results table, split by a
    /// draggable divider; the split position is persisted to the config on
    /// drag and restored on launch via the editor panel's initial size.
    fn render_editor_results(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // The SQL editor with its tab bar; always shown.
        let editor = v_flex().size_full().child(self.render_tab_bar(cx)).child(
            div().flex_1().min_h(px(0.)).p_2().child(
                Editor::new(&self.editor())
                    .h_full()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size),
            ),
        );

        // The bottom panel is the debug view while a session runs, else the
        // results table, and is hidden entirely (cmd-3) when neither applies.
        let debugging = self.debug.is_some();
        if !debugging && !self.config.results_panel_visible {
            return v_flex().size_full().child(editor).into_any_element();
        }

        let mut editor_panel = resizable_panel().child(editor);
        if let Some(height) = self.config.editor_height {
            editor_panel = editor_panel.size(px(height));
        }

        v_resizable("editor-results")
            .with_state(&self.resizable_state)
            .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                if let Some(height) = state.read(cx).sizes().first() {
                    this.config.editor_height = Some(f32::from(*height));
                    this.schedule_save(cx);
                }
            }))
            .child(editor_panel)
            .child(
                // Results table with pager — replaced by the debug panel while
                // a debug session is active.
                //
                // The content sits out of the panel's flow (the panel itself is
                // `relative`) and clips: a stepped routine's source, or a long
                // message log, is taller than the panel, and in flow that
                // height becomes the panel's own layout size — which squeezes
                // the editor above and leaves the content painting over it.
                // Absolute keeps the panel at the height the splitter gave it,
                // and the background covers anything the editor spills into it.
                resizable_panel().child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .bg(cx.theme().background)
                        .child(if debugging {
                            self.render_debug_panel(cx).into_any_element()
                        } else {
                            match self.bottom_view {
                                BottomView::Data => self.render_data_view(cx).into_any_element(),
                                BottomView::Log => self.render_log(cx).into_any_element(),
                                BottomView::Plan => self.render_plan_view(cx),
                            }
                        }),
                ),
            )
            .into_any_element()
    }

    /// The files side panel: the working directory's tree, or an "Open
    /// Folder…" hint while none is set. Non-SQL files are disabled — shown
    /// greyed out, with no click handlers.
    /// Compose the three-column workspace: the database browser (left), the
    /// editor+results area (center), and the files panel (right). Each side
    /// panel is present only when visible, with its own draggable divider.
    pub(in crate::app) fn render_workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        let center_and_files = if self.config.files_panel_visible {
            h_resizable("editor-sidebar")
                .with_state(&self.sidebar_state)
                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                    // The files panel is the second (right) child.
                    if let Some(width) = state.read(cx).sizes().last() {
                        this.config.files_panel_width = Some(f32::from(*width));
                        this.schedule_save(cx);
                    }
                }))
                .child(resizable_panel().child(self.render_editor_results(cx)))
                .child(
                    resizable_panel()
                        .size(px(self.config.files_panel_width.unwrap_or(240.)))
                        .size_range(px(120.)..px(600.))
                        .child(self.render_files_panel(cx)),
                )
                .into_any_element()
        } else {
            self.render_editor_results(cx).into_any_element()
        };

        if self.config.db_panel_visible {
            h_resizable("db-sidebar")
                .with_state(&self.db_sidebar_state)
                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                    // The database browser is the first (left) child.
                    if let Some(width) = state.read(cx).sizes().first() {
                        this.config.db_panel_width = Some(f32::from(*width));
                        this.schedule_save(cx);
                    }
                }))
                .child(
                    resizable_panel()
                        .size(px(self.config.db_panel_width.unwrap_or(260.)))
                        .size_range(px(120.)..px(600.))
                        .child(self.render_db_panel(cx)),
                )
                .child(resizable_panel().child(center_and_files))
                .into_any_element()
        } else {
            center_and_files
        }
    }
}
