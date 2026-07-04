use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gpui::Subscription;
use gpui::{
    App, AppContext as _, Context, Entity, EntityInputHandler as _, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Pixels, Render, SharedString, Styled as _,
    Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Root, Sizable as _, Theme, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, RopeExt as _, TabSize},
    list::{List, ListEvent, ListState},
    resizable::{ResizableState, resizable_panel, v_resizable},
    table::{Table, TableState},
    v_flex,
};

use crate::results::ResultsDelegate;
use crate::{
    AiComplete, FormatScript, OpenConfig, OpenFile, OpenSnippets, RunQuery, SaveFile, ShowHelp,
    ToggleToolbar, ZoomIn, ZoomOut, ZoomReset, ai, config, db, lsp, snippets, statement,
};

const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 2.0;

/// Every command with its keybinding(s), shown in the help dialog (cmd-?).
const COMMANDS: &[(&str, &str)] = &[
    (
        "cmd-enter / ctrl-enter",
        "Run the selection or the statement at the cursor",
    ),
    ("cmd-i / ctrl-space", "AI-complete SQL at the cursor"),
    ("cmd-shift-f", "Format the script"),
    ("cmd-p", "Insert a snippet"),
    ("cmd-o", "Open a SQL script"),
    ("cmd-s", "Save the script"),
    ("cmd-b", "Show or hide the toolbar"),
    ("cmd-,", "Open config.json in the system editor"),
    ("cmd-= / cmd--", "Zoom in / out"),
    ("cmd-0", "Reset zoom"),
    ("cmd-?", "Show this help"),
    ("cmd-q", "Quit"),
];

fn default_conn() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "postgres".to_string());
    format!("postgres://{user}@localhost:5432/postgres")
}

