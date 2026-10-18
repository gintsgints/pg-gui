//! Window chrome: title bar, tab bar, session toolbar and status bar.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, canvas, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    combobox::Combobox,
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    spinner::Spinner,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
};

use crate::app::{PgGuiApp, format_elapsed};

/// Scroll and overflow state of the tab bar's strip of script tabs.
#[derive(Default)]
pub(in crate::app) struct TabStrip {
    /// Its child bounds are what tell which tabs a narrow window clipped.
    scroll: ScrollHandle,
    /// Indices of the tabs clipped on the last frame, listed by the overflow
    /// button. Written after layout, read by the next render.
    hidden: Rc<RefCell<Vec<usize>>>,
    /// The tab last scrolled into view, so a tab that becomes active by any
    /// route (click, menu, keyboard, a new file) is revealed once, without
    /// pinning the strip against the user's own scrolling.
    revealed: Cell<Option<usize>>,
}
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
    ///
    /// The strip scrolls rather than wraps, so a narrow window clips tabs off
    /// either end; a "» N" button then lists the clipped ones. Which tabs are
    /// clipped is only known after layout, so a zero-size canvas in the
    /// suffix (prepainted after the strip) measures them into `TabStrip::hidden`
    /// and re-renders when that set changes.
    pub(in crate::app) fn render_tab_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        if self.tab_strip.revealed.get() != Some(self.active_tab) {
            self.tab_strip.scroll.scroll_to_item(self.active_tab);
            self.tab_strip.revealed.set(Some(self.active_tab));
        }
        let hidden: Vec<usize> = self
            .tab_strip
            .hidden
            .borrow()
            .iter()
            .copied()
            .filter(|&ix| ix < self.tabs.len())
            .collect();
        TabBar::new("script-tabs")
            .small()
            .track_scroll(&self.tab_strip.scroll)
            .selected_index(self.active_tab)
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.activate_tab(*ix, window, cx);
            }))
            .suffix(
                h_flex()
                    .child(self.render_tab_overflow_probe(cx))
                    .when(!hidden.is_empty(), |this| {
                        this.child(self.render_hidden_tabs_button(&hidden, cx))
                    })
                    .child(self.render_session_toolbar(cx)),
            )
            .children(self.tabs.iter().enumerate().map(|(ix, _)| {
                Tab::new().label(self.tab_title(ix)).suffix(
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

    /// A tab's label with its unsaved ("•") or diverged ("⟳") marker.
    fn tab_title(&self, ix: usize) -> String {
        let tab = &self.tabs[ix];
        if tab.diverged {
            format!("⟳ {}", self.tab_label(ix))
        } else if tab.dirty {
            format!("• {}", self.tab_label(ix))
        } else {
            self.tab_label(ix)
        }
    }

    /// Measures, after the strip is laid out and scrolled, which tabs it
    /// clipped, and schedules a re-render when that differs from what the
    /// last render drew the overflow button for. The notify waits for the
    /// next frame because a view cannot be invalidated mid-draw.
    fn render_tab_overflow_probe(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let scroll = self.tab_strip.scroll.clone();
        let hidden = self.tab_strip.hidden.clone();
        let count = self.tabs.len();
        let app = cx.weak_entity();
        canvas(
            move |_, window, _| {
                let clipped = clipped_tabs(&scroll, count);
                if *hidden.borrow() != clipped {
                    *hidden.borrow_mut() = clipped;
                    window.on_next_frame(move |_, cx| {
                        app.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, (), _, _| {},
        )
        .size_0()
    }

    /// The "» N" button listing the tabs the strip clipped; picking one
    /// activates it, which also scrolls it into view.
    fn render_hidden_tabs_button(
        &self,
        hidden: &[usize],
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let items: Vec<(usize, String)> =
            hidden.iter().map(|&ix| (ix, self.tab_title(ix))).collect();
        let count = items.len();
        let app = cx.weak_entity();
        Button::new("hidden-tabs")
            .ghost()
            .small()
            .label(format!("» {count}"))
            .tooltip(if count == 1 {
                "1 tab out of view".to_string()
            } else {
                format!("{count} tabs out of view")
            })
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                menu = menu.scrollable(true);
                for (ix, title) in &items {
                    let ix = *ix;
                    let app = app.clone();
                    menu = menu.item(PopupMenuItem::new(title.clone()).on_click(
                        move |_, window, cx| {
                            app.update(cx, |this, cx| this.activate_tab(ix, window, cx))
                                .ok();
                        },
                    ));
                }
                menu
            })
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

/// Indices of the tabs the strip's viewport does not fully show. Child
/// bounds are recorded before the scroll offset is applied, so the offset is
/// added back; a child the strip has not laid out yet counts as visible.
fn clipped_tabs(scroll: &ScrollHandle, count: usize) -> Vec<usize> {
    let viewport = scroll.bounds();
    let offset = scroll.offset().x;
    // Sub-pixel rounding must not flag a tab that sits flush with an edge.
    let slack = px(1.);
    (0..count)
        .filter(|&ix| {
            scroll.bounds_for_item(ix).is_some_and(|bounds| {
                bounds.left() + offset < viewport.left() - slack
                    || bounds.right() + offset > viewport.right() + slack
            })
        })
        .collect()
}
