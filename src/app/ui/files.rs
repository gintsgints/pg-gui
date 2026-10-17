//! The files panel: the working folder's script tree, its header and the
//! batch-run bar.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    list::ListItem,
    menu::{PopupMenu, PopupMenuItem},
    tree::tree,
    v_flex,
};

use crate::app::PgGuiApp;
use crate::{OpenFolder, RunScripts};

impl PgGuiApp {
    /// The files panel before a working folder has been picked.
    pub(in crate::app) fn render_no_folder(cx: &mut Context<Self>) -> gpui::Div {
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
    pub(in crate::app) fn render_folder_row(
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
                .items_center()
                .child(Self::clip_label(label_row.flex_1()))
                .child(Self::tree_row_action(
                    SharedString::from(format!("new-script-{id}")),
                    "file-row",
                    "+",
                    "New script here",
                    cx,
                    cx.listener(move |_, _, window, cx| {
                        Self::new_script_in(&dir, window, cx);
                    }),
                )),
        )
    }

    /// The files panel's right-click menu, built per row. `batch` is set
    /// when the row is part of a multi-row selection, in which case Run
    /// takes the whole selection rather than just this file — right-clicking
    /// inside a selection to run one of its files would be a surprise.
    ///
    /// The tree builds this with a [`Context<TreeState>`], so the app is
    /// reached through its own entity rather than through `cx`.
    pub(in crate::app) fn file_row_menu(
        view: &Entity<Self>,
        id: &SharedString,
        is_dir: bool,
        batch: Option<usize>,
        menu: PopupMenu,
    ) -> PopupMenu {
        let path = PathBuf::from(id.to_string());
        // "New Script" lands inside a folder row, and beside a file row.
        let dir = if is_dir {
            path.clone()
        } else {
            path.parent()
                .map_or_else(|| path.clone(), Path::to_path_buf)
        };
        let menu = if is_dir {
            menu
        } else {
            let (open, run) = (view.clone(), view.clone());
            let (open_path, run_path) = (path.clone(), path.clone());
            let run_label = match batch {
                Some(count) => format!("Run {count} Selected Scripts"),
                None => "Run Script".to_string(),
            };
            menu.item(PopupMenuItem::new("Open").on_click(move |_, window, cx| {
                let path = open_path.clone();
                open.update(cx, |this, cx| this.open_path(&path, window, cx));
            }))
            .item(
                PopupMenuItem::new(run_label).on_click(move |_, window, cx| {
                    let path = run_path.clone();
                    run.update(cx, |this, cx| {
                        if batch.is_some() {
                            this.run_scripts(&RunScripts, window, cx);
                        } else {
                            this.run_one_script(path, window, cx);
                        }
                    });
                }),
            )
            .separator()
        };
        let (new, rename, delete) = (view.clone(), view.clone(), view.clone());
        let (rename_path, delete_path) = (path.clone(), path);
        menu.item(
            PopupMenuItem::new("New Script\u{2026}").on_click(move |_, window, cx| {
                let dir = dir.clone();
                new.update(cx, |_, cx| Self::new_script_in(&dir, window, cx));
            }),
        )
        .item(
            PopupMenuItem::new("Rename\u{2026}").on_click(move |_, window, cx| {
                let path = rename_path.clone();
                rename.update(cx, |_, cx| Self::prompt_rename_path(path, window, cx));
            }),
        )
        .item(
            PopupMenuItem::new("Delete\u{2026}").on_click(move |_, window, cx| {
                let path = delete_path.clone();
                delete.update(cx, |this, cx| this.prompt_delete_path(path, window, cx));
            }),
        )
    }

    /// The files panel's title row: the working folder's name, and the "+"
    /// that creates a script directly inside it — the root has no row of
    /// its own in the tree, where every other folder carries its own.
    pub(in crate::app) fn render_files_header(
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
    pub(in crate::app) fn render_script_run_bar(
        picked: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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

    pub(in crate::app) fn render_files_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some(root) = self.config.working_dir.clone() else {
            return Self::render_no_folder(cx);
        };

        let folder_name = root.file_name().map_or_else(
            || root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let picked = self.selected_scripts.len();
        let files_tree = self.render_files_tree(cx);
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
            .child(div().flex_1().min_h(px(0.)).px_1().child(files_tree))
    }

    /// The files panel's tree widget: the row renderer plus the per-row
    /// right-click menu. Split out of [`Self::render_files_panel`] because
    /// both closures capture, and both are long.
    pub(in crate::app) fn render_files_tree(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let dirs = self.tree_dirs.clone();
        let selected: Rc<HashSet<SharedString>> = Rc::new(self.selected_scripts.clone());
        let view = cx.entity();
        let (menu_dirs, menu_selected, menu_view) = (dirs.clone(), selected.clone(), view.clone());
        tree(
            &self.tree_state,
            move |ix, entry, _selected, _window, cx| {
                view.update(cx, |_, cx| {
                    let item = entry.item();
                    // The tree's own `is_folder()` is children-based
                    // and misses empty directories.
                    let is_dir = entry.is_folder() || dirs.contains(&item.id);
                    // A text glyph, like the object browser's rows.
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
                                // See the database tree: muted is too dim
                                // for a single arrow glyph on dark.
                                .text_color(cx.theme().sidebar_foreground)
                                .child(glyph),
                        )
                        .child(item.label.clone());
                    let list_item = ListItem::new(ix)
                        .w_full()
                        .h(Self::tree_row_height(cx))
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
                        list_item
                            .child(Self::clip_label(label_row.w_full()))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
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
                            }))
                    }
                })
            },
        )
        .context_menu(move |_, entry, menu, _window, _cx| {
            let id = entry.item().id.clone();
            // The tree's own `is_folder()` is children-based and misses
            // empty directories, as in the row renderer above.
            let is_dir = entry.is_folder() || menu_dirs.contains(&id);
            let batch = (menu_selected.len() > 1 && menu_selected.contains(&id))
                .then(|| menu_selected.len());
            Self::file_row_menu(&menu_view, &id, is_dir, batch, menu)
        })
    }
}
