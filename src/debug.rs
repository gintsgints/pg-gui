//! In-app PL/pgSQL step debugger.
//!
//! Drives `EnterpriseDB`'s pldebugger (the `pldbg_*` proxy API) through
//! [`pgdap::debugger::DebugSession`], embedded as a library rather than spawned
//! as a DAP server. The shape mirrors [`crate::lsp::Client`]: a cheap handle
//! over background threads, commands pushed in over a channel, async events
//! pulled out over a [`futures`] receiver the UI awaits.
//!
//! pldebugger needs **two** connections that both block for seconds (or
//! indefinitely): a persistent *controller* that arms breakpoints and steps,
//! and a *target* that actually runs the debugged routine and stays trapped in
//! the backend until the controller lets it continue. Neither may touch the UI
//! thread, so [`Session::start`] spins up a controller thread which in turn
//! spawns the target thread once the entry breakpoint is armed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc as async_mpsc;
use pgdap::debugger::{DebugSession, cancel_backend};
use postgres::{Config, NoTls, SimpleQueryMessage};
use tracing::debug;

pub use pgdap::debugger::{Frame, Var};

/// Async events pushed from the controller/target threads to the UI.
pub type DebugEventReceiver = async_mpsc::UnboundedReceiver<DebugEvent>;

/// What the UI learns as the session runs.
pub enum DebugEvent {
    /// A progress note for the panel while no stop has happened yet
    /// (connecting, resolving, waiting for the target to trap, …).
    Status(String),
    /// Execution is parked at `line` inside `func_oid`; carries everything the
    /// panel renders at a stop.
    Stopped(StopState),
    /// The target routine returned; carries a best-effort render of its result.
    Output(String),
    /// The debugged routine raised a server notice (`RAISE NOTICE`/`WARNING`,
    /// …) while running. Arrives live from the target connection, so a
    /// routine stepped through reports each notice as it is raised.
    Notice(String),
    /// A one-line remark for the status bar that changes nothing about the
    /// session — e.g. a gutter breakpoint that could not be armed.
    Note(String),
    /// The debugged execution finished (or was aborted); the session is over.
    Terminated,
    /// The session could not start, or a control op failed hard.
    Error(String),
}

/// A snapshot of the paused target: the source being stepped, the current
/// line, the call stack, and the variables of the active frame.
///
/// Everything but `stack` describes the *selected* frame, which is the
/// innermost one at every stop but follows [`Session::select_frame`] after
/// that — so inspecting a caller shows that caller's source, line and
/// variables, not the innermost frame's.
pub struct StopState {
    pub line: i32,
    pub source: String,
    /// Level of the frame `line`/`source`/`variables` belong to.
    pub frame: i32,
    pub stack: Vec<Frame>,
    pub variables: Vec<Var>,
}

/// A control action sent from the UI to the controller thread.
enum Command {
    StepOver,
    StepInto,
    Continue,
    /// 0-based editor row; mapped to a body line by the controller, which is
    /// the side that knows the entry routine's installed source.
    SetBreakpoint(usize),
    DropBreakpoint(usize),
    SelectFrame(i32),
    Deposit {
        name: String,
        value: String,
    },
    Stop,
}

/// Handle to a running debug session. Dropping it (or calling [`Self::stop`])
/// aborts the trapped target and tears the threads down.
pub struct Session {
    cmd_tx: Sender<Command>,
    /// Set before the controller aborts the target, so the target thread knows
    /// its resulting query error is expected and not worth surfacing.
    aborting: Arc<AtomicBool>,
    /// Set once the target has trapped. Before that the controller is parked
    /// in `pldbg_wait_for_target` and never reads a command, so a stop has to
    /// reach it from outside — see [`Self::request_stop`].
    attached: Arc<AtomicBool>,
    /// Where to send that out-of-band cancel, and the `application_name` tag
    /// the session's connections carry.
    config: Config,
    control_tag: String,
    /// The fully-qualified routine being debugged, for the panel header.
    pub target: String,
}

