//! SQL language support for the editor, backed by the Postgres Language
//! Server (`postgrestools lsp-proxy`, <https://pg-language-server.com>).
//!
//! This is a minimal LSP client over stdio. Completions, hover and
//! diagnostics are plugged into the `gpui-component` editor through its
//! provider traits. The server reads its database credentials from a
//! generated `postgres-language-server.jsonc` in a private workspace
//! directory, which is what makes completions schema-aware.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context as _, Result, anyhow, bail};
use futures::channel::{mpsc, oneshot};
use gpui::{App, AppContext as _, Context, Task, Window};
use gpui_component::input::{CompletionProvider, HoverProvider, InputState, Rope, RopeExt as _};
use lsp_types::{
    ClientCapabilities, CompletionContext, CompletionParams, CompletionResponse,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, Hover, HoverParams, InitializeParams,
    PartialResultParams, PublishDiagnosticsParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Uri,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams, WorkspaceFolder,
};
use serde_json::{Value, json};

const BINARY: &str = "postgrestools";

/// Diagnostics published by the server for the editor document.
pub type DiagnosticsReceiver = mpsc::UnboundedReceiver<Vec<lsp_types::Diagnostic>>;

/// A handle to a running language server. Cloning is cheap; the server
/// process is killed once the last clone is dropped.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    connection_string: String,
    uri: Uri,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    version: AtomicI32,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

impl Client {
    /// Spawn `postgrestools lsp-proxy`, run the LSP handshake and open the
    /// editor buffer as a document. Blocks until the server has answered
    /// `initialize`, so call it from a background thread.
    ///
    /// # Errors
    ///
    /// Fails when the binary is not installed, the workspace directory
    /// cannot be prepared, or the server misbehaves during the handshake.
    pub fn start(connection_string: &str, text: &str) -> Result<(Self, DiagnosticsReceiver)> {
        let workspace = workspace_dir()?;
        std::fs::create_dir_all(&workspace)?;
        write_server_config(&workspace, connection_string)?;
        let scratch = workspace.join("scratch.sql");
        std::fs::write(&scratch, text)?;

        let mut child = Command::new(BINARY)
            .arg("lsp-proxy")
            .current_dir(&workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| {
                format!("failed to launch `{BINARY} lsp-proxy` (install it with `brew install {BINARY}`)")
            })?;
        let stdin = child.stdin.take().context("language server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("language server has no stdout")?;

        let inner = Arc::new(Inner {
            connection_string: connection_string.to_string(),
            uri: file_uri(&scratch)?,
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            version: AtomicI32::new(0),
        });

        let (diagnostics_tx, diagnostics_rx) = mpsc::unbounded();
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("pg-lsp-reader".into())
            .spawn(move || reader_loop(BufReader::new(stdout), &weak, &diagnostics_tx))?;

        let client = Self { inner };
        client.initialize(&workspace, text)?;
        Ok((client, diagnostics_rx))
    }

    fn initialize(&self, workspace: &Path, text: &str) -> Result<()> {
        let root = file_uri(workspace)?;
        // root_uri is deprecated in the LSP spec in favor of workspace
        // folders, but postgrestools resolves its configuration through it.
        #[allow(deprecated)]
        let params = InitializeParams {
            root_uri: Some(root.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root,
                name: "pg-gui".into(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..InitializeParams::default()
        };

        let request = self
            .inner
            .request("initialize", &serde_json::to_value(params)?);
        futures::executor::block_on(request)
            .map_err(|_| anyhow!("language server exited during initialization"))?
            .map_err(|err| anyhow!("initialize failed: {err}"))?;

        self.inner.notify("initialized", &json!({}))?;
        self.inner.notify(
            "textDocument/didOpen",
            &serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: self.inner.uri.clone(),
                    language_id: "sql".into(),
                    version: 0,
                    text: text.to_string(),
                },
            })?,
        )
    }

    /// The connection string the server was configured with at startup.
    #[must_use]
    pub fn connection_string(&self) -> &str {
        &self.inner.connection_string
    }

    /// Tell the server the editor buffer changed (full-text sync).
    pub fn document_changed(&self, text: String) {
        let version = self.inner.version.fetch_add(1, Ordering::Relaxed) + 1;
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: self.inner.uri.clone(),
                version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text,
            }],
        };
        if let Ok(params) = serde_json::to_value(params) {
            self.inner.notify("textDocument/didChange", &params).ok();
        }
    }

    /// Stop the server process.
    pub fn shutdown(&self) {
        self.inner.notify("exit", &Value::Null).ok();
        if let Ok(mut child) = self.inner.child.lock() {
            child.kill().ok();
            child.wait().ok();
        }
    }

    fn text_document_position(&self, text: &Rope, offset: usize) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: self.inner.uri.clone(),
            },
            position: text.offset_to_position(offset),
        }
    }
}

impl Inner {
    fn send(&self, message: &Value) -> Result<()> {
        let body = serde_json::to_string(message)?;
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| anyhow!("language server stdin poisoned"))?;
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len())?;
        stdin.flush()?;
        Ok(())
    }

    fn notify(&self, method: &str, params: &Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn request(&self, method: &str, params: &Value) -> oneshot::Receiver<Result<Value, String>> {
        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, tx);
        }
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(err) = self.send(&message)
            && let Ok(mut pending) = self.pending.lock()
            && let Some(tx) = pending.remove(&id)
        {
            tx.send(Err(err.to_string())).ok();
        }
        rx
    }
}

fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("language server closed its stdout");
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let length = content_length.context("message without a Content-Length header")?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

