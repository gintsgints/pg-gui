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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc as async_mpsc;
use pgdap::debugger::{DebugSession, cancel_backend};
use postgres::{Config, NoTls, SimpleQueryMessage};

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
    /// The debugged execution finished (or was aborted); the session is over.
    Terminated,
    /// The session could not start, or a control op failed hard.
    Error(String),
}

/// A snapshot of the paused target: the source being stepped, the current
/// line, the call stack, and the variables of the active frame.
pub struct StopState {
    pub line: i32,
    pub source: String,
    pub stack: Vec<Frame>,
    pub variables: Vec<Var>,
}

/// A control action sent from the UI to the controller thread.
enum Command {
    StepOver,
    StepInto,
    Continue,
    SetBreakpoint(i32),
    DropBreakpoint(i32),
    SelectFrame(i32),
    Deposit { name: String, value: String },
    Stop,
}

/// Handle to a running debug session. Dropping it (or calling [`Self::stop`])
/// aborts the trapped target and tears the threads down.
pub struct Session {
    cmd_tx: Sender<Command>,
    /// Set before the controller aborts the target, so the target thread knows
    /// its resulting query error is expected and not worth surfacing.
    aborting: Arc<AtomicBool>,
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
        breakpoints: &[i32],
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

        let launch = Launch {
            config,
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
                if let Err(err) = controller(launch) {
                    let _ = event_tx.unbounded_send(DebugEvent::Error(format!("{err:#}")));
                    let _ = event_tx.unbounded_send(DebugEvent::Terminated);
                }
            })?;

        Ok((
            Self {
                cmd_tx,
                aborting,
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

    pub fn set_breakpoint(&self, line: i32) {
        let _ = self.cmd_tx.send(Command::SetBreakpoint(line));
    }

    pub fn drop_breakpoint(&self, line: i32) {
        let _ = self.cmd_tx.send(Command::DropBreakpoint(line));
    }

    pub fn select_frame(&self, level: i32) {
        let _ = self.cmd_tx.send(Command::SelectFrame(level));
    }

    pub fn deposit(&self, name: String, value: String) {
        let _ = self.cmd_tx.send(Command::Deposit { name, value });
    }

    /// Abort the trapped target and end the session.
    pub fn stop(&self) {
        self.aborting.store(true, Ordering::SeqCst);
        let _ = self.cmd_tx.send(Command::Stop);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Best-effort: unblock the target if the handle is dropped without an
        // explicit stop. Dropping cmd_tx also makes the controller's recv fail.
        self.aborting.store(true, Ordering::SeqCst);
        let _ = self.cmd_tx.send(Command::Stop);
    }
}

/// Everything the controller thread needs, bundled so the spawn closure stays
/// small.
struct Launch {
    config: Config,
    signature: String,
    args: String,
    /// Editor-relative 0-based line numbers, mapped to body lines after attach.
    breakpoints: Vec<i32>,
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
    let mut session = DebugSession::connect(&config)
        .with_context(|| format!("connecting to {}", describe_addr(&config)))?;
    status(&format!("Resolving {signature}…"));
    let target = session.resolve_target(&signature)?;
    let oid = target.oid;

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

    // Now that the trap is set, launch the routine on its own connection.
    {
        let event_tx = event_tx.clone();
        let aborting = aborting.clone();
        let config = config.clone();
        let fq_name = target.fq_name.clone();
        std::thread::Builder::new()
            .name("pg-debug-target".into())
            .spawn(move || run_target(&config, oid, &fq_name, &args, &event_tx, &aborting))?;
    }

    status(&format!("Waiting for {} to be called…", target.fq_name));
    let pid = session.wait_for_target()?;
    status(&format!("Target attached (pid {pid}); reading state…"));

    // Attached at entry: map the editor-relative breakpoint lines to body
    // lines and arm them. The body source (`pldbg_get_source`) appears verbatim
    // in the editor buffer, so its starting line locates the offset.
    if !breakpoints.is_empty() {
        let body = session.get_source(oid).unwrap_or_default();
        if let Some(base) = body_base_line(&editor_text, &body) {
            let base = i32::try_from(base).unwrap_or(0);
            for &editor_line in &breakpoints {
                let body_line = editor_line - base + 1;
                if body_line >= 1 {
                    let _ = session.set_breakpoint(oid, body_line);
                }
            }
        }
    }

    let stopped = if stop_on_entry {
        report_top(&mut session, &event_tx)
    } else {
        session.continue_()?.is_some() && report_top(&mut session, &event_tx)
    };
    if !stopped {
        finish(&event_tx);
        return Ok(());
    }

    command_loop(
        &mut session,
        oid,
        &config,
        pid,
        &cmd_rx,
        &event_tx,
        &aborting,
    )
}

/// Serve control commands until the target finishes, the user stops, or the
/// handle is dropped.
fn command_loop(
    session: &mut DebugSession,
    oid: i32,
    config: &Config,
    pid: i32,
    cmd_rx: &Receiver<Command>,
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
    aborting: &AtomicBool,
) -> Result<()> {
    while let Ok(cmd) = cmd_rx.recv() {
        let stepped = match cmd {
            Command::StepOver => Some(session.step_over()?),
            Command::StepInto => Some(session.step_into()?),
            Command::Continue => Some(session.continue_()?),
            Command::SetBreakpoint(line) => {
                let _ = session.set_breakpoint(oid, line);
                None
            }
            Command::DropBreakpoint(line) => {
                let _ = session.drop_breakpoint(oid, line);
                None
            }
            Command::SelectFrame(level) => {
                if session.select_frame(level).is_ok() {
                    report_top(session, event_tx);
                }
                None
            }
            Command::Deposit { name, value } => {
                let _ = session.deposit_value(&name, -1, &value);
                // Re-read so the changed value shows immediately.
                report_top(session, event_tx);
                None
            }
            Command::Stop => {
                aborting.store(true, Ordering::SeqCst);
                let _ = session.abort();
                let _ = cancel_backend(config, pid);
                finish(event_tx);
                return Ok(());
            }
        };

        // A step/continue that returned `None` (or ran off an empty stack) means
        // the target is done.
        if let Some(stop) = stepped {
            let alive = stop.is_some() && report_top(session, event_tx);
            if !alive {
                finish(event_tx);
                return Ok(());
            }
        }
    }
    // Handle dropped without an explicit Stop.
    aborting.store(true, Ordering::SeqCst);
    let _ = session.abort();
    let _ = cancel_backend(config, pid);
    finish(event_tx);
    Ok(())
}

/// Gather stack + source + variables for the current stop and emit a
/// [`DebugEvent::Stopped`]. Returns `false` when the stack is empty (the target
/// has run off the end), so the caller can terminate.
fn report_top(
    session: &mut DebugSession,
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
) -> bool {
    let stack = session.get_stack().unwrap_or_default();
    let Some(top) = stack.first() else {
        return false;
    };
    let line = top.line;
    let source = session.get_source(top.func_oid).unwrap_or_default();
    let variables = session.get_variables().unwrap_or_default();
    event_tx
        .unbounded_send(DebugEvent::Stopped(StopState {
            line,
            source,
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

/// The target thread: opens a fresh connection and runs the routine, which
/// blocks in the backend until the controller continues or aborts it.
fn run_target(
    config: &Config,
    oid: i32,
    fq_name: &str,
    args: &str,
    event_tx: &async_mpsc::UnboundedSender<DebugEvent>,
    aborting: &AtomicBool,
) {
    let mut client = match config.connect(NoTls) {
        Ok(client) => client,
        Err(err) => {
            let _ = event_tx.unbounded_send(DebugEvent::Error(format!(
                "target connection failed: {}",
                crate::db::describe(&err)
            )));
            return;
        }
    };

    let sql = match build_call_sql(&mut client, oid, fq_name, args) {
        Ok(sql) => sql,
        Err(err) => {
            let _ = event_tx.unbounded_send(DebugEvent::Error(format!("{err:#}")));
            return;
        }
    };
    let _ = event_tx.unbounded_send(DebugEvent::Status(format!("Running target: {sql}")));

    // Simple query so every column comes back as text, regardless of the
    // routine's return/INOUT types (same reason `db.rs` uses it).
    match client.simple_query(&sql) {
        Ok(messages) => {
            let _ = event_tx.unbounded_send(DebugEvent::Output(render_messages(&messages)));
        }
        // A Stop aborts the target mid-call; that error is expected.
        Err(err) if aborting.load(Ordering::SeqCst) => {
            let _ = err;
        }
        Err(err) => {
            let _ = event_tx.unbounded_send(DebugEvent::Error(crate::db::describe(&err)));
        }
    }
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
