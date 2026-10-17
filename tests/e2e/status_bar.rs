//! The status bar: the run indicator and last status on its left, the
//! format-on-save toggle, AI and language-server states on its right. Some
//! scenarios need `PG_GUI_E2E_DATABASE` and skip without it; the rest run
//! on the seeded dead connection or on none.
//!
//! The bar is drawn by GPUI, so it is read with text recognition
//! (`App::status_bar_text`). Recognition is good but not exact — `AI` reads
//! as `Al`, a `·` as `•` or `-` — so the scenarios look for distinctive
//! words rather than whole lines.

use std::time::{Duration, Instant};

use crate::harness::{self, App, Key, Mod, Schema};

/// How long a status may take to show: the language server's states wait
/// on its first connection attempt.
const STATUS_TIMEOUT: Duration = Duration::from_secs(20);

/// Wait until the status bar reads `needle`; panics with the last reading.
fn wait_for_status(app: &mut App, needle: &str) {
    let start = Instant::now();
    loop {
        let text = app.status_bar_text();
        if text.contains(needle) {
            return;
        }
        assert!(
            start.elapsed() < STATUS_TIMEOUT,
            "the status bar never read {needle:?}; last: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Read the status bar over and over until it reads `done`, returning every
/// reading taken on the way (the last one included).
fn watch_status_until(app: &mut App, done: &str) -> Vec<String> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        let text = app.status_bar_text();
        let finished = text.contains(done);
        seen.push(text);
        if finished {
            return seen;
        }
        assert!(
            start.elapsed() < STATUS_TIMEOUT,
            "the status bar never read {done:?}; readings: {seen:#?}"
        );
    }
}

/// The `n` of every `statement n/total` the readings show, in order, with
/// repeats dropped.
fn statement_numbers(seen: &[String], total: usize) -> Vec<usize> {
    let mut numbers: Vec<usize> = seen
        .iter()
        .filter_map(|text| {
            let (before, _) = text.split_once(&format!("/{total}"))?;
            let digits: String = before
                .chars()
                .rev()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.chars().rev().collect::<String>().parse().ok()
        })
        .collect();
    numbers.dedup();
    numbers
}

#[test]
fn shows_format_on_save_state() {
    let mut app = App::launch();

    wait_for_status(&mut app, "fmt: on");
    app.click_menu("Edit", "Format on Save");
    wait_for_status(&mut app, "fmt: off");

    app.quit();
}

#[test]
fn shows_unreachable_database() {
    // The seeded connection points at a port nothing listens on.
    let mut app = App::launch();

    wait_for_status(&mut app, "database unreachable");

    app.quit();
}

#[test]
fn shows_no_database() {
    // A connection string the driver cannot parse: no database at all.
    let mut app = App::launch_prepared(|_, seed| {
        seed["connection_string"] = serde_json::Value::from("e2e-no-database");
    });

    wait_for_status(&mut app, "no schema");
    wait_for_status(&mut app, "no database");

    app.quit();
}

#[test]
fn shows_failed_run() {
    let mut app = App::launch_for_results(None, "select 1;");

    // The dead connection fails the run; the summary goes to the bar.
    app.press(&[Mod::Cmd], Key::Return);
    wait_for_status(&mut app, "Error:");
    wait_for_status(&mut app, "connection failed");

    app.quit();
}

#[test]
fn shows_schema_loaded() {
    let Some(schema) = Schema::create("status_bar::shows_schema_loaded") else {
        return;
    };
    let mut app = App::launch_with_database(&schema, "");

    wait_for_status(&mut app, "SQL LSP: schema loaded");

    app.quit();
}

#[test]
fn shows_run_progress_and_summary() {
    let Some(schema) = Schema::create("status_bar::shows_run_progress_and_summary") else {
        return;
    };
    // Each statement holds for long enough that the bar is read at least
    // once while it runs.
    let script = "select pg_sleep(2);\nselect pg_sleep(2);\nselect pg_sleep(2);";
    let mut app = App::launch_with_database(&schema, script);
    wait_for_status(&mut app, "schema loaded");

    app.send_keys(&[Mod::Cmd], 'a');
    app.press(&[Mod::Cmd], Key::Return);
    let seen = watch_status_until(&mut app, "3 statement(s) executed");

    assert!(
        seen.iter().any(|text| text.contains("Running selection")),
        "the run is announced: {seen:#?}"
    );
    let numbers = statement_numbers(&seen, 3);
    assert!(
        numbers.len() >= 2 && numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "the statement counter climbs through the run: {numbers:?} in {seen:#?}"
    );
    let summary = seen.last().expect("a reading");
    assert!(
        summary.contains("selection: 3 statement(s) executed"),
        "the summary names the scope: {summary:?}"
    );

    app.quit();
}

#[test]
fn shows_script_batch_progress_and_summary() {
    let Some(schema) = Schema::create("status_bar::shows_script_batch_progress_and_summary") else {
        return;
    };
    let mut app = App::launch_with_scripts(
        &schema,
        &[
            ("alpha.sql", "select pg_sleep(2);\nselect pg_sleep(2);\n"),
            ("beta.sql", "select pg_sleep(2);\n"),
        ],
    );
    wait_for_status(&mut app, "schema loaded");

    // Pick both and run them from the menu; Enter confirms.
    app.click_files_row(harness::Button::Left, 0);
    app.click_files_row_with(&[Mod::Shift], 1, false);
    app.click_menu("Connection", "Run Selected Scripts");
    app.press(&[], Key::Return);
    let seen = watch_status_until(&mut app, "3 statement(s) executed");

    let running = |file: &str| {
        seen.iter()
            .position(|text| text.contains("Running 2 script(s)") && text.contains(file))
    };
    let (alpha, beta) = (running("alpha.sql"), running("beta.sql"));
    assert!(
        alpha.is_some() && beta.is_some() && alpha < beta,
        "the bar names each file as it runs, in order: {seen:#?}"
    );
    assert!(
        seen.iter()
            .any(|text| text.contains("alpha.sql") && text.contains("/2")),
        "a file's own statements are counted: {seen:#?}"
    );
    let summary = seen.last().expect("a reading");
    assert!(
        summary.contains("2 script(s): 3 statement(s) executed"),
        "the summary covers the batch: {summary:?}"
    );

    app.quit();
}
