use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::Subscription;
use gpui::{
    App, AppContext as _, Context, Entity, EntityInputHandler as _, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Window,
    div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, TabSize},
    table::{Table, TableState},
    v_flex,
};

use crate::results::ResultsDelegate;
use crate::{AiComplete, OpenFile, RunQuery, SaveFile, ai, config, db, lsp, statement};

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
    status: SharedString,
    running: bool,
    ai_running: bool,
    config: config::Config,
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

        let results = cx.new(|cx| TableState::new(ResultsDelegate::new(), window, cx));

        editor.update(cx, |state, cx| state.focus(window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&conn_input, window, Self::on_conn_input_event),
            cx.subscribe_in(&editor, window, Self::on_editor_event),
            // Flush any debounced (not yet written) changes on quit,
            // and stop the language server.
            cx.on_app_quit(|this, _| {
                config::save(&this.config);
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
            status: "Ready".into(),
            running: false,
            ai_running: false,
            config,
            save_generation: 0,
            syncing_conn_input: false,
            lsp: None,
            _subscriptions: subscriptions,
        };
        this.start_lsp(cx);
        this
    }

    /// Launch the Postgres language server in the background and plug it
    /// into the editor once the handshake completes.
    fn start_lsp(&mut self, cx: &mut Context<Self>) {
        let conn = self.config.connection_string.clone();
        let text = self.editor.read(cx).value().to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { lsp::Client::start(&conn, &text) })
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
        // The connection string changed while the server was starting up;
        // reconnect with the current one instead.
        if client.connection_string() != self.config.connection_string {
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
                    config::save(&this.config);
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

    pub fn ai_complete(&mut self, _: &AiComplete, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_running {
            return;
        }
        let Some(key) = ai::api_key() else {
            self.set_status(
                "AI completion needs ANTHROPIC_API_KEY set in the environment",
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
                    this.config.script_path = Some(path);
                    config::save(&this.config);
                }
                Err(err) => this.set_status(format!("Open failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.editor.read(cx).value().to_string();

        if let Some(path) = self.config.script_path.clone() {
            match std::fs::write(&path, &content) {
                Ok(()) => self.set_status(format!("Saved {}", path.display()), cx),
                Err(err) => self.set_status(format!("Save failed: {err}"), cx),
            }
            return;
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&cwd, Some("script.sql"));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let result = std::fs::write(&path, &content);

            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.set_status(format!("Saved {}", path.display()), cx);
                    this.config.script_path = Some(path);
                    config::save(&this.config);
                }
                Err(err) => this.set_status(format!("Save failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }
}

impl Render for PgGuiApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ai_available = ai::api_key().is_some();

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::run_query))
            .on_action(cx.listener(Self::ai_complete))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::save_file))
            .child(
                // Toolbar: connection string + actions
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
                    .child(Button::new("open").label("Open").on_click(
                        cx.listener(|this, _, window, cx| this.open_file(&OpenFile, window, cx)),
                    ))
                    .child(Button::new("save").label("Save").on_click(
                        cx.listener(|this, _, window, cx| this.save_file(&SaveFile, window, cx)),
                    )),
            )
            .child(
                // SQL editor
                div().flex_1().min_h(px(120.)).p_2().child(
                    Input::new(&self.editor)
                        .h_full()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(cx.theme().mono_font_size),
                ),
            )
            .child(
                // Results table
                div()
                    .flex_1()
                    .min_h(px(120.))
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(Table::new(&self.results)),
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
                    .child(if ai_available {
                        "cmd-enter run · cmd-i AI complete · cmd-o open · cmd-s save"
                    } else {
                        "cmd-enter run · cmd-o open · cmd-s save (set ANTHROPIC_API_KEY for AI)"
                    }),
            )
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
