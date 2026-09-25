//! The database object browser's tree model.
//!
//! The tree is schema → object-type folder (Tables, Views, …) → objects, with
//! tables expanding one level further to their Columns, Indexes, Constraints
//! and Triggers folders.
//! Every folder loads lazily, one catalog query per folder expansion.
//! gpui-component's `tree` has no lazy API — a node with zero
//! children can't be expanded, and only a full `set_items` rebuild exists — so
//! this module keeps its own `Vec<DbNode>` as the source of truth and projects
//! it to `TreeItem`s with [`to_tree_items`]. An unloaded (but expandable)
//! folder is given a placeholder child so its expand arrow shows; expanding it
//! fires a catalog query (on the background executor, since [`DbNode`] is `Send`
//! unlike `TreeItem`) whose result fills the folder's children and triggers a
//! re-projection.

use std::collections::HashSet;

use gpui::SharedString;
use gpui_component::tree::TreeItem;

use crate::db;

/// Separator woven into node ids to keep them unique across the hierarchy.
/// A control character can't occur in a rendered label, so ids never collide
/// with one built from a real object name.
const SEP: char = '\u{1}';

/// What a node represents. Drives whether it can be expanded and, for the
/// lazy folders, which catalog query loads its children.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    Schema,
    // Object-type folders under a schema (lazy).
    TablesFolder,
    ViewsFolder,
    MatViewsFolder,
    FunctionsFolder,
    SequencesFolder,
    TypesFolder,
    // A table, which expands to its Definition/Columns/Indexes/Constraints
    // entries.
    Table,
    // The table's reconstructed CREATE TABLE DDL (a leaf).
    TableDefinition,
    // Sub-folders under a table (lazy).
    ColumnsFolder,
    IndexesFolder,
    ConstraintsFolder,
    TriggersFolder,
    // Object leaves.
    View,
    MatView,
    Function,
    Sequence,
    Type,
    Index,
    Constraint,
    Trigger,
    Column,
}

impl NodeKind {
    /// True for the folders whose children are fetched lazily by
    /// [`load_children`]. These are the nodes a per-row refresh re-queries.
    #[must_use]
    pub fn is_lazy_folder(self) -> bool {
        matches!(
            self,
            NodeKind::TablesFolder
                | NodeKind::ViewsFolder
                | NodeKind::MatViewsFolder
                | NodeKind::FunctionsFolder
                | NodeKind::SequencesFolder
                | NodeKind::TypesFolder
                | NodeKind::ColumnsFolder
                | NodeKind::IndexesFolder
                | NodeKind::ConstraintsFolder
                | NodeKind::TriggersFolder
        )
    }
}

/// Load state of a node's children.
#[derive(Clone, Debug)]
pub enum Load {
    /// Expandable, children not fetched yet.
    Unloaded,
    /// A fetch is in flight.
    Loading,
    /// Children are present (possibly an empty set).
    Loaded,
    /// The fetch failed; re-expanding retries.
    Failed(String),
    /// Never expandable.
    Leaf,
}

/// One node of the object browser. `Send` (only `SharedString`/enum/`Vec`
/// fields), so a whole subtree can be built on the background executor.
pub struct DbNode {
    pub id: SharedString,
    pub label: SharedString,
    pub kind: NodeKind,
    /// Owning schema (empty for leaves). Carried so a folder's lazy fetch has
    /// its query context without re-parsing the id.
    pub schema: SharedString,
    /// Owning table, for a table's Indexes/Constraints folders and for
    /// index/constraint leaves (empty otherwise).
    pub relation: SharedString,
    /// The raw catalog name an object leaf refers to, used to look up its
    /// `.sql` file or fetch its definition. Distinct from `label`, which may
    /// carry decorations (a function's `(args)`, an index's ` · PK`, …). Empty
    /// for folders and containers. For functions this is the
    /// `name(identity arguments)` signature.
    pub object: SharedString,
    pub load: Load,
    pub children: Vec<DbNode>,
}