impl Session {
    /// Connect a controller, arm the entry breakpoint, launch the target, and
    /// stop (at entry, or at the first breakpoint if `stop_on_entry` is false).
    ///
    /// `args` is the raw argument list spliced into the call verbatim (e.g.
    /// `1, 99.50` or `p_id => 1`), so overloaded/`DEFAULT`/named arguments all
    /// work the way they would in a hand-written call.
    ///
    /// # Errors
    ///
    /// Fails when the connection string does not parse. Connection and
    /// pldebugger failures after that surface as [`DebugEvent::Error`] on the
    /// returned receiver instead.
    pub fn start(
        conn_str: &str,
        signature: &str,
        args: &str,
        breakpoints: &[usize],
        editor_text: &str,
        stop_on_entry: bool,
    ) -> Result<(Self, DebugEventReceiver)> {
        let mut config = conn_str
            .parse::<Config>()
            .with_context(|| "parsing connection string")?;
        // Only bounds the initial connect; wait_for_target/step block on the
        // server regardless and must be allowed to.
        config.connect_timeout(std::time::Duration::from_secs(4));

        let (event_tx, event_rx) = async_mpsc::unbounded();
        let (cmd_tx, cmd_rx) = channel();
        let aborting = Arc::new(AtomicBool::new(false));
        let attached = Arc::new(AtomicBool::new(false));
        // The connections are tagged so a run that never traps, or a stop that
        // arrives before it does, can find and cancel them.
        let control_tag = control_tag();

        let launch = Launch {
            config: config.clone(),
            control_tag: control_tag.clone(),
            attached: attached.clone(),
            signature: signature.to_string(),
            args: args.to_string(),
            breakpoints: breakpoints.to_vec(),
            editor_text: editor_text.to_string(),
            stop_on_entry,
            cmd_rx,
            event_tx: event_tx.clone(),
            aborting: aborting.clone(),
        };

        std::thread::Builder::new()
            .name("pg-debug-controller".into())
            .spawn(move || {
                let event_tx = launch.event_tx.clone();
                let aborting = launch.aborting.clone();
                if let Err(err) = controller(launch) {
                    // After a stop, cancelled queries are how the threads are
                    // brought down; the errors they raise are expected.
                    if !aborting.load(Ordering::SeqCst) {
                        let _ = event_tx.unbounded_send(DebugEvent::Error(format!("{err:#}")));
                    }
                    let _ = event_tx.unbounded_send(DebugEvent::Terminated);
                }
            })?;

        Ok((
            Self {
                cmd_tx,
                aborting,
                attached,
                config,
                control_tag,
                target: signature.to_string(),
            },
            event_rx,
        ))
    }

    pub fn step_over(&self) {
        let _ = self.cmd_tx.send(Command::StepOver);
    }

    pub fn step_into(&self) {
        let _ = self.cmd_tx.send(Command::StepInto);
    }

    pub fn continue_(&self) {
        let _ = self.cmd_tx.send(Command::Continue);
    }

    /// Arm a breakpoint on a 0-based editor row of the debugged buffer.
    pub fn set_breakpoint(&self, row: usize) {
        let _ = self.cmd_tx.send(Command::SetBreakpoint(row));
    }

    /// Clear the breakpoint on a 0-based editor row of the debugged buffer.
    pub fn drop_breakpoint(&self, row: usize) {
        let _ = self.cmd_tx.send(Command::DropBreakpoint(row));
    }

    pub fn select_frame(&self, level: i32) {
        let _ = self.cmd_tx.send(Command::SelectFrame(level));
    }

    pub fn deposit(&self, name: String, value: String) {
        let _ = self.cmd_tx.send(Command::Deposit { name, value });
    }

    /// Abort the trapped target and end the session.
    pub fn stop(&self) {
        self.request_stop();
    }

    /// Ask the controller to abort, and cancel the tagged connections from
    /// outside too, because the controller only reads the queued `Stop` between
    /// two blocking calls. The target's query is always cancelled: while a
    /// Continue runs toward a breakpoint it never reaches, the controller sits
    /// in `pldbg_continue` until the routine ends. Before the trap the
    /// controller's own query is cancelled as well, since it is parked in
    /// `pldbg_wait_for_target`, which a routine stuck before its first
    /// statement (a lock wait, a slow argument) never ends. Idempotent: the
    /// first call flips `aborting`, later ones (a `stop` followed by the drop)
    /// do nothing.
    fn request_stop(&self) {
        if self.aborting.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.cmd_tx.send(Command::Stop);
        let mut tags = vec![target_tag(&self.control_tag)];
        if !self.attached.load(Ordering::SeqCst) {
            tags.push(self.control_tag.clone());
        }
        // Connecting may block for the connect timeout; never on the UI thread.
        let config = self.config.clone();
        let _ = std::thread::Builder::new()
            .name("pg-debug-cancel".into())
            .spawn(move || cancel_tagged(&config, &tags));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Best-effort: unblock the target if the handle is dropped without an
        // explicit stop. Dropping cmd_tx also makes the controller's recv fail.
        self.request_stop();
    }
}

