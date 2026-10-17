//! Running SQL against the database: from the editor, cancelling a run, and
//! script files from the files panel. Needs `PG_GUI_E2E_DATABASE`; skipped
//! without it.

use crate::harness::{self, App, Button, Key, Mod, Schema};

#[test]
fn run_query_executes_against_database() {
    let Some(mut schema) = Schema::create("query::run_query_executes_against_database") else {
        return;
    };

    // One statement and the cursor at the buffer's start, so cmd-enter runs
    // exactly it; a new tab autocommits.
    let script = format!("create table {}.created_by_run (id int);", schema.name());
    let mut app = App::launch_with_database(&schema, &script);
    app.press(&[Mod::Cmd], Key::Return);
    app.wait_for("the table created by the run", harness::DB_TIMEOUT, |_| {
        schema.table_exists("created_by_run")
    });

    app.quit();
}

#[test]
fn run_script_from_files_panel_menu() {
    let Some(mut schema) = Schema::create("query::run_script_from_files_panel_menu") else {
        return;
    };
    let name = schema.name().to_string();
    let seed = format!(
        "create table {name}.seeded (id int);\n\
         insert into {name}.seeded values (1), (2), (3);\n"
    );
    let other = format!("create table {name}.not_run (id int);\n");
    let mut app = App::launch_with_scripts(
        &schema,
        &[
            ("a_other.sql", other.as_str()),
            ("b_seed.sql", seed.as_str()),
        ],
    );

    // Unfiltered, a_other.sql would be the top row.
    app.filter_files("b_seed");
    // Right-click the one row left; a file's menu is Open, Run Script, then
    // New Script…, Rename…, Delete…, and Up from no selection wraps to the
    // last item, so four Ups land on Run Script. Enter on it opens the
    // confirmation, and Enter runs.
    app.click_files_row(Button::Right, 0);
    for _ in 0..4 {
        app.press(&[], Key::Up);
    }
    app.press(&[], Key::Return);
    app.press(&[], Key::Return);

    // Both statements ran, in the run's autocommitted session.
    app.wait_for(
        "the script's rows in the database",
        harness::DB_TIMEOUT,
        |_| schema.table_exists("seeded") && schema.row_count("seeded") == 3,
    );
    assert!(
        !schema.table_exists("not_run"),
        "only the clicked script runs"
    );

    app.quit();
}

#[test]
fn run_selected_scripts_in_tree_order() {
    let Some(mut schema) = Schema::create("query::run_selected_scripts_in_tree_order") else {
        return;
    };
    let name = schema.name().to_string();
    let create = format!("create table {name}.runs (n int);\n");
    let first = format!("insert into {name}.runs values (1);\n");
    let skipped = format!("create table {name}.not_run (id int);\n");
    let second = format!("insert into {name}.runs values (2);\n");
    let mut app = App::launch_with_scripts(
        &schema,
        &[
            ("a_create.sql", create.as_str()),
            ("b_insert.sql", first.as_str()),
            ("c_skip.sql", skipped.as_str()),
            ("d_insert.sql", second.as_str()),
        ],
    );

    // A plain click anchors on a_create, shift-click takes the range down to
    // c_skip, cmd-click drops c_skip again and adds d_insert.
    app.click_files_row(Button::Left, 0);
    app.click_files_row_with(&[Mod::Shift], 2, false);
    app.click_files_row_with(&[Mod::Cmd], 2, true);
    app.click_files_row_with(&[Mod::Cmd], 3, true);
    // The batch confirms first; Enter runs it.
    app.click_menu("Connection", "Run Selected Scripts");
    app.press(&[], Key::Return);

    // The inserts need the table, so both landing means the batch ran in
    // tree order, not selection order; c_skip would have run before
    // d_insert had it still been selected.
    app.wait_for("both inserts in the database", harness::DB_TIMEOUT, |_| {
        schema.table_exists("runs") && schema.row_count("runs") == 2
    });
    assert!(
        !schema.table_exists("not_run"),
        "the deselected script does not run"
    );

    app.quit();
}

#[test]
fn run_bar_runs_cmd_picked_scripts() {
    let Some(mut schema) = Schema::create("query::run_bar_runs_cmd_picked_scripts") else {
        return;
    };
    let name = schema.name().to_string();
    let create = format!("create table {name}.runs (n int);\n");
    let skipped = format!("create table {name}.not_run (id int);\n");
    let insert = format!("insert into {name}.runs values (1);\n");
    let mut app = App::launch_with_scripts(
        &schema,
        &[
            ("a_create.sql", create.as_str()),
            ("b_skip.sql", skipped.as_str()),
            ("c_insert.sql", insert.as_str()),
        ],
    );

    // A plain click picks a_create, cmd-click adds c_insert; with two
    // picked, the panel's Run bar shows above the tree.
    app.click_files_row(Button::Left, 0);
    app.click_files_row_with(&[Mod::Cmd], 2, false);
    app.click_files_run_bar();
    app.press(&[], Key::Return);

    app.wait_for("the insert in the database", harness::DB_TIMEOUT, |_| {
        schema.table_exists("runs") && schema.row_count("runs") == 1
    });
    assert!(
        !schema.table_exists("not_run"),
        "the unpicked script does not run"
    );

    app.quit();
}

#[test]
fn cancel_query_stops_running_statement() {
    let Some(mut schema) = Schema::create("query::cancel_query_stops_running_statement") else {
        return;
    };
    schema.execute("create table {schema}.t (n int)");
    // The row would land only once the sleep is over; the sleep is far
    // longer than anything the scenario waits for.
    let script = format!(
        "insert into {}.t select 1 from pg_sleep(60);",
        schema.name()
    );
    let mut app = App::launch_with_database(&schema, &script);

    app.press(&[Mod::Cmd], Key::Return);
    app.wait_for("the statement running", harness::DB_TIMEOUT, |_| {
        schema
            .backends()
            .iter()
            .any(|(state, query)| state == "active" && query.contains("pg_sleep"))
    });

    // Cancelled, the tab's session goes idle — out of any transaction, as
    // autocommit leaves it — and the insert never happens.
    app.click_menu("Session", "Cancel Query");
    app.wait_for("the statement cancelled", harness::DB_TIMEOUT, |_| {
        schema
            .backends()
            .iter()
            .any(|(state, query)| state == "idle" && query.contains("pg_sleep"))
    });
    assert_eq!(schema.row_count("t"), 0, "the cancelled insert never lands");

    app.quit();
}
