//! SQL language support for the editor, backed by the Postgres Language
//! Server (<https://pg-language-server.com>) used as a library.
//!
//! Instead of spawning `postgrestools lsp-proxy` and talking LSP over stdio,
//! we embed the server's `pgls_workspace` crate directly and call its
//! [`Workspace`] trait. Completions, hover and diagnostics are plugged into
//! the `gpui-component` editor through its provider traits. The database
//! credentials come from the workspace settings we push at startup, which is
//! what makes completions schema-aware.
//!
//! Being a library, there is no wire traffic to trace, so the boundary logs
//! itself: the configuration derived from the connection string, each
//! document sync, and every answer the workspace gives, with completions
//! tallied by item kind. See the `PG_GUI_LOG` recipe in `CLAUDE.md`.

use std::sync::Arc;
use std::sync::mpsc::{Receiver as StdReceiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures::channel::{mpsc, oneshot};
use gpui::{App, AppContext as _, Task, Window};
use gpui_component::input::{CompletionProvider, HoverProvider, Rope, RopeExt as _};
use lsp_types::{
    CompletionContext, CompletionItemLabelDetails, CompletionResponse, CompletionTextEdit,
    DiagnosticSeverity, Hover, HoverContents, InsertTextFormat, MarkedString, NumberOrString,
    Range, TextEdit,
};

use pgls_analyse::RuleCategories;
use pgls_completions::CompletionItemKind as PgCompletionItemKind;
use pgls_configuration::database::PartialDatabaseConfiguration;
use pgls_configuration::format::{KeywordCase, PartialFormatConfiguration};
use pgls_configuration::{PartialConfiguration, PartialTypecheckConfiguration};
use pgls_diagnostics::{Diagnostic as _, PrintDescription, Severity};
use pgls_fs::PgLSPath;
use pgls_text_size::{TextRange, TextSize};
use pgls_workspace::features::completions::GetCompletionsParams;
use pgls_workspace::features::diagnostics::PullFileDiagnosticsParams;
use pgls_workspace::features::format::PullFileFormattingParams;
use pgls_workspace::features::on_hover::OnHoverParams;
use pgls_workspace::workspace::{
    ChangeFileParams, CloseFileParams, GetFileContentParams, OpenFileParams,
    RegisterProjectFolderParams, UpdateSettingsParams,
};
use pgls_workspace::{Workspace, WorkspaceError};
use tracing::{debug, info, trace, warn};

use crate::config::CaseStyle;

/// How long a burst of edits is allowed to settle before we re-run the
/// (potentially database-touching) diagnostics analysis.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// How much of the text before the cursor the completion/hover trace shows.
const CONTEXT_CHARS: usize = 40;

/// How long to wait before asking again whether the schema cache has loaded,
/// while it has not. The server backs a failed connection off for up to a
/// minute, so a tighter poll would only add log lines.
const SCHEMA_PROBE_INTERVAL: Duration = Duration::from_secs(20);

/// The statement the schema probe completes. A `FROM` with nothing after it is
/// the one position where the server can only answer out of the schema cache,
/// so a reply holding relations proves the cache loaded.
const PROBE_TEXT: &str = "select * from ";

/// Stack for the document worker. Every language feature parses the statement
/// under the cursor, and `pgls_query` parses by decoding `libpg_query`'s protobuf
/// AST with `prost`: the generated decoder for the `Node` message — a `oneof`
/// with hundreds of variants — has enormous stack frames, so a handful of
/// nesting levels (a `CREATE TABLE`'s `ColumnDef` → `TypeName` is already
/// enough) overruns a thread's default 2 MiB and aborts the process. A stack
/// overflow cannot be caught, so [`contain_panic`] is no help here; the only
/// defence is room. This is address space, committed page by page as used.
const WORKER_STACK: usize = 64 * 1024 * 1024;

/// Run a workspace call, containing any panic inside the `pgls_*` crates.
/// The language server panics on some inputs (e.g. its tree-sitter scope
/// tracker), and a panic unwinding into the background executor's
/// `extern "C"` dispatch trampoline aborts the whole app — a language
/// feature must never take the editor down with it.
fn contain_panic<T>(f: impl FnOnce() -> T) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        // Most callers drop the error (a language feature must not fail the
        // edit), so this is the only place the panic is ever recorded.
        warn!("language server panicked: {message}");
        anyhow!("language server panicked: {message}")
    })
}

/// Diagnostics computed for the editor document.
pub type DiagnosticsReceiver = mpsc::UnboundedReceiver<Vec<lsp_types::Diagnostic>>;

/// Schema-cache state, as the status bar reports it.
///
/// The workspace loads its schema cache lazily, on the first feature that
/// needs a database, and every such feature degrades silently when the load
/// fails — completions come back as keywords alone, hover empty. Nothing in
/// the [`Workspace`] trait reports on the cache (it can only invalidate it),
/// so this is derived from a probe the document worker runs: see
/// [`probe_schema`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaStatus {
    /// No answer yet — the first probe is still out.
    Loading,
    /// The workspace answered out of a loaded schema cache.
    Loaded,
    /// The database is unreachable, so there is no schema to complete from.
    Offline,
    /// The workspace was configured without a database (the connection string
    /// did not parse), so no schema can ever load.
    Disabled,
}

impl SchemaStatus {
    /// How the status bar words it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Loading => "SQL LSP: schema loading",
            Self::Loaded => "SQL LSP: schema loaded",
            Self::Offline => "SQL LSP: no schema — database unreachable",
            Self::Disabled => "SQL LSP: no schema — no database",
        }
    }
}

/// Schema-cache state pushed by the document worker as it changes.
pub type SchemaStatusReceiver = mpsc::UnboundedReceiver<SchemaStatus>;

/// A handle to an embedded language-server workspace. Cloning is cheap; the
/// workspace and its background document worker are torn down once the last
/// clone is dropped.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    // Runtime provider calls go through the document worker (`doc_tx`), which
    // owns its own workspace/path clones; these two are kept only so the
    // `#[cfg(test)]` probes can hit the workspace synchronously.
    #[cfg_attr(not(test), allow(dead_code))]
    workspace: Arc<dyn Workspace>,
    #[cfg_attr(not(test), allow(dead_code))]
    path: PgLSPath,
    connection_string: String,
    keyword_case: CaseStyle,
    constant_case: CaseStyle,
    /// Feeds the document worker. Every write to the workspace document
    /// (change, format, close) goes through it, because workspace calls can
    /// block for seconds waiting on an unreachable database and must never
    /// run on the UI thread. Dropping it (with the last [`Client`] clone)
    /// makes the worker close the document and exit.
    doc_tx: std::sync::mpsc::Sender<DocEvent>,
}