/// Everything the controller thread needs, bundled so the spawn closure stays
/// small.
struct Launch {
    config: Config,
    /// `application_name` of the control connection; the target's is derived
    /// from it ([`target_tag`]).
    control_tag: String,
    attached: Arc<AtomicBool>,
    signature: String,
    args: String,
    /// Editor-relative 0-based line numbers, mapped to body lines after attach.
    breakpoints: Vec<usize>,
    /// The editor buffer, used to map editor lines to `pldbg` body lines.
    editor_text: String,
    stop_on_entry: bool,
    cmd_rx: Receiver<Command>,
    event_tx: async_mpsc::UnboundedSender<DebugEvent>,
    aborting: Arc<AtomicBool>,
}

/// The controller thread: owns the pldebugger control connection for the whole
/// session and serialises every `pldbg_*` call. Runs the arm → attach → stop →
/// command-loop state machine.
fn controller(launch: Launch) -> Result<()> {
    let Launch {
        config,
        control_tag,
        attached,
        signature,
        args,
        breakpoints,
        editor_text,
        stop_on_entry,
        cmd_rx,
        event_tx,
        aborting,
    } = launch;

    let status = |message: &str| {
        let _ = event_tx.unbounded_send(DebugEvent::Status(message.to_string()));
    };

    status(&format!("Connecting to {}…", describe_addr(&config)));
    // The control connection is tagged so a target that finishes without ever
    // trapping can find and cancel the controller's `pldbg_wait_for_target`.
    let mut control_config = config.clone();
    control_config.application_name(&control_tag);
    let mut session = DebugSession::connect(&control_config)
        .with_context(|| format!("connecting to {}", describe_addr(&config)))?;
    status(&format!("Resolving {signature}…"));
    let target = session.resolve_target(&signature)?;
    let oid = target.oid;

    // Refuse up front the two setups that can never trap, instead of arming a
    // breakpoint and waiting forever for a routine that runs straight through.
    preflight(&config, oid, &target.fq_name)?;

    // Arm the entry breakpoint before the target starts, so the very first
    // statement traps and hands control to us. A `false` result means
    // pldebugger refused to arm it — waiting would then hang forever.
    if !session.set_global_breakpoint(oid, -1, None)? {
        return Err(anyhow!(
            "pldebugger could not arm a breakpoint on {} — is it a PL/pgSQL routine \
             and is plugin_debugger loaded?",
            target.fq_name
        ));
    }

    // `attached` is set once the target traps, so the target thread can tell
    // "stepped" from "ran straight through"; `target_done` marks the routine as
    // finished, so a cancelled wait is recognised as expected rather than
    // reported as a fault.
    let target_done = Arc::new(AtomicBool::new(false));

    // A stop during setup found no target to cancel; launching one now would
    // leave it to trap with nobody left to release it.
    if aborting.load(Ordering::SeqCst) {
        finish(&event_tx);
        return Ok(());
    }

    // Now that the trap is set, launch the routine on its own connection.
    std::thread::Builder::new()
        .name("pg-debug-target".into())
        .spawn({
            let run = TargetRun {
                config: config.clone(),
                oid,
                fq_name: target.fq_name.clone(),
                args,
                control_tag,
                event_tx: event_tx.clone(),
                aborting: aborting.clone(),
                attached: attached.clone(),
                target_done: target_done.clone(),
            };
            move || run_target(&run)
        })?;

    status(&format!("Waiting for {} to be called…", target.fq_name));
    let pid = match session.wait_for_target() {
        Ok(pid) => pid,
        // A stop before the trap cancelled our wait (and the target with it).
        Err(_) if aborting.load(Ordering::SeqCst) => {
            finish(&event_tx);
            return Ok(());
        }
        // The routine finished without trapping and cancelled our wait; it has
        // already reported why, so end quietly.
        Err(_) if target_done.load(Ordering::SeqCst) => return Ok(()),
        Err(err) => return Err(err),
    };
    attached.store(true, Ordering::SeqCst);
    let teardown = Teardown {
        config: &config,
        pid,
        aborting: &aborting,
        target_done: &target_done,
    };
    // A stop that raced the trap: the handle saw `attached` still false and
    // cancelled from outside, but the wait had already returned. Abort the
    // trapped target properly instead of arming breakpoints and stepping on.
    if aborting.load(Ordering::SeqCst) {
        teardown.abort(&mut session);
        finish(&event_tx);
        return Ok(());
    }
    status(&format!("Target attached (pid {pid}); reading state…"));

    // Attached at entry: map the editor-relative breakpoint lines to body
    // lines and arm them. The body source (`pldbg_get_source`) appears verbatim
    // in the editor buffer, so its starting line locates the offset. Kept for
    // the whole session, so gutter toggles during the run map the same way.
    let entry = Entry {
        oid,
        fq_name: target.fq_name,
        editor_text,
        body: session.get_source(oid).unwrap_or_default(),
    };
    arm_launch_breakpoints(&mut session, &entry, &breakpoints, &event_tx);

    let stopped = if stop_on_entry {
        report_top(&mut session, &event_tx)
    } else {
        session.continue_()?.is_some() && report_top(&mut session, &event_tx)
    };
    if !stopped {
        finish(&event_tx);
        return Ok(());
    }

    command_loop(&mut session, &entry, &teardown, &cmd_rx, &event_tx)
}

