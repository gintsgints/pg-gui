//! Editor commands: snippets and their tab stops, format, comment toggling,
//! problem navigation and AI completion.

use gpui::{
    AppContext as _, Context, EntityInputHandler as _, Focusable as _, ParentElement as _,
    Styled as _, Window, div, px,
};
use gpui_base::input::DiagnosticSeverity;
use gpui_component::{
    WindowExt as _,
    input::{Escape as InputEscape, IndentInline, RopeExt as _},
    list::{List, ListEvent, ListState},
};

use crate::app::{PgGuiApp, diagnostic_target, toggle_line_comments};
use crate::{
    AiComplete, FormatScript, NextDiagnostic, OpenSnippets, PrevDiagnostic, ToggleComment,
    ToggleFormatOnSave, ai, snippets,
};

impl PgGuiApp {
    /// Open the searchable snippet picker; the confirmed snippet is
    /// inserted into the editor at the cursor.
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub(in crate::app) fn open_snippet_picker(
        &mut self,
        _: &OpenSnippets,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }

        let app = cx.weak_entity();
        let list = cx.new(|cx| {
            let delegate =
                snippets::PickerDelegate::new(snippets::load(), move |snippet, window, cx| {
                    window.close_dialog(cx);
                    app.update(cx, |this, cx| this.insert_snippet(snippet, window, cx))
                        .ok();
                });
            ListState::new(delegate, window, cx).searchable(true)
        });
        cx.subscribe_in(&list, window, |_, _, event, window, cx| {
            if matches!(event, ListEvent::Cancel) {
                window.close_dialog(cx);
            }
        })
        .detach();