/// Work items for the document worker thread.
enum DocEvent {
    /// The editor buffer changed (full-text sync).
    Changed(String),
    /// Format the workspace document and reply with the result.
    Format {
        text: String,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    /// Sync the buffer to `text`, then compute completions at `position` and
    /// reply. Routed through the worker (rather than hitting the workspace
    /// directly) so the completion query is ordered *after* the change that
    /// triggered it — otherwise it races the pending [`DocEvent::Changed`] and
    /// sees a stale document, yielding clause keywords instead of the schema
    /// item being typed.
    Completions {
        text: String,
        position: TextSize,
        reply: oneshot::Sender<CompletionOutcome>,
    },
    /// Sync the buffer to `text`, then compute hover at `position` and reply.
    /// Routed through the worker for the same reason as [`Self::Completions`]:
    /// hovering the just-typed token must see the current document.
    Hover {
        text: String,
        position: TextSize,
        reply: oneshot::Sender<HoverOutcome>,
    },
    /// Close the workspace document and stop the worker.
    Close,
}

/// Result of a worker-side completion request.
enum CompletionOutcome {
    Items(Vec<pgls_completions::CompletionItem>),
    /// The database is unreachable; the caller falls back to snippets alone.
    DatabaseOffline,
    Failed(String),
}

/// Result of a worker-side hover request.
enum HoverOutcome {
    /// Markdown blocks for the hovered symbol.
    Content(Vec<String>),
    /// The database is unreachable; the caller shows nothing.
    DatabaseOffline,
    Failed(String),
}

impl Client {
    /// Create an in-process workspace, configure it with the database
    /// credentials and formatter options, and open the editor buffer as a
    /// document. Loading the schema cache happens lazily on the first
    /// completion/diagnostic, so call this from a background thread.
    ///
    /// Besides the diagnostics stream, the worker reports what the schema
    /// cache is doing ([`SchemaStatus`]) so the status bar can say whether
    /// completions are schema-aware yet.
    ///
    /// # Errors
    ///
    /// Fails when the workspace directory cannot be prepared or the workspace
    /// rejects the initial configuration or document (or panics doing so).
    pub fn start(
        connection_string: &str,
        text: &str,
        keyword_case: CaseStyle,
        constant_case: CaseStyle,
    ) -> Result<(Self, DiagnosticsReceiver, SchemaStatusReceiver)> {
        contain_panic(|| Self::start_inner(connection_string, text, keyword_case, constant_case))?
    }

    fn start_inner(
        connection_string: &str,
        text: &str,
        keyword_case: CaseStyle,
        constant_case: CaseStyle,
    ) -> Result<(Self, DiagnosticsReceiver, SchemaStatusReceiver)> {
        let workspace = pgls_workspace::workspace::server_sync();
        let dir = workspace_dir()?;
        std::fs::create_dir_all(&dir).ok();
        let path = PgLSPath::new(dir.join("scratch.sql"));
        // The probe completes its own scratch document, so it never disturbs
        // (or is disturbed by) the editor buffer opened below.
        let probe_path = PgLSPath::new(dir.join("schema-probe.sql"));

        workspace
            .register_project_folder(RegisterProjectFolderParams {
                path: Some(dir.clone()),
                set_as_current_workspace: true,
            })
            .map_err(|err| anyhow!("failed to register language server project: {err}"))?;

        let configuration = build_configuration(connection_string, keyword_case, constant_case);
        log_configuration(&configuration, &dir);
        // Settled here rather than re-parsed: `build_configuration` is what
        // decides a connection string the driver rejects runs without a
        // database, and a disabled connection can never load a schema.
        let connection_disabled = configuration
            .db
            .as_ref()
            .and_then(|db| db.disable_connection)
            .unwrap_or(false);

        workspace
            .update_settings(UpdateSettingsParams {
                configuration,
                vcs_base_path: None,
                gitignore_matches: Vec::new(),
                workspace_directory: Some(dir),
            })
            .map_err(|err| anyhow!("failed to configure language server: {err}"))?;

        workspace
            .open_file(OpenFileParams {
                path: path.clone(),
                content: text.to_string(),
                version: 0,
            })
            .map_err(|err| anyhow!("failed to open document: {err}"))?;

        let (diagnostics_tx, diagnostics_rx) = mpsc::unbounded();
        let (schema_tx, schema_rx) = mpsc::unbounded();
        let (doc_tx, doc_rx) = std::sync::mpsc::channel();

        let worker = Worker {
            workspace: workspace.clone(),
            path: path.clone(),
            probe_path,
            diagnostics: diagnostics_tx,
            schema: schema_tx,
            connection_disabled,
        };
        std::thread::Builder::new()
            .name("pg-lsp-document".into())
            .stack_size(WORKER_STACK)
            .spawn(move || {
                document_worker(&worker, &doc_rx);
            })?;
        info!(
            bytes = text.len(),
            "language server workspace started, document opened"
        );
        // Publish an initial diagnostics set for the freshly opened document.
        doc_tx.send(DocEvent::Changed(text.to_string())).ok();

        let inner = Arc::new(Inner {
            workspace,
            path,
            connection_string: connection_string.to_string(),
            keyword_case,
            constant_case,
            doc_tx,
        });
        Ok((Self { inner }, diagnostics_rx, schema_rx))
    }

    /// The connection string the workspace was configured with at startup.
    #[must_use]
    pub fn connection_string(&self) -> &str {
        &self.inner.connection_string
    }

    /// The formatter casing options the workspace was configured with at
    /// startup, as `(keyword_case, constant_case)`.
    #[must_use]
    pub fn case_options(&self) -> (CaseStyle, CaseStyle) {
        (self.inner.keyword_case, self.inner.constant_case)
    }

    /// Tell the workspace the editor buffer changed (full-text sync) and
    /// schedule a fresh diagnostics run. Returns immediately: the change is
    /// applied on the document worker thread, since workspace calls can block
    /// on the database (e.g. a diagnostics run waiting out an unreachable
    /// server holds the document lock) and this is called from the UI thread.
    pub fn document_changed(&self, text: String) {
        self.inner.doc_tx.send(DocEvent::Changed(text)).ok();
    }

