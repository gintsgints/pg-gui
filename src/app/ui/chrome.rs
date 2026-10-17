//! Window chrome: title bar, tab bar, session toolbar and status bar.

use std::fmt::Write as _;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    combobox::Combobox,
    h_flex,
    spinner::Spinner,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
};

use crate::app::{PgGuiApp, format_elapsed};
use crate::{CancelQuery, Commit, NewFile, Rollback, ToggleAutocommit, ToggleFormatOnSave, ai};

impl PgGuiApp {
    /// Custom title bar (the native one is transparent, see main.rs): the
    /// app name, the connection picker, and the active tab's file path,
    /// Zed style. `TitleBar` pads past the macOS traffic lights.
    pub(in crate::app) fn render_title_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let path = self.tabs[self.active_tab]
            .path
            .as_deref()
            .map(|path| path.display().to_string());
        let title_bar = TitleBar::new();
        // Linux/Windows: the drawn menu bar sits at the head of the title bar,
        // before the app name. macOS relies on the OS menu bar instead.
        #[cfg(not(target_os = "macos"))]
        let title_bar =
            title_bar.child(div().flex().items_center().child(self.app_menu_bar.clone()));
        title_bar.child(
            h_flex()
                .gap_2()
                .flex_1()
                .min_w(px(0.))
                .child(div().text_sm().child("pg-gui"))
                .child(
                    div()
                        .id("connection-combo")
                        .tooltip(|window, cx| {
                            Tooltip::new("Active connection — click to switch").build(window, cx)
                        })
                        .child(
                            Combobox::new(&self.connections)
                                .small()
                                .w(px(320.))
                                .menu_width(px(420.))
                                .placeholder("Select connection…")
                                .search_placeholder("Switch or search connections…"),
                        ),
                )
                .children(path.map(|path| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(path)
                })),
        )
    }

    /// The status line at the bottom: the latest message on the left, the
    /// format-on-save switch and the AI / language-server availability
    /// summary on the right. The `fmt:` segment is the only interactive
    /// piece; it stays plain text (not a `Button`) so the bar keeps its
    /// thin single-line typography, with the pointer cursor and a hover
    /// brightening as the affordance.
    /// The left-hand end of the status bar: a live indicator while the active
    /// tab has work in flight (spinner, what is running, the file and
    /// statement it has reached, and a clock), else the last status message.
    ///
    /// The clock is read from the tab's start instant on every frame, so it
    /// keeps counting across a tab switch; the repaints come from
    /// [`Self::start_progress_tick`].
    fn render_run_indicator(&self) -> AnyElement {
        let Some(progress) = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.progress.as_ref())
        else {
            return div().child(self.status.clone()).into_any_element();
        };
        let mut line = format!("Running {}", progress.scope);
        if let Some(file) = &progress.file {
            let _ = write!(line, " · {file}");
        }
        // A lone statement needs no count; "1/1" is noise.
        if let Some((n, total)) = progress.statement.filter(|(_, total)| *total > 1) {
            let _ = write!(line, " · statement {n}/{total}");
        }
        let _ = write!(line, " · {}", format_elapsed(progress.started.elapsed()));
        h_flex()
            .gap_2()
            .child(Spinner::new().xsmall())
            .child(line)
            .into_any_element()
    }

    pub(in crate::app) fn render_status_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ai_available = ai::api_key(&self.config.ai_api_key).is_some();
        let format_on_save = self.config.format_on_save;
        h_flex()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.render_run_indicator())
            .child(div().flex_1())
            .child(
                div()
                    .id("format-on-save")
                    .cursor_pointer()
                    .hover(|this| this.text_color(cx.theme().foreground))
                    .tooltip(move |window, cx| {
                        Tooltip::new(if format_on_save {
                            "Format on save ON — cmd-s formats first. Click to disable."
                        } else {
                            "Format on save OFF — cmd-s writes as typed. Click to enable."
                        })
                        .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_format_on_save(&ToggleFormatOnSave, window, cx);
                    }))
                    .child(if format_on_save {
                        "fmt: on"
                    } else {
                        "fmt: off"
                    }),
            )
            .child(format!(
                " · {} · {} · cmd-h help",
                if ai_available {
                    "AI ready"
                } else {
                    "AI off — set ai_api_key or ANTHROPIC_API_KEY"
                },
                if self.lsp.is_some() {
                    self.lsp_schema.label()
                } else {
                    "SQL LSP offline"
                },
            ))
    }

    /// One tab per open script, with a "×" close button each and a
    /// trailing "+" that opens a fresh one. A tab with unsaved edits is
    /// marked with a leading "•", or "⟳" when its file also changed on disk
    /// (diverged). These are text glyphs rather than `IconName` SVGs.
    pub(in crate::app) fn render_tab_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        TabBar::new("script-tabs")
            .small()
            .selected_index(self.active_tab)
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.activate_tab(*ix, window, cx);
            }))
            .suffix(self.render_session_toolbar(cx))
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let label = if tab.diverged {
                    format!("⟳ {}", self.tab_label(ix))
                } else if tab.dirty {
                    format!("• {}", self.tab_label(ix))
                } else {
                    self.tab_label(ix)
                };
                Tab::new().label(label).suffix(
                    Button::new(("close-tab", ix))
                        .ghost()
                        .xsmall()
                        .label("×")
                        .tooltip("Close tab")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.request_close_tab(ix, window, cx);
                        })),
                )
            }))
    }

    /// Session controls sitting after the tabs, next to "+": an autocommit
    /// toggle, Commit/Rollback (enabled only with an open transaction), a
    /// Cancel button (enabled while a query runs), and the new-tab "+".
    /// They act on the active tab's session; the buttons are labelled with
    /// text glyphs rather than `IconName` SVGs.
    fn render_session_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let autocommit = self.active_autocommit();
        let running = self.active_running();
        // Commit/Rollback only make sense on a manual transaction that has
        // actually begun, and never while a query is in flight.
        let txn_actionable = !autocommit && self.active_in_txn() && !running;

        let autocommit_base = Button::new("session-autocommit")
            .small()
            .label(if autocommit { "AC: on" } else { "AC: off" })
            .tooltip(if autocommit {
                "Autocommit ON — each Run commits. Click for manual transactions."
            } else {
                "Autocommit OFF — Run works inside a transaction. Click to commit each Run."
            });
        // Highlight the non-default (manual transaction) mode.
        let autocommit_btn = if autocommit {
            autocommit_base.ghost()
        } else {
            autocommit_base.primary()
        }
        .on_click(cx.listener(|this, _, window, cx| {
            this.toggle_autocommit(&ToggleAutocommit, window, cx);
        }));

        let commit_btn = Button::new("session-commit")
            .outline()
            .small()
            .label("✓")
            .tooltip("Commit transaction")
            .disabled(!txn_actionable)
            .on_click(cx.listener(|this, _, window, cx| {
                this.commit_txn(&Commit, window, cx);
            }));

        let rollback_btn = Button::new("session-rollback")
            .outline()
            .small()
            .label("↺")
            .tooltip("Roll back transaction")
            .disabled(!txn_actionable)
            .on_click(cx.listener(|this, _, window, cx| {
                this.rollback_txn(&Rollback, window, cx);
            }));

        // Filled while a query is in flight, flat when there is nothing to
        // cancel: disabling alone only dims it, which reads the same in both
        // states and hides the one way out of a long query.
        let cancel_base = Button::new("session-cancel")
            .small()
            .label("⊘")
            .tooltip("Cancel running query")
            .disabled(!running)
            .on_click(cx.listener(|this, _, window, cx| {
                this.cancel_query(&CancelQuery, window, cx);
            }));
        let cancel_btn = if running {
            cancel_base.danger()
        } else {
            cancel_base.ghost()
        };

        let new_tab_btn = Button::new("new-tab")
            .ghost()
            .small()
            .label("+")
            .tooltip("New script tab")
            .on_click(cx.listener(|this, _, window, cx| {
                this.new_file(&NewFile, window, cx);
            }));

        h_flex()
            .gap_1()
            .px_1()
            .child(autocommit_btn)
            .child(commit_btn)
            .child(rollback_btn)
            .child(cancel_btn)
            .child(new_tab_btn)
    }
}
