//! Query plans: Explain Plan (cmd-e) and Explain Analyze (cmd-shift-e) over
//! the statement under the cursor. Needs `PG_GUI_E2E_DATABASE`; skipped
//! without it.
//!
//! The flame graph cannot be read from outside the window, so the scenarios
//! watch the tab's session from the database: once an explain has finished,
//! the app's backend is idle with the `EXPLAIN (…)` it sent as its last
//! statement (`Schema::backends`). The statement explained is an insert,
//! which tells the two apart — a plain plan never runs it, `ANALYZE` does.

use crate::harness::{self, App, Mod, Schema};

const PLAN: &str = "EXPLAIN (FORMAT JSON) ";
const ANALYZE: &str = "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) ";

/// The schema's `t`, and an app connected to its database with one tab
/// holding an insert into it, the cursor at its start.
fn launch(schema: &mut Schema) -> App {
    schema.execute("create table {schema}.t (n int)");
    let script = format!("insert into {}.t values (1);", schema.name());
    App::launch_with_database(schema, &script)
}

/// Wait until the app's session has finished an explain sent as `prefix`
/// and sits in `state`.
fn wait_explained(app: &mut App, schema: &mut Schema, prefix: &str, state: &str) {
    app.wait_for(
        &format!("{prefix}… finished, session {state}"),
        harness::DB_TIMEOUT,
        |_| {
            schema
                .backends()
                .iter()
                .any(|(s, query)| s == state && query.starts_with(prefix))
        },
    );
}

#[test]
fn explain_plan_does_not_run_statement() {
    let Some(mut schema) = Schema::create("explain::explain_plan_does_not_run_statement") else {
        return;
    };
    let mut app = launch(&mut schema);

    app.send_keys(&[Mod::Cmd], 'e');
    wait_explained(&mut app, &mut schema, PLAN, "idle");
    assert_eq!(
        schema.row_count("t"),
        0,
        "a plain plan never runs the insert"
    );

    app.quit();
}

#[test]
fn explain_analyze_runs_statement() {
    let Some(mut schema) = Schema::create("explain::explain_analyze_runs_statement") else {
        return;
    };
    let mut app = launch(&mut schema);

    // Autocommit is on by default, so the analyzed insert commits.
    app.send_keys(&[Mod::Cmd, Mod::Shift], 'e');
    wait_explained(&mut app, &mut schema, ANALYZE, "idle");
    assert_eq!(schema.row_count("t"), 1, "ANALYZE runs the insert for real");

    app.quit();
}

#[test]
fn explain_analyze_rolls_back_without_autocommit() {
    let Some(mut schema) = Schema::create("explain::explain_analyze_rolls_back_without_autocommit")
    else {
        return;
    };
    let mut app = launch(&mut schema);
    app.click_menu("Session", "Toggle Autocommit");
    app.wait_for_config("autocommit off", |c| c["autocommit"] == false);

    // The analyzed insert lands in the tab's transaction, uncommitted...
    app.send_keys(&[Mod::Cmd, Mod::Shift], 'e');
    wait_explained(&mut app, &mut schema, ANALYZE, "idle in transaction");
    assert_eq!(schema.row_count("t"), 0, "not committed yet");

    // ...where Rollback undoes it.
    app.click_menu("Session", "Rollback");
    app.wait_for("the transaction to end", harness::DB_TIMEOUT, |_| {
        schema.open_transactions() == 0
    });
    assert_eq!(
        schema.row_count("t"),
        0,
        "the rolled-back insert never lands"
    );

    app.quit();
}
