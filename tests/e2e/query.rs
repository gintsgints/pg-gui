//! Running SQL from the editor against the database. Needs
//! `PG_GUI_E2E_DATABASE`; skipped without it.

use crate::harness::{self, App, Key, Mod, Schema};

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
