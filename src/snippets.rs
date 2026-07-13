//! The snippet library: curated queries compiled into the app plus the
//! user's own `.sql` files, presented in a searchable picker.
//!
//! User snippets live as individual files in the config directory
//! (`~/Library/Application Support/pg-gui/snippets/*.sql` on macOS); the
//! file stem is the snippet name, and a user file whose name matches a
//! built-in overrides it. The directory is re-read every time the picker
//! opens.
//!
//! Snippets are written to run unedited; where they filter on a name they
//! use `ILIKE '%%'` (match everything), and on insert the caret is placed
//! between the two `%` so typing narrows the filter.
//!
//! Snippets may also carry numbered tab stops — `$1`, `${2}` or
//! `${3:placeholder}` — visited in numeric order with the tab key after
//! insertion (`$0` last, per LSP convention). At each stop the marker is
//! replaced by its placeholder text (empty for the bare forms), left
//! selected so typing overwrites it. User `.sql` snippet files can use the
//! same markers.
//!
//! Besides the cmd-p picker, snippets surface in the editor's completion
//! menu: typing words of a snippet's name or the leading words of its SQL
//! suggests it (see [`suggestions`]).

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Task, Window, div,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, h_flex,
    list::{ListDelegate, ListItem, ListState},
    v_flex,
};

pub struct Snippet {
    pub name: SharedString,
    pub sql: SharedString,
    /// First meaningful SQL line, shown dimmed under the name in the picker.
    preview: SharedString,
}

impl Snippet {
    fn new(name: String, sql: String) -> Self {
        let preview = strip_tab_stops(
            sql.lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("--"))
                .unwrap_or_default(),
        );
        Self {
            name: name.into(),
            sql: sql.into(),
            preview: preview.into(),
        }
    }
}

/// A `$n` / `${n}` / `${n:placeholder}` tab-stop marker found in snippet
/// text.
pub struct TabStop {
    /// Byte range of the whole marker.
    pub range: Range<usize>,
    /// Text left (selected) in place of the marker; empty for the bare
    /// forms.
    pub placeholder: String,
    number: u32,
    /// Whether the marker is one of the braced forms. Only those are
    /// serviced outside snippet mode, so a hand-written Postgres
    /// parameter (`$1`) never captures the tab key.
    braced: bool,
}

/// All tab-stop markers in `text`, in text order. `$` not followed by a
/// digit or `{n…}` is left alone, so Postgres dollar quoting (`$$…$$`)
/// never reads as a marker.
fn tab_stops(text: &str) -> Vec<TabStop> {
    let mut stops = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find('$') {
        let start = from + offset;
        from = start + 1;
        let rest = &text[start + 1..];
        let (body, braced) = match rest.strip_prefix('{') {
            Some(body) => (body, true),
            None => (rest, false),
        };
        let digits = body
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(body.len());
        let Ok(number) = body[..digits].parse::<u32>() else {
            continue;
        };
        let (placeholder, len) = if braced {
            match body[digits..].split_once('}') {
                // ${n}
                Some(("", _)) => (String::new(), 2 + digits + 1),
                // ${n:placeholder}
                Some((rest, _)) if rest.starts_with(':') => {
                    (rest[1..].to_string(), 2 + digits + rest.len() + 1)
                }
                _ => continue,
            }
        } else {
            (String::new(), 1 + digits)
        };
        stops.push(TabStop {
            range: start..start + len,
            placeholder,
            number,
            braced,
        });
        from = start + len;
    }
    stops
}

/// The next tab stop to visit: lowest number first (`$0` last, per LSP
/// convention), leftmost on a tie. With `braced_only`, bare `$n` markers
/// are ignored.
pub fn next_tab_stop(text: &str, braced_only: bool) -> Option<TabStop> {
    tab_stops(text)
        .into_iter()
        .filter(|stop| stop.braced || !braced_only)
        .min_by_key(|stop| {
            let order = if stop.number == 0 {
                u32::MAX
            } else {
                stop.number
            };
            (order, stop.range.start)
        })
}

pub fn has_tab_stops(text: &str) -> bool {
    !tab_stops(text).is_empty()
}

/// A snippet whose name or leading SQL matches the words typed before
/// the cursor, offered by the editor's completion menu.
pub struct Suggestion {
    pub name: SharedString,
    pub sql: SharedString,
    /// How many bytes immediately before the cursor the completion
    /// replaces: the matched words plus any trailing whitespace.
    pub replace_len: usize,
}

