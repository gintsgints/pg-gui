//! SQL syntax checking in the editor: the embedded language server's
//! diagnostics, as the user reaches them through Next Problem (cmd-j).
//!
//! Neither the squiggles nor the status-bar echo can be read from outside
//! the window, but cmd-j leaves the cursor on the start of the problem it
//! lands on, so typing a marker character there and reading the tab's
//! `script` back from config.json says exactly where the problem is — and,
//! with no problem at all, the marker stays where the cursor was. Every
//! scenario starts the walk from the buffer's end (cmd-a, Right), where the
//! first press wraps round to the first error.
//!
//! When to press is the harder part: diagnostics are published on a
//! debounce after each change, and a press before they arrive finds
//! nothing. The app logs each published run (`server diagnostics count=N`,
//! see [`harness::App::log`]), so the scenarios wait for that line first.
//!
//! The offline scenarios run on a connection string the driver cannot
//! parse, which the language server takes as "no database": it still parses
//! and lints, and never spends a run on a failed connection attempt (whose
//! error would replace the run's syntax errors with nothing). The last
//! scenario checks the same against a live schema cache.

use std::time::Duration;

use crate::harness::{self, App, Key, Mod, Schema};

/// A connection string that does not parse, so the language server checks
/// without a database rather than failing to reach one.
const NO_DATABASE: &str = "e2e-no-database";

/// The character typed where cmd-j leaves the cursor. Not one SQL pairs or
/// completes on, and not in any scenario's script.
const MARKER: &str = "#";

/// How long the language server may take to publish a run: it starts in the
/// background, and its first build of the parser is slow.
const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(30);

/// Time for the app to take a run the log has already reported: the log
/// line is written as the run is sent, the editor picks it up a frame later.
const SETTLE: Duration = Duration::from_millis(500);

/// Start offline with one untitled tab holding `script`.
fn launch_offline(script: &str) -> App {
    App::launch_prepared(|_, seed| {
        seed["connection_string"] = serde_json::Value::from(NO_DATABASE);
        seed["tabs"] = serde_json::json!([{ "script": script, "file": null }]);
    })
}

/// The problem count of every diagnostics run published so far, oldest
/// first.
fn diagnostics_runs(app: &App) -> Vec<usize> {
    app.log()
        .lines()
        .filter(|line| line.contains("server diagnostics"))
        .filter_map(|line| {
            let count = line.split_once("count=")?.1;
            let digits = count.split(|c: char| !c.is_ascii_digit()).next()?;
            digits.parse().ok()
        })
        .collect()
}

/// Wait until a run after the first `after` has been published and the
/// latest one's problem count satisfies `pred`, then give the editor time
/// to take it.
fn wait_for_diagnostics(app: &mut App, what: &str, after: usize, pred: impl Fn(usize) -> bool) {
    app.wait_for(what, DIAGNOSTICS_TIMEOUT, |app| {
        let runs = diagnostics_runs(app);
        runs.len() > after && runs.last().is_some_and(|&count| pred(count))
    });
    std::thread::sleep(SETTLE);
}

/// Put the cursor at the buffer's end, press cmd-j `presses` times and type
/// [`MARKER`] where it lands.
fn mark_problem(app: &mut App, presses: usize) {
    app.send_keys(&[Mod::Cmd], 'a');
    app.press(&[], Key::Right);
    for _ in 0..presses {
        app.send_keys(&[Mod::Cmd], 'j');
    }
    app.type_text(MARKER);
}

/// Wait until the active tab's text is exactly `expected`.
fn wait_for_script(app: &mut App, what: &str, expected: &str) {
    app.wait_for_config(&format!("{what}: {expected:?}"), |config| {
        harness::active_script(config) == Some(expected)
    });
}

#[test]
fn valid_script_reports_no_problems() {
    let script = "select 1;\nselect 2;";
    let mut app = launch_offline(script);

    wait_for_diagnostics(&mut app, "a clean run", 0, |count| count == 0);
    // Nothing to land on, so the cursor stays at the end.
    mark_problem(&mut app, 1);
    wait_for_script(
        &mut app,
        "the marker at the end",
        &format!("{script}{MARKER}"),
    );

    app.quit();
}

#[test]
fn syntax_error_is_located_at_its_statement() {
    let mut app = launch_offline("select 1;\nselect from where;\nselect 3;");

    wait_for_diagnostics(&mut app, "a run with the error", 0, |count| count > 0);
    // A syntax error spans the whole statement, so cmd-j lands on its start
    // — not on the valid statements either side.
    mark_problem(&mut app, 1);
    wait_for_script(
        &mut app,
        "the marker on the broken statement",
        "select 1;\n#select from where;\nselect 3;",
    );

    app.quit();
}

#[test]
fn next_problem_walks_every_error() {
    let mut app = launch_offline("select from where;\nselect 2;\nselect 1 +;");

    wait_for_diagnostics(&mut app, "a run with both errors", 0, |count| count >= 2);
    // From the end the first press wraps to the first error, the second
    // moves on to the next one, past the valid statement between them.
    mark_problem(&mut app, 2);
    wait_for_script(
        &mut app,
        "the marker on the second error",
        "select from where;\nselect 2;\n#select 1 +;",
    );

    app.quit();
}

#[test]
fn typed_error_is_flagged_and_its_fix_clears_it() {
    let mut app = launch_offline("");
    wait_for_diagnostics(&mut app, "the opening run", 0, |count| count == 0);

    let before = diagnostics_runs(&app).len();
    app.type_text("select from where;");
    app.press(&[], Key::Escape);
    wait_for_script(&mut app, "the typed error", "select from where;");
    wait_for_diagnostics(&mut app, "the typed error flagged", before, |count| {
        count > 0
    });
    mark_problem(&mut app, 1);
    wait_for_script(&mut app, "the marker on the error", "#select from where;");

    let before = diagnostics_runs(&app).len();
    app.send_keys(&[Mod::Cmd], 'a');
    app.type_text("select 1;");
    app.press(&[], Key::Escape);
    wait_for_script(&mut app, "the fix", "select 1;");
    wait_for_diagnostics(&mut app, "the error cleared", before, |count| count == 0);
    mark_problem(&mut app, 1);
    wait_for_script(&mut app, "the marker at the end", "select 1;#");

    app.quit();
}

#[test]
fn syntax_error_is_flagged_with_database_connected() {
    let Some(mut schema) =
        Schema::create("editor_syntax::syntax_error_is_flagged_with_database_connected")
    else {
        return;
    };
    schema.execute("create table {schema}.t (id int)");

    // With a database the run also typechecks against the schema cache;
    // that must neither swallow the syntax error nor flag the valid query
    // on a table that exists.
    let valid = format!("select id from {}.t;", schema.name());
    let mut app = App::launch_with_database(&schema, &format!("{valid}\nselect from where;"));

    wait_for_diagnostics(&mut app, "a run with the error", 0, |count| count > 0);
    // The broken statement is the only error, so a second press wraps back
    // onto it; a problem on the valid query would take the second press.
    mark_problem(&mut app, 2);
    wait_for_script(
        &mut app,
        "the marker on the broken statement",
        &format!("{valid}\n#select from where;"),
    );

    app.quit();
}
