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
/// handlers.
pub fn to_tree_items(
    nodes: &[FileNode],
    expanded: &HashSet<SharedString>,
    dirs_out: &mut HashSet<SharedString>,
) -> Vec<TreeItem> {
    nodes
        .iter()
        .map(|node| {
            let id: SharedString = node.path.to_string_lossy().into_owned().into();
            if node.is_dir {
                dirs_out.insert(id.clone());
                TreeItem::new(id.clone(), node.name.clone())
                    .children(to_tree_items(&node.children, expanded, dirs_out))
                    .expanded(expanded.contains(&id))
            } else {
                TreeItem::new(id, node.name.clone()).disabled(!is_sql(&node.path))
            }
        })
        .collect()
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
        let items = to_tree_items(&nodes, &expanded, &mut dirs);

        // empty, scripts, notes.txt
        assert!(dirs.contains(&items[0].id), "empty dir recorded in dirs");
        assert!(dirs.contains(&scripts_id));
        assert!(items[1].is_expanded());
        assert!(!items[1].children[0].is_disabled(), "query.sql openable");
        assert!(items[2].is_disabled(), "notes.txt greyed out");
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
