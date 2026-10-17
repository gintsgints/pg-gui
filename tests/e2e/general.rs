//! The app as a whole: launch, menus, tabs, panel toggles.

use serde_json::Value;

use crate::harness::{self, App, Mod};

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

/// Brightness of the results panel's empty background, well clear of the
/// side panels and the panel's placeholder icon.
fn background_brightness(app: &App) -> f64 {
    let (_, _, width, height) = app.window_frame();
    app.brightness_at(width * 0.3, height * 0.85)
}

#[test]
fn theme_menu_switches_theme() {
    let mut app = App::launch();
    let checked = |app: &App| -> Vec<String> {
        ["Light", "Dark", "System"]
            .into_iter()
            .filter(|theme| app.submenu_checked("View", "Theme", theme))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(checked(&app), ["System"], "the default follows the OS");

    app.click_submenu("View", "Theme", "Dark");
    app.wait_for_config("theme dark", |c| c["theme"] == "dark");
    assert_eq!(checked(&app), ["Dark"]);
    app.wait_for("a dark window", harness::REACT_TIMEOUT, |app| {
        background_brightness(app) < 0.35
    });

    app.click_submenu("View", "Theme", "Light");
    app.wait_for_config("theme light", |c| c["theme"] == "light");
    assert_eq!(checked(&app), ["Light"]);
    app.wait_for("a light window", harness::REACT_TIMEOUT, |app| {
        background_brightness(app) > 0.65
    });

    app.click_submenu("View", "Theme", "System");
    app.wait_for_config("theme system", |c| c["theme"] == "system");
    assert_eq!(checked(&app), ["System"]);

    app.quit();
}
