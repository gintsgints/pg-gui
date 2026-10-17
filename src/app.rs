use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gpui::Subscription;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, Menu,
    MenuItem, NoAction, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    Styled as _, Window, div, px,
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
use gpui_component::{GlobalState, menu::AppMenuBar};

use crate::results::ResultsDelegate;
use crate::{
    AiComplete, CancelQuery, CloseTab, Commit, Connect, DebugContinue, DebugStepInto,
    DebugStepOver, DebugStop, EditConnection, ExplainAnalyze, ExplainPlan, ExportCsv,
    ExportInserts, FormatScript, NewConnection, NewFile, NextDiagnostic, NextTab, OpenConfig,
    OpenFile, OpenFolder, OpenGitHub, OpenRecentFolder, OpenSnippets, PrevDiagnostic, PrevTab,
    Quit, RefreshDbTree, Rollback, RunQuery, RunScripts, SaveFile, SetTheme, ShowHelp, StartDebug,
    ToggleAutocommit, ToggleComment, ToggleDbPanel, ToggleFilesPanel, ToggleFormatOnSave,
    ToggleResultsPanel, ZoomIn, ZoomOut, ZoomReset, config, db, db_tree, debug, definitions,
    file_tree, lsp, snippets, statement,
};

mod connection;
mod debugger;
mod editing;
mod file_ops;
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

/// Menu label for a recent folder: the path with the home directory
/// shortened to `~`, since full paths are long and the menu is narrow.
fn folder_menu_label(path: &Path) -> String {
    let stripped = dirs::home_dir()
        .and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf))
        .map(|rest| format!("~/{}", rest.display()));
    stripped.unwrap_or_else(|| path.display().to_string())
}

/// One View ▸ Theme entry. Native menu items have no checked state
/// reachable from here, so the selected theme is marked with a check-mark
/// prefix and the menu is rebuilt whenever the selection changes.
fn theme_menu_item(theme: config::ThemeSelection, current: config::ThemeSelection) -> MenuItem {
    let label = if theme == current {
        format!("✓ {}", theme.label())
    } else {
        theme.label().to_string()
    };
    MenuItem::action(label, SetTheme(theme))
}

/// The Edit ▸ Format on Save entry, check-marked while the setting is on —
/// same trick (and same reason) as [`theme_menu_item`].
fn format_on_save_menu_item(format_on_save: bool) -> MenuItem {
    let label = if format_on_save {
        "✓ Format on Save"
    } else {
        "Format on Save"
    };
    MenuItem::action(label, ToggleFormatOnSave)
}

/// The application menu bar. Every command lives here now that the toolbar
/// is gone; the OS fills in each item's shortcut from the keybindings in
/// `main.rs`. `recents` becomes the Connection ▸ Recent submenu, each entry
/// carrying its (unmasked) connection string in a [`Connect`] action while
/// showing the user-given name (or the masked connection string when
/// unnamed). `theme` marks the selected View ▸ Theme entry.
fn connection_menu(recents: &[config::RecentConnection]) -> Menu {
    let recent_items = if recents.is_empty() {
        vec![MenuItem::action("No recent connections", NoAction)]
    } else {
        recents
            .iter()
            .map(|c| {
                let label = if c.name.is_empty() {
                    mask_credentials(&c.url)
                } else {
                    c.name.clone()
                };
                MenuItem::action(
                    label,
                    Connect {
                        url: c.url.clone(),
                        name: c.name.clone(),
                    },
                )
            })
            .collect()
    };

    Menu {
        name: "Connection".into(),
        disabled: false,
        items: vec![
            MenuItem::action("New Connection…", NewConnection),
            MenuItem::action("Edit Connection…", EditConnection),
            MenuItem::submenu(Menu {
                name: "Recent".into(),
                disabled: false,
                items: recent_items,
            }),
            MenuItem::separator(),
            MenuItem::action("Run Query", RunQuery),
            MenuItem::action("Run Selected Scripts", RunScripts),
            MenuItem::separator(),
            MenuItem::action("Explain Plan", ExplainPlan),
            MenuItem::action("Explain Analyze", ExplainAnalyze),
            MenuItem::separator(),
            MenuItem::action("Export as CSV…", ExportCsv),
            MenuItem::action("Export as INSERT…", ExportInserts),
        ],
    }
}