impl DbNode {
    fn folder(
        id: String,
        label: impl Into<SharedString>,
        kind: NodeKind,
        schema: &str,
        relation: &str,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            schema: schema.to_string().into(),
            relation: relation.to_string().into(),
            object: SharedString::default(),
            load: Load::Unloaded,
            children: Vec::new(),
        }
    }

    fn container(
        id: String,
        label: impl Into<SharedString>,
        kind: NodeKind,
        schema: &str,
        children: Vec<DbNode>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            schema: schema.to_string().into(),
            relation: SharedString::default(),
            object: SharedString::default(),
            load: Load::Loaded,
            children,
        }
    }

    /// An object leaf. `schema`/`relation`/`object` carry the context needed
    /// to look up its `.sql` file or fetch its definition on click.
    fn leaf(
        id: String,
        label: impl Into<SharedString>,
        kind: NodeKind,
        schema: &str,
        relation: &str,
        object: &str,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            schema: schema.to_string().into(),
            relation: relation.to_string().into(),
            object: object.to_string().into(),
            load: Load::Leaf,
            children: Vec::new(),
        }
    }

    /// True while this node has never been expandable.
    fn is_leaf(&self) -> bool {
        matches!(self.load, Load::Leaf)
    }

    /// A schema node, pre-populated with its six (still-unloaded) object-type
    /// folders so expanding a schema needs no query.
    fn schema(name: &str) -> Self {
        let id = format!("{SEP}s{SEP}{name}");
        let categories = [
            ("Tables", NodeKind::TablesFolder, "tables"),
            ("Views", NodeKind::ViewsFolder, "views"),
            ("Materialized Views", NodeKind::MatViewsFolder, "matviews"),
            ("Functions", NodeKind::FunctionsFolder, "functions"),
            ("Sequences", NodeKind::SequencesFolder, "sequences"),
            ("Types", NodeKind::TypesFolder, "types"),
        ];
        let children = categories
            .iter()
            .map(|(label, kind, tag)| {
                DbNode::folder(format!("{id}{SEP}{tag}"), *label, *kind, name, "")
            })
            .collect();
        DbNode::container(id, name, NodeKind::Schema, name, children)
    }

    /// A table node, pre-populated with its (still-unloaded) Columns, Indexes,
    /// Constraints and Triggers folders. `parent_id` is the owning Tables
    /// folder's id.
    fn table(parent_id: &str, schema: &str, name: &str) -> Self {
        let id = format!("{parent_id}{SEP}{name}");
        let sub = |tag: &str, label: &str, kind: NodeKind| {
            DbNode::folder(format!("{id}{SEP}{tag}"), label, kind, schema, name)
        };
        let children = vec![
            // A leaf that opens the table's CREATE TABLE DDL; `object` is the
            // table name so a `.sql` lookup and the DDL fetch both use it.
            DbNode::leaf(
                format!("{id}{SEP}def"),
                "definition",
                NodeKind::TableDefinition,
                schema,
                name,
                name,
            ),
            sub("cols", "Columns", NodeKind::ColumnsFolder),
            sub("idx", "Indexes", NodeKind::IndexesFolder),
            sub("cons", "Constraints", NodeKind::ConstraintsFolder),
            sub("trg", "Triggers", NodeKind::TriggersFolder),
        ];
        DbNode::container(id, name, NodeKind::Table, schema, children)
    }
}

/// Fetch the schema list and build the top level of the tree.
pub fn load_schemas(conn_str: &str, show_system: bool) -> Result<Vec<DbNode>, String> {
    Ok(db::list_schemas(conn_str, show_system)?
        .iter()
        .map(|name| DbNode::schema(name))
        .collect())
}

