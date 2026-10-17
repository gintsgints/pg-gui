//! End-to-end UI tests: launch the real pg-gui binary and drive it through
//! the macOS window server. See `harness.rs` for what can and cannot be
//! observed from outside a GPUI window.
//!
//! These take over the keyboard focus and need Accessibility permission for
//! the terminal, so the target is only built behind the `e2e` feature and
//! runs on request:
//!
//! ```sh
//! cargo test --features e2e --test e2e
//! ```
//!
//! None of them needs a database.

#![cfg(target_os = "macos")]

mod harness;

use std::path::Path;

use harness::{App, Button, Key, Mod};
use serde_json::Value;

fn tab_count(config: &Value) -> usize {
    config["tabs"].as_array().map_or(0, Vec::len)
}

#[test]
fn launches_with_main_window_and_menus() {
    let app = App::launch();

    assert_eq!(app.window_titles(), ["pg-gui"]);
    let menus = app.menu_bar();
    for menu in ["Connection", "File", "Edit", "Session", "Debug", "View"] {
        assert!(
            menus.iter().any(|m| m == menu),
            "missing {menu:?} menu: {menus:?}"
        );
    }

    let status = app.quit();
    assert!(status.success(), "pg-gui exited with {status}");
}

#[test]
fn new_tab_and_close_tab() {
    let mut app = App::launch();
    assert_eq!(tab_count(&app.config()), 1);

    app.send_keys(&[Mod::Cmd], 't');
    let config = app.wait_for_config("a second tab", |c| tab_count(c) == 2);
    assert_eq!(config["active_tab"], 1, "the new tab becomes active");

    // A blank untitled tab loses nothing, so it closes without a prompt.
    app.send_keys(&[Mod::Cmd], 'w');
    app.wait_for_config("the tab to close", |c| tab_count(c) == 1);

    app.quit();
}

#[test]
fn panel_shortcuts_toggle_panels() {
    let mut app = App::launch();

    for (key, field) in [
        ('1', "db_panel_visible"),
        ('2', "files_panel_visible"),
        ('3', "results_panel_visible"),
    ] {
        let before = app.config()[field].as_bool().unwrap_or(true);
        app.send_keys(&[Mod::Cmd], key);
        app.wait_for_config(&format!("cmd-{key} to flip {field}"), |c| {
            c[field].as_bool() == Some(!before)
        });
        app.send_keys(&[Mod::Cmd], key);
        app.wait_for_config(&format!("cmd-{key} to restore {field}"), |c| {
            c[field].as_bool() == Some(before)
        });
    }

    app.quit();
}

#[test]
fn format_on_save_menu_toggles_check_mark() {
    let mut app = App::launch();
    assert!(app.menu_checked("Edit", "Format on Save"));

    app.click_menu("Edit", "Format on Save");
    app.wait_for_config("format_on_save off", |c| c["format_on_save"] == false);
    assert!(!app.menu_checked("Edit", "Format on Save"));

    app.click_menu("Edit", "Format on Save");
    app.wait_for_config("format_on_save on", |c| c["format_on_save"] == true);
    assert!(app.menu_checked("Edit", "Format on Save"));

    app.quit();
}

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
fn files_panel_new_script_from_header() {
    let mut app = App::launch_with_folder(&["existing.sql"]);
    let created = app.work_dir().join("created_by_e2e.sql");

    // The "+" asks for the name with the platform's save panel, opened on
    // the working folder; its name field starts focused.
    app.click_files_new_script();
    app.wait_for_extra_window("the new-script save panel");
    app.send_keys(&[Mod::Cmd], 'a');
    app.type_text("created_by_e2e.sql");
    app.press(&[], Key::Return);

    app.wait_for("the script on disk", harness::REACT_TIMEOUT, |_| {
        created.is_file()
    });
    // The save panel answers with the path as typed into it, which may be
    // the un-canonical `/var/…` spelling of the temp folder.
    app.wait_for_config("the new script opened in the active tab", |c| {
        active_file(c).is_some_and(|file| same_file(file, &created))
    });

    app.quit();
}

#[test]
fn files_panel_filter_finds_nested_script() {
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
fn files_panel_rename_from_context_menu() {
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
fn files_panel_delete_from_context_menu() {
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