fn reader_loop(
    mut reader: BufReader<ChildStdout>,
    inner: &Weak<Inner>,
    diagnostics_tx: &mpsc::UnboundedSender<Vec<lsp_types::Diagnostic>>,
) {
    while let Ok(message) = read_message(&mut reader) {
        let Some(inner) = inner.upgrade() else { return };
        match (
            message.get("method").and_then(Value::as_str),
            message.get("id"),
        ) {
            // Server-to-client request: answer with an empty result so the
            // server never blocks waiting on capabilities we don't have.
            (Some(method), Some(id)) => {
                let result = if method == "workspace/configuration" {
                    let items = message
                        .pointer("/params/items")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    Value::Array(vec![Value::Null; items])
                } else {
                    Value::Null
                };
                inner
                    .send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
                    .ok();
            }
            (Some("textDocument/publishDiagnostics"), None) => {
                let Some(params) = message.get("params") else {
                    continue;
                };
                if let Ok(params) =
                    serde_json::from_value::<PublishDiagnosticsParams>(params.clone())
                    && params.uri == inner.uri
                {
                    diagnostics_tx.unbounded_send(params.diagnostics).ok();
                }
            }
            (None, Some(id)) => {
                let Some(id) = id.as_u64() else { continue };
                let Some(tx) = inner
                    .pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(&id))
                else {
                    continue;
                };
                let result = match message.get("error") {
                    Some(error) => Err(error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown language server error")
                        .to_string()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                tx.send(result).ok();
            }
            // Notifications we don't care about.
            (_, None) => {}
        }
    }
    // The server is gone; fail whatever is still waiting on it.
    if let Some(inner) = inner.upgrade()
        && let Ok(mut pending) = inner.pending.lock()
    {
        for (_, tx) in pending.drain() {
            tx.send(Err("language server exited".into())).ok();
        }
    }
}

async fn await_response(request: oneshot::Receiver<Result<Value, String>>) -> Result<Value> {
    request
        .await
        .map_err(|_| anyhow!("language server exited"))?
        .map_err(anyhow::Error::msg)
}

/// Bridges the language server into the editor's LSP provider traits.
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
        let params = CompletionParams {
            text_document_position: self.client.text_document_position(text, offset),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: Some(trigger),
        };
        let request = match serde_json::to_value(params) {
            Ok(params) => self
                .client
                .inner
                .request("textDocument/completion", &params),
            Err(err) => return Task::ready(Err(err.into())),
        };
        cx.background_spawn(async move {
            let response = await_response(request).await?;
            if response.is_null() {
                return Ok(CompletionResponse::Array(vec![]));
            }
            Ok(serde_json::from_value(response)?)
        })
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        _: &mut Context<InputState>,
    ) -> bool {
        // Word characters continue an existing completion; the rest are the
        // trigger characters postgrestools declares in its capabilities.
        new_text
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | '"' | '(' | ' '))
    }
}

impl HoverProvider for Provider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let params = HoverParams {
            text_document_position_params: self.client.text_document_position(text, offset),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let request = match serde_json::to_value(params) {
            Ok(params) => self.client.inner.request("textDocument/hover", &params),
            Err(err) => return Task::ready(Err(err.into())),
        };
        cx.background_spawn(async move {
            let response = await_response(request).await?;
            Ok(serde_json::from_value(response)?)
        })
    }
}

fn workspace_dir() -> Result<PathBuf> {
    Ok(dirs::cache_dir()
        .context("no cache directory on this platform")?
        .join("pg-gui")
        .join("lsp-workspace"))
}

/// Write the server configuration next to the scratch document. The server
/// picks up the `db` credentials from here; without them it still parses
/// and lints, but completions are no longer schema-aware.
fn write_server_config(workspace: &Path, connection_string: &str) -> Result<()> {
    let db = connection_string.parse::<postgres::Config>().ok();
    let db = db.as_ref();
    let host = db.and_then(|db| db.get_hosts().first()).map_or_else(
        || "127.0.0.1".to_string(),
        |host| match host {
            postgres::config::Host::Tcp(host) => host.clone(),
            #[cfg(unix)]
            postgres::config::Host::Unix(path) => path.display().to_string(),
        },
    );
    let port = db
        .and_then(|db| db.get_ports().first().copied())
        .unwrap_or(5432);
    let username = db
        .and_then(postgres::Config::get_user)
        .map_or_else(default_user, ToString::to_string);
    let password = db
        .and_then(postgres::Config::get_password)
        .map(|password| String::from_utf8_lossy(password).into_owned())
        .unwrap_or_default();
    let database = db
        .and_then(postgres::Config::get_dbname)
        .map_or_else(|| username.clone(), ToString::to_string);

    let config = json!({
        "$schema": "https://pg-language-server.com/latest/schema.json",
        "db": {
            "host": host,
            "port": port,
            "username": username,
            "password": password,
            "database": database,
            "connTimeoutSecs": 10,
            "disableConnection": db.is_none(),
        },
        "linter": { "enabled": true },
    });
    std::fs::write(
        workspace.join("postgres-language-server.jsonc"),
        serde_json::to_string_pretty(&config)?,
    )?;
    Ok(())
}

fn default_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "postgres".to_string())
}

/// Build a `file://` URI, percent-encoding anything outside the unreserved
/// set (macOS config paths contain spaces, for example).
fn file_uri(path: &Path) -> Result<Uri> {
    let mut uri = String::from("file://");
    for &byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(byte));
            }
            _ => {
                let _ = write!(uri, "%{byte:02X}");
            }
        }
    }
    Uri::from_str(&uri).map_err(|err| anyhow!("cannot express {} as a URI: {err}", path.display()))
}
