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

use harness::{App, Mod};
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