/// Fetch and build the object leaves of a lazy object-type folder. Returns an
/// empty vector for a node kind that is not a folder (which should never be
/// reached, since only folders are left `Unloaded`).
pub fn load_children(
    conn_str: &str,
    kind: NodeKind,
    parent_id: &str,
    schema: &str,
    relation: &str,
) -> Result<Vec<DbNode>, String> {
    let leaves = |names: Vec<String>, node_kind: NodeKind| -> Vec<DbNode> {
        names
            .iter()
            .map(|name| {
                DbNode::leaf(
                    format!("{parent_id}{SEP}{name}"),
                    name.clone(),
                    node_kind,
                    schema,
                    "",
                    name,
                )
            })
            .collect()
    };
    let relation_leaves = |relkind: char, node_kind: NodeKind| -> Result<Vec<DbNode>, String> {
        Ok(leaves(
            db::list_relations(conn_str, schema, relkind)?,
            node_kind,
        ))
    };

    match kind {
        NodeKind::TablesFolder => Ok(db::list_relations(conn_str, schema, 'r')?
            .iter()
            .map(|name| DbNode::table(parent_id, schema, name))
            .collect()),
        NodeKind::ViewsFolder => relation_leaves('v', NodeKind::View),
        NodeKind::MatViewsFolder => relation_leaves('m', NodeKind::MatView),
        NodeKind::SequencesFolder => relation_leaves('S', NodeKind::Sequence),
        NodeKind::FunctionsFolder => Ok(leaves(
            db::list_functions(conn_str, schema)?,
            NodeKind::Function,
        )),
        NodeKind::TypesFolder => Ok(leaves(db::list_types(conn_str, schema)?, NodeKind::Type)),
        _ => load_table_children(conn_str, kind, parent_id, schema, relation),
    }
}

/// The lazy folders under a table row: its columns, indexes, constraints and
/// triggers. Split out of [`load_children`] so neither half grows unwieldy.
fn load_table_children(
    conn_str: &str,
    kind: NodeKind,
    parent_id: &str,
    schema: &str,
    relation: &str,
) -> Result<Vec<DbNode>, String> {
    // Every leaf here belongs to `schema.relation` and is named by its own
    // catalog name; only the drawn label differs per kind.
    let leaf = |name: &str, label: String, node_kind: NodeKind| {
        DbNode::leaf(
            format!("{parent_id}{SEP}{name}"),
            label,
            node_kind,
            schema,
            relation,
            name,
        )
    };

    match kind {
        NodeKind::ColumnsFolder => Ok(db::list_columns(conn_str, schema, relation)?
            .iter()
            .map(|col| leaf(&col.name, column_label(col), NodeKind::Column))
            .collect()),
        NodeKind::IndexesFolder => Ok(db::list_indexes(conn_str, schema, relation)?
            .iter()
            .map(|idx| {
                let mut label = idx.name.clone();
                if idx.primary {
                    label.push_str(" · PK");
                } else if idx.unique {
                    label.push_str(" · unique");
                }
                leaf(&idx.name, label, NodeKind::Index)
            })
            .collect()),
        NodeKind::ConstraintsFolder => Ok(db::list_constraints(conn_str, schema, relation)?
            .iter()
            .map(|con| {
                let kind_label = match con.kind {
                    'p' => "PK",
                    'f' => "FK",
                    'u' => "unique",
                    'c' => "check",
                    'x' => "exclude",
                    _ => "?",
                };
                leaf(
                    &con.name,
                    format!("{} ({kind_label})", con.name),
                    NodeKind::Constraint,
                )
            })
            .collect()),
        NodeKind::TriggersFolder => Ok(db::list_triggers(conn_str, schema, relation)?
            .iter()
            .map(|trg| leaf(&trg.name, trg.name.clone(), NodeKind::Trigger))
            .collect()),
        _ => Ok(Vec::new()),
    }
}

/// Longest default expression shown on a column row before it is elided, so
/// one long default cannot dominate the panel's width.
const MAX_DEFAULT_LEN: usize = 40;

/// A column row's label: name, type, then the markers that matter. A primary
/// key column is implicitly `NOT NULL`, so only the `PK` marker is drawn for
/// it.
fn column_label(col: &db::ColumnInfo) -> String {
    let mut label = format!("{} {}", col.name, col.data_type);
    if col.primary {
        label.push_str(" · PK");
    } else if col.not_null {
        label.push_str(" · not null");
    }
    if let Some(default) = &col.default {
        label.push_str(" · default ");
        label.push_str(&elide(default, MAX_DEFAULT_LEN));
    }
    label
}

