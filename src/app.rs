use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gpui::Subscription;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, ScrollHandle, SharedString, Styled as _, Window, div, px,
};
use gpui_base::TextSelectionHandle;
use gpui_component::{
    ActiveTheme as _, Theme, ThemeMode,
    combobox::ComboboxState,
    input::{EditorState, InputEvent, InputState},
    resizable::ResizableState,
    searchable_list::SearchableVec,
    table::TableState,
    tree::{TreeEvent, TreeItem, TreeState},
    v_flex,
};
// Linux and Windows have no OS-native menu bar, so an in-window one is drawn
// in the title bar. macOS uses the real menu bar and skips all of this.
#[cfg(not(target_os = "macos"))]
use gpui_component::menu::AppMenuBar;

use crate::results::ResultsDelegate;
use crate::{config, db_tree, debug, definitions, file_tree, lsp, snippets, statement};

mod connection;
mod debugger;
mod editing;
mod file_ops;
mod menus;
mod objects;
mod output;
mod queries;
mod tab;
mod ui;
mod view;

use connection::ConnectionItem;
use tab::{EditorTab, ParkedResults};

fn default_conn() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "postgres".to_string());
    format!("postgres://{user}@localhost:5432/postgres")
}

/// Number of connections kept in the Recent menu.
const MAX_RECENT_CONNECTIONS: usize = 10;

/// Move the connection to the front of the recent list (capped), ignoring an
/// empty url. Entries are deduped by [`config::connection_key`], not by url,
/// so pointing a saved connection at another server updates its entry in
/// place instead of adding a second one next to it. `replacing` is the key
/// the edit started from, dropped as well so a rename leaves no entry behind
/// under the old name. A non-empty `name` labels the entry; an empty one
/// keeps whatever name the url was already recorded under, so simply
/// reconnecting doesn't erase a saved name. Returns the key the connection is
/// now filed under (empty for an empty url).
fn record_recent(
    recents: &mut Vec<config::RecentConnection>,
    url: &str,
    name: &str,
    replacing: Option<&str>,
) -> String {
    if url.is_empty() {
        return String::new();
    }
    let name = if name.is_empty() {
        replacing
            .and_then(|key| recents.iter().find(|c| c.key() == key))
            .or_else(|| recents.iter().find(|c| c.url == url))
            .map_or(String::new(), |c| c.name.clone())
    } else {
        name.to_string()
    };
    let key = config::connection_key(&name, url).to_string();
    // An unnamed entry says nothing the url doesn't, so a named connection
    // taking that url over absorbs it rather than sitting beside it.
    recents.retain(|c| {
        c.key() != key && replacing != Some(c.key()) && !(c.name.is_empty() && c.url == url)
    });
    recents.insert(
        0,
        config::RecentConnection {
            name,
            url: url.to_string(),
        },
    );
    recents.truncate(MAX_RECENT_CONNECTIONS);
    key
}

/// Number of folders kept in the File ▸ Open Recent Folder menu.
const MAX_RECENT_FOLDERS: usize = 10;

/// Move `path` to the front of the recent-folders list (dedup by path,
/// capped).
fn record_recent_folder(recents: &mut Vec<PathBuf>, path: &Path) {
    recents.retain(|dir| dir != path);
    recents.insert(0, path.to_path_buf());
    recents.truncate(MAX_RECENT_FOLDERS);
}

/// Point the global theme at the configured selection: a fixed light/dark
/// mode, or whatever the OS appearance currently is.
fn apply_theme_selection(theme: config::ThemeSelection, window: &mut Window, cx: &mut App) {
    match theme {
        config::ThemeSelection::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        config::ThemeSelection::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        config::ThemeSelection::System => Theme::sync_system_appearance(Some(window), cx),
    }
}

/// Strip `--` line comments and `/* */` block comments from SQL, so the words
/// before a routine definition can be scanned without prose interfering.
fn strip_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '-' if chars.peek() == Some(&'-') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A file's last modification time, or `None` when it's missing or can't be
/// stat'd. Used to notice when a tab's file was edited outside the app.
fn file_mtime(path: &Path) -> Option<SystemTime> {
    path.metadata().ok()?.modified().ok()
}

/// Replace an editor's whole buffer, keeping the gutter's origin in step.
/// [`InputState::set_value`] deliberately emits no `Change` event, so the
/// body-relative line numbering that [`PgGui::on_editor_event`] recomputes on
/// every edit has to be recomputed here instead — otherwise a routine opened
/// into an existing buffer keeps plain file numbering until the first keypress.
fn set_editor_value(
    state: &mut EditorState,
    text: String,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) {
    let offset = statement::body_line_offset(&text);
    state.set_value(text, window, cx);
    state.set_line_number_offset(offset, cx);
}

/// Where a file dialog (Open, Save As, Export) should start: the directory
/// the last dialog picked a file in, then the active tab's file's directory,
/// then home — the first of those that still exists on disk.
fn dialog_start_dir(last_dir: Option<&Path>, tab_file: Option<&Path>) -> PathBuf {
    last_dir
        .filter(|dir| dir.is_dir())
        .or_else(|| tab_file.and_then(Path::parent).filter(|dir| dir.is_dir()))
        .map_or_else(
            || dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
            Path::to_path_buf,
        )
}

