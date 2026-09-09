//! Directory scanning for the files side panel.
//!
//! The scan runs on the background executor, so its output must be `Send`.
//! gpui-component's `TreeItem` is not (`Rc<RefCell<..>>` inside), hence the
//! intermediate [`FileNode`]: the walk produces nodes off-thread and the UI
//! thread converts them to `TreeItem`s with [`to_tree_items`].

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};

use gpui::SharedString;
use gpui_component::tree::TreeItem;

/// How deep [`scan_dir`] descends below the working directory.
pub const MAX_DEPTH: usize = 8;
/// Total entries [`scan_dir`] collects before it stops descending, so a
/// huge working directory costs bounded memory and time.
pub const ENTRY_BUDGET: usize = 20_000;

/// One scanned file or directory; `Send`, unlike `TreeItem`.
pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub children: Vec<FileNode>,
}

/// Recursively list `dir`, skipping dot entries (which covers `.git`),
/// folders first then names case-insensitively. Symlinks are never
/// followed into (a symlinked directory shows as a plain leaf), so cycles
/// can't recurse. `budget` caps the total entry count across the walk.
pub fn scan_dir(dir: &Path, depth: usize, budget: &mut usize) -> Vec<FileNode> {
    let mut nodes = Vec::new();
    if depth == 0 {
        return nodes;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return nodes;
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // `DirEntry::file_type` doesn't traverse symlinks, so a symlinked
        // directory reports `is_dir() == false` and stays a leaf.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        *budget -= 1;
        let path = entry.path();
        let is_dir = file_type.is_dir();
        let children = if is_dir {
            scan_dir(&path, depth - 1, budget)
        } else {
            Vec::new()
        };
        nodes.push(FileNode {
            path,
            name,
            is_dir,
            children,
        });
    }
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    nodes
}

/// A hash over the scanned paths, used to skip rebuilding the tree when a
/// re-scan found nothing changed (rebuilds reset the tree's selection).
pub fn signature(nodes: &[FileNode]) -> u64 {
    fn walk(nodes: &[FileNode], hasher: &mut DefaultHasher) {
        for node in nodes {
            node.path.hash(hasher);
            node.is_dir.hash(hasher);
            walk(&node.children, hasher);
        }
    }
    let mut hasher = DefaultHasher::new();
    walk(nodes, &mut hasher);
    hasher.finish()
}

/// Convert scanned nodes to tree items on the UI thread. Item ids are the
/// absolute paths (unique by construction). Directories re-apply their
/// previous `expanded` state and are recorded in `dirs_out` — the tree's
/// own `is_folder()` is children-based, so an empty directory would
/// otherwise be indistinguishable from a file when picking icons.
/// Non-SQL files are disabled, which greys them and drops their mouse
/// handlers. A non-empty `filter` keeps only entries whose name matches it
/// (case-insensitive substring) plus the directories above them; a matching
/// directory keeps its whole subtree, and every directory left standing is
/// expanded so the matches show without clicking.
pub fn to_tree_items(
    nodes: &[FileNode],
    expanded: &HashSet<SharedString>,
    filter: &str,
    dirs_out: &mut HashSet<SharedString>,
) -> Vec<TreeItem> {
    let needle = filter.trim().to_lowercase();
    build(nodes, expanded, &needle, dirs_out)
}

fn build(
    nodes: &[FileNode],
    expanded: &HashSet<SharedString>,
    needle: &str,
    dirs_out: &mut HashSet<SharedString>,
) -> Vec<TreeItem> {
    nodes
        .iter()
        .filter_map(|node| project(node, expanded, needle, dirs_out))
        .collect()
}

fn project(
    node: &FileNode,
    expanded: &HashSet<SharedString>,
    needle: &str,
    dirs_out: &mut HashSet<SharedString>,
) -> Option<TreeItem> {
    let id: SharedString = node.path.to_string_lossy().into_owned().into();
    let self_match = needle.is_empty() || node.name.to_lowercase().contains(needle);
    if !node.is_dir {
        return self_match
            .then(|| TreeItem::new(id, node.name.clone()).disabled(!is_sql(&node.path)));
    }
    // A directory whose own name matches keeps its whole subtree; otherwise
    // only matching entries below it survive, and it is dropped when none do.
    let child_needle = if self_match { "" } else { needle };
    let children = build(&node.children, expanded, child_needle, dirs_out);
    if !self_match && children.is_empty() {
        return None;
    }
    dirs_out.insert(id.clone());
    // While filtering, reveal the matches instead of the user's own
    // expansion state (which would hide them behind collapsed folders).
    let is_expanded = if needle.is_empty() {
        expanded.contains(&id)
    } else {
        !children.is_empty()
    };
    Some(
        TreeItem::new(id, node.name.clone())
            .children(children)
            .expanded(is_expanded),
    )
}

