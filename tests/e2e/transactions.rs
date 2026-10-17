//! Transaction control: autocommit, and the Session menu's Commit and
//! Rollback. Needs `PG_GUI_E2E_DATABASE`; skipped without it.
//!
//! Each scenario runs an insert from the editor and watches the row from
//! its own connection, which sees it only once the app's transaction has
//! committed. "Ran but not committed" is told apart from "not run yet" by
//! the app's backend showing up as `idle in transaction`
//! (`Schema::open_transactions`).

use crate::harness::{self, App, Key, Mod, Schema};

/// The schema's `t`, and an app connected to its database with one tab
/// holding an insert into it, the cursor at its start.
fn launch(schema: &mut Schema) -> App {
    schema.execute("create table {schema}.t (n int)");
    let script = format!("insert into {}.t values (1);", schema.name());
    App::launch_with_database(schema, &script)
}

/// Turn the active tab's autocommit off from the Session menu.
fn autocommit_off(app: &mut App) {
    app.click_menu("Session", "Toggle Autocommit");
    app.wait_for_config("autocommit off", |c| c["autocommit"] == false);
}

/// Run the insert and wait until it sits uncommitted in the app's open
/// transaction: the backend is idle in it and the row is not visible yet.
fn run_uncommitted(app: &mut App, schema: &mut Schema) {
    app.press(&[Mod::Cmd], Key::Return);
    app.wait_for("the insert's open transaction", harness::DB_TIMEOUT, |_| {
        schema.open_transactions() == 1
    });
    assert_eq!(
        schema.row_count("t"),
        0,
        "the row stays invisible until the transaction commits"
    );
}

#[test]
fn autocommit_commits_each_run() {
    let Some(mut schema) = Schema::create("transactions::autocommit_commits_each_run") else {
        return;
    };
    let mut app = launch(&mut schema);
    // The seed leaves autocommit at its default: on.

    app.press(&[Mod::Cmd], Key::Return);
    app.wait_for("the committed row", harness::DB_TIMEOUT, |_| {
        schema.row_count("t") == 1
    });
    assert_eq!(schema.open_transactions(), 0, "no transaction left open");

    app.quit();
}

#[test]
fn manual_commit_needs_commit() {
    let Some(mut schema) = Schema::create("transactions::manual_commit_needs_commit") else {
        return;
    };
    let mut app = launch(&mut schema);
    autocommit_off(&mut app);
    run_uncommitted(&mut app, &mut schema);

    app.click_menu("Session", "Commit");
    app.wait_for("the committed row", harness::DB_TIMEOUT, |_| {
        schema.row_count("t") == 1 && schema.open_transactions() == 0
    });

    app.quit();
}

#[test]
fn manual_rollback_discards() {
    let Some(mut schema) = Schema::create("transactions::manual_rollback_discards") else {
        return;
    };
    let mut app = launch(&mut schema);
    autocommit_off(&mut app);
    run_uncommitted(&mut app, &mut schema);

    app.click_menu("Session", "Rollback");
    app.wait_for("the transaction to end", harness::DB_TIMEOUT, |_| {
        schema.open_transactions() == 0
    });
    assert_eq!(schema.row_count("t"), 0, "the rolled-back row never lands");

    app.quit();
}

#[test]
fn autocommit_on_commits_open_transaction() {
    let Some(mut schema) = Schema::create("transactions::autocommit_on_commits_open_transaction")
    else {
        return;
    };
    let mut app = launch(&mut schema);
    autocommit_off(&mut app);
    run_uncommitted(&mut app, &mut schema);

    // Turning autocommit back on commits what the tab left open.
    app.click_menu("Session", "Toggle Autocommit");
    app.wait_for("the committed row", harness::DB_TIMEOUT, |_| {
        schema.row_count("t") == 1 && schema.open_transactions() == 0
    });
    assert_eq!(app.config()["autocommit"], true, "autocommit back on");

    app.quit();
}
