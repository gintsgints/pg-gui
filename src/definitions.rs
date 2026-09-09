//! Go to Definition for the SQL editor: cmd-clicking a routine or relation
//! name opens the object's code.
//!
//! The language server has nothing to offer here — `pgls` has no definition
//! feature, and its hover returns rendered markdown rather than object
//! identity — so the symbol under the cursor is resolved against the catalog
//! ([`db::find_object`]) instead. That also means this keeps working while the
//! language server is down.
//!
//! The editor's [`DefinitionProvider`] can only move the cursor inside the
//! current buffer, but opening a database object is the app's job (find its
//! `.sql` file in the working folder, else fetch its definition into a tab —
//! the same path a click in the database browser takes). So the target of the
//! returned link is a `pggui:kind/schema/object` URI, which the editor hands
//! back through its `show_document` hook for [`crate::app::PgGuiApp`] to open.

use std::collections::HashMap;
use std::ops::Range as ByteRange;
use std::str::FromStr as _;

use anyhow::Result;
use gpui::{App, AppContext as _, Task, WeakEntity, Window};
use gpui_component::input::{DefinitionProvider, Rope, RopeExt as _};
use lsp_types::{LocationLink, Position, Range, Uri};

use crate::app::{PgGuiApp, percent_decode, percent_encode};
use crate::{db, db_tree};

/// The URI scheme the editor hands back to the app, naming a database object
/// rather than a file on disk.
const SCHEME: &str = "pggui";

/// An identifier under the cursor, as written in the script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// The `schema.` qualifier, when the identifier carried one.
    pub schema: Option<String>,
    pub name: String,
    /// Byte range of the whole (qualified) identifier in the buffer; this is
    /// what the editor underlines while cmd is held.
    pub range: ByteRange<usize>,
}

impl Symbol {
    /// Key this symbol is cached under: case-folded the way an unquoted
    /// identifier reaches the server.
    fn key(&self) -> (String, String) {
        (
            self.schema.as_deref().unwrap_or_default().to_lowercase(),
            self.name.to_lowercase(),
        )
    }
}

/// A database object a symbol resolved to, in the terms the object-opening
/// path speaks: a browser [`db_tree::NodeKind`] plus the name its definition
/// query expects (a `name(identity arguments)` signature for a routine, the
/// plain name for a relation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub kind: db_tree::NodeKind,
    pub schema: String,
    pub object: String,
}

impl Target {
    /// Map a catalog hit onto a browser node kind. `table` becomes
    /// `TableDefinition` — the leaf kind that carries a table's DDL, since
    /// `Table` itself is a branch node with no definition of its own.
    fn from_ref(found: db::ObjectRef) -> Option<Self> {
        let kind = match found.kind.as_str() {
            "function" => db_tree::NodeKind::Function,
            "view" => db_tree::NodeKind::View,
            "matview" => db_tree::NodeKind::MatView,
            "table" => db_tree::NodeKind::TableDefinition,
            _ => return None,
        };
        Some(Self {
            kind,
            schema: found.schema,
            object: found.object,
        })
    }

    fn kind_tag(&self) -> Option<&'static str> {
        match self.kind {
            db_tree::NodeKind::Function => Some("function"),
            db_tree::NodeKind::View => Some("view"),
            db_tree::NodeKind::MatView => Some("matview"),
            db_tree::NodeKind::TableDefinition => Some("table"),
            _ => None,
        }
    }
}

/// The identifier at `offset`, with its `schema.` qualifier when it has one.
/// The editor's own word scan stops at the dot, so the qualifier is picked up
/// here — both to resolve the right object and so the underline spans the
/// whole name.
#[must_use]
pub fn symbol_at(text: &Rope, offset: usize) -> Option<Symbol> {
    let range = text.word_range(offset)?;
    let name = text.word_at(range.start);
    // A number, or a word that starts like one, is not an object name.
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }

    let mut chars = text.chars_at(range.start).reversed();
    if chars.next() != Some('.') {
        return Some(Symbol {
            schema: None,
            name,
            range,
        });
    }
    let mut schema = String::new();
    for c in chars {
        if c.is_alphanumeric() || c == '_' {
            schema.insert(0, c);
        } else {
            break;
        }
    }
    if schema.is_empty() {
        return Some(Symbol {
            schema: None,
            name,
            range,
        });
    }
    let start = range.start - '.'.len_utf8() - schema.len();
    Some(Symbol {
        schema: Some(schema),
        name,
        range: start..range.end,
    })
}