/// The File ▸ Open Recent Folder submenu, one entry per remembered
/// working folder, each carrying its path in an [`OpenRecentFolder`]
/// action.
fn recent_folders_menu(recent_folders: &[PathBuf]) -> MenuItem {
    let items = if recent_folders.is_empty() {
        vec![MenuItem::action("No recent folders", NoAction)]
    } else {
        recent_folders
            .iter()
            .map(|dir| MenuItem::action(folder_menu_label(dir), OpenRecentFolder(dir.clone())))
            .collect()
    };
    MenuItem::submenu(Menu {
        name: "Open Recent Folder".into(),
        disabled: false,
        items,
    })
}

fn build_menus(
    recents: &[config::RecentConnection],
    recent_folders: &[PathBuf],
    theme: config::ThemeSelection,
    format_on_save: bool,
) -> Vec<Menu> {
    vec![
        Menu {
            name: "pg-gui".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Preferences…", OpenConfig),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        },
        connection_menu(recents),
        Menu {
            name: "File".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Open…", OpenFile),
                MenuItem::action("Open Folder…", OpenFolder),
                recent_folders_menu(recent_folders),
                MenuItem::action("Save", SaveFile),
                MenuItem::separator(),
                MenuItem::action("New Tab", NewFile),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Format", FormatScript),
                format_on_save_menu_item(format_on_save),
                MenuItem::separator(),
                MenuItem::action("Snippets", OpenSnippets),
                MenuItem::action("AI Complete", AiComplete),
                MenuItem::separator(),
                MenuItem::action("Toggle Comment", ToggleComment),
                MenuItem::separator(),
                MenuItem::action("Next Problem", NextDiagnostic),
                MenuItem::action("Previous Problem", PrevDiagnostic),
            ],
        },
        Menu {
            name: "Session".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Toggle Autocommit", ToggleAutocommit),
                MenuItem::separator(),
                MenuItem::action("Commit", Commit),
                MenuItem::action("Rollback", Rollback),
                MenuItem::separator(),
                MenuItem::action("Cancel Query", CancelQuery),
            ],
        },
        Menu {
            name: "Debug".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Start Debug", StartDebug),
                MenuItem::separator(),
                MenuItem::action("Step Over", DebugStepOver),
                MenuItem::action("Step Into", DebugStepInto),
                MenuItem::action("Continue", DebugContinue),
                MenuItem::separator(),
                MenuItem::action("Stop", DebugStop),
            ],
        },
        Menu {
            name: "View".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Toggle Database Browser", ToggleDbPanel),
                MenuItem::action("Toggle Files Panel", ToggleFilesPanel),
                MenuItem::action("Toggle Results Panel", ToggleResultsPanel),
                MenuItem::action("Refresh Database Browser", RefreshDbTree),
                MenuItem::separator(),
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Actual Size", ZoomReset),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "Theme".into(),
                    disabled: false,
                    items: vec![
                        theme_menu_item(config::ThemeSelection::Light, theme),
                        theme_menu_item(config::ThemeSelection::Dark, theme),
                        theme_menu_item(config::ThemeSelection::System, theme),
                    ],
                }),
            ],
        },
        Menu {
            name: "About".into(),
            disabled: false,
            items: vec![
                MenuItem::action("pg-gui on GitHub", OpenGitHub),
                MenuItem::action("Keyboard Shortcuts", ShowHelp),
            ],
        },
    ]
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

/// Whether an object-browser leaf of this kind has a fetchable definition.
/// Tables are branch nodes (they expand to Indexes/Constraints), so they
/// never reach here.
fn object_kind_has_definition(kind: db_tree::NodeKind) -> bool {
    use db_tree::NodeKind::{
        Constraint, Function, Index, MatView, Sequence, TableDefinition, Trigger, Type, View,
    };
    matches!(
        kind,
        View | MatView
            | Function
            | Sequence
            | Type
            | Index
            | Constraint
            | Trigger
            | TableDefinition
    )
}

/// Fetch the SQL definition of a database object, dispatching to the right
/// catalog query per kind. Runs on the background executor.
fn object_definition(
    conn: &str,
    kind: db_tree::NodeKind,
    schema: &str,
    object: &str,
    relation: &str,
) -> Result<String, String> {
    use db_tree::NodeKind::{
        Constraint, Function, Index, MatView, Sequence, TableDefinition, Trigger, Type, View,
    };
    match kind {
        View => db::view_definition(conn, schema, object, false),
        MatView => db::view_definition(conn, schema, object, true),
        Function => db::function_definition(conn, schema, object),
        Sequence => db::sequence_definition(conn, schema, object),
        Type => db::type_definition(conn, schema, object),
        Index => db::index_definition(conn, schema, object),
        Constraint => db::constraint_definition(conn, schema, relation, object),
        Trigger => db::trigger_definition(conn, schema, relation, object),
        TableDefinition => db::table_definition(conn, schema, object),
        _ => Err("no definition for this object".to_string()),
    }
}

