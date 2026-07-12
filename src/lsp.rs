//! SQL language support for the editor, backed by the Postgres Language
//! Server (<https://pg-language-server.com>) used as a library.
//!
//! Instead of spawning `postgrestools lsp-proxy` and talking LSP over stdio,
//! we embed the server's `pgls_workspace` crate directly and call its
//! [`Workspace`] trait. Completions, hover and diagnostics are plugged into
//! the `gpui-component` editor through its provider traits. The database
//! credentials come from the workspace settings we push at startup, which is
//! what makes completions schema-aware.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, RecvTimeoutError};
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::channel::{mpsc, oneshot};
use gpui::{App, AppContext as _, Context, Task, Window};
use gpui_component::input::{CompletionProvider, HoverProvider, InputState, Rope, RopeExt as _};
use lsp_types::{
    CompletionContext, CompletionItemLabelDetails, CompletionResponse, CompletionTextEdit,
    DiagnosticSeverity, Hover, HoverContents, InsertTextFormat, MarkedString, NumberOrString,
    Range, TextEdit,
};

use pgls_analyse::RuleCategories;
use pgls_completions::CompletionItemKind as PgCompletionItemKind;
use pgls_configuration::PartialConfiguration;
use pgls_configuration::database::PartialDatabaseConfiguration;
use pgls_configuration::format::{KeywordCase, PartialFormatConfiguration};
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

use crate::config::CaseStyle;

/// How long a burst of edits is allowed to settle before we re-run the
/// (potentially database-touching) diagnostics analysis.
const DEBOUNCE: Duration = Duration::from_millis(300);

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
        anyhow!("language server panicked: {message}")
    })
}

/// Diagnostics computed for the editor document.
pub type DiagnosticsReceiver = mpsc::UnboundedReceiver<Vec<lsp_types::Diagnostic>>;

/// A handle to an embedded language-server workspace. Cloning is cheap; the
/// workspace and its background diagnostics worker are torn down once the last
/// clone is dropped.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    workspace: Arc<dyn Workspace>,
    path: PgLSPath,
    connection_string: String,
    keyword_case: CaseStyle,
    constant_case: CaseStyle,
    version: AtomicI32,
    /// Wakes the diagnostics worker after the document changed. Dropping it
    /// (with the last [`Client`] clone) signals the worker to exit.
    diag_signal: std::sync::mpsc::Sender<()>,
}