/// The `pggui:` URI naming `target`, handed to the editor as a link target.
#[must_use]
pub fn target_uri(target: &Target) -> Option<Uri> {
    let kind = target.kind_tag()?;
    let uri = format!(
        "{SCHEME}:{kind}/{}/{}",
        percent_encode(&target.schema),
        percent_encode(&target.object)
    );
    Uri::from_str(&uri).ok()
}

/// Read back what [`target_uri`] wrote. Anything else (a real file, an http
/// link) is not ours and yields `None`, leaving the editor's own handling.
#[must_use]
pub fn parse_uri(uri: &Uri) -> Option<Target> {
    let uri = uri.to_string();
    let path = uri.strip_prefix(SCHEME)?.strip_prefix(':')?;
    let mut parts = path.splitn(3, '/');
    let kind = match parts.next()? {
        "function" => db_tree::NodeKind::Function,
        "view" => db_tree::NodeKind::View,
        "matview" => db_tree::NodeKind::MatView,
        "table" => db_tree::NodeKind::TableDefinition,
        _ => return None,
    };
    let schema = percent_decode(parts.next()?);
    let object = percent_decode(parts.next()?);
    if schema.is_empty() || object.is_empty() {
        return None;
    }
    Some(Target {
        kind,
        schema,
        object,
    })
}

/// Resolved symbols, kept because every lookup opens its own connection and
/// the editor asks again on each mouse move while cmd is held. Misses are
/// cached too — that is the case that would otherwise reconnect per pixel,
/// including when the server is unreachable. Held by
/// [`crate::app::PgGuiApp`], which drops it when the connection changes.
#[derive(Default)]
pub struct Cache {
    /// The connection the entries were resolved against; a different one
    /// invalidates all of them.
    conn: String,
    entries: HashMap<(String, String), Option<Target>>,
}

/// What the cache knows about a symbol.
pub enum Cached {
    /// Not looked up against this connection yet.
    Unknown,
    /// Looked up, and it names no object we can open (or the lookup failed).
    Missing,
    Found(Target),
}

impl Cache {
    /// The cached answer for `key` on this connection.
    pub fn get(&mut self, conn: &str, key: &(String, String)) -> Cached {
        if self.conn != conn {
            self.conn = conn.to_string();
            self.entries.clear();
        }
        match self.entries.get(key) {
            None => Cached::Unknown,
            Some(None) => Cached::Missing,
            Some(Some(target)) => Cached::Found(target.clone()),
        }
    }

    pub fn insert(&mut self, conn: &str, key: (String, String), target: Option<Target>) {
        if self.conn != conn {
            self.conn = conn.to_string();
            self.entries.clear();
        }
        self.entries.insert(key, target);
    }

    /// Forget everything, so the next cmd-hover asks the server again.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The editor-side provider: resolves the symbol under the cursor and points
/// the link at a `pggui:` URI.
pub struct Provider {
    app: WeakEntity<PgGuiApp>,
}

impl Provider {
    #[must_use]
    pub fn new(app: WeakEntity<PgGuiApp>) -> Self {
        Self { app }
    }
}

impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let Some(symbol) = symbol_at(text, offset) else {
            return Task::ready(Ok(vec![]));
        };
        let Some(app) = self.app.upgrade() else {
            return Task::ready(Ok(vec![]));
        };
        let conn = app.read(cx).connection_string().to_string();
        if conn.is_empty() {
            return Task::ready(Ok(vec![]));
        }

        let key = symbol.key();
        match app.update(cx, |this, _| this.definition_cache_get(&conn, &key)) {
            Cached::Found(target) => {
                return Task::ready(Ok(links(Some(&target), &symbol, text)));
            }
            Cached::Missing => return Task::ready(Ok(vec![])),
            Cached::Unknown => {}
        }

        let rope = text.clone();
        // The weak handle, not the upgraded one: the app owns the editor that
        // owns this task, so a strong handle in it would be a cycle.
        let app = self.app.clone();
        let (query_conn, schema, name) = (conn.clone(), symbol.schema.clone(), symbol.name.clone());
        cx.spawn(async move |cx| {
            let found = cx
                .background_spawn(
                    async move { db::find_object(&query_conn, schema.as_deref(), &name) },
                )
                .await;
            // A failed lookup caches as a miss: the editor asks again on the
            // very next mouse move, and retrying a dead connection there would
            // stall a background thread per pixel.
            let target = found.ok().flatten().and_then(Target::from_ref);
            app.update(cx, |this, _| {
                this.definition_cache_insert(&conn, key, target.clone());
            })
            .ok();
            Ok(links(target.as_ref(), &symbol, &rope))
        })
    }
}

