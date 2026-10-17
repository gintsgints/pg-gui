//! Running SQL against the database: from the editor, and a script file
//! from the files panel. Needs `PG_GUI_E2E_DATABASE`; skipped without it.

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
