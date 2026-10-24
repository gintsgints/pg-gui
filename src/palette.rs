//! The cmd-p palette: one searchable list with three modes, picked by the
//! first character of the query. A plain query searches the working
//! folder's SQL files, `>` switches to the snippet library and `:` to the
//! app's commands — so one dialog replaces three, and the mode is never
//! more than one keystroke away from wherever the query started.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    Action, App, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Task, Window,
    div,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, h_flex,
    kbd::Kbd,
    list::{ListDelegate, ListItem, ListState},
    v_flex,
};

use crate::snippets::{Library, Snippet};
use crate::{
    AiComplete, CancelQuery, CloseTab, Commit, DebugContinue, DebugStepInto, DebugStepOver,
    DebugStop, EditConnection, ExplainAnalyze, ExplainPlan, ExportCsv, ExportInserts, FormatScript,
    NewConnection, NewFile, NextDiagnostic, NextTab, OpenConfig, OpenFile, OpenFolder, OpenGitHub,
    PrevDiagnostic, PrevTab, Quit, RefreshDbTree, Rollback, RunQuery, RunScripts, SaveFile,
    SetTheme, ShowHelp, StartDebug, ToggleAutocommit, ToggleComment, ToggleDbPanel,
    ToggleFilesPanel, ToggleFormatOnSave, ToggleResultsPanel, ZoomIn, ZoomOut, ZoomReset, config,
    file_tree,
};

/// Query prefix that lists snippets.
pub const SNIPPETS_PREFIX: &str = ">";
/// Query prefix that lists commands.
pub const COMMANDS_PREFIX: &str = ":";

/// What a query searches, decided by its first character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Files,
    Snippets,
    Commands,
}

/// Split a query into its mode and the search text after the prefix.
fn parse_query(query: &str) -> (Mode, &str) {
    let query = query.trim_start();
    if let Some(rest) = query.strip_prefix(SNIPPETS_PREFIX) {
        (Mode::Snippets, rest)
    } else if let Some(rest) = query.strip_prefix(COMMANDS_PREFIX) {
        (Mode::Commands, rest)
    } else {
        (Mode::Files, query)
    }
}

/// A SQL file under the working folder.
pub struct FileEntry {
    pub path: PathBuf,
    /// The path relative to the working folder: what is shown and searched.
    relative: SharedString,
}

/// Every `.sql` file the files panel's last scan found, in the panel's
/// order, labelled relative to `root`.
pub fn sql_files(nodes: &[file_tree::FileNode], root: Option<&Path>) -> Vec<Rc<FileEntry>> {
    fn walk(nodes: &[file_tree::FileNode], root: Option<&Path>, out: &mut Vec<Rc<FileEntry>>) {
        for node in nodes {
            if node.is_dir {
                walk(&node.children, root, out);
            } else if file_tree::is_sql(&node.path) {
                let relative = root
                    .and_then(|root| node.path.strip_prefix(root).ok())
                    .unwrap_or(&node.path)
                    .to_string_lossy()
                    .into_owned();
                out.push(Rc::new(FileEntry {
                    path: node.path.clone(),
                    relative: relative.into(),
                }));
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, root, &mut out);
    out
}

/// One command: a label to search and the action it dispatches.
pub struct Command {
    label: SharedString,
    action: Box<dyn Action>,
}

impl Command {
    fn new(label: &str, action: impl Action) -> Rc<Self> {
        Rc::new(Self {
            label: label.into(),
            action: Box::new(action),
        })
    }
}

/// The commands `:` lists. Category-prefixed like the snippets, so the
/// category doubles as a search word. Actions that only make sense with
/// the results grid focused (copy, select all) are left out, as is the
/// palette itself.
fn commands() -> Vec<Rc<Command>> {
    vec![
        Command::new("Connection: New Connection…", NewConnection),
        Command::new("Connection: Edit Connection…", EditConnection),
        Command::new("Connection: Run Query", RunQuery),
        Command::new("Connection: Run Selected Scripts", RunScripts),
        Command::new("Connection: Explain Plan", ExplainPlan),
        Command::new("Connection: Explain Analyze", ExplainAnalyze),
        Command::new("Connection: Export as CSV…", ExportCsv),
        Command::new("Connection: Export as INSERT…", ExportInserts),
        Command::new("File: Open…", OpenFile),
        Command::new("File: Open Folder…", OpenFolder),
        Command::new("File: Save", SaveFile),
        Command::new("File: New Tab", NewFile),
        Command::new("File: Close Tab", CloseTab),
        Command::new("File: Next Tab", NextTab),
        Command::new("File: Previous Tab", PrevTab),
        Command::new("Edit: Format", FormatScript),
        Command::new("Edit: Toggle Format on Save", ToggleFormatOnSave),
        Command::new("Edit: AI Complete", AiComplete),
        Command::new("Edit: Toggle Comment", ToggleComment),
        Command::new("Edit: Next Problem", NextDiagnostic),
        Command::new("Edit: Previous Problem", PrevDiagnostic),
        Command::new("Session: Toggle Autocommit", ToggleAutocommit),
        Command::new("Session: Commit", Commit),
        Command::new("Session: Rollback", Rollback),
        Command::new("Session: Cancel Query", CancelQuery),
        Command::new("Debug: Start Debug", StartDebug),
        Command::new("Debug: Step Over", DebugStepOver),
        Command::new("Debug: Step Into", DebugStepInto),
        Command::new("Debug: Continue", DebugContinue),
        Command::new("Debug: Stop", DebugStop),
        Command::new("View: Toggle Database Browser", ToggleDbPanel),
        Command::new("View: Toggle Files Panel", ToggleFilesPanel),
        Command::new("View: Toggle Results Panel", ToggleResultsPanel),
        Command::new("View: Refresh Database Browser", RefreshDbTree),
        Command::new("View: Zoom In", ZoomIn),
        Command::new("View: Zoom Out", ZoomOut),
        Command::new("View: Actual Size", ZoomReset),
        Command::new("View: Theme Light", SetTheme(config::ThemeSelection::Light)),
        Command::new("View: Theme Dark", SetTheme(config::ThemeSelection::Dark)),
        Command::new(
            "View: Theme System",
            SetTheme(config::ThemeSelection::System),
        ),
        Command::new("Preferences…", OpenConfig),
        Command::new("Help: Keyboard Shortcuts", ShowHelp),
        Command::new("Help: pg-gui on GitHub", OpenGitHub),
        Command::new("Quit", Quit),
    ]
}

/// Whether every whitespace-separated word of `query` appears in `text`,
/// so "new table" finds "New: table" despite the colon.
fn words_match(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| text.contains(word))
}

/// Whether `needle`'s characters appear in `haystack` in order, not
/// necessarily adjacent — the usual quick-open match, so `ordsql` finds
/// `orders.sql` and `fnord` finds `functions/orders.sql`.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut haystack = haystack.chars();
    needle.chars().all(|c| haystack.any(|h| h == c))
}