/// Turn the definition file mask into a concrete filename proposed when a
/// definition tab is saved: substitute the object name, then drop the `*`/`?`
/// wildcards (a filename can't contain them) and any separator debris they
/// leave at the front. Falls back to `<object>.sql` if nothing is left.
fn suggested_name_from_mask(mask: &str, object: &str) -> String {
    let name: String = mask
        .replace("{object}", object)
        .chars()
        .filter(|&c| c != '*' && c != '?')
        .collect();
    let trimmed = name.trim_start_matches(['_', '-', '.', ' ']);
    if trimmed.is_empty() {
        format!("{object}.sql")
    } else {
        trimmed.to_string()
    }
}

/// Match a filename glob supporting `*` (any run of characters) and `?` (any
/// single character). `pattern` and `text` are expected already lowercased.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // The last `*` seen and the text position to resume from if the greedy
    // match has to give a character back to it.
    let (mut star, mut resume) = (None, 0);
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            resume = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            resume += 1;
            ti = resume;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

/// Characters that separate the parts of a definition file's name — folders
/// included, so `order_utils/place_order` reads the same way as
/// `order_utils__place_order`.
const NAME_SEPARATORS: [char; 5] = ['_', '-', '.', '/', '\\'];

/// Whether `text` ends with `word` standing on its own, i.e. preceded by a
/// name separator or by nothing at all: `order_utils` ends
/// `test__order_utils` but not `test__xorder_utils`. Both sides are expected
/// already lowercased.
fn ends_with_name_word(text: &str, word: &str) -> bool {
    !word.is_empty()
        && text
            .strip_suffix(word)
            .is_some_and(|rest| rest.is_empty() || rest.ends_with(NAME_SEPARATORS))
}

/// How well a file matching the definition mask fits the clicked object,
/// lower being better. The mask only knows the object name, so several files
/// can match the same glob — with the default `*_{object}.sql`, both
/// `order_utils__place_order.sql` and
/// `test__order_utils__place_order.sql` match object `place_order` —
/// and the schema is what tells them apart: it must be what qualifies the
/// object name, not merely appear somewhere in it.
fn definition_match_rank(path: &Path, root: &Path, schema: &str, object: &str) -> u8 {
    // The file's path below the working folder without its extension: folders
    // qualify the object name just as filename prefixes do.
    let rel = path.strip_prefix(root).unwrap_or(path).with_extension("");
    let rel = rel.to_string_lossy().to_ascii_lowercase();
    // What stands before the object name, if the name is there at all as a
    // whole word.
    let Some(qualifier) = rel.strip_suffix(object) else {
        return 3;
    };
    if !qualifier.is_empty() && !qualifier.ends_with(NAME_SEPARATORS) {
        return 3;
    }
    let qualifier = qualifier.trim_end_matches(NAME_SEPARATORS);
    if qualifier == schema {
        // `order_utils__place_order`, `order_utils/place_order`.
        0
    } else if qualifier.is_empty() {
        // `place_order` — unqualified, so still plausibly ours.
        1
    } else if ends_with_name_word(qualifier, schema) {
        // `test__order_utils__place_order` — our schema is in there, but
        // something else qualifies it.
        2
    } else {
        3
    }
}

/// Words allowed between `CREATE` and the keyword naming the object kind, so
/// `CREATE OR REPLACE VIEW`, `CREATE UNLOGGED TABLE` and
/// `CREATE CONSTRAINT TRIGGER` are all still read as definitions.
const CREATE_MODIFIERS: [&str; 12] = [
    "or",
    "replace",
    "unique",
    "temp",
    "temporary",
    "global",
    "local",
    "unlogged",
    "foreign",
    "recursive",
    "materialized",
    "constraint",
];

/// Words allowed between the keyword naming the object kind and the object's
/// own name.
const NAME_PREFIXES: [&str; 4] = ["if", "not", "exists", "concurrently"];

/// Upper bound on a candidate file read while looking for its `CREATE`
/// statement. Anything bigger is left undecided rather than stalling the walk.
const MAX_DEFINITION_SCAN_BYTES: u64 = 4 * 1024 * 1024;

/// The keyword that names this object kind in the statement defining it, or
/// nothing for kinds that are not objects with a definition.
fn definition_keywords(kind: db_tree::NodeKind) -> &'static [&'static str] {
    use db_tree::NodeKind::{
        Constraint, Function, Index, MatView, Sequence, TableDefinition, Trigger, Type, View,
    };
    match kind {
        TableDefinition => &["table"],
        View | MatView => &["view"],
        // A routine leaf covers both, and neither keyword tells them apart
        // before the name is read.
        Function => &["function", "procedure"],
        Sequence => &["sequence"],
        Type => &["type"],
        Index => &["index"],
        Trigger => &["trigger"],
        Constraint => &["constraint"],
        _ => &[],
    }
}