    /// Format the whole document. The workspace formats its own copy (kept in
    /// sync via [`Self::document_changed`]); `text` is used only to decide
    /// whether anything changed. `None` when there is nothing to change —
    /// including when formatting is unavailable or the document does not
    /// parse. Runs on the document worker thread, ordered after any pending
    /// buffer changes, so the caller's executor is not blocked.
    ///
    /// # Errors
    ///
    /// Fails when the workspace rejects the request.
    pub async fn format(&self, text: &str) -> Result<Option<String>> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .doc_tx
            .send(DocEvent::Format {
                text: text.to_string(),
                reply,
            })
            .map_err(|_| anyhow!("language server is shut down"))?;
        rx.await
            .map_err(|_| anyhow!("formatting task was cancelled"))?
    }

    /// Close the workspace document and stop the document worker. Returns
    /// immediately; the worker closes the document on its own thread.
    pub fn shutdown(&self) {
        self.inner.doc_tx.send(DocEvent::Close).ok();
    }
}

/// What the document worker owns: the workspace, the two documents it writes,
/// and the channels it answers on.
struct Worker {
    workspace: Arc<dyn Workspace>,
    /// The editor buffer, mirrored into the workspace.
    path: PgLSPath,
    /// The scratch document [`probe_schema`] completes.
    probe_path: PgLSPath,
    diagnostics: mpsc::UnboundedSender<Vec<lsp_types::Diagnostic>>,
    schema: mpsc::UnboundedSender<SchemaStatus>,
    /// The workspace was configured without a database, so no schema can ever
    /// load and probing for one is pointless.
    connection_disabled: bool,
}

/// Owns every write to the workspace document. Applies buffer changes as they
/// arrive, answers format/completion/hover requests in order, republishes
/// diagnostics once a burst of edits settles ([`DEBOUNCE`]), and polls the
/// schema cache until it has loaded ([`SCHEMA_PROBE_INTERVAL`]). Workspace
/// calls can block for seconds while the database is unreachable (diagnostics
/// hold the document lock across connection attempts), which is why all of
/// this runs on its own thread. Returns — closing the workspace document on
/// the way out — when a [`DocEvent::Close`] arrives or the channel is dropped
/// (i.e. the last [`Client`] is gone).
fn document_worker(worker: &Worker, events: &StdReceiver<DocEvent>) {
    let workspace = &worker.workspace;
    let path = &worker.path;
    let mut version = 0;
    // When the diagnostics of the last change are due; cleared once published.
    let mut settle_at: Option<Instant> = None;
    let mut status = if worker.connection_disabled {
        SchemaStatus::Disabled
    } else {
        SchemaStatus::Loading
    };
    worker.schema.unbounded_send(status).ok();
    // When to probe the schema cache next; `None` once it has loaded (or can
    // never load). The first probe waits out the opening diagnostics run,
    // which loads the cache itself and so usually makes the probe a cache hit.
    let mut probe_at = (!worker.connection_disabled).then(|| Instant::now() + DEBOUNCE);
    loop {
        // Wait only until the nearest of the two deadlines, so a burst of
        // edits still coalesces into one analysis run.
        let now = Instant::now();
        let deadline = [settle_at, probe_at]
            .into_iter()
            .flatten()
            .min()
            .map(|at| at.saturating_duration_since(now));
        let event = match deadline {
            Some(timeout) => match events.recv_timeout(timeout) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match events.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };
        match event {
            Some(DocEvent::Changed(content)) => {
                version += 1;
                apply_change(workspace, path, version, content);
                settle_at = Some(Instant::now() + DEBOUNCE);
            }
            Some(DocEvent::Format { text, reply }) => {
                reply.send(format_document(workspace, path, &text)).ok();
            }
            Some(DocEvent::Completions {
                text,
                position,
                reply,
            }) => {
                trace!(
                    position = u32::from(position),
                    before = %cursor_context(&text, position),
                    "completion request"
                );
                version += 1;
                apply_change(workspace, path, version, text);
                reply
                    .send(worker_completions(workspace, path, position))
                    .ok();
                // The document moved, so diagnostics need a fresh run.
                settle_at = Some(Instant::now() + DEBOUNCE);
            }
            Some(DocEvent::Hover {
                text,
                position,
                reply,
            }) => {
                trace!(
                    position = u32::from(position),
                    before = %cursor_context(&text, position),
                    "hover request"
                );
                version += 1;
                apply_change(workspace, path, version, text);
                reply.send(worker_hover(workspace, path, position)).ok();
                // The document moved, so diagnostics need a fresh run.
                settle_at = Some(Instant::now() + DEBOUNCE);
            }
            Some(DocEvent::Close) => break,
            // A deadline passed rather than an event arriving; both may be due.
            None => {
                if settle_at.is_some_and(|at| at <= Instant::now()) {
                    settle_at = None;
                    if !publish_diagnostics(worker) {
                        break;
                    }
                }
                if probe_at.is_some_and(|at| at <= Instant::now()) {
                    let Some(probed) = report_schema_status(worker, status) else {
                        break;
                    };
                    status = probed;
                    // Once loaded it stays loaded: the workspace caches the
                    // schema per connection for the life of the process.
                    probe_at = (status != SchemaStatus::Loaded)
                        .then(|| Instant::now() + SCHEMA_PROBE_INTERVAL);
                }
            }
        }
    }
    contain_panic(|| {
        workspace
            .close_file(CloseFileParams { path: path.clone() })
            .ok();
    })
    .ok();
}

/// Run the settled diagnostics analysis and publish it. `false` once the
/// receiver is gone, i.e. the worker has nobody left to report to.
fn publish_diagnostics(worker: &Worker) -> bool {
    // A contained panic publishes nothing and keeps the worker alive for the
    // next edit.
    let diagnostics =
        contain_panic(|| pull_diagnostics(&worker.workspace, &worker.path)).unwrap_or_default();
    worker.diagnostics.unbounded_send(diagnostics).is_ok()
}

/// Re-probe the schema cache and push the result when it differs from `status`.
/// The new status, or `None` once the receiver is gone.
fn report_schema_status(worker: &Worker, status: SchemaStatus) -> Option<SchemaStatus> {
    let probed = probe_schema(&worker.workspace, &worker.probe_path);
    if probed == status {
        return Some(status);
    }
    info!(status = ?probed, "schema cache status");
    worker.schema.unbounded_send(probed).ok()?;
    Some(probed)
}

