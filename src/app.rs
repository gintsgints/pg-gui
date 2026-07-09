use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gpui::Subscription;
use gpui::{
    App, AppContext as _, Context, Entity, EntityInputHandler as _, InteractiveElement as _,
    IntoElement, Menu, MenuItem, NoAction, ParentElement as _, PathPromptOptions, Pixels, Render,
    SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Root, Sizable as _, Theme, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, RopeExt as _, TabSize},
    list::{List, ListEvent, ListState},
    notification::Notification,
    resizable::{ResizableState, resizable_panel, v_resizable},
    tab::{Tab, TabBar},
    table::{Table, TableState},
    v_flex,
};

use crate::results::ResultsDelegate;
use crate::{
    AiComplete, CloseTab, Connect, FormatScript, NewConnection, NewFile, NextTab, OpenConfig,
    OpenFile, OpenGitHub, OpenSnippets, PrevTab, Quit, RunQuery, SaveFile, ShowHelp, ToggleComment,
    ZoomIn, ZoomOut, ZoomReset, ai, config, db, lsp, snippets, statement,
};

/// The project's GitHub page, opened from the About application menu.
const REPO_URL: &str = "https://github.com/gintsgints/pg-gui";

const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 2.0;

/// Every command with its keybinding(s), shown in the help dialog
/// (cmd-h on macOS, F1 elsewhere). Must mirror the bindings in main.rs.
#[cfg(target_os = "macos")]
const COMMANDS: &[(&str, &str)] = &[
    (
        "cmd-enter / ctrl-enter",
        "Run the selection or the statement at the cursor",
    ),
    ("cmd-i / ctrl-space", "AI-complete SQL at the cursor"),
    ("cmd-shift-f", "Format the script"),
    ("cmd-/", "Comment or uncomment the line / selection"),
    ("cmd-p", "Insert a snippet"),
    ("cmd-n", "New script tab"),
    ("cmd-w", "Close the tab"),
    ("ctrl-tab / ctrl-shift-tab", "Next / previous tab"),
    ("cmd-o", "Open a SQL script"),
    ("cmd-s", "Save the script"),
    ("cmd-,", "Open config.json in the system editor"),
    ("cmd-plus / cmd-minus", "Zoom in / out"),
    ("cmd-0", "Reset zoom"),
    ("cmd-h", "Show this help"),
    ("cmd-q", "Quit"),
];
#[cfg(not(target_os = "macos"))]
const COMMANDS: &[(&str, &str)] = &[
    (
        "ctrl-enter",
        "Run the selection or the statement at the cursor",
    ),
    ("ctrl-i / ctrl-space", "AI-complete SQL at the cursor"),
    ("ctrl-shift-f", "Format the script"),
    ("ctrl-/", "Comment or uncomment the line / selection"),
    ("ctrl-p", "Insert a snippet"),
    ("ctrl-n", "New script tab"),
    ("ctrl-w", "Close the tab"),
    ("ctrl-tab / ctrl-shift-tab", "Next / previous tab"),
    ("ctrl-o", "Open a SQL script"),
    ("ctrl-s", "Save the script"),
    ("ctrl-,", "Open config.json in the system editor"),
    ("ctrl-plus / ctrl-minus", "Zoom in / out"),
    ("ctrl-0", "Reset zoom"),
    ("f1", "Show this help"),
    ("ctrl-q", "Quit"),
];

fn default_conn() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "postgres".to_string());
    format!("postgres://{user}@localhost:5432/postgres")
}

/// Number of connection strings kept in the Recent menu.
const MAX_RECENT_CONNECTIONS: usize = 10;

/// Move `url` to the front of the recent-connections list (dedup, capped),
/// ignoring an empty string.
fn record_recent(recents: &mut Vec<String>, url: &str) {
    if url.is_empty() {
        return;
    }
    recents.retain(|c| c != url);
    recents.insert(0, url.to_string());
    recents.truncate(MAX_RECENT_CONNECTIONS);
}