/// The files whose relative path matches `query`, best first: every word
/// must be a subsequence of the path, and matches whose file name holds
/// the query verbatim rank above those that only match scattered, shorter
/// paths above longer ones.
fn match_files(files: &[Rc<FileEntry>], query: &str) -> Vec<Rc<FileEntry>> {
    let query = query.to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return files.to_vec();
    }
    let joined = words.concat();
    let mut matched: Vec<(bool, usize, Rc<FileEntry>)> = files
        .iter()
        .filter_map(|file| {
            let path = file.relative.to_lowercase();
            if !words.iter().all(|word| is_subsequence(word, &path)) {
                return None;
            }
            let name = file
                .path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            Some((!name.contains(&joined), path.len(), file.clone()))
        })
        .collect();
    matched.sort_by_key(|(scattered, len, _)| (*scattered, *len));
    matched.into_iter().map(|(_, _, file)| file).collect()
}

/// One listed entry.
#[derive(Clone)]
enum Entry {
    File(Rc<FileEntry>),
    Snippet(Rc<Snippet>),
    Command(Rc<Command>),
}

/// What the user confirmed.
pub enum Pick<'a> {
    File(&'a Path),
    Snippet(&'a Snippet),
    Command(&'a dyn Action),
}

/// Called with the confirmed entry.
type OnPick = Box<dyn Fn(Pick<'_>, &mut Window, &mut App)>;

/// List delegate for the palette. Empty sections are dropped entirely —
/// the virtual list measures row height from the item at (0, 0), so that
/// index must always be occupied while anything is listed.
pub struct PaletteDelegate {
    files: Vec<Rc<FileEntry>>,
    has_working_dir: bool,
    library: Library,
    commands: Vec<Rc<Command>>,
    mode: Mode,
    filtered: Vec<(SharedString, Vec<Entry>)>,
    selected: Option<IndexPath>,
    on_pick: OnPick,
}

impl PaletteDelegate {
    /// `files` is `None` when no working folder is open, which the files
    /// mode then says instead of showing an empty list.
    pub fn new(
        files: Option<Vec<Rc<FileEntry>>>,
        library: Library,
        on_pick: impl Fn(Pick<'_>, &mut Window, &mut App) + 'static,
    ) -> Self {
        let mut this = Self {
            has_working_dir: files.is_some(),
            files: files.unwrap_or_default(),
            library,
            commands: commands(),
            mode: Mode::Files,
            filtered: Vec::new(),
            selected: None,
            on_pick: Box::new(on_pick),
        };
        this.filter("");
        this
    }

    fn filter(&mut self, query: &str) {
        let (mode, text) = parse_query(query);
        self.mode = mode;
        let sections: Vec<(SharedString, Vec<Entry>)> = match mode {
            Mode::Files => vec![(
                "Files".into(),
                match_files(&self.files, text)
                    .into_iter()
                    .map(Entry::File)
                    .collect(),
            )],
            Mode::Snippets => {
                let pick = |snippets: &[Rc<Snippet>]| {
                    snippets
                        .iter()
                        .filter(|snippet| words_match(&snippet.name, text))
                        .cloned()
                        .map(Entry::Snippet)
                        .collect()
                };
                vec![
                    ("Your snippets".into(), pick(&self.library.user)),
                    ("Built-in".into(), pick(&self.library.builtin)),
                ]
            }
            Mode::Commands => vec![(
                "Commands".into(),
                self.commands
                    .iter()
                    .filter(|command| words_match(&command.label, text))
                    .cloned()
                    .map(Entry::Command)
                    .collect(),
            )],
        };
        self.filtered = sections
            .into_iter()
            .filter(|(_, entries)| !entries.is_empty())
            .collect();
        self.selected = if self.filtered.is_empty() {
            None
        } else {
            Some(IndexPath::default())
        };
    }

    fn entry(&self, ix: IndexPath) -> Option<&Entry> {
        self.filtered.get(ix.section)?.1.get(ix.row)
    }
}

impl ListDelegate for PaletteDelegate {
    type Item = ListItem;

    fn sections_count(&self, _: &App) -> usize {
        self.filtered.len()
    }

    fn items_count(&self, section: usize, _: &App) -> usize {
        self.filtered
            .get(section)
            .map_or(0, |(_, entries)| entries.len())
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.filter(query);
        Task::ready(())
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let (label, _) = self.filtered.get(section)?;
        Some(
            h_flex()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.clone()),
        )
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let muted = cx.theme().muted_foreground;
        let dim = |text: SharedString| {
            div()
                .text_xs()
                .text_color(muted)
                .overflow_hidden()
                .whitespace_nowrap()
                .child(text)
        };
        let content = match self.entry(ix)? {
            Entry::File(file) => {
                let name: SharedString = file
                    .path
                    .file_name()
                    .map_or_else(
                        || file.relative.to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    )
                    .into();
                v_flex()
                    .py_1()
                    .child(div().text_sm().child(name))
                    .child(dim(file.relative.clone()))
                    .into_any_element()
            }
            Entry::Snippet(snippet) => v_flex()
                .py_1()
                .child(div().text_sm().child(snippet.name.clone()))
                .child(dim(snippet.preview.clone()))
                .into_any_element(),
            Entry::Command(command) => h_flex()
                .py_1()
                .justify_between()
                .gap_2()
                .child(div().text_sm().child(command.label.clone()))
                .children(Kbd::global_binding_for_action(
                    command.action.as_ref(),
                    window,
                ))
                .into_any_element(),
        };
        Some(
            ListItem::new(ix)
                .selected(self.selected == Some(ix))
                .child(content),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let message = match self.mode {
            Mode::Files if !self.has_working_dir => {
                "No folder open — File ▸ Open Folder… (type > for snippets, : for commands)"
            }
            Mode::Files => "No matching files (type > for snippets, : for commands)",
            Mode::Snippets => "No matching snippets",
            Mode::Commands => "No matching commands",
        };
        h_flex()
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(message)
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(entry) = self.selected.and_then(|ix| self.entry(ix)).cloned() else {
            return;
        };
        let pick = match &entry {
            Entry::File(file) => Pick::File(&file.path),
            Entry::Snippet(snippet) => Pick::Snippet(snippet),
            Entry::Command(command) => Pick::Command(command.action.as_ref()),
        };
        (self.on_pick)(pick, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::rc::Rc;

    use super::{FileEntry, Mode, is_subsequence, match_files, parse_query};

    #[test]
    fn prefix_picks_mode() {
        assert_eq!(parse_query("orders"), (Mode::Files, "orders"));
        assert_eq!(parse_query(">new"), (Mode::Snippets, "new"));
        assert_eq!(parse_query("> new"), (Mode::Snippets, " new"));
        assert_eq!(parse_query(":run"), (Mode::Commands, "run"));
        assert_eq!(parse_query(""), (Mode::Files, ""));
    }

    #[test]
    fn subsequence_matches_scattered_chars() {
        assert!(is_subsequence("ordsql", "orders.sql"));
        assert!(is_subsequence("", "x"));
        assert!(!is_subsequence("sqlord", "orders.sql"));
    }

    #[test]
    fn files_rank_name_hits_first() {
        let file = |relative: &str| {
            Rc::new(FileEntry {
                path: PathBuf::from("/w").join(relative),
                relative: relative.to_string().into(),
            })
        };
        let files = vec![
            file("orders/archive/old_report.sql"),
            file("reports/orders.sql"),
            file("x/orders.sql"),
            file("misc.sql"),
        ];
        let ranked: Vec<String> = match_files(&files, "orders")
            .iter()
            .map(|file| file.relative.to_string())
            .collect();
        assert_eq!(
            ranked,
            [
                "x/orders.sql",
                "reports/orders.sql",
                "orders/archive/old_report.sql"
            ]
        );
        assert_eq!(match_files(&files, "").len(), 4);
    }
}