/// Whether the kind keyword at `i` is the one in a statement that *defines*
/// the object, rather than one that drops, alters or merely mentions it.
fn introduces_definition(words: &[String], i: usize, kind: db_tree::NodeKind) -> bool {
    use db_tree::NodeKind::{Constraint, MatView, View};
    let previous = i.checked_sub(1).map(|p| words[p].as_str());
    match kind {
        // Constraints are defined both inline in a `CREATE TABLE` and by
        // `ALTER TABLE … ADD CONSTRAINT`, so there is no `CREATE` to find;
        // only `DROP CONSTRAINT` names one without defining it.
        Constraint => previous != Some("drop"),
        // The same `view` keyword ends `CREATE VIEW` and `CREATE MATERIALIZED
        // VIEW`, so the word before it is what tells the two kinds apart.
        MatView if previous != Some("materialized") => false,
        View if previous == Some("materialized") => false,
        _ => {
            // Walk back over the modifiers to the `create` they qualify.
            let mut at = i;
            while let Some(previous) = at.checked_sub(1) {
                match words[previous].as_str() {
                    "create" => return true,
                    word if CREATE_MODIFIERS.contains(&word) => at = previous,
                    _ => return false,
                }
            }
            false
        }
    }
}

/// The name defined by the statement whose kind keyword sits at `keyword`:
/// the next word that is not one of the modifiers a name may hide behind,
/// stripped of the argument or column list that follows it.
fn definition_name(words: &[String], keyword: usize) -> Option<&str> {
    let mut at = keyword + 1;
    while NAME_PREFIXES.contains(&words.get(at)?.as_str()) {
        at += 1;
    }
    let word = words.get(at)?;
    let end = word.find('(').unwrap_or(word.len());
    let name = word[..end].trim_matches([',', ';', '"']);
    // `CREATE INDEX ON t (…)` leaves the index unnamed; the keyword that
    // follows is not its name.
    (!name.is_empty() && name != "on").then_some(name)
}

/// How well a file's *contents* fit the clicked object, lower being better.
/// The filename alone cannot tell `tables/V.0.06.01.1__customers.sql` from
/// `upgrade/V.2026.09.06.10.42__customers.sql`; the statement in the body can.
///
/// - 0 — the file defines this very object.
/// - 1 — the file defines no object of this kind, so it says nothing either
///   way (a data script full of `INSERT`s, a scratch query).
/// - 2 — the file defines objects of this kind, but other ones.
///
/// `schema` and `object` are expected already lowercased.
fn definition_content_rank(sql: &str, kind: db_tree::NodeKind, schema: &str, object: &str) -> u8 {
    let keywords = definition_keywords(kind);
    if keywords.is_empty() {
        return 1;
    }
    let stripped = strip_sql_comments(sql);
    let words: Vec<String> = stripped
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    let qualified = format!("{schema}.{object}");
    let mut defines_another = false;
    for i in 0..words.len() {
        if !keywords.contains(&words[i].as_str()) || !introduces_definition(&words, i, kind) {
            continue;
        }
        let Some(name) = definition_name(&words, i) else {
            continue;
        };
        if name == object || name == qualified {
            return 0;
        }
        defines_another = true;
    }
    u8::from(defines_another) + 1
}

/// [`definition_content_rank`] for a file on disk. One that cannot be read, or
/// that is far too big to be a hand-written definition, is left undecided.
fn file_content_rank(path: &Path, kind: db_tree::NodeKind, schema: &str, object: &str) -> u8 {
    if path
        .metadata()
        .is_ok_and(|meta| meta.len() > MAX_DEFINITION_SCAN_BYTES)
    {
        return 1;
    }
    std::fs::read_to_string(path)
        .map_or(1, |sql| definition_content_rank(&sql, kind, schema, object))
}