/// Replace the username and password in a connection string with stars, for
/// display. Handles both URL (`postgres://user:pass@host/db`) and
/// key-value (`host=… user=… password=…`) forms.
fn mask_credentials(conn: &str) -> String {
    if let Some(scheme_end) = conn.find("://") {
        let auth_start = scheme_end + 3;
        let authority_end = conn[auth_start..]
            .find(['/', '?', '#'])
            .map_or(conn.len(), |i| auth_start + i);
        if let Some(at) = conn[auth_start..authority_end].rfind('@') {
            let stars = if conn[auth_start..auth_start + at].contains(':') {
                "****:****"
            } else {
                "****"
            };
            return format!("{}{stars}{}", &conn[..auth_start], &conn[auth_start + at..]);
        }
        return conn.to_string();
    }
    conn.split_whitespace()
        .map(|pair| match pair.split_once('=') {
            Some((key @ ("user" | "password"), _)) => format!("{key}=****"),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Percent-decode the reserved characters we encode in [`ConnectionParts::to_url`];
/// leaves any other `%`-sequence (or a lone `%`) untouched.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // `get` rather than a slice: the two bytes after a `%` may run past
        // the end, or into the middle of a multi-byte character (`%aé`),
        // either of which would panic on `&s[..]`.
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode the characters that would otherwise be read as URL
/// delimiters, so a username/password/database containing `@`, `:`, `/`,
/// etc. round-trips through the connection string.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' | ':' | '@' | '/' | '?' | '#' | '[' | ']' | ' ' => {
                // Writing to a String cannot fail.
                let _ = write!(out, "%{:02X}", ch as u8);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Which diagnostic cmd-j / cmd-k lands on, given each entry's start
/// offset and whether it is an error, sorted by start the way
/// `DiagnosticSet` keeps them. Errors come first — while there is any, the
/// warnings and hints between them are skipped — and the walk wraps around
/// at either end. The cursor is left on a diagnostic's start, so "after"
/// and "before" are strict: the next press moves on rather than landing on
/// the same one again.
///
/// Returns the index into `entries`, its 1-based place among the entries
/// being walked, and how many of those there are.
fn diagnostic_target(
    entries: &[(usize, bool)],
    cursor: usize,
    forward: bool,
) -> Option<(usize, usize, usize)> {
    let errors_only = entries.iter().any(|&(_, is_error)| is_error);
    let candidates: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, (_, is_error))| *is_error || !errors_only)
        .map(|(ix, (start, _))| (ix, *start))
        .collect();
    let place = if forward {
        candidates
            .iter()
            .position(|&(_, start)| start > cursor)
            .unwrap_or(0)
    } else {
        candidates
            .iter()
            .rposition(|&(_, start)| start < cursor)
            .or(candidates.len().checked_sub(1))?
    };
    let &(ix, _) = candidates.get(place)?;
    Some((ix, place + 1, candidates.len()))
}

/// Toggle `--` line comments on a block of full lines: when every
/// non-blank line is already commented the prefix is removed, otherwise
/// `-- ` is inserted after each line's leading whitespace (blank lines
/// are left alone).
fn toggle_line_comments(block: &str) -> String {
    let uncomment = block.lines().any(|line| !line.trim().is_empty())
        && block
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| line.trim_start().starts_with("--"));

    block
        .split('\n')
        .map(|line| {
            let indent_len = line.len() - line.trim_start().len();
            let (indent, rest) = line.split_at(indent_len);
            if uncomment {
                let rest = rest.strip_prefix("--").unwrap_or(rest);
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                format!("{indent}{rest}")
            } else if rest.is_empty() {
                line.to_string()
            } else {
                format!("{indent}-- {rest}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Live state of a running debug session, shown in the debug panel that
/// replaces the results table while active.
struct DebugState {
    session: debug::Session,
    /// Id of the tab the session was launched from, so each stop can move that
    /// editor's cursor onto the line being executed even if tabs were
    /// reordered or the user switched away.
    tab_id: u64,
    /// The most recent stop (source, current line, stack, variables). `None`
    /// before the first stop.
    stop: Option<debug::StopState>,
    /// The debugged routine's result once it returns.
    output: Option<String>,
    /// The latest progress note shown in the panel before the first stop
    /// (connecting, waiting for the target to trap, …).
    status: String,
    /// Set once the session reports termination; the panel stays up (showing
    /// the final output) until the user starts another or stops.
    terminated: bool,
    /// The session is working and not parked at a line: starting up, waiting
    /// for the target to be entered, or running on after a step or continue.
    /// Not derivable from `stop`/`terminated` — after the first stop, the
    /// previous stop's state is still on screen while the next step is in
    /// flight — so it is set where commands go out and cleared where events
    /// come back.
    busy: bool,
}

/// Which view the bottom panel shows: the returned rows, the message log, or
/// the last plan's flame graph.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomView {
    Data,
    Log,
    Plan,
}

/// State of the object browser's top-level schema-list fetch. The three
/// states are mutually exclusive, so they live in one enum rather than a
/// `loading` bool plus an `error` option.
enum DbSchemaLoad {
    /// Loaded (or idle before the first load); the tree shows the current nodes.
    Ready,
    /// A fetch is in flight.
    Loading,
    /// The last fetch failed; the message is shown in the panel body.
    Failed(String),
}

pub struct PgGuiApp {
    tabs: Vec<EditorTab>,
    active_tab: usize,
    /// The active debug session, if any. Drives the debug panel.
    debug: Option<DebugState>,
    results: Entity<TableState<ResultsDelegate>>,
    /// Which view the bottom panel shows (Data table vs. message log).
    bottom_view: BottomView,
    /// Per-statement messages from the last executed query/script, shown in
    /// the Log view.
    log: Vec<SharedString>,
    /// Scroll state of the Log view, so a running script's new lines can
    /// follow the bottom (see [`PgGuiApp::log_at_bottom`]).
    log_scroll: ScrollHandle,
    /// Focus target of the Log view. A drag-select there does not move focus
    /// by itself, so cmd-c would still reach the editor (whose copy writes
    /// its own, possibly empty, selection) or the results grid; focusing this
    /// on mouse-down lets cmd-c reach `Root`'s window-selection copy instead.
    log_focus: FocusHandle,
    /// Selection participant the Log view's text registers under.
    log_selection: TextSelectionHandle,
    /// Split state of the editor/results panels; the editor height is
    /// persisted to the config whenever the divider is dragged.
    resizable_state: Entity<ResizableState>,
    /// Split state of the files panel / editor area; the panel width is
    /// persisted to the config whenever the divider is dragged.
    sidebar_state: Entity<ResizableState>,
    /// Items shown in the files panel, scanned from `config.working_dir`.
    tree_state: Entity<TreeState>,
    /// Ids (absolute paths) of the tree's expanded directories, re-applied
    /// when the tree is rebuilt after a re-scan.
    expanded_dirs: HashSet<SharedString>,
    /// Ids of every directory in the tree. The tree's own `is_folder()` is
    /// children-based, so empty directories need this to get a folder icon.
    tree_dirs: Rc<HashSet<SharedString>>,
    /// The `TreeItem`s last handed to [`Self::tree_state`]. Their expanded
    /// flags live behind a shared `Rc`, so this copy tracks the user's own
    /// expanding and collapsing; flattening it (`file_tree::visible_ids`)
    /// reproduces the row order the tree draws, which is how a shift-click
    /// resolves the rows between the anchor and the clicked one.
    tree_items: Vec<TreeItem>,
    /// Scripts picked in the files panel, run together by [`RunScripts`].
    /// Ids (absolute paths), unordered — the run order comes from the tree.
    selected_scripts: HashSet<SharedString>,
    /// The row a plain click last landed on; a shift-click selects the
    /// range from here to the row it hits.
    select_anchor: Option<SharedString>,
    /// Hash of the last scan, so an unchanged re-scan skips the rebuild
    /// (which would reset the tree's selection).
    tree_signature: u64,
    /// The last directory scan, kept so a filter change can re-project the
    /// tree without walking the disk again.
    file_nodes: Vec<file_tree::FileNode>,
    /// The files panel's filter box; its text filters scanned entries.
    file_filter_input: Entity<InputState>,
    file_filter: String,
    /// Split state of the database browser / editor area; the panel width is
    /// persisted to the config whenever the divider is dragged.
    db_sidebar_state: Entity<ResizableState>,
    /// The database object browser tree widget (left panel).
    db_tree_state: Entity<TreeState>,
    /// The lazily-loaded object model backing [`Self::db_tree_state`]; the
    /// single source of truth, re-projected to `TreeItem`s on every change.
    db_nodes: Vec<db_tree::DbNode>,
    /// Ids of the browser's expanded nodes, re-applied when the tree is
    /// rebuilt (the widget itself has no lazy/expansion-preserving API).
    db_expanded: HashSet<SharedString>,
    /// The browser's filter box; its text filters loaded nodes client-side.
    db_filter_input: Entity<InputState>,
    db_filter: String,
    /// Whether system schemas (`pg_*`, `information_schema`) are shown;
    /// runtime-only, defaults off.
    show_system_schemas: bool,
    /// State of the browser's top-level schema-list fetch.
    db_schema_load: DbSchemaLoad,
    status: SharedString,
    /// A repaint loop is alive, driving the status bar's clock while some tab
    /// has work in flight. Guards against a second run stacking another loop.
    ticking: bool,
    ai_running: bool,
    /// Next id handed to a new tab; only ever increments (see [`EditorTab::id`]).
    next_tab_id: u64,
    /// Bumped whenever the active connection changes, so a query that was in
    /// flight against the previous server is discarded (its session not
    /// stored back) when it finally returns.
    db_epoch: u64,
    /// Query output belonging to connections other than the active one,
    /// keyed by [`config::connection_key`] like `config.parked_tabs`, where
    /// the scripts of the same parked tabs live.
    parked_results: HashMap<String, ParkedResults>,
    config: config::Config,
    /// Mtime of the config file after our last read or write; a different
    /// mtime on disk means it was edited externally and should be reloaded.
    config_disk_time: Option<SystemTime>,
    /// Theme font sizes at startup, i.e. at 100% zoom; the configured zoom
    /// factor scales these.
    base_font_size: Pixels,
    base_mono_font_size: Pixels,
    save_generation: usize,
    lsp: Option<lsp::Client>,
    /// What the language server's schema cache is doing, as the worker last
    /// reported it. Only meaningful while `lsp` is set, and reset with every
    /// (re)start, since each client loads its own.
    lsp_schema: lsp::SchemaStatus,
    /// Symbols already resolved for Go to Definition. Lives here, not in the
    /// per-editor provider, so every tab shares one cache and a reconnect or
    /// a browser refresh can drop it in one place.
    definition_cache: definitions::Cache,
    /// The title-bar connection picker, mirroring `config.recent_connections`
    /// with the active connection selected.
    connections: Entity<ComboboxState<SearchableVec<ConnectionItem>>>,
    /// In-window menu bar for Linux/Windows, mirroring the menus set via
    /// `cx.set_menus`; rebuilt alongside them in [`Self::refresh_menus`]. On
    /// macOS the native menu bar is used and this field does not exist.
    #[cfg(not(target_os = "macos"))]
    app_menu_bar: Entity<AppMenuBar>,
    _subscriptions: Vec<Subscription>,
    /// Kept alive for the lifetime of the open New Connection dialog: one
    /// subscription per field input that recomputes the connection-string
    /// preview. Replaced (dropping the previous set) each time the dialog
    /// opens; only ever written, never read.
    #[allow(dead_code)]
    connection_dialog_subs: Vec<Subscription>,
}

/// Run each of `files` on `session` in turn, heading every file with its
/// own log line — statement numbering restarts per file, so without that
/// line the log lines below it could not be told apart. Stops at the first
/// file that fails to be read or that a statement fails in, and reports it
/// named. Blocking; called on the background executor.
/// A running clock for the status bar: `0.4s` below a minute, `1m 05s`
/// above it. Deliberately not `Duration`'s `Debug` formatting, which the
/// finished-run log lines use — that switches units as it goes (`843ms`,
/// `1.2s`), which reads as jitter on a number that is ticking.
fn format_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 60. {
        format!("{secs:.1}s")
    } else {
        let whole = elapsed.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

impl PgGuiApp {
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    /// Load the config and patch it up for this launch: resolve the
    /// connection string (`DATABASE_URL` wins), keep it in the recent list,
    /// guarantee at least one tab with a valid active index, and drop
    /// remembered tab files that no longer exist (back to prompt-on-save).
    fn launch_config() -> config::Config {
        let mut config = config::load();
        // DATABASE_URL (explicit at launch) wins over the saved config, which
        // holds whatever was last typed into the connection field.
        if let Some(url) = std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty()) {
            config.connection_string = url;
        } else if config.connection_string.is_empty() {
            config.connection_string = default_conn();
        }
        // Keep the active connection at the head of the recent list so
        // Connection ▸ Recent always offers to reconnect to it.
        let active = record_recent(
            &mut config.recent_connections,
            &config.connection_string,
            "",
            None,
        );

        // Parks were once keyed by connection string; move a legacy entry
        // onto its connection's name, which is the key now, so an edit to the
        // url can no longer strand it.
        for conn in &config.recent_connections {
            if conn.name.is_empty() {
                continue;
            }
            if let Some(parked) = config.parked_tabs.remove(&conn.url) {
                config
                    .parked_tabs
                    .entry(conn.name.clone())
                    .or_default()
                    .extend(parked);
            }
        }

        // The active connection never keeps a park — selecting it empties one
        // — so an entry left under it was stranded by a crash or a hand edit.
        // Fold it back in rather than leave those scripts unreachable.
        for parked in config.parked_tabs.remove(&active).unwrap_or_default() {
            let ix = parked.index.min(config.tabs.len());
            config.tabs.insert(
                ix,
                config::ScriptTab {
                    script: parked.script,
                    file: None,
                    disk_time: None,
                },
            );
        }
        if config.tabs.is_empty() {
            config.tabs.push(config::ScriptTab::default());
        }
        config.active_tab = config.active_tab.min(config.tabs.len() - 1);
        for tab in &mut config.tabs {
            if tab.file.as_ref().is_some_and(|path| !path.exists()) {
                tab.file = None;
            }
        }
        config
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        snippets::ensure_dir();
        let mut config = Self::launch_config();
        // The folder already open predates the recent list (or was set by
        // hand in config.json); seed it so the menu isn't empty.
        if let Some(dir) = config.working_dir.clone() {
            record_recent_folder(&mut config.recent_folders, &dir);
        }

        // Set the theme before anything reads it: the editors pick up
        // highlighting from it and the base font sizes below come from it.
        apply_theme_selection(config.theme, window, cx);

        let (tabs, next_tab_id) = Self::build_initial_tabs(&config, window, cx);
        let active_tab = config.active_tab;

        let results = Self::build_results(config.page_size, window, cx);
        let resizable_state = cx.new(|_| ResizableState::default());
        let sidebar_state = cx.new(|_| ResizableState::default());
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let file_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter files…"));
        let (db_sidebar_state, db_tree_state, db_filter_input) = Self::build_db_browser(window, cx);

        let (connections, connections_sub) = Self::build_connection_combo(&config, window, cx);

        tabs[active_tab]
            .editor
            .update(cx, |state, cx| state.focus(window, cx));

        // The log's selection participant. `SelectableText` alone repaints
        // only on its own frames, so a drag would show the selection just on
        // release; refreshing the window on every change makes it follow the
        // pointer.
        let log_selection = TextSelectionHandle::new(String::new(), cx);
        let log_selection_refresh = log_selection.refresh_window_on_change(window, cx);

        let weak_this = cx.weak_entity();
        let subscriptions = vec![
            log_selection_refresh,
            // Flush any debounced (not yet written) changes on quit,
            // and stop the language server.
            cx.on_app_quit(|this, _| {
                this.save_config();
                if let Some(client) = this.lsp.take() {
                    client.shutdown();
                }
                async {}
            }),
            connections_sub,
            Self::track_expanded_dirs(&tree_state, cx),
            Self::track_file_filter(&file_filter_input, cx),
            Self::track_db_expanded(&db_tree_state, cx),
            Self::track_db_filter(&db_filter_input, cx),
            // Follow OS light/dark switches while the theme is "System".
            window.observe_window_appearance(move |window, cx| {
                weak_this
                    .update(cx, |this, cx| {
                        if this.config.theme == config::ThemeSelection::System {
                            Theme::sync_system_appearance(Some(window), cx);
                            this.apply_zoom(cx);
                        }
                    })
                    .ok();
            }),
        ];

        Self::install_close_guard(window, cx);

        let mut this = Self {
            tabs,
            active_tab,
            debug: None,
            results,
            bottom_view: BottomView::Data,
            log: Vec::new(),
            log_scroll: ScrollHandle::new(),
            log_focus: cx.focus_handle(),
            log_selection,
            resizable_state,
            sidebar_state,
            tree_state,
            expanded_dirs: HashSet::new(),
            tree_dirs: Rc::new(HashSet::new()),
            tree_items: Vec::new(),
            selected_scripts: HashSet::new(),
            select_anchor: None,
            tree_signature: 0,
            file_nodes: Vec::new(),
            file_filter_input,
            file_filter: String::new(),
            db_sidebar_state,
            db_tree_state,
            db_nodes: Vec::new(),
            db_expanded: HashSet::new(),
            db_filter_input,
            db_filter: String::new(),
            show_system_schemas: false,
            db_schema_load: DbSchemaLoad::Ready,
            status: "Ready".into(),
            ticking: false,
            ai_running: false,
            next_tab_id,
            db_epoch: 0,
            parked_results: HashMap::new(),
            config,
            config_disk_time: config::modified_time(),
            base_font_size: cx.theme().font_size,
            base_mono_font_size: cx.theme().mono_font_size,
            save_generation: 0,
            lsp: None,
            lsp_schema: lsp::SchemaStatus::Loading,
            definition_cache: definitions::Cache::default(),
            connections,
            #[cfg(not(target_os = "macos"))]
            app_menu_bar: AppMenuBar::new(cx),
            _subscriptions: subscriptions,
            connection_dialog_subs: Vec::new(),
        };
        this.finish_setup(window, cx);
        this
    }

    /// The startup work that needs the assembled app: title, menus, the
    /// language server, the two trees, the file watcher and the zoom.
    fn finish_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_window_title(window);
        self.refresh_menus(cx);
        self.start_lsp(cx);
        self.load_tree(cx);
        if self.config.db_panel_visible {
            self.load_db_schemas(cx);
        }
        Self::watch_files(window, cx);
        self.apply_zoom(cx);
    }

    /// Route the window's close button through the same unsaved-edits
    /// confirmation as cmd-q: veto the close and prompt instead.
    fn install_close_guard(window: &mut Window, cx: &mut Context<Self>) {
        let weak_this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak_this
                .update(cx, |this, cx| {
                    if this.has_unsaved_tabs() {
                        this.prompt_quit(window, cx);
                        false
                    } else {
                        true
                    }
                })
                .unwrap_or(true)
        });
    }

    /// The results grid. Cell selection makes a single value pickable (by
    /// click or arrow keys) so `CopyCell` has something to copy; the
    /// row-header gutter it adds on the left selects whole rows.
    fn build_results(
        page_size: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<ResultsDelegate>> {
        cx.new(|cx| {
            TableState::new(ResultsDelegate::new(page_size), window, cx).cell_selectable(true)
        })
    }

    /// Write the config and remember the file's new mtime, so the watcher
    /// doesn't mistake our own write for an external edit.
    fn save_config(&mut self) {
        // Persist each tab's last-synced file mtime alongside its script, so
        // the next launch can detect files edited while the app was closed.
        for (cfg, tab) in self.config.tabs.iter_mut().zip(&self.tabs) {
            cfg.disk_time = tab.disk_time;
        }
        config::save(&self.config);
        self.config_disk_time = config::modified_time();
    }

    /// Where the next file dialog should start (see [`dialog_start_dir`]).
    fn start_dir(&self) -> PathBuf {
        dialog_start_dir(
            self.config.last_dir.as_deref(),
            self.tabs
                .get(self.active_tab)
                .and_then(|tab| tab.path.as_deref()),
        )
    }

    /// Where the Open Folder dialog opens: the folder currently shown in
    /// the files panel, so re-opening starts where the user already is.
    /// Falls back to the same directory the file dialogs use.
    fn folder_start_dir(&self) -> PathBuf {
        self.config
            .working_dir
            .as_ref()
            .filter(|dir| dir.is_dir())
            .map_or_else(|| self.start_dir(), PathBuf::clone)
    }

    /// Adopt the directory of a file just chosen in a dialog as the start
    /// directory for the next one. Callers persist it with `save_config`.
    fn remember_dir(&mut self, path: &Path) {
        self.config.last_dir = path.parent().map(Path::to_path_buf);
    }

    /// Poll the config file and every open script file once a second, so
    /// external edits (config via cmd-,, scripts via another editor) are
    /// picked up live instead of being overwritten by our next save.
    /// Every third tick also re-scans the working directory for the files
    /// panel; an unchanged scan is dropped by [`Self::load_tree`], so this
    /// stays cheap.
    fn watch_files(window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let mut tick: u32 = 0;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                tick = tick.wrapping_add(1);
                let refresh_tree = tick.is_multiple_of(3);
                let alive = this.update_in(cx, |this, window, cx| {
                    this.check_config_file(window, cx);
                    this.check_tab_files(window, cx);
                    if refresh_tree && this.config.files_panel_visible {
                        this.load_tree(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Track which directories the user has open in the files panel, so a
    /// tree rebuild after a re-scan can restore them.
    fn track_expanded_dirs(tree_state: &Entity<TreeState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(tree_state, |this, _, event: &TreeEvent, _| match event {
            TreeEvent::Expanded(id) => {
                this.expanded_dirs.insert(id.clone());
            }
            TreeEvent::Collapsed(id) => {
                this.expanded_dirs.remove(id);
            }
        })
    }

    /// Re-scan the working directory on the background executor and swap
    /// the result into the files panel — unless nothing changed since the
    /// last scan, since a rebuild resets the tree's selection. A no-op
    /// without a working directory.
    fn load_tree(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.config.working_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            // The scan produces Send-able nodes; `TreeItem` itself is
            // Rc-based and must be built on the UI thread below.
            let (sig, nodes) = cx
                .background_spawn(async move {
                    let mut budget = file_tree::ENTRY_BUDGET;
                    let nodes = file_tree::scan_dir(&root, file_tree::MAX_DEPTH, &mut budget);
                    (file_tree::signature(&nodes), nodes)
                })
                .await;
            this.update(cx, |this, cx| {
                if sig == this.tree_signature {
                    return;
                }
                this.tree_signature = sig;
                this.file_nodes = nodes;
                // A script that was deleted or renamed since it was picked
                // can no longer be run; drop it rather than fail the batch.
                if !this.selected_scripts.is_empty() {
                    let mut scanned = HashSet::new();
                    file_tree::scanned_ids(&this.file_nodes, &mut scanned);
                    this.selected_scripts.retain(|id| scanned.contains(id));
                }
                this.rebuild_file_tree(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Re-filter the files panel as its filter box changes.
    fn track_file_filter(input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(input, |this, input, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let text = input.read(cx).value().to_string();
            if text == this.file_filter {
                return;
            }
            this.file_filter = text;
            this.rebuild_file_tree(cx);
        })
    }

    /// Re-project the last scan into the files tree widget. Cheap (UI-side
    /// only); runs after every scan or filter change.
    fn rebuild_file_tree(&mut self, cx: &mut Context<Self>) {
        let mut dirs = HashSet::new();
        let items = file_tree::to_tree_items(
            &self.file_nodes,
            &self.expanded_dirs,
            &self.file_filter,
            &mut dirs,
        );
        self.tree_dirs = Rc::new(dirs);
        // Keep the items as well as handing them over: cloning a `TreeItem`
        // shares its expanded flag, so this copy stays in step with the
        // widget and can be flattened into the drawn row order.
        self.tree_items.clone_from(&items);
        self.tree_state
            .update(cx, |state, cx| state.set_items(items, cx));
        cx.notify();
    }

    /// Forget every picked script.
    fn clear_script_selection(&mut self) {
        self.selected_scripts.clear();
        self.select_anchor = None;
    }

    /// A plain click on a script row: it becomes the whole selection and
    /// the anchor a later shift-click extends from. (The file also opens —
    /// that is the caller's job.)
    fn select_script(&mut self, id: &SharedString) {
        self.selected_scripts.clear();
        self.selected_scripts.insert(id.clone());
        self.select_anchor = Some(id.clone());
    }

    /// A cmd/ctrl-click on a script row: add or drop just that one, and
    /// re-anchor there so a following shift-click extends from it.
    fn toggle_script(&mut self, id: &SharedString) {
        if !self.selected_scripts.remove(id) {
            self.selected_scripts.insert(id.clone());
        }
        self.select_anchor = Some(id.clone());
    }

    /// A shift-click on a script row: select every script between the
    /// anchor row and this one. Rows in between that are folders or
    /// non-SQL files are skipped, and the anchor stays where it was so
    /// the range can be re-dragged. Without an anchor this is a plain
    /// click.
    fn extend_script_selection(&mut self, id: &SharedString) {
        let Some(anchor) = self.select_anchor.clone() else {
            self.select_script(id);
            return;
        };
        let rows = file_tree::visible_ids(&self.tree_items);
        let (Some(from), Some(to)) = (
            rows.iter().position(|row| row == &anchor),
            rows.iter().position(|row| row == id),
        ) else {
            // The anchor scrolled out of the tree (collapsed folder, or a
            // filter that no longer matches it); start over from here.
            self.select_script(id);
            return;
        };
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        self.selected_scripts.clear();
        for row in &rows[lo..=hi] {
            if self.is_selectable_script(row) {
                self.selected_scripts.insert(row.clone());
            }
        }
    }

    /// Whether a tree row is a script the batch runner can execute: a file
    /// (not a directory) with a `.sql` name.
    fn is_selectable_script(&self, id: &SharedString) -> bool {
        !self.tree_dirs.contains(id) && file_tree::is_sql(Path::new(id.as_ref()))
    }

    /// The picked scripts as paths, in the order the tree lists them —
    /// folders first, then names, which is the order the timestamp-named
    /// files under `sql/upgrade` have to run in.
    fn selected_script_paths(&self) -> Vec<PathBuf> {
        file_tree::ordered_ids(&self.tree_items)
            .into_iter()
            .filter(|id| self.selected_scripts.contains(id))
            .map(|id| PathBuf::from(id.to_string()))
            .collect()
    }

    /// Create the object browser's widgets: its resizable-split state, the
    /// tree, and the filter box.
    fn build_db_browser(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (
        Entity<ResizableState>,
        Entity<TreeState>,
        Entity<InputState>,
    ) {
        let sidebar = cx.new(|_| ResizableState::default());
        let tree = cx.new(|cx| TreeState::new(cx));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter loaded objects…"));
        (sidebar, tree, filter)
    }

    /// Record the browser tree's expanded ids (re-applied on rebuild) and
    /// kick off the lazy fetch when an unloaded node is expanded.
    fn track_db_expanded(
        db_tree_state: &Entity<TreeState>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(
            db_tree_state,
            |this, _, event: &TreeEvent, cx| match event {
                TreeEvent::Expanded(id) => {
                    this.db_expanded.insert(id.clone());
                    this.load_db_children(id.clone(), cx);
                }
                TreeEvent::Collapsed(id) => {
                    this.db_expanded.remove(id);
                }
            },
        )
    }

    /// Re-filter the browser tree as the filter box changes.
    fn track_db_filter(input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(input, |this, input, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let text = input.read(cx).value().to_string();
            if text == this.db_filter {
                return;
            }
            this.db_filter = text;
            this.rebuild_db_tree(cx);
        })
    }

    /// Re-project the object model into the browser tree widget. Cheap
    /// (UI-side only); runs after every model or filter change.
    fn rebuild_db_tree(&mut self, cx: &mut Context<Self>) {
        let items = db_tree::to_tree_items(&self.db_nodes, &self.db_expanded, &self.db_filter);
        self.db_tree_state
            .update(cx, |state, cx| state.set_items(items, cx));
        cx.notify();
    }

    /// Reset the browser and fetch the schema list on the background executor.
    /// A no-op reset (clears the tree) when there is no connection string.
    fn load_db_schemas(&mut self, cx: &mut Context<Self>) {
        self.db_nodes.clear();
        self.db_expanded.clear();
        // Objects may have been created or dropped since the last browse, and
        // a Go to Definition miss is cached; a refresh re-asks for both.
        self.definition_cache.clear();
        let conn = self.config.connection_string.clone();
        if conn.is_empty() {
            self.db_schema_load = DbSchemaLoad::Ready;
            self.rebuild_db_tree(cx);
            return;
        }
        self.db_schema_load = DbSchemaLoad::Loading;
        self.rebuild_db_tree(cx);
        let show_system = self.show_system_schemas;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { db_tree::load_schemas(&conn, show_system) })
                .await;
            this.update(cx, |this, cx| {
                this.db_schema_load = match result {
                    Ok(nodes) => {
                        this.db_nodes = nodes;
                        DbSchemaLoad::Ready
                    }
                    Err(err) => DbSchemaLoad::Failed(err),
                };
                this.rebuild_db_tree(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Fetch the children of a just-expanded folder node. Ignores nodes that
    /// are already loaded/loading (re-expanding a failed node retries).
    fn load_db_children(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(node) = db_tree::find(&self.db_nodes, &id) else {
            return;
        };
        if !matches!(
            node.load,
            db_tree::Load::Unloaded | db_tree::Load::Failed(_)
        ) {
            return;
        }
        let (kind, schema, relation) = (node.kind, node.schema.clone(), node.relation.clone());
        let conn = self.config.connection_string.clone();
        if let Some(node) = db_tree::find_mut(&mut self.db_nodes, &id) {
            node.load = db_tree::Load::Loading;
        }
        self.rebuild_db_tree(cx);

        let parent_id = id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    db_tree::load_children(&conn, kind, &parent_id, &schema, &relation)
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(node) = db_tree::find_mut(&mut this.db_nodes, &id) {
                    match result {
                        Ok(children) => {
                            node.children = children;
                            node.load = db_tree::Load::Loaded;
                        }
                        Err(err) => node.load = db_tree::Load::Failed(err),
                    }
                }
                this.rebuild_db_tree(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Re-fetch a folder's contents on demand — the refresh button on each
    /// folder row. A lazy folder re-runs its catalog query; a container
    /// (schema/table) resets its lazy sub-folders and reloads the expanded
    /// ones so newly created objects appear without a full tree reload.
    fn refresh_db_node(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(node) = db_tree::find(&self.db_nodes, &id) else {
            return;
        };
        if node.kind.is_lazy_folder() {
            // Force a reload: back to Unloaded so `load_db_children` re-fetches
            // (it skips nodes already Loaded/Loading).
            if let Some(node) = db_tree::find_mut(&mut self.db_nodes, &id) {
                node.load = db_tree::Load::Unloaded;
            }
            self.load_db_children(id, cx);
            return;
        }
        let expanded = self.db_expanded.clone();
        let mut reload = Vec::new();
        if let Some(node) = db_tree::find_mut(&mut self.db_nodes, &id) {
            db_tree::reset_lazy_descendants(node, &expanded, &mut reload);
        }
        for child_id in reload {
            self.load_db_children(child_id, cx);
        }
        self.rebuild_db_tree(cx);
    }

    /// The connection Go to Definition resolves symbols against.
    pub(crate) fn connection_string(&self) -> &str {
        &self.config.connection_string
    }

    fn check_config_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let disk_time = config::modified_time();
        if disk_time.is_none() || disk_time == self.config_disk_time {
            return;
        }
        self.config_disk_time = disk_time;
        match config::try_load() {
            Some(new) => self.apply_external_config(new, window, cx),
            None => self.set_status("config.json is invalid — keeping current settings", cx),
        }
    }

    /// Adopt an externally edited config: swap it in and resync the UI
    /// pieces that mirror it. `editor_height` is the exception — the panel
    /// split isn't writable from outside, so it applies on the next launch.
    fn apply_external_config(
        &mut self,
        new: config::Config,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = std::mem::replace(&mut self.config, new);

        if self.config.tabs != old.tabs || self.config.active_tab != old.active_tab {
            self.rebuild_tabs(window, cx);
        }

        if self.config.connection_string != old.connection_string {
            // The tabs follow the connection here too; the config just read
            // from disk is the base the park is applied on top of.
            let previous = Self::active_key(&old);
            let url = self.config.connection_string.clone();
            let key = record_recent(&mut self.config.recent_connections, &url, "", None);
            // An edited url that still belongs to the same saved connection
            // keeps that connection's tabs — only a different connection
            // swaps them.
            if key != previous {
                self.swap_connection_tabs(&previous, &key, window, cx);
            }
            self.refresh_menus(cx);
            if self.config.db_panel_visible {
                self.load_db_schemas(cx);
            } else {
                self.db_nodes.clear();
                self.db_expanded.clear();
            }
        }
        // Covers both a changed active connection and an edited recent list.
        self.sync_connection_combo(window, cx);

        // The Edit ▸ Format on Save check-mark is baked into the menu label,
        // so an external edit of the flag needs a rebuild.
        if self.config.format_on_save != old.format_on_save {
            self.refresh_menus(cx);
        }

        // The language server reads all of these from its generated
        // workspace config at startup, so a change means a restart.
        if self.config.connection_string != old.connection_string
            || self.config.keyword_case != old.keyword_case
            || self.config.constant_case != old.constant_case
        {
            self.restart_lsp(cx);
        }

        if self.config.page_size != old.page_size {
            let page_size = self.config.page_size;
            self.results.update(cx, |table, cx| {
                table.delegate_mut().set_page_size(page_size);
                table.refresh(cx);
            });
        }

        if (self.config.zoom - old.zoom).abs() > f32::EPSILON {
            self.apply_zoom(cx);
        }

        if self.config.theme != old.theme {
            self.apply_theme(window, cx);
        }

        if self.config.working_dir != old.working_dir {
            self.expanded_dirs.clear();
            self.clear_script_selection();
            self.tree_signature = 0;
            self.load_tree(cx);
        }

        self.set_status("Reloaded config.json", cx);
        cx.notify();
    }

    /// Recreate every editor tab from the config, after an external edit
    /// changed the tab list itself.
    fn rebuild_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.config.tabs.is_empty() {
            self.config.tabs.push(config::ScriptTab::default());
        }
        self.config.active_tab = self.config.active_tab.min(self.config.tabs.len() - 1);
        let mut id = self.next_tab_id;
        let autocommit = self.config.autocommit;
        self.tabs = self
            .config
            .tabs
            .iter()
            .map(|tab| {
                let this_id = id;
                id += 1;
                Self::build_tab(
                    tab,
                    Self::launch_baseline(tab),
                    this_id,
                    autocommit,
                    window,
                    cx,
                )
            })
            .collect();
        self.next_tab_id = id;
        if let Some(client) = &self.lsp {
            for tab in &self.tabs {
                Self::attach_lsp_providers(client, &tab.editor, cx);
            }
        }
        self.activate_tab(self.config.active_tab, window, cx);
    }

    /// Launch the Postgres language server in the background and plug it
    /// into the editor once the handshake completes.
    fn start_lsp(&mut self, cx: &mut Context<Self>) {
        let conn = self.config.connection_string.clone();
        let text = self.editor().read(cx).value().to_string();
        let (keyword_case, constant_case) = (self.config.keyword_case, self.config.constant_case);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    lsp::Client::start(&conn, &text, keyword_case, constant_case)
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok((client, diagnostics, schema)) => {
                    this.attach_lsp(client, diagnostics, schema, cx);
                }
                Err(err) => this.set_status(format!("SQL language server unavailable: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn attach_lsp(
        &mut self,
        client: lsp::Client,
        mut diagnostics: lsp::DiagnosticsReceiver,
        mut schema: lsp::SchemaStatusReceiver,
        cx: &mut Context<Self>,
    ) {
        // The settings the server reads at startup changed while it was
        // starting up; reconnect with the current ones instead.
        if client.connection_string() != self.config.connection_string
            || client.case_options() != (self.config.keyword_case, self.config.constant_case)
        {
            client.shutdown();
            self.start_lsp(cx);
            return;
        }

        for tab in &self.tabs {
            Self::attach_lsp_providers(&client, &tab.editor, cx);
        }
        // Resync whatever was typed while the server was starting.
        client.document_changed(self.editor().read(cx).value().to_string());
        self.lsp = Some(client);
        self.lsp_schema = lsp::SchemaStatus::Loading;
        self.set_status("SQL language server connected", cx);

        // Whether completions are schema-aware is the difference between a
        // working editor and one offering keywords only, and the server says
        // nothing about it; the status bar reports what the probe finds.
        cx.spawn(async move |this, cx| {
            while let Some(status) = schema.next().await {
                let updated = this.update(cx, |this, cx| {
                    this.lsp_schema = status;
                    if status == lsp::SchemaStatus::Loaded {
                        this.set_status("SQL language server schema loaded", cx);
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Some(diagnostics) = diagnostics.next().await {
                let updated = this.update(cx, |this, cx| {
                    // Diagnostics are for the server's single document,
                    // which mirrors the active tab.
                    this.editor().update(cx, |state, cx| {
                        if let Some(set) = state.diagnostics_mut() {
                            set.clear();
                            set.extend(diagnostics);
                        }
                        cx.notify();
                    });
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Plug the language server's completion and hover providers into one
    /// tab's editor.
    fn attach_lsp_providers(
        client: &lsp::Client,
        editor: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        let provider = Rc::new(lsp::Provider::new(client.clone()));
        editor.update(cx, |state, _| {
            state.lsp_mut().completion_provider = Some(provider.clone());
            state.lsp_mut().hover_provider = Some(provider);
        });
    }

    /// Restart the language server so it reconnects with the current
    /// connection string (completions follow the database schema).
    fn restart_lsp(&mut self, cx: &mut Context<Self>) {
        if let Some(client) = self.lsp.take() {
            client.shutdown();
        }
        for tab in &self.tabs {
            tab.editor.update(cx, |state, _| {
                // Fall back to snippets-only completions until the new
                // server connects.
                state.lsp_mut().completion_provider = Some(Rc::new(lsp::SnippetCompletions));
                state.lsp_mut().hover_provider = None;
            });
        }
        self.start_lsp(cx);
    }

    fn on_editor_event(
        &mut self,
        state: &Entity<EditorState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::BreakpointToggled(line) = event {
            let set = state.read(cx).breakpoints().contains(line);
            let verb = if set { "set" } else { "cleared" };
            self.set_status(format!("Breakpoint {verb}: line {}", line + 1), cx);
            self.sync_debug_breakpoint(state, *line, set);
            return;
        }
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Some(ix) = self.tabs.iter().position(|tab| tab.editor == *state) else {
            return;
        };
        let text = state.read(cx).value().to_string();
        // The body's origin moves as lines are added above it.
        let offset = statement::body_line_offset(&text);
        state.update(cx, |state, cx| state.set_line_number_offset(offset, cx));
        // A change that grows the buffer by more than one character and
        // brings in braced tab-stop markers is a template landing (an
        // accepted completion suggestion, or a paste) — never plain
        // typing, which completes a marker one character at a time. Jump
        // to its first stop, like the picker does. `config.tabs` still
        // holds the pre-change text here; snippet mode means the picker
        // already jumped.
        let template_landed = ix == self.active_tab
            && !self.tabs[ix].snippet_mode
            && text.len() > self.config.tabs[ix].script.len() + 1
            && snippets::next_tab_stop(&text, true).is_some()
            && snippets::next_tab_stop(&self.config.tabs[ix].script, true).is_none();
        // The server tracks a single document: the active tab's.
        if ix == self.active_tab
            && let Some(client) = &self.lsp
        {
            client.document_changed(text.clone());
        }
        self.config.tabs[ix].script = text;
        self.refresh_dirty(ix, cx);
        self.schedule_save(cx);
        if template_landed {
            self.next_snippet_stop(true, window, cx);
        }
    }

    /// Persist the config after a short debounce, so typing in the editor
    /// doesn't hit the disk on every keystroke. Also restarts the language
    /// server when the connection string has settled on a new value.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            this.update(cx, |this, cx| {
                if this.save_generation == generation {
                    this.save_config();
                    if this.lsp.as_ref().is_some_and(|client| {
                        client.connection_string() != this.config.connection_string
                    }) {
                        this.restart_lsp(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_status(&mut self, status: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = status.into();
        cx.notify();
    }

    // ===== PL/pgSQL step debugger =====
}

impl Render for PgGuiApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Dialogs, sheets and notifications are drawn by gpui-component's own
        // `Root` plugin overlay, above this element.
        v_flex()
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Capture-phase, so they run before the editor's own handlers
            // and can service snippet tab stops.
            .capture_action(cx.listener(Self::on_editor_tab))
            .capture_action(cx.listener(Self::on_editor_escape))
            .on_action(cx.listener(Self::run_query))
            .on_action(cx.listener(Self::run_scripts))
            .on_action(cx.listener(Self::explain_plan))
            .on_action(cx.listener(Self::explain_analyze))
            .on_action(cx.listener(Self::toggle_autocommit))
            .on_action(cx.listener(Self::commit_txn))
            .on_action(cx.listener(Self::rollback_txn))
            .on_action(cx.listener(Self::cancel_query))
            .on_action(cx.listener(Self::export_csv))
            .on_action(cx.listener(Self::export_inserts))
            .on_action(cx.listener(Self::copy_cell))
            .on_action(cx.listener(Self::select_all_rows))
            .on_action(cx.listener(Self::clear_row_selection))
            .on_action(cx.listener(Self::ai_complete))
            .on_action(cx.listener(Self::new_connection))
            .on_action(cx.listener(Self::edit_connection))
            .on_action(cx.listener(Self::connect_recent))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::open_recent_folder))
            .on_action(cx.listener(Self::toggle_files_panel))
            .on_action(cx.listener(Self::toggle_results_panel))
            .on_action(cx.listener(Self::toggle_db_panel))
            .on_action(cx.listener(Self::refresh_db_tree))
            .on_action(cx.listener(Self::save_file))
            .on_action(cx.listener(Self::open_snippet_picker))
            .on_action(cx.listener(Self::open_config))
            .on_action(cx.listener(Self::format_script))
            .on_action(cx.listener(Self::toggle_format_on_save))
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::next_diagnostic))
            .on_action(cx.listener(Self::prev_diagnostic))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::set_theme))
            .on_action(cx.listener(Self::show_help))
            .on_action(cx.listener(Self::open_github))
            .on_action(cx.listener(Self::start_debug))
            .on_action(cx.listener(Self::debug_step_over))
            .on_action(cx.listener(Self::debug_step_into))
            .on_action(cx.listener(Self::debug_continue))
            .on_action(cx.listener(Self::debug_stop))
            .on_action(cx.listener(Self::request_quit))
            .child(self.render_title_bar(cx))
            .child(
                // The database browser (left) and files panel (right) flank
                // the editor area, each split by a draggable divider whose
                // position is persisted to the config on drag and restored on
                // launch.
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_workspace(cx)),
            )
            .child(self.render_status_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        MAX_RECENT_CONNECTIONS, MAX_RECENT_FOLDERS, diagnostic_target, dialog_start_dir,
        format_elapsed, mask_credentials, percent_decode, record_recent, record_recent_folder,
        toggle_line_comments,
    };

    #[test]
    fn dialog_start_dir_prefers_existing_last_dir() {
        let dir = std::env::temp_dir();
        assert_eq!(dialog_start_dir(Some(&dir), None), dir);
    }

    #[test]
    fn dialog_start_dir_skips_missing_last_dir() {
        let missing = Path::new("/pg-gui-test-does-not-exist");
        let tab_file = std::env::temp_dir().join("script.sql");
        assert_eq!(
            dialog_start_dir(Some(missing), Some(&tab_file)),
            std::env::temp_dir()
        );
    }

    #[test]
    fn dialog_start_dir_falls_back_to_home() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(dialog_start_dir(None, None), home);
        let missing_tab = Path::new("/pg-gui-test-does-not-exist/script.sql");
        assert_eq!(dialog_start_dir(None, Some(missing_tab)), home);
    }

    #[test]
    fn recent_folders_dedup_and_cap() {
        let mut recents = Vec::new();
        for i in 0..12 {
            record_recent_folder(&mut recents, &PathBuf::from(format!("/dir{i}")));
        }
        assert_eq!(recents.len(), MAX_RECENT_FOLDERS);
        assert_eq!(recents[0], PathBuf::from("/dir11"));

        record_recent_folder(&mut recents, &PathBuf::from("/dir5"));
        assert_eq!(recents[0], PathBuf::from("/dir5"));
        assert_eq!(
            recents.iter().filter(|d| *d == Path::new("/dir5")).count(),
            1
        );
    }

    /// Re-pointing a saved connection at another server updates its entry
    /// rather than leaving the old url behind as a second one, and a rename
    /// replaces the entry it started from.
    #[test]
    fn recent_connections_are_keyed_by_name() {
        let mut recents = Vec::new();
        record_recent(&mut recents, "postgres://a/db", "Prod", None);
        assert_eq!(recents.len(), 1);

        // Edit: same name, new url.
        let key = record_recent(&mut recents, "postgres://b/db", "Prod", Some("Prod"));
        assert_eq!(key, "Prod");
        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].url, "postgres://b/db");

        // Rename: the entry it started from goes with it.
        let key = record_recent(&mut recents, "postgres://b/db", "Staging", Some("Prod"));
        assert_eq!(key, "Staging");
        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].name, "Staging");
    }

    /// An unnamed connection is keyed by its url, and naming that url folds
    /// the unnamed entry into the named one instead of listing both.
    #[test]
    fn unnamed_connections_are_keyed_by_url() {
        let mut recents = Vec::new();
        let key = record_recent(&mut recents, "postgres://a/db", "", None);
        assert_eq!(key, "postgres://a/db");

        record_recent(&mut recents, "postgres://c/db", "", None);
        assert_eq!(recents.len(), 2);

        let key = record_recent(&mut recents, "postgres://a/db", "Prod", None);
        assert_eq!(key, "Prod");
        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].name, "Prod");
        assert_eq!(recents[1].url, "postgres://c/db");

        // Reconnecting without a name keeps the one it was saved under.
        let key = record_recent(&mut recents, "postgres://a/db", "", None);
        assert_eq!(key, "Prod");
        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].name, "Prod");
    }

    #[test]
    fn recent_connections_are_capped() {
        let mut recents = Vec::new();
        for i in 0..MAX_RECENT_CONNECTIONS + 2 {
            record_recent(&mut recents, &format!("postgres://host{i}/db"), "", None);
        }
        assert_eq!(recents.len(), MAX_RECENT_CONNECTIONS);
        // An empty url is not a connection.
        record_recent(&mut recents, "", "Nothing", None);
        assert_eq!(recents.len(), MAX_RECENT_CONNECTIONS);
    }

    #[test]
    fn comments_a_single_line() {
        assert_eq!(toggle_line_comments("select 1;"), "-- select 1;");
    }

    #[test]
    fn uncomments_a_single_line() {
        assert_eq!(toggle_line_comments("-- select 1;"), "select 1;");
        assert_eq!(toggle_line_comments("--select 1;"), "select 1;");
    }

    #[test]
    fn comments_after_indentation() {
        assert_eq!(
            toggle_line_comments("  from users\n\tjoin orders"),
            "  -- from users\n\t-- join orders"
        );
    }

    #[test]
    fn uncomments_indented_lines() {
        assert_eq!(
            toggle_line_comments("  -- from users\n\t--join orders"),
            "  from users\n\tjoin orders"
        );
    }

    #[test]
    fn comments_when_any_line_is_uncommented() {
        assert_eq!(
            toggle_line_comments("-- select 1;\nselect 2;"),
            "-- -- select 1;\n-- select 2;"
        );
    }

    #[test]
    fn skips_blank_lines_when_commenting() {
        assert_eq!(
            toggle_line_comments("select 1;\n\nselect 2;"),
            "-- select 1;\n\n-- select 2;"
        );
    }

    #[test]
    fn ignores_blank_lines_when_uncommenting() {
        assert_eq!(
            toggle_line_comments("-- select 1;\n\n-- select 2;"),
            "select 1;\n\nselect 2;"
        );
    }

    #[test]
    fn blank_only_block_is_untouched() {
        assert_eq!(toggle_line_comments(""), "");
        assert_eq!(toggle_line_comments("  \n"), "  \n");
    }

    #[test]
    fn masks_user_and_password_in_url() {
        assert_eq!(
            mask_credentials("postgres://alice:secret@localhost:5432/db"),
            "postgres://****:****@localhost:5432/db"
        );
    }

    #[test]
    fn masks_user_without_password() {
        assert_eq!(
            mask_credentials("postgres://alice@localhost/db"),
            "postgres://****@localhost/db"
        );
    }

    #[test]
    fn leaves_credential_free_urls_untouched() {
        assert_eq!(
            mask_credentials("postgres://localhost:5432/db"),
            "postgres://localhost:5432/db"
        );
    }

    #[test]
    fn masks_credentials_in_query_only_urls() {
        assert_eq!(
            mask_credentials("postgres://alice:secret@localhost?sslmode=require"),
            "postgres://****:****@localhost?sslmode=require"
        );
    }

    #[test]
    fn masks_key_value_form() {
        assert_eq!(
            mask_credentials("host=localhost user=alice password=secret dbname=db"),
            "host=localhost user=**** password=**** dbname=db"
        );
    }

    #[test]
    fn percent_decode_decodes_escapes() {
        assert_eq!(percent_decode("a%40b%3Ac"), "a@b:c");
        assert_eq!(percent_decode("%C3%A9"), "é");
    }

    #[test]
    fn percent_decode_leaves_incomplete_escapes_alone() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("50%z9"), "50%z9");
        // A `%` whose next two bytes end inside a multi-byte character: the
        // sequence is left as it stands rather than panicking on the slice.
        assert_eq!(percent_decode("%aé"), "%aé");
        assert_eq!(percent_decode("pass%é"), "pass%é");
    }

    #[test]
    fn elapsed_reads_as_a_clock() {
        use std::time::Duration;
        // Sub-second: one decimal, so the number moves visibly.
        assert_eq!(format_elapsed(Duration::from_millis(0)), "0.0s");
        assert_eq!(format_elapsed(Duration::from_millis(432)), "0.4s");
        // Seconds keep the same shape rather than switching unit.
        assert_eq!(format_elapsed(Duration::from_millis(12_340)), "12.3s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59.0s");
        // Past a minute the seconds are zero-padded so the width is stable.
        assert_eq!(format_elapsed(Duration::from_mins(1)), "1m 00s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format_elapsed(Duration::from_hours(1)), "60m 00s");
    }

    #[test]
    fn diagnostic_target_walks_errors_first_and_wraps() {
        // A warning at 5, errors at 10 and 30, a hint at 20.
        let entries = [(5, false), (10, true), (20, false), (30, true)];
        assert_eq!(diagnostic_target(&entries, 0, true), Some((1, 1, 2)));
        // Strictly after: sitting on an error's start moves on.
        assert_eq!(diagnostic_target(&entries, 10, true), Some((3, 2, 2)));
        assert_eq!(diagnostic_target(&entries, 30, true), Some((1, 1, 2)));
        assert_eq!(diagnostic_target(&entries, 30, false), Some((1, 1, 2)));
        assert_eq!(diagnostic_target(&entries, 10, false), Some((3, 2, 2)));
        assert_eq!(diagnostic_target(&entries, 0, false), Some((3, 2, 2)));
    }

    #[test]
    fn diagnostic_target_falls_back_to_other_severities() {
        let entries = [(5, false), (20, false)];
        assert_eq!(diagnostic_target(&entries, 5, true), Some((1, 2, 2)));
        assert_eq!(diagnostic_target(&entries, 25, true), Some((0, 1, 2)));
        assert_eq!(diagnostic_target(&entries, 5, false), Some((1, 2, 2)));
        assert_eq!(diagnostic_target(&[], 0, true), None);
        assert_eq!(diagnostic_target(&[], 0, false), None);
    }
}
