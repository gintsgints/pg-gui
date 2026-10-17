//! Hover in the SQL editor: resting the pointer on a name asks the embedded
//! language server about it, which answers from its schema cache. Needs
//! `PG_GUI_E2E_DATABASE`; skipped without it.
//!
//! The popup is drawn inside the window and cannot be read from outside,
//! but the app logs every answer the server gives (`server hover blocks=N`,
//! the number of markdown blocks it returned; see [`harness::App::log`]).
//! The schema cache loads in the background and nothing outside the window
//! says when, so each scenario re-hovers until an answer with content
//! arrives.

use crate::harness::{self, App, Schema};

/// The block counts of every hover answer logged so far, in order.
fn hover_answers(app: &App) -> Vec<usize> {
    app.log()
        .lines()
        .map(strip_ansi)
        .filter(|line| line.contains("server hover"))
        .filter_map(|line| {
            let count = line.split_once("blocks=")?.1;
            count
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .collect()
}

/// `line` without terminal colour codes, which a log line may carry
/// around its field names.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Up to and including the sequence's final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Hover editor row 0, column `col` until the server answers with content.
fn hover_until_answered(app: &mut App, what: &str, col: u32) {
    app.wait_for(what, harness::DB_TIMEOUT, |app| {
        let before = hover_answers(app).len();
        app.hover_text(0, col);
        // Give the editor's hover delay and the server's answer time.
        std::thread::sleep(std::time::Duration::from_millis(1200));
        hover_answers(app)[before..]
            .iter()
            .any(|&blocks| blocks > 0)
    });
}

/// A table with a column, and a script naming both; returns the script and
/// the columns of the middle of the column's and the table's names in it.
fn select_from_table(schema: &mut Schema) -> (String, u32, u32) {
    schema.execute("create table {schema}.zebra_orders (id int, zebra_total numeric)");
    let head = "select zebra_total from ";
    let table = format!("{}.", schema.name());
    let script = format!("{head}{table}zebra_orders;");
    let column_col = 7 + 5; // inside `zebra_total`
    let table_col = u32::try_from(head.len() + table.len() + 6).expect("short script");
    (script, column_col, table_col)
}

#[test]
fn hover_on_table_name_answers() {
    let Some(mut schema) = Schema::create("hover::hover_on_table_name_answers") else {
        return;
    };
    let (script, _, table_col) = select_from_table(&mut schema);
    let mut app = App::launch_for_results(Some(schema.url()), &script);

    hover_until_answered(&mut app, "a hover answer on the table", table_col);

    app.quit();
}

#[test]
fn hover_on_column_name_answers() {
    let Some(mut schema) = Schema::create("hover::hover_on_column_name_answers") else {
        return;
    };
    let (script, column_col, _) = select_from_table(&mut schema);
    let mut app = App::launch_for_results(Some(schema.url()), &script);

    hover_until_answered(&mut app, "a hover answer on the column", column_col);

    app.quit();
}

#[test]
fn hover_on_keyword_answers_nothing() {
    let Some(mut schema) = Schema::create("hover::hover_on_keyword_answers_nothing") else {
        return;
    };
    let (script, _, table_col) = select_from_table(&mut schema);
    let mut app = App::launch_for_results(Some(schema.url()), &script);
    // Answered on the table first, so the schema cache is known loaded and
    // an empty answer below is about the word, not the cache.
    hover_until_answered(&mut app, "a hover answer on the table", table_col);

    let before = hover_answers(&app).len();
    app.hover_text(0, 2); // inside `select`
    app.wait_for(
        "a hover answer on the keyword",
        harness::REACT_TIMEOUT,
        |app| hover_answers(app).len() > before,
    );
    // Moving onto the keyword is all the scenario did since, so every answer
    // after `before` is the keyword's (or the blank spot's the pointer
    // passed on its way).
    let answers = hover_answers(&app);
    assert!(
        answers[before..].iter().all(|&blocks| blocks == 0),
        "a keyword has nothing to show: {answers:?}"
    );

    app.quit();
}