/// The application menu bar. Every command lives here now that the toolbar
/// is gone; the OS fills in each item's shortcut from the keybindings in
/// `main.rs`. `recents` becomes the Connection ▸ Recent submenu, each entry
/// carrying its (unmasked) connection string in a [`Connect`] action while
/// showing the credentials masked.
fn build_menus(recents: &[String]) -> Vec<Menu> {
    let recent_items = if recents.is_empty() {
        vec![MenuItem::action("No recent connections", NoAction)]
    } else {
        recents
            .iter()
            .map(|url| MenuItem::action(mask_credentials(url), Connect { url: url.clone() }))
            .collect()
    };

    vec![
        Menu {
            name: "pg-gui".into(),
            items: vec![
                MenuItem::action("Preferences…", OpenConfig),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        },
        Menu {
            name: "Connection".into(),
            items: vec![
                MenuItem::action("New Connection…", NewConnection),
                MenuItem::submenu(Menu {
                    name: "Recent".into(),
                    items: recent_items,
                }),
                MenuItem::separator(),
                MenuItem::action("Run Query", RunQuery),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New", NewFile),
                MenuItem::action("Open…", OpenFile),
                MenuItem::action("Save", SaveFile),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Format", FormatScript),
                MenuItem::action("Snippets", OpenSnippets),
                MenuItem::action("AI Complete", AiComplete),
                MenuItem::separator(),
                MenuItem::action("Toggle Comment", ToggleComment),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Actual Size", ZoomReset),
            ],
        },
        Menu {
            name: "About".into(),
            items: vec![
                MenuItem::action("pg-gui on GitHub", OpenGitHub),
                MenuItem::action("Keyboard Shortcuts", ShowHelp),
            ],
        },
    ]
}

/// A file's last modification time, or `None` when it's missing or can't be
/// stat'd. Used to notice when a tab's file was edited outside the app.
fn file_mtime(path: &Path) -> Option<SystemTime> {
    path.metadata().ok()?.modified().ok()
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

/// Marker type keying the Test Connection notification, so the "testing…"
/// toast is replaced in place by its success/failure result rather than
/// stacking.
struct TestConnectionNotice;

/// The individual pieces of a `PostgreSQL` connection, edited as separate
/// fields in the New Connection dialog and recombined into a URL.
#[derive(Default)]
struct ConnectionParts {
    host: String,
    port: String,
    database: String,
    user: String,
    password: String,
}

/// The five text inputs of the New Connection dialog, grouped so their
/// current values can be read back into [`ConnectionParts`] in one place.
#[derive(Clone)]
struct ConnectionFields {
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
}

impl ConnectionFields {
    fn as_array(&self) -> [Entity<InputState>; 5] {
        [
            self.host.clone(),
            self.port.clone(),
            self.database.clone(),
            self.user.clone(),
            self.password.clone(),
        ]
    }

    /// Snapshot the fields; everything but the password is trimmed (a
    /// password may legitimately contain leading/trailing spaces).
    fn read(&self, cx: &App) -> ConnectionParts {
        ConnectionParts {
            host: self.host.read(cx).value().trim().to_string(),
            port: self.port.read(cx).value().trim().to_string(),
            database: self.database.read(cx).value().trim().to_string(),
            user: self.user.read(cx).value().trim().to_string(),
            password: self.password.read(cx).value().to_string(),
        }
    }
}

/// Percent-decode the reserved characters we encode in [`ConnectionParts::to_url`];
/// leaves any other `%`-sequence (or a lone `%`) untouched.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
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
fn percent_encode(s: &str) -> String {
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

impl ConnectionParts {
    /// Split a `postgres://user:password@host:port/database` URL into its
    /// fields. A string that is not in URL form (e.g. key-value) yields
    /// empty fields, leaving the dialog for the user to fill in.
    fn parse(conn: &str) -> Self {
        let Some((_, rest)) = conn.split_once("://") else {
            return Self::default();
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };

        let mut parts = Self {
            database: percent_decode(path.split(['?', '#']).next().unwrap_or("")),
            ..Self::default()
        };

        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        if let Some(userinfo) = userinfo {
            match userinfo.split_once(':') {
                Some((user, password)) => {
                    parts.user = percent_decode(user);
                    parts.password = percent_decode(password);
                }
                None => parts.user = percent_decode(userinfo),
            }
        }
        // A bracketed IPv6 host keeps its own colons; only a trailing
        // `:port` after the closing bracket (or on a bare host) is the port.
        match hostport.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') => {
                parts.host = host.to_string();
                parts.port = port.to_string();
            }
            _ => parts.host = hostport.to_string(),
        }
        parts
    }

    /// Recombine the fields into a `postgres://` URL, percent-encoding the
    /// credential and database segments.
    fn to_url(&self) -> String {
        let mut url = String::from("postgres://");
        if !self.user.is_empty() {
            url.push_str(&percent_encode(&self.user));
            if !self.password.is_empty() {
                url.push(':');
                url.push_str(&percent_encode(&self.password));
            }
            url.push('@');
        }
        url.push_str(&self.host);
        if !self.port.is_empty() {
            url.push(':');
            url.push_str(&self.port);
        }
        url.push('/');
        url.push_str(&percent_encode(&self.database));
        url
    }
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

/// One open script: its editor buffer and the file it belongs to, if
/// any — cmd-s writes there without prompting. Mirrored in
/// `config.tabs` at the same index, which holds the persisted text.
struct EditorTab {
    editor: Entity<InputState>,
    path: Option<PathBuf>,
    /// The content last written to (or read from) disk — the baseline the
    /// buffer is compared against to decide if the tab has unsaved edits.
    /// Empty for a never-saved tab. Not persisted: recomputed on launch
    /// from the file so restored edits still register as unsaved.
    saved: String,
    /// Whether the buffer differs from `saved`, cached so the tab bar can
    /// show a marker without diffing on every frame.
    dirty: bool,
    /// Mtime of `path` after our last read or write; a newer mtime on disk
    /// means the file was edited externally. `None` for a tab with no file
    /// (or one whose file is missing).
    disk_time: Option<SystemTime>,
    /// Set when the file changed on disk while the tab had unsaved edits, so
    /// a plain reload would clobber one side or the other. Shown with a
    /// distinct tab glyph and enforced with a prompt before overwriting.
    diverged: bool,
    _subscription: Subscription,
}

pub struct PgGuiApp {
    tabs: Vec<EditorTab>,
    active_tab: usize,
    results: Entity<TableState<ResultsDelegate>>,
    /// Split state of the editor/results panels; the editor height is
    /// persisted to the config whenever the divider is dragged.
    resizable_state: Entity<ResizableState>,
    status: SharedString,
    running: bool,
    ai_running: bool,
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
    _subscriptions: Vec<Subscription>,
    /// Kept alive for the lifetime of the open New Connection dialog: one
    /// subscription per field input that recomputes the connection-string
    /// preview. Replaced (dropping the previous set) each time the dialog
    /// opens; only ever written, never read.
    #[allow(dead_code)]
    connection_dialog_subs: Vec<Subscription>,
}

impl PgGuiApp {
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        snippets::ensure_dir();
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
        record_recent(&mut config.recent_connections, &config.connection_string);

        if config.tabs.is_empty() {
            config.tabs.push(config::ScriptTab::default());
        }
        config.active_tab = config.active_tab.min(config.tabs.len() - 1);
        // A remembered path whose file is gone is dropped, back to
        // prompt-on-save.
        for tab in &mut config.tabs {
            if tab.file.as_ref().is_some_and(|path| !path.exists()) {
                tab.file = None;
            }
        }

        let tabs: Vec<EditorTab> = config
            .tabs
            .iter()
            .map(|tab| Self::build_tab(tab, Self::launch_baseline(tab), window, cx))
            .collect();
        let active_tab = config.active_tab;

        let results =
            cx.new(|cx| TableState::new(ResultsDelegate::new(config.page_size), window, cx));
        let resizable_state = cx.new(|_| ResizableState::default());

        tabs[active_tab]
            .editor
            .update(cx, |state, cx| state.focus(window, cx));

        let subscriptions = vec![
            // Flush any debounced (not yet written) changes on quit,
            // and stop the language server.
            cx.on_app_quit(|this, _| {
                this.save_config();
                if let Some(client) = this.lsp.take() {
                    client.shutdown();
                }
                async {}
            }),
        ];

        let mut this = Self {
            tabs,
            active_tab,
            results,
            resizable_state,
            status: "Ready".into(),
            running: false,
            ai_running: false,
            config,
            config_disk_time: config::modified_time(),
            base_font_size: cx.theme().font_size,
            base_mono_font_size: cx.theme().mono_font_size,
            save_generation: 0,
            lsp: None,
            _subscriptions: subscriptions,
            connection_dialog_subs: Vec::new(),
        };
        this.update_window_title(window);
        this.refresh_menus(cx);
        this.start_lsp(cx);
        Self::watch_files(window, cx);
        this.apply_zoom(cx);
        this
    }

    /// Create the editor for one tab and wire it into the change plumbing.
    /// `saved` is the on-disk baseline used for the unsaved-edits marker.
    /// The caller hooks up the language server, if connected.
    fn build_tab(
        tab: &config::ScriptTab,
        saved: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EditorTab {
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("sql")
                .multi_line(true)
                .line_number(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    ..Default::default()
                })
                .placeholder("-- Write PostgreSQL here, then press cmd-enter to run")
                .default_value(tab.script.clone())
        });
        let subscription = cx.subscribe_in(&editor, window, Self::on_editor_event);
        let disk_time = tab.file.as_deref().and_then(file_mtime);
        let dirty = tab.script != saved;
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
            _subscription: subscription,
        }
    }

    /// The on-disk baseline for a restored tab: the file's current content
    /// (so edits persisted since the last save show as unsaved), or empty
    /// for a tab that was never saved to a file.
    fn launch_baseline(tab: &config::ScriptTab) -> String {
        match &tab.file {
            Some(path) => std::fs::read_to_string(path).unwrap_or_else(|_| tab.script.clone()),
            None => String::new(),
        }
    }

    /// Recompute a tab's unsaved-edits marker and repaint if it flipped.
    fn refresh_dirty(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let value = self.tabs[ix].editor.read(cx).value().to_string();
        let dirty = value != self.tabs[ix].saved;
        if self.tabs[ix].dirty != dirty {
            self.tabs[ix].dirty = dirty;
            cx.notify();
        }
    }

    /// The active tab's editor.
    fn editor(&self) -> Entity<InputState> {
        self.tabs[self.active_tab].editor.clone()
    }

    /// Append a tab (not yet selected) and its config mirror.
    fn add_tab(
        &mut self,
        script: String,
        file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let tab_config = config::ScriptTab {
            script,
            file,
            disk_time: None,
        };
        // A freshly added tab starts clean: its content is the baseline
        // (empty for a new script, the file's text for an opened one).
        let tab = Self::build_tab(&tab_config, tab_config.script.clone(), window, cx);
        if let Some(client) = &self.lsp {
            Self::attach_lsp_providers(client, &tab.editor, cx);
        }
        self.tabs.push(tab);
        self.config.tabs.push(tab_config);
        self.tabs.len() - 1
    }

    /// Select a tab: focus its editor, retitle the window, and point the
    /// language server at its content.
    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.config.active_tab = ix;
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
    fn close_tab_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Close a tab, but prompt first when it has unsaved edits.
    fn request_close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && self.tabs[ix].dirty {
            self.prompt_save_before_close(ix, window, cx);
        } else {
            self.close_tab_at(ix, window, cx);
        }
    }

    /// Ask whether to save a tab's unsaved edits before closing it.
    fn prompt_save_before_close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let name = self.tab_label(ix);
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (save, discard) = (app.clone(), app.clone());
            dialog.title("Unsaved changes").w(px(420.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(
                        div()
                            .text_sm()
                            .child(format!("“{name}” has unsaved changes.")),
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

    /// Save a tab (prompting for a path if it has no file) and close it
    /// once the write succeeds.
    fn save_tab_then_close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Poll the config file and every open script file once a second, so
    /// external edits (config via cmd-,, scripts via another editor) are
    /// picked up live instead of being overwritten by our next save.
    fn watch_files(window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    this.check_config_file(window, cx);
                    this.check_tab_files(window, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Notice when an open tab's file changed on disk. A clean tab reloads
    /// silently; a tab with unsaved edits is flagged diverged (its buffer is
    /// left untouched) so the next save can prompt before clobbering.
    /// A missing file is ignored — only "exists with a newer mtime" reacts,
    /// which sidesteps the transient gap during an external temp-file swap.
    fn check_tab_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for ix in 0..self.tabs.len() {
            let Some(path) = self.tabs[ix].path.clone() else {
                continue;
            };
            let Some(disk_time) = file_mtime(&path) else {
                continue;
            };
            if Some(disk_time) == self.tabs[ix].disk_time {
                continue;
            }
            self.tabs[ix].disk_time = Some(disk_time);

            let name = self.tab_label(ix);
            if self.tabs[ix].dirty {
                // Already flagged: stay silent, the glyph is the standing
                // signal. Only announce the first divergence.
                if !self.tabs[ix].diverged {
                    self.tabs[ix].diverged = true;
                    self.set_status(
                        format!("{name} changed on disk — you have unsaved edits"),
                        cx,
                    );
                    cx.notify();
                }
            } else {
                self.reload_tab(ix, window, cx);
                self.set_status(format!("Reloaded {name} — it changed on disk"), cx);
            }
        }
    }

    /// Replace a tab's buffer with its file's current content, resetting the
    /// saved baseline and clearing the diverged flag. Best-effort keeps the
    /// caret near where it was. Shared by the silent clean-tab reload and the
    /// "reload theirs" choice in the diverged-save prompt.
    fn reload_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.tabs[ix].path.clone() else {
            return;
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                self.set_status(format!("Reload failed: {err}"), cx);
                return;
            }
        };
        // Baseline first, so the Change event set_value emits recomputes the
        // tab as clean rather than newly dirty.
        self.tabs[ix].saved.clone_from(&content);
        self.tabs[ix].disk_time = file_mtime(&path);
        self.tabs[ix].diverged = false;
        self.tabs[ix].editor.update(cx, |state, cx| {
            let mut offset = state.cursor().min(content.len());
            while offset > 0 && !content.is_char_boundary(offset) {
                offset -= 1;
            }
            state.set_value(content, window, cx);
            let position = state.text().offset_to_position(offset);
            state.set_cursor_position(position, window, cx);
        });
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
            record_recent(
                &mut self.config.recent_connections,
                &self.config.connection_string,
            );
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
        self.tabs = self
            .config
            .tabs
            .iter()
            .map(|tab| Self::build_tab(tab, Self::launch_baseline(tab), window, cx))
            .collect();
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
                Ok((client, diagnostics)) => this.attach_lsp(client, diagnostics, cx),
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
        self.set_status("SQL language server connected", cx);

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
        editor: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        let provider = Rc::new(lsp::Provider::new(client.clone()));
        editor.update(cx, |state, _| {
            state.lsp.completion_provider = Some(provider.clone());
            state.lsp.hover_provider = Some(provider);
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
                state.lsp.completion_provider = None;
                state.lsp.hover_provider = None;
            });
        }
        self.start_lsp(cx);
    }

    fn on_editor_event(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Some(ix) = self.tabs.iter().position(|tab| tab.editor == *state) else {
            return;
        };
        let text = state.read(cx).value().to_string();
        // The server tracks a single document: the active tab's.
        if ix == self.active_tab
            && let Some(client) = &self.lsp
        {
            client.document_changed(text.clone());
        }
        self.config.tabs[ix].script = text;
        self.refresh_dirty(ix, cx);
        self.schedule_save(cx);
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

    /// Open the searchable snippet picker; the confirmed snippet is
    /// inserted into the editor at the cursor.
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn open_snippet_picker(
        &mut self,
        _: &OpenSnippets,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }

        let app = cx.weak_entity();
        let list = cx.new(|cx| {
            let delegate =
                snippets::PickerDelegate::new(snippets::load(), move |snippet, window, cx| {
                    window.close_dialog(cx);
                    app.update(cx, |this, cx| this.insert_snippet(snippet, window, cx))
                        .ok();
                });
            ListState::new(delegate, window, cx).searchable(true)
        });
        cx.subscribe_in(&list, window, |_, _, event, window, cx| {
            if matches!(event, ListEvent::Cancel) {
                window.close_dialog(cx);
            }
        })
        .detach();

        let list_in_dialog = list.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("Insert snippet").w(px(560.)).child(
                div()
                    .h(px(400.))
                    .child(List::new(&list_in_dialog).search_placeholder("Search snippets…")),
            )
        });
        list.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Insert a snippet at the cursor as its own statement. When the
    /// snippet contains a `%%` filter placeholder, the caret is placed
    /// between the two `%` so typing narrows the filter.
    fn insert_snippet(
        &mut self,
        snippet: &snippets::Snippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor().update(cx, |state, cx| {
            let sql = snippet.sql.trim();
            let text = state.value();
            let mut cursor = state.cursor().min(text.len());
            while cursor > 0 && !text.is_char_boundary(cursor) {
                cursor -= 1;
            }

            let mut inserted = String::new();
            if !text[..cursor].is_empty() && !text[..cursor].ends_with('\n') {
                inserted.push('\n');
            }
            inserted.push_str(sql);
            if !text[cursor..].starts_with('\n') {
                inserted.push('\n');
            }

            let placeholder = inserted.find("%%");
            state.insert(inserted.clone(), window, cx);
            if let Some(pos) = placeholder {
                let target = state.cursor() - inserted.len() + pos + 1;
                let position = state.text().offset_to_position(target);
                state.set_cursor_position(position, window, cx);
            } else {
                state.focus(window, cx);
            }
        });
        self.set_status(format!("Inserted “{}”", snippet.name), cx);
    }

    pub fn run_query(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        // The input may be showing the masked value; the config always
        // holds the real connection string.
        let conn = self.config.connection_string.clone();

        // Run the selected block when there is a selection, otherwise the
        // statement the cursor is on (or, when the cursor sits after a
        // statement, the one to its left).
        let selection = self.editor().update(cx, |state, cx| {
            state
                .selected_text_range(false, window, cx)
                .filter(|sel| !sel.range.is_empty())
                .and_then(|sel| state.text_for_range(sel.range, &mut None, window, cx))
                .filter(|text| !text.trim().is_empty())
        });
        let (sql, scope) = if let Some(sql) = selection {
            (sql, "selection")
        } else {
            let state = self.editor().read(cx);
            let text = state.value();
            let sql = statement::at(&text, state.cursor())
                .map(|range| text[range].to_string())
                .unwrap_or_default();
            (sql, "statement")
        };
        if sql.trim().is_empty() {
            self.set_status("Nothing to run", cx);
            return;
        }

        self.running = true;
        self.set_status(format!("Running {scope}…"), cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let result = cx
                .background_spawn(async move { db::run_script(&conn, &sql) })
                .await;
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| {
                this.running = false;
                match result {
                    Ok(outcome) => {
                        let row_count = outcome.rows.len();
                        let statements = outcome.messages.len();
                        this.results.update(cx, |table, cx| {
                            table.delegate_mut().set_data(outcome.columns, outcome.rows);
                            table.refresh(cx);
                        });
                        this.set_status(
                            format!(
                                "{scope}: {statements} statement(s) executed in {elapsed:.0?} — showing {row_count} row(s)"
                            ),
                            cx,
                        );
                    }
                    Err(err) => {
                        this.results.update(cx, |table, cx| {
                            table.delegate_mut().clear();
                            table.refresh(cx);
                        });
                        this.show_query_error(&err, window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Surface a failed execution in a dialog — the status bar alone is
    /// easy to miss, and postgres errors are often too long to fit there.
    fn show_query_error(&mut self, error: &str, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Rebuild the application menu bar, e.g. after the recent-connections
    /// list changes. `set_menus` is on `App`, reached through `Context`.
    fn refresh_menus(&self, cx: &mut Context<Self>) {
        cx.set_menus(build_menus(&self.config.recent_connections));
    }

    /// Connection ▸ New Connection…: prompt for the connection's fields
    /// (host, port, database, user, password), showing the assembled
    /// connection string live, then reconnect. Seeded with the current
    /// connection so it can be tweaked rather than retyped.
    pub fn new_connection(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let parts = ConnectionParts::parse(&self.config.connection_string);
        let mut field = |value: String, placeholder: &str, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let fields = ConnectionFields {
            host: field(parts.host, "localhost", cx),
            port: field(parts.port, "5432", cx),
            database: field(parts.database, "postgres", cx),
            user: field(parts.user, "postgres", cx),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .default_value(parts.password)
            }),
        };
        let preview = cx.new(|cx| {
            InputState::new(window, cx).default_value(self.config.connection_string.clone())
        });

        // Recompute the previewed connection string whenever any field
        // changes. The subscriptions live in `self` so they outlast this
        // method but are dropped the next time the dialog opens.
        let recompute = {
            let (fields, preview) = (fields.clone(), preview.clone());
            move |window: &mut Window, cx: &mut App| {
                let url = fields.read(cx).to_url();
                preview.update(cx, |state, cx| state.set_value(url, window, cx));
            }
        };
        self.connection_dialog_subs = fields
            .as_array()
            .iter()
            .map(|input| {
                let recompute = recompute.clone();
                cx.subscribe_in(
                    input,
                    window,
                    move |_, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Change) {
                            recompute(window, cx);
                        }
                    },
                )
            })
            .collect();

        Self::open_connection_dialog(fields, preview, window, cx);
    }

    /// Build and show the New Connection dialog for the given field inputs
    /// and live connection-string preview.
    fn open_connection_dialog(
        fields: ConnectionFields,
        preview: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (fields, preview) = (fields.clone(), preview.clone());
            let connect = {
                let (app, fields) = (app.clone(), fields.clone());
                move |window: &mut Window, cx: &mut App| {
                    let url = fields.read(cx).to_url();
                    window.close_dialog(cx);
                    app.update(cx, |this, cx| this.apply_connection(&url, cx))
                        .ok();
                }
            };
            let test = {
                let (app, fields) = (app.clone(), fields.clone());
                move |window: &mut Window, cx: &mut App| {
                    let url = fields.read(cx).to_url();
                    app.update(cx, |_, cx| Self::run_connection_test(url, window, cx))
                        .ok();
                }
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

            dialog.title("New connection").w(px(520.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().flex_1().child(labeled("Host", &fields.host, cx)))
                            .child(div().w(px(120.)).child(labeled("Port", &fields.port, cx))),
                    )
                    .child(labeled("Database", &fields.database, cx))
                    .child(labeled("Username", &fields.user, cx))
                    .child(labeled("Password", &fields.password, cx))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Connection string"),
                            )
                            .child(Input::new(&preview).disabled(true)),
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
                            .child(
                                Button::new("test")
                                    .outline()
                                    .label("Test Connection")
                                    .on_click(move |_, window, cx| test(window, cx)),
                            )
                            .child(
                                Button::new("connect")
                                    .primary()
                                    .label("Connect")
                                    .on_click(move |_, window, cx| connect(window, cx)),
                            ),
                    ),
            )
        });
    }

    /// Try to open a connection with `url` in the background and report the
    /// outcome as a notification, so the New Connection dialog's Test
    /// Connection button gives feedback without leaving the dialog.
    fn run_connection_test(url: String, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            Notification::info("Testing connection…")
                .id::<TestConnectionNotice>()
                .autohide(false),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { db::test_connection(&url) })
                .await;
            this.update_in(cx, |_, window, cx| {
                let note = match result {
                    Ok(()) => {
                        Notification::success("Connection succeeded").id::<TestConnectionNotice>()
                    }
                    Err(err) => Notification::error(format!(
                        "Connection failed: {}",
                        err.lines().next().unwrap_or_default()
                    ))
                    .id::<TestConnectionNotice>(),
                };
                window.push_notification(note, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Connection ▸ Recent ▸ …: reconnect to a previously used string.
    pub fn connect_recent(&mut self, action: &Connect, _: &mut Window, cx: &mut Context<Self>) {
        self.apply_connection(&action.url, cx);
    }

    /// Switch to `url`: remember it, persist, and restart the language
    /// server so completions follow the new database's schema.
    fn apply_connection(&mut self, url: &str, cx: &mut Context<Self>) {
        if url.is_empty() {
            return;
        }
        let masked = mask_credentials(url);
        self.config.connection_string = url.to_string();
        record_recent(&mut self.config.recent_connections, url);
        self.save_config();
        self.refresh_menus(cx);
        self.restart_lsp(cx);
        self.set_status(format!("Connecting to {masked}"), cx);
    }

    /// About ▸ pg-gui on GitHub: open the project page in the browser.
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn open_github(&mut self, _: &OpenGitHub, _: &mut Window, cx: &mut Context<Self>) {
        cx.open_url(REPO_URL);
    }

    /// The "Prev / Next / Page x of y" bar under the results table; `None`
    /// when everything fits on one page.
    fn render_results_pager(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let delegate = self.results.read(cx).delegate();
        let (page, page_count, total_rows) = (
            delegate.page(),
            delegate.page_count(),
            delegate.total_rows(),
        );
        if page_count <= 1 {
            return None;
        }

        Some(
            h_flex()
                .gap_2()
                .child(
                    Button::new("prev-page")
                        .outline()
                        .small()
                        .label("‹ Prev")
                        .disabled(page == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change_results_page(false, cx);
                        })),
                )
                .child(
                    Button::new("next-page")
                        .outline()
                        .small()
                        .label("Next ›")
                        .disabled(page + 1 >= page_count)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.change_results_page(true, cx);
                        })),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Page {} of {page_count} · {total_rows} rows",
                            page + 1
                        )),
                ),
        )
    }

    fn change_results_page(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.results.update(cx, |table, cx| {
            let moved = if forward {
                table.delegate_mut().next_page()
            } else {
                table.delegate_mut().prev_page()
            };
            if moved {
                table.scroll_to_row(0, cx);
                table.refresh(cx);
            }
        });
        cx.notify();
    }

    pub fn ai_complete(&mut self, _: &AiComplete, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_running {
            return;
        }
        let Some(key) = ai::api_key(&self.config.ai_api_key) else {
            self.set_status(
                "AI completion needs an API key: set ai_api_key in config.json or ANTHROPIC_API_KEY in the environment",
                cx,
            );
            return;
        };

        let (before, after) = {
            let state = self.editor().read(cx);
            let text = state.value().to_string();
            let mut cursor = state.cursor().min(text.len());
            while cursor > 0 && !text.is_char_boundary(cursor) {
                cursor -= 1;
            }
            (text[..cursor].to_string(), text[cursor..].to_string())
        };

        self.ai_running = true;
        self.set_status("AI completing…", cx);

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { ai::complete(&key, &before, &after) })
                .await;

            this.update_in(cx, |this, window, cx| {
                this.ai_running = false;
                match result {
                    Ok(completion) => {
                        this.editor().update(cx, |state, cx| {
                            state.insert(completion, window, cx);
                            state.focus(window, cx);
                        });
                        this.set_status("AI completion inserted", cx);
                    }
                    Err(err) => this.set_status(format!("AI error: {err}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Start a fresh script in a new tab (cmd-n); cmd-s prompts for its
    /// location.
    pub fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.add_tab(String::new(), None, window, cx);
        self.activate_tab(ix, window, cx);
        self.save_config();
        self.set_status("New script", cx);
    }

    /// Whether a tab holds an untouched fresh script, safe to reuse for an
    /// opened file.
    fn tab_is_pristine(&self, ix: usize, cx: &Context<Self>) -> bool {
        self.tabs[ix].path.is_none() && self.tabs[ix].editor.read(cx).value().is_empty()
    }

    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open SQL script".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let content = std::fs::read_to_string(&path);

            this.update_in(cx, |this, window, cx| match content {
                Ok(content) => {
                    // A file that is already open just gets its tab
                    // selected, keeping any unsaved edits in its buffer.
                    if let Some(ix) = this
                        .tabs
                        .iter()
                        .position(|tab| tab.path.as_deref() == Some(path.as_path()))
                    {
                        this.activate_tab(ix, window, cx);
                        this.set_status(format!("{} was already open", path.display()), cx);
                        return;
                    }
                    // Open into a new tab, except an untouched fresh tab
                    // is filled in place.
                    let ix = if this.tab_is_pristine(this.active_tab, cx) {
                        let ix = this.active_tab;
                        this.config.tabs[ix].script.clone_from(&content);
                        this.tabs[ix].saved.clone_from(&content);
                        this.tabs[ix].dirty = false;
                        this.tabs[ix].editor.update(cx, |state, cx| {
                            state.set_value(content, window, cx);
                        });
                        ix
                    } else {
                        this.add_tab(content, None, window, cx)
                    };
                    this.activate_tab(ix, window, cx);
                    this.set_tab_path(ix, path.clone(), window);
                    this.save_config();
                    this.set_status(format!("Opened {}", path.display()), cx);
                }
                Err(err) => this.set_status(format!("Open failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Open the config file in the system default editor (cmd-,).
    /// Saved edits are picked up live by the config watcher.
    pub fn open_config(&mut self, _: &OpenConfig, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = config::path() else {
            self.set_status("No config directory on this platform", cx);
            return;
        };
        // Write the current state first, so the file exists on a fresh
        // install and reflects this session rather than a stale launch.
        self.save_config();
        cx.open_with_system(&path);
        self.set_status(
            format!("Opened {} — saved edits reload live", path.display()),
            cx,
        );
    }

    /// Open a dialog listing every command and its keybinding
    /// (cmd-h on macOS, F1 elsewhere).
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn show_help(&mut self, _: &ShowHelp, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }

        window.open_dialog(cx, |dialog, _, cx| {
            dialog
                .title("Commands")
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_1()
                        .pb_2()
                        .text_sm()
                        .children(COMMANDS.iter().map(|(keys, description)| {
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(190.))
                                        .flex_none()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(*keys),
                                )
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(*description),
                                )
                        })),
                )
        });
    }

    pub fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.config.zoom + ZOOM_STEP, cx);
    }

    pub fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.config.zoom - ZOOM_STEP, cx);
    }

    pub fn zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(1.0, cx);
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        // Snap to the step grid so repeated f32 steps don't accumulate drift.
        self.config.zoom = (zoom / ZOOM_STEP).round() * ZOOM_STEP;
        self.apply_zoom(cx);
        self.set_status(format!("Zoom {:.0}%", self.config.zoom * 100.), cx);
        self.schedule_save(cx);
    }

    /// Scale the theme font sizes by the configured zoom factor. All
    /// default-sized text follows `font_size` (the root sets the window rem
    /// size from it each frame); the SQL editor follows `mono_font_size`.
    fn apply_zoom(&mut self, cx: &mut Context<Self>) {
        let zoom = self.config.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.config.zoom = zoom;
        let theme = Theme::global_mut(cx);
        theme.font_size = self.base_font_size * zoom;
        theme.mono_font_size = self.base_mono_font_size * zoom;
        cx.refresh_windows();
    }

    /// Format the script through the language server without saving
    /// (cmd-shift-f).
    pub fn format_script(&mut self, _: &FormatScript, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.clone() else {
            self.set_status("Format unavailable — SQL language server not connected", cx);
            return;
        };

        let text = self.editor().read(cx).value().to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.format(&text).await;
            this.update_in(cx, |this, window, cx| match result {
                // Skip stale results: the buffer changed while the
                // server was formatting.
                Ok(Some(formatted)) if this.editor().read(cx).value() == text => {
                    this.apply_formatted(&formatted, window, cx);
                    this.set_status("Formatted script", cx);
                }
                Ok(Some(_)) => {}
                Ok(None) => this.set_status("Script already formatted", cx),
                Err(err) => this.set_status(format!("Format failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Comment or uncomment the current line, or every line the selection
    /// touches, with `--` (cmd-/). Goes through the input handler so the
    /// edit is undoable and the usual change plumbing runs.
    pub fn toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor().update(cx, |state, cx| {
            let selection = state
                .selected_text_range(false, window, cx)
                .map(|sel| sel.range);
            let cursor = state.cursor();

            let text = state.text();
            let selection = selection.map_or(cursor..cursor, |range| {
                text.offset_utf16_to_offset(range.start)..text.offset_utf16_to_offset(range.end)
            });

            // Expand to whole lines. A selection ending at the start of a
            // line does not pull that line in.
            let start_row = text.offset_to_point(selection.start).row;
            let mut end_row = text.offset_to_point(selection.end).row;
            if end_row > start_row && text.offset_to_point(selection.end).column == 0 {
                end_row -= 1;
            }
            let start = text.line_start_offset(start_row);
            let end = text.line_end_offset(end_row);
            let block = text.slice(start..end).to_string();
            let range_utf16 = text.offset_to_offset_utf16(start)..text.offset_to_offset_utf16(end);

            let toggled = toggle_line_comments(&block);
            if toggled == block {
                return;
            }

            // Map the cursor onto the toggled text so it stays put within
            // its line, shifted by that line's inserted/removed prefix.
            let mut new_cursor = start + toggled.len();
            let mut old_line_start = start;
            let mut new_line_start = start;
            for (old_line, new_line) in block.split('\n').zip(toggled.split('\n')) {
                if cursor <= old_line_start + old_line.len() {
                    let column = cursor - old_line_start;
                    let column = if new_line.len() >= old_line.len() {
                        column + (new_line.len() - old_line.len())
                    } else {
                        column.saturating_sub(old_line.len() - new_line.len())
                    };
                    new_cursor = new_line_start + column.min(new_line.len());
                    break;
                }
                old_line_start += old_line.len() + 1;
                new_line_start += new_line.len() + 1;
            }

            state.replace_text_in_range(Some(range_utf16), &toggled, window, cx);
            let position = state.text().offset_to_position(new_cursor);
            state.set_cursor_position(position, window, cx);
        });
    }

    pub fn save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        self.save_active(false, window, cx);
    }

    /// Save the active tab, optionally closing it once the write succeeds
    /// (`close_after` is set by the save-before-close prompt). When the file
    /// changed on disk since our last read or write, prompt before clobbering
    /// it; otherwise write straight through. The check re-stats the file here
    /// rather than trusting the 1s watcher, so a save that races an external
    /// edit still catches it.
    fn save_active(&mut self, close_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active_tab;
        // Either the watcher already flagged it, or it changed in the sub-
        // second race since the last poll (the watcher clears the mtime gap
        // when it flags, so both checks are needed to cover both timings).
        if self.tabs[ix].diverged || self.file_changed_on_disk(ix) {
            // Keep the tab-bar glyph in step in case the watcher hadn't yet.
            self.tabs[ix].diverged = true;
            cx.notify();
            self.prompt_overwrite_diverged(close_after, window, cx);
        } else {
            self.perform_save(close_after, window, cx);
        }
    }

    /// Whether a tab's file exists on disk with a different mtime than the one
    /// we recorded at our last read or write — i.e. it was edited externally.
    /// A missing file is not a conflict: a save simply recreates it.
    fn file_changed_on_disk(&self, ix: usize) -> bool {
        let tab = &self.tabs[ix];
        tab.path
            .as_deref()
            .and_then(file_mtime)
            .is_some_and(|mtime| Some(mtime) != tab.disk_time)
    }

    /// Ask what to do about a tab whose file changed on disk since our last
    /// read or write: keep our version (overwrite), take the disk version
    /// (losing our edits), or cancel.
    fn prompt_overwrite_diverged(
        &mut self,
        close_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let ix = self.active_tab;
        let name = self.tab_label(ix);
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (overwrite, reload) = (app.clone(), app.clone());
            dialog.title("Changed on disk").w(px(460.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(div().text_sm().child(format!(
                        "“{name}” changed on disk since you last opened or saved it. \
                         Overwrite it with your version, or reload the version on disk?"
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
                                Button::new("reload")
                                    .danger()
                                    .label("Reload theirs (lose my edits)")
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        reload
                                            .update(cx, |this, cx| {
                                                this.reload_tab(ix, window, cx);
                                                if close_after {
                                                    this.close_tab_at(ix, window, cx);
                                                }
                                            })
                                            .ok();
                                    }),
                            )
                            .child(
                                Button::new("overwrite")
                                    .primary()
                                    .label("Overwrite")
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        overwrite
                                            .update(cx, |this, cx| {
                                                this.perform_save(close_after, window, cx);
                                            })
                                            .ok();
                                    }),
                            ),
                    ),
            )
        });
    }

    /// Write the active tab to disk, applying format-on-save first when
    /// enabled. Split out from [`Self::save_active`] so the diverged-save
    /// prompt's "Overwrite" can reuse it without re-checking divergence.
    fn perform_save(&mut self, close_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.clone().filter(|_| self.config.format_on_save) else {
            self.write_script(close_after, window, cx);
            return;
        };

        let text = self.editor().read(cx).value().to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.format(&text).await;
            this.update_in(cx, |this, window, cx| {
                let format_error = match result {
                    // Skip stale results: the buffer changed while the
                    // server was formatting.
                    Ok(Some(formatted)) if this.editor().read(cx).value() == text => {
                        this.apply_formatted(&formatted, window, cx);
                        None
                    }
                    Ok(_) => None,
                    Err(err) => Some(err),
                };
                this.write_script(close_after, window, cx);
                if let Some(err) = format_error {
                    this.set_status(format!("Format on save failed: {err}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Swap the formatted text into the editor as a single undoable edit,
    /// keeping the cursor near where it was. Goes through the input
    /// handler so the usual change plumbing (LSP sync, config save) runs.
    fn apply_formatted(&mut self, formatted: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor().update(cx, |state, cx| {
            let cursor = state.cursor();
            let full_range = 0..state.value().chars().map(char::len_utf16).sum();
            state.replace_text_in_range(Some(full_range), formatted, window, cx);

            let mut offset = cursor.min(formatted.len());
            while offset > 0 && !formatted.is_char_boundary(offset) {
                offset -= 1;
            }
            let position = state.text().offset_to_position(offset);
            state.set_cursor_position(position, window, cx);
        });
    }

    /// Write the active tab's content to its script file, prompting for a
    /// location the first time. On success the tab's saved baseline is
    /// updated (clearing its unsaved-edits marker) and, when `close_after`
    /// is set, the tab is closed.
    fn write_script(&mut self, close_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active_tab;
        let content = self.editor().read(cx).value().to_string();

        if let Some(path) = self.tabs[ix].path.clone() {
            match std::fs::write(&path, &content) {
                Ok(()) => {
                    self.mark_saved(ix, content, cx);
                    self.set_status(format!("Saved {}", path.display()), cx);
                    if close_after {
                        self.close_tab_at(ix, window, cx);
                    }
                }
                Err(err) => self.set_status(format!("Save failed: {err}"), cx),
            }
            return;
        }

        let dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&dir, Some("script.sql"));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let result = std::fs::write(&path, &content);

            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.set_status(format!("Saved {}", path.display()), cx);
                    this.set_tab_path(ix, path, window);
                    this.mark_saved(ix, content, cx);
                    this.save_config();
                    if close_after {
                        this.close_tab_at(ix, window, cx);
                    }
                }
                Err(err) => this.set_status(format!("Save failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Adopt `content` as a tab's on-disk baseline, clearing its
    /// unsaved-edits and diverged markers and remembering the file's new
    /// mtime so the watcher doesn't read our own write as an external edit.
    fn mark_saved(&mut self, ix: usize, content: String, cx: &mut Context<Self>) {
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
    fn set_tab_path(&mut self, ix: usize, path: PathBuf, window: &mut Window) {
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
    fn update_window_title(&self, window: &mut Window) {
        match &self.tabs[self.active_tab].path {
            Some(path) => window.set_window_title(&format!("pg-gui — {}", path.display())),
            None => window.set_window_title("pg-gui"),
        }
    }

    /// A tab's display name: its file name, or "untitled".
    fn tab_label(&self, ix: usize) -> String {
        self.tabs[ix]
            .path
            .as_deref()
            .and_then(|path| path.file_name())
            .map_or_else(
                || "untitled".to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
    }

    /// One tab per open script, with a "×" close button each and a
    /// trailing "+" that opens a fresh one. A tab with unsaved edits is
    /// marked with a leading "•", or "⟳" when its file also changed on disk
    /// (diverged). gpui-component ships no icon assets, so these use text
    /// glyphs rather than `IconName` SVGs.
    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        TabBar::new("script-tabs")
            .small()
            .selected_index(self.active_tab)
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.activate_tab(*ix, window, cx);
            }))
            .suffix(
                Button::new("new-tab")
                    .ghost()
                    .small()
                    .label("+")
                    .tooltip("New script tab")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_file(&NewFile, window, cx);
                    })),
            )
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let label = if tab.diverged {
                    format!("⟳ {}", self.tab_label(ix))
                } else if tab.dirty {
                    format!("• {}", self.tab_label(ix))
                } else {
                    self.tab_label(ix)
                };
                Tab::new().label(label).suffix(
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
}

impl Render for PgGuiApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ai_available = ai::api_key(&self.config.ai_api_key).is_some();

        v_flex()
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::run_query))
            .on_action(cx.listener(Self::ai_complete))
            .on_action(cx.listener(Self::new_connection))
            .on_action(cx.listener(Self::connect_recent))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::save_file))
            .on_action(cx.listener(Self::open_snippet_picker))
            .on_action(cx.listener(Self::open_config))
            .on_action(cx.listener(Self::format_script))
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::show_help))
            .on_action(cx.listener(Self::open_github))
            .child(
                // Editor over results, split by a draggable divider. The
                // split position is persisted to the config on drag and
                // restored on launch via the editor panel's initial size.
                div().flex_1().min_h(px(0.)).child(
                    v_resizable("editor-results")
                        .with_state(&self.resizable_state)
                        .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                            if let Some(height) = state.read(cx).sizes().first() {
                                this.config.editor_height = Some(f32::from(*height));
                                this.schedule_save(cx);
                            }
                        }))
                        .child({
                            // Tab bar over the SQL editor
                            let mut panel = resizable_panel().child(
                                v_flex().size_full().child(self.render_tab_bar(cx)).child(
                                    div().flex_1().min_h(px(0.)).p_2().child(
                                        Input::new(&self.editor())
                                            .h_full()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(cx.theme().mono_font_size),
                                    ),
                                ),
                            );
                            if let Some(height) = self.config.editor_height {
                                panel = panel.size(px(height));
                            }
                            panel
                        })
                        .child(
                            // Results table with pager
                            resizable_panel().child(
                                v_flex()
                                    .size_full()
                                    .p_2()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h(px(0.))
                                            .child(Table::new(&self.results)),
                                    )
                                    .children(self.render_results_pager(cx)),
                            ),
                        ),
                ),
            )
            .child(
                // Status bar
                h_flex()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone())
                    .child(div().flex_1())
                    .child(format!(
                        "{} · {} · cmd-h help",
                        if ai_available {
                            "AI ready"
                        } else {
                            "AI off — set ai_api_key or ANTHROPIC_API_KEY"
                        },
                        if self.lsp.is_some() {
                            "SQL LSP connected"
                        } else {
                            "SQL LSP offline"
                        },
                    )),
            )
            // Dialogs (e.g. the snippet picker) are drawn by the app's root
            // element; gpui-component's Root only stores them.
            .children(Root::render_dialog_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{mask_credentials, toggle_line_comments};

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
}