        let list_in_dialog = list.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("Insert snippet").w(px(560.)).child(
                div()
                    .h(px(400.))
                    .child(List::new(&list_in_dialog).search_placeholder("Search snippets…")),
            )
        });
        list.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Insert a snippet at the cursor as its own statement. A snippet with
    /// `$n` tab stops enters snippet mode and jumps to the first stop;
    /// otherwise, when it contains a `%%` filter placeholder, the caret is
    /// placed between the two `%` so typing narrows the filter.
    fn insert_snippet(
        &mut self,
        snippet: &snippets::Snippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let has_stops = snippets::has_tab_stops(&snippet.sql);
        self.editor().update(cx, |state, cx| {
            let sql = snippet.sql.trim();
            let text = state.value();
            let mut cursor = state.cursor().min(text.len());
            while cursor > 0 && !text.is_char_boundary(cursor) {
                cursor -= 1;
            }

            let mut inserted = String::new();
            if !text[..cursor].is_empty() && !text[..cursor].ends_with('\n') {
                inserted.push('\n');
            }
            inserted.push_str(sql);
            if !text[cursor..].starts_with('\n') {
                inserted.push('\n');
            }

            let placeholder = inserted.find("%%").filter(|_| !has_stops);
            state.insert(inserted.clone(), window, cx);
            if let Some(pos) = placeholder {
                let target = state.cursor() - inserted.len() + pos + 1;
                let position = state.text().offset_to_position(target);
                state.set_cursor_position(position, window, cx);
            } else {
                state.focus(window, cx);
            }
        });
        if has_stops {
            self.tabs[self.active_tab].snippet_mode = true;
            self.next_snippet_stop(false, window, cx);
        }
        self.set_status(format!("Inserted “{}”", snippet.name), cx);
    }

    /// Jump to the buffer's next `$n` tab stop: the marker is replaced by
    /// its placeholder text, which is left selected so typing overwrites
    /// it. Returns false when no marker remains.
    pub(in crate::app) fn next_snippet_stop(
        &mut self,
        braced_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor().update(cx, |state, cx| {
            let Some(stop) = snippets::next_tab_stop(&state.value(), braced_only) else {
                return false;
            };
            state.set_selected_range(stop.range, cx);
            state.replace(stop.placeholder.clone(), window, cx);
            let end = state.cursor();
            state.set_selected_range(end - stop.placeholder.len()..end, cx);
            state.focus(window, cx);
            true
        })
    }

    /// Capture-phase tab handler: tab visits the buffer's next tab stop
    /// instead of indenting. In snippet mode (after a picker insertion)
    /// every marker form is serviced; otherwise only the braced forms —
    /// left by an accepted completion suggestion — so a hand-written
    /// Postgres parameter (`$1`) never captures the tab key.
    pub(in crate::app) fn on_editor_tab(
        &mut self,
        _: &IndentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editor().focus_handle(cx).is_focused(window) {
            return;
        }
        let snippet_mode = self.tabs[self.active_tab].snippet_mode;
        if self.next_snippet_stop(!snippet_mode, window, cx) {
            cx.stop_propagation();
        } else if snippet_mode {
            self.tabs[self.active_tab].snippet_mode = false;
        }
    }

    /// Escape ends snippet mode (and still bubbles on to the editor).
    pub(in crate::app) fn on_editor_escape(
        &mut self,
        _: &InputEscape,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.tabs[self.active_tab].snippet_mode = false;
    }

    pub(in crate::app) fn ai_complete(
        &mut self,
        _: &AiComplete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ai_running {
            return;
        }
        let Some(key) = ai::api_key(&self.config.ai_api_key) else {
            self.set_status(
                "AI completion needs an API key: set ai_api_key in config.json or ANTHROPIC_API_KEY in the environment",
                cx,
            );
            return;
        };

        let model = ai::model(&self.config.ai_model);
        let prompt_addition = self.config.ai_prompt.clone();

        let (before, after) = {
            let state = self.editor().read(cx);
            let text = state.value().to_string();
            let mut cursor = state.cursor().min(text.len());
            while cursor > 0 && !text.is_char_boundary(cursor) {
                cursor -= 1;
            }
            (text[..cursor].to_string(), text[cursor..].to_string())
        };

        self.ai_running = true;
        self.set_status("AI completing…", cx);

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    ai::complete(&key, &model, &prompt_addition, &before, &after)
                })
                .await;

            this.update_in(cx, |this, window, cx| {
                this.ai_running = false;
                match result {
                    Ok(completion) => {
                        this.editor().update(cx, |state, cx| {
                            state.insert(completion, window, cx);
                            state.focus(window, cx);
                        });
                        this.set_status("AI completion inserted", cx);
                    }
                    Err(err) => this.set_status(format!("AI error: {err}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Format the script through the language server without saving
    /// (cmd-shift-f).
    pub(in crate::app) fn format_script(
        &mut self,
        _: &FormatScript,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.lsp.clone() else {
            self.set_status("Format unavailable — SQL language server not connected", cx);
            return;
        };

        let text = self.editor().read(cx).value().to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.format(&text).await;
            this.update_in(cx, |this, window, cx| match result {
                // Skip stale results: the buffer changed while the
                // server was formatting.
                Ok(Some(formatted)) if this.editor().read(cx).value() == text => {
                    this.apply_formatted(&formatted, window, cx);
                    this.set_status("Formatted script", cx);
                }
                Ok(Some(_)) => {}
                Ok(None) => this.set_status("Script already formatted", cx),
                Err(err) => this.set_status(format!("Format failed: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Turn format-on-save on or off (Edit ▸ Format on Save, or the
    /// `fmt:` segment in the status bar). Persisted, so it holds across
    /// restarts; the menu is rebuilt for its check-mark.
    pub(in crate::app) fn toggle_format_on_save(
        &mut self,
        _: &ToggleFormatOnSave,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.format_on_save = !self.config.format_on_save;
        self.refresh_menus(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Comment or uncomment the current line, or every line the selection
    /// touches, with `--` (cmd-/). Goes through the input handler so the
    /// edit is undoable and the usual change plumbing runs.
    pub(in crate::app) fn toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor().update(cx, |state, cx| {
            let selection = state
                .selected_text_range(false, window, cx)
                .map(|sel| sel.range);
            let cursor = state.cursor();

            let text = state.text();
            let selection = selection.map_or(cursor..cursor, |range| {
                text.offset_utf16_to_offset(range.start)..text.offset_utf16_to_offset(range.end)
            });

            // Expand to whole lines. A selection ending at the start of a
            // line does not pull that line in.
            let start_row = text.offset_to_point(selection.start).row;
            let mut end_row = text.offset_to_point(selection.end).row;
            if end_row > start_row && text.offset_to_point(selection.end).column == 0 {
                end_row -= 1;
            }
            let start = text.line_start_offset(start_row);
            let end = text.line_end_offset(end_row);
            let block = text.slice(start..end).to_string();
            let range_utf16 = text.offset_to_offset_utf16(start)..text.offset_to_offset_utf16(end);

            let toggled = toggle_line_comments(&block);
            if toggled == block {
                return;
            }

            // Map the cursor onto the toggled text so it stays put within
            // its line, shifted by that line's inserted/removed prefix.
            let mut new_cursor = start + toggled.len();
            let mut old_line_start = start;
            let mut new_line_start = start;
            for (old_line, new_line) in block.split('\n').zip(toggled.split('\n')) {
                if cursor <= old_line_start + old_line.len() {
                    let column = cursor - old_line_start;
                    let column = if new_line.len() >= old_line.len() {
                        column + (new_line.len() - old_line.len())
                    } else {
                        column.saturating_sub(old_line.len() - new_line.len())
                    };
                    new_cursor = new_line_start + column.min(new_line.len());
                    break;
                }
                old_line_start += old_line.len() + 1;
                new_line_start += new_line.len() + 1;
            }

            state.replace_text_in_range(Some(range_utf16), &toggled, window, cx);
            let position = state.text().offset_to_position(new_cursor);
            state.set_cursor_position(position, window, cx);
        });
    }

    /// Move the cursor to the next language-server problem (cmd-j).
    pub(in crate::app) fn next_diagnostic(
        &mut self,
        _: &NextDiagnostic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump_to_diagnostic(true, window, cx);
    }

    /// Move the cursor to the previous language-server problem (cmd-k).
    pub(in crate::app) fn prev_diagnostic(
        &mut self,
        _: &PrevDiagnostic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump_to_diagnostic(false, window, cx);
    }

    /// Put the cursor on the start of the problem `diagnostic_target`
    /// picks and open its popover there. The popover only follows the
    /// mouse otherwise, and the next mouse move closes it, so the message
    /// is echoed in the status bar as well.
    fn jump_to_diagnostic(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let found = self.editor().update(cx, |state, cx| {
            let entries: Vec<_> = state.diagnostics()?.iter().cloned().collect();
            let keys: Vec<_> = entries
                .iter()
                .map(|entry| {
                    (
                        entry.range.start,
                        entry.severity == DiagnosticSeverity::Error,
                    )
                })
                .collect();
            let (ix, rank, total) = diagnostic_target(&keys, state.cursor(), forward)?;
            let is_error = keys[ix].1;
            let entry = entries[ix].clone();
            let position = state.text().offset_to_position(entry.range.start);
            state.set_cursor_position(position, window, cx);
            let message = entry.message.lines().next().unwrap_or_default().to_string();
            state.present_diagnostic(entry, cx);
            Some((is_error, rank, total, message))
        });
        let status = match found {
            Some((is_error, rank, total, message)) => {
                let kind = if is_error { "Error" } else { "Problem" };
                format!("{kind} {rank}/{total}: {message}")
            }
            None => "No problems in this script".to_string(),
        };
        self.set_status(status, cx);
    }
}
