//! Editor tabs: the per-tab state (buffer, file, session, last output) and
//! the lifecycle that builds, activates, closes and tracks them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gpui::{
    AppContext as _, Context, Entity, ParentElement as _, SharedString, Styled as _, Subscription,
    Window, div, px,
};
use gpui_component::{
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    chart::FlamePath,
    h_flex,
    input::{EditorState, TabSize},
    v_flex,
};

use super::{PgGuiApp, file_mtime};
use crate::{
    CloseTab, NextTab, PrevTab, Quit, config, db, definitions, highlight, lsp, plan, statement,
};

/// How often the status bar's clock is repainted while a run is in flight.
/// Fast enough that a tenth-of-a-second reading moves smoothly, and the loop
/// only exists while something is running.
const PROGRESS_TICK: Duration = Duration::from_millis(200);

/// One open script: its editor buffer and the file it belongs to, if
/// any — cmd-s writes there without prompting. Mirrored in
/// `config.tabs` at the same index, which holds the persisted text.
// The bools are independent per-tab flags (edit/disk state, snippet mode,
// autocommit, in-flight); they don't form a state machine, so the
// excessive-bools lint is a false positive here.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct EditorTab {
    pub(super) editor: Entity<EditorState>,
    pub(super) path: Option<PathBuf>,
    /// The content last written to (or read from) disk — the baseline the
    /// buffer is compared against to decide if the tab has unsaved edits.
    /// Empty for a never-saved tab. Not persisted: recomputed on launch
    /// from the file so restored edits still register as unsaved.
    pub(super) saved: String,
    /// Whether the buffer differs from `saved`, cached so the tab bar can
    /// show a marker without diffing on every frame. Always `false` for an
    /// untitled tab: it has no file to be stale against, and its text is
    /// persisted to config.json, so closing or quitting never prompts.
    pub(super) dirty: bool,
    /// Mtime of `path` after our last read or write; a newer mtime on disk
    /// means the file was edited externally. `None` for a tab with no file
    /// (or one whose file is missing).
    pub(super) disk_time: Option<SystemTime>,
    /// Set when the file changed on disk while the tab had unsaved edits, so
    /// a plain reload would clobber one side or the other. Shown with a
    /// distinct tab glyph and enforced with a prompt before overwriting.
    pub(super) diverged: bool,
    /// Set after inserting a snippet with tab-stop markers: tab visits the
    /// next `$n` marker in the buffer instead of indenting. Cleared by
    /// escape or once no markers remain, so a stray `$1` in hand-written
    /// SQL never hijacks the tab key.
    pub(super) snippet_mode: bool,
    /// Default filename proposed by the Save As dialog for a never-saved tab
    /// (`None` falls back to `script.sql`). Set when the tab was opened from a
    /// database object's definition, so its file is proposed as
    /// `<object>.sql`. Cleared once the tab has a real path.
    pub(super) suggested_name: Option<String>,
    _subscription: Subscription,
    /// Stable id, so a query completing after the tab was reordered or
    /// closed still lands on the right tab (indices shift; ids don't).
    pub(super) id: u64,
    /// This tab's live database session — one persistent connection, opened
    /// lazily on the first Run and isolated from every other tab's. `None`
    /// before the first Run or after it was torn down (connection change,
    /// dropped connection).
    pub(super) session: Option<db::Session>,
    /// Whether this tab's session commits each Run immediately (ON) or runs
    /// inside a transaction ended by Commit/Rollback (OFF). Seeded from
    /// `config.autocommit`; not persisted per tab.
    pub(super) autocommit: bool,
    /// A query is in flight on this tab's session (single in-flight per tab).
    pub(super) running: bool,
    /// What that in-flight work is and how long it has been going. Set
    /// alongside `running` and cleared with it, so the status bar can show
    /// the tab it is looking at rather than whichever run reported last.
    pub(super) progress: Option<RunProgress>,
    /// Cancels the in-flight query; set while `running`, taken by Cancel.
    pub(super) cancel: Option<postgres::CancelToken>,
    /// This tab's last query output, mirrored into the shared results table
    /// and log view while the tab is active and restored when it is
    /// reactivated.
    pub(super) result: TabResult,
}

