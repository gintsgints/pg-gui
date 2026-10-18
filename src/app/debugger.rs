//! PL/pgSQL step debugging: starting a session from the editor, its event
//! stream, the step/continue/stop commands, breakpoints and value deposits.

use std::rc::Rc;

use futures::StreamExt as _;
use gpui::{
    App, AppContext as _, Context, Entity, ParentElement as _, SharedString, Styled as _, Window,
    div, px,
};
use gpui_component::{
    ActiveTheme as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{EditorState, Input, InputState, RopeExt as _},
    v_flex,
};

use crate::app::{DebugState, PgGuiApp, strip_sql_comments};
use crate::{DebugContinue, DebugStepInto, DebugStepOver, DebugStop, StartDebug, debug, statement};

/// Best-effort routine name from a `CREATE FUNCTION` / `CREATE PROCEDURE`
/// statement, to prefill the debug launch dialog. Returns the (possibly
/// schema-qualified) name up to its argument list; `None` when the statement is
/// not a routine definition.
///
/// Comments are stripped first, and the `function`/`procedure` keyword only
/// counts when the preceding word is `create`/`replace` — so prose like
/// `-- function to add two numbers` does not masquerade as a definition.
fn guess_signature(sql: &str) -> Option<String> {
    let stripped = strip_sql_comments(sql);
    let words: Vec<&str> = stripped.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        let keyword = word.to_ascii_lowercase();
        if (keyword != "function" && keyword != "procedure") || i == 0 {
            continue;
        }
        if !matches!(
            words[i - 1].to_ascii_lowercase().as_str(),
            "create" | "replace"
        ) {
            continue;
        }
        let Some(name) = words.get(i + 1) else {
            continue;
        };
        let end = name.find('(').unwrap_or(name.len());
        let name = name[..end].trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

impl PgGuiApp {
    /// Start Debug (cmd-shift-d): guess the routine at the cursor and open the
    /// launch dialog.
    pub(in crate::app) fn start_debug(
        &mut self,
        _: &StartDebug,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.debug.as_ref().is_some_and(|d| !d.terminated) {
            self.set_status("A debug session is already running", cx);
            return;
        }
        let guess = self.editor().read(cx).value().to_string();
        let cursor = self.editor().read(cx).cursor();
        let signature = statement::at(&guess, cursor)
            .and_then(|range| guess_signature(&guess[range]))
            .unwrap_or_default();
        Self::open_debug_dialog(signature, window, cx);
    }

    /// The launch dialog: routine signature, an argument list spliced into the
    /// call, and a stop-on-entry toggle.
    fn open_debug_dialog(signature: String, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let sig = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("schema.routine or routine")
                .default_value(signature)
        });
        let args = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. 1, 99.50"));
        // Deferred for the same reason as the rename dialog: `open_dialog`
        // focuses the dialog itself, so focusing an input before it opens is
        // immediately undone.
        cx.defer_in(window, {
            let sig = sig.clone();
            move |_, window, cx| {
                sig.update(cx, |state, cx| state.focus(window, cx));
            }
        });
        let stop_on_entry = cx.new(|_| true);
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (sig, args, stop_on_entry) = (sig.clone(), args.clone(), stop_on_entry.clone());
            // Shared by the Debug button and the dialog's Enter binding: reads
            // the inputs and launches, reporting whether the dialog may close.
            let start: Rc<dyn Fn(&mut App) -> bool> = {
                let (app, sig, args, stop_on_entry) = (
                    app.clone(),
                    sig.clone(),
                    args.clone(),
                    stop_on_entry.clone(),
                );
                Rc::new(move |cx: &mut App| {
                    let signature = sig.read(cx).value().trim().to_string();
                    if signature.is_empty() {
                        return false;
                    }
                    let args = args.read(cx).value().trim().to_string();
                    let stop = *stop_on_entry.read(cx);
                    app.update(cx, |this, cx| this.launch_debug(signature, args, stop, cx))
                        .ok();
                    true
                })
            };
            let labeled = |label: &str, input: &Entity<InputState>, cx: &mut App| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(label.to_string()),
                    )
                    .child(Input::new(input))
            };
            let checked = *stop_on_entry.read(cx);
            let toggle = stop_on_entry.clone();
            let confirm = start.clone();
            dialog
                .title("Debug routine")
                .w(px(480.))
                // Enter starts the session, Escape closes (the dialog's own
                // `escape` binding); both bubble up out of the inputs.
                .on_ok(move |_, _, cx| confirm(cx))
                .child(
                    v_flex()
                        .gap_4()
                        .pb_2()
                        .child(labeled("Routine", &sig, cx))
                        .child(labeled("Arguments", &args, cx))
                        .child(
                            Checkbox::new("stop-on-entry")
                                .label("Stop on entry")
                                .checked(checked)
                                .on_click(move |checked, _, cx| {
                                    toggle.update(cx, |state, cx| {
                                        *state = *checked;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(Button::new("cancel").label("Cancel").on_click(
                                    |_, window, cx| {
                                        window.close_dialog(cx);
                                    },
                                ))
                                .child(Button::new("start").primary().label("Debug").on_click(
                                    move |_, window, cx| {
                                        if start(cx) {
                                            window.close_dialog(cx);
                                        }
                                    },
                                )),
                        ),
                )
        });
    }

    /// Kick off the session on a background thread and attach on success.
    fn launch_debug(
        &mut self,
        signature: String,
        args: String,
        stop_on_entry: bool,
        cx: &mut Context<Self>,
    ) {
        let conn = self.config.connection_string.clone();
        let tab_id = self.tabs[self.active_tab].id;
        // Breakpoints set in the editor gutter (0-based buffer lines) plus the
        // buffer itself, so the session can map them to pldbg body lines.
        let (editor_text, breakpoints) = {
            let editor = self.editor().read(cx);
            let mut lines: Vec<usize> = editor.breakpoints().iter().copied().collect();
            lines.sort_unstable();
            (editor.value().to_string(), lines)
        };
        self.set_status(format!("Starting debug: {signature}…"), cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    debug::Session::start(
                        &conn,
                        &signature,
                        &args,
                        &breakpoints,
                        &editor_text,
                        stop_on_entry,
                    )
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok((session, events)) => this.attach_debug(session, tab_id, events, cx),
                Err(err) => this.set_status(format!("Debug failed to start: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Store the session and pump its events into the panel until it ends.
    fn attach_debug(
        &mut self,
        session: debug::Session,
        tab_id: u64,
        mut events: debug::DebugEventReceiver,
        cx: &mut Context<Self>,
    ) {
        self.debug = Some(DebugState {
            session,
            tab_id,
            stop: None,
            output: None,
            status: "Starting…".to_string(),
            terminated: false,
            busy: true,
        });
        self.set_status("Debug session started", cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                match this.update(cx, |this, cx| this.on_debug_event(event, cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Apply one debug event. Returns `false` once the session is over so the
    /// event pump stops.
    fn on_debug_event(&mut self, event: debug::DebugEvent, cx: &mut Context<Self>) -> bool {
        let Some(dbg) = self.debug.as_mut() else {
            return false;
        };
        let mut status = None;
        let mut keep = true;
        let mut stopped = false;
        match event {
            debug::DebugEvent::Status(note) => dbg.status = note,
            debug::DebugEvent::Stopped(stop) => {
                dbg.stop = Some(stop);
                dbg.busy = false;
                stopped = true;
            }
            debug::DebugEvent::Output(output) => dbg.output = Some(output),
            // The debug panel replaces the results table, so a notice raised
            // mid-step goes to the launching tab's message log, where the
            // Run path's notices land too.
            debug::DebugEvent::Notice(line) => {
                let tab_id = dbg.tab_id;
                if let Some(ix) = self.tab_index_by_id(tab_id) {
                    self.tabs[ix].result.log.push(SharedString::from(line));
                }
            }
            debug::DebugEvent::Error(err) => {
                dbg.output = Some(err.clone());
                dbg.busy = false;
                status = Some(format!("Debug error: {err}"));
            }
            debug::DebugEvent::Note(note) => status = Some(note),
            debug::DebugEvent::Terminated => {
                dbg.terminated = true;
                dbg.busy = false;
                status = Some("Debug finished".to_string());
                keep = false;
            }
        }
        if stopped {
            self.select_debug_line(cx);
        }
        if let Some(status) = status {
            self.set_status(status, cx);
        }
        cx.notify();
        keep
    }

    /// Select the line the debugger is parked on in the editor the session was
    /// launched from, so stepping walks the cursor through the source and
    /// scrolls it into view alongside the debug panel.
    ///
    /// Silently does nothing when the stop is not locatable in that buffer —
    /// a routine stepped into from elsewhere, or a buffer edited since the
    /// routine was installed.
    fn select_debug_line(&mut self, cx: &mut Context<Self>) {
        let Some(dbg) = self.debug.as_ref() else {
            return;
        };
        let Some(stop) = dbg.stop.as_ref() else {
            return;
        };
        let line = stop.line;
        let source = stop.source.clone();
        let Some(ix) = self.tab_index_by_id(dbg.tab_id) else {
            return;
        };
        self.tabs[ix].editor.clone().update(cx, |state, cx| {
            let Some(row) = debug::editor_line_of(&state.value(), &source, line) else {
                return;
            };
            let text = state.text();
            if row >= text.lines_len() {
                return;
            }
            let range = text.line_start_offset(row)..text.line_end_offset(row);
            state.set_selected_range(range, cx);
        });
    }

    pub(in crate::app) fn debug_step_over(
        &mut self,
        _: &DebugStepOver,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dbg) = self.debug.as_mut().filter(|d| !d.terminated) {
            dbg.session.step_over();
            dbg.busy = true;
            cx.notify();
        }
    }

    pub(in crate::app) fn debug_step_into(
        &mut self,
        _: &DebugStepInto,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dbg) = self.debug.as_mut().filter(|d| !d.terminated) {
            dbg.session.step_into();
            dbg.busy = true;
            cx.notify();
        }
    }

    /// F5: continue a live session, or start one when idle.
    pub(in crate::app) fn debug_continue(
        &mut self,
        _: &DebugContinue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.debug.as_mut() {
            Some(dbg) if !dbg.terminated => {
                dbg.session.continue_();
                dbg.busy = true;
                cx.notify();
            }
            _ => self.start_debug(&StartDebug, window, cx),
        }
    }

    pub(in crate::app) fn debug_stop(
        &mut self,
        _: &DebugStop,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dbg) = self.debug.take() {
            dbg.session.stop();
        }
        self.set_status("Debug stopped", cx);
        cx.notify();
    }

    /// Mirror an editor gutter toggle onto a live session, so breakpoints can
    /// be added and removed mid-run and not only at launch.
    ///
    /// The row is sent as-is: the controller owns the row → `pldbg` body-line
    /// mapping, because only it knows the entry routine's installed source —
    /// the frame the panel happens to be inspecting is not necessarily the
    /// routine a breakpoint would be armed on.
    pub(in crate::app) fn sync_debug_breakpoint(
        &self,
        editor: &Entity<EditorState>,
        row: usize,
        set: bool,
    ) {
        let Some(dbg) = self.debug.as_ref().filter(|dbg| !dbg.terminated) else {
            return;
        };
        let Some(ix) = self.tab_index_by_id(dbg.tab_id) else {
            return;
        };
        if self.tabs[ix].editor != *editor {
            return;
        }
        if set {
            dbg.session.set_breakpoint(row);
        } else {
            dbg.session.drop_breakpoint(row);
        }
    }

    /// Edit a variable's value mid-execution (`pldbg_deposit_value`).
    pub(in crate::app) fn open_deposit_dialog(
        name: String,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let value = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (value, name, app) = (value.clone(), name.clone(), app.clone());
            let submit = {
                let (value, name, app) = (value.clone(), name.clone(), app.clone());
                move |window: &mut Window, cx: &mut App| {
                    let new_value = value.read(cx).value().to_string();
                    window.close_dialog(cx);
                    app.update(cx, |this, _| {
                        if let Some(dbg) = this.debug.as_ref() {
                            dbg.session.deposit(name.clone(), new_value.clone());
                        }
                    })
                    .ok();
                }
            };
            dialog.title(format!("Set {name}")).w(px(360.)).child(
                v_flex().gap_4().pb_2().child(Input::new(&value)).child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("set")
                                .primary()
                                .label("Set")
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                ),
            )
        });
    }
}
