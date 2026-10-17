//! Running SQL: the statement under the cursor, batches of script files and
//! EXPLAIN, on the tab's own session — plus the event stream, settle and
//! error paths every run shares, cancel, and Fetch More.

use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use gpui::{
    AppContext as _, Context, EntityInputHandler as _, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::app::{BottomView, PgGuiApp, mask_credentials};
use crate::{CancelQuery, ExplainAnalyze, ExplainPlan, RunQuery, RunScripts, db, plan, statement};

impl PgGuiApp {
    /// The SQL an action should operate on, with a scope label for status
    /// messages: the selected block when there is a selection, otherwise
    /// the statement the cursor is on (or, when the cursor sits after a
    /// statement, the one to its left), which gets selected so it's clear
    /// which one was used.
    pub(in crate::app) fn sql_under_cursor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(String, &'static str)> {
        let selection = self.editor().update(cx, |state, cx| {
            state
                .selected_text_range(false, window, cx)
                .filter(|sel| !sel.range.is_empty())
                .and_then(|sel| state.text_for_range(sel.range, &mut None, window, cx))
                .filter(|text| !text.trim().is_empty())
        });
        if let Some(sql) = selection {
            return Some((sql, "selection"));
        }
        let sql = self.editor().update(cx, |state, cx| {
            let text = state.value();
            let range = statement::at(&text, state.cursor())?;
            let sql = text[range.clone()].to_string();
            state.set_selected_range(range, cx);
            Some(sql)
        })?;
        Some((sql, "statement"))
    }

    pub(in crate::app) fn run_query(
        &mut self,
        _: &RunQuery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        // The input may be showing the masked value; the config always
        // holds the real connection string.
        let conn = self.config.connection_string.clone();

        let Some((sql, scope)) = self.sql_under_cursor(window, cx) else {
            self.set_status("Nothing to run", cx);
            return;
        };

        let ix = self.active_tab;
        let tab_id = self.tabs[ix].id;
        let autocommit = self.tabs[ix].autocommit;
        let batch_size = self.config.fetch_size.max(1);
        let epoch = self.db_epoch;
        // Move the tab's live session into the worker (opened lazily below on
        // the first Run). A single SELECT-style statement pages through a
        // server-side cursor on that connection; scripts and DML run directly.
        let session = self.tabs[ix].session.take();
        self.start_progress(ix, scope, cx);
        // The run streams its own result sets in; drop the previous Run's so
        // the selector only ever shows this one's.
        self.tabs[ix].result.results.clear();
        self.tabs[ix].result.selected = 0;
        self.tabs[ix].result.has_more = false;
        if ix == self.active_tab {
            self.show_tab_result(ix, cx);
        }
        self.set_status(format!("Running {scope}…"), cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            // Open the session on the first Run, off the UI thread.
            let opened = cx
                .background_spawn(async move {
                    match session {
                        Some(session) => Ok(session),
                        None => db::Session::connect(&conn)
                            .map_err(|e| format!("connection failed: {}", db::describe(&e))),
                    }
                })
                .await;
            let mut session = match opened {
                Ok(session) => session,
                Err(err) => {
                    this.update_in(cx, |this, window, cx| {
                        this.on_query_error(tab_id, epoch, None, &err, window, cx);
                    })
                    .ok();
                    return;
                }
            };
            // Publish the cancel token so Cancel can abort the query below.
            let token = session.cancel_token();
            this.update(cx, |this, _| {
                if let Some(ix) = this.tab_index_by_id(tab_id) {
                    this.tabs[ix].cancel = Some(token);
                }
            })
            .ok();

            // The run pushes a log line (and a result set) per statement as
            // it goes; apply them here while the block keeps running, then
            // wait for the run itself to finish.
            let (tx, mut events) = futures::channel::mpsc::unbounded();
            let running = cx.background_spawn(async move {
                let result = session.run(&sql, batch_size, autocommit, &tx);
                (session, result)
            });
            while let Some(event) = events.next().await {
                this.update(cx, |this, cx| this.on_run_event(tab_id, epoch, event, cx))
                    .ok();
            }
            let (session, result) = running.await;
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| match result {
                Ok(run) => this.on_query_ok(tab_id, epoch, session, &run, scope, elapsed, cx),
                Err(err) => this.on_query_error(tab_id, epoch, Some(session), &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Explain the statement under the cursor and draw its plan as a flame
    /// graph (cmd-e).
    pub(in crate::app) fn explain_plan(
        &mut self,
        _: &ExplainPlan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_explain(false, window, cx);
    }

    /// The same, with `ANALYZE` (cmd-shift-e): the widths are then the time
    /// actually spent per node rather than the planner's estimate — at the
    /// price of really executing the statement, inside the tab's transaction
    /// when autocommit is off.
    pub(in crate::app) fn explain_analyze(
        &mut self,
        _: &ExplainAnalyze,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_explain(true, window, cx);
    }

    /// Run an `EXPLAIN` on the active tab's session and hand the plan to the
    /// Plan view. Mirrors [`Self::run_query`] — same session, same
    /// autocommit/transaction, same cancel token — but the statement's output
    /// is one JSON document rather than a stream of result sets, so it takes
    /// no progress channel.
    pub(in crate::app) fn start_explain(
        &mut self,
        analyze: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        let conn = self.config.connection_string.clone();

        let Some((sql, scope)) = self.sql_under_cursor(window, cx) else {
            self.set_status("Nothing to explain", cx);
            return;
        };

        let ix = self.active_tab;
        let tab_id = self.tabs[ix].id;
        let autocommit = self.tabs[ix].autocommit;
        let epoch = self.db_epoch;
        let session = self.tabs[ix].session.take();
        let verb = if analyze {
            "Explaining (analyze)"
        } else {
            "Explaining"
        };
        self.start_progress(ix, format!("{verb} {scope}"), cx);
        self.set_status(format!("{verb} {scope}…"), cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let opened = cx
                .background_spawn(async move {
                    match session {
                        Some(session) => Ok(session),
                        None => db::Session::connect(&conn)
                            .map_err(|e| format!("connection failed: {}", db::describe(&e))),
                    }
                })
                .await;
            let mut session = match opened {
                Ok(session) => session,
                Err(err) => {
                    this.update_in(cx, |this, window, cx| {
                        this.on_query_error(tab_id, epoch, None, &err, window, cx);
                    })
                    .ok();
                    return;
                }
            };
            // `EXPLAIN (ANALYZE)` runs the statement for real, so it is as
            // cancellable as a Run.
            let token = session.cancel_token();
            this.update(cx, |this, _| {
                if let Some(ix) = this.tab_index_by_id(tab_id) {
                    this.tabs[ix].cancel = Some(token);
                }
            })
            .ok();

            let explained = sql.clone();
            let (session, result) = cx
                .background_spawn(async move {
                    let result = session.explain(&explained, analyze, autocommit);
                    (session, result)
                })
                .await;
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| match result {
                Ok(json) => {
                    this.on_plan_ok(tab_id, epoch, session, &json, &sql, analyze, elapsed, cx);
                }
                Err(err) => this.on_query_error(tab_id, epoch, Some(session), &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Settle a finished `EXPLAIN`: put its session back, parse the plan onto
    /// the tab and show it. A plan that will not parse is reported in the log
    /// rather than thrown away silently — the JSON came from the server, so a
    /// failure here is pg-gui's problem to see.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn on_plan_ok(
        &mut self,
        tab_id: u64,
        epoch: u64,
        session: db::Session,
        json: &str,
        sql: &str,
        analyze: bool,
        elapsed: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.settle_session(tab_id, epoch, session) else {
            return;
        };
        // `EXPLAIN (ANALYZE)` really runs the statement, so it raises whatever
        // notices the statement does; they belong in the log like a Run's.
        let notices = self.tabs[ix]
            .session
            .as_mut()
            .map(db::Session::take_notices)
            .unwrap_or_default();
        for notice in notices {
            self.tabs[ix].result.log.push(SharedString::from(notice));
        }

        let parsed = plan::parse(json, analyze, sql);
        let plan = match parsed {
            Ok(plan) => plan,
            Err(err) => {
                let line = format!("EXPLAIN: {err}");
                self.tabs[ix].result.log.push(SharedString::from(line));
                self.bottom_view = BottomView::Log;
                self.show_tab_result(ix, cx);
                self.set_status("Could not read the plan — see the log", cx);
                return;
            }
        };

        let verb = if analyze {
            "explain analyze"
        } else {
            "explain"
        };
        let status = format!(
            "{verb}: {} node(s), {} deep — {} in {elapsed:.0?}",
            plan.nodes, plan.depth, plan.summary,
        );
        self.tabs[ix]
            .result
            .log
            .push(SharedString::from(status.clone()));
        self.tabs[ix].result.plan = Some(plan);
        // A new plan starts unzoomed; the old focus is a path into a tree that
        // no longer exists.
        self.tabs[ix].result.plan_focus = None;
        self.bottom_view = BottomView::Plan;
        if !self.config.results_panel_visible {
            self.config.results_panel_visible = true;
            self.schedule_save(cx);
        }
        self.show_tab_result(ix, cx);
        self.set_status(status, cx);
    }

    /// The picked scripts as `(label, path)`, in run order. The label is
    /// the path relative to the working folder — what the panel shows —
    /// since the absolute path is mostly noise in a log line.
    pub(in crate::app) fn selected_script_batch(&self) -> Vec<(String, PathBuf)> {
        self.selected_script_paths()
            .into_iter()
            .map(|path| {
                let label = self.script_label(&path);
                (label, path)
            })
            .collect()
    }

    /// How a script is named in a log line and in the run confirmation: its
    /// path relative to the working folder — what the panel shows — since
    /// the absolute path is mostly noise.
    pub(in crate::app) fn script_label(&self, path: &Path) -> String {
        self.config
            .working_dir
            .as_deref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// A batch reads its scripts from disk, so a tab still holding unsaved
    /// edits to one of them would run something other than what its editor
    /// shows; note that in the log rather than silently running the old
    /// text.
    pub(in crate::app) fn note_unsaved_scripts(&mut self, ix: usize, files: &[(String, PathBuf)]) {
        let unsaved: Vec<String> = files
            .iter()
            .filter(|(_, path)| {
                self.tabs
                    .iter()
                    .any(|tab| tab.dirty && tab.path.as_deref() == Some(path.as_path()))
            })
            .map(|(label, _)| label.clone())
            .collect();
        for label in unsaved {
            self.tabs[ix].result.log.push(SharedString::from(format!(
                "note: {label} has unsaved edits in a tab — the file on disk is what runs"
            )));
        }
    }

    /// Run the scripts picked in the files panel, in tree order, on the
    /// active tab's session — so they see that tab's autocommit setting,
    /// its open transaction, and report into its log next to ordinary Runs.
    /// Each file is a separate `Session::run`, headed by its own log line,
    /// and the batch stops at the first file that fails (like
    /// `psql -v ON_ERROR_STOP=1`, which is how `sql/00-run-init.sh` applies
    /// the same files). Files that already ran stay committed unless the
    /// tab is holding a transaction open.
    pub(in crate::app) fn run_scripts(
        &mut self,
        _: &RunScripts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        let files = self.selected_script_batch();
        if files.is_empty() {
            self.set_status("No scripts selected", cx);
            return;
        }
        self.prompt_run_scripts(files, window, cx);
    }

    /// Run one script picked from the files panel's context menu. Goes
    /// through the same confirmation as a batch: it is still a file the user
    /// is not necessarily looking at, run against whatever connection the
    /// window happens to be on.
    pub(in crate::app) fn run_one_script(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        let label = self.script_label(&path);
        self.prompt_run_scripts(vec![(label, path)], window, cx);
    }

    /// The name the current connection goes by in the UI — its saved name,
    /// or its URL with the password masked when it has none.
    pub(in crate::app) fn connection_label(&self) -> String {
        self.config
            .recent_connections
            .iter()
            .find(|c| c.url == self.config.connection_string && !c.name.is_empty())
            .map_or_else(
                || mask_credentials(&self.config.connection_string),
                |c| c.name.clone(),
            )
    }

    /// Confirm a batch before it runs. A batch is the one action here that
    /// executes files the user is not looking at — several of them, against
    /// whatever connection the window happens to be on — so it names the
    /// target and lists the files rather than starting on a stray
    /// cmd-shift-enter.
    pub(in crate::app) fn prompt_run_scripts(
        &mut self,
        files: Vec<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A long selection would push the buttons off-screen; the tail is
        // summarised instead.
        const SHOWN: usize = 12;

        if window.has_active_dialog(cx) {
            return;
        }
        let count = files.len();
        let target = self.connection_label();
        let listed: Vec<String> = files
            .iter()
            .take(SHOWN)
            .map(|(label, _)| label.clone())
            .collect();
        let rest = count.saturating_sub(listed.len());
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (run, files) = (app.clone(), files.clone());
            let mut list = v_flex().gap_0p5();
            for label in listed.clone() {
                list = list.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                );
            }
            if rest > 0 {
                list = list.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("…and {rest} more")),
                );
            }
            dialog
                .title("Run scripts")
                // Enter runs, Escape cancels (the dialog's own binding).
                .on_ok({
                    let (run, files) = (run.clone(), files.clone());
                    move |_, window, cx| {
                        let files = files.clone();
                        run.update(cx, |this, cx| this.start_script_run(files, window, cx))
                            .is_ok()
                    }
                })
                .w(px(480.))
                .child(
                    v_flex()
                        .gap_4()
                        .pb_2()
                        .child(
                            div()
                                .text_sm()
                                .child(format!("Run {count} script(s) on “{target}”?")),
                        )
                        .child(list)
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(Button::new("cancel").label("Cancel").on_click(
                                    |_, window, cx| {
                                        window.close_dialog(cx);
                                    },
                                ))
                                .child(
                                    Button::new("run")
                                        .primary()
                                        .label(format!("Run {count} script(s)"))
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let files = files.clone();
                                            run.update(cx, |this, cx| {
                                                this.start_script_run(files, window, cx);
                                            })
                                            .ok();
                                        }),
                                ),
                        ),
                )
        });
    }

    /// Run a confirmed batch: see [`PgGuiApp::run_scripts`].
    pub(in crate::app) fn start_script_run(
        &mut self,
        files: Vec<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        let count = files.len();

        let conn = self.config.connection_string.clone();
        let ix = self.active_tab;
        let tab_id = self.tabs[ix].id;
        let autocommit = self.tabs[ix].autocommit;
        let batch_size = self.config.fetch_size.max(1);
        let epoch = self.db_epoch;
        let session = self.tabs[ix].session.take();
        self.start_progress(ix, format!("{count} script(s)"), cx);
        self.tabs[ix].result.results.clear();
        self.tabs[ix].result.selected = 0;
        self.tabs[ix].result.has_more = false;
        self.note_unsaved_scripts(ix, &files);
        // The per-file lines are the point of a batch; show the log rather
        // than whatever table the tab was left on, and reveal the panel if
        // it was hidden — otherwise the run reports into nothing visible.
        self.bottom_view = BottomView::Log;
        if !self.config.results_panel_visible {
            self.config.results_panel_visible = true;
            self.schedule_save(cx);
        }
        self.show_tab_result(ix, cx);
        self.set_status(format!("Running {count} script(s)…"), cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let opened = cx
                .background_spawn(async move {
                    match session {
                        Some(session) => Ok(session),
                        None => db::Session::connect(&conn)
                            .map_err(|e| format!("connection failed: {}", db::describe(&e))),
                    }
                })
                .await;
            let mut session = match opened {
                Ok(session) => session,
                Err(err) => {
                    this.update_in(cx, |this, window, cx| {
                        this.on_query_error(tab_id, epoch, None, &err, window, cx);
                    })
                    .ok();
                    return;
                }
            };
            let token = session.cancel_token();
            this.update(cx, |this, _| {
                if let Some(ix) = this.tab_index_by_id(tab_id) {
                    this.tabs[ix].cancel = Some(token);
                }
            })
            .ok();

            let (tx, mut events) = futures::channel::mpsc::unbounded();
            let running = cx.background_spawn(async move {
                let result = run_files(&mut session, files, batch_size, autocommit, &tx);
                (session, result)
            });
            while let Some(event) = events.next().await {
                this.update(cx, |this, cx| this.on_run_event(tab_id, epoch, event, cx))
                    .ok();
            }
            let (session, result) = running.await;
            let elapsed = started.elapsed();
            let scope = format!("{count} script(s)");

            this.update_in(cx, |this, window, cx| match result {
                Ok(run) => this.on_query_ok(tab_id, epoch, session, &run, &scope, elapsed, cx),
                Err(err) => this.on_query_error(tab_id, epoch, Some(session), &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Put a finished query's session back on its tab and clear the running
    /// state. Returns the tab index to display the result on, or `None` when
    /// the tab was closed, the connection changed underneath it, or the
    /// connection died (session discarded so the next Run reconnects).
    pub(in crate::app) fn settle_session(
        &mut self,
        tab_id: u64,
        epoch: u64,
        session: db::Session,
    ) -> Option<usize> {
        let ix = self.tab_index_by_id(tab_id)?;
        self.tabs[ix].running = false;
        self.clear_progress(ix);
        self.tabs[ix].cancel = None;
        if epoch != self.db_epoch || session.is_closed() {
            self.tabs[ix].session = None;
            self.tabs[ix].result.has_more = false;
            return None;
        }
        self.tabs[ix].session = Some(session);
        Some(ix)
    }

    /// Apply one streamed run event to its tab: a log line, or a result set
    /// that joins the selector and is shown right away, so a long block's
    /// output appears statement by statement instead of all at the end.
    pub(in crate::app) fn on_run_event(
        &mut self,
        tab_id: u64,
        epoch: u64,
        event: db::RunEvent,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.db_epoch {
            return;
        }
        let Some(ix) = self.tab_index_by_id(tab_id) else {
            return;
        };
        let mut rows = false;
        let mut logged = false;
        match event {
            // Progress, not output: these move the status bar's indicator on
            // and stay out of the message log.
            db::RunEvent::Statement { n, total } => {
                if let Some(progress) = self.tabs[ix].progress.as_mut() {
                    progress.statement = Some((n, total));
                }
            }
            db::RunEvent::File(label) => {
                if let Some(progress) = self.tabs[ix].progress.as_mut() {
                    progress.file = Some(SharedString::from(label));
                    // Each file numbers its statements from 1 again; drop the
                    // previous file's count until this one announces its first.
                    progress.statement = None;
                }
            }
            db::RunEvent::Log(line) | db::RunEvent::Notice(line) => {
                self.tabs[ix].result.log.push(SharedString::from(line));
                logged = true;
            }
            db::RunEvent::Result(set) => {
                rows = !set.rows.is_empty();
                let result = &mut self.tabs[ix].result;
                result.selected = result.results.len();
                result.results.push(set);
            }
        }
        if ix == self.active_tab {
            self.show_tab_result(ix, cx);
        }
        // Reveal the results panel (cmd-3) when a statement returns rows so
        // the output isn't silently hidden, and switch the panel back to the
        // table in case the log was left showing from an earlier run.
        if rows {
            self.reveal_bottom_view(BottomView::Data, cx);
        } else if logged && ix == self.active_tab && self.show_log_for_run(ix) {
            // Nothing to put on the table: a script of DDL and DML returns no
            // result set at all, so the run's only output is the log. Show it,
            // or the whole run plays out behind an empty table.
            self.reveal_bottom_view(BottomView::Log, cx);
        }
        cx.notify();
    }

    /// Whether this log line should pull the bottom panel over to the log:
    /// the run has produced no result set, so the table has nothing to show,
    /// and it has not already done so once (see [`RunProgress::log_shown`]).
    pub(in crate::app) fn show_log_for_run(&mut self, ix: usize) -> bool {
        if !self.tabs[ix].result.results.is_empty() {
            return false;
        }
        match self.tabs[ix].progress.as_mut() {
            // No progress entry: the line comes from outside a run, which has
            // its own idea of which view to show.
            None => false,
            Some(progress) => !std::mem::replace(&mut progress.log_shown, true),
        }
    }

    /// Switch the bottom panel to `view` and open the panel (cmd-3) if it is
    /// closed, so a run's output is never produced into a hidden panel.
    pub(in crate::app) fn reveal_bottom_view(&mut self, view: BottomView, cx: &mut Context<Self>) {
        self.bottom_view = view;
        if !self.config.results_panel_visible {
            self.config.results_panel_visible = true;
            self.schedule_save(cx);
        }
    }

    /// Settle a finished run: put its session back on the tab and report what
    /// it did. The rows and log lines already arrived through
    /// [`Self::on_run_event`].
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn on_query_ok(
        &mut self,
        tab_id: u64,
        epoch: u64,
        session: db::Session,
        run: &db::RunResult,
        scope: &str,
        elapsed: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.settle_session(tab_id, epoch, session) else {
            return;
        };
        let db::RunResult { statements, more } = *run;
        self.tabs[ix].result.has_more = more;
        let result = &self.tabs[ix].result;
        let row_count = result.selected().map_or(0, |set| set.rows.len());
        let sets = result.results.len();
        if ix == self.active_tab {
            cx.notify();
        }
        let more_txt = if more { ", more available" } else { "" };
        let sets_txt = if sets > 1 {
            format!(" of {sets} result sets")
        } else {
            String::new()
        };
        self.set_status(
            format!(
                "{scope}: {statements} statement(s) executed in {elapsed:.0?} — showing {row_count} row(s){sets_txt}{more_txt}"
            ),
            cx,
        );
    }

    /// Record a failed query on its tab: keep a live session (so an aborted
    /// transaction can still be rolled back), discard a dead or superseded
    /// one, clear the tab's result, and surface the error.
    pub(in crate::app) fn on_query_error(
        &mut self,
        tab_id: u64,
        epoch: u64,
        session: Option<db::Session>,
        err: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tab_index_by_id(tab_id) {
            self.tabs[ix].running = false;
            self.clear_progress(ix);
            self.tabs[ix].cancel = None;
            self.tabs[ix].session = session.filter(|s| epoch == self.db_epoch && !s.is_closed());
            let result = &mut self.tabs[ix].result;
            // The statements that did run were logged as they went; the error
            // closes the log off. Their result sets stay on the selector.
            result.log.push(SharedString::from(err.to_string()));
            result.has_more = false;
            if ix == self.active_tab {
                // Switch to the log so the error line the run just appended is
                // visible instead of hiding behind the table.
                self.reveal_bottom_view(BottomView::Log, cx);
                self.show_tab_result(ix, cx);
            }
        }
        self.show_query_error(err, window, cx);
    }

    /// Cancel the query in flight on the active tab, over a side connection.
    /// The in-flight Run reports the cancellation and clears its own running
    /// state when it returns.
    pub(in crate::app) fn cancel_query(
        &mut self,
        _: &CancelQuery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if !tab.running {
            return;
        }
        let Some(token) = tab.cancel.clone() else {
            return;
        };
        self.set_status("Cancelling…", cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { db::cancel(&token) }).await;
            if let Err(err) = result {
                this.update(cx, |this, cx| {
                    this.set_status(format!("Cancel failed: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Surface a failed execution in a dialog — the status bar alone is
    /// easy to miss, and postgres errors are often too long to fit there.
    pub(in crate::app) fn show_query_error(
        &mut self,
        error: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The dialog gets the full multi-line cause; the single-line
        // status bar keeps just the summary.
        let summary = error.lines().next().unwrap_or_default();
        self.set_status(format!("Error: {summary}"), cx);
        let message = SharedString::from(error.to_string());
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog.title("Query failed").w(px(520.)).child(
                div()
                    .pb_2()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(message.clone()),
            )
        });
    }

    /// Fetch More: pull the next batch from the active tab's open cursor and
    /// append it to the results table.
    pub(in crate::app) fn fetch_more_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active_tab;
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        if tab.running || !tab.result.has_more {
            return;
        }
        let Some(mut session) = tab.session.take() else {
            return;
        };
        let tab_id = tab.id;
        let epoch = self.db_epoch;
        let batch_size = self.config.fetch_size.max(1);
        self.start_progress(ix, "more rows", cx);
        self.set_status("Fetching more rows…", cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let (mut session, result) = cx
                .background_spawn(async move {
                    let result = session.fetch_more(batch_size);
                    (session, result)
                })
                .await;
            // The fetch runs outside the Run path's progress channel, so its
            // notices are drained here instead.
            let notices = session.take_notices();
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| match result {
                Ok((rows, more)) => {
                    let Some(ix) = this.settle_session(tab_id, epoch, session) else {
                        return;
                    };
                    let fetched = rows.len();
                    let result = &mut this.tabs[ix].result;
                    result
                        .log
                        .extend(notices.into_iter().map(SharedString::from));
                    // Only a lone statement pages, so the cursor's rows
                    // belong to the one result set the run produced.
                    let total = match result.results.last_mut() {
                        Some(set) => {
                            set.rows.extend(rows.iter().cloned());
                            set.rows.len()
                        }
                        None => 0,
                    };
                    result.has_more = more;
                    if ix == this.active_tab {
                        this.results.update(cx, |table, cx| {
                            table.delegate_mut().append_rows(rows, cx);
                            table.refresh(cx);
                        });
                    }
                    let more_txt = if more { ", more available" } else { "" };
                    this.set_status(
                        format!(
                            "Fetched {fetched} more row(s) in {elapsed:.0?} — {total} row(s) total{more_txt}"
                        ),
                        cx,
                    );
                }
                Err(err) => {
                    this.on_query_error(tab_id, epoch, Some(session), &err, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

fn run_files(
    session: &mut db::Session,
    files: Vec<(String, PathBuf)>,
    batch_size: usize,
    autocommit: bool,
    progress: &db::Progress,
) -> Result<db::RunResult, String> {
    let mut statements = 0;
    let mut more = false;
    for (label, path) in files {
        let sql = std::fs::read_to_string(&path).map_err(|err| format!("{label}: {err}"))?;
        let _ = progress.unbounded_send(db::RunEvent::File(label.clone()));
        let _ = progress.unbounded_send(db::RunEvent::Log(format!("▶ {label}")));
        let run = session
            .run(&sql, batch_size, autocommit, progress)
            .map_err(|err| format!("{label}: {err}"))?;
        statements += run.statements;
        more = run.more;
    }
    Ok(db::RunResult { statements, more })
}