/// Arm the breakpoints the gutter held at launch, mapped from editor rows to
/// body lines, and say so when only some — or none — of them took.
fn arm_launch_breakpoints(
    session: &mut DebugSession,
    entry: &Entry,
    breakpoints: &[usize],
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
) {
    if breakpoints.is_empty() {
        return;
    }
    let mut armed = 0usize;
    for &row in breakpoints {
        let Some(body_line) = body_line_of(&entry.editor_text, &entry.body, row) else {
            continue;
        };
        if session
            .set_breakpoint(entry.oid, body_line)
            .unwrap_or(false)
        {
            armed += 1;
        }
    }
    if armed == breakpoints.len() {
        return;
    }
    // Silently arming nothing is how a "Continue" ends up running the whole
    // routine, so say it out loud; arming some of them is worth a note.
    let event = if armed == 0 {
        DebugEvent::Error(format!(
            "none of the {} editor breakpoint(s) could be armed — the buffer does not \
             contain the installed body of {} verbatim, or those lines carry no \
             executable statement",
            breakpoints.len(),
            entry.fq_name
        ))
    } else {
        DebugEvent::Note(format!(
            "{armed} of {} breakpoints armed — the others sit on lines with no \
             executable statement, or outside the body of {}",
            breakpoints.len(),
            entry.fq_name
        ))
    };
    let _ = event_tx.unbounded_send(event);
}

/// How long an abort waits for the target to finish before cancelling its
/// query a second time.
const ABORT_GRACE: Duration = Duration::from_secs(2);

/// What tearing a trapped target down needs: where it runs, and the flags it
/// shares with the target thread.
struct Teardown<'a> {
    config: &'a Config,
    /// Backend pid of the trapped target.
    pid: i32,
    aborting: &'a AtomicBool,
    target_done: &'a AtomicBool,
}