/// Replace the username and password in a connection string with stars, for
/// display. Handles both URL (`postgres://user:pass@host/db`) and
/// key-value (`host=… user=… password=…`) forms.
fn mask_credentials(conn: &str) -> String {
    if let Some(scheme_end) = conn.find("://") {
        let auth_start = scheme_end + 3;
        let authority_end = conn[auth_start..]
            .find(['/', '?', '#'])
            .map_or(conn.len(), |i| auth_start + i);
        if let Some(at) = conn[auth_start..authority_end].rfind('@') {
            let stars = if conn[auth_start..auth_start + at].contains(':') {
                "****:****"
            } else {
                "****"
            };
            return format!("{}{stars}{}", &conn[..auth_start], &conn[auth_start + at..]);
        }
        return conn.to_string();
    }
    conn.split_whitespace()
        .map(|pair| match pair.split_once('=') {
            Some((key @ ("user" | "password"), _)) => format!("{key}=****"),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct PgGuiApp {
    conn_input: Entity<InputState>,
    editor: Entity<InputState>,
    results: Entity<TableState<ResultsDelegate>>,
    /// Split state of the editor/results panels; the editor height is
    /// persisted to the config whenever the divider is dragged.
    resizable_state: Entity<ResizableState>,
    status: SharedString,
    running: bool,
    ai_running: bool,
    config: config::Config,
    /// The file the script was opened from or saved to this session, if
    /// any; cmd-s writes there without prompting. Deliberately not
    /// persisted — only the script text is restored across launches.
    script_path: Option<PathBuf>,
    /// Mtime of the config file after our last read or write; a different
    /// mtime on disk means it was edited externally and should be reloaded.
    config_disk_time: Option<SystemTime>,
    /// Theme font sizes at startup, i.e. at 100% zoom; the configured zoom
    /// factor scales these.
    base_font_size: Pixels,
    base_mono_font_size: Pixels,
    save_generation: usize,
    /// True while we programmatically swap the connection field between its
    /// real and credential-masked value, so the change isn't taken as input.
    syncing_conn_input: bool,
    lsp: Option<lsp::Client>,
    _subscriptions: Vec<Subscription>,
}

impl PgGuiApp {
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        snippets::ensure_dir();
        let mut config = config::load();
        // DATABASE_URL (explicit at launch) wins over the saved config, which
        // holds whatever was last typed into the connection field.
        if let Some(url) = std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty()) {
            config.connection_string = url;
        } else if config.connection_string.is_empty() {
            config.connection_string = default_conn();
        }

        let conn_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("postgres://user:password@host:5432/database")
                // The field shows masked credentials until focused; the real
                // value lives in `config.connection_string`.
                .default_value(mask_credentials(&config.connection_string))
        });

        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("sql")
                .multi_line(true)
                .line_number(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    ..Default::default()
                })
                .placeholder("-- Write PostgreSQL here, then press cmd-enter to run")
                .default_value(config.script.clone())
        });

        let results =
            cx.new(|cx| TableState::new(ResultsDelegate::new(config.page_size), window, cx));
        let resizable_state = cx.new(|_| ResizableState::default());

        editor.update(cx, |state, cx| state.focus(window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&conn_input, window, Self::on_conn_input_event),
            cx.subscribe_in(&editor, window, Self::on_editor_event),
            // Flush any debounced (not yet written) changes on quit,
            // and stop the language server.
            cx.on_app_quit(|this, _| {
                this.save_config();
                if let Some(client) = this.lsp.take() {
                    client.shutdown();
                }
                async {}
            }),
        ];

        let mut this = Self {
            conn_input,
            editor,
            results,
            resizable_state,
            status: "Ready".into(),
            running: false,
            ai_running: false,
            config,
            script_path: None,
            config_disk_time: config::modified_time(),
            base_font_size: cx.theme().font_size,
            base_mono_font_size: cx.theme().mono_font_size,
            save_generation: 0,
            syncing_conn_input: false,
            lsp: None,
            _subscriptions: subscriptions,
        };
        this.start_lsp(cx);
        Self::watch_config(window, cx);
        this.apply_zoom(cx);
        this
    }

    /// Write the config and remember the file's new mtime, so the watcher
    /// doesn't mistake our own write for an external edit.
    fn save_config(&mut self) {
        config::save(&self.config);
        self.config_disk_time = config::modified_time();
    }

    /// Poll the config file so external edits (e.g. made via cmd-,) are
    /// picked up live instead of being overwritten by our next save.
    fn watch_config(window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    this.check_config_file(window, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn check_config_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let disk_time = config::modified_time();
        if disk_time.is_none() || disk_time == self.config_disk_time {
            return;
        }
        self.config_disk_time = disk_time;
        match config::try_load() {
            Some(new) => self.apply_external_config(new, window, cx),
            None => self.set_status("config.json is invalid — keeping current settings", cx),
        }
    }

    /// Adopt an externally edited config: swap it in and resync the UI
    /// pieces that mirror it. `editor_height` is the exception — the panel
    /// split isn't writable from outside, so it applies on the next launch.
    fn apply_external_config(
        &mut self,
        new: config::Config,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = std::mem::replace(&mut self.config, new);

        if self.config.script != old.script {
            let script = self.config.script.clone();
            self.editor
                .update(cx, |state, cx| state.set_value(script, window, cx));
        }

        if self.config.connection_string != old.connection_string {
            self.sync_conn_input(mask_credentials(&self.config.connection_string), window, cx);
        }

        // The language server reads all of these from its generated
        // workspace config at startup, so a change means a restart.
        if self.config.connection_string != old.connection_string
            || self.config.keyword_case != old.keyword_case
            || self.config.constant_case != old.constant_case
        {
            self.restart_lsp(cx);
        }

        if self.config.page_size != old.page_size {
            let page_size = self.config.page_size;
            self.results.update(cx, |table, cx| {
                table.delegate_mut().set_page_size(page_size);
                table.refresh(cx);
            });
        }

        if (self.config.zoom - old.zoom).abs() > f32::EPSILON {
            self.apply_zoom(cx);
        }

        self.set_status("Reloaded config.json", cx);
        cx.notify();
    }

    /// Launch the Postgres language server in the background and plug it
    /// into the editor once the handshake completes.
    fn start_lsp(&mut self, cx: &mut Context<Self>) {
        let conn = self.config.connection_string.clone();
        let text = self.editor.read(cx).value().to_string();
        let (keyword_case, constant_case) = (self.config.keyword_case, self.config.constant_case);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    lsp::Client::start(&conn, &text, keyword_case, constant_case)
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok((client, diagnostics)) => this.attach_lsp(client, diagnostics, cx),
                Err(err) => this.set_status(format!("SQL language server unavailable: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn attach_lsp(
        &mut self,
        client: lsp::Client,
        mut diagnostics: lsp::DiagnosticsReceiver,
        cx: &mut Context<Self>,
    ) {
        // The settings the server reads at startup changed while it was
        // starting up; reconnect with the current ones instead.
        if client.connection_string() != self.config.connection_string
            || client.case_options() != (self.config.keyword_case, self.config.constant_case)
        {
            client.shutdown();
            self.start_lsp(cx);
            return;
        }

        let provider = Rc::new(lsp::Provider::new(client.clone()));
        self.editor.update(cx, |state, _| {
            state.lsp.completion_provider = Some(provider.clone());
            state.lsp.hover_provider = Some(provider);
        });
        // Resync whatever was typed while the server was starting.
        client.document_changed(self.editor.read(cx).value().to_string());
        self.lsp = Some(client);
        self.set_status("SQL language server connected", cx);

        cx.spawn(async move |this, cx| {
            while let Some(diagnostics) = diagnostics.next().await {
                let updated = this.update(cx, |this, cx| {
                    this.editor.update(cx, |state, cx| {
                        if let Some(set) = state.diagnostics_mut() {
                            set.clear();
                            set.extend(diagnostics);
                        }
                        cx.notify();
                    });
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Restart the language server so it reconnects with the current
    /// connection string (completions follow the database schema).
    fn restart_lsp(&mut self, cx: &mut Context<Self>) {
        if let Some(client) = self.lsp.take() {
            client.shutdown();
        }
        self.editor.update(cx, |state, _| {
            state.lsp.completion_provider = None;
            state.lsp.hover_provider = None;
        });
        self.start_lsp(cx);
    }

    fn on_conn_input_event(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                if self.syncing_conn_input {
                    return;
                }
                self.config.connection_string = state.read(cx).value().to_string();
                self.schedule_save(cx);
            }
            // Reveal the real connection string while editing, mask the
            // credentials the rest of the time.
            InputEvent::Focus => {
                self.sync_conn_input(self.config.connection_string.clone(), window, cx);
            }
            InputEvent::Blur => {
                self.sync_conn_input(mask_credentials(&self.config.connection_string), window, cx);
            }
            InputEvent::PressEnter { .. } => {}
        }
    }

    fn sync_conn_input(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        self.syncing_conn_input = true;
        self.conn_input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
        });
        self.syncing_conn_input = false;
    }

    fn on_editor_event(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            let text = state.read(cx).value().to_string();
            if let Some(client) = &self.lsp {
                client.document_changed(text.clone());
            }
            self.config.script = text;
            self.schedule_save(cx);
        }
    }

    /// Persist the config after a short debounce, so typing in the editor
    /// doesn't hit the disk on every keystroke. Also restarts the language
    /// server when the connection string has settled on a new value.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            this.update(cx, |this, cx| {
                if this.save_generation == generation {
                    this.save_config();
                    if this.lsp.as_ref().is_some_and(|client| {
                        client.connection_string() != this.config.connection_string
                    }) {
                        this.restart_lsp(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_status(&mut self, status: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = status.into();
        cx.notify();
    }

    /// Open the searchable snippet picker; the confirmed snippet is
    /// inserted into the editor at the cursor.
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn open_snippet_picker(
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

    /// Insert a snippet at the cursor as its own statement. When the
    /// snippet contains a `%%` filter placeholder, the caret is placed
    /// between the two `%` so typing narrows the filter.
    fn insert_snippet(
        &mut self,
        snippet: &snippets::Snippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |state, cx| {
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

            let placeholder = inserted.find("%%");
            state.insert(inserted.clone(), window, cx);
            if let Some(pos) = placeholder {
                let target = state.cursor() - inserted.len() + pos + 1;
                let position = state.text().offset_to_position(target);
                state.set_cursor_position(position, window, cx);
            } else {
                state.focus(window, cx);
            }
        });
        self.set_status(format!("Inserted “{}”", snippet.name), cx);
    }

    pub fn run_query(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        // The input may be showing the masked value; the config always
        // holds the real connection string.
        let conn = self.config.connection_string.clone();

        // Run the selected block when there is a selection, otherwise the
        // statement the cursor is on (or, when the cursor sits after a
        // statement, the one to its left).
        let selection = self.editor.update(cx, |state, cx| {
            state
                .selected_text_range(false, window, cx)
                .filter(|sel| !sel.range.is_empty())
                .and_then(|sel| state.text_for_range(sel.range, &mut None, window, cx))
                .filter(|text| !text.trim().is_empty())
        });
        let (sql, scope) = if let Some(sql) = selection {
            (sql, "selection")
        } else {
            let state = self.editor.read(cx);
            let text = state.value();
            let sql = statement::at(&text, state.cursor())
                .map(|range| text[range].to_string())
                .unwrap_or_default();
            (sql, "statement")
        };
        if sql.trim().is_empty() {
            self.set_status("Nothing to run", cx);
            return;
        }

        self.running = true;
        self.set_status(format!("Running {scope}…"), cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let result = cx
                .background_spawn(async move { db::run_script(&conn, &sql) })
                .await;
            let elapsed = started.elapsed();

            this.update(cx, |this, cx| {
                this.running = false;
                match result {
                    Ok(outcome) => {
                        let row_count = outcome.rows.len();
                        let statements = outcome.messages.len();
                        this.results.update(cx, |table, cx| {
                            table.delegate_mut().set_data(outcome.columns, outcome.rows);
                            table.refresh(cx);
                        });
                        this.set_status(
                            format!(
                                "{scope}: {statements} statement(s) executed in {elapsed:.0?} — showing {row_count} row(s)"
                            ),
                            cx,
                        );
                    }
                    Err(err) => {
                        this.results.update(cx, |table, cx| {
                            table.delegate_mut().clear();
                            table.refresh(cx);
                        });
                        this.set_status(format!("Error: {err}"), cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Toolbar with the connection string and action buttons; hidden and
    /// shown with cmd-b.
    fn render_toolbar(
        &self,
        ai_available: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        h_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().child(Input::new(&self.conn_input)))
            .child(
                Button::new("run")
                    .primary()
                    .label(if self.running { "Running…" } else { "Run" })
                    .disabled(self.running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.run_query(&RunQuery, window, cx);
                    })),
            )
            .child(
                Button::new("ai")
                    .label(if self.ai_running {
                        "AI…"
                    } else {
                        "AI Complete"
                    })
                    .disabled(self.ai_running || !ai_available)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.ai_complete(&AiComplete, window, cx);
                    })),
            )
            .child(
                Button::new("snippets")
                    .label("Snippets")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_snippet_picker(&OpenSnippets, window, cx);
                    })),
            )
            .child(
                Button::new("open").label("Open").on_click(
                    cx.listener(|this, _, window, cx| this.open_file(&OpenFile, window, cx)),
                ),
            )
            .child(
                Button::new("save").label("Save").on_click(
                    cx.listener(|this, _, window, cx| this.save_file(&SaveFile, window, cx)),
                ),
            )
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
        if page_count <= 1 {
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
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Page {} of {page_count} · {total_rows} rows",
                            page + 1
                        )),
                ),
        )
    }

    fn change_results_page(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.results.update(cx, |table, cx| {
            let moved = if forward {
                table.delegate_mut().next_page()
            } else {
                table.delegate_mut().prev_page()
            };
            if moved {
                table.scroll_to_row(0, cx);
                table.refresh(cx);
            }
        });
        cx.notify();
    }

    pub fn ai_complete(&mut self, _: &AiComplete, window: &mut Window, cx: &mut Context<Self>) {
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

        let (before, after) = {
            let state = self.editor.read(cx);
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
                .background_spawn(async move { ai::complete(&key, &before, &after) })
                .await;

            this.update_in(cx, |this, window, cx| {
                this.ai_running = false;
                match result {
                    Ok(completion) => {
                        this.editor.update(cx, |state, cx| {
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

    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open SQL script".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let content = std::fs::read_to_string(&path);

            this.update_in(cx, |this, window, cx| match content {
                Ok(content) => {
                    this.editor.update(cx, |state, cx| {
                        state.set_value(content.clone(), window, cx);
                    });
                    this.set_status(format!("Opened {}", path.display()), cx);
                    this.config.script = content;
                    this.script_path = Some(path);
                    this.save_config();
                }
                Err(err) => this.set_status(format!("Open failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Open the config file in the system default editor (cmd-,).
    /// Saved edits are picked up live by the config watcher.
    pub fn open_config(&mut self, _: &OpenConfig, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = config::path() else {
            self.set_status("No config directory on this platform", cx);
            return;
        };
        // Write the current state first, so the file exists on a fresh
        // install and reflects this session rather than a stale launch.
        self.save_config();
        cx.open_with_system(&path);
        self.set_status(
            format!("Opened {} — saved edits reload live", path.display()),
            cx,
        );
    }

    /// Open a dialog listing every command and its keybinding (cmd-?).
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn show_help(&mut self, _: &ShowHelp, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }

        window.open_dialog(cx, |dialog, _, cx| {
            dialog
                .title("Commands")
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_1()
                        .pb_2()
                        .text_sm()
                        .children(COMMANDS.iter().map(|(keys, description)| {
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(190.))
                                        .flex_none()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(*keys),
                                )
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(*description),
                                )
                        })),
                )
        });
    }

    pub fn toggle_toolbar(&mut self, _: &ToggleToolbar, _: &mut Window, cx: &mut Context<Self>) {
        self.config.toolbar_visible = !self.config.toolbar_visible;
        self.schedule_save(cx);
        cx.notify();
    }

    pub fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.config.zoom + ZOOM_STEP, cx);
    }

    pub fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.config.zoom - ZOOM_STEP, cx);
    }

    pub fn zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(1.0, cx);
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        // Snap to the step grid so repeated f32 steps don't accumulate drift.
        self.config.zoom = (zoom / ZOOM_STEP).round() * ZOOM_STEP;
        self.apply_zoom(cx);
        self.set_status(format!("Zoom {:.0}%", self.config.zoom * 100.), cx);
        self.schedule_save(cx);
    }

    /// Scale the theme font sizes by the configured zoom factor. All
    /// default-sized text follows `font_size` (the root sets the window rem
    /// size from it each frame); the SQL editor follows `mono_font_size`.
    fn apply_zoom(&mut self, cx: &mut Context<Self>) {
        let zoom = self.config.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.config.zoom = zoom;
        let theme = Theme::global_mut(cx);
        theme.font_size = self.base_font_size * zoom;
        theme.mono_font_size = self.base_mono_font_size * zoom;
        cx.refresh_windows();
    }

    /// Format the script through the language server without saving
    /// (cmd-shift-f).
    pub fn format_script(&mut self, _: &FormatScript, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.clone() else {
            self.set_status("Format unavailable — SQL language server not connected", cx);
            return;
        };

        let text = self.editor.read(cx).value().to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.format(&text).await;
            this.update_in(cx, |this, window, cx| match result {
                // Skip stale results: the buffer changed while the
                // server was formatting.
                Ok(Some(formatted)) if this.editor.read(cx).value() == text => {
                    this.apply_formatted(&formatted, window, cx);
                    this.set_status("Formatted script", cx);
                }
                Ok(Some(_)) => {}
                Ok(None) => this.set_status("Script already formatted", cx),
                Err(err) => this.set_status(format!("Format failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.clone().filter(|_| self.config.format_on_save) else {
            self.write_script(window, cx);
            return;
        };

        let text = self.editor.read(cx).value().to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.format(&text).await;
            this.update_in(cx, |this, window, cx| {
                let format_error = match result {
                    // Skip stale results: the buffer changed while the
                    // server was formatting.
                    Ok(Some(formatted)) if this.editor.read(cx).value() == text => {
                        this.apply_formatted(&formatted, window, cx);
                        None
                    }
                    Ok(_) => None,
                    Err(err) => Some(err),
                };
                this.write_script(window, cx);
                if let Some(err) = format_error {
                    this.set_status(format!("Format on save failed: {err}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Swap the formatted text into the editor as a single undoable edit,
    /// keeping the cursor near where it was. Goes through the input
    /// handler so the usual change plumbing (LSP sync, config save) runs.
    fn apply_formatted(&mut self, formatted: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            let cursor = state.cursor();
            let full_range = 0..state.value().chars().map(char::len_utf16).sum();
            state.replace_text_in_range(Some(full_range), formatted, window, cx);

            let mut offset = cursor.min(formatted.len());
            while offset > 0 && !formatted.is_char_boundary(offset) {
                offset -= 1;
            }
            let position = state.text().offset_to_position(offset);
            state.set_cursor_position(position, window, cx);
        });
    }

    /// Write the editor content to the script file, prompting for a
    /// location the first time.
    fn write_script(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.editor.read(cx).value().to_string();

        if let Some(path) = self.script_path.clone() {
            match std::fs::write(&path, &content) {
                Ok(()) => self.set_status(format!("Saved {}", path.display()), cx),
                Err(err) => self.set_status(format!("Save failed: {err}"), cx),
            }
            return;
        }

        let dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&dir, Some("script.sql"));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let result = std::fs::write(&path, &content);

            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.set_status(format!("Saved {}", path.display()), cx);
                    this.script_path = Some(path);
                }
                Err(err) => this.set_status(format!("Save failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }
}

impl Render for PgGuiApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ai_available = ai::api_key(&self.config.ai_api_key).is_some();

        v_flex()
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::run_query))
            .on_action(cx.listener(Self::ai_complete))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::save_file))
            .on_action(cx.listener(Self::open_snippet_picker))
            .on_action(cx.listener(Self::open_config))
            .on_action(cx.listener(Self::toggle_toolbar))
            .on_action(cx.listener(Self::format_script))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::show_help))
            .children(
                self.config
                    .toolbar_visible
                    .then(|| self.render_toolbar(ai_available, cx)),
            )
            .child(
                // Editor over results, split by a draggable divider. The
                // split position is persisted to the config on drag and
                // restored on launch via the editor panel's initial size.
                div().flex_1().min_h(px(0.)).child(
                    v_resizable("editor-results")
                        .with_state(&self.resizable_state)
                        .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                            if let Some(height) = state.read(cx).sizes().first() {
                                this.config.editor_height = Some(f32::from(*height));
                                this.schedule_save(cx);
                            }
                        }))
                        .child({
                            // SQL editor
                            let mut panel = resizable_panel().child(
                                div().size_full().p_2().child(
                                    Input::new(&self.editor)
                                        .h_full()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_size(cx.theme().mono_font_size),
                                ),
                            );
                            if let Some(height) = self.config.editor_height {
                                panel = panel.size(px(height));
                            }
                            panel
                        })
                        .child(
                            // Results table with pager
                            resizable_panel().child(
                                v_flex()
                                    .size_full()
                                    .p_2()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h(px(0.))
                                            .child(Table::new(&self.results)),
                                    )
                                    .children(self.render_results_pager(cx)),
                            ),
                        ),
                ),
            )
            .child(
                // Status bar
                h_flex()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone())
                    .child(div().flex_1())
                    .child(format!(
                        "{} · {} · cmd-? help",
                        if ai_available {
                            "AI ready"
                        } else {
                            "AI off — set ai_api_key or ANTHROPIC_API_KEY"
                        },
                        if self.lsp.is_some() {
                            "SQL LSP connected"
                        } else {
                            "SQL LSP offline"
                        },
                    )),
            )
            // Dialogs (e.g. the snippet picker) are drawn by the app's root
            // element; gpui-component's Root only stores them.
            .children(Root::render_dialog_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::mask_credentials;

    #[test]
    fn masks_user_and_password_in_url() {
        assert_eq!(
            mask_credentials("postgres://alice:secret@localhost:5432/db"),
            "postgres://****:****@localhost:5432/db"
        );
    }

    #[test]
    fn masks_user_without_password() {
        assert_eq!(
            mask_credentials("postgres://alice@localhost/db"),
            "postgres://****@localhost/db"
        );
    }

    #[test]
    fn leaves_credential_free_urls_untouched() {
        assert_eq!(
            mask_credentials("postgres://localhost:5432/db"),
            "postgres://localhost:5432/db"
        );
    }

    #[test]
    fn masks_credentials_in_query_only_urls() {
        assert_eq!(
            mask_credentials("postgres://alice:secret@localhost?sslmode=require"),
            "postgres://****:****@localhost?sslmode=require"
        );
    }

    #[test]
    fn masks_key_value_form() {
        assert_eq!(
            mask_credentials("host=localhost user=alice password=secret dbname=db"),
            "host=localhost user=**** password=**** dbname=db"
        );
    }
}
