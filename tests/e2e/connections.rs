//! Connections: creating one through the New Connection dialog and switching
//! between saved ones from the Connection ▸ Recent menu.
//!
//! Untitled tabs belong to the connection, not to the window: switching away
//! parks them (in config.json's `parked_tabs`, under the outgoing
//! connection's name) and switching back brings them home, while a tab
//! backed by a file stays open throughout. No database needed — both
//! connections point at a port nothing listens on, which is all a switch
//! needs.

use serde_json::Value;

use crate::harness::{App, Key, Mod};

const FIRST_URL: &str = "postgres://e2e@127.0.0.1:1/first";
const SECOND_URL: &str = "postgres://e2e@127.0.0.1:1/second";
const FIRST_SCRIPT: &str = "select 1 as on_first;";

/// Every tab's text, in tab-bar order.
fn scripts(config: &Value) -> Vec<&str> {
    config["tabs"]
        .as_array()
        .map(|tabs| tabs.iter().filter_map(|t| t["script"].as_str()).collect())
        .unwrap_or_default()
}

/// The scripts parked under the connection named `name`, in slot order.
fn parked(config: &Value, name: &str) -> Vec<String> {
    config["parked_tabs"][name]
        .as_array()
        .map(|tabs| {
            tabs.iter()
                .filter_map(|t| t["script"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Names of the saved connections, most recent first.
fn recent_names(config: &Value) -> Vec<&str> {
    config["recent_connections"]
        .as_array()
        .map(|c| c.iter().filter_map(|c| c["name"].as_str()).collect())
        .unwrap_or_default()
}

#[test]
fn recent_menu_switches_untitled_tabs() {
    let mut kept = String::new();
    let mut app = App::launch_prepared(|dir, seed| {
        let file = dir.join("kept.sql");
        std::fs::write(&file, "select 'kept';\n").expect("write seeded script");
        kept = file
            .canonicalize()
            .expect("canonicalize seeded script")
            .to_str()
            .expect("utf-8 temp path")
            .to_string();
        seed["connection_string"] = Value::from(FIRST_URL);
        seed["recent_connections"] = serde_json::json!([
            { "name": "first", "url": FIRST_URL },
            { "name": "second", "url": SECOND_URL },
        ]);
        seed["tabs"] = serde_json::json!([
            { "script": FIRST_SCRIPT, "file": null },
            { "script": "select 'kept';\n", "file": kept },
        ]);
        seed["active_tab"] = Value::from(0);
    });

    // Away from first: its untitled tab is parked under its name, the file
    // tab stays.
    app.click_submenu("Connection", "Recent", "second");
    app.wait_for_config("the untitled tab parked under first", |c| {
        c["connection_string"] == SECOND_URL
            && scripts(c) == ["select 'kept';\n"]
            && parked(c, "first") == [FIRST_SCRIPT]
    });

    // An untitled tab of second's own.
    app.send_keys(&[Mod::Cmd], 't');
    app.wait_for_config("a new tab on second", |c| scripts(c).len() == 2);
    app.type_text("select 2 as on_second;");
    app.press(&[], Key::Escape);
    app.wait_for_config("the typed text on second", |c| {
        scripts(c) == ["select 'kept';\n", "select 2 as on_second;"]
    });

    // Back to first: its tab returns to the slot it left, second's is parked.
    app.click_submenu("Connection", "Recent", "first");
    let config = app.wait_for_config("first's tab restored", |c| {
        c["connection_string"] == FIRST_URL
            && scripts(c) == [FIRST_SCRIPT, "select 'kept';\n"]
            && parked(c, "second") == ["select 2 as on_second;"]
    });
    assert!(
        config["parked_tabs"].get("first").is_none(),
        "the active connection holds no park: {}",
        config["parked_tabs"]
    );

    // And second's comes back in turn.
    app.click_submenu("Connection", "Recent", "second");
    app.wait_for_config("second's tab restored", |c| {
        c["connection_string"] == SECOND_URL
            && scripts(c) == ["select 'kept';\n", "select 2 as on_second;"]
            && parked(c, "first") == [FIRST_SCRIPT]
    });

    app.quit();
}

#[test]
fn new_connection_dialog_creates_and_switches() {
    let mut app = App::launch_prepared(|_, seed| {
        seed["connection_string"] = Value::from(FIRST_URL);
        seed["recent_connections"] = serde_json::json!([{ "name": "first", "url": FIRST_URL }]);
        seed["tabs"] = serde_json::json!([{ "script": FIRST_SCRIPT, "file": null }]);
    });

    app.click_menu("Connection", "New Connection…");
    app.fill_connection_dialog("second", SECOND_URL);

    // The new connection is saved, selected, and starts on a blank tab;
    // first's script waits in the park.
    let config = app.wait_for_config("the new connection selected", |c| {
        c["connection_string"] == SECOND_URL
            && recent_names(c) == ["second", "first"]
            && parked(c, "first") == [FIRST_SCRIPT]
    });
    assert_eq!(scripts(&config), [""], "a blank tab on the new connection");

    // Switching back restores first's script.
    app.click_submenu("Connection", "Recent", "first");
    app.wait_for_config("first's script restored", |c| {
        c["connection_string"] == FIRST_URL
            && scripts(c) == [FIRST_SCRIPT]
            && c["parked_tabs"].get("first").is_none()
    });

    app.quit();
}

/// The schema travels in the URL's `options`: a connection string typed in
/// the `--search_path` form is parsed into the dialog's Schema field and
/// saved back from the fields in the `-c search_path=` form, next to the
/// query parameters the dialog has no field for.
#[test]
fn new_connection_dialog_keeps_schema() {
    const TYPED: &str =
        "postgres://e2e@127.0.0.1:1/third?sslmode=disable&options=--search_path%3Dapp";
    const SAVED: &str =
        "postgres://e2e@127.0.0.1:1/third?options=-c%20search_path%3Dapp&sslmode=disable";
    let mut app = App::launch_prepared(|_, seed| {
        seed["connection_string"] = Value::from(FIRST_URL);
        seed["recent_connections"] = serde_json::json!([{ "name": "first", "url": FIRST_URL }]);
    });

    app.click_menu("Connection", "New Connection…");
    app.fill_connection_dialog("third", TYPED);

    let config = app.wait_for_config("the schema connection selected", |c| {
        recent_names(c) == ["third", "first"]
    });
    assert_eq!(config["connection_string"], SAVED);

    app.quit();
}