/// Work in flight on a tab, as the status bar shows it. The elapsed time is
/// derived from `started` on every frame rather than stored, so it keeps
/// counting while the user is on another tab.
pub(super) struct RunProgress {
    /// What is running, in the words the finished run will use in its own
    /// status line ("statement", "3 script(s)", "EXPLAIN ANALYZE", …).
    pub(super) scope: SharedString,
    /// When the work started; the clock in the status bar counts from here.
    pub(super) started: std::time::Instant,
    /// The script file being executed, for a batch run. `None` for anything
    /// that runs the buffer.
    pub(super) file: Option<SharedString>,
    /// The statement about to run and the run's statement count, from the
    /// moment it is announced. `None` until the first announcement, and for
    /// work that does not run statements (fetch, export).
    pub(super) statement: Option<(usize, usize)>,
    /// Whether this run has already pulled the bottom panel over to the log.
    /// It does that once, on its first log line, so a user who switches back
    /// to the table mid-run is not dragged away again by the next statement.
    pub(super) log_shown: bool,
}

impl RunProgress {
    /// Start the clock on `scope`.
    pub(super) fn new(scope: impl Into<SharedString>) -> Self {
        Self {
            scope: scope.into(),
            started: std::time::Instant::now(),
            file: None,
            statement: None,
            log_shown: false,
        }
    }
}

/// A tab's last query output, mirrored into the shared results table and
/// log view when the tab is active.
#[derive(Default)]
pub(super) struct TabResult {
    /// One entry per statement of the last Run that returned rows, in the
    /// order they arrived; the result selector switches between them.
    pub(super) results: Vec<db::ResultSet>,
    /// Index into `results` of the set the table is showing.
    pub(super) selected: usize,
    /// The cursor is still open on the tab's session, so Fetch More is valid.
    /// Only a lone statement pages, so it always belongs to the last set.
    pub(super) has_more: bool,
    /// Per-statement messages, shown in the Log view.
    pub(super) log: Vec<SharedString>,
    /// The last `EXPLAIN` this tab ran, drawn as a flame graph in the Plan
    /// view. Kept per tab like the rows and the log, so switching tabs brings
    /// each one's own plan back.
    pub(super) plan: Option<plan::Plan>,
    /// The plan frame the flame graph is zoomed to. The chart derives the
    /// ancestor rows, the zoomed domain and the way back out from this one
    /// path, so it is the whole zoom state.
    pub(super) plan_focus: Option<FlamePath>,
}

impl TabResult {
    /// The result set the table is showing, if the last Run produced any.
    pub(super) fn selected(&self) -> Option<&db::ResultSet> {
        self.results.get(self.selected)
    }
}

/// The query output parked with an inactive connection: what its tabs had
/// last produced, put back when that connection is selected again.
///
/// It is deliberately runtime-only. The parked *scripts* are persisted (in
/// `config.parked_tabs`, the only copy of an untitled tab's text), but rows,
/// logs and plans are output, not user input, and are not worth carrying
/// across a restart.
#[derive(Default)]
pub(super) struct ParkedResults {
    /// Output of the parked untitled tabs, positionally aligned with that
    /// connection's `config::ParkedTab` list.
    pub(super) untitled: Vec<TabResult>,
    /// Output of the tabs that stayed open across the switch, keyed by file.
    pub(super) files: HashMap<PathBuf, TabResult>,
}

impl PgGuiApp {
    /// Build the editor tabs restored from the config at launch, handing
    /// each a fresh id. Returns the tabs and the next free id.
    pub(super) fn build_initial_tabs(
        config: &config::Config,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Vec<EditorTab>, u64) {
        let mut next_id: u64 = 0;
        let tabs = config
            .tabs
            .iter()
            .map(|tab| {
                let id = next_id;
                next_id += 1;
                Self::build_tab(
                    tab,
                    Self::launch_baseline(tab),
                    id,
                    config.autocommit,
                    window,
                    cx,
                )
            })
            .collect();
        (tabs, next_id)
    }