/// Ask the workspace to complete a bare `FROM` on a scratch document, to find
/// out whether its schema cache has loaded. There is no direct way to ask: the
/// [`Workspace`] trait can invalidate the cache but not report on it, and the
/// features that read it fail soft, so the answer has to be inferred from one.
/// Relations in the reply mean the cache loaded; a `DatabaseConnectionError`
/// means it did not. An answer with no relations at all (the server skips
/// completions entirely when it has no pool, e.g. during its connection
/// backoff) is reported as offline too — the user has no schema either way.
fn probe_schema(workspace: &Arc<dyn Workspace>, path: &PgLSPath) -> SchemaStatus {
    contain_panic(|| {
        if let Err(err) = workspace.open_file(OpenFileParams {
            path: path.clone(),
            content: PROBE_TEXT.to_string(),
            version: 0,
        }) {
            debug!("schema probe could not open its document: {err}");
            return SchemaStatus::Offline;
        }
        let result = workspace.get_completions(GetCompletionsParams {
            path: path.clone(),
            position: to_text_size(PROBE_TEXT.len()),
        });
        workspace
            .close_file(CloseFileParams { path: path.clone() })
            .ok();
        match result {
            Ok(items) => {
                let relations = items
                    .into_iter()
                    .filter(|item| !matches!(item.kind, PgCompletionItemKind::Keyword))
                    .count();
                debug!(relations, "schema probe");
                if relations > 0 {
                    SchemaStatus::Loaded
                } else {
                    SchemaStatus::Offline
                }
            }
            Err(err @ WorkspaceError::DatabaseConnectionError(_)) => {
                debug!("schema probe has no database connection: {err}");
                SchemaStatus::Offline
            }
            Err(err) => {
                warn!("schema probe failed: {err}");
                SchemaStatus::Offline
            }
        }
    })
    .unwrap_or(SchemaStatus::Offline)
}

/// Apply a full-text change to the workspace document, containing any panic.
fn apply_change(workspace: &Arc<dyn Workspace>, path: &PgLSPath, version: i32, content: String) {
    contain_panic(|| {
        trace!(version, bytes = content.len(), "syncing document");
        if let Err(err) = workspace.change_file(ChangeFileParams {
            path: path.clone(),
            version,
            content,
        }) {
            // A rejected change leaves the workspace document behind the
            // buffer, which is itself a reason completions go stale.
            warn!("document sync failed: {err}");
        }
    })
    .ok();
}

/// Compute completions at `position` on the (already synced) document.
fn worker_completions(
    workspace: &Arc<dyn Workspace>,
    path: &PgLSPath,
    position: TextSize,
) -> CompletionOutcome {
    match contain_panic(|| {
        workspace.get_completions(GetCompletionsParams {
            path: path.clone(),
            position,
        })
    }) {
        Ok(Ok(result)) => {
            let items: Vec<_> = result.into_iter().collect();
            log_completion_items(&items);
            CompletionOutcome::Items(items)
        }
        Ok(Err(err @ WorkspaceError::DatabaseConnectionError(_))) => {
            // The single most common reason schema items are missing, and
            // invisible otherwise: the provider silently falls back to
            // snippets, and the server only warns on the first failure of a
            // backoff window.
            debug!("completions have no database connection: {err}");
            CompletionOutcome::DatabaseOffline
        }
        Ok(Err(err)) => {
            warn!("completions failed: {err}");
            CompletionOutcome::Failed(err.to_string())
        }
        // `contain_panic` has already logged the panic.
        Err(err) => CompletionOutcome::Failed(err.to_string()),
    }
}

/// Summarise what the server answered a completion request with. A list that
/// holds keywords but no `Table`/`Column` items is the signature of a schema
/// cache that never loaded — the usual reason table names do not complete —
/// so the tally is logged even when the list is not empty.
fn log_completion_items(items: &[pgls_completions::CompletionItem]) {
    let mut tables = 0usize;
    let mut columns = 0usize;
    let mut schemas = 0usize;
    let mut functions = 0usize;
    let mut keywords = 0usize;
    let mut other = 0usize;
    for item in items {
        match &item.kind {
            PgCompletionItemKind::Table => tables += 1,
            PgCompletionItemKind::Column => columns += 1,
            PgCompletionItemKind::Schema => schemas += 1,
            PgCompletionItemKind::Function => functions += 1,
            PgCompletionItemKind::Keyword => keywords += 1,
            PgCompletionItemKind::Policy | PgCompletionItemKind::Role => other += 1,
        }
    }
    debug!(
        total = items.len(),
        tables, columns, schemas, functions, keywords, other, "server completions"
    );
    if !items.is_empty() {
        trace!(
            labels = ?items.iter().map(|item| item.label.as_str()).collect::<Vec<_>>(),
            "server completion labels"
        );
    }
}

/// The characters just before `position`, for the completion/hover trace.
/// What the server took to be the statement under the cursor is the first
/// thing to check when nothing schema-aware comes back.
fn cursor_context(text: &str, position: TextSize) -> String {
    let mut end = usize::from(position).min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let start = text[..end]
        .char_indices()
        .rev()
        .take(CONTEXT_CHARS)
        .last()
        .map_or(end, |(offset, _)| offset);
    text[start..end].replace('\n', "\\n")
}

/// Compute hover at `position` on the (already synced) document.
fn worker_hover(
    workspace: &Arc<dyn Workspace>,
    path: &PgLSPath,
    position: TextSize,
) -> HoverOutcome {
    match contain_panic(|| {
        workspace.on_hover(OnHoverParams {
            path: path.clone(),
            position,
        })
    }) {
        Ok(Ok(result)) => {
            let blocks: Vec<_> = result.into_iter().collect();
            debug!(blocks = blocks.len(), "server hover");
            HoverOutcome::Content(blocks)
        }
        Ok(Err(err @ WorkspaceError::DatabaseConnectionError(_))) => {
            debug!("hover has no database connection: {err}");
            HoverOutcome::DatabaseOffline
        }
        Ok(Err(err)) => {
            warn!("hover failed: {err}");
            HoverOutcome::Failed(err.to_string())
        }
        Err(err) => HoverOutcome::Failed(err.to_string()),
    }
}

fn pull_diagnostics(workspace: &Arc<dyn Workspace>, path: &PgLSPath) -> Vec<lsp_types::Diagnostic> {
    let content = match workspace.get_file_content(GetFileContentParams { path: path.clone() }) {
        Ok(content) => content,
        Err(err) => {
            warn!("diagnostics could not read the workspace document: {err}");
            return Vec::new();
        }
    };
    let rope = Rope::from(content.as_str());
    let result = workspace.pull_file_diagnostics(PullFileDiagnosticsParams {
        path: path.clone(),
        categories: RuleCategories::all(),
        max_diagnostics: u32::MAX,
        only: Vec::new(),
        skip: Vec::new(),
    });
    let result = match result {
        Ok(result) => result,
        Err(err @ WorkspaceError::DatabaseConnectionError(_)) => {
            debug!("diagnostics have no database connection: {err}");
            return Vec::new();
        }
        Err(err) => {
            warn!("diagnostics failed: {err}");
            return Vec::new();
        }
    };
    let diagnostics: Vec<_> = result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic_to_lsp(diagnostic, &rope))
        .collect();
    debug!(
        count = diagnostics.len(),
        reported = result.diagnostics.len(),
        "server diagnostics"
    );
    diagnostics
}

