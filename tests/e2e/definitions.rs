//! Go to Definition: cmd-click on a name in the editor resolves it in the
//! catalog and opens its definition — the object's `.sql` file from the
//! working folder when one matches `definition_file_mask`, else the
//! definition fetched from the server, in a tab. Needs
//! `PG_GUI_E2E_DATABASE`; skipped without it.
//!
//! The pointer has to rest on the name with cmd held before the click
//! counts: the editor resolves the link on that hover, and the lookup opens
//! a connection of its own. Nothing outside the window says when it has,
//! so each scenario repeats the hover-and-click (`App::cmd_click_text`)
//! until the definition shows up.

use std::time::Duration;

use serde_json::Value;

use crate::harness::{self, App, Schema};

/// How long the pointer rests on the name before each click.
const DWELL: Duration = Duration::from_secs(1);

/// The scenario's table, and a script naming it qualified. Returns the
/// script and the column of the middle of `orders` in it.
fn select_orders(schema: &mut Schema) -> (String, u32) {
    schema.execute("create table {schema}.orders (id int primary key, total numeric)");
    let prefix = format!("select * from {}.", schema.name());
    let script = format!("{prefix}orders;");
    let col = u32::try_from(prefix.len() + 3).expect("short script");
    (script, col)
}

/// The text and file of the active tab.
fn active_tab(config: &Value) -> (Option<&str>, Option<&str>) {
    let tab = config["active_tab"]
        .as_u64()
        .and_then(|ix| config["tabs"].get(usize::try_from(ix).ok()?));
    (
        tab.and_then(|t| t["script"].as_str()),
        tab.and_then(|t| t["file"].as_str()),
    )
}

#[test]
fn cmd_click_opens_fetched_definition() {
    let Some(mut schema) = Schema::create("definitions::cmd_click_opens_fetched_definition") else {
        return;
    };
    let (script, col) = select_orders(&mut schema);
    let mut app = App::launch_for_results(Some(schema.url()), &script);

    // No working folder, so the definition is fetched and opens in a new
    // untitled tab beside the script.
    let config = app.retry_until(
        "the table's definition in a tab",
        harness::DB_TIMEOUT,
        |app| app.cmd_click_text(0, col, DWELL),
        |c| {
            matches!(active_tab(c), (Some(text), None)
                if text.contains("CREATE TABLE") && text.contains("orders"))
        },
    );
    assert_eq!(
        config["tabs"].as_array().map(Vec::len),
        Some(2),
        "the script's tab stays open beside the definition"
    );

    app.quit();
}

#[test]
fn cmd_click_opens_matching_file() {
    let Some(mut schema) = Schema::create("definitions::cmd_click_opens_matching_file") else {
        return;
    };
    let (script, col) = select_orders(&mut schema);
    let mut file = String::new();
    let mut app = App::launch_for_results_prepared(Some(schema.url()), &script, |dir, seed| {
        // The default mask, `*_{object}.sql`, matches this one.
        let work = dir.join("work");
        std::fs::create_dir_all(work.join("tables")).expect("create working folder");
        let path = work.join("tables/01_orders.sql");
        std::fs::write(&path, "-- orders, as kept in the repository\n").expect("write file");
        let work = work.canonicalize().expect("canonicalize working folder");
        seed["working_dir"] = Value::from(work.to_str().expect("utf-8 temp path"));
        file = work
            .join("tables/01_orders.sql")
            .to_str()
            .expect("utf-8 temp path")
            .to_string();
    });

    app.retry_until(
        "the matching file opened",
        harness::DB_TIMEOUT,
        |app| app.cmd_click_text(0, col, DWELL),
        |c| active_tab(c) == (Some("-- orders, as kept in the repository\n"), Some(&file)),
    );

    app.quit();
}
