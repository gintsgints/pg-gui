//! The snippet picker (Edit ▸ Snippets, cmd-p): built-in snippets and the
//! user's own `.sql` files under `snippets/` in the config directory,
//! inserted at the cursor as a statement of their own. All but the last
//! scenario need no database; that one runs what a snippet built and needs
//! `PG_GUI_E2E_DATABASE`, skipping without it.
//!
//! As in `editor`, the buffer is read back through the tab's `script` in
//! config.json, and typed text avoids quotes and brackets.

use crate::harness::{self, App, Key, Mod, Schema};

/// Start with one untitled tab holding `script` (the cursor at its start)
/// and the user snippets `snippets` (file stem, SQL) in the scratch config
/// directory.
fn launch(script: &str, snippets: &[(&str, &str)]) -> App {
    App::launch_prepared(|dir, seed| {
        let folder = dir.join("snippets");
        std::fs::create_dir_all(&folder).expect("create snippets dir");
        for (name, sql) in snippets {
            std::fs::write(folder.join(format!("{name}.sql")), sql).expect("write snippet");
        }
        seed["tabs"] = serde_json::json!([{ "script": script, "file": null }]);
    })
}

/// Wait until the active tab's text is exactly `expected`.
fn wait_for_script(app: &mut App, what: &str, expected: &str) {
    app.wait_for_config(&format!("{what}: {expected:?}"), |config| {
        harness::active_script(config) == Some(expected)
    });
}

#[test]
fn picker_inserts_user_snippet_as_own_statement() {
    let mut app = launch("select 1;", &[("Zebra probe", "select 4242;")]);

    // Inserted at the cursor (the buffer's start), on a line of its own.
    app.pick_snippet("zebra");
    wait_for_script(
        &mut app,
        "the snippet above the script",
        "select 4242;\nselect 1;",
    );

    app.quit();
}

#[test]
fn filter_snippet_leaves_caret_between_percents() {
    let mut app = launch("", &[]);

    // A built-in whose name filter defaults to `ILIKE '%%'`: the caret
    // lands between the two `%`, so typing narrows the filter.
    app.pick_snippet("table & index breakdown");
    app.wait_for_config("the snippet inserted", |c| {
        harness::active_script(c).is_some_and(|s| s.contains("ILIKE '%%'"))
    });
    app.type_text("orders");
    app.wait_for_config("the filter typed between the percents", |c| {
        harness::active_script(c).is_some_and(|s| s.contains("WHERE relname ILIKE '%orders%'"))
    });

    app.quit();
}

#[test]
fn tab_stops_take_typed_values_in_order() {
    let mut app = launch(
        "",
        &[("Zebra stops", "select ${1:columns} from ${2:source};")],
    );

    // Each marker turns into its placeholder only when its stop is
    // reached, selected so typing replaces it; Tab moves to the next one.
    app.pick_snippet("zebra");
    wait_for_script(
        &mut app,
        "the first stop resolved",
        "select columns from ${2:source};\n",
    );
    app.type_text("id");
    wait_for_script(
        &mut app,
        "the first stop typed",
        "select id from ${2:source};\n",
    );
    app.press(&[], Key::Tab);
    app.type_text("orders");
    // Close a completion menu the typing may have opened.
    app.press(&[], Key::Escape);
    wait_for_script(&mut app, "both stops filled", "select id from orders;\n");

    app.quit();
}

#[test]
fn view_snippet_creates_view() {
    let Some(mut schema) = Schema::create("snippets::view_snippet_creates_view") else {
        return;
    };
    let name = schema.name().to_string();
    schema.execute("create table {schema}.orders (id int, total numeric)");
    let mut app = App::launch_with_database(&schema, "");

    // Every word of "New: view" is in "New: materialized view" too, which
    // sorts ahead of it, so the view template is the second match. Its
    // stops are the view's name, the select list and the source table; Tab
    // keeps the `*` placeholder.
    app.pick_snippet_nth("new: view", 1);
    app.type_text(&format!("{name}.order_totals"));
    app.press(&[], Key::Tab);
    app.press(&[], Key::Tab);
    app.type_text(&format!("{name}.orders"));
    // Close a completion menu the typing may have opened.
    app.press(&[], Key::Escape);
    wait_for_script(
        &mut app,
        "every stop filled",
        &format!("CREATE OR REPLACE VIEW {name}.order_totals AS\nSELECT *\nFROM {name}.orders;\n"),
    );

    // The cursor is still inside the statement, so cmd-enter runs it; a
    // new tab autocommits.
    app.press(&[Mod::Cmd], Key::Return);
    app.wait_for("the view created by the run", harness::DB_TIMEOUT, |_| {
        schema.view_exists("order_totals")
    });

    app.quit();
}