fn diagnostic_to_lsp(
    diagnostic: &pgls_diagnostics::serde::Diagnostic,
    rope: &Rope,
) -> Option<lsp_types::Diagnostic> {
    let span = diagnostic.location().span?;
    let severity = match diagnostic.severity() {
        Severity::Fatal | Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Information => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    };
    let code = diagnostic
        .category()
        .map(|category| NumberOrString::String(category.name().to_string()));
    let message = PrintDescription(diagnostic).to_string();
    if message.is_empty() {
        return None;
    }
    Some(lsp_types::Diagnostic {
        range: text_range_to_range(span, rope),
        severity: Some(severity),
        code,
        source: Some("pg".into()),
        message,
        ..Default::default()
    })
}

fn format_document(
    workspace: &Arc<dyn Workspace>,
    path: &PgLSPath,
    text: &str,
) -> Result<Option<String>> {
    let result = contain_panic(|| {
        workspace.pull_file_formatting(PullFileFormattingParams {
            path: path.clone(),
            range: None,
        })
    })?
    .map_err(|err| anyhow!("formatting failed: {err}"))?;
    let formatted = result.formatted;
    // Formatting is disabled or the document did not parse: never blank the
    // buffer with an empty result.
    if formatted.is_empty() && !text.is_empty() {
        return Ok(None);
    }
    Ok((formatted != text).then_some(formatted))
}

/// Bridges the embedded workspace into the editor's LSP provider traits.
pub struct Provider {
    client: Client,
}

impl Provider {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        // gpui-component smuggles the query typed so far in here; keep it for
        // clamping the items' filter_text below.
        let query = trigger.trigger_character.unwrap_or_default();
        let position = to_text_size(offset);
        let doc_tx = self.client.inner.doc_tx.clone();
        let content = text.to_string();
        let rope = text.clone();
        let snippets = snippet_items(text, offset);
        let snippet_count = snippets.len();
        cx.background_spawn(async move {
            // Route through the document worker so the query runs *after* the
            // buffer change it was triggered by; the workspace document is
            // otherwise stale by one keystroke.
            let (reply, reply_rx) = oneshot::channel();
            if doc_tx
                .send(DocEvent::Completions {
                    text: content,
                    position,
                    reply,
                })
                .is_err()
            {
                return Ok(CompletionResponse::Array(snippets));
            }
            let result = match reply_rx.await {
                Ok(CompletionOutcome::Items(result)) => result,
                // The database is unreachable (or the worker is gone); offer
                // the snippets alone rather than error.
                Ok(CompletionOutcome::DatabaseOffline) | Err(_) => {
                    return Ok(CompletionResponse::Array(snippets));
                }
                Ok(CompletionOutcome::Failed(err)) => {
                    return Err(anyhow!("completion request failed: {err}"));
                }
            };
            let mut items: Vec<lsp_types::CompletionItem> = snippets;
            items.extend(
                result
                    .into_iter()
                    .map(|item| completion_to_lsp(item, &rope)),
            );
            clamp_filter_text(&mut items, &query);
            debug!(
                total = items.len(),
                snippets = snippet_count,
                query = query.as_str(),
                "completion menu"
            );
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _: &mut App) -> bool {
        is_trigger(new_text)
    }
}

/// Word characters continue an existing completion; the rest are the
/// trigger characters the completion sources act on.
fn is_trigger(new_text: &str) -> bool {
    new_text
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | '"' | '(' | ' '))
}

/// Completion provider installed while the language server is offline:
/// snippet suggestions only, so `New:` templates still complete without a
/// database connection.
pub struct SnippetCompletions;

impl CompletionProvider for SnippetCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let query = trigger.trigger_character.unwrap_or_default();
        let mut items = snippet_items(text, offset);
        clamp_filter_text(&mut items, &query);
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _: &mut App) -> bool {
        is_trigger(new_text)
    }
}

/// Snippet suggestions for the words before the cursor (see
/// [`snippets::suggestions`]), as completion items whose text edit
/// replaces those words with the template. The `$n` markers land in the
/// buffer verbatim; the app's tab handler then walks them.
fn snippet_items(rope: &Rope, offset: usize) -> Vec<lsp_types::CompletionItem> {
    let row = rope.offset_to_point(offset).row;
    let line_start = rope.line_start_offset(row);
    let line = rope.slice_line(row).to_string();
    let before_cursor = &line[..offset - line_start];
    crate::snippets::suggestions(before_cursor)
        .into_iter()
        .map(|suggestion| lsp_types::CompletionItem {
            label: suggestion.name.to_string(),
            kind: Some(lsp_types::CompletionItemKind::SNIPPET),
            label_details: Some(CompletionItemLabelDetails {
                description: Some("snippet".into()),
                detail: None,
            }),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                new_text: suggestion.sql.trim().to_string(),
                range: Range {
                    start: rope.offset_to_position(offset - suggestion.replace_len),
                    end: rope.offset_to_position(offset),
                },
            })),
            ..lsp_types::CompletionItem::default()
        })
        .collect()
}

impl HoverProvider for Provider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let position = to_text_size(offset);
        let doc_tx = self.client.inner.doc_tx.clone();
        let content = text.to_string();
        cx.background_spawn(async move {
            // Route through the document worker so the query runs after the
            // buffer change it follows; the workspace document is otherwise
            // stale by one keystroke.
            let (reply, reply_rx) = oneshot::channel();
            if doc_tx
                .send(DocEvent::Hover {
                    text: content,
                    position,
                    reply,
                })
                .is_err()
            {
                return Ok(None);
            }
            let result = match reply_rx.await {
                Ok(HoverOutcome::Content(result)) => result,
                Ok(HoverOutcome::DatabaseOffline) | Err(_) => return Ok(None),
                Ok(HoverOutcome::Failed(err)) => {
                    return Err(anyhow!("hover request failed: {err}"));
                }
            };
            let blocks: Vec<MarkedString> = result
                .into_iter()
                .map(MarkedString::from_markdown)
                .collect();
            if blocks.is_empty() {
                return Ok(None);
            }
            Ok(Some(Hover {
                contents: HoverContents::Array(blocks),
                range: None,
            }))
        })
    }
}

