use std::cell::Cell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gpui::Subscription;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EntityInputHandler as _,
    Focusable as _, Hsla, InteractiveElement as _, IntoElement, Menu, MenuItem, MouseButton,
    NoAction, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath, Root, Sizable as _, StyledExt as _, Theme,
    ThemeMode, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    combobox::{Combobox, ComboboxEvent, ComboboxState},
    h_flex,
    input::{
        Editor, EditorState, Escape as InputEscape, IndentInline, Input, InputEvent, InputState,
        RopeExt as _, TabSize,
    },
    list::{List, ListEvent, ListItem, ListState},
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    searchable_list::{SearchableListItem, SearchableVec},
    tab::{Tab, TabBar},
    table::{DataTable, TableState},
    tooltip::Tooltip,
    tree::{TreeEvent, TreeItem, TreeState, tree},
    v_flex,
};
// Linux and Windows have no OS-native menu bar, so an in-window one is drawn
// in the title bar. macOS uses the real menu bar and skips all of this.
#[cfg(not(target_os = "macos"))]
use gpui_component::{GlobalState, menu::AppMenuBar};

use crate::results::ResultsDelegate;
use crate::{
    AiComplete, CancelQuery, CloseTab, Commit, Connect, DebugContinue, DebugStepInto,
    DebugStepOver, DebugStop, EditConnection, ExportCsv, ExportInserts, FormatScript,
    NewConnection, NewFile, NextTab, OpenConfig, OpenFile, OpenFolder, OpenGitHub,
    OpenRecentFolder, OpenSnippets, PrevTab, Quit, RefreshDbTree, Rollback, RunQuery, RunScripts,
    SaveFile, SetTheme, ShowHelp, StartDebug, ToggleAutocommit, ToggleComment, ToggleDbPanel,
    ToggleFilesPanel, ToggleFormatOnSave, ToggleResultsPanel, ZoomIn, ZoomOut, ZoomReset, ai,
    config, db, db_tree, debug, definitions, export, file_tree, lsp, snippets, statement,
};

/// The project's GitHub page, opened from the About application menu.
const REPO_URL: &str = "https://github.com/gintsgints/pg-gui";

const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 2.0;

/// Which export the Connection ▸ Export menu items requested; maps onto
/// `db::export_csv` / `db::export_inserts`.
#[derive(Clone, Copy)]
enum ExportFormat {
    Csv,
    Inserts,
}

/// Every command with its keybinding(s), shown in the help dialog
/// (cmd-h on macOS, F1 elsewhere). Must mirror the bindings in main.rs.
#[cfg(target_os = "macos")]
const COMMANDS: &[(&str, &str)] = &[
    (
        "cmd-enter / ctrl-enter",
        "Run the selection or the statement at the cursor",
    ),
    (
        "cmd-shift-enter",
        "Run the scripts selected in the files panel",
    ),
    ("cmd-i / ctrl-space", "AI-complete SQL at the cursor"),
    ("cmd-shift-f", "Format the script"),
    ("cmd-/", "Comment or uncomment the line / selection"),
    ("cmd-f", "Find in the script"),
    ("cmd-r", "Find and replace in the script"),
    ("cmd-p", "Insert a snippet"),
    ("cmd-t", "New script tab"),
    ("cmd-w", "Close the tab"),
    ("ctrl-tab / ctrl-shift-tab", "Next / previous tab"),
    ("cmd-o", "Open a SQL script"),
    ("cmd-1", "Show or hide the database browser"),
    ("cmd-b / cmd-2", "Show or hide the files panel"),
    ("cmd-3", "Show or hide the results panel"),
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
    (
        "ctrl-shift-enter",
        "Run the scripts selected in the files panel",
    ),
    ("ctrl-i / ctrl-space", "AI-complete SQL at the cursor"),
    ("ctrl-shift-f", "Format the script"),
    ("ctrl-/", "Comment or uncomment the line / selection"),
    ("ctrl-f", "Find in the script"),
    ("ctrl-r", "Find and replace in the script"),
    ("ctrl-p", "Insert a snippet"),
    ("ctrl-t", "New script tab"),
    ("ctrl-w", "Close the tab"),
    ("ctrl-tab / ctrl-shift-tab", "Next / previous tab"),
    ("ctrl-o", "Open a SQL script"),
    ("ctrl-1", "Show or hide the database browser"),
    ("ctrl-b / ctrl-2", "Show or hide the files panel"),
    ("ctrl-3", "Show or hide the results panel"),
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

/// Number of connections kept in the Recent menu.
const MAX_RECENT_CONNECTIONS: usize = 10;

/// Move the connection at `url` to the front of the recent list (dedup by
/// url, capped), ignoring an empty url. A non-empty `name` labels the
/// entry; an empty one keeps whatever name the url was already recorded
/// under, so simply reconnecting doesn't erase a saved name.
fn record_recent(recents: &mut Vec<config::RecentConnection>, url: &str, name: &str) {
    if url.is_empty() {
        return;
    }
    let name = if name.is_empty() {
        recents
            .iter()
            .find(|c| c.url == url)
            .map_or(String::new(), |c| c.name.clone())
    } else {
        name.to_string()
    };
    recents.retain(|c| c.url != url);
    recents.insert(
        0,
        config::RecentConnection {
            name,
            url: url.to_string(),
        },
    );
    recents.truncate(MAX_RECENT_CONNECTIONS);
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

/// One entry of the title-bar connection combobox: a recent connection,
/// shown by its saved name (or masked url when unnamed) and identified by
/// its unmasked url.
#[derive(Clone)]
struct ConnectionItem {
    url: SharedString,
    label: SharedString,
}

impl SearchableListItem for ConnectionItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.url
    }
}

/// The combobox items for the current recent-connections list, most recent
/// first — the same order as the Connection ▸ Recent menu.
fn connection_items(recents: &[config::RecentConnection]) -> SearchableVec<ConnectionItem> {
    SearchableVec::new(
        recents
            .iter()
            .map(|c| ConnectionItem {
                url: c.url.clone().into(),
                label: if c.name.is_empty() {
                    mask_credentials(&c.url).into()
                } else {
                    c.name.clone().into()
                },
            })
            .collect::<Vec<_>>(),
    )
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

/// The Test Connection outcome, shown inline in the New Connection dialog
/// as green ("succeeded") or red ("failed: …") text.
enum ConnectionTest {
    /// No test run yet — nothing is shown.
    Idle,
    Testing,
    Ok,
    Failed(SharedString),
}

impl Render for ConnectionTest {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (text, color): (SharedString, Hsla) = match self {
            ConnectionTest::Idle => return div(),
            ConnectionTest::Testing => ("Testing connection…".into(), cx.theme().muted_foreground),
            ConnectionTest::Ok => ("Connection succeeded".into(), cx.theme().green),
            ConnectionTest::Failed(message) => (message.clone(), cx.theme().red),
        };
        // `w_full` keeps a long failure message wrapping within the dialog
        // instead of stretching it wider than the window.
        div().w_full().text_sm().text_color(color).child(text)
    }
}

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

    /// Overwrite every field from `parts`. Used when the connection string is
    /// edited directly, to drive the individual fields from the parsed URL.
    fn set(&self, parts: &ConnectionParts, window: &mut Window, cx: &mut App) {
        self.host
            .update(cx, |s, cx| s.set_value(parts.host.clone(), window, cx));
        self.port
            .update(cx, |s, cx| s.set_value(parts.port.clone(), window, cx));
        self.database
            .update(cx, |s, cx| s.set_value(parts.database.clone(), window, cx));
        self.user
            .update(cx, |s, cx| s.set_value(parts.user.clone(), window, cx));
        self.password
            .update(cx, |s, cx| s.set_value(parts.password.clone(), window, cx));
    }
}

