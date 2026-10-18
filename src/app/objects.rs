//! Opening a database object — from the object browser or a Go to
//! Definition jump — in its `.sql` file from the working folder when one can
//! be found (`find_sql_file` and its name/content ranking), else in a tab
//! holding its definition fetched from the catalog.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, SharedString, Window};

use crate::app::{PgGuiApp, set_editor_value, strip_sql_comments};
use crate::{db, db_tree, definitions};

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
/// catalog query per kind, all on one [`db::Catalog`] connection. Runs on the
/// background executor.
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
    let mut catalog = db::Catalog::connect(conn)?;
    match kind {
        View => catalog.view_definition(schema, object, false),
        MatView => catalog.view_definition(schema, object, true),
        Function => catalog.function_definition(schema, object),
        Sequence => catalog.sequence_definition(schema, object),
        Type => catalog.type_definition(schema, object),
        Index => catalog.index_definition(schema, object),
        Constraint => catalog.constraint_definition(schema, relation, object),
        Trigger => catalog.trigger_definition(schema, relation, object),
        TableDefinition => catalog.table_definition(schema, object),
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

impl PgGuiApp {
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
    pub(in crate::app) fn show_definition_document(
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
    pub(in crate::app) fn open_db_object(
        &mut self,
        id: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        definition_content_rank, ends_with_name_word, find_sql_file, glob_match,
        suggested_name_from_mask,
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
}