/// Snippets matching the last one or two words of `line` (the text before
/// the cursor). A snippet matches when each typed word prefixes a word of
/// its name ("new tab" → "New: table") or when the typed words are the
/// leading words of its SQL ("create seq" → the sequence template). A
/// two-word match replaces both words, otherwise just the last one.
/// Queries under two characters suggest nothing.
pub fn suggestions(line: &str) -> Vec<Suggestion> {
    library_suggestions(&load(), line)
}

fn library_suggestions(library: &Library, line: &str) -> Vec<Suggestion> {
    let trimmed = line.trim_end();
    let Some(last_start) = last_word_start(trimmed) else {
        return Vec::new();
    };
    let last = trimmed[last_start..].to_lowercase();
    let earlier = trimmed[..last_start].trim_end();
    let prev = last_word_start(earlier).map(|start| (start, earlier[start..].to_lowercase()));

    let mut out = Vec::new();
    for snippet in library.user.iter().chain(&library.builtin) {
        let name = snippet.name.to_lowercase();
        let sql = snippet.sql.to_lowercase();
        let matches = |words: &[&str]| name_matches(&name, words) || sql_matches(&sql, words);
        let start = if let Some((start, prev_word)) = &prev
            && matches(&[prev_word, &last])
        {
            Some(*start)
        } else if matches(&[&last]) {
            Some(last_start)
        } else {
            None
        };
        let Some(start) = start else { continue };
        if trimmed.len() - start < 2 {
            continue;
        }
        out.push(Suggestion {
            name: snippet.name.clone(),
            sql: snippet.sql.clone(),
            replace_len: line.len() - start,
        });
    }
    // Two-word matches are more specific than last-word-only ones; rank
    // them first.
    out.sort_by_key(|suggestion| std::cmp::Reverse(suggestion.replace_len));
    out
}

/// Byte offset where the last whitespace-separated word of `text` starts;
/// `None` when there is none. `text` must not end with whitespace.
fn last_word_start(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let start = text.rfind(char::is_whitespace).map_or(0, |ws| {
        ws + text[ws..].chars().next().map_or(1, char::len_utf8)
    });
    Some(start)
}

/// Every typed word is a prefix of some word of the snippet name.
fn name_matches(name_lower: &str, words: &[&str]) -> bool {
    words.iter().all(|word| {
        !word.is_empty()
            && name_lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|part| part.starts_with(word))
    })
}

/// The typed words are the snippet's leading SQL words, the last possibly
/// partial ("create seq" matches "CREATE SEQUENCE …").
fn sql_matches(sql_lower: &str, words: &[&str]) -> bool {
    let mut sql_words = sql_lower.split_whitespace();
    words.iter().enumerate().all(|(ix, word)| {
        sql_words.next().is_some_and(|sql_word| {
            if ix + 1 == words.len() {
                !word.is_empty() && sql_word.starts_with(word)
            } else {
                sql_word == *word
            }
        })
    })
}

/// Replace every marker with its placeholder text, for picker previews.
fn strip_tab_stops(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for stop in tab_stops(text) {
        out.push_str(&text[last..stop.range.start]);
        out.push_str(&stop.placeholder);
        last = stop.range.end;
    }
    out.push_str(&text[last..]);
    out
}

pub struct Library {
    pub user: Vec<Rc<Snippet>>,
    pub builtin: Vec<Rc<Snippet>>,
}

fn dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("pg-gui").join("snippets"))
}

/// Create the user snippets directory so it's discoverable. Best effort.
pub fn ensure_dir() {
    if let Some(dir) = dir() {
        std::fs::create_dir_all(dir).ok();
    }
}

/// Load user snippets from disk and merge them with the built-ins: both
/// sorted by name, built-ins shadowed by same-named user files.
pub fn load() -> Library {
    let mut user = Vec::new();
    if let Some(dir) = dir()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("sql") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(sql) = std::fs::read_to_string(&path) else {
                continue;
            };
            if sql.trim().is_empty() {
                continue;
            }
            user.push(Rc::new(Snippet::new(name.to_string(), sql)));
        }
    }
    user.sort_by(|a, b| a.name.cmp(&b.name));

    let shadowed: HashSet<&str> = user.iter().map(|snippet| snippet.name.as_ref()).collect();
    let mut builtin: Vec<Rc<Snippet>> = BUILTINS
        .iter()
        .filter(|(name, _)| !shadowed.contains(name))
        .map(|(name, sql)| Rc::new(Snippet::new((*name).to_string(), (*sql).to_string())))
        .collect();
    builtin.sort_by(|a, b| a.name.cmp(&b.name));

    Library { user, builtin }
}