impl Teardown<'_> {
    /// Abort the target (`pldbg_abort_target`) and cancel its query. A cancel
    /// that lands between two statements is a no-op server side, so when the
    /// target thread has not finished within [`ABORT_GRACE`] the cancel is
    /// sent once more rather than leaving a backend trapped behind a session
    /// the UI already considers over.
    fn abort(&self, session: &mut DebugSession) {
        self.aborting.store(true, Ordering::SeqCst);
        let _ = session.abort();
        let _ = cancel_backend(self.config, self.pid);
        let deadline = Instant::now() + ABORT_GRACE;
        while !self.target_done.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                debug!(
                    pid = self.pid,
                    "debug target still running after abort; cancelling again"
                );
                let _ = cancel_backend(self.config, self.pid);
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The routine the session was launched on, plus the two texts the controller
/// maps gutter rows through: the editor buffer as it was at launch, and that
/// routine's installed body. Breakpoints are always armed on this routine,
/// whichever frame the panel is inspecting.
struct Entry {
    oid: i32,
    /// For messages about breakpoints that could not be armed.
    fq_name: String,
    editor_text: String,
    /// The routine's installed body, as `pldbg_get_source` returns it.
    body: String,
}

/// Serve control commands until the target finishes, the user stops, or the
/// handle is dropped.
fn command_loop(
    session: &mut DebugSession,
    entry: &Entry,
    teardown: &Teardown,
    cmd_rx: &Receiver<Command>,
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
) -> Result<()> {
    // Which frame the panel is inspecting. A step or continue lands in the
    // innermost frame again, and pldebugger resets its own selection with it.
    let mut selected = 0;
    while let Ok(cmd) = cmd_rx.recv() {
        let stepped = match cmd {
            Command::StepOver => Some(session.step_over()?),
            Command::StepInto => Some(session.step_into()?),
            Command::Continue => Some(session.continue_()?),
            Command::SetBreakpoint(row) => {
                arm_breakpoint(session, entry, row, event_tx);
                None
            }
            Command::DropBreakpoint(row) => {
                if let Some(line) = body_line_of(&entry.editor_text, &entry.body, row) {
                    let _ = session.drop_breakpoint(entry.oid, line);
                } else {
                    // Never armed, so there is nothing to drop; only worth a trace.
                    debug!(
                        row,
                        "debug breakpoint row maps to no body line; nothing to drop"
                    );
                }
                None
            }
            Command::SelectFrame(level) => {
                if session.select_frame(level).is_ok() {
                    selected = level;
                    report_frame(session, event_tx, selected);
                }
                None
            }
            Command::Deposit { name, value } => {
                let _ = session.deposit_value(&name, -1, &value);
                // Re-read so the changed value shows immediately — of the frame
                // being inspected, which is the one the value was edited in.
                report_frame(session, event_tx, selected);
                None
            }
            Command::Stop => {
                teardown.abort(session);
                finish(event_tx);
                return Ok(());
            }
        };

        // A step/continue that returned `None` (or ran off an empty stack) means
        // the target is done.
        if let Some(stop) = stepped {
            selected = 0;
            let alive = stop.is_some() && report_top(session, event_tx);
            if !alive {
                finish(event_tx);
                return Ok(());
            }
        }
    }
    // Handle dropped without an explicit Stop.
    teardown.abort(session);
    finish(event_tx);
    Ok(())
}

/// Arm a breakpoint toggled on in the gutter mid-run, and say so when it
/// cannot be: a row that maps to no body line, or one pldebugger refuses,
/// would otherwise leave a "Continue" running past a breakpoint the gutter
/// still shows.
fn arm_breakpoint(
    session: &mut DebugSession,
    entry: &Entry,
    row: usize,
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
) {
    let note = match body_line_of(&entry.editor_text, &entry.body, row) {
        None => {
            debug!(row, routine = %entry.fq_name, "debug breakpoint row maps to no body line");
            format!(
                "Breakpoint not armed: that line is outside the body of {} as installed \
                 (re-create the routine to debug an edited version)",
                entry.fq_name
            )
        }
        Some(line) => match session.set_breakpoint(entry.oid, line) {
            Ok(true) => return,
            Ok(false) => {
                debug!(row, line, routine = %entry.fq_name, "pldebugger refused a breakpoint");
                format!(
                    "Breakpoint not armed: body line {line} of {} holds no executable \
                     statement",
                    entry.fq_name
                )
            }
            Err(err) => {
                debug!(row, line, error = %format!("{err:#}"), "arming a debug breakpoint failed");
                format!("Breakpoint not armed: {err:#}")
            }
        },
    };
    let _ = event_tx.unbounded_send(DebugEvent::Note(note));
}

/// [`report_frame`] for the innermost frame — where every fresh stop lands.
fn report_top(
    session: &mut DebugSession,
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
) -> bool {
    report_frame(session, event_tx, 0)
}

/// Gather the stack, plus the source, line and variables of frame `level`, and
/// emit a [`DebugEvent::Stopped`]. Returns `false` when the stack is empty (the
/// target has run off the end), so the caller can terminate.
///
/// `pldbg_get_variables` reports the frame `pldbg_select_frame` last selected,
/// so `level` must be that frame for the source and variables to describe the
/// same one.
fn report_frame(
    session: &mut DebugSession,
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
    level: i32,
) -> bool {
    let stack = session.get_stack().unwrap_or_default();
    // A level the stack no longer has (it shrank under us) falls back to the
    // innermost frame rather than reporting nothing.
    let Some(frame) = stack
        .iter()
        .find(|frame| frame.level == level)
        .or_else(|| stack.first())
    else {
        return false;
    };
    let (line, func_oid, frame) = (frame.line, frame.func_oid, frame.level);
    let source = session.get_source(func_oid).unwrap_or_default();
    let variables = session.get_variables().unwrap_or_default();
    event_tx
        .unbounded_send(DebugEvent::Stopped(StopState {
            line,
            source,
            frame,
            stack,
            variables,
        }))
        .is_ok()
}

/// Signal the UI that the session is over.
fn finish(event_tx: &async_mpsc::UnboundedSender<DebugEvent>) {
    let _ = event_tx.unbounded_send(DebugEvent::Terminated);
}

/// A `host:port` (or socket) summary of where a config will connect, so a
/// wrong host/port behind a "connection refused" is visible in the panel.
fn describe_addr(config: &Config) -> String {
    use postgres::config::Host;
    let all_ports = config.get_ports();
    let entries: Vec<String> = config
        .get_hosts()
        .iter()
        .enumerate()
        .map(|(i, host)| {
            let port = all_ports.get(i).or_else(|| all_ports.first()).copied();
            let name = match host {
                Host::Tcp(name) => name.clone(),
                #[cfg(unix)]
                Host::Unix(path) => path.display().to_string(),
            };
            port.map_or_else(|| name.clone(), |port| format!("{name}:{port}"))
        })
        .collect();
    if entries.is_empty() {
        "the database".to_string()
    } else {
        entries.join(", ")
    }
}

/// The 0-based editor line where the routine body (`pldbg_get_source`) begins
/// inside `editor_text`, or `None` when the body is not found verbatim (e.g. the
/// buffer was edited without re-creating the routine). Editor line `L` then maps
/// to body line `L - base + 1`.
fn body_base_line(editor_text: &str, body: &str) -> Option<usize> {
    if body.is_empty() {
        return None;
    }
    let byte = editor_text.find(body)?;
    Some(editor_text[..byte].bytes().filter(|&b| b == b'\n').count())
}

/// The 0-based editor line showing body line `body_line` of `body` — the
/// inverse of the breakpoint mapping above, used to walk the editor cursor
/// along with the debugger.
///
/// `None` when the buffer does not hold that body verbatim: the routine was
/// stepped into from another file, or the buffer was edited since it was
/// installed. Guessing a line there would highlight the wrong statement, so
/// the caller leaves the cursor where it is instead.
pub fn editor_line_of(editor_text: &str, body: &str, body_line: i32) -> Option<usize> {
    let base = body_base_line(editor_text, body)?;
    let line = usize::try_from(body_line).ok()?;
    Some(base + line.checked_sub(1)?)
}

/// The `pldbg` body line shown on 0-based editor `row` — the mapping the launch
/// breakpoints and the gutter toggles during a run are both armed through.
///
/// `None` when the row sits above the body, or when the buffer does not hold
/// that body verbatim.
fn body_line_of(editor_text: &str, body: &str, row: usize) -> Option<i32> {
    let base = body_base_line(editor_text, body)?;
    i32::try_from(row.checked_sub(base)? + 1).ok()
}

/// Everything the target thread needs, owned so the spawn closure is `'static`.
struct TargetRun {
    config: Config,
    oid: i32,
    fq_name: String,
    args: String,
    /// `application_name` of the control connection, so a run that never traps
    /// can cancel the controller's wait instead of leaving it parked forever.
    control_tag: String,
    event_tx: async_mpsc::UnboundedSender<DebugEvent>,
    aborting: Arc<AtomicBool>,
    attached: Arc<AtomicBool>,
    target_done: Arc<AtomicBool>,
}

/// How the target's invocation ended, which decides what the controller and the
/// panel are told next.
enum Outcome {
    /// The routine returned normally (its result was already reported).
    Completed,
    /// Connecting, building the call, or the call itself failed (reported).
    Failed,
    /// A Stop aborted the call; the controller is tearing the session down.
    Aborted,
}

/// The target thread: opens a fresh connection and runs the routine, which
/// blocks in the backend until the controller continues or aborts it.
fn run_target(run: &TargetRun) {
    let outcome = invoke_target(run);
    run.target_done.store(true, Ordering::SeqCst);

    if matches!(outcome, Outcome::Aborted) || run.attached.load(Ordering::SeqCst) {
        return;
    }

    // The routine ran (or failed) without the debugger ever attaching: the
    // controller is still blocked in `pldbg_wait_for_target` and would stay
    // there for good, so explain the miss and cancel that wait.
    if matches!(outcome, Outcome::Completed) {
        let _ = run.event_tx.unbounded_send(DebugEvent::Error(format!(
            "{} ran to completion without ever trapping — the debugger never attached. \
             pldebugger only traps when its plugin is loaded into the backend running the \
             routine: check `SHOW shared_preload_libraries` for plugin_debugger, and that \
             the call above hits the same routine the breakpoint was armed on (overloads \
             resolve independently).",
            run.fq_name
        )));
    }
    cancel_control_wait(&run.config, &run.control_tag);
    let _ = run.event_tx.unbounded_send(DebugEvent::Terminated);
}

/// Connect, build the call, run it, and report its result or failure.
fn invoke_target(run: &TargetRun) -> Outcome {
    // The routine's own `RAISE` output only reaches the client over this
    // connection's async-message stream; without a sink the driver drops it.
    let mut config = run.config.clone();
    config.application_name(&target_tag(&run.control_tag));
    let notices = run.event_tx.clone();
    config.notice_callback(move |notice| {
        let _ = notices.unbounded_send(DebugEvent::Notice(crate::db::notice_line(&notice)));
    });
    let mut client = match config.connect(NoTls) {
        Ok(client) => client,
        Err(err) => {
            let _ = run.event_tx.unbounded_send(DebugEvent::Error(format!(
                "target connection failed: {}",
                crate::db::describe(&err)
            )));
            return Outcome::Failed;
        }
    };

    let sql = match build_call_sql(&mut client, run.oid, &run.fq_name, &run.args) {
        Ok(sql) => sql,
        Err(err) => {
            let _ = run
                .event_tx
                .unbounded_send(DebugEvent::Error(format!("{err:#}")));
            return Outcome::Failed;
        }
    };
    let _ = run
        .event_tx
        .unbounded_send(DebugEvent::Status(format!("Running target: {sql}")));

    // Simple query so every column comes back as text, regardless of the
    // routine's return/INOUT types (same reason `db.rs` uses it).
    match client.simple_query(&sql) {
        Ok(messages) => {
            let _ = run
                .event_tx
                .unbounded_send(DebugEvent::Output(render_messages(&messages)));
            Outcome::Completed
        }
        // A Stop aborts the target mid-call; that error is expected.
        Err(err) if run.aborting.load(Ordering::SeqCst) => {
            let _ = err;
            Outcome::Aborted
        }
        Err(err) => {
            let _ = run
                .event_tx
                .unbounded_send(DebugEvent::Error(crate::db::describe(&err)));
            Outcome::Failed
        }
    }
}

/// A per-session `application_name` for the control connection.
fn control_tag() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "pg-gui-debug-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The `application_name` of a session's target connection, derived from its
/// control connection's tag.
fn target_tag(control_tag: &str) -> String {
    format!("{control_tag}-target")
}

/// Cancel the controller's `pldbg_wait_for_target`, found by the
/// `application_name` its connection was tagged with.
fn cancel_control_wait(config: &Config, control_tag: &str) {
    cancel_tagged(config, &[control_tag.to_string()]);
}

/// Cancel whatever the backends tagged with one of `tags` are running.
/// Best-effort: a server that will not signal the backend just leaves the
/// query running as before.
fn cancel_tagged(config: &Config, tags: &[String]) {
    let Ok(mut client) = config.connect(NoTls) else {
        return;
    };
    let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
    let _ = client.execute(
        "SELECT pg_cancel_backend(pid) FROM pg_stat_activity \
         WHERE application_name = ANY($1) AND pid <> pg_backend_pid()",
        &[&tags],
    );
}

/// The setups pldebugger cannot ever trap on, checked before a breakpoint is
/// armed: a routine in another language (an SQL one is inlined by the planner
/// and never enters the plugin at all), and a server whose backends never load
/// `plugin_debugger`, where the `pldbg_*` calls all succeed but nothing stops.
fn preflight(config: &Config, oid: i32, fq_name: &str) -> Result<()> {
    let mut client = config
        .connect(NoTls)
        .with_context(|| "opening a preflight connection")?;

    let row = client
        .query_one(
            "SELECT l.lanname AS lang FROM pg_proc p JOIN pg_language l ON l.oid = p.prolang \
             WHERE p.oid = $1::int4::oid",
            &[&oid],
        )
        .with_context(|| format!("looking up the language of {fq_name}"))?;
    let lang: String = row.get("lang");
    if lang != "plpgsql" {
        return Err(anyhow!(
            "{fq_name} is a LANGUAGE {lang} routine; pldebugger can only step PL/pgSQL, \
             so it would run to completion without ever stopping"
        ));
    }

    // The GUC is superuser-only on many servers; not being able to read it is
    // no reason to refuse to start.
    if let Ok(row) = client.query_one("SHOW shared_preload_libraries", &[]) {
        let libs: String = row.get(0);
        if !libs.contains("plugin_debugger") {
            let listed = if libs.trim().is_empty() {
                "the server preloads none".to_string()
            } else {
                format!("the server preloads {libs}")
            };
            return Err(anyhow!(
                "plugin_debugger is not in shared_preload_libraries ({listed}). The pldbgapi \
                 functions all work without it — the library loads on demand here — but \
                 PL/pgSQL only picks up the debugger hook at backend start, so the routine \
                 runs straight through instead of trapping. Add plugin_debugger to \
                 shared_preload_libraries and restart the server"
            ));
        }
    }
    Ok(())
}

/// Build the statement that invokes the routine. Procedures need `CALL`;
/// set-returning functions need `SELECT * FROM f(..)`; everything else is a
/// scalar `SELECT f(..)`. pgdap's own binary only emits `SELECT`, so this is
/// re-derived here to also cover procedures like the sample `place_order`.
fn build_call_sql(
    client: &mut postgres::Client,
    oid: i32,
    fq_name: &str,
    args: &str,
) -> Result<String> {
    let row = client
        .query_one(
            "SELECT prokind::text AS kind, proretset AS set FROM pg_proc WHERE oid = $1::int4::oid",
            &[&oid],
        )
        .with_context(|| format!("looking up routine kind for oid {oid}"))?;
    let kind: String = row.get("kind");
    let returns_set: bool = row.get("set");

    Ok(if kind == "p" {
        format!("CALL {fq_name}({args})")
    } else if returns_set {
        format!("SELECT * FROM {fq_name}({args})")
    } else {
        format!("SELECT {fq_name}({args})")
    })
}

/// Best-effort render of the routine's result (INOUT procedure outputs, or a
/// function's return) from the simple-query messages, every value as text.
fn render_messages(messages: &[SimpleQueryMessage]) -> String {
    let rows: Vec<String> = messages
        .iter()
        .filter_map(|message| match message {
            SimpleQueryMessage::Row(row) => Some(
                (0..row.columns().len())
                    .map(|i| row.get(i).unwrap_or("NULL").to_string())
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            _ => None,
        })
        .collect();
    if rows.is_empty() {
        "(no rows)".to_string()
    } else {
        rows.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use postgres::Config;

    use super::{body_line_of, describe_addr, editor_line_of};

    fn addr(conn: &str) -> String {
        describe_addr(&conn.parse::<Config>().expect("a valid connection string"))
    }

    #[test]
    fn the_address_names_each_host_with_its_port() {
        assert_eq!(addr("postgres://u@db:6543/x"), "db:6543");
        assert_eq!(addr("host=a,b port=1,2"), "a:1, b:2");
        // One port applies to every host.
        assert_eq!(addr("host=a,b port=7"), "a:7, b:7");
        // No port given: the host alone, not a made-up default.
        assert_eq!(addr("host=db"), "db");
    }

    #[test]
    fn an_address_without_a_host_is_described_generically() {
        assert_eq!(addr("dbname=x"), "the database");
    }

    /// `pldbg_get_source` returns the body starting right after the opening
    /// `$function$`, so its body line 1 is the line carrying that delimiter —
    /// the same origin the editor gutter counts from.
    #[test]
    fn body_lines_map_back_onto_editor_rows() {
        let editor = "CREATE OR REPLACE FUNCTION f()\n\
                      RETURNS int\n\
                      LANGUAGE plpgsql\n\
                      AS $function$\n\
                      BEGIN\n\
                      RETURN 1;\n\
                      END;\n\
                      $function$;\n";
        let body = "\nBEGIN\nRETURN 1;\nEND;\n";
        // Body line 1 is the `AS $function$` row (0-based row 3).
        assert_eq!(editor_line_of(editor, body, 1), Some(3));
        assert_eq!(editor_line_of(editor, body, 3), Some(5));
    }

    /// Gutter toggles map the other way, back onto the lines the launch
    /// breakpoints are armed on.
    #[test]
    fn editor_rows_map_onto_body_lines() {
        let editor =
            "CREATE FUNCTION f() RETURNS int\nAS $function$\nBEGIN\nRETURN 1;\nEND;\n$function$;\n";
        let body = "\nBEGIN\nRETURN 1;\nEND;\n";
        assert_eq!(body_line_of(editor, body, 1), Some(1));
        assert_eq!(body_line_of(editor, body, 3), Some(3));
        // Rows above the body have no body line.
        assert_eq!(body_line_of(editor, body, 0), None);
    }

    #[test]
    fn a_body_the_buffer_does_not_hold_has_no_row() {
        assert_eq!(editor_line_of("SELECT 1;\n", "\nBEGIN\nEND;\n", 2), None);
        assert_eq!(editor_line_of("SELECT 1;\n", "", 1), None);
    }

    /// Line 0 is pldebugger's "no line" marker, not a row above the body.
    #[test]
    fn line_zero_has_no_row() {
        let editor = "AS $$\nBEGIN\nEND;\n$$;\n";
        assert_eq!(editor_line_of(editor, "\nBEGIN\nEND;\n", 0), None);
    }
}
