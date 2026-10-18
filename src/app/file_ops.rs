//! Script files: new/open, save (with the diverged-on-disk guard), the files
//! panel's create/rename/delete, the working folder, and reloading tabs whose
//! file changed on disk.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, EntityInputHandler as _, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use gpui_component::{
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState, RopeExt as _},
    v_flex,
};

use crate::app::{PgGuiApp, file_mtime, record_recent_folder, set_editor_value};
use crate::{NewFile, OpenFile, OpenFolder, OpenRecentFolder, SaveFile, file_tree};

/// A dialog's confirm action, shared by its primary button and the dialog's
/// own Enter binding; returns whether the dialog may close.
type DialogConfirm = Rc<dyn Fn(&mut Window, &mut App) -> bool>;

impl PgGuiApp {
    /// Notice when an open tab's file changed on disk. A clean tab reloads
    /// silently; a tab with unsaved edits is flagged diverged (its buffer is
    /// left untouched) so the next save can prompt before clobbering.
    /// A missing file is ignored — only "exists with a newer mtime" reacts,
    /// which sidesteps the transient gap during an external temp-file swap.
    pub(in crate::app) fn check_tab_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Start a fresh script in a new tab (cmd-t, or the tab bar's "+");
    /// cmd-s prompts for its location.
    pub(in crate::app) fn new_file(
        &mut self,
        _: &NewFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = self.add_tab(String::new(), None, window, cx);
        self.activate_tab(ix, window, cx);
        self.save_config();
        self.set_status("New script", cx);
    }

    pub(in crate::app) fn open_file(
        &mut self,
        _: &OpenFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    pub(in crate::app) fn open_path(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Ask for a new script's name and create it inside `dir` — the files
    /// panel's "+" buttons and New Script… items. An in-app name dialog,
    /// like New Folder's: the panel only ever creates inside the folder it
    /// was asked on, so the platform's save panel (which offers a location
    /// too) asked for more than the action needs.
    pub(in crate::app) fn new_script_in(dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let dir = dir.to_path_buf();
        Self::prompt_name(
            "New Script",
            String::new(),
            window,
            cx,
            move |this, name, window, cx| this.create_script(&dir, name, window, cx),
        );
    }

    /// Create a confirmed new script `name` inside `dir` and open it in a
    /// tab. The name is forced to `.sql`, since the panel only opens and
    /// runs those, and `dir` is expanded so the file shows at once. An
    /// existing file is never clobbered.
    fn create_script(
        &mut self,
        dir: &Path,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = Path::new(name);
        if !Self::is_plain_name(name) {
            self.set_status("Enter a name, not a path", cx);
            return;
        }
        let mut path = dir.join(name);
        if !file_tree::is_sql(&path) {
            path.set_extension("sql");
        }
        // `create_new` rather than `write`, so an existing file is refused.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match created {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                self.set_status(format!("{} already exists", path.display()), cx);
                return;
            }
            Err(err) => {
                self.set_status(format!("Create failed: {err}"), cx);
                return;
            }
        }
        self.reveal_created(dir, cx);
        self.open_path(&path, window, cx);
    }

    /// Rename the file or folder the files panel's context menu was opened
    /// on. Asked with an in-app dialog rather than the platform's save
    /// panel: a rename is a new *name* in the same folder, which that panel
    /// cannot express (it offers a location too).
    pub(in crate::app) fn prompt_rename_path(
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return;
        };
        Self::prompt_name(
            "Rename",
            current,
            window,
            cx,
            move |this, name, window, cx| {
                this.rename_path(&path, name, window, cx);
            },
        );
    }

    /// Ask for a new folder's name and create it inside `dir` — the files
    /// panel's New Folder… item. An in-app dialog like Rename's, since the
    /// platform's save panel asks for a file.
    pub(in crate::app) fn prompt_new_folder(
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::prompt_name(
            "New Folder",
            String::new(),
            window,
            cx,
            move |this, name, _, cx| this.create_folder(&dir, name, cx),
        );
    }