/// Called with the chosen snippet when the user confirms a picker entry.
type OnPick = Box<dyn Fn(&Snippet, &mut Window, &mut App)>;

/// List delegate for the snippet picker: user snippets first, then
/// built-ins, filtered by name as the user types. Empty sections are
/// dropped entirely — the virtual list measures row height from the item
/// at (0, 0), so that index must always be occupied while anything is
/// listed.
pub struct PickerDelegate {
    sections: Vec<(SharedString, Vec<Rc<Snippet>>)>,
    filtered: Vec<(SharedString, Vec<Rc<Snippet>>)>,
    selected: Option<IndexPath>,
    on_pick: OnPick,
}

impl PickerDelegate {
    pub fn new(
        library: Library,
        on_pick: impl Fn(&Snippet, &mut Window, &mut App) + 'static,
    ) -> Self {
        let sections = vec![
            ("Your snippets".into(), library.user),
            ("Built-in".into(), library.builtin),
        ];
        let mut this = Self {
            sections,
            filtered: Vec::new(),
            selected: None,
            on_pick: Box::new(on_pick),
        };
        this.filter("");
        this
    }

    fn filter(&mut self, query: &str) {
        // Every whitespace-separated word must appear in the name, so
        // "new table" finds "New: table" despite the colon.
        let query = query.to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        self.filtered = self
            .sections
            .iter()
            .map(|(label, snippets)| {
                let matched = snippets
                    .iter()
                    .filter(|snippet| {
                        let name = snippet.name.to_lowercase();
                        words.iter().all(|word| name.contains(word))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (label.clone(), matched)
            })
            .filter(|(_, matched)| !matched.is_empty())
            .collect();
        self.selected = if self.filtered.is_empty() {
            None
        } else {
            Some(IndexPath::default())
        };
    }

    fn snippet(&self, ix: IndexPath) -> Option<&Rc<Snippet>> {
        self.filtered.get(ix.section)?.1.get(ix.row)
    }
}

impl ListDelegate for PickerDelegate {
    type Item = ListItem;

    fn sections_count(&self, _: &App) -> usize {
        self.filtered.len()
    }

    fn items_count(&self, section: usize, _: &App) -> usize {
        self.filtered
            .get(section)
            .map_or(0, |(_, matched)| matched.len())
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
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let snippet = self.snippet(ix)?;
        let preview = div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .overflow_hidden()
            .whitespace_nowrap()
            .child(snippet.preview.clone());
        Some(
            ListItem::new(ix).selected(self.selected == Some(ix)).child(
                v_flex()
                    .py_1()
                    .child(div().text_sm().child(snippet.name.clone()))
                    .child(preview),
            ),
        )
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
        if let Some(snippet) = self.selected.and_then(|ix| self.snippet(ix)).cloned() {
            (self.on_pick)(&snippet, window, cx);
        }
    }
}

/// Curated queries shipped with the app. Category-prefixed names make the
/// picker's search double as category navigation. Every query runs
/// unedited: name filters default to `ILIKE '%%'` (match all), and
/// destructive templates default to a no-row predicate. The `New:` DDL
/// templates use `${n:placeholder}` tab stops (see [`tab_stops`]), so every
/// blank has a valid default and tab walks through them.
const BUILTINS: &[(&str, &str)] = &[
    (
        "New: table",
        "CREATE TABLE ${1:table_name} (\n    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,\n    ${2:name} ${3:text} NOT NULL,\n    created_at timestamptz NOT NULL DEFAULT now()\n);",
    ),
    (
        "New: view",
        "CREATE OR REPLACE VIEW ${1:view_name} AS\nSELECT ${2:*}\nFROM ${3:table_name};",
    ),
    (
        "New: materialized view",
        "CREATE MATERIALIZED VIEW ${1:view_name} AS\nSELECT ${2:*}\nFROM ${3:table_name}\nWITH DATA;",
    ),
    (
        "New: index",
        "CREATE INDEX ${1:index_name} ON ${2:table_name} (${3:column_name});",
    ),
    (
        "New: function",
        "CREATE OR REPLACE FUNCTION ${1:function_name}(${2})\nRETURNS ${3:integer}\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    ${4:RETURN 0;}\nEND;\n$$;",
    ),
    (
        "New: procedure",
        "CREATE OR REPLACE PROCEDURE ${1:procedure_name}(${2})\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    ${3:NULL;}\nEND;\n$$;",
    ),
    (
        "New: trigger",
        "CREATE OR REPLACE FUNCTION ${1:trigger_fn}()\nRETURNS trigger\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    -- Adjust NEW before it is written.\n    RETURN NEW;\nEND;\n$$;\n\nCREATE TRIGGER ${2:trigger_name}\n    BEFORE INSERT OR UPDATE ON ${3:table_name}\n    FOR EACH ROW\n    EXECUTE FUNCTION ${4:trigger_fn}();",
    ),
    (
        "New: sequence",
        "CREATE SEQUENCE ${1:sequence_name}\n    START WITH ${2:1}\n    INCREMENT BY ${3:1};",
    ),
    (
        "New: enum type",
        "CREATE TYPE ${1:type_name} AS ENUM (${2:'value1', 'value2'});",
    ),
    ("New: schema", "CREATE SCHEMA ${1:schema_name};"),
    (
        "New: role",
        "CREATE ROLE ${1:role_name} WITH LOGIN PASSWORD '${2:changeme}';",
    ),
    ("New: database", "CREATE DATABASE ${1:database_name};"),
    (
        "New: extension",
        "CREATE EXTENSION IF NOT EXISTS ${1:pg_stat_statements};",
    ),
    (
        "Size: database",
        "SELECT current_database() AS database,\n       pg_size_pretty(pg_database_size(current_database())) AS size;",
    ),
    (
        "Size: largest tables",
        "SELECT c.oid::regclass AS \"table\",\n       pg_size_pretty(pg_total_relation_size(c.oid)) AS total_size\nFROM pg_class c\nJOIN pg_namespace n ON n.oid = c.relnamespace\nWHERE c.relkind IN ('r', 'm', 'p')\n  AND n.nspname NOT IN ('pg_catalog', 'information_schema')\nORDER BY pg_total_relation_size(c.oid) DESC\nLIMIT 20;",
    ),
    (
        "Size: largest indexes",
        "SELECT c.oid::regclass AS \"index\",\n       pg_size_pretty(pg_relation_size(c.oid)) AS size\nFROM pg_class c\nJOIN pg_namespace n ON n.oid = c.relnamespace\nWHERE c.relkind = 'i'\n  AND n.nspname NOT IN ('pg_catalog', 'information_schema')\nORDER BY pg_relation_size(c.oid) DESC\nLIMIT 20;",
    ),
    (
        "Size: table & index breakdown",
        "SELECT relid::regclass AS \"table\",\n       pg_size_pretty(pg_relation_size(relid)) AS table_size,\n       pg_size_pretty(pg_indexes_size(relid)) AS indexes_size,\n       pg_size_pretty(pg_total_relation_size(relid)) AS total_size\nFROM pg_stat_user_tables\nWHERE relname ILIKE '%%'\nORDER BY pg_total_relation_size(relid) DESC\nLIMIT 50;",
    ),
    (
        "Activity: running queries",
        "SELECT pid, usename, state, now() - query_start AS runtime,\n       left(query, 120) AS query\nFROM pg_stat_activity\nWHERE state <> 'idle' AND pid <> pg_backend_pid()\nORDER BY query_start;",
    ),
    (
        "Activity: long-running queries",
        "SELECT pid, usename, state, now() - query_start AS runtime,\n       left(query, 200) AS query\nFROM pg_stat_activity\nWHERE state = 'active'\n  AND now() - query_start > interval '1 minute'\n  AND pid <> pg_backend_pid()\nORDER BY runtime DESC;",
    ),
    (
        "Activity: connections by state",
        "SELECT state, count(*)\nFROM pg_stat_activity\nGROUP BY state\nORDER BY count(*) DESC;",
    ),
    (
        "Activity: connections by database & user",
        "SELECT datname AS database, usename AS \"user\", count(*)\nFROM pg_stat_activity\nGROUP BY datname, usename\nORDER BY count(*) DESC;",
    ),
    (
        "Activity: cancel a query",
        "-- Replace NULL with the pid to cancel (see \"Activity: running queries\").\nSELECT pg_cancel_backend(pid)\nFROM pg_stat_activity\nWHERE pid = NULL::int;",
    ),
    (
        "Activity: terminate a backend",
        "-- Replace NULL with the pid to terminate. Terminating rolls back its transaction.\nSELECT pg_terminate_backend(pid)\nFROM pg_stat_activity\nWHERE pid = NULL::int;",
    ),
    (
        "Locks: blocking chains",
        "SELECT blocked.pid AS blocked_pid,\n       left(blocked.query, 100) AS blocked_query,\n       blocking.pid AS blocking_pid,\n       left(blocking.query, 100) AS blocking_query\nFROM pg_stat_activity blocked\nJOIN LATERAL unnest(pg_blocking_pids(blocked.pid)) AS b(pid) ON true\nJOIN pg_stat_activity blocking ON blocking.pid = b.pid;",
    ),
    (
        "Locks: waiting locks",
        "SELECT a.pid, a.usename, l.locktype, l.mode, l.granted,\n       left(a.query, 120) AS query\nFROM pg_locks l\nJOIN pg_stat_activity a ON a.pid = l.pid\nWHERE NOT l.granted\nORDER BY a.query_start;",
    ),
    (
        "Perf: cache hit ratio",
        "SELECT round(sum(blks_hit) * 100.0\n             / nullif(sum(blks_hit) + sum(blks_read), 0), 2) AS cache_hit_pct\nFROM pg_stat_database;",
    ),
    (
        "Perf: seq-scan-heavy tables",
        "SELECT relname AS \"table\", seq_scan, idx_scan, n_live_tup AS approx_rows\nFROM pg_stat_user_tables\nWHERE seq_scan > 0\nORDER BY seq_scan DESC\nLIMIT 20;",
    ),
    (
        "Perf: unused indexes",
        "SELECT s.indexrelid::regclass AS \"index\",\n       s.relid::regclass AS \"table\",\n       pg_size_pretty(pg_relation_size(s.indexrelid)) AS size,\n       s.idx_scan\nFROM pg_stat_user_indexes s\nJOIN pg_index i ON i.indexrelid = s.indexrelid\nWHERE s.idx_scan = 0 AND NOT i.indisunique AND NOT i.indisprimary\nORDER BY pg_relation_size(s.indexrelid) DESC;",
    ),
    (
        "Perf: slowest queries (pg_stat_statements)",
        "-- Requires the pg_stat_statements extension.\nSELECT calls,\n       round(mean_exec_time::numeric, 2) AS mean_ms,\n       round(total_exec_time::numeric, 2) AS total_ms,\n       left(query, 200) AS query\nFROM pg_stat_statements\nORDER BY mean_exec_time DESC\nLIMIT 20;",
    ),
    (
        "Vacuum: dead tuples by table",
        "SELECT relname AS \"table\", n_dead_tup, n_live_tup,\n       last_vacuum, last_autovacuum\nFROM pg_stat_user_tables\nORDER BY n_dead_tup DESC\nLIMIT 20;",
    ),
    (
        "Vacuum: progress",
        "SELECT pid, relid::regclass AS \"table\", phase,\n       heap_blks_scanned, heap_blks_total\nFROM pg_stat_progress_vacuum;",
    ),
    (
        "Replication: status",
        "SELECT client_addr, usename, state, sync_state,\n       sent_lsn, replay_lsn, replay_lag\nFROM pg_stat_replication;",
    ),
    (
        "Replication: slots",
        "SELECT slot_name, slot_type, active,\n       pg_size_pretty(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn)) AS retained_wal\nFROM pg_replication_slots;",
    ),
    (
        "Schema: tables with row estimates",
        "SELECT schemaname AS schema, relname AS \"table\",\n       n_live_tup AS approx_rows,\n       pg_size_pretty(pg_total_relation_size(relid)) AS total_size\nFROM pg_stat_user_tables\nWHERE relname ILIKE '%%'\nORDER BY n_live_tup DESC;",
    ),
    (
        "Schema: columns of a table",
        "SELECT table_schema, table_name, column_name, data_type,\n       is_nullable, column_default\nFROM information_schema.columns\nWHERE table_schema NOT IN ('pg_catalog', 'information_schema')\n  AND table_name ILIKE '%%'\nORDER BY table_schema, table_name, ordinal_position;",
    ),
    (
        "Schema: find a column across tables",
        "SELECT table_schema, table_name, column_name, data_type\nFROM information_schema.columns\nWHERE table_schema NOT IN ('pg_catalog', 'information_schema')\n  AND column_name ILIKE '%%'\nORDER BY table_schema, table_name, ordinal_position;",
    ),
    (
        "Schema: indexes of a table",
        "SELECT schemaname AS schema, tablename AS \"table\",\n       indexname AS \"index\", indexdef\nFROM pg_indexes\nWHERE schemaname NOT IN ('pg_catalog', 'information_schema')\n  AND tablename ILIKE '%%'\nORDER BY schemaname, tablename, indexname;",
    ),
    (
        "Schema: constraints of a table",
        "SELECT conrelid::regclass AS \"table\", conname AS \"constraint\",\n       contype, pg_get_constraintdef(oid) AS definition\nFROM pg_constraint\nWHERE conrelid <> 0\n  AND connamespace NOT IN ('pg_catalog'::regnamespace, 'information_schema'::regnamespace)\n  AND conrelid::regclass::text ILIKE '%%'\nORDER BY conrelid::regclass::text, conname;",
    ),
    (
        "Schema: foreign keys",
        "SELECT conrelid::regclass AS \"table\", conname AS fk_name,\n       confrelid::regclass AS \"references\",\n       pg_get_constraintdef(oid) AS definition\nFROM pg_constraint\nWHERE contype = 'f' AND conrelid::regclass::text ILIKE '%%'\nORDER BY 1, 2;",
    ),
    (
        "Schema: functions",
        "SELECT n.nspname AS schema, p.proname AS \"function\",\n       pg_get_function_arguments(p.oid) AS arguments,\n       pg_get_function_result(p.oid) AS \"returns\"\nFROM pg_proc p\nJOIN pg_namespace n ON n.oid = p.pronamespace\nWHERE n.nspname NOT IN ('pg_catalog', 'information_schema')\n  AND p.proname ILIKE '%%'\nORDER BY n.nspname, p.proname;",
    ),
    (
        "Schema: views",
        "SELECT schemaname AS schema, viewname AS \"view\", definition\nFROM pg_views\nWHERE schemaname NOT IN ('pg_catalog', 'information_schema')\n  AND viewname ILIKE '%%'\nORDER BY schemaname, viewname;",
    ),
    (
        "Schema: triggers",
        "SELECT event_object_schema AS schema, event_object_table AS \"table\",\n       trigger_name, action_timing, event_manipulation, action_statement\nFROM information_schema.triggers\nWHERE event_object_table ILIKE '%%'\nORDER BY event_object_schema, event_object_table, trigger_name;",
    ),
    (
        "Schema: sequences",
        "SELECT schemaname AS schema, sequencename AS \"sequence\",\n       last_value, increment_by\nFROM pg_sequences\nORDER BY schemaname, sequencename;",
    ),
    (
        "Schema: extensions",
        "SELECT extname AS extension, extversion AS version\nFROM pg_extension\nORDER BY extname;",
    ),
    (
        "Config: non-default settings",
        "SELECT name, setting, unit, source\nFROM pg_settings\nWHERE source NOT IN ('default', 'override')\nORDER BY name;",
    ),
    (
        "Config: memory & connections",
        "SELECT name, setting, unit\nFROM pg_settings\nWHERE name IN ('shared_buffers', 'work_mem', 'maintenance_work_mem',\n               'effective_cache_size', 'max_connections', 'max_parallel_workers')\nORDER BY name;",
    ),
    (
        "Users: roles",
        "SELECT rolname AS \"role\", rolsuper AS superuser,\n       rolcreatedb AS can_create_db, rolcanlogin AS can_login,\n       rolconnlimit AS conn_limit\nFROM pg_roles\nORDER BY rolname;",
    ),
    (
        "Users: table privileges",
        "SELECT grantee, table_schema, table_name, privilege_type\nFROM information_schema.table_privileges\nWHERE table_schema NOT IN ('pg_catalog', 'information_schema')\n  AND grantee ILIKE '%%'\nORDER BY grantee, table_schema, table_name;",
    ),
];

#[cfg(test)]
mod tests {
    use super::{BUILTINS, has_tab_stops, next_tab_stop, strip_tab_stops, tab_stops};

    #[test]
    fn parses_all_marker_forms() {
        let text = "a $1 b ${2} c ${3:placeholder} d";
        let stops = tab_stops(text);
        assert_eq!(stops.len(), 3);
        assert_eq!(&text[stops[0].range.clone()], "$1");
        assert_eq!(stops[0].placeholder, "");
        assert_eq!(&text[stops[1].range.clone()], "${2}");
        assert_eq!(stops[1].placeholder, "");
        assert_eq!(&text[stops[2].range.clone()], "${3:placeholder}");
        assert_eq!(stops[2].placeholder, "placeholder");
    }

    #[test]
    fn ignores_dollar_quoting_and_plain_dollars() {
        assert!(!has_tab_stops("AS $$ BEGIN RETURN NEW; END; $$"));
        assert!(!has_tab_stops("AS $body$ ... $body$"));
        assert!(!has_tab_stops("price > 100$ and ${x} and ${1x}"));
    }

    #[test]
    fn visits_lowest_number_first_and_zero_last() {
        let stop = next_tab_stop("${2:b} then ${1:a}", false).unwrap();
        assert_eq!(stop.placeholder, "a");
        let stop = next_tab_stop("${0:last} then ${3:first}", false).unwrap();
        assert_eq!(stop.placeholder, "first");
    }

    #[test]
    fn leftmost_wins_a_number_tie() {
        let text = "${1:left} then ${1:right}";
        let stop = next_tab_stop(text, false).unwrap();
        assert_eq!(stop.range.start, 0);
    }

    #[test]
    fn braced_only_skips_bare_markers() {
        let text = "WHERE id = $1 AND name = ${2:name}";
        let stop = next_tab_stop(text, true).unwrap();
        assert_eq!(stop.placeholder, "name");
        assert!(next_tab_stop("WHERE id = $1", true).is_none());
    }

    #[test]
    fn strips_markers_to_placeholders() {
        assert_eq!(
            strip_tab_stops("CREATE TABLE ${1:table_name} ($2)"),
            "CREATE TABLE table_name ()"
        );
    }

    fn test_library() -> super::Library {
        let builtin = super::BUILTINS
            .iter()
            .map(|(name, sql)| {
                std::rc::Rc::new(super::Snippet::new((*name).to_string(), (*sql).to_string()))
            })
            .collect();
        super::Library {
            user: Vec::new(),
            builtin,
        }
    }

    #[test]
    fn suggests_by_name_words() {
        let library = test_library();
        let found = super::library_suggestions(&library, "SELECT 1; new tab");
        // The two-word match ranks first; single-word "tab" matches
        // (e.g. "Schema: tables…") may follow.
        assert_eq!(found[0].name.as_ref(), "New: table");
        assert_eq!(found[0].replace_len, "new tab".len());
    }

    #[test]
    fn suggests_by_leading_sql_words() {
        let library = test_library();
        let found = super::library_suggestions(&library, "create seq");
        assert_eq!(found[0].name.as_ref(), "New: sequence");
        assert_eq!(found[0].replace_len, "create seq".len());
    }

    #[test]
    fn falls_back_to_the_last_word_alone() {
        let library = test_library();
        let found = super::library_suggestions(&library, "SELECT * FROM new");
        assert!(!found.is_empty());
        assert!(found.iter().all(|s| s.replace_len == "new".len()));
    }

    #[test]
    fn replaces_trailing_whitespace_too() {
        let library = test_library();
        let found = super::library_suggestions(&library, "new  ");
        assert!(!found.is_empty());
        assert_eq!(found[0].replace_len, "new  ".len());
    }

    #[test]
    fn short_or_unmatched_queries_suggest_nothing() {
        let library = test_library();
        assert!(super::library_suggestions(&library, "n").is_empty());
        assert!(super::library_suggestions(&library, "banana").is_empty());
        assert!(super::library_suggestions(&library, "   ").is_empty());
        assert!(super::library_suggestions(&library, "").is_empty());
    }

    #[test]
    fn new_templates_have_stops_and_valid_defaults() {
        for (name, sql) in BUILTINS.iter().filter(|(n, _)| n.starts_with("New:")) {
            assert!(has_tab_stops(sql), "{name} has no tab stops");
            // Substituting every placeholder must leave no marker behind.
            assert!(
                !has_tab_stops(&strip_tab_stops(sql)),
                "{name} leaves a marker after substitution"
            );
        }
    }
}