/// The link list for a resolved (or unresolved) symbol. `target_range` and
/// `target_selection_range` are unused: the app's `show_document` hook always
/// claims a `pggui:` URI before the editor's own in-buffer jump can run.
fn links(target: Option<&Target>, symbol: &Symbol, text: &Rope) -> Vec<LocationLink> {
    let Some(uri) = target.and_then(target_uri) else {
        return vec![];
    };
    let origin = Range {
        start: text.offset_to_position(symbol.range.start),
        end: text.offset_to_position(symbol.range.end),
    };
    let empty = Range::new(Position::new(0, 0), Position::new(0, 0));
    vec![LocationLink {
        origin_selection_range: Some(origin),
        target_uri: uri,
        target_range: empty,
        target_selection_range: empty,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(text: &str, offset: usize) -> Option<Symbol> {
        symbol_at(&Rope::from(text), offset)
    }

    #[test]
    fn reads_a_bare_identifier() {
        let text = "CALL place_order(1);";
        let found = symbol(text, 8).expect("a symbol");
        assert_eq!(found.schema, None);
        assert_eq!(found.name, "place_order");
        assert_eq!(&text[found.range], "place_order");
    }

    #[test]
    fn reads_the_schema_qualifier() {
        let text = "CALL order_utils.place_order(1);";
        let found = symbol(text, 20).expect("a symbol");
        assert_eq!(found.schema.as_deref(), Some("order_utils"));
        assert_eq!(found.name, "place_order");
        // The underline spans the whole qualified name, not just the routine.
        assert_eq!(&text[found.range], "order_utils.place_order");
    }

    #[test]
    fn the_cursor_on_the_qualifier_names_the_schema() {
        let text = "CALL order_utils.place_order(1);";
        let found = symbol(text, 8).expect("a symbol");
        assert_eq!(found.schema, None);
        assert_eq!(found.name, "order_utils");
    }

    #[test]
    fn reaches_the_word_the_cursor_sits_at_the_end_of() {
        let text = "SELECT customers;";
        let found = symbol(text, 16).expect("a symbol");
        assert_eq!(found.name, "customers");
        assert_eq!(&text[found.range], "customers");
    }

    #[test]
    fn ignores_numbers() {
        // On the number's first digit, its second, and the `;` right after it.
        for offset in [7, 8, 9] {
            assert!(symbol("SELECT 42;", offset).is_none(), "at {offset}");
        }
    }

    #[test]
    fn uri_round_trips_a_signature() {
        let target = Target {
            kind: db_tree::NodeKind::Function,
            schema: "order utils".to_string(),
            object: "place_order(integer, text)".to_string(),
        };
        let uri = target_uri(&target).expect("a uri");
        assert_eq!(parse_uri(&uri), Some(target));
    }

    #[test]
    fn uri_round_trips_every_kind() {
        for kind in [
            db_tree::NodeKind::Function,
            db_tree::NodeKind::View,
            db_tree::NodeKind::MatView,
            db_tree::NodeKind::TableDefinition,
        ] {
            let target = Target {
                kind,
                schema: "public".to_string(),
                object: "customers".to_string(),
            };
            let uri = target_uri(&target).expect("a uri");
            assert_eq!(parse_uri(&uri), Some(target));
        }
    }

    #[test]
    fn a_file_uri_is_not_ours() {
        let uri = Uri::from_str("file:///tmp/place_order.sql").expect("a uri");
        assert_eq!(parse_uri(&uri), None);
    }

    /// Needs the docker database (`docker compose up -d`) with `sql/` applied.
    #[test]
    #[ignore = "requires the local docker database"]
    fn resolves_a_routine_against_the_database() {
        let conn = "postgres://pgui:pgui@localhost:5433/pgui_test";
        let found = db::find_object(conn, None, "place_order")
            .expect("the lookup to run")
            .expect("place_order to exist");
        let target = Target::from_ref(found).expect("a supported kind");
        assert_eq!(target.kind, db_tree::NodeKind::Function);
        assert!(
            target.object.starts_with("place_order("),
            "unexpected signature {}",
            target.object
        );
    }
}
