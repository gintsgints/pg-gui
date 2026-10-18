//! The files panel: creating, finding, renaming and deleting scripts and
//! folders in the working folder.

use std::path::Path;

use serde_json::Value;

use crate::harness::{self, App, Button, Key};

/// The file the active tab is backed by, if any.
fn active_file(config: &Value) -> Option<&str> {
    let active = usize::try_from(config["active_tab"].as_u64()?).ok()?;
    config["tabs"][active]["file"].as_str()
}

/// Whether `file` names `path`, whichever spelling of it either uses.
fn same_file(file: &str, path: &Path) -> bool {
    Path::new(file).canonicalize().ok() == path.canonicalize().ok()
}

#[test]
fn new_script_from_header() {
    let mut app = App::launch_with_folder(&["existing.sql"]);
    let created = app.work_dir().join("created_by_e2e.sql");

    // The "+" asks for the name in an in-app dialog, empty and focused; the
    // `.sql` is forced on.
    app.click_files_new_script();
    app.type_text("created_by_e2e");
    app.press(&[], Key::Return);

    app.wait_for("the script on disk", harness::REACT_TIMEOUT, |_| {
        created.is_file()
    });
    app.wait_for_config("the new script opened in the active tab", |c| {
        active_file(c).is_some_and(|file| same_file(file, &created))
    });

    app.quit();
}

#[test]
fn filter_finds_nested_script() {
    let mut app = App::launch_with_folder(&[
        "aaa_first.sql",
        "reports/monthly/needle_report.sql",
        "reports/monthly/other.sql",
        "reports/yearly.sql",
        "zzz_last.sql",
    ]);
    let needle = app.work_dir().join("reports/monthly/needle_report.sql");

    // Unfiltered, the folders are collapsed and the needle is not drawn.
    // Filtered, only it and the folders above it remain, expanded:
    // reports, monthly, needle_report.sql.
    app.filter_files("NEEDLE");
    app.click_files_row(Button::Left, 2);

    app.wait_for_config("the filtered script opened", |c| {
        active_file(c) == needle.to_str()
    });

    app.quit();
}

#[test]
fn rename_from_context_menu() {
    let mut app = App::launch_with_folder(&["keep.sql", "old_name.sql", "other.sql"]);
    let work = app.work_dir();
    let (old, new) = (work.join("old_name.sql"), work.join("new_name.sql"));

    // Unfiltered, keep.sql would be the top row.
    app.filter_files("old_name");
    // Right-click the one row left; the menu ends Rename…, Delete…, and
    // Up from no selection wraps to the last item.
    app.click_files_row(Button::Right, 0);
    app.press(&[], Key::Up);
    app.press(&[], Key::Up);
    app.press(&[], Key::Return);
    // The dialog starts with the current name selected, so typing replaces
    // it; the `.sql` is forced back on.
    app.type_text("new_name");
    app.press(&[], Key::Return);

    app.wait_for("the rename on disk", harness::REACT_TIMEOUT, |_| {
        new.is_file() && !old.exists()
    });
    assert!(work.join("keep.sql").is_file(), "neighbours untouched");
    assert!(work.join("other.sql").is_file(), "neighbours untouched");

    app.quit();
}

#[test]
fn delete_from_context_menu() {
    let mut app = App::launch_with_folder(&["a_keep.sql", "doomed.sql"]);
    let work = app.work_dir();
    let doomed = work.join("doomed.sql");

    app.filter_files("doomed");
    app.click_files_row(Button::Right, 0);
    // Delete… is the menu's last item; Enter confirms the dialog.
    app.press(&[], Key::Up);
    app.press(&[], Key::Return);
    app.press(&[], Key::Return);

    app.wait_for("the file deleted", harness::REACT_TIMEOUT, |_| {
        !doomed.exists()
    });
    // Unfiltered, a_keep.sql would be the top row.
    assert!(
        work.join("a_keep.sql").is_file(),
        "only the filtered row goes"
    );

    app.quit();
}

#[test]
fn new_folder_from_empty_space() {
    let mut app = App::launch_with_folder(&["only.sql"]);
    let created = app.work_dir().join("made_by_e2e");

    // Well below the one row: the empty space's menu is New Script…,
    // New Folder…, and Up from no selection wraps to the last item.
    app.click_files_row(Button::Right, 6);
    app.press(&[], Key::Up);
    app.press(&[], Key::Return);
    // The name dialog starts empty and focused.
    app.type_text("made_by_e2e");
    app.press(&[], Key::Return);

    app.wait_for("the folder on disk", harness::REACT_TIMEOUT, |_| {
        created.is_dir()
    });

    app.quit();
}

#[test]
fn new_folder_from_folder_row_menu() {
    let mut app = App::launch_with_folder(&["reports/", "z_last.sql"]);
    let created = app.work_dir().join("reports/nested");

    // A folder's menu is New Script…, New Folder…, Rename…, Delete…; three
    // Ups from no selection land on New Folder…, which creates inside the
    // clicked folder rather than beside it.
    app.click_files_row(Button::Right, 0);
    for _ in 0..3 {
        app.press(&[], Key::Up);
    }
    app.press(&[], Key::Return);
    app.type_text("nested");
    app.press(&[], Key::Return);

    app.wait_for("the folder on disk", harness::REACT_TIMEOUT, |_| {
        created.is_dir()
    });
    assert!(
        !app.work_dir().join("nested").exists(),
        "created inside the folder, not beside it"
    );

    app.quit();
}

#[test]
fn new_script_from_folder_row_menu() {
    let mut app = App::launch_with_folder(&["reports/", "z_last.sql"]);
    let created = app.work_dir().join("reports/monthly.sql");

    // A folder's menu is New Script…, New Folder…, Rename…, Delete…; four
    // Ups from no selection land on New Script…, which creates inside the
    // clicked folder.
    app.click_files_row(Button::Right, 0);
    for _ in 0..4 {
        app.press(&[], Key::Up);
    }
    app.press(&[], Key::Return);
    app.type_text("monthly");
    app.press(&[], Key::Return);

    app.wait_for("the script on disk", harness::REACT_TIMEOUT, |_| {
        created.is_file()
    });
    app.wait_for_config("the new script opened in the active tab", |c| {
        active_file(c).is_some_and(|file| same_file(file, &created))
    });

    app.quit();
}