/// Truncate `text` to at most `max` characters, marking the cut with an
/// ellipsis. Counts characters, not bytes, so a multi-byte default never
/// splits mid-character.
fn elide(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// Reset every lazy-folder descendant of `node` to [`Load::Unloaded`],
/// dropping any fetched children. Ids of the ones currently in `expanded`
/// are pushed to `reload` so the caller can re-fetch them in place. Used to
/// refresh a container (schema/table) row: its lazy sub-folders are cleared
/// and the visible ones re-queried.
pub fn reset_lazy_descendants(
    node: &mut DbNode,
    expanded: &HashSet<SharedString>,
    reload: &mut Vec<SharedString>,
) {
    for child in &mut node.children {
        if child.kind.is_lazy_folder() {
            child.children.clear();
            child.load = Load::Unloaded;
            if expanded.contains(&child.id) {
                reload.push(child.id.clone());
            }
        } else {
            reset_lazy_descendants(child, expanded, reload);
        }
    }
}

/// Find a node by id, walking the whole tree.
pub fn find<'a>(nodes: &'a [DbNode], id: &SharedString) -> Option<&'a DbNode> {
    for node in nodes {
        if node.id == *id {
            return Some(node);
        }
        if let Some(found) = find(&node.children, id) {
            return Some(found);
        }
    }
    None
}

/// Find a node by id for mutation.
pub fn find_mut<'a>(nodes: &'a mut [DbNode], id: &SharedString) -> Option<&'a mut DbNode> {
    for node in nodes {
        if node.id == *id {
            return Some(node);
        }
        if let Some(found) = find_mut(&mut node.children, id) {
            return Some(found);
        }
    }
    None
}

/// Placeholder label shown as an unloaded/loading/failed/empty node's single
/// child, so its expand arrow keeps showing. `None` means "render no
/// placeholder" (the node has real children that were merely filtered out).
fn placeholder_label(node: &DbNode) -> SharedString {
    match &node.load {
        Load::Unloaded | Load::Loading => "Loading…".into(),
        Load::Failed(err) => {
            // Keep the row short: first line of the error only.
            let first = err.lines().next().unwrap_or(err);
            format!("⚠ {first}").into()
        }
        // Loaded with no real children: the object set is empty.
        _ => "(empty)".into(),
    }
}

/// Project the model to `TreeItem`s. Expansion is re-applied from `expanded`
/// so it survives the rebuild. A non-empty `filter` keeps only nodes whose
/// label matches (case-insensitive substring) or that have a matching
/// descendant.
pub fn to_tree_items(
    nodes: &[DbNode],
    expanded: &HashSet<SharedString>,
    filter: &str,
) -> Vec<TreeItem> {
    let needle = filter.trim().to_lowercase();
    build(nodes, expanded, &needle)
}

fn build(nodes: &[DbNode], expanded: &HashSet<SharedString>, needle: &str) -> Vec<TreeItem> {
    nodes
        .iter()
        .filter_map(|node| project(node, expanded, needle))
        .collect()
}