impl Client {
    /// Create an in-process workspace, configure it with the database
    /// credentials and formatter options, and open the editor buffer as a
    /// document. Loading the schema cache happens lazily on the first
    /// completion/diagnostic, so call this from a background thread.
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
    ) -> Result<(Self, DiagnosticsReceiver)> {
        contain_panic(|| Self::start_inner(connection_string, text, keyword_case, constant_case))?
    }

    fn start_inner(
        connection_string: &str,
        text: &str,
        keyword_case: CaseStyle,
        constant_case: CaseStyle,
    ) -> Result<(Self, DiagnosticsReceiver)> {
        let workspace = pgls_workspace::workspace::server_sync();
        let dir = workspace_dir()?;
        std::fs::create_dir_all(&dir).ok();
        let path = PgLSPath::new(dir.join("scratch.sql"));

        workspace
            .register_project_folder(RegisterProjectFolderParams {
                path: Some(dir.clone()),
                set_as_current_workspace: true,
            })
            .map_err(|err| anyhow!("failed to register language server project: {err}"))?;

        workspace
            .update_settings(UpdateSettingsParams {
                configuration: build_configuration(connection_string, keyword_case, constant_case),
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
        let (signal_tx, signal_rx) = std::sync::mpsc::channel();

        let worker_workspace = workspace.clone();
        let worker_path = path.clone();
        std::thread::Builder::new()
            .name("pg-lsp-diagnostics".into())
            .spawn(move || {
                diagnostics_worker(&worker_workspace, &worker_path, &signal_rx, &diagnostics_tx);
            })?;
        // Publish an initial set for the freshly opened document.
        signal_tx.send(()).ok();

        let inner = Arc::new(Inner {
            workspace,
            path,
            connection_string: connection_string.to_string(),
            keyword_case,
            constant_case,
            version: AtomicI32::new(0),
            diag_signal: signal_tx,
        });
        Ok((Self { inner }, diagnostics_rx))
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
    /// schedule a fresh diagnostics run.
    pub fn document_changed(&self, text: String) {
        let version = self.inner.version.fetch_add(1, Ordering::Relaxed) + 1;
        contain_panic(|| {
            self.inner
                .workspace
                .change_file(ChangeFileParams {
                    path: self.inner.path.clone(),
                    version,
                    content: text,
                })
                .ok();
        })
        .ok();
        self.inner.diag_signal.send(()).ok();
    }

    /// Format the whole document. The workspace formats its own copy (kept in
    /// sync via [`Self::document_changed`]); `text` is used only to decide
    /// whether anything changed. `None` when there is nothing to change —
    /// including when formatting is unavailable or the document does not
    /// parse. Runs on a dedicated thread so the caller's executor is not
    /// blocked.
    ///
    /// # Errors
    ///
    /// Fails when the workspace rejects the request.
    pub async fn format(&self, text: &str) -> Result<Option<String>> {
        let workspace = self.inner.workspace.clone();
        let path = self.inner.path.clone();
        let text = text.to_string();
        let (tx, rx) = oneshot::channel();
        std::thread::spawn(move || {
            tx.send(format_document(&workspace, &path, &text)).ok();
        });
        rx.await
            .map_err(|_| anyhow!("formatting task was cancelled"))?
    }

    /// Close the workspace document. The background worker stops once the last
    /// [`Client`] clone is dropped.
    pub fn shutdown(&self) {
        contain_panic(|| {
            self.inner
                .workspace
                .close_file(CloseFileParams {
                    path: self.inner.path.clone(),
                })
                .ok();
        })
        .ok();
    }
}

/// Blocks on the workspace waiting for change signals, coalescing bursts and
/// republishing diagnostics after each settled edit. Returns when the signal
/// channel is dropped (i.e. the last [`Client`] is gone).
fn diagnostics_worker(
    workspace: &Arc<dyn Workspace>,
    path: &PgLSPath,
    signal: &StdReceiver<()>,
    diagnostics_tx: &mpsc::UnboundedSender<Vec<lsp_types::Diagnostic>>,
) {
    while signal.recv().is_ok() {
        // Coalesce a rapid-fire burst of edits into a single analysis run.
        loop {
            match signal.recv_timeout(DEBOUNCE) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        // A contained panic publishes nothing and keeps the worker alive
        // for the next edit.
        let diagnostics = contain_panic(|| pull_diagnostics(workspace, path)).unwrap_or_default();
        if diagnostics_tx.unbounded_send(diagnostics).is_err() {
            return;
        }
    }
}

fn pull_diagnostics(workspace: &Arc<dyn Workspace>, path: &PgLSPath) -> Vec<lsp_types::Diagnostic> {
    let Ok(content) = workspace.get_file_content(GetFileContentParams { path: path.clone() })
    else {
        return Vec::new();
    };
    let rope = Rope::from(content.as_str());
    let result = workspace.pull_file_diagnostics(PullFileDiagnosticsParams {
        path: path.clone(),
        categories: RuleCategories::all(),
        max_diagnostics: u32::MAX,
        only: Vec::new(),
        skip: Vec::new(),
    });
    let Ok(result) = result else {
        return Vec::new();
    };
    result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic_to_lsp(diagnostic, &rope))
        .collect()
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
        cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        // gpui-component smuggles the query typed so far in here; keep it for
        // clamping the items' filter_text below.
        let query = trigger.trigger_character.unwrap_or_default();
        let position = to_text_size(offset);
        let workspace = self.client.inner.workspace.clone();
        let path = self.client.inner.path.clone();
        let rope = text.clone();
        let snippets = snippet_items(text, offset);
        cx.background_spawn(async move {
            let result = contain_panic(|| {
                workspace.get_completions(GetCompletionsParams { path, position })
            })?;
            let result = match result {
                Ok(result) => result,
                // The database is unreachable; offer the snippets alone
                // rather than error.
                Err(WorkspaceError::DatabaseConnectionError(_)) => {
                    return Ok(CompletionResponse::Array(snippets));
                }
                Err(err) => return Err(anyhow!("completion request failed: {err}")),
            };
            let mut items: Vec<lsp_types::CompletionItem> = snippets;
            items.extend(
                result
                    .into_iter()
                    .map(|item| completion_to_lsp(item, &rope)),
            );
            clamp_filter_text(&mut items, &query);
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        _: &mut Context<InputState>,
    ) -> bool {
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
        _: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let query = trigger.trigger_character.unwrap_or_default();
        let mut items = snippet_items(text, offset);
        clamp_filter_text(&mut items, &query);
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        _: &mut Context<InputState>,
    ) -> bool {
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
        _text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let position = to_text_size(offset);
        let workspace = self.client.inner.workspace.clone();
        let path = self.client.inner.path.clone();
        cx.background_spawn(async move {
            let result = contain_panic(|| workspace.on_hover(OnHoverParams { path, position }))?;
            let result = match result {
                Ok(result) => result,
                Err(WorkspaceError::DatabaseConnectionError(_)) => return Ok(None),
                Err(err) => return Err(anyhow!("hover request failed: {err}")),
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
    config
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
    use std::time::Duration;

    use lsp_types::CompletionItem;

    use super::{CaseStyle, Client, clamp_filter_text, contain_panic, to_text_size};
    use pgls_workspace::features::completions::GetCompletionsParams;
    use pgls_workspace::features::on_hover::OnHoverParams;

    fn item(label: &str, filter_text: Option<&str>) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            filter_text: filter_text.map(ToString::to_string),
            ..CompletionItem::default()
        }
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

    /// Hovering a buffer holding snippet tab-stop markers must never take
    /// the app down: `pgls_treesitter`'s scope tracker panics on some such
    /// inputs (SIGABRT'd the app in the wild on 2026-07-12), and
    /// [`contain_panic`] — used by every provider call — has to absorb it.
    /// No database needed: hover fails soft when the DB is unreachable.
    #[test]
    fn hover_survives_snippet_tab_stop_markers() {
        let text = "CREATE SEQUENCE ${1:sequence_name}\n    START WITH ${2:1}\n    INCREMENT BY ${3:1};invoice_seqCREATE SEQUENCE 100\n    START WITH 1\n    INCREMENT BY ${3:1};invoice_seqcreate table\n";
        let (client, _diagnostics) = Client::start(
            "postgres://nobody:nope@127.0.0.1:1/none",
            text,
            CaseStyle::Lower,
            CaseStyle::Lower,
        )
        .expect("client starts without a database");

        for position in 0..text.len() {
            // Contained panics come back as errors; only an uncontained
            // panic (or abort) can fail this test.
            let _ = contain_panic(|| {
                client.inner.workspace.on_hover(OnHoverParams {
                    path: client.inner.path.clone(),
                    position: to_text_size(position),
                })
            });
        }
        client.shutdown();
    }

    /// The same probe with a real database: the workspace only builds the
    /// tree-sitter hover context (where the panic lives) after loading the
    /// schema cache, so the panic path needs a reachable Postgres.
    #[test]
    #[ignore = "requires the docker Postgres on localhost:5433"]
    fn embedded_server_hover_survives_snippet_markers() {
        let text = "CREATE SEQUENCE ${1:sequence_name}\n    START WITH ${2:1}\n    INCREMENT BY ${3:1};invoice_seqCREATE SEQUENCE 100\n    START WITH 1\n    INCREMENT BY ${3:1};invoice_seqcreate table\n";
        let (client, _diagnostics) = Client::start(
            "postgres://pgui:pgui@localhost:5433/pgui_test",
            text,
            CaseStyle::Lower,
            CaseStyle::Lower,
        )
        .expect("client starts");

        for position in 0..text.len() {
            let _ = contain_panic(|| {
                client.inner.workspace.on_hover(OnHoverParams {
                    path: client.inner.path.clone(),
                    position: to_text_size(position),
                })
            });
        }
        client.shutdown();
    }

    /// End-to-end check of the embedded language server against the local
    /// Docker Postgres (`docker compose up -d`). Ignored by default because it
    /// needs the database; run with:
    /// `cargo test --  --ignored embedded_server`.
    #[test]
    #[ignore = "requires the docker Postgres on localhost:5433"]
    fn embedded_server_completions_diagnostics_and_formatting() {
        const CONN: &str = "postgres://pgui:pgui@localhost:5433/pgui_test";

        let (client, mut diagnostics) =
            Client::start(CONN, "SELECT * FROM o", CaseStyle::Upper, CaseStyle::Upper)
                .expect("client starts");

        // Schema-aware completions: the public `orders` table is offered for
        // the `o` prefix, which only works if the schema cache loaded from the
        // database.
        let completions = client
            .inner
            .workspace
            .get_completions(GetCompletionsParams {
                path: client.inner.path.clone(),
                position: to_text_size("SELECT * FROM o".len()),
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