fn completion_to_lsp(
    item: pgls_completions::CompletionItem,
    rope: &Rope,
) -> lsp_types::CompletionItem {
    let is_snippet = item.completion_text.as_ref().is_some_and(|c| c.is_snippet);
    let detail = item
        .detail
        .map_or_else(|| format!(" {}", item.kind), |detail| format!(" {detail}"));
    let text_edit = item.completion_text.map(|completion| {
        CompletionTextEdit::Edit(TextEdit {
            new_text: completion.text,
            range: text_range_to_range(completion.range, rope),
        })
    });
    lsp_types::CompletionItem {
        kind: Some(completion_kind(&item.kind)),
        label: item.label,
        label_details: Some(CompletionItemLabelDetails {
            description: Some(item.description),
            detail: Some(detail),
        }),
        preselect: Some(item.preselected),
        sort_text: Some(item.sort_text),
        insert_text_format: Some(if is_snippet {
            InsertTextFormat::SNIPPET
        } else {
            InsertTextFormat::PLAIN_TEXT
        }),
        text_edit,
        ..lsp_types::CompletionItem::default()
    }
}

fn completion_kind(kind: &PgCompletionItemKind) -> lsp_types::CompletionItemKind {
    match kind {
        PgCompletionItemKind::Function => lsp_types::CompletionItemKind::FUNCTION,
        PgCompletionItemKind::Table | PgCompletionItemKind::Schema => {
            lsp_types::CompletionItemKind::CLASS
        }
        PgCompletionItemKind::Column => lsp_types::CompletionItemKind::FIELD,
        PgCompletionItemKind::Policy | PgCompletionItemKind::Role => {
            lsp_types::CompletionItemKind::CONSTANT
        }
        PgCompletionItemKind::Keyword => lsp_types::CompletionItemKind::KEYWORD,
    }
}

/// The completion menu highlights the first `filter_text.len()` bytes of each
/// item's label — falling back to the typed query's length when `filter_text`
/// is missing (the server never sets it). When that length exceeds the label
/// or splits a multi-byte character, gpui aborts on a char-boundary assertion
/// while rendering the menu. Pin every item's `filter_text` to a prefix of its
/// own label so the highlight is always valid.
fn clamp_filter_text(items: &mut [lsp_types::CompletionItem], query: &str) {
    for item in items {
        let len = item.filter_text.as_deref().unwrap_or(query).len();
        let mut safe = len.min(item.label.len());
        while safe > 0 && !item.label.is_char_boundary(safe) {
            safe -= 1;
        }
        item.filter_text = Some(item.label[..safe].to_string());
    }
}

/// A byte offset into the editor buffer as the workspace's [`TextSize`].
fn to_text_size(offset: usize) -> TextSize {
    TextSize::from(u32::try_from(offset).unwrap_or(u32::MAX))
}

/// Convert a workspace byte range into an LSP line/column range using the
/// document rope.
fn text_range_to_range(range: TextRange, rope: &Rope) -> Range {
    Range {
        start: rope.offset_to_position(usize::from(range.start())),
        end: rope.offset_to_position(usize::from(range.end())),
    }
}

fn workspace_dir() -> Result<std::path::PathBuf> {
    Ok(dirs::cache_dir()
        .ok_or_else(|| anyhow!("no cache directory on this platform"))?
        .join("pg-gui")
        .join("lsp-workspace"))
}

/// Build the workspace configuration: database credentials (so completions are
/// schema-aware) and the formatter casing options. The connection string is
/// decomposed into individual fields; when it does not parse, the connection is
/// disabled and the workspace still parses and lints.
fn build_configuration(
    connection_string: &str,
    keyword_case: CaseStyle,
    constant_case: CaseStyle,
) -> PartialConfiguration {
    let db = connection_string.parse::<postgres::Config>().ok();
    let db_ref = db.as_ref();
    let host = db_ref.and_then(|db| db.get_hosts().first()).map_or_else(
        || "127.0.0.1".to_string(),
        |host| match host {
            postgres::config::Host::Tcp(host) => host.clone(),
            #[cfg(unix)]
            postgres::config::Host::Unix(path) => path.display().to_string(),
        },
    );
    let port = db_ref
        .and_then(|db| db.get_ports().first().copied())
        .unwrap_or(5432);
    let username = db_ref
        .and_then(postgres::Config::get_user)
        .map_or_else(default_user, ToString::to_string);
    let password = db_ref
        .and_then(postgres::Config::get_password)
        .map(|password| String::from_utf8_lossy(password).into_owned())
        .unwrap_or_default();
    let database = db_ref
        .and_then(postgres::Config::get_dbname)
        .map_or_else(|| username.clone(), ToString::to_string);

    let mut config = PartialConfiguration::init();
    config.db = Some(PartialDatabaseConfiguration {
        host: Some(host),
        port: Some(port),
        username: Some(username),
        password: Some(password),
        database: Some(database),
        conn_timeout_secs: Some(10),
        disable_connection: Some(db.is_none()),
        ..PartialDatabaseConfiguration::default()
    });
    config.format = Some(PartialFormatConfiguration {
        enabled: Some(true),
        keyword_case: Some(to_keyword_case(keyword_case)),
        constant_case: Some(to_keyword_case(constant_case)),
        ..PartialFormatConfiguration::default()
    });
    // Typecheck resolves unqualified table names against this list and
    // defaults to just `public`, flagging tables that the connection's real
    // search_path (e.g. set per role or database) would find. Mirror the
    // server's effective search path so diagnostics match query execution.
    if let Some(search_path) = crate::db::search_path(connection_string) {
        config.typecheck = Some(PartialTypecheckConfiguration {
            search_path: Some(search_path.into_iter().collect()),
            ..PartialTypecheckConfiguration::default()
        });
    }
    config
}

/// Record the settings the workspace is configured with. These decide
/// whether completions are schema-aware at all, and they are derived from the
/// connection string rather than given, so the derivation is worth seeing.
/// The password is the one field that never goes in the log.
fn log_configuration(config: &PartialConfiguration, dir: &std::path::Path) {
    let db = config.db.as_ref();
    let search_path = config
        .typecheck
        .as_ref()
        .and_then(|typecheck| typecheck.search_path.as_ref());
    debug!(
        workspace = %dir.display(),
        host = db.and_then(|db| db.host.as_deref()).unwrap_or_default(),
        port = db.and_then(|db| db.port).unwrap_or_default(),
        user = db.and_then(|db| db.username.as_deref()).unwrap_or_default(),
        database = db.and_then(|db| db.database.as_deref()).unwrap_or_default(),
        connection_disabled = db.and_then(|db| db.disable_connection).unwrap_or(false),
        search_path = ?search_path,
        "language server configuration"
    );
}