/// Percent-decode the reserved characters we encode in [`ConnectionParts::to_url`];
/// leaves any other `%`-sequence (or a lone `%`) untouched.
pub(crate) fn percent_decode(s: &str) -> String {
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
// The bools are independent per-tab flags (edit/disk state, snippet mode,
// autocommit, in-flight); they don't form a state machine, so the
// excessive-bools lint is a false positive here.
#[allow(clippy::struct_excessive_bools)]
struct EditorTab {
    editor: Entity<EditorState>,
    path: Option<PathBuf>,
    /// The content last written to (or read from) disk — the baseline the
    /// buffer is compared against to decide if the tab has unsaved edits.
    /// Empty for a never-saved tab. Not persisted: recomputed on launch
    /// from the file so restored edits still register as unsaved.
    saved: String,
    /// Whether the buffer differs from `saved`, cached so the tab bar can
    /// show a marker without diffing on every frame. Always `false` for an
    /// untitled tab: it has no file to be stale against, and its text is
    /// persisted to config.json, so closing or quitting never prompts.
    dirty: bool,
    /// Mtime of `path` after our last read or write; a newer mtime on disk
    /// means the file was edited externally. `None` for a tab with no file
    /// (or one whose file is missing).
    disk_time: Option<SystemTime>,
    /// Set when the file changed on disk while the tab had unsaved edits, so
    /// a plain reload would clobber one side or the other. Shown with a
    /// distinct tab glyph and enforced with a prompt before overwriting.
    diverged: bool,
    /// Set after inserting a snippet with tab-stop markers: tab visits the
    /// next `$n` marker in the buffer instead of indenting. Cleared by
    /// escape or once no markers remain, so a stray `$1` in hand-written
    /// SQL never hijacks the tab key.
    snippet_mode: bool,
    /// Default filename proposed by the Save As dialog for a never-saved tab
    /// (`None` falls back to `script.sql`). Set when the tab was opened from a
    /// database object's definition, so its file is proposed as
    /// `<object>.sql`. Cleared once the tab has a real path.
    suggested_name: Option<String>,
    _subscription: Subscription,
    /// Stable id, so a query completing after the tab was reordered or
    /// closed still lands on the right tab (indices shift; ids don't).
    id: u64,
    /// This tab's live database session — one persistent connection, opened
    /// lazily on the first Run and isolated from every other tab's. `None`
    /// before the first Run or after it was torn down (connection change,
    /// dropped connection).
    session: Option<db::Session>,
    /// Whether this tab's session commits each Run immediately (ON) or runs
    /// inside a transaction ended by Commit/Rollback (OFF). Seeded from
    /// `config.autocommit`; not persisted per tab.
    autocommit: bool,
    /// A query is in flight on this tab's session (single in-flight per tab).
    running: bool,
    /// Cancels the in-flight query; set while `running`, taken by Cancel.
    cancel: Option<postgres::CancelToken>,
    /// This tab's last query output, mirrored into the shared results table
    /// and log view while the tab is active and restored when it is
    /// reactivated.
    result: TabResult,
}

/// A tab's last query output, mirrored into the shared results table and
/// log view when the tab is active.
#[derive(Default)]
struct TabResult {
    /// One entry per statement of the last Run that returned rows, in the
    /// order they arrived; the result selector switches between them.
    results: Vec<db::ResultSet>,
    /// Index into `results` of the set the table is showing.
    selected: usize,
    /// The cursor is still open on the tab's session, so Fetch More is valid.
    /// Only a lone statement pages, so it always belongs to the last set.
    has_more: bool,
    /// Per-statement messages, shown in the Log view.
    log: Vec<SharedString>,
}

impl TabResult {
    /// The result set the table is showing, if the last Run produced any.
    fn selected(&self) -> Option<&db::ResultSet> {
        self.results.get(self.selected)
    }
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
}

/// Which view the bottom panel shows: the returned rows or the message log.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BottomView {
    Data,
    Log,
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
    ai_running: bool,
    /// Next id handed to a new tab; only ever increments (see [`EditorTab::id`]).
    next_tab_id: u64,
    /// Bumped whenever the active connection changes, so a query that was in
    /// flight against the previous server is discarded (its session not
    /// stored back) when it finally returns.
    db_epoch: u64,
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
fn run_files(
    session: &mut db::Session,
    files: Vec<(String, PathBuf)>,
    batch_size: usize,
    autocommit: bool,
    progress: &db::Progress,
) -> Result<db::RunResult, String> {
    let mut statements = 0;
    let mut more = false;
    for (label, path) in files {
        let sql = std::fs::read_to_string(&path).map_err(|err| format!("{label}: {err}"))?;
        let _ = progress.unbounded_send(db::RunEvent::Log(format!("▶ {label}")));
        let run = session
            .run(&sql, batch_size, autocommit, progress)
            .map_err(|err| format!("{label}: {err}"))?;
        statements += run.statements;
        more = run.more;
    }
    Ok(db::RunResult { statements, more })
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
        record_recent(
            &mut config.recent_connections,
            &config.connection_string,
            "",
        );

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

        let results =
            cx.new(|cx| TableState::new(ResultsDelegate::new(config.page_size), window, cx));
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

        let weak_this = cx.weak_entity();
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
            ai_running: false,
            next_tab_id,
            db_epoch: 0,
            config,
            config_disk_time: config::modified_time(),
            base_font_size: cx.theme().font_size,
            base_mono_font_size: cx.theme().mono_font_size,
            save_generation: 0,
            lsp: None,
            definition_cache: definitions::Cache::default(),
            connections,
            #[cfg(not(target_os = "macos"))]
            app_menu_bar: AppMenuBar::new(cx),
            _subscriptions: subscriptions,
            connection_dialog_subs: Vec::new(),
        };
        this.update_window_title(window);
        this.refresh_menus(cx);
        this.start_lsp(cx);
        this.load_tree(cx);
        if this.config.db_panel_visible {
            this.load_db_schemas(cx);
        }
        Self::watch_files(window, cx);
        this.apply_zoom(cx);
        this
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

    /// Create the editor for one tab and wire it into the change plumbing.
    /// `saved` is the on-disk baseline used for the unsaved-edits marker.
    /// The caller hooks up the language server, if connected.
    /// Build the editor tabs restored from the config at launch, handing
    /// each a fresh id. Returns the tabs and the next free id.
    fn build_initial_tabs(
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

    fn build_tab(
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
        // Only a tab backed by a file can be dirty: an untitled tab's text
        // lives in config.json and is restored on the next launch, so there
        // is nothing to save and nothing to warn about.
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
            cancel: None,
            result: TabResult::default(),
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
    /// An untitled tab is never dirty — see [`Self::build_tab`].
    fn refresh_dirty(&mut self, ix: usize, cx: &mut Context<Self>) {
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
    fn editor(&self) -> Entity<EditorState> {
        self.tabs[self.active_tab].editor.clone()
    }

    /// Index of the tab with `id`, if it still exists (it may have been
    /// closed while a query ran).
    fn tab_index_by_id(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Set a tab's in-flight flag by id (no-op if the tab was closed).
    fn set_tab_running(&mut self, tab_id: u64, running: bool) {
        if let Some(ix) = self.tab_index_by_id(tab_id) {
            self.tabs[ix].running = running;
        }
    }

    /// Whether the active tab has a query in flight.
    fn active_running(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.running)
    }

    /// Whether the active tab's last SELECT left a cursor open (Fetch More).
    fn active_has_more(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.result.has_more)
    }

    /// Whether the active tab runs each statement autocommitted.
    fn active_autocommit(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_none_or(|tab| tab.autocommit)
    }

    /// Whether the active tab's session has an open (autocommit-off) transaction.
    fn active_in_txn(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.session.as_ref().is_some_and(db::Session::in_txn))
    }

    /// Mirror the tab's stored result and log into the shared results table
    /// and Log view.
    fn show_tab_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        let (columns, rows) = tab.result.selected().map_or_else(
            || (Vec::new(), db::Rows::new()),
            |set| (set.columns.clone(), set.rows.clone()),
        );
        self.log = tab.result.log.clone();
        self.results.update(cx, |table, cx| {
            table.delegate_mut().set_data(columns, rows);
            table.refresh(cx);
        });
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

    /// Close a tab, but prompt first when it has an open transaction (which
    /// closing would roll back) or unsaved edits.
    fn request_close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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
    /// prompt for unsaved edits, otherwise close.
    fn after_txn_close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && self.tabs[ix].dirty {
            self.prompt_save_before_close(ix, window, cx);
        } else {
            self.close_tab_at(ix, window, cx);
        }
    }

    /// Warn before closing a tab whose session has an open transaction —
    /// closing drops the connection, which rolls the transaction back.
    /// Committing on close is intentionally not offered; commit explicitly
    /// first if the work should be kept.
    fn prompt_txn_before_close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Quit, but confirm first when any tab has unsaved edits. cmd-q and
    /// the window's close button both funnel here.
    pub fn request_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_unsaved_tabs() {
            self.prompt_quit(window, cx);
        } else {
            cx.quit();
        }
    }

    fn has_unsaved_tabs(&self) -> bool {
        self.tabs.iter().any(|tab| tab.dirty)
    }

    /// Ask for confirmation before quitting with unsaved edits. The
    /// buffers themselves survive a quit (they're restored from
    /// config.json on the next launch); it's the tabs' files on disk that
    /// would be left stale.
    fn prompt_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        // Replacing the buffer emits no Change event, so the bookkeeping that
        // one would have done — baseline, config mirror, dirty marker — is all
        // done here.
        self.tabs[ix].saved.clone_from(&content);
        self.tabs[ix].disk_time = file_mtime(&path);
        self.tabs[ix].diverged = false;
        self.config.tabs[ix].script.clone_from(&content);
        self.tabs[ix].editor.update(cx, |state, cx| {
            let mut offset = state.cursor().min(content.len());
            while offset > 0 && !content.is_char_boundary(offset) {
                offset -= 1;
            }
            set_editor_value(state, content, window, cx);
            let position = state.text().offset_to_position(offset);
            state.set_cursor_position(position, window, cx);
        });
        self.refresh_dirty(ix, cx);
        // The server tracks the active tab's document only.
        if ix == self.active_tab
            && let Some(client) = &self.lsp
        {
            client.document_changed(self.config.tabs[ix].script.clone());
        }
        self.schedule_save(cx);
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
                "",
            );
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

    /// Insert a snippet at the cursor as its own statement. A snippet with
    /// `$n` tab stops enters snippet mode and jumps to the first stop;
    /// otherwise, when it contains a `%%` filter placeholder, the caret is
    /// placed between the two `%` so typing narrows the filter.
    fn insert_snippet(
        &mut self,
        snippet: &snippets::Snippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let has_stops = snippets::has_tab_stops(&snippet.sql);
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

            let placeholder = inserted.find("%%").filter(|_| !has_stops);
            state.insert(inserted.clone(), window, cx);
            if let Some(pos) = placeholder {
                let target = state.cursor() - inserted.len() + pos + 1;
                let position = state.text().offset_to_position(target);
                state.set_cursor_position(position, window, cx);
            } else {
                state.focus(window, cx);
            }
        });
        if has_stops {
            self.tabs[self.active_tab].snippet_mode = true;
            self.next_snippet_stop(false, window, cx);
        }
        self.set_status(format!("Inserted “{}”", snippet.name), cx);
    }

    /// Jump to the buffer's next `$n` tab stop: the marker is replaced by
    /// its placeholder text, which is left selected so typing overwrites
    /// it. Returns false when no marker remains.
    fn next_snippet_stop(
        &mut self,
        braced_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor().update(cx, |state, cx| {
            let Some(stop) = snippets::next_tab_stop(&state.value(), braced_only) else {
                return false;
            };
            state.set_selected_range(stop.range, cx);
            state.replace(stop.placeholder.clone(), window, cx);
            let end = state.cursor();
            state.set_selected_range(end - stop.placeholder.len()..end, cx);
            state.focus(window, cx);
            true
        })
    }

    /// Capture-phase tab handler: tab visits the buffer's next tab stop
    /// instead of indenting. In snippet mode (after a picker insertion)
    /// every marker form is serviced; otherwise only the braced forms —
    /// left by an accepted completion suggestion — so a hand-written
    /// Postgres parameter (`$1`) never captures the tab key.
    fn on_editor_tab(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor().focus_handle(cx).is_focused(window) {
            return;
        }
        let snippet_mode = self.tabs[self.active_tab].snippet_mode;
        if self.next_snippet_stop(!snippet_mode, window, cx) {
            cx.stop_propagation();
        } else if snippet_mode {
            self.tabs[self.active_tab].snippet_mode = false;
        }
    }

    /// Escape ends snippet mode (and still bubbles on to the editor).
    fn on_editor_escape(&mut self, _: &InputEscape, _: &mut Window, _: &mut Context<Self>) {
        self.tabs[self.active_tab].snippet_mode = false;
    }

    /// The SQL an action should operate on, with a scope label for status
    /// messages: the selected block when there is a selection, otherwise
    /// the statement the cursor is on (or, when the cursor sits after a
    /// statement, the one to its left), which gets selected so it's clear
    /// which one was used.
    fn sql_under_cursor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(String, &'static str)> {
        let selection = self.editor().update(cx, |state, cx| {
            state
                .selected_text_range(false, window, cx)
                .filter(|sel| !sel.range.is_empty())
                .and_then(|sel| state.text_for_range(sel.range, &mut None, window, cx))
                .filter(|text| !text.trim().is_empty())
        });
        if let Some(sql) = selection {
            return Some((sql, "selection"));
        }
        let sql = self.editor().update(cx, |state, cx| {
            let text = state.value();
            let range = statement::at(&text, state.cursor())?;
            let sql = text[range.clone()].to_string();
            state.set_selected_range(range, cx);
            Some(sql)
        })?;
        Some((sql, "statement"))
    }

    pub fn run_query(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        // The input may be showing the masked value; the config always
        // holds the real connection string.
        let conn = self.config.connection_string.clone();

        let Some((sql, scope)) = self.sql_under_cursor(window, cx) else {
            self.set_status("Nothing to run", cx);
            return;
        };

        let ix = self.active_tab;
        let tab_id = self.tabs[ix].id;
        let autocommit = self.tabs[ix].autocommit;
        let batch_size = self.config.fetch_size.max(1);
        let epoch = self.db_epoch;
        // Move the tab's live session into the worker (opened lazily below on
        // the first Run). A single SELECT-style statement pages through a
        // server-side cursor on that connection; scripts and DML run directly.
        let session = self.tabs[ix].session.take();
        self.tabs[ix].running = true;
        // The run streams its own result sets in; drop the previous Run's so
        // the selector only ever shows this one's.
        self.tabs[ix].result.results.clear();
        self.tabs[ix].result.selected = 0;
        self.tabs[ix].result.has_more = false;
        if ix == self.active_tab {
            self.show_tab_result(ix, cx);
        }
        self.set_status(format!("Running {scope}…"), cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            // Open the session on the first Run, off the UI thread.
            let opened = cx
                .background_spawn(async move {
                    match session {
                        Some(session) => Ok(session),
                        None => db::Session::connect(&conn)
                            .map_err(|e| format!("connection failed: {}", db::describe(&e))),
                    }
                })
                .await;
            let mut session = match opened {
                Ok(session) => session,
                Err(err) => {
                    this.update_in(cx, |this, window, cx| {
                        this.on_query_error(tab_id, epoch, None, &err, window, cx);
                    })
                    .ok();
                    return;
                }
            };
            // Publish the cancel token so Cancel can abort the query below.
            let token = session.cancel_token();
            this.update(cx, |this, _| {
                if let Some(ix) = this.tab_index_by_id(tab_id) {
                    this.tabs[ix].cancel = Some(token);
                }
            })
            .ok();

            // The run pushes a log line (and a result set) per statement as
            // it goes; apply them here while the block keeps running, then
            // wait for the run itself to finish.
            let (tx, mut events) = futures::channel::mpsc::unbounded();
            let running = cx.background_spawn(async move {
                let result = session.run(&sql, batch_size, autocommit, &tx);
                (session, result)
            });
            while let Some(event) = events.next().await {
                this.update(cx, |this, cx| this.on_run_event(tab_id, epoch, event, cx))
                    .ok();
            }
            let (session, result) = running.await;
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| match result {
                Ok(run) => this.on_query_ok(tab_id, epoch, session, &run, scope, elapsed, cx),
                Err(err) => this.on_query_error(tab_id, epoch, Some(session), &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The picked scripts as `(label, path)`, in run order. The label is
    /// the path relative to the working folder — what the panel shows —
    /// since the absolute path is mostly noise in a log line.
    fn selected_script_batch(&self) -> Vec<(String, PathBuf)> {
        let root = self.config.working_dir.as_ref();
        self.selected_script_paths()
            .into_iter()
            .map(|path| {
                let label = root
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(path.as_path())
                    .display()
                    .to_string();
                (label, path)
            })
            .collect()
    }

    /// A batch reads its scripts from disk, so a tab still holding unsaved
    /// edits to one of them would run something other than what its editor
    /// shows; note that in the log rather than silently running the old
    /// text.
    fn note_unsaved_scripts(&mut self, ix: usize, files: &[(String, PathBuf)]) {
        let unsaved: Vec<String> = files
            .iter()
            .filter(|(_, path)| {
                self.tabs
                    .iter()
                    .any(|tab| tab.dirty && tab.path.as_deref() == Some(path.as_path()))
            })
            .map(|(label, _)| label.clone())
            .collect();
        for label in unsaved {
            self.tabs[ix].result.log.push(SharedString::from(format!(
                "note: {label} has unsaved edits in a tab — the file on disk is what runs"
            )));
        }
    }

    /// Run the scripts picked in the files panel, in tree order, on the
    /// active tab's session — so they see that tab's autocommit setting,
    /// its open transaction, and report into its log next to ordinary Runs.
    /// Each file is a separate `Session::run`, headed by its own log line,
    /// and the batch stops at the first file that fails (like
    /// `psql -v ON_ERROR_STOP=1`, which is how `sql/00-run-init.sh` applies
    /// the same files). Files that already ran stay committed unless the
    /// tab is holding a transaction open.
    pub fn run_scripts(&mut self, _: &RunScripts, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        let files = self.selected_script_batch();
        if files.is_empty() {
            self.set_status("No scripts selected", cx);
            return;
        }
        self.prompt_run_scripts(files, window, cx);
    }

    /// The name the current connection goes by in the UI — its saved name,
    /// or its URL with the password masked when it has none.
    fn connection_label(&self) -> String {
        self.config
            .recent_connections
            .iter()
            .find(|c| c.url == self.config.connection_string && !c.name.is_empty())
            .map_or_else(
                || mask_credentials(&self.config.connection_string),
                |c| c.name.clone(),
            )
    }

    /// Confirm a batch before it runs. A batch is the one action here that
    /// executes files the user is not looking at — several of them, against
    /// whatever connection the window happens to be on — so it names the
    /// target and lists the files rather than starting on a stray
    /// cmd-shift-enter.
    fn prompt_run_scripts(
        &mut self,
        files: Vec<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A long selection would push the buttons off-screen; the tail is
        // summarised instead.
        const SHOWN: usize = 12;

        if window.has_active_dialog(cx) {
            return;
        }
        let count = files.len();
        let target = self.connection_label();
        let listed: Vec<String> = files
            .iter()
            .take(SHOWN)
            .map(|(label, _)| label.clone())
            .collect();
        let rest = count.saturating_sub(listed.len());
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (run, files) = (app.clone(), files.clone());
            let mut list = v_flex().gap_0p5();
            for label in listed.clone() {
                list = list.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                );
            }
            if rest > 0 {
                list = list.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("…and {rest} more")),
                );
            }
            dialog
                .title("Run scripts")
                // Enter runs, Escape cancels (the dialog's own binding).
                .on_ok({
                    let (run, files) = (run.clone(), files.clone());
                    move |_, window, cx| {
                        let files = files.clone();
                        run.update(cx, |this, cx| this.start_script_run(files, window, cx))
                            .is_ok()
                    }
                })
                .w(px(480.))
                .child(
                    v_flex()
                        .gap_4()
                        .pb_2()
                        .child(
                            div()
                                .text_sm()
                                .child(format!("Run {count} script(s) on “{target}”?")),
                        )
                        .child(list)
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
                                    Button::new("run")
                                        .primary()
                                        .label(format!("Run {count} script(s)"))
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let files = files.clone();
                                            run.update(cx, |this, cx| {
                                                this.start_script_run(files, window, cx);
                                            })
                                            .ok();
                                        }),
                                ),
                        ),
                )
        });
    }

    /// Run a confirmed batch: see [`PgGuiApp::run_scripts`].
    fn start_script_run(
        &mut self,
        files: Vec<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running {
            return;
        }
        let count = files.len();

        let conn = self.config.connection_string.clone();
        let ix = self.active_tab;
        let tab_id = self.tabs[ix].id;
        let autocommit = self.tabs[ix].autocommit;
        let batch_size = self.config.fetch_size.max(1);
        let epoch = self.db_epoch;
        let session = self.tabs[ix].session.take();
        self.tabs[ix].running = true;
        self.tabs[ix].result.results.clear();
        self.tabs[ix].result.selected = 0;
        self.tabs[ix].result.has_more = false;
        self.note_unsaved_scripts(ix, &files);
        // The per-file lines are the point of a batch; show the log rather
        // than whatever table the tab was left on, and reveal the panel if
        // it was hidden — otherwise the run reports into nothing visible.
        self.bottom_view = BottomView::Log;
        if !self.config.results_panel_visible {
            self.config.results_panel_visible = true;
            self.schedule_save(cx);
        }
        self.show_tab_result(ix, cx);
        self.set_status(format!("Running {count} script(s)…"), cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let opened = cx
                .background_spawn(async move {
                    match session {
                        Some(session) => Ok(session),
                        None => db::Session::connect(&conn)
                            .map_err(|e| format!("connection failed: {}", db::describe(&e))),
                    }
                })
                .await;
            let mut session = match opened {
                Ok(session) => session,
                Err(err) => {
                    this.update_in(cx, |this, window, cx| {
                        this.on_query_error(tab_id, epoch, None, &err, window, cx);
                    })
                    .ok();
                    return;
                }
            };
            let token = session.cancel_token();
            this.update(cx, |this, _| {
                if let Some(ix) = this.tab_index_by_id(tab_id) {
                    this.tabs[ix].cancel = Some(token);
                }
            })
            .ok();

            let (tx, mut events) = futures::channel::mpsc::unbounded();
            let running = cx.background_spawn(async move {
                let result = run_files(&mut session, files, batch_size, autocommit, &tx);
                (session, result)
            });
            while let Some(event) = events.next().await {
                this.update(cx, |this, cx| this.on_run_event(tab_id, epoch, event, cx))
                    .ok();
            }
            let (session, result) = running.await;
            let elapsed = started.elapsed();
            let scope = format!("{count} script(s)");

            this.update_in(cx, |this, window, cx| match result {
                Ok(run) => this.on_query_ok(tab_id, epoch, session, &run, &scope, elapsed, cx),
                Err(err) => this.on_query_error(tab_id, epoch, Some(session), &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Put a finished query's session back on its tab and clear the running
    /// state. Returns the tab index to display the result on, or `None` when
    /// the tab was closed, the connection changed underneath it, or the
    /// connection died (session discarded so the next Run reconnects).
    fn settle_session(&mut self, tab_id: u64, epoch: u64, session: db::Session) -> Option<usize> {
        let ix = self.tab_index_by_id(tab_id)?;
        self.tabs[ix].running = false;
        self.tabs[ix].cancel = None;
        if epoch != self.db_epoch || session.is_closed() {
            self.tabs[ix].session = None;
            self.tabs[ix].result.has_more = false;
            return None;
        }
        self.tabs[ix].session = Some(session);
        Some(ix)
    }

    /// Apply one streamed run event to its tab: a log line, or a result set
    /// that joins the selector and is shown right away, so a long block's
    /// output appears statement by statement instead of all at the end.
    fn on_run_event(
        &mut self,
        tab_id: u64,
        epoch: u64,
        event: db::RunEvent,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.db_epoch {
            return;
        }
        let Some(ix) = self.tab_index_by_id(tab_id) else {
            return;
        };
        let mut rows = false;
        match event {
            db::RunEvent::Log(line) | db::RunEvent::Notice(line) => {
                self.tabs[ix].result.log.push(SharedString::from(line));
            }
            db::RunEvent::Result(set) => {
                rows = !set.rows.is_empty();
                let result = &mut self.tabs[ix].result;
                result.selected = result.results.len();
                result.results.push(set);
            }
        }
        if ix == self.active_tab {
            self.show_tab_result(ix, cx);
        }
        // Reveal the results panel (cmd-3) when a statement returns rows so
        // the output isn't silently hidden, and switch the panel back to the
        // table in case the log was left showing from an earlier run.
        if rows {
            self.bottom_view = BottomView::Data;
            if !self.config.results_panel_visible {
                self.config.results_panel_visible = true;
                self.schedule_save(cx);
            }
        }
        cx.notify();
    }

    /// Settle a finished run: put its session back on the tab and report what
    /// it did. The rows and log lines already arrived through
    /// [`Self::on_run_event`].
    #[allow(clippy::too_many_arguments)]
    fn on_query_ok(
        &mut self,
        tab_id: u64,
        epoch: u64,
        session: db::Session,
        run: &db::RunResult,
        scope: &str,
        elapsed: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.settle_session(tab_id, epoch, session) else {
            return;
        };
        let db::RunResult { statements, more } = *run;
        self.tabs[ix].result.has_more = more;
        let result = &self.tabs[ix].result;
        let row_count = result.selected().map_or(0, |set| set.rows.len());
        let sets = result.results.len();
        if ix == self.active_tab {
            cx.notify();
        }
        let more_txt = if more { ", more available" } else { "" };
        let sets_txt = if sets > 1 {
            format!(" of {sets} result sets")
        } else {
            String::new()
        };
        self.set_status(
            format!(
                "{scope}: {statements} statement(s) executed in {elapsed:.0?} — showing {row_count} row(s){sets_txt}{more_txt}"
            ),
            cx,
        );
    }

    /// Record a failed query on its tab: keep a live session (so an aborted
    /// transaction can still be rolled back), discard a dead or superseded
    /// one, clear the tab's result, and surface the error.
    fn on_query_error(
        &mut self,
        tab_id: u64,
        epoch: u64,
        session: Option<db::Session>,
        err: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tab_index_by_id(tab_id) {
            self.tabs[ix].running = false;
            self.tabs[ix].cancel = None;
            self.tabs[ix].session = session.filter(|s| epoch == self.db_epoch && !s.is_closed());
            let result = &mut self.tabs[ix].result;
            // The statements that did run were logged as they went; the error
            // closes the log off. Their result sets stay on the selector.
            result.log.push(SharedString::from(err.to_string()));
            result.has_more = false;
            if ix == self.active_tab {
                // Switch to the log so the error line the run just appended is
                // visible instead of hiding behind the table.
                self.bottom_view = BottomView::Log;
                self.show_tab_result(ix, cx);
            }
        }
        self.show_query_error(err, window, cx);
    }

    /// Toggle the active tab's autocommit mode. Turning it ON commits any
    /// transaction the tab left open while it was OFF.
    fn toggle_autocommit(
        &mut self,
        _: &ToggleAutocommit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = self.active_tab;
        if self.tabs.get(ix).is_none_or(|tab| tab.running) {
            return;
        }
        let now_on = !self.tabs[ix].autocommit;
        self.tabs[ix].autocommit = now_on;
        // Remember the toggle as the default that seeds future tabs and
        // survives restart; without this the change is lost on exit.
        self.config.autocommit = now_on;
        self.save_config();
        if now_on && self.active_in_txn() {
            self.end_txn(true, window, cx);
        } else {
            self.set_status(
                format!("Autocommit {}", if now_on { "on" } else { "off" }),
                cx,
            );
            cx.notify();
        }
    }

    fn commit_txn(&mut self, _: &Commit, window: &mut Window, cx: &mut Context<Self>) {
        self.end_txn(true, window, cx);
    }

    fn rollback_txn(&mut self, _: &Rollback, window: &mut Window, cx: &mut Context<Self>) {
        self.end_txn(false, window, cx);
    }

    /// Commit or roll back the active tab's open transaction on its session.
    fn end_txn(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active_tab;
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        // Nothing to do without an open transaction, and never mid-query.
        if tab.running || !tab.session.as_ref().is_some_and(db::Session::in_txn) {
            return;
        }
        let Some(mut session) = tab.session.take() else {
            return;
        };
        let tab_id = tab.id;
        let epoch = self.db_epoch;
        tab.running = true;
        self.set_status(
            if commit {
                "Committing…"
            } else {
                "Rolling back…"
            },
            cx,
        );

        cx.spawn_in(window, async move |this, cx| {
            let (session, result) = cx
                .background_spawn(async move {
                    let result = if commit {
                        session.commit()
                    } else {
                        session.rollback()
                    };
                    (session, result)
                })
                .await;

            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    if let Some(ix) = this.settle_session(tab_id, epoch, session) {
                        // Ending the transaction closed any cursor with it.
                        this.tabs[ix].result.has_more = false;
                    }
                    this.set_status(if commit { "Committed" } else { "Rolled back" }, cx);
                    cx.notify();
                }
                Err(err) => {
                    this.on_query_error(tab_id, epoch, Some(session), &err, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Cancel the query in flight on the active tab, over a side connection.
    /// The in-flight Run reports the cancellation and clears its own running
    /// state when it returns.
    fn cancel_query(&mut self, _: &CancelQuery, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if !tab.running {
            return;
        }
        let Some(token) = tab.cancel.clone() else {
            return;
        };
        self.set_status("Cancelling…", cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { db::cancel(&token) }).await;
            if let Err(err) = result {
                this.update(cx, |this, cx| {
                    this.set_status(format!("Cancel failed: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn export_csv(&mut self, _: &ExportCsv, window: &mut Window, cx: &mut Context<Self>) {
        self.export_results(ExportFormat::Csv, window, cx);
    }

    fn export_inserts(&mut self, _: &ExportInserts, window: &mut Window, cx: &mut Context<Self>) {
        self.export_results(ExportFormat::Inserts, window, cx);
    }

    /// Prompt for a destination file, then re-run the selection or the
    /// statement at the cursor in the background and stream its result
    /// there — CSV via server-side `COPY TO STDOUT`, or as generated
    /// INSERT statements.
    fn export_results(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_running() {
            return;
        }
        let conn = self.config.connection_string.clone();
        let Some((sql, scope)) = self.sql_under_cursor(window, cx) else {
            self.set_status("Nothing to export", cx);
            return;
        };
        let tab_id = self.tabs[self.active_tab].id;

        let default_name = export::default_file_name(
            &sql,
            match format {
                ExportFormat::Csv => "csv",
                ExportFormat::Inserts => "sql",
            },
        );
        let rx = cx.prompt_for_new_path(&self.start_dir(), Some(&default_name));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            this.update(cx, |this, cx| {
                this.set_tab_running(tab_id, true);
                this.set_status(format!("Exporting {scope}…"), cx);
            })
            .ok();

            let started = std::time::Instant::now();
            let result = cx
                .background_spawn({
                    let path = path.clone();
                    async move {
                        match format {
                            ExportFormat::Csv => db::export_csv(&conn, &sql, &path)
                                .map(|bytes| format!("{bytes} byte(s)")),
                            ExportFormat::Inserts => db::export_inserts(&conn, &sql, &path)
                                .map(|rows| format!("{rows} row(s)")),
                        }
                    }
                })
                .await;
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| {
                this.set_tab_running(tab_id, false);
                match result {
                    Ok(detail) => {
                        this.remember_dir(&path);
                        this.save_config();
                        this.set_status(
                            format!("Exported {detail} to {} in {elapsed:.0?}", path.display()),
                            cx,
                        );
                    }
                    Err(err) => this.show_query_error(&err, window, cx),
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

    /// Create the title-bar connection combobox, selecting the active
    /// connection (at the head of the recent list), and the subscription
    /// that connects to whatever gets picked from it.
    fn build_connection_combo(
        config: &config::Config,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (
        Entity<ComboboxState<SearchableVec<ConnectionItem>>>,
        Subscription,
    ) {
        let connections = cx.new(|cx| {
            ComboboxState::new(
                connection_items(&config.recent_connections),
                vec![IndexPath::default()],
                window,
                cx,
            )
            .searchable(true)
        });
        let subscription = cx.subscribe_in(
            &connections,
            window,
            |this, _, event: &ComboboxEvent<SearchableVec<ConnectionItem>>, window, cx| {
                if let ComboboxEvent::Confirm(values) = event
                    && let Some(url) = values.first()
                {
                    let name = this
                        .config
                        .recent_connections
                        .iter()
                        .find(|c| c.url == url.as_ref())
                        .map_or(String::new(), |c| c.name.clone());
                    this.apply_connection(&url.clone(), &name, window, cx);
                }
            },
        );
        (connections, subscription)
    }

    /// Mirror `config.recent_connections` into the title-bar combobox and
    /// mark the active connection selected.
    fn sync_connection_combo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = connection_items(&self.config.recent_connections);
        let selected = self
            .config
            .recent_connections
            .iter()
            .position(|c| c.url == self.config.connection_string)
            .map(|row| IndexPath::default().row(row));
        self.connections.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_indices(selected, window, cx);
        });
    }

    /// Connection ▸ New Connection…: open the connection form on a fresh
    /// default connection to fill in.
    pub fn new_connection(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_connection_form("New connection", &default_conn(), "", window, cx);
    }

    /// Connection ▸ Edit Connection…: open the connection form seeded with
    /// the current connection's details, to update it in place.
    pub fn edit_connection(
        &mut self,
        _: &EditConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url = self.config.connection_string.clone();
        // Seed the name from the matching recent entry, so an already-named
        // connection shows its name.
        let name = self
            .config
            .recent_connections
            .iter()
            .find(|c| c.url == url)
            .map_or(String::new(), |c| c.name.clone());
        self.open_connection_form("Edit connection", &url, &name, window, cx);
    }

    /// Build the field inputs seeded from `seed_url`/`seed_name`, wire up the
    /// live connection-string preview, and show the connection dialog titled
    /// `title`. Shared by New Connection and Edit Connection.
    fn open_connection_form(
        &mut self,
        title: &'static str,
        seed_url: &str,
        seed_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let parts = ConnectionParts::parse(seed_url);
        let mut field = |value: String, placeholder: &str, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let name = field(seed_name.to_string(), "My database (optional)", cx);
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
        let preview = cx.new(|cx| InputState::new(window, cx).default_value(seed_url.to_string()));
        let test_status = cx.new(|_| ConnectionTest::Idle);

        self.connection_dialog_subs =
            Self::wire_connection_sync(&fields, &preview, &test_status, window, cx);

        Self::open_connection_dialog(title, name, fields, preview, test_status, window, cx);
    }

    /// Wire the two-way sync between the individual fields and the editable
    /// connection-string preview: a field edit recomputes the string, and a
    /// string edit re-parses it back into the fields. A shared re-entrancy
    /// flag stops one side's programmatic write from echoing back through the
    /// other. Any edit also drops a stale Test Connection result. Returns the
    /// subscriptions, which the caller keeps alive for the dialog's lifetime.
    fn wire_connection_sync(
        fields: &ConnectionFields,
        preview: &Entity<InputState>,
        test_status: &Entity<ConnectionTest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        // While one side programmatically writes the other, the other's Change
        // event is ignored instead of writing straight back.
        let syncing = Rc::new(Cell::new(false));

        let clear_test = {
            let test_status = test_status.clone();
            move |cx: &mut App| {
                test_status.update(cx, |status, cx| {
                    if !matches!(status, ConnectionTest::Idle) {
                        *status = ConnectionTest::Idle;
                        cx.notify();
                    }
                });
            }
        };

        // Field edit: recompute the previewed connection string.
        let recompute = {
            let (fields, preview, syncing, clear_test) = (
                fields.clone(),
                preview.clone(),
                syncing.clone(),
                clear_test.clone(),
            );
            move |window: &mut Window, cx: &mut App| {
                if syncing.get() {
                    return;
                }
                let url = fields.read(cx).to_url();
                syncing.set(true);
                preview.update(cx, |state, cx| state.set_value(url, window, cx));
                syncing.set(false);
                clear_test(cx);
            }
        };
        // String edit: parse it and drive the fields. Skipped for a partial
        // entry that is not yet a URL, so the fields are not wiped mid-type.
        let apply_preview = {
            let (fields, preview, syncing, clear_test) =
                (fields.clone(), preview.clone(), syncing, clear_test);
            move |window: &mut Window, cx: &mut App| {
                if syncing.get() {
                    return;
                }
                let url = preview.read(cx).value().to_string();
                if !url.contains("://") {
                    return;
                }
                let parts = ConnectionParts::parse(&url);
                syncing.set(true);
                fields.set(&parts, window, cx);
                syncing.set(false);
                clear_test(cx);
            }
        };

        let mut subs: Vec<Subscription> = fields
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
        subs.push(cx.subscribe_in(
            preview,
            window,
            move |_, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    apply_preview(window, cx);
                }
            },
        ));
        subs
    }

    /// Build and show the connection dialog for the given title, name and
    /// field inputs, live connection-string preview, and Test Connection
    /// result.
    fn open_connection_dialog(
        title: &'static str,
        name: Entity<InputState>,
        fields: ConnectionFields,
        preview: Entity<InputState>,
        test_status: Entity<ConnectionTest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (name, fields, preview, test_status) = (
                name.clone(),
                fields.clone(),
                preview.clone(),
                test_status.clone(),
            );
            let connect = {
                let (app, name, fields) = (app.clone(), name.clone(), fields.clone());
                move |window: &mut Window, cx: &mut App| {
                    let url = fields.read(cx).to_url();
                    let name = name.read(cx).value().trim().to_string();
                    window.close_dialog(cx);
                    app.update(cx, |this, cx| {
                        this.apply_connection(&url, &name, window, cx);
                    })
                    .ok();
                }
            };
            let test = {
                let (app, fields, test_status) = (app.clone(), fields.clone(), test_status.clone());
                move |cx: &mut App| {
                    let url = fields.read(cx).to_url();
                    let status = test_status.clone();
                    app.update(cx, |_, cx| Self::run_connection_test(url, status, cx))
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

            dialog.title(title).w(px(520.)).child(
                v_flex()
                    .gap_4()
                    .pb_2()
                    .child(labeled("Name", &name, cx))
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
                            .child(Input::new(&preview)),
                    )
                    .child(test_status)
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
                                    .on_click(move |_, _, cx| test(cx)),
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
    /// outcome through `status`, which the New Connection dialog renders as
    /// green ("succeeded") or red ("failed: …") text — feedback without
    /// leaving the dialog.
    fn run_connection_test(url: String, status: Entity<ConnectionTest>, cx: &mut Context<Self>) {
        status.update(cx, |status, cx| {
            *status = ConnectionTest::Testing;
            cx.notify();
        });
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(async move { db::test_connection(&url) })
                .await;
            status.update(cx, |status, cx| {
                *status = match result {
                    Ok(()) => ConnectionTest::Ok,
                    Err(err) => ConnectionTest::Failed(
                        format!(
                            "Connection failed: {}",
                            err.lines().next().unwrap_or_default()
                        )
                        .into(),
                    ),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Connection ▸ Recent ▸ …: reconnect to a previously used connection.
    pub fn connect_recent(
        &mut self,
        action: &Connect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_connection(&action.url, &action.name, window, cx);
    }

    /// Switch to `url` (saved under `name`, which may be empty): remember
    /// it, persist, and restart the language server so completions follow
    /// the new database's schema.
    fn apply_connection(
        &mut self,
        url: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if url.is_empty() {
            return;
        }
        // Every tab's session belongs to the previous server; tear them all
        // down (rolling back any open transaction) so the next Run reconnects.
        // Bumping the epoch discards any query still in flight against the old
        // server when it returns.
        self.db_epoch += 1;
        for tab in &mut self.tabs {
            tab.session = None;
            tab.running = false;
            tab.cancel = None;
            tab.result.has_more = false;
        }
        self.config.connection_string = url.to_string();
        record_recent(&mut self.config.recent_connections, url, name);
        self.save_config();
        self.refresh_menus(cx);
        self.sync_connection_combo(window, cx);
        self.restart_lsp(cx);
        if self.config.db_panel_visible {
            self.load_db_schemas(cx);
        } else {
            self.db_nodes.clear();
            self.db_expanded.clear();
        }
        let label = if name.is_empty() {
            mask_credentials(url)
        } else {
            name.to_string()
        };
        self.set_status(format!("Connecting to {label}"), cx);
    }

    /// About ▸ pg-gui on GitHub: open the project page in the browser.
    // &mut self is imposed by the action listener signature.
    #[allow(clippy::unused_self)]
    pub fn open_github(&mut self, _: &OpenGitHub, _: &mut Window, cx: &mut Context<Self>) {
        cx.open_url(REPO_URL);
    }

    /// The "Prev / Next / Page x of y" bar under the results table; `None`
    /// when everything fits on one page.
    /// A small outline button that switches the bottom panel to `view`.
    /// gpui-component ships no icon assets, so the icon is a text glyph.
    fn bottom_view_button(
        view: BottomView,
        glyph: &'static str,
        tooltip: &'static str,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        Button::new(id)
            .outline()
            .small()
            .label(glyph)
            .tooltip(tooltip)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.bottom_view != view {
                    this.bottom_view = view;
                    cx.notify();
                }
            }))
    }

    /// The row of buttons switching between the result sets a multi-statement
    /// run produced, one per statement that returned rows. `None` when the run
    /// produced at most one.
    fn render_result_selector(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let result = &self.tabs.get(self.active_tab)?.result;
        if result.results.len() < 2 {
            return None;
        }
        let selected = result.selected;
        Some(
            h_flex().gap_1().flex_wrap().children(
                result
                    .results
                    .iter()
                    .enumerate()
                    .map(|(ix, set)| {
                        let button = Button::new(("result-set", ix))
                            .small()
                            .label(format!("#{} ({})", set.statement, set.rows.len()))
                            .tooltip(set.label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_result_set(ix, cx);
                            }));
                        if ix == selected {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
        )
    }

    /// Show the `ix`th result set of the active tab's last run in the table.
    fn select_result_set(&mut self, ix: usize, cx: &mut Context<Self>) {
        let active = self.active_tab;
        let Some(tab) = self.tabs.get_mut(active) else {
            return;
        };
        if ix >= tab.result.results.len() || tab.result.selected == ix {
            return;
        }
        tab.result.selected = ix;
        self.show_tab_result(active, cx);
        cx.notify();
    }

    /// The results table between the result-set selector (when a run produced
    /// more than one) and a bottom row holding the pager (when present) and
    /// the Log switch button.
    fn render_data_view(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        v_flex()
            .size_full()
            .p_2()
            .gap_1()
            .children(self.render_result_selector(cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(DataTable::new(&self.results)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .children(self.render_results_pager(cx))
                    .child(div().flex_1())
                    // The Log switch stays visible so the log is always one
                    // click away, even before a run produced any messages.
                    .child(Self::bottom_view_button(
                        BottomView::Log,
                        "☰",
                        "Show log",
                        "bottom-view-log",
                        cx,
                    )),
            )
    }

    /// The message log (one line per statement from the last query/script)
    /// over a Data switch button at the bottom.
    fn render_log(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let body = if self.log.is_empty() {
            v_flex().child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No messages yet."),
            )
        } else {
            v_flex()
                .w_full()
                .gap_0p5()
                .children(self.log.iter().cloned().map(|line| div().child(line)))
        };
        v_flex()
            .size_full()
            .p_2()
            .gap_1()
            .child(
                div()
                    .id("log-view")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .child(body),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("clear-log")
                            .outline()
                            .small()
                            .label("🗑")
                            .tooltip("Clear log")
                            .disabled(self.log.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                // The view is a mirror of the tab's own log;
                                // clearing only the mirror would bring every
                                // line back on the next run.
                                if let Some(tab) = this.tabs.get_mut(this.active_tab) {
                                    tab.result.log.clear();
                                }
                                this.log.clear();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    // The Data switch stays visible so the table is always one
                    // click away, even when the last run returned no rows.
                    .child(Self::bottom_view_button(
                        BottomView::Data,
                        "▦",
                        "Show data",
                        "bottom-view-data",
                        cx,
                    )),
            )
    }

    fn render_results_pager(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let delegate = self.results.read(cx).delegate();
        let (page, page_count, total_rows) = (
            delegate.page(),
            delegate.page_count(),
            delegate.total_rows(),
        );
        let has_more = self.active_has_more();
        if page_count <= 1 && !has_more {
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
                .children(has_more.then(|| {
                    Button::new("fetch-more")
                        .outline()
                        .small()
                        .label("Fetch more")
                        .disabled(self.active_running())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.fetch_more_rows(window, cx);
                        }))
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Page {} of {page_count} · {total_rows}{} rows",
                            page + 1,
                            if has_more { "+" } else { "" }
                        )),
                ),
        )
    }

    /// Fetch More: pull the next batch from the active tab's open cursor and
    /// append it to the results table.
    fn fetch_more_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active_tab;
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        if tab.running || !tab.result.has_more {
            return;
        }
        let Some(mut session) = tab.session.take() else {
            return;
        };
        let tab_id = tab.id;
        let epoch = self.db_epoch;
        let batch_size = self.config.fetch_size.max(1);
        self.tabs[ix].running = true;
        self.set_status("Fetching more rows…", cx);

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let (mut session, result) = cx
                .background_spawn(async move {
                    let result = session.fetch_more(batch_size);
                    (session, result)
                })
                .await;
            // The fetch runs outside the Run path's progress channel, so its
            // notices are drained here instead.
            let notices = session.take_notices();
            let elapsed = started.elapsed();

            this.update_in(cx, |this, window, cx| match result {
                Ok((rows, more)) => {
                    let Some(ix) = this.settle_session(tab_id, epoch, session) else {
                        return;
                    };
                    let fetched = rows.len();
                    let result = &mut this.tabs[ix].result;
                    result
                        .log
                        .extend(notices.into_iter().map(SharedString::from));
                    // Only a lone statement pages, so the cursor's rows
                    // belong to the one result set the run produced.
                    let total = match result.results.last_mut() {
                        Some(set) => {
                            set.rows.extend(rows.iter().cloned());
                            set.rows.len()
                        }
                        None => 0,
                    };
                    result.has_more = more;
                    if ix == this.active_tab {
                        this.results.update(cx, |table, cx| {
                            table.delegate_mut().append_rows(rows);
                            table.refresh(cx);
                        });
                    }
                    let more_txt = if more { ", more available" } else { "" };
                    this.set_status(
                        format!(
                            "Fetched {fetched} more row(s) in {elapsed:.0?} — {total} row(s) total{more_txt}"
                        ),
                        cx,
                    );
                }
                Err(err) => {
                    this.on_query_error(tab_id, epoch, Some(session), &err, window, cx);
                }
            })
            .ok();
        })
        .detach();
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

        let model = ai::model(&self.config.ai_model);
        let prompt_addition = self.config.ai_prompt.clone();

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
                .background_spawn(async move {
                    ai::complete(&key, &model, &prompt_addition, &before, &after)
                })
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

    /// Start a fresh script in a new tab (cmd-t, or the tab bar's "+");
    /// cmd-s prompts for its location.
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

    pub fn open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        // rfd instead of gpui's `prompt_for_paths`, which offers no way to
        // pick the directory the panel opens in.
        let dialog = rfd::AsyncFileDialog::new()
            .set_directory(self.start_dir())
            .set_title("Open SQL script")
            .add_filter("SQL scripts", &["sql"])
            .add_filter("All files", &["*"]);

        cx.spawn_in(window, async move |this, cx| {
            let Some(file) = dialog.pick_file().await else {
                return;
            };
            let path = file.path().to_path_buf();
            this.update_in(cx, |this, window, cx| this.open_path(&path, window, cx))
                .ok();
        })
        .detach();
    }

    /// Open a script file into a tab; shared by the Open… dialog and the
    /// files panel.
    fn open_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                self.set_status(format!("Open failed: {err}"), cx);
                return;
            }
        };
        self.remember_dir(path);
        // A file that is already open just gets its tab selected, keeping
        // any unsaved edits in its buffer.
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| tab.path.as_deref() == Some(path))
        {
            self.activate_tab(ix, window, cx);
            self.save_config();
            self.set_status(format!("{} was already open", path.display()), cx);
            return;
        }
        // Open into a new tab, except an untouched fresh tab is filled in
        // place.
        let ix = if self.tab_is_pristine(self.active_tab, cx) {
            let ix = self.active_tab;
            self.config.tabs[ix].script.clone_from(&content);
            self.tabs[ix].saved.clone_from(&content);
            self.tabs[ix].dirty = false;
            self.tabs[ix].editor.update(cx, |state, cx| {
                set_editor_value(state, content, window, cx);
            });
            ix
        } else {
            self.add_tab(content, None, window, cx)
        };
        self.activate_tab(ix, window, cx);
        self.set_tab_path(ix, path.to_path_buf(), window);
        self.save_config();
        self.set_status(format!("Opened {}", path.display()), cx);
    }

    /// Create a new script under `dir` — the "+" on a folder row in the
    /// files panel — and open it in a tab. The name comes from the
    /// platform's save dialog (which can also land the file elsewhere);
    /// anything it returns is forced to `.sql`, since the panel only opens
    /// and runs those. An existing file is never clobbered.
    fn new_script_in(dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_new_path(dir, Some("script.sql"));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            let path = if file_tree::is_sql(&path) {
                path
            } else {
                path.with_extension("sql")
            };
            // `create_new` rather than `write`: the dialog's own overwrite
            // confirmation was for the name it returned, which the `.sql`
            // above may have changed underneath it.
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map(|_| ());

            this.update_in(cx, |this, window, cx| match created {
                Ok(()) => {
                    // The panel re-scans on a timer; show the file now.
                    this.tree_signature = 0;
                    this.load_tree(cx);
                    this.open_path(&path, window, cx);
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    this.set_status(format!("{} already exists", path.display()), cx);
                }
                Err(err) => this.set_status(format!("Create failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Pick the working directory shown in the files side panel.
    pub fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = rfd::AsyncFileDialog::new()
            .set_directory(self.folder_start_dir())
            .set_title("Open folder");

        cx.spawn_in(window, async move |this, cx| {
            let Some(folder) = dialog.pick_folder().await else {
                return;
            };
            // Canonicalize once here so every id derived from it (the tree
            // items' paths) is consistent across scans.
            let path = folder.path();
            let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            this.update_in(cx, |this, _, cx| this.set_working_dir(path, cx))
                .ok();
        })
        .detach();
    }

    /// Re-open a folder picked from File ▸ Open Recent Folder. A folder
    /// that has since been deleted or moved is dropped from the list
    /// instead of leaving the panel pointed at nothing.
    pub fn open_recent_folder(
        &mut self,
        action: &OpenRecentFolder,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = action.0.clone();
        if !path.is_dir() {
            self.config.recent_folders.retain(|dir| dir != &path);
            self.save_config();
            self.refresh_menus(cx);
            self.set_status(format!("Folder is gone: {}", path.display()), cx);
            return;
        }
        self.set_working_dir(path, cx);
    }

    /// Show `path` in the files side panel and remember it as a recent
    /// folder; shared by the Open Folder dialog and the recent-folders menu.
    fn set_working_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        record_recent_folder(&mut self.config.recent_folders, &path);
        self.config.working_dir = Some(path);
        self.config.files_panel_visible = true;
        self.expanded_dirs.clear();
        self.clear_script_selection();
        self.tree_signature = 0;
        self.load_tree(cx);
        self.save_config();
        self.refresh_menus(cx);
        cx.notify();
    }

    /// Show or hide the files side panel (cmd-shift-e).
    pub fn toggle_files_panel(
        &mut self,
        _: &ToggleFilesPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.files_panel_visible = !self.config.files_panel_visible;
        if self.config.files_panel_visible {
            self.load_tree(cx);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Show or hide the results panel (cmd-3).
    pub fn toggle_results_panel(
        &mut self,
        _: &ToggleResultsPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.results_panel_visible = !self.config.results_panel_visible;
        self.schedule_save(cx);
        cx.notify();
    }

    /// Show or hide the database object browser (cmd-1). Loads the schema
    /// list the first time it's revealed for the current connection.
    pub fn toggle_db_panel(&mut self, _: &ToggleDbPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.config.db_panel_visible = !self.config.db_panel_visible;
        if self.config.db_panel_visible
            && self.db_nodes.is_empty()
            && !matches!(self.db_schema_load, DbSchemaLoad::Loading)
        {
            self.load_db_schemas(cx);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Reload the object browser from scratch (collapse to the schema list
    /// and refetch), picking up objects created since the last load.
    pub fn refresh_db_tree(&mut self, _: &RefreshDbTree, _: &mut Window, cx: &mut Context<Self>) {
        self.load_db_schemas(cx);
    }

    /// Toggle whether system schemas are listed, then reload.
    fn toggle_system_schemas(&mut self, cx: &mut Context<Self>) {
        self.show_system_schemas = !self.show_system_schemas;
        self.load_db_schemas(cx);
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

    pub fn set_theme(&mut self, action: &SetTheme, window: &mut Window, cx: &mut Context<Self>) {
        if self.config.theme == action.0 {
            return;
        }
        self.config.theme = action.0;
        self.apply_theme(window, cx);
        self.set_status(format!("Theme: {}", action.0.label()), cx);
        self.schedule_save(cx);
    }

    /// Apply the configured theme, restore the zoomed font sizes it may
    /// have reset, and re-mark the selected View ▸ Theme entry.
    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        apply_theme_selection(self.config.theme, window, cx);
        self.apply_zoom(cx);
        self.refresh_menus(cx);
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

    /// Turn format-on-save on or off (Edit ▸ Format on Save, or the
    /// `fmt:` segment in the status bar). Persisted, so it holds across
    /// restarts; the menu is rebuilt for its check-mark.
    pub fn toggle_format_on_save(
        &mut self,
        _: &ToggleFormatOnSave,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config.format_on_save = !self.config.format_on_save;
        self.refresh_menus(cx);
        self.schedule_save(cx);
        cx.notify();
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

        let default_name = self.tabs[ix]
            .suggested_name
            .as_deref()
            .unwrap_or("script.sql");
        let rx = cx.prompt_for_new_path(&self.start_dir(), Some(default_name));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let result = std::fs::write(&path, &content);

            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.remember_dir(&path);
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

    /// Custom title bar (the native one is transparent, see main.rs): the
    /// app name, the connection picker, and the active tab's file path,
    /// Zed style. `TitleBar` pads past the macOS traffic lights.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let path = self.tabs[self.active_tab]
            .path
            .as_deref()
            .map(|path| path.display().to_string());
        let title_bar = TitleBar::new();
        // Linux/Windows: the drawn menu bar sits at the head of the title bar,
        // before the app name. macOS relies on the OS menu bar instead.
        #[cfg(not(target_os = "macos"))]
        let title_bar =
            title_bar.child(div().flex().items_center().child(self.app_menu_bar.clone()));
        title_bar.child(
            h_flex()
                .gap_2()
                .flex_1()
                .min_w(px(0.))
                .child(div().text_sm().child("pg-gui"))
                .child(
                    div()
                        .id("connection-combo")
                        .tooltip(|window, cx| {
                            Tooltip::new("Active connection — click to switch").build(window, cx)
                        })
                        .child(
                            Combobox::new(&self.connections)
                                .small()
                                .w(px(320.))
                                .menu_width(px(420.))
                                .placeholder("Select connection…")
                                .search_placeholder("Switch or search connections…"),
                        ),
                )
                .children(path.map(|path| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(path)
                })),
        )
    }

    /// The status line at the bottom: the latest message on the left, the
    /// format-on-save switch and the AI / language-server availability
    /// summary on the right. The `fmt:` segment is the only interactive
    /// piece; it stays plain text (not a `Button`) so the bar keeps its
    /// thin single-line typography, with the pointer cursor and a hover
    /// brightening as the affordance.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ai_available = ai::api_key(&self.config.ai_api_key).is_some();
        let format_on_save = self.config.format_on_save;
        h_flex()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.status.clone())
            .child(div().flex_1())
            .child(
                div()
                    .id("format-on-save")
                    .cursor_pointer()
                    .hover(|this| this.text_color(cx.theme().foreground))
                    .tooltip(move |window, cx| {
                        Tooltip::new(if format_on_save {
                            "Format on save ON — cmd-s formats first. Click to disable."
                        } else {
                            "Format on save OFF — cmd-s writes as typed. Click to enable."
                        })
                        .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_format_on_save(&ToggleFormatOnSave, window, cx);
                    }))
                    .child(if format_on_save {
                        "fmt: on"
                    } else {
                        "fmt: off"
                    }),
            )
            .child(format!(
                " · {} · {} · cmd-h help",
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
            ))
    }

    /// The editor (with its tab bar) over the results table, split by a
    /// draggable divider; the split position is persisted to the config on
    /// drag and restored on launch via the editor panel's initial size.
    fn render_editor_results(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // The SQL editor with its tab bar; always shown.
        let editor = v_flex().size_full().child(self.render_tab_bar(cx)).child(
            div().flex_1().min_h(px(0.)).p_2().child(
                Editor::new(&self.editor())
                    .h_full()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size),
            ),
        );

        // The bottom panel is the debug view while a session runs, else the
        // results table, and is hidden entirely (cmd-3) when neither applies.
        let debugging = self.debug.is_some();
        if !debugging && !self.config.results_panel_visible {
            return v_flex().size_full().child(editor).into_any_element();
        }

        let mut editor_panel = resizable_panel().child(editor);
        if let Some(height) = self.config.editor_height {
            editor_panel = editor_panel.size(px(height));
        }

        v_resizable("editor-results")
            .with_state(&self.resizable_state)
            .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                if let Some(height) = state.read(cx).sizes().first() {
                    this.config.editor_height = Some(f32::from(*height));
                    this.schedule_save(cx);
                }
            }))
            .child(editor_panel)
            .child(
                // Results table with pager — replaced by the debug panel while
                // a debug session is active.
                //
                // The content sits out of the panel's flow (the panel itself is
                // `relative`) and clips: a stepped routine's source, or a long
                // message log, is taller than the panel, and in flow that
                // height becomes the panel's own layout size — which squeezes
                // the editor above and leaves the content painting over it.
                // Absolute keeps the panel at the height the splitter gave it,
                // and the background covers anything the editor spills into it.
                resizable_panel().child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .bg(cx.theme().background)
                        .child(if debugging {
                            self.render_debug_panel(cx).into_any_element()
                        } else {
                            match self.bottom_view {
                                BottomView::Data => self.render_data_view(cx).into_any_element(),
                                BottomView::Log => self.render_log(cx).into_any_element(),
                            }
                        }),
                ),
            )
            .into_any_element()
    }

    /// The files side panel: the working directory's tree, or an "Open
    /// Folder…" hint while none is set. Non-SQL files are disabled — shown
    /// greyed out, with no click handlers.
    /// Compose the three-column workspace: the database browser (left), the
    /// editor+results area (center), and the files panel (right). Each side
    /// panel is present only when visible, with its own draggable divider.
    fn render_workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        let center_and_files = if self.config.files_panel_visible {
            h_resizable("editor-sidebar")
                .with_state(&self.sidebar_state)
                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                    // The files panel is the second (right) child.
                    if let Some(width) = state.read(cx).sizes().last() {
                        this.config.files_panel_width = Some(f32::from(*width));
                        this.schedule_save(cx);
                    }
                }))
                .child(resizable_panel().child(self.render_editor_results(cx)))
                .child(
                    resizable_panel()
                        .size(px(self.config.files_panel_width.unwrap_or(240.)))
                        .size_range(px(120.)..px(600.))
                        .child(self.render_files_panel(cx)),
                )
                .into_any_element()
        } else {
            self.render_editor_results(cx).into_any_element()
        };

        if self.config.db_panel_visible {
            h_resizable("db-sidebar")
                .with_state(&self.db_sidebar_state)
                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                    // The database browser is the first (left) child.
                    if let Some(width) = state.read(cx).sizes().first() {
                        this.config.db_panel_width = Some(f32::from(*width));
                        this.schedule_save(cx);
                    }
                }))
                .child(
                    resizable_panel()
                        .size(px(self.config.db_panel_width.unwrap_or(260.)))
                        .size_range(px(120.)..px(600.))
                        .child(self.render_db_panel(cx)),
                )
                .child(resizable_panel().child(center_and_files))
                .into_any_element()
        } else {
            center_and_files
        }
    }

    /// The database object browser (left panel): a header with a system-schema
    /// toggle and refresh button, a filter box, and the lazily-loaded tree.
    fn render_db_panel(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let header = h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child("Database"),
            )
            .child(
                Checkbox::new("db-system")
                    .label("System")
                    .checked(self.show_system_schemas)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_system_schemas(cx))),
            )
            .child(
                Button::new("db-refresh")
                    .ghost()
                    .xsmall()
                    .tooltip("Refresh")
                    .label("↻")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.refresh_db_tree(&RefreshDbTree, window, cx);
                    })),
            );

        let filter = div()
            .px_1()
            .pb_1()
            .child(Input::new(&self.db_filter_input).xsmall());

        v_flex()
            .size_full()
            .child(header)
            .child(filter)
            .child(div().flex_1().min_h(px(0.)).child(self.render_db_body(cx)))
    }

    /// The browser panel's body: one of the empty/error/loading placeholders,
    /// or the object tree itself.
    fn render_db_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let message = |text: &str, danger: bool| {
            let color = if danger {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            };
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .child(div().text_sm().text_color(color).child(text.to_string()))
                .into_any_element()
        };

        if self.config.connection_string.is_empty() {
            message("Not connected", false)
        } else if let DbSchemaLoad::Failed(err) = &self.db_schema_load {
            message(err, true)
        } else if matches!(self.db_schema_load, DbSchemaLoad::Loading) && self.db_nodes.is_empty() {
            message("Loading…", false)
        } else if self.db_nodes.is_empty() {
            message("No schemas", false)
        } else {
            let view = cx.entity();
            div()
                .size_full()
                .px_1()
                .child(tree(
                    &self.db_tree_state,
                    move |ix, entry, _selected, _window, cx| {
                        view.update(cx, |_, cx| {
                            let item = entry.item();
                            // Text glyphs (gpui-component ships no icon assets):
                            // an arrow for expandable nodes, blank for leaves.
                            let glyph = if !entry.is_folder() {
                                ""
                            } else if entry.is_expanded() {
                                "▾"
                            } else {
                                "▸"
                            };
                            let label_row = h_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .w_4()
                                        .flex_none()
                                        .text_center()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(glyph),
                                )
                                .child(item.label.clone());
                            let list_item = ListItem::new(ix)
                                .w_full()
                                .rounded(cx.theme().radius)
                                .px_2()
                                .pl(px(14.) * entry.depth() + px(8.));
                            if entry.is_folder() {
                                // Folders expand through the tree's own click
                                // handling; only object leaves open. A refresh
                                // button re-fetches this folder's contents so
                                // objects created since the last load appear.
                                let node_id = item.id.clone();
                                list_item.child(
                                    h_flex()
                                        .group("db-row")
                                        .w_full()
                                        .justify_between()
                                        .items_center()
                                        .child(label_row)
                                        .child(
                                            // Hidden until the row is hovered.
                                            div()
                                                .invisible()
                                                .group_hover("db-row", gpui::Styled::visible)
                                                // The tree toggles a row on
                                                // mouse-down; swallow it here so
                                                // pressing refresh doesn't also
                                                // collapse the folder.
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation();
                                                })
                                                .child(
                                                    Button::new(SharedString::from(format!(
                                                        "db-refresh-{}",
                                                        item.id
                                                    )))
                                                    .ghost()
                                                    .xsmall()
                                                    .tooltip("Refresh")
                                                    .label("↻")
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.refresh_db_node(node_id.clone(), cx);
                                                    })),
                                                ),
                                        ),
                                )
                            } else {
                                let list_item = list_item.child(label_row);
                                let id = item.id.clone();
                                list_item.on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_db_object(&id, window, cx);
                                }))
                            }
                        })
                    },
                ))
                .into_any_element()
        }
    }

    /// The files panel before a working folder has been picked.
    fn render_no_folder(cx: &mut Context<Self>) -> gpui::Div {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No folder open"),
            )
            .child(
                Button::new("open-folder")
                    .outline()
                    .small()
                    .label("Open Folder…")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_folder(&OpenFolder, window, cx);
                    })),
            )
    }

    /// A folder row in the files panel: the tree's own click handling
    /// expands it, so the row only adds the "+" that creates a script
    /// inside it, hidden until the row is hovered.
    fn render_folder_row(
        list_item: ListItem,
        label_row: gpui::Div,
        id: &SharedString,
        cx: &mut Context<Self>,
    ) -> ListItem {
        let dir = PathBuf::from(id.to_string());
        list_item.child(
            h_flex()
                .group("file-row")
                .w_full()
                .justify_between()
                .items_center()
                .child(label_row)
                .child(
                    div()
                        .invisible()
                        .group_hover("file-row", gpui::Styled::visible)
                        // The tree toggles a row on mouse-down; swallow it
                        // here so pressing "+" doesn't also collapse the
                        // folder.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            Button::new(SharedString::from(format!("new-script-{id}")))
                                .ghost()
                                .xsmall()
                                .tooltip("New script here")
                                .label("+")
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    cx.stop_propagation();
                                    Self::new_script_in(&dir, window, cx);
                                })),
                        ),
                ),
        )
    }

    /// The files panel's title row: the working folder's name, and the "+"
    /// that creates a script directly inside it — the root has no row of
    /// its own in the tree, where every other folder carries its own.
    fn render_files_header(
        root: PathBuf,
        folder_name: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        h_flex()
            .px_2()
            .py_1()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(folder_name),
            )
            .child(
                Button::new("new-script-root")
                    .ghost()
                    .xsmall()
                    .tooltip("New script in this folder")
                    .label("+")
                    .on_click(cx.listener(move |_, _, window, cx| {
                        Self::new_script_in(&root, window, cx);
                    })),
            )
    }

    /// The files panel's "Run N script(s)" bar, shown while scripts are
    /// picked: runs them in tree order, or drops the selection.
    fn render_script_run_bar(picked: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        h_flex()
            .px_1()
            .pb_1()
            .gap_1()
            .child(
                Button::new("run-scripts")
                    .primary()
                    .xsmall()
                    .flex_1()
                    .label(format!("▶ Run {picked} script(s)"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.run_scripts(&RunScripts, window, cx);
                    })),
            )
            .child(
                Button::new("clear-scripts")
                    .ghost()
                    .xsmall()
                    .tooltip("Clear the selection")
                    .label("×")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clear_script_selection();
                        cx.notify();
                    })),
            )
    }

    fn render_files_panel(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let Some(root) = self.config.working_dir.clone() else {
            return Self::render_no_folder(cx);
        };

        let folder_name = root.file_name().map_or_else(
            || root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let dirs = self.tree_dirs.clone();
        let selected: Rc<HashSet<SharedString>> = Rc::new(self.selected_scripts.clone());
        let picked = selected.len();
        let view = cx.entity();
        v_flex()
            .size_full()
            .child(Self::render_files_header(root, folder_name, cx))
            .child(
                div()
                    .px_1()
                    .pb_1()
                    .child(Input::new(&self.file_filter_input).xsmall()),
            )
            // Only shown once a *batch* is picked: a plain click selects
            // the one file it opens, so a bar at one script would be up
            // permanently for anyone who never selects a range.
            .when(picked > 1, |this| {
                this.child(Self::render_script_run_bar(picked, cx))
            })
            .child(div().flex_1().min_h(px(0.)).px_1().child(tree(
                &self.tree_state,
                move |ix, entry, _selected, _window, cx| {
                    view.update(cx, |_, cx| {
                        let item = entry.item();
                        // The tree's own `is_folder()` is children-based
                        // and misses empty directories.
                        let is_dir = entry.is_folder() || dirs.contains(&item.id);
                        // Text glyphs, like the tab bar: gpui-component
                        // ships no icon assets, so `IconName` renders blank.
                        let glyph = if !is_dir {
                            ""
                        } else if entry.is_expanded() {
                            "▾"
                        } else {
                            "▸"
                        };
                        // The tree owns its own single selection (and
                        // overwrites `ListItem::selected` after this
                        // closure returns), so a picked script is marked
                        // with its own background instead.
                        let is_picked = selected.contains(&item.id);
                        let label_row = h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .w_4()
                                    .flex_none()
                                    .text_center()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(glyph),
                            )
                            .child(item.label.clone());
                        let list_item = ListItem::new(ix)
                            .w_full()
                            .rounded(cx.theme().radius)
                            .px_2()
                            .pl(px(14.) * entry.depth() + px(8.))
                            .when(is_picked, |this| this.bg(cx.theme().accent));
                        if is_dir {
                            Self::render_folder_row(list_item, label_row, &item.id, cx)
                        } else {
                            // Plain click opens the script and re-anchors;
                            // shift-click takes the range from the anchor,
                            // cmd/ctrl-click toggles the one row. Only a
                            // plain click opens a tab — extending a
                            // selection shouldn't fill the tab bar.
                            let id = item.id.clone();
                            let path = PathBuf::from(item.id.to_string());
                            list_item.child(label_row).on_click(cx.listener(
                                move |this, event: &ClickEvent, window, cx| {
                                    let modifiers = event.modifiers();
                                    if modifiers.shift {
                                        this.extend_script_selection(&id);
                                    } else if modifiers.secondary() {
                                        this.toggle_script(&id);
                                    } else {
                                        this.select_script(&id);
                                        this.open_path(&path, window, cx);
                                    }
                                    cx.notify();
                                },
                            ))
                        }
                    })
                },
            )))
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
            .suffix(self.render_session_toolbar(cx))
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

    /// Session controls sitting after the tabs, next to "+": an autocommit
    /// toggle, Commit/Rollback (enabled only with an open transaction), a
    /// Cancel button (enabled while a query runs), and the new-tab "+".
    /// They act on the active tab's session. gpui-component ships no icon
    /// assets, so these use text glyphs rather than `IconName` SVGs.
    fn render_session_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let autocommit = self.active_autocommit();
        let running = self.active_running();
        // Commit/Rollback only make sense on a manual transaction that has
        // actually begun, and never while a query is in flight.
        let txn_actionable = !autocommit && self.active_in_txn() && !running;

        let autocommit_base = Button::new("session-autocommit")
            .small()
            .label(if autocommit { "AC: on" } else { "AC: off" })
            .tooltip(if autocommit {
                "Autocommit ON — each Run commits. Click for manual transactions."
            } else {
                "Autocommit OFF — Run works inside a transaction. Click to commit each Run."
            });
        // Highlight the non-default (manual transaction) mode.
        let autocommit_btn = if autocommit {
            autocommit_base.ghost()
        } else {
            autocommit_base.primary()
        }
        .on_click(cx.listener(|this, _, window, cx| {
            this.toggle_autocommit(&ToggleAutocommit, window, cx);
        }));

        let commit_btn = Button::new("session-commit")
            .outline()
            .small()
            .label("✓")
            .tooltip("Commit transaction")
            .disabled(!txn_actionable)
            .on_click(cx.listener(|this, _, window, cx| {
                this.commit_txn(&Commit, window, cx);
            }));

        let rollback_btn = Button::new("session-rollback")
            .outline()
            .small()
            .label("↺")
            .tooltip("Roll back transaction")
            .disabled(!txn_actionable)
            .on_click(cx.listener(|this, _, window, cx| {
                this.rollback_txn(&Rollback, window, cx);
            }));

        let cancel_btn = Button::new("session-cancel")
            .danger()
            .small()
            .label("⊘")
            .tooltip("Cancel running query")
            .disabled(!running)
            .on_click(cx.listener(|this, _, window, cx| {
                this.cancel_query(&CancelQuery, window, cx);
            }));

        let new_tab_btn = Button::new("new-tab")
            .ghost()
            .small()
            .label("+")
            .tooltip("New script tab")
            .on_click(cx.listener(|this, _, window, cx| {
                this.new_file(&NewFile, window, cx);
            }));

        h_flex()
            .gap_1()
            .px_1()
            .child(autocommit_btn)
            .child(commit_btn)
            .child(rollback_btn)
            .child(cancel_btn)
            .child(new_tab_btn)
    }

    // ===== PL/pgSQL step debugger =====

    /// Start Debug (cmd-shift-d): guess the routine at the cursor and open the
    /// launch dialog.
    pub fn start_debug(&mut self, _: &StartDebug, window: &mut Window, cx: &mut Context<Self>) {
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
        sig.update(cx, |state, cx| state.focus(window, cx));
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
                status = Some(format!("Debug error: {err}"));
            }
            debug::DebugEvent::Terminated => {
                dbg.terminated = true;
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

    pub fn debug_step_over(&mut self, _: &DebugStepOver, _: &mut Window, _: &mut Context<Self>) {
        if let Some(dbg) = self.debug.as_ref().filter(|d| !d.terminated) {
            dbg.session.step_over();
        }
    }

    pub fn debug_step_into(&mut self, _: &DebugStepInto, _: &mut Window, _: &mut Context<Self>) {
        if let Some(dbg) = self.debug.as_ref().filter(|d| !d.terminated) {
            dbg.session.step_into();
        }
    }

    /// F5: continue a live session, or start one when idle.
    pub fn debug_continue(
        &mut self,
        _: &DebugContinue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.debug.as_ref() {
            Some(dbg) if !dbg.terminated => dbg.session.continue_(),
            _ => self.start_debug(&StartDebug, window, cx),
        }
    }

    pub fn debug_stop(&mut self, _: &DebugStop, _: &mut Window, cx: &mut Context<Self>) {
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
    fn sync_debug_breakpoint(&self, editor: &Entity<EditorState>, row: usize, set: bool) {
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
    fn open_deposit_dialog(
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

    /// The debug panel that replaces the results table while a session runs:
    /// a toolbar, the stepped source, and a variables/stack sidebar.
    fn render_debug_panel(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let Some(dbg) = self.debug.as_ref() else {
            return div().into_any_element();
        };
        let running = !dbg.terminated;
        let toolbar = h_flex()
            .gap_2()
            .p_2()
            .items_center()
            .child(
                div()
                    .font_semibold()
                    .child(format!("Debug: {}", dbg.session.target)),
            )
            .child(div().flex_1())
            .child(
                Button::new("dbg-step-over")
                    .outline()
                    .small()
                    .label("Step Over")
                    .disabled(!running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_step_over(&DebugStepOver, window, cx);
                    })),
            )
            .child(
                Button::new("dbg-step-into")
                    .outline()
                    .small()
                    .label("Step Into")
                    .disabled(!running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_step_into(&DebugStepInto, window, cx);
                    })),
            )
            .child(
                Button::new("dbg-continue")
                    .outline()
                    .small()
                    .label("Continue")
                    .disabled(!running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_continue(&DebugContinue, window, cx);
                    })),
            )
            .child(
                Button::new("dbg-stop")
                    .danger()
                    .small()
                    .label("Stop")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_stop(&DebugStop, window, cx);
                    })),
            );
        // The stepped source lives in the editor itself — the selection follows
        // each stop — so the panel only carries the state around it.
        let body = div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .child(if dbg.stop.is_some() {
                self.render_debug_sidebar(cx)
            } else {
                self.render_debug_status(cx)
            });
        // A long routine's result is one line; keep it from growing the panel.
        let output = dbg.output.as_ref().map(|out| {
            div()
                .id("dbg-output")
                .flex_none()
                .max_h(px(64.))
                .overflow_y_scroll()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!("Result: {out}"))
        });
        v_flex()
            .size_full()
            .child(toolbar)
            .child(body)
            .children(output)
            .into_any_element()
    }

    /// What the session is doing before its first stop (connecting, waiting
    /// for the target to trap), plus any output or error, so a run that never
    /// traps is visible rather than silent. Once stopped, the panel shows the
    /// variables/stack instead and the source is read in the editor.
    fn render_debug_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dbg) = self.debug.as_ref() else {
            return div().into_any_element();
        };
        let mut pane = v_flex()
            .p_2()
            .gap_2()
            .text_color(cx.theme().muted_foreground);
        pane = pane.child(if dbg.terminated {
            format!("Session ended before stopping. {}", dbg.status)
        } else {
            format!("{}…", dbg.status.trim_end_matches('…'))
        });
        if let Some(output) = &dbg.output {
            pane = pane.child(div().child(format!("Target: {output}")));
        }
        pane.into_any_element()
    }

    /// Variables of the active frame (click to change) over the call stack
    /// (click a frame to inspect it).
    fn render_debug_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(stop) = self.debug.as_ref().and_then(|d| d.stop.as_ref()) else {
            return div().into_any_element();
        };
        // A narrow marker column down the left, mirroring the editor's gutter:
        // it carries the arrow on the frame being inspected and nothing else,
        // so every name in both lists starts at the same x.
        let muted = cx.theme().muted_foreground;
        let gutter = move |label: &'static str| {
            div()
                .w(px(14.))
                .flex_none()
                .text_center()
                .text_color(muted)
                .child(label)
        };
        let mut vars = v_flex().gap_0().child(
            h_flex()
                .gap_2()
                .px_1()
                .py_1()
                .child(gutter(""))
                .child(div().font_semibold().child("Variables")),
        );
        for var in &stop.variables {
            let (name, value) = (var.name.clone(), var.value.clone());
            vars = vars.child(
                h_flex()
                    .gap_2()
                    .px_1()
                    .cursor_pointer()
                    .child(gutter(""))
                    .child(
                        div()
                            .w(px(130.))
                            .flex_none()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(var.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .child(var.value.clone()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, window, cx| {
                            Self::open_deposit_dialog(name.clone(), value.clone(), window, cx);
                        }),
                    ),
            );
        }
        let mut stack = v_flex().gap_0().child(
            h_flex()
                .gap_2()
                .px_1()
                .py_1()
                .mt_2()
                .child(gutter(""))
                .child(div().font_semibold().child("Call stack")),
        );
        for frame in &stop.stack {
            let level = frame.level;
            // Mark the frame the panel is showing — the innermost one at every
            // stop, or whichever the user clicked into since.
            let marker = if level == stop.frame { "▸" } else { "" };
            stack = stack.child(
                h_flex()
                    .gap_2()
                    .px_1()
                    .cursor_pointer()
                    .child(gutter(marker))
                    .child(
                        div()
                            .truncate()
                            .child(format!("{} :{}", frame.target_name, frame.line)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, _| {
                            if let Some(dbg) = this.debug.as_ref() {
                                dbg.session.select_frame(level);
                            }
                        }),
                    ),
            );
        }
        div()
            .id("dbg-sidebar")
            .size_full()
            .overflow_scroll()
            // Left gutter, so the names line up off the panel edge rather than
            // against it.
            .pl_3()
            .pr_2()
            .text_size(cx.theme().mono_font_size)
            .child(vars)
            .child(stack)
            .into_any_element()
    }
}

impl Render for PgGuiApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_action(cx.listener(Self::toggle_autocommit))
            .on_action(cx.listener(Self::commit_txn))
            .on_action(cx.listener(Self::rollback_txn))
            .on_action(cx.listener(Self::cancel_query))
            .on_action(cx.listener(Self::export_csv))
            .on_action(cx.listener(Self::export_inserts))
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
            // Dialogs (e.g. the snippet picker) are drawn by the app's root
            // element; gpui-component's Root only stores them.
            .children(Root::render_dialog_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        MAX_RECENT_FOLDERS, definition_content_rank, dialog_start_dir, ends_with_name_word,
        find_sql_file, folder_menu_label, glob_match, mask_credentials, record_recent_folder,
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
}