    /// The files panel's one-field name dialog: `title` names both the
    /// dialog and its primary button, `initial` starts selected so typing
    /// replaces it, and `apply` gets the trimmed, non-empty name.
    fn prompt_name(
        title: &'static str,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, &str, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        // Deferred, so it lands after `open_dialog` below: opening a dialog
        // focuses the dialog's own handle, which would take the focus
        // straight back off the input.
        cx.defer_in(window, {
            let input = input.clone();
            move |_, window, cx| {
                input.update(cx, |state, cx| {
                    state.focus(window, cx);
                    state.select_all(window, cx);
                });
            }
        });
        let app = cx.weak_entity();
        let apply = Rc::new(apply);
        window.open_dialog(cx, move |dialog, _, _| {
            let input = input.clone();
            // Shared by the primary button and the dialog's own Enter
            // binding; reports whether the dialog may close.
            let confirm: DialogConfirm = {
                let (app, input, apply) = (app.clone(), input.clone(), apply.clone());
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    let name = input.read(cx).value().trim().to_string();
                    if name.is_empty() {
                        return false;
                    }
                    app.update(cx, |this, cx| apply(this, &name, window, cx))
                        .ok();
                    true
                })
            };
            let ok = confirm.clone();
            dialog
                .title(title)
                .w(px(420.))
                .on_ok(move |_, window, cx| ok(window, cx))
                .child(
                    v_flex().gap_4().pb_2().child(Input::new(&input)).child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new("cancel").label("Cancel").on_click(
                                |_, window, cx| {
                                    window.close_dialog(cx);
                                },
                            ))
                            .child(Button::new("confirm").primary().label(title).on_click(
                                move |_, window, cx| {
                                    if confirm(window, cx) {
                                        window.close_dialog(cx);
                                    }
                                },
                            )),
                    ),
                )
        });
    }

    /// Whether `name` is a single plain path component — what the name
    /// dialogs accept, since a path would land the entry somewhere the
    /// dialog never offered.
    fn is_plain_name(name: &Path) -> bool {
        let mut parts = name.components();
        matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
    }

    /// Create a confirmed new folder `name` inside `dir`, expanding `dir` so
    /// the folder shows at once. An existing entry is never clobbered.
    pub(in crate::app) fn create_folder(&mut self, dir: &Path, name: &str, cx: &mut Context<Self>) {
        let name = Path::new(name);
        if !Self::is_plain_name(name) {
            self.set_status("Enter a name, not a path", cx);
            return;
        }
        let path = dir.join(name);
        // `create_dir` fails on an existing entry, so nothing is clobbered.
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                self.set_status(format!("{} already exists", path.display()), cx);
                return;
            }
            Err(err) => {
                self.set_status(format!("Create failed: {err}"), cx);
                return;
            }
        }
        self.reveal_created(dir, cx);
        self.set_status(format!("Created {}", path.display()), cx);
    }

    /// Show an entry just created inside `dir`: expand `dir` and re-scan now
    /// rather than on the panel's timer.
    fn reveal_created(&mut self, dir: &Path, cx: &mut Context<Self>) {
        // The tree's row ids are the paths themselves; the working folder
        // has no row and is always open.
        self.expanded_dirs
            .insert(SharedString::from(dir.to_string_lossy().into_owned()));
        self.tree_signature = 0;
        self.load_tree(cx);
    }

    /// Apply a confirmed rename. The name is taken as a plain file name in
    /// the same folder — a path would move the entry, which the dialog does
    /// not offer — and a file is forced to `.sql`, like
    /// [`Self::new_script_in`], since those are the only ones the panel
    /// opens and runs. An existing entry is never clobbered.
    pub(in crate::app) fn rename_path(
        &mut self,
        from: &Path,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = Path::new(name);
        if !Self::is_plain_name(name) {
            self.set_status("Enter a name, not a path", cx);
            return;
        }
        let Some(parent) = from.parent() else {
            return;
        };
        let mut to = parent.join(name);
        if from.is_file() && !file_tree::is_sql(&to) {
            to.set_extension("sql");
        }
        if to == from {
            return;
        }
        if to.exists() {
            self.set_status(format!("{} already exists", to.display()), cx);
            return;
        }
        if let Err(err) = std::fs::rename(from, &to) {
            self.set_status(format!("Rename failed: {err}"), cx);
            return;
        }
        // Tabs on the renamed file — or on anything under a renamed folder —
        // follow it, so saving still writes the file the panel now shows.
        for ix in 0..self.tabs.len() {
            let Some(open) = self.tabs[ix].path.clone() else {
                continue;
            };
            if let Ok(rest) = open.strip_prefix(from) {
                self.set_tab_path(ix, to.join(rest), window);
            }
        }
        // The panel re-scans on a timer; show the new name now.
        self.tree_signature = 0;
        self.load_tree(cx);
        self.save_config();
        self.set_status(format!("Renamed to {}", to.display()), cx);
    }

    /// Confirm before deleting: there is no undo and nothing goes to a
    /// trash can, so the dialog names what goes.
    pub(in crate::app) fn prompt_delete_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let what = if path.is_dir() { "folder" } else { "file" };
        let label = self.script_label(&path);
        let app = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (app, path) = (app.clone(), path.clone());
            let delete: DialogConfirm = {
                let (app, path) = (app.clone(), path.clone());
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    let path = path.clone();
                    app.update(cx, |this, cx| this.delete_path(&path, window, cx))
                        .ok();
                    true
                })
            };
            let ok = delete.clone();
            dialog
                .title(format!("Delete {what}"))
                .w(px(420.))
                // Enter deletes, Escape cancels (the dialog's own binding).
                .on_ok(move |_, window, cx| ok(window, cx))
                .child(
                    v_flex()
                        .gap_4()
                        .pb_2()
                        .child(div().text_sm().child(format!(
                            "Delete {what} \u{201c}{label}\u{201d} permanently?"
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
                                .child(Button::new("delete").danger().label("Delete").on_click(
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                        delete(window, cx);
                                    },
                                )),
                        ),
                )
        });
    }

    /// Delete a confirmed file, or an *empty* folder — a recursive delete
    /// from a one-click menu would take a whole tree with no undo, so a
    /// folder with anything in it is refused rather than emptied.
    pub(in crate::app) fn delete_path(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_dir = path.is_dir();
        if is_dir
            && std::fs::read_dir(path)
                .ok()
                .and_then(|mut dir| dir.next())
                .is_some()
        {
            self.set_status(format!("{} is not empty", path.display()), cx);
            return;
        }
        let removed = if is_dir {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        };
        if let Err(err) = removed {
            self.set_status(format!("Delete failed: {err}"), cx);
            return;
        }
        // A tab on a deleted file keeps its text but loses its file: saving
        // it again would silently re-create what was just deleted, so it
        // goes back to being untitled. Nothing is lost — an untitled
        // buffer is persisted in config.json.
        for ix in 0..self.tabs.len() {
            let Some(open) = self.tabs[ix].path.clone() else {
                continue;
            };
            if open.strip_prefix(path).is_err() {
                continue;
            }
            self.config.tabs[ix].file = None;
            self.tabs[ix].path = None;
            self.tabs[ix].disk_time = None;
            self.tabs[ix].diverged = false;
            self.refresh_dirty(ix, cx);
        }
        self.update_window_title(window);
        self.tree_signature = 0;
        self.load_tree(cx);
        self.save_config();
        self.set_status(format!("Deleted {}", path.display()), cx);
    }

    /// Pick the working directory shown in the files side panel.
    pub(in crate::app) fn open_folder(
        &mut self,
        _: &OpenFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    pub(in crate::app) fn open_recent_folder(
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

    pub(in crate::app) fn save_file(
        &mut self,
        _: &SaveFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_active(false, window, cx);
    }

    /// Save the active tab, optionally closing it once the write succeeds
    /// (`close_after` is set by the save-before-close prompt). When the file
    /// changed on disk since our last read or write, prompt before clobbering
    /// it; otherwise write straight through. The check re-stats the file here
    /// rather than trusting the 1s watcher, so a save that races an external
    /// edit still catches it.
    pub(in crate::app) fn save_active(
        &mut self,
        close_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                    this.set_status(format!("Format on save failed: {err:#}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Swap the formatted text into the editor as a single undoable edit,
    /// keeping the cursor near where it was. Goes through the input
    /// handler so the usual change plumbing (LSP sync, config save) runs.
    pub(in crate::app) fn apply_formatted(
        &mut self,
        formatted: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
}