/// The ids of `items` in the order the tree draws them: pre-order, and a
/// folder's children only while it is expanded. `TreeItem` keeps its
/// expanded flag behind a shared `Rc`, so this reads the *live* state of
/// the items handed to `TreeState::set_items` and stays index-aligned with
/// the row index the tree passes its renderer — which is what lets a
/// shift-click resolve the rows between two clicks without the tree
/// exposing its own entry list.
pub fn visible_ids(items: &[TreeItem]) -> Vec<SharedString> {
    fn walk(items: &[TreeItem], out: &mut Vec<SharedString>) {
        for item in items {
            out.push(item.id.clone());
            if item.is_expanded() {
                walk(&item.children, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(items, &mut out);
    out
}

/// Every id in `items`, pre-order, whether or not its folder is expanded.
/// Unlike [`visible_ids`] this is not index-aligned with the drawn rows; it
/// exists to order a set of picked scripts the way the tree lists them,
/// including any that sit inside a folder the user has since collapsed.
pub fn ordered_ids(items: &[TreeItem]) -> Vec<SharedString> {
    fn walk(items: &[TreeItem], out: &mut Vec<SharedString>) {
        for item in items {
            out.push(item.id.clone());
            walk(&item.children, out);
        }
    }
    let mut out = Vec::new();
    walk(items, &mut out);
    out
}

/// Every path the last scan found, so selections whose file has since been
/// deleted or renamed can be dropped.
pub fn scanned_ids(nodes: &[FileNode], out: &mut HashSet<SharedString>) {
    for node in nodes {
        out.insert(node.path.to_string_lossy().into_owned().into());
        scanned_ids(&node.children, out);
    }
}

/// Whether the panel lets this file be opened: `.sql`, any casing.
pub fn is_sql(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp directory seeded with the given `(relative path,
    /// is_dir)` entries; removed on drop.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(name: &str, entries: &[(&str, bool)]) -> Self {
            let root = std::env::temp_dir().join(format!("pg-gui-file-tree-{name}"));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            for (path, is_dir) in entries {
                let path = root.join(path);
                if *is_dir {
                    std::fs::create_dir_all(&path).unwrap();
                } else {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, "select 1;").unwrap();
                }
            }
            Self(root)
        }

        fn scan(&self) -> Vec<FileNode> {
            let mut budget = ENTRY_BUDGET;
            scan_dir(&self.0, MAX_DEPTH, &mut budget)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_hides_dotfiles_and_sorts_folders_first() {
        let tree = TempTree::new(
            "sort",
            &[
                (".git/HEAD", false),
                (".hidden.sql", false),
                ("zeta.sql", false),
                ("Alpha.sql", false),
                ("sub/inner.sql", false),
                ("empty", true),
            ],
        );
        let nodes = tree.scan();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["empty", "sub", "Alpha.sql", "zeta.sql"]);
        assert!(nodes[1].is_dir);
        assert_eq!(nodes[1].children[0].name, "inner.sql");
    }

    #[test]
    fn scan_respects_depth_cap() {
        let tree = TempTree::new("depth", &[("a/b/c/deep.sql", false)]);
        let mut budget = ENTRY_BUDGET;
        let nodes = scan_dir(&tree.0, 2, &mut budget);
        // Depth 2: `a` and `a/b` are listed, but `b` isn't descended into.
        let a = &nodes[0];
        assert_eq!(a.name, "a");
        assert_eq!(a.children[0].name, "b");
        assert!(a.children[0].children.is_empty());
    }

    #[test]
    fn scan_respects_entry_budget() {
        let tree = TempTree::new(
            "budget",
            &[("one.sql", false), ("two.sql", false), ("three.sql", false)],
        );
        let mut budget = 2;
        let nodes = scan_dir(&tree.0, MAX_DEPTH, &mut budget);
        assert_eq!(nodes.len(), 2);
        assert_eq!(budget, 0);
    }

    #[test]
    fn is_sql_matches_extension_case_insensitively() {
        assert!(is_sql(Path::new("/x/query.sql")));
        assert!(is_sql(Path::new("/x/QUERY.SQL")));
        assert!(!is_sql(Path::new("/x/readme.md")));
        assert!(!is_sql(Path::new("/x/sql")));
    }

    #[test]
    fn tree_items_disable_non_sql_and_apply_expanded() {
        let tree = TempTree::new(
            "items",
            &[
                ("scripts/query.sql", false),
                ("notes.txt", false),
                ("empty", true),
            ],
        );
        let nodes = tree.scan();
        let scripts_id: SharedString = tree.0.join("scripts").to_string_lossy().into_owned().into();
        let expanded = HashSet::from([scripts_id.clone()]);
        let mut dirs = HashSet::new();
        let items = to_tree_items(&nodes, &expanded, "", &mut dirs);

        // empty, scripts, notes.txt
        assert!(dirs.contains(&items[0].id), "empty dir recorded in dirs");
        assert!(dirs.contains(&scripts_id));
        assert!(items[1].is_expanded());
        assert!(!items[1].children[0].is_disabled(), "query.sql openable");
        assert!(items[2].is_disabled(), "notes.txt greyed out");
    }

    #[test]
    fn filter_keeps_matches_and_their_folders() {
        let tree = TempTree::new(
            "filter",
            &[
                ("reports/monthly.sql", false),
                ("reports/notes.txt", false),
                ("scripts/query.sql", false),
                ("top.sql", false),
            ],
        );
        let nodes = tree.scan();
        let expanded = HashSet::new();
        let mut dirs = HashSet::new();
        let items = to_tree_items(&nodes, &expanded, "MONTH", &mut dirs);

        // Only `reports` survives, expanded, with its single match inside.
        let labels: Vec<String> = items.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(labels, ["reports"]);
        assert!(items[0].is_expanded(), "match revealed without clicking");
        let children: Vec<String> = items[0]
            .children
            .iter()
            .map(|i| i.label.to_string())
            .collect();
        assert_eq!(children, ["monthly.sql"]);

        // A matching directory keeps its whole subtree.
        let mut dirs = HashSet::new();
        let items = to_tree_items(&nodes, &expanded, "reports", &mut dirs);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].children.len(), 2);

        // A match at the root level needs no folder around it.
        let mut dirs = HashSet::new();
        let items = to_tree_items(&nodes, &expanded, "top", &mut dirs);
        let labels: Vec<String> = items.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(labels, ["top.sql"]);
    }

    #[test]
    fn visible_ids_follow_expansion_and_ordered_ids_do_not() {
        let tree = TempTree::new(
            "visible",
            &[
                ("scripts/one.sql", false),
                ("scripts/two.sql", false),
                ("top.sql", false),
            ],
        );
        let nodes = tree.scan();
        let scripts_id: SharedString = tree.0.join("scripts").to_string_lossy().into_owned().into();
        let mut dirs = HashSet::new();
        let items = to_tree_items(&nodes, &HashSet::new(), "", &mut dirs);

        // Collapsed: the folder's children are not drawn…
        let names = |ids: Vec<SharedString>| -> Vec<String> {
            ids.iter()
                .map(|id| {
                    Path::new(id.as_ref())
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        };
        assert_eq!(names(visible_ids(&items)), ["scripts", "top.sql"]);
        // …but they still order as the tree lists them.
        assert_eq!(
            names(ordered_ids(&items)),
            ["scripts", "one.sql", "two.sql", "top.sql"]
        );

        // Expanding is what a click does: it mutates the shared state of
        // the very items handed to the widget, so `visible_ids` sees it
        // without a rebuild.
        let expanded = to_tree_items(&nodes, &HashSet::from([scripts_id]), "", &mut dirs);
        assert_eq!(
            names(visible_ids(&expanded)),
            ["scripts", "one.sql", "two.sql", "top.sql"]
        );
    }

    #[test]
    fn scanned_ids_covers_every_entry() {
        let tree = TempTree::new(
            "scanned",
            &[("scripts/one.sql", false), ("notes.txt", false)],
        );
        let mut ids = HashSet::new();
        scanned_ids(&tree.scan(), &mut ids);
        let id =
            |rel: &str| -> SharedString { tree.0.join(rel).to_string_lossy().into_owned().into() };
        assert!(ids.contains(&id("scripts")));
        assert!(ids.contains(&id("scripts/one.sql")));
        assert!(ids.contains(&id("notes.txt")));
        assert!(!ids.contains(&id("gone.sql")));
    }

    #[test]
    fn signature_stable_until_contents_change() {
        let tree = TempTree::new("signature", &[("a.sql", false)]);
        let first = signature(&tree.scan());
        assert_eq!(first, signature(&tree.scan()));
        std::fs::write(tree.0.join("b.sql"), "select 2;").unwrap();
        assert_ne!(first, signature(&tree.scan()));
    }
}