/// Recursively search `dir` for the file holding `schema.object`'s definition,
/// among the files whose name matches `pattern` (a glob with `*`/`?`, already
/// lowercased — see [`config::Config::definition_file_mask`]). The body decides
/// first ([`definition_content_rank`]): a file carrying the object's `CREATE`
/// statement beats one that merely shares its name, which is what keeps a click
/// on `customers` off the `INSERT INTO customers` script sitting beside it.
/// Files whose bodies are equally (un)informative fall back to
/// [`definition_match_rank`], then to the shallowest and alphabetically first
/// path so the same click always opens the same file. Hidden directories and
/// the usual heavy build directories are skipped, and a total-entry budget caps
/// a runaway walk.
fn find_sql_file(
    dir: &Path,
    pattern: &str,
    kind: db_tree::NodeKind,
    schema: &str,
    object: &str,
) -> Option<PathBuf> {
    let schema = schema.to_ascii_lowercase();
    let object = object.to_ascii_lowercase();
    let mut best: Option<(u8, u8, usize, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    let mut budget = 20_000usize;
    'walk: while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                break 'walk;
            }
            budget -= 1;
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                let skip = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with('.') || matches!(name, "target" | "node_modules")
                    });
                if !skip {
                    stack.push(path);
                }
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| glob_match(pattern, &name.to_ascii_lowercase()))
            {
                let candidate = (
                    file_content_rank(&path, kind, &schema, &object),
                    definition_match_rank(&path, dir, &schema, &object),
                    path.components().count(),
                    path,
                );
                if best.as_ref().is_none_or(|current| candidate < *current) {
                    best = Some(candidate);
                }
            }
        }
    }
    best.map(|(_, _, _, path)| path)
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

    /// What the Go to Definition cache knows about a symbol.
    pub(crate) fn definition_cache_get(
        &mut self,
        conn: &str,
        key: &(String, String),
    ) -> definitions::Cached {
        self.definition_cache.get(conn, key)
    }

    /// Remember a Go to Definition lookup, hit or miss.
    pub(crate) fn definition_cache_insert(
        &mut self,
        conn: &str,
        key: (String, String),
        target: Option<definitions::Target>,
    ) {
        self.definition_cache.insert(conn, key, target);
    }

    /// Open the object a `pggui:` link names, i.e. follow a Go to Definition
    /// from the editor. Returns whether the link was ours to handle.
    fn show_definition_document(
        &mut self,
        params: &lsp_types::ShowDocumentParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = definitions::parse_uri(&params.uri) else {
            return false;
        };
        self.open_object(target.kind, &target.schema, &target.object, "", window, cx);
        true
    }

    /// Handle a click on an object leaf in the database browser: open the
    /// object's `.sql` file if one exists in the working directory, otherwise
    /// fetch its definition and open that in a new tab. Folders and
    /// placeholder rows (no backing node) are ignored.
    fn open_db_object(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = db_tree::find(&self.db_nodes, id) else {
            return;
        };
        let (kind, schema, object, relation) = (
            node.kind,
            node.schema.to_string(),
            node.object.to_string(),
            node.relation.to_string(),
        );
        self.open_object(kind, &schema, &object, &relation, window, cx);
    }

    /// Open a database object identified by kind, schema and name: its `.sql`
    /// file from the working directory when one matches, otherwise its
    /// definition fetched from the catalog into a tab. Shared by the object
    /// browser and Go to Definition, which name the same objects the same way
    /// (a routine's `object` is its `name(identity arguments)` signature).
    fn open_object(
        &mut self,
        kind: db_tree::NodeKind,
        schema: &str,
        object: &str,
        relation: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !object_kind_has_definition(kind) {
            return;
        }
        let (schema, object, relation) =
            (schema.to_string(), object.to_string(), relation.to_string());
        // A function leaf's `object` is a `name(args)` signature; the file is
        // named after the bare routine name.
        let stem = object
            .split_once('(')
            .map_or(object.as_str(), |(name, _)| name)
            .to_string();
        let conn = self.config.connection_string.clone();
        let working_dir = self.config.working_dir.clone();
        // Concrete glob for this object, lowercased for case-insensitive match.
        let pattern = self
            .config
            .definition_file_mask
            .replace("{object}", &stem)
            .to_ascii_lowercase();
        // Filename proposed if the fetched definition is later saved.
        let suggested = suggested_name_from_mask(&self.config.definition_file_mask, &stem);

        cx.spawn_in(window, async move |this, cx| {
            if let Some(dir) = working_dir {
                let (match_schema, match_object) = (schema.clone(), stem.clone());
                let found = cx
                    .background_spawn(async move {
                        find_sql_file(&dir, &pattern, kind, &match_schema, &match_object)
                    })
                    .await;
                if let Some(path) = found {
                    this.update_in(cx, |this, window, cx| this.open_path(&path, window, cx))
                        .ok();
                    return;
                }
            }
            let definition = cx
                .background_spawn(async move {
                    object_definition(&conn, kind, &schema, &object, &relation)
                })
                .await;
            this.update_in(cx, |this, window, cx| match definition {
                Ok(sql) => {
                    // Reuse an untouched untitled tab (the active one first) if
                    // there is one, rather than piling up empty tabs.
                    let pristine = if this.tab_is_pristine(this.active_tab, cx) {
                        Some(this.active_tab)
                    } else {
                        (0..this.tabs.len()).find(|&i| this.tab_is_pristine(i, cx))
                    };
                    let ix = match pristine {
                        Some(i) => {
                            this.config.tabs[i].script.clone_from(&sql);
                            this.tabs[i].saved.clone_from(&sql);
                            this.tabs[i].dirty = false;
                            this.tabs[i].editor.update(cx, |state, cx| {
                                set_editor_value(state, sql, window, cx);
                            });
                            i
                        }
                        None => this.add_tab(sql, None, window, cx),
                    };
                    // Propose a save name derived from the file mask.
                    this.tabs[ix].suggested_name = Some(suggested);
                    this.activate_tab(ix, window, cx);
                    this.save_config();
                    this.set_status(format!("Opened definition of {stem}"), cx);
                }
                Err(err) => this.set_status(format!("Definition failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
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

    /// Rebuild the application menu bar, e.g. after the recent-connections
    /// list changes. `set_menus` is on `App`, reached through `Context`.
    fn refresh_menus(&self, cx: &mut Context<Self>) {
        cx.set_menus(build_menus(
            &self.config.recent_connections,
            &self.config.recent_folders,
            self.config.theme,
            self.config.format_on_save,
        ));
        // Feed the same menus to the in-window bar (Linux/Windows) and rebuild
        // it; macOS renders the native bar from `set_menus` alone.
        #[cfg(not(target_os = "macos"))]
        {
            let menus = build_menus(
                &self.config.recent_connections,
                &self.config.recent_folders,
                self.config.theme,
                self.config.format_on_save,
            )
            .into_iter()
            .map(gpui::Menu::owned)
            .collect();
            GlobalState::global_mut(cx).set_app_menus(menus);
            self.app_menu_bar.update(cx, AppMenuBar::reload);
        }
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
        MAX_RECENT_CONNECTIONS, MAX_RECENT_FOLDERS, definition_content_rank, diagnostic_target,
        dialog_start_dir, ends_with_name_word, find_sql_file, folder_menu_label, format_elapsed,
        glob_match, mask_credentials, percent_decode, record_recent, record_recent_folder,
        suggested_name_from_mask, toggle_line_comments,
    };
    use crate::db_tree::NodeKind;

    /// A throwaway folder holding the given files, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str, files: &[&str]) -> Self {
            // Bodies that say nothing about any object, so these files are
            // ranked on their names alone.
            let files: Vec<(&str, &str)> = files.iter().map(|f| (*f, "select 1;")).collect();
            Self::with_contents(name, &files)
        }

        fn with_contents(name: &str, files: &[(&str, &str)]) -> Self {
            let root = std::env::temp_dir().join(format!("pg-gui-app-{name}"));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            for (file, body) in files {
                let path = root.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, body).unwrap();
            }
            Self(root)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn suggested_name_strips_wildcards_and_leading_separators() {
        assert_eq!(
            suggested_name_from_mask("*__{object}.sql", "place_order"),
            "place_order.sql"
        );
        assert_eq!(
            suggested_name_from_mask("create_{object}.sql", "orders"),
            "create_orders.sql"
        );
        assert_eq!(
            suggested_name_from_mask("{object}_def.sql", "orders"),
            "orders_def.sql"
        );
        // A mask that is nothing but wildcards falls back to `<object>.sql`.
        assert_eq!(suggested_name_from_mask("*", "orders"), "orders.sql");
    }

    #[test]
    fn name_words_respect_separators() {
        assert!(ends_with_name_word("test__order_utils", "order_utils"));
        assert!(ends_with_name_word("test/order_utils", "order_utils"));
        assert!(ends_with_name_word("order_utils", "order_utils"));
        assert!(!ends_with_name_word("xorder_utils", "order_utils"));
        assert!(!ends_with_name_word("order_utils__test", "order_utils"));
        assert!(!ends_with_name_word("", "order_utils"));
    }

    #[test]
    fn find_sql_file_prefers_the_clicked_schema() {
        // Both files match the default mask's glob for `place_order`;
        // the schema decides which one belongs to the clicked routine.
        let dir = TempDir::new(
            "schema-match",
            &[
                "order_utils__place_order.sql",
                "test__order_utils__place_order.sql",
            ],
        );
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*_place_order.sql",
                NodeKind::Function,
                "order_utils",
                "place_order"
            ),
            Some(dir.0.join("order_utils__place_order.sql"))
        );
        // The test routine's own name only matches its own file.
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*_order_utils__place_order.sql",
                NodeKind::Function,
                "test",
                "order_utils__place_order"
            ),
            Some(dir.0.join("test__order_utils__place_order.sql"))
        );
    }

    #[test]
    fn find_sql_file_reads_the_schema_from_a_folder() {
        let dir = TempDir::new(
            "schema-folder",
            &[
                "test/order_utils__place_order.sql",
                "order_utils/place_order.sql",
            ],
        );
        // A mask without the leading separator, so the unprefixed file in the
        // schema folder is a candidate too.
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*place_order.sql",
                NodeKind::Function,
                "order_utils",
                "place_order"
            ),
            Some(dir.0.join("order_utils/place_order.sql"))
        );
        // And the file under `test/` belongs to the `test` schema's routine.
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*order_utils__place_order.sql",
                NodeKind::Function,
                "test",
                "order_utils__place_order"
            ),
            Some(dir.0.join("test/order_utils__place_order.sql"))
        );
    }

    #[test]
    fn find_sql_file_prefers_an_unqualified_name_to_a_foreign_schema() {
        let dir = TempDir::new(
            "schema-plain",
            &["test__order_utils__place_order.sql", "_place_order.sql"],
        );
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*_place_order.sql",
                NodeKind::Function,
                "order_utils",
                "place_order"
            ),
            Some(dir.0.join("_place_order.sql"))
        );
    }

    #[test]
    fn find_sql_file_falls_back_to_a_plain_glob_match() {
        // Nothing names the schema, so the only match still wins — and a
        // deeper alternative loses to the shallower one.
        let dir = TempDir::new(
            "schema-none",
            &["01__place_order.sql", "old/02__place_order.sql"],
        );
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*_place_order.sql",
                NodeKind::Function,
                "shop",
                "place_order"
            ),
            Some(dir.0.join("01__place_order.sql"))
        );
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*_no_such_thing.sql",
                NodeKind::Function,
                "shop",
                "no_such_thing"
            ),
            None
        );
    }

    #[test]
    fn find_sql_file_prefers_the_file_that_defines_the_object() {
        // The seed layout that motivated this: the table and the data loaded
        // into it are two files of the same name in sibling folders, and only
        // the body tells them apart.
        let dir = TempDir::with_contents(
            "content-rank",
            &[
                (
                    "upgrade/V.2026.09.06.10.42__customers.sql",
                    "INSERT INTO customers (name) SELECT 'x';",
                ),
                (
                    "tables/V.0.06.01.1__customers.sql",
                    "CREATE TABLE customers (\n    id serial PRIMARY KEY\n);",
                ),
            ],
        );
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*_customers.sql",
                NodeKind::TableDefinition,
                "public",
                "customers"
            ),
            Some(dir.0.join("tables/V.0.06.01.1__customers.sql"))
        );
    }

    #[test]
    fn find_sql_file_demotes_a_file_defining_another_object() {
        // `a_orders.sql` sorts first and both names match the glob equally,
        // so without reading the bodies the wrong file would win.
        let dir = TempDir::with_contents(
            "content-other",
            &[
                ("a_orders.sql", "CREATE TABLE archived_orders (id int);"),
                ("z_orders.sql", "CREATE TABLE orders (id int);"),
            ],
        );
        assert_eq!(
            find_sql_file(
                &dir.0,
                "*_orders.sql",
                NodeKind::TableDefinition,
                "public",
                "orders"
            ),
            Some(dir.0.join("z_orders.sql"))
        );
    }

    #[test]
    fn definition_content_rank_reads_the_create_statement() {
        // The object's own definition, plain and schema-qualified.
        assert_eq!(
            definition_content_rank(
                "CREATE TABLE customers (id serial);",
                NodeKind::TableDefinition,
                "public",
                "customers"
            ),
            0
        );
        assert_eq!(
            definition_content_rank(
                "CREATE TABLE IF NOT EXISTS app.feature_flags (id serial);",
                NodeKind::TableDefinition,
                "app",
                "feature_flags"
            ),
            0
        );
        // No definition of this kind at all — uninformative, not wrong.
        assert_eq!(
            definition_content_rank(
                "INSERT INTO customers (name) VALUES ('x');",
                NodeKind::TableDefinition,
                "public",
                "customers"
            ),
            1
        );
        // A definition, but of something else.
        assert_eq!(
            definition_content_rank(
                "CREATE TABLE orders (id serial);",
                NodeKind::TableDefinition,
                "public",
                "customers"
            ),
            2
        );
        // Dropping the object is not defining it.
        assert_eq!(
            definition_content_rank(
                "DROP TABLE customers;",
                NodeKind::TableDefinition,
                "public",
                "customers"
            ),
            1
        );
        // Comments naming the object do not count as a definition.
        assert_eq!(
            definition_content_rank(
                "-- create table customers\nSELECT 1;",
                NodeKind::TableDefinition,
                "public",
                "customers"
            ),
            1
        );
    }

    #[test]
    fn definition_content_rank_covers_the_other_kinds() {
        // A routine, named right against its argument list.
        assert_eq!(
            definition_content_rank(
                "CREATE OR REPLACE FUNCTION public.add(a int, b int)\nRETURNS int",
                NodeKind::Function,
                "public",
                "add"
            ),
            0
        );
        assert_eq!(
            definition_content_rank(
                "CREATE PROCEDURE place_order(p_customer int) LANGUAGE plpgsql",
                NodeKind::Function,
                "app",
                "place_order"
            ),
            0
        );
        // `view` ends both kinds of view definition; the word before it is
        // what separates them.
        assert_eq!(
            definition_content_rank(
                "CREATE MATERIALIZED VIEW sales AS SELECT 1;",
                NodeKind::MatView,
                "public",
                "sales"
            ),
            0
        );
        assert_eq!(
            definition_content_rank(
                "CREATE MATERIALIZED VIEW sales AS SELECT 1;",
                NodeKind::View,
                "public",
                "sales"
            ),
            1
        );
        assert_eq!(
            definition_content_rank(
                "CREATE OR REPLACE VIEW sales AS SELECT 1;",
                NodeKind::View,
                "public",
                "sales"
            ),
            0
        );
        // Indexes carry modifiers on both sides of their name.
        assert_eq!(
            definition_content_rank(
                "CREATE UNIQUE INDEX CONCURRENTLY orders_pkey ON orders (id);",
                NodeKind::Index,
                "public",
                "orders_pkey"
            ),
            0
        );
        // An unnamed index does not lend its `ON` to the next object.
        assert_eq!(
            definition_content_rank(
                "CREATE INDEX ON orders (id);",
                NodeKind::Index,
                "public",
                "on"
            ),
            1
        );
        // Constraints are defined with no `CREATE` of their own, inline or by
        // `ALTER TABLE`, but dropping one still does not define it.
        assert_eq!(
            definition_content_rank(
                "ALTER TABLE orders ADD CONSTRAINT orders_customer_fk FOREIGN KEY (c);",
                NodeKind::Constraint,
                "public",
                "orders_customer_fk"
            ),
            0
        );
        assert_eq!(
            definition_content_rank(
                "CREATE TABLE orders (\n  CONSTRAINT orders_pk PRIMARY KEY (id)\n);",
                NodeKind::Constraint,
                "public",
                "orders_pk"
            ),
            0
        );
        assert_eq!(
            definition_content_rank(
                "ALTER TABLE orders DROP CONSTRAINT orders_pk;",
                NodeKind::Constraint,
                "public",
                "orders_pk"
            ),
            1
        );
        assert_eq!(
            definition_content_rank(
                "CREATE CONSTRAINT TRIGGER audit AFTER INSERT ON orders",
                NodeKind::Trigger,
                "public",
                "audit"
            ),
            0
        );
    }

    #[test]
    fn glob_match_handles_star_and_question() {
        // Default mask `*__{object}.sql` with object `place_order`.
        let pattern = "*__place_order.sql";
        assert!(glob_match(pattern, "01__place_order.sql"));
        assert!(glob_match(pattern, "create__place_order.sql"));
        assert!(glob_match(pattern, "__place_order.sql"));
        assert!(!glob_match(pattern, "place_order.sql"));
        assert!(!glob_match(pattern, "01__place_order.txt"));
        // `?` matches exactly one character.
        assert!(glob_match("v?.sql", "v1.sql"));
        assert!(!glob_match("v?.sql", "v12.sql"));
        // A trailing `*` may match nothing.
        assert!(glob_match("orders*", "orders"));
        // Default mask `*_{object}.sql` matches both single- and
        // double-underscore separators.
        let default = "*_add.sql";
        assert!(glob_match(default, "r__001_add.sql"));
        assert!(glob_match(default, "01__add.sql"));
        assert!(glob_match(default, "_add.sql"));
        assert!(!glob_match(default, "add.sql"));
    }

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
    fn folder_label_shortens_home() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(folder_menu_label(&home.join("sql")), "~/sql");
        assert_eq!(folder_menu_label(Path::new("/opt/sql")), "/opt/sql");
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