fn to_keyword_case(case: CaseStyle) -> KeywordCase {
    match case {
        CaseStyle::Lower => KeywordCase::Lower,
        CaseStyle::Upper => KeywordCase::Upper,
    }
}

fn default_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "postgres".to_string())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;
    use std::time::Duration;

    use lsp_types::CompletionItem;

    use super::{
        CaseStyle, Client, CompletionOutcome, DocEvent, SchemaStatus, SchemaStatusReceiver,
        WORKER_STACK, clamp_filter_text, contain_panic, cursor_context, to_text_size,
    };
    use pgls_workspace::features::completions::GetCompletionsParams;
    use pgls_workspace::features::on_hover::OnHoverParams;

    /// Install a stderr subscriber for the `#[ignore]`d probes, so a run with
    /// `PG_GUI_LOG` set shows the same boundary trace the app logs — which is
    /// how the language server is debugged without a window. Stderr only: the
    /// app's log file is not this binary's to rotate. A second call is a
    /// no-op, as is a call in a binary that already has a subscriber.
    fn trace_to_stderr() {
        use tracing_subscriber::filter::Targets;
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        use tracing_subscriber::{Layer as _, fmt};

        let directives = std::env::var("PG_GUI_LOG").unwrap_or_else(|_| "pg_gui=trace".to_string());
        let filter = Targets::from_str(&directives)
            .unwrap_or_else(|_| Targets::new().with_default(tracing::Level::TRACE));
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(filter),
            )
            .try_init()
            .ok();
    }

    /// Run a workspace probe with the stack the document worker gets. These
    /// tests call the workspace directly instead of going through the worker,
    /// and parsing overruns a default thread stack ([`WORKER_STACK`]) — which
    /// aborts the test binary rather than failing a test.
    fn on_worker_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(WORKER_STACK)
            .spawn(f)
            .expect("probe thread starts")
            .join()
            .expect("probe thread does not panic")
    }

    fn item(label: &str, filter_text: Option<&str>) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            filter_text: filter_text.map(ToString::to_string),
            ..CompletionItem::default()
        }
    }

    #[test]
    fn cursor_context_shows_the_text_before_the_cursor() {
        let text = "select * from ord";
        assert_eq!(
            cursor_context(text, to_text_size(text.len())),
            "select * from ord"
        );
        // Newlines would break the one-event-per-line log.
        assert_eq!(cursor_context("a\nb", to_text_size(3)), "a\\nb");
        // A position inside a multi-byte character must not panic the trace.
        assert_eq!(cursor_context("é", to_text_size(1)), "");
    }

    #[test]
    fn clamps_filter_text_to_the_label() {
        // The query is longer than the "for" label: the highlight length must
        // not exceed the label (this aborted the app in the wild).
        let mut items = [item("for", None), item("active", None)];
        clamp_filter_text(&mut items, "active");
        assert_eq!(items[0].filter_text.as_deref(), Some("for"));
        assert_eq!(items[1].filter_text.as_deref(), Some("active"));
    }

    #[test]
    fn clamps_filter_text_to_char_boundaries() {
        let mut items = [item("héllo", None), item("ab", Some("abcdef"))];
        // 2 bytes lands inside the two-byte 'é'; back off to its start.
        clamp_filter_text(&mut items, "xy");
        assert_eq!(items[0].filter_text.as_deref(), Some("h"));
        // An existing filter_text longer than the label is clamped too.
        assert_eq!(items[1].filter_text.as_deref(), Some("ab"));
    }

    /// Wait for a schema status matching `wanted`, up to `attempts` × 100ms.
    fn wait_for_status(
        schema: &mut SchemaStatusReceiver,
        wanted: impl Fn(SchemaStatus) -> bool,
        attempts: usize,
    ) -> Option<SchemaStatus> {
        for _ in 0..attempts {
            match schema.try_recv() {
                Ok(status) if wanted(status) => return Some(status),
                Ok(_) => continue,
                Err(err) if err.is_closed() => return None,
                Err(_) => {}
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    /// A connection string the driver cannot parse leaves the workspace
    /// without a database, so the status bar must say so rather than wait for
    /// a schema that can never load. No database needed.
    #[test]
    fn schema_status_is_disabled_without_a_database() {
        let (client, _diagnostics, mut schema) = Client::start(
            "not a connection string",
            "select 1;\n",
            CaseStyle::Lower,
            CaseStyle::Lower,
        )
        .expect("client starts");
        assert_eq!(
            wait_for_status(&mut schema, |status| status == SchemaStatus::Disabled, 20),
            Some(SchemaStatus::Disabled)
        );
        client.shutdown();
    }

    /// The probe is the only thing that can tell the status bar completions
    /// are schema-aware; with the docker database up it has to reach `Loaded`.
    #[test]
    #[ignore = "requires the docker Postgres on localhost:5433"]
    fn embedded_server_reports_a_loaded_schema() {
        trace_to_stderr();

        let (client, _diagnostics, mut schema) = Client::start(
            "postgres://pgui:pgui@localhost:5433/pgui_test",
            "select 1;\n",
            CaseStyle::Lower,
            CaseStyle::Lower,
        )
        .expect("client starts");
        assert_eq!(
            wait_for_status(&mut schema, |status| status == SchemaStatus::Loaded, 300),
            Some(SchemaStatus::Loaded)
        );
        client.shutdown();
    }

    /// Hover at every byte offset of `text`. Contained panics come back as
    /// errors; only an uncontained panic (or an abort) fails the test.
    fn hover_every_position(client: &Client, text: &str) {
        let workspace = client.inner.workspace.clone();
        let path = client.inner.path.clone();
        let len = text.len();
        on_worker_stack(move || {
            for position in 0..len {
                let _ = contain_panic(|| {
                    workspace.on_hover(OnHoverParams {
                        path: path.clone(),
                        position: to_text_size(position),
                    })
                });
            }
        });
    }

    /// Hovering a buffer holding snippet tab-stop markers must never take
    /// the app down: `pgls_treesitter`'s scope tracker panics on some such
    /// inputs (SIGABRT'd the app in the wild on 2026-07-12), and
    /// [`contain_panic`] — used by every provider call — has to absorb it.
    /// No database needed: hover fails soft when the DB is unreachable.
    #[test]
    fn hover_survives_snippet_tab_stop_markers() {
        let text = "CREATE SEQUENCE ${1:sequence_name}\n    START WITH ${2:1}\n    INCREMENT BY ${3:1};invoice_seqCREATE SEQUENCE 100\n    START WITH 1\n    INCREMENT BY ${3:1};invoice_seqcreate table\n";
        let (client, _diagnostics, _schema) = Client::start(
            "postgres://nobody:nope@127.0.0.1:1/none",
            text,
            CaseStyle::Lower,
            CaseStyle::Lower,
        )
        .expect("client starts without a database");

        hover_every_position(&client, text);
        client.shutdown();
    }

    /// The same probe with a real database: the workspace only builds the
    /// tree-sitter hover context (where the panic lives) after loading the
    /// schema cache, so the panic path needs a reachable Postgres.
    #[test]
    #[ignore = "requires the docker Postgres on localhost:5433"]
    fn embedded_server_hover_survives_snippet_markers() {
        let text = "CREATE SEQUENCE ${1:sequence_name}\n    START WITH ${2:1}\n    INCREMENT BY ${3:1};invoice_seqCREATE SEQUENCE 100\n    START WITH 1\n    INCREMENT BY ${3:1};invoice_seqcreate table\n";
        let (client, _diagnostics, _schema) = Client::start(
            "postgres://pgui:pgui@localhost:5433/pgui_test",
            text,
            CaseStyle::Lower,
            CaseStyle::Lower,
        )
        .expect("client starts");

        hover_every_position(&client, text);
        client.shutdown();
    }

    /// Diagnostics must resolve unqualified table names through the role's
    /// `search_path` (`ALTER ROLE pgui SET search_path TO app, public` in the
    /// docker seed), not just `public`: `feature_flags` lives in `app` and
    /// must not be flagged, while a genuinely missing table still is.
    #[test]
    #[ignore = "requires the docker Postgres on localhost:5433"]
    fn embedded_server_resolves_search_path_schemas() {
        const CONN: &str = "postgres://pgui:pgui@localhost:5433/pgui_test";

        trace_to_stderr();

        let (client, mut diagnostics, _schema) = Client::start(
            CONN,
            "SELECT * FROM feature_flags;\nSELECT * FROM no_such_table_xyz;\n",
            CaseStyle::Lower,
            CaseStyle::Lower,
        )
        .expect("client starts");

        // The initial batch includes db-backed typecheck results; loading
        // the schema cache can take a while on the first run.
        let mut batch = None;
        for _ in 0..300 {
            match diagnostics.try_recv() {
                Ok(diags) => {
                    batch = Some(diags);
                    break;
                }
                Err(err) if err.is_closed() => break,
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        let batch = batch.expect("diagnostics are published for the opened document");

        assert!(
            !batch.iter().any(|d| d.message.contains("feature_flags")),
            "search_path table must not be flagged, got {batch:?}"
        );
        assert!(
            batch
                .iter()
                .any(|d| d.message.contains("no_such_table_xyz")),
            "missing table must still be flagged, got {batch:?}"
        );

        client.shutdown();
    }

    /// End-to-end check of the embedded language server against the local
    /// Docker Postgres (`docker compose up -d`). Ignored by default because it
    /// needs the database; run with:
    /// `cargo test --  --ignored embedded_server`.
    /// The completion path the editor actually drives: through the document
    /// worker, which syncs the buffer first. The direct-workspace probe below
    /// bypasses it, so it cannot catch an ordering or sync fault — and it is
    /// the worker that logs the per-kind tally a missing-tables report needs.
    #[test]
    #[ignore = "requires the docker Postgres on localhost:5433"]
    fn embedded_server_completions_through_the_worker() {
        const CONN: &str = "postgres://pgui:pgui@localhost:5433/pgui_test";

        trace_to_stderr();

        let text = "SELECT * FROM o";
        let (client, _diagnostics, _schema) =
            Client::start(CONN, text, CaseStyle::Lower, CaseStyle::Lower).expect("client starts");

        let (reply, reply_rx) = futures::channel::oneshot::channel();
        client
            .inner
            .doc_tx
            .send(DocEvent::Completions {
                text: text.to_string(),
                position: to_text_size(text.len()),
                reply,
            })
            .expect("the worker takes the request");
        let labels: Vec<String> = match futures::executor::block_on(reply_rx)
            .expect("the worker replies")
        {
            CompletionOutcome::Items(items) => items.into_iter().map(|item| item.label).collect(),
            CompletionOutcome::DatabaseOffline => panic!("the workspace reported no database"),
            CompletionOutcome::Failed(err) => panic!("completions failed: {err}"),
        };
        assert!(
            labels.iter().any(|label| label == "orders"),
            "expected `orders` in completions, got {labels:?}"
        );

        client.shutdown();
    }

    #[test]
    #[ignore = "requires the docker Postgres on localhost:5433"]
    fn embedded_server_completions_diagnostics_and_formatting() {
        const CONN: &str = "postgres://pgui:pgui@localhost:5433/pgui_test";

        trace_to_stderr();

        let (client, mut diagnostics, _schema) =
            Client::start(CONN, "SELECT * FROM o", CaseStyle::Upper, CaseStyle::Upper)
                .expect("client starts");

        // Schema-aware completions: the public `orders` table is offered for
        // the `o` prefix, which only works if the schema cache loaded from the
        // database.
        let workspace = client.inner.workspace.clone();
        let path = client.inner.path.clone();
        let completions = on_worker_stack(move || {
            workspace.get_completions(GetCompletionsParams {
                path,
                position: to_text_size("SELECT * FROM o".len()),
            })
        })
        .expect("completions");
        let labels: Vec<String> = completions.into_iter().map(|item| item.label).collect();
        assert!(
            labels.iter().any(|label| label == "orders"),
            "expected `orders` in completions, got {labels:?}"
        );

        // Diagnostics: a syntax error reaches the receiver.
        client.document_changed("SELCT 1;\n".to_string());
        let mut diags = Vec::new();
        for _ in 0..50 {
            match diagnostics.try_recv() {
                Ok(batch) if !batch.is_empty() => {
                    diags = batch;
                    break;
                }
                Ok(_) => {}
                Err(err) if err.is_closed() => break,
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        assert!(!diags.is_empty(), "expected diagnostics for a syntax error");

        // Formatting applies the configured (upper) keyword casing.
        client.document_changed("select 1;\n".to_string());
        let formatted = futures::executor::block_on(client.format("select 1;\n"))
            .expect("format request succeeds")
            .expect("formatting changed the text");
        assert!(
            formatted.contains("SELECT"),
            "expected uppercased keyword, got {formatted:?}"
        );

        client.shutdown();
    }
}