    /// Create the editor for one tab and wire it into the change plumbing.
    /// `saved` is the on-disk baseline used for the unsaved-edits marker.
    /// The caller hooks up the language server, if connected.
    pub(super) fn build_tab(
        tab: &config::ScriptTab,
        saved: String,
        id: u64,
        autocommit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EditorTab {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                // A routine definition is numbered the way Postgres numbers it
                // in error messages, i.e. relative to the body.
                .line_number_offset(statement::body_line_offset(&tab.script))
                .breakpoints_enabled(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    ..Default::default()
                })
                .placeholder("-- Write PostgreSQL here, then press cmd-enter to run")
                .default_value(tab.script.clone())
        });
        // Snippet suggestions work from the start; the language server's
        // richer provider replaces this once (and whenever) it connects.
        //
        // Go to Definition is the app's own catalog lookup, so it is wired
        // here rather than with the language server's providers: cmd-hover
        // resolves the symbol, and the `show_document` hook takes the jump
        // away from the editor (which could only move the cursor inside this
        // buffer) so the object opens in its file or a definition tab.
        let weak = cx.weak_entity();
        editor.update(cx, |state, _| {
            state.lsp_mut().completion_provider = Some(Rc::new(lsp::SnippetCompletions));
            // The PL/pgSQL keywords and `$n` parameters the SQL grammar leaves
            // uncoloured, painted over the tree-sitter result.
            state.lsp_mut().semantic_tokens_provider = Some(Rc::new(highlight::Tokens));
            state.lsp_mut().definition_provider =
                Some(Rc::new(definitions::Provider::new(weak.clone())));
            state.lsp_mut().show_document = Some(Rc::new(move |params, window, cx| {
                weak.update(cx, |this, cx| {
                    this.show_definition_document(params, window, cx)
                })
                .unwrap_or(false)
            }));
        });
        let subscription = cx.subscribe_in(&editor, window, Self::on_editor_event);
        let disk_time = tab.file.as_deref().and_then(file_mtime);
        // Only a tab backed by a file can be dirty: an untitled tab has no
        // file to leave stale, and its text lives in config.json, so a quit
        // costs nothing. Closing it does drop that text, which is what
        // [`Self::close_loses_work`] catches instead.
        let dirty = tab.file.is_some() && tab.script != saved;
        // If the file changed while the app was closed (its mtime differs
        // from the one we persisted last session) and this tab still has
        // unsaved edits, a save would clobber that external change — start
        // it diverged so the save path prompts. A never-synced tab
        // (`tab.disk_time` is `None`) has nothing to compare, so it can't.
        let diverged = dirty && tab.disk_time.is_some() && disk_time != tab.disk_time;
        EditorTab {
            editor,
            disk_time,
            path: tab.file.clone(),
            dirty,
            saved,
            diverged,
            snippet_mode: false,
            suggested_name: None,
            _subscription: subscription,
            id,
            session: None,
            autocommit,
            running: false,
            progress: None,
            cancel: None,
            result: TabResult::default(),
        }
    }

    /// The on-disk baseline for a restored tab: the file's current content
    /// (so edits persisted since the last save show as unsaved), or empty
    /// for a tab that was never saved to a file.
    pub(super) fn launch_baseline(tab: &config::ScriptTab) -> String {
        match &tab.file {
            Some(path) => std::fs::read_to_string(path).unwrap_or_else(|_| tab.script.clone()),
            None => String::new(),
        }
    }

    /// Recompute a tab's unsaved-edits marker and repaint if it flipped.
    /// An untitled tab is never dirty — see [`Self::build_tab`].
    pub(super) fn refresh_dirty(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let value = self.tabs[ix].editor.read(cx).value().to_string();
        let dirty = self.tabs[ix].path.is_some() && value != self.tabs[ix].saved;
        if self.tabs[ix].dirty != dirty {
            self.tabs[ix].dirty = dirty;
            cx.notify();
        }
    }

    /// The active tab's editor.
    pub(super) fn editor(&self) -> Entity<EditorState> {
        self.tabs[self.active_tab].editor.clone()
    }

    /// Index of the tab with `id`, if it still exists (it may have been
    /// closed while a query ran).
    pub(super) fn tab_index_by_id(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Set a tab's in-flight flag by id (no-op if the tab was closed).
    /// Clearing it also stops that tab's clock; starting it is left to
    /// [`Self::start_progress_by_id`], which needs to say what is running.
    pub(super) fn set_tab_running(&mut self, tab_id: u64, running: bool) {
        if let Some(ix) = self.tab_index_by_id(tab_id) {
            self.tabs[ix].running = running;
            if !running {
                self.tabs[ix].progress = None;
            }
        }
    }

    /// Mark a tab busy with `scope` and start its clock, so the status bar
    /// shows a live indicator for as long as the work is in flight.
    pub(super) fn start_progress(
        &mut self,
        ix: usize,
        scope: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.tabs[ix].running = true;
        self.tabs[ix].progress = Some(RunProgress::new(scope));
        self.start_progress_tick(cx);
    }

    /// The same by tab id, for work that starts after an `await` (the export
    /// path, which only reaches this point once the user has picked a file).
    pub(super) fn start_progress_by_id(
        &mut self,
        tab_id: u64,
        scope: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tab_index_by_id(tab_id) {
            self.start_progress(ix, scope, cx);
        }
    }

    /// Stop a tab's clock, leaving `running` to its own owner.
    pub(super) fn clear_progress(&mut self, ix: usize) {
        self.tabs[ix].progress = None;
    }

    /// Whether any tab still has work in flight; the tick loop runs only
    /// while this holds.
    pub(super) fn any_progress(&self) -> bool {
        self.tabs.iter().any(|tab| tab.progress.is_some())
    }

    /// Repaint every [`PROGRESS_TICK`] while any tab has work in flight, so
    /// the status bar's clock advances on its own. The spinner animates
    /// itself; only the number needs this. One loop serves every tab and
    /// exits as soon as the last run settles.
    pub(super) fn start_progress_tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_TICK).await;
                let alive = this.update(cx, |this, cx| {
                    if this.any_progress() {
                        cx.notify();
                        true
                    } else {
                        this.ticking = false;
                        false
                    }
                });
                // Stop on a dropped window as well as on the last run ending.
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Whether the active tab has a query in flight.
    pub(super) fn active_running(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.running)
    }

    /// Whether the active tab's last SELECT left a cursor open (Fetch More).
    pub(super) fn active_has_more(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.result.has_more)
    }

    /// Whether the active tab runs each statement autocommitted.
    pub(super) fn active_autocommit(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_none_or(|tab| tab.autocommit)
    }

    /// Whether the active tab's session has an open (autocommit-off) transaction.
    pub(super) fn active_in_txn(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.session.as_ref().is_some_and(db::Session::in_txn))
    }

    /// Mirror the tab's stored result and log into the shared results table
    /// and Log view.
    pub(super) fn show_tab_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        let (columns, rows) = tab.result.selected().map_or_else(
            || (Vec::new(), db::Rows::new()),
            |set| (set.columns.clone(), set.rows.clone()),
        );
        // A run appends to the log a line at a time; the scroll offset stays
        // where it was, so without this the new lines pile up below the
        // viewport and a long script's log stops being live. Follow the
        // bottom only when the view is already there, so a user who scrolled
        // up to read an earlier statement is not yanked back down by the
        // next one.
        let follow = tab.result.log.len() != self.log.len() && self.log_at_bottom();
        self.log = tab.result.log.clone();
        if follow {
            self.log_scroll.scroll_to_bottom();
        }
        self.results.update(cx, |table, cx| {
            table.delegate_mut().set_data(columns, rows, cx);
            table.refresh(cx);
        });
    }

    /// Append a tab (not yet selected) and its config mirror.
    pub(super) fn add_tab(
        &mut self,
        script: String,
        file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        self.insert_tab_at(
            self.tabs.len(),
            config::ScriptTab {
                script,
                file,
                disk_time: None,
            },
            window,
            cx,
        )
    }

    /// Insert a tab (not yet selected) at `ix`, clamped to the end, keeping
    /// the config mirror aligned with it. Returns the slot used.
    pub(super) fn insert_tab_at(
        &mut self,
        ix: usize,
        tab_config: config::ScriptTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let ix = ix.min(self.tabs.len());
        // A freshly inserted tab starts clean: its content is the baseline
        // (empty for a new script, the file's text for an opened one).
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let tab = Self::build_tab(
            &tab_config,
            tab_config.script.clone(),
            id,
            self.config.autocommit,
            window,
            cx,
        );
        if let Some(client) = &self.lsp {
            Self::attach_lsp_providers(client, &tab.editor, cx);
        }
        self.tabs.insert(ix, tab);
        self.config.tabs.insert(ix, tab_config);
        ix
    }

    /// Select a tab: focus its editor, retitle the window, and point the
    /// language server at its content.
    pub(super) fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.config.active_tab = ix;
        // Swap the shared results/log view over to this tab's own output.
        self.show_tab_result(ix, cx);
        let editor = self.editor();
        editor.update(cx, |state, cx| {
            // Any diagnostics in this buffer are from when it was last
            // active; clear them until the server re-checks it.
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
            }
            state.focus(window, cx);
        });
        if let Some(client) = &self.lsp {
            client.document_changed(editor.read(cx).value().to_string());
        }
        self.update_window_title(window);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Close a tab; the last one is replaced with a fresh empty script.
    pub(super) fn close_tab_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        self.config.tabs.remove(ix);
        if self.tabs.is_empty() {
            self.add_tab(String::new(), None, window, cx);
        }
        let active = if ix < self.active_tab {
            self.active_tab - 1
        } else {
            self.active_tab.min(self.tabs.len() - 1)
        };
        self.activate_tab(active, window, cx);
        self.save_config();
    }

    pub fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.request_close_tab(self.active_tab, window, cx);
    }

    /// Close a tab, but prompt first when it has an open transaction (which
    /// closing would roll back) or unsaved edits.
    pub(super) fn request_close_tab(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.tabs.len() {
            return;
        }
        if self.tabs[ix]
            .session
            .as_ref()
            .is_some_and(db::Session::in_txn)
        {
            self.prompt_txn_before_close(ix, window, cx);
        } else {
            self.after_txn_close(ix, window, cx);
        }
    }

    /// Continue closing a tab once any open transaction has been dealt with:
    /// prompt for work the close would lose, otherwise close.
    pub(super) fn after_txn_close(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_loses_work(ix) {
            self.prompt_save_before_close(ix, window, cx);
        } else {
            self.close_tab_at(ix, window, cx);
        }
    }

    /// Whether closing this tab drops work the user would want back: unsaved
    /// edits to its file, or an untitled tab's script, which has no file to
    /// fall back on and leaves the config the moment the tab does.
    ///
    /// Deliberately wider than `dirty`, which is about a file left stale on
    /// disk. Quitting is the other way round: an untitled tab's text survives
    /// a quit (config.json holds it, parked or not), so the quit guard stays
    /// on `dirty` alone.
    pub(super) fn close_loses_work(&self, ix: usize) -> bool {
        let Some(tab) = self.tabs.get(ix) else {
            return false;
        };
        tab.dirty
            || (tab.path.is_none()
                && self
                    .config
                    .tabs
                    .get(ix)
                    .is_some_and(|cfg| !cfg.script.trim().is_empty()))
    }

    /// Warn before closing a tab whose session has an open transaction —
    /// closing drops the connection, which rolls the transaction back.
    /// Committing on close is intentionally not offered; commit explicitly
    /// first if the work should be kept.
    pub(super) fn prompt_txn_before_close(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let name = self.tab_label(ix);
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let close = app.clone();
            dialog.title("Open transaction").w(px(420.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(div().text_sm().child(format!(
                        "“{name}” has an open transaction that will be rolled back."
                    )))
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
                                Button::new("close")
                                    .danger()
                                    .label("Roll Back & Close")
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        close
                                            .update(cx, |this, cx| {
                                                this.after_txn_close(ix, window, cx);
                                            })
                                            .ok();
                                    }),
                            ),
                    ),
            )
        });
    }

    /// Ask whether to save a tab before closing it — its unsaved edits, or,
    /// for an untitled tab, the script itself.
    pub(super) fn prompt_save_before_close(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let name = self.tab_label(ix);
        let message = if self.tabs[ix].dirty {
            format!("“{name}” has unsaved changes.")
        } else {
            format!("“{name}” was never saved to a file; closing it discards the script.")
        };
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (save, discard) = (app.clone(), app.clone());
            dialog.title("Unsaved changes").w(px(420.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(div().text_sm().child(message.clone()))
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
                                Button::new("discard")
                                    .danger()
                                    .label("Don't Save")
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        discard
                                            .update(cx, |this, cx| {
                                                this.close_tab_at(ix, window, cx);
                                            })
                                            .ok();
                                    }),
                            )
                            .child(Button::new("save").primary().label("Save").on_click(
                                move |_, window, cx| {
                                    window.close_dialog(cx);
                                    save.update(cx, |this, cx| {
                                        this.save_tab_then_close(ix, window, cx);
                                    })
                                    .ok();
                                },
                            )),
                    ),
            )
        });
    }

    /// Quit, but confirm first when any tab has unsaved edits. cmd-q and
    /// the window's close button both funnel here.
    pub fn request_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_unsaved_tabs() {
            self.prompt_quit(window, cx);
        } else {
            cx.quit();
        }
    }

    pub(super) fn has_unsaved_tabs(&self) -> bool {
        self.tabs.iter().any(|tab| tab.dirty)
    }

    /// Ask for confirmation before quitting with unsaved edits. The
    /// buffers themselves survive a quit (they're restored from
    /// config.json on the next launch); it's the tabs' files on disk that
    /// would be left stale.
    pub(super) fn prompt_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let dirty: Vec<String> = (0..self.tabs.len())
            .filter(|&ix| self.tabs[ix].dirty)
            .map(|ix| self.tab_label(ix))
            .collect();
        let message = if let [name] = dirty.as_slice() {
            format!("“{name}” has unsaved changes. Quit anyway?")
        } else {
            format!("{} tabs have unsaved changes. Quit anyway?", dirty.len())
        };
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("Unsaved changes").w(px(420.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(div().text_sm().child(message.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new("cancel").label("Cancel").on_click(
                                |_, window, cx| {
                                    window.close_dialog(cx);
                                },
                            ))
                            .child(Button::new("quit").danger().label("Quit").on_click(
                                |_, window, cx| {
                                    window.close_dialog(cx);
                                    cx.quit();
                                },
                            )),
                    ),
            )
        });
    }

    /// Save a tab (prompting for a path if it has no file) and close it
    /// once the write succeeds.
    pub(super) fn save_tab_then_close(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_tab(ix, window, cx);
        self.save_active(true, window, cx);
    }

    pub fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab((self.active_tab + 1) % self.tabs.len(), window, cx);
    }

    pub fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        self.activate_tab((self.active_tab + count - 1) % count, window, cx);
    }

    /// Whether a tab holds an untouched fresh script, safe to reuse for an
    /// opened file.
    pub(super) fn tab_is_pristine(&self, ix: usize, cx: &Context<Self>) -> bool {
        self.tabs[ix].path.is_none() && self.tabs[ix].editor.read(cx).value().is_empty()
    }

    /// Adopt `content` as a tab's on-disk baseline, clearing its
    /// unsaved-edits and diverged markers and remembering the file's new
    /// mtime so the watcher doesn't read our own write as an external edit.
    pub(super) fn mark_saved(&mut self, ix: usize, content: String, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs[ix].saved = content;
        self.tabs[ix].disk_time = self.tabs[ix].path.as_deref().and_then(file_mtime);
        self.tabs[ix].diverged = false;
        self.refresh_dirty(ix, cx);
    }

    /// Remember where a tab's script lives on disk and refresh the window
    /// title. Callers persist the config afterwards.
    pub(super) fn set_tab_path(&mut self, ix: usize, path: PathBuf, window: &mut Window) {
        // The tab may have been closed while a save dialog was open.
        if ix >= self.tabs.len() {
            return;
        }
        self.config.tabs[ix].file = Some(path.clone());
        self.tabs[ix].disk_time = file_mtime(&path);
        self.tabs[ix].path = Some(path);
        self.update_window_title(window);
    }

    /// Show the active tab's file path in the window title.
    pub(super) fn update_window_title(&self, window: &mut Window) {
        match &self.tabs[self.active_tab].path {
            Some(path) => window.set_window_title(&format!("pg-gui — {}", path.display())),
            None => window.set_window_title("pg-gui"),
        }
    }

    /// A tab's display name: its file name, or "untitled".
    pub(super) fn tab_label(&self, ix: usize) -> String {
        self.tabs[ix]
            .path
            .as_deref()
            .and_then(|path| path.file_name())
            .map_or_else(
                || "untitled".to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
    }
}