fn project(node: &DbNode, expanded: &HashSet<SharedString>, needle: &str) -> Option<TreeItem> {
    let self_match = needle.is_empty() || node.label.to_lowercase().contains(needle);
    let child_items = build(&node.children, expanded, needle);
    // While filtering, drop a node that neither matches nor has a match below.
    if !needle.is_empty() && !self_match && child_items.is_empty() {
        return None;
    }

    let mut item = TreeItem::new(node.id.clone(), node.label.clone());
    if node.is_leaf() {
        return Some(item);
    }

    if child_items.is_empty() {
        if node.children.is_empty() {
            // No real children exist yet (or the object set is empty): a
            // placeholder keeps the expand arrow. Placeholders carry a control
            // char in their id so the panel's renderer greys them out.
            item = item.child(
                TreeItem::new(format!("{}{SEP}·", node.id), placeholder_label(node)).disabled(true),
            );
            item = item.expanded(expanded.contains(&node.id));
        }
        // else: real children exist but were filtered out — render bare.
    } else {
        item = item
            .children(child_items)
            .expanded(expanded.contains(&node.id));
    }
    Some(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(items: &[TreeItem]) -> Vec<String> {
        items.iter().map(|i| i.id.to_string()).collect()
    }

    #[test]
    fn schema_has_six_category_folders() {
        let schema = DbNode::schema("app");
        assert_eq!(schema.kind, NodeKind::Schema);
        assert_eq!(schema.children.len(), 6);
        assert!(matches!(schema.load, Load::Loaded));
        assert!(
            schema
                .children
                .iter()
                .all(|c| matches!(c.load, Load::Unloaded))
        );
    }

    #[test]
    fn table_expands_to_definition_columns_indexes_constraints_and_triggers() {
        let table = DbNode::table("p", "app", "users");
        assert_eq!(table.kind, NodeKind::Table);
        assert!(matches!(table.load, Load::Loaded));
        let kinds: Vec<NodeKind> = table.children.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                NodeKind::TableDefinition,
                NodeKind::ColumnsFolder,
                NodeKind::IndexesFolder,
                NodeKind::ConstraintsFolder,
                NodeKind::TriggersFolder,
            ]
        );
        // Every child carries the table's query context.
        assert!(
            table
                .children
                .iter()
                .all(|c| c.schema.as_ref() == "app" && c.relation.as_ref() == "users")
        );
        // The definition entry is a leaf carrying the table name as its
        // object; the sub-folders are lazily loaded.
        let def = &table.children[0];
        assert!(matches!(def.load, Load::Leaf));
        assert_eq!(def.object.as_ref(), "users");
        assert!(
            table.children[1..]
                .iter()
                .all(|c| matches!(c.load, Load::Unloaded))
        );
    }

    #[test]
    fn unloaded_folder_gets_placeholder_so_it_can_expand() {
        let nodes = vec![DbNode::schema("app")];
        let items = to_tree_items(&nodes, &HashSet::new(), "");
        // The schema's Tables folder is unloaded → one placeholder child.
        let tables = &items[0].children[0];
        assert_eq!(tables.children.len(), 1);
        assert!(tables.children[0].is_disabled());
    }

    #[test]
    fn expansion_state_reapplied_from_set() {
        let nodes = vec![DbNode::schema("app")];
        let schema_id = nodes[0].id.clone();
        let expanded = HashSet::from([schema_id]);
        let items = to_tree_items(&nodes, &expanded, "");
        assert!(items[0].is_expanded());
    }

    #[test]
    fn filter_keeps_matches_and_their_ancestors() {
        let mut schema = DbNode::schema("app");
        // Pretend the Tables folder loaded two tables.
        let tables = &mut schema.children[0];
        tables.load = Load::Loaded;
        tables.children = vec![
            DbNode::leaf("t1".into(), "orders", NodeKind::Table, "app", "", "orders"),
            DbNode::leaf(
                "t2".into(),
                "customers",
                NodeKind::Table,
                "app",
                "",
                "customers",
            ),
        ];
        let items = to_tree_items(&[schema], &HashSet::new(), "order");
        // Only the schema → Tables → orders spine survives.
        assert_eq!(ids(&items).len(), 1);
        let tables_item = &items[0].children[0];
        let labels: Vec<String> = tables_item
            .children
            .iter()
            .map(|c| c.label.to_string())
            .collect();
        assert_eq!(labels, ["orders"]);
    }
    fn column(name: &str, data_type: &str) -> db::ColumnInfo {
        db::ColumnInfo {
            name: name.to_string(),
            data_type: data_type.to_string(),
            not_null: false,
            primary: false,
            default: None,
        }
    }

    #[test]
    fn column_label_marks_primary_key_over_not_null() {
        let mut col = column("id", "integer");
        col.not_null = true;
        col.primary = true;
        assert_eq!(column_label(&col), "id integer · PK");
    }

    #[test]
    fn column_label_marks_not_null_and_default() {
        let mut col = column("placed_at", "timestamptz");
        col.not_null = true;
        col.default = Some("now()".to_string());
        assert_eq!(
            column_label(&col),
            "placed_at timestamptz · not null · default now()"
        );
    }

    #[test]
    fn column_label_is_bare_without_markers() {
        assert_eq!(column_label(&column("note", "text")), "note text");
    }

    #[test]
    fn column_label_elides_a_long_default() {
        let mut col = column("payload", "jsonb");
        col.default = Some("x".repeat(MAX_DEFAULT_LEN + 10));
        let label = column_label(&col);
        assert!(label.ends_with(&format!("{}…", "x".repeat(MAX_DEFAULT_LEN))));
    }
}
