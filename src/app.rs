use std::path::PathBuf;

use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Window, div, px,
};
use gpui::Subscription;
use gpui_component::{
    ActiveTheme as _, Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, TabSize},
    table::{Table, TableState},
    v_flex,
};

use crate::results::ResultsDelegate;
use crate::{AiComplete, OpenFile, RunQuery, SaveFile, ai, config, db};

fn default_conn() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "postgres".to_string());
    format!("postgres://{user}@localhost:5432/postgres")
}

pub struct PgGuiApp {
    conn_input: Entity<InputState>,
    editor: Entity<InputState>,
    results: Entity<TableState<ResultsDelegate>>,
    status: SharedString,
    running: bool,
    ai_running: bool,
    file_path: Option<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

impl PgGuiApp {
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // DATABASE_URL (explicit at launch) wins over the saved config, which
        // holds whatever was last typed into the connection field.
        let conn_str = std::env::var("DATABASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let saved = config::load().connection_string;
                (!saved.is_empty()).then_some(saved)
            })
            .unwrap_or_else(default_conn);
        let conn_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("postgres://user:password@host:5432/database")
                .default_value(conn_str)
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
        });

        let results = cx.new(|cx| TableState::new(ResultsDelegate::new(), window, cx));

        editor.update(cx, |state, cx| state.focus(window, cx));

        let subscriptions =
            vec![cx.subscribe_in(&conn_input, window, Self::on_conn_input_event)];

        Self {
            conn_input,
            editor,
            results,
            status: "Ready".into(),
            running: false,
            ai_running: false,
            file_path: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_conn_input_event(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            config::save(&config::Config {
                connection_string: state.read(cx).value().to_string(),
            });
        }
    }

    fn set_status(&mut self, status: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = status.into();
        cx.notify();
    }

    pub fn run_query(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let conn = self.conn_input.read(cx).value().to_string();
        let sql = self.editor.read(cx).value().to_string();
        if sql.trim().is_empty() {
            self.set_status("Nothing to run", cx);
            return;
        }

        self.running = true;
        self.set_status("Running…", cx);

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
                                "{statements} statement(s) executed in {:.0?} — showing {row_count} row(s)",
                                elapsed
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
            self.set_status("AI completion needs ANTHROPIC_API_KEY set in the environment", cx);
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

    pub fn open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open SQL script".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let content = std::fs::read_to_string(&path);

            this.update_in(cx, |this, window, cx| match content {
                Ok(content) => {
                    this.editor.update(cx, |state, cx| {
                        state.set_value(content, window, cx);
                    });
                    this.set_status(format!("Opened {}", path.display()), cx);
                    this.file_path = Some(path);
                }
                Err(err) => this.set_status(format!("Open failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.editor.read(cx).value().to_string();

        if let Some(path) = self.file_path.clone() {
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
                    this.file_path = Some(path);
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
                                this.run_query(&RunQuery, window, cx)
                            })),
                    )
                    .child(
                        Button::new("ai")
                            .label(if self.ai_running { "AI…" } else { "AI Complete" })
                            .disabled(self.ai_running || !ai_available)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.ai_complete(&AiComplete, window, cx)
                            })),
                    )
                    .child(Button::new("open").label("Open").on_click(cx.listener(
                        |this, _, window, cx| this.open_file(&OpenFile, window, cx),
                    )))
                    .child(Button::new("save").label("Save").on_click(cx.listener(
                        |this, _, window, cx| this.save_file(&SaveFile, window, cx),
                    ))),
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
