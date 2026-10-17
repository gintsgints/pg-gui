//! Formatting through the embedded language server: Edit ▸ Format and the
//! `keyword_case` / `constant_case` settings. No database needed.
//!
//! The server starts in the background, and a format sent before it is up
//! only reports that it is unavailable, so each scenario repeats its
//! trigger until the buffer changes (formatting formatted text is a no-op)
//! and then checks the exact text. Format-on-save and the cmd-shift-f
//! shortcut are covered in `editor`.

use serde_json::Value;

use crate::harness::{self, App};

/// Every kind of word the casing settings tell apart: keywords (`NULL` in
/// `IS NULL` is one), a boolean constant, and a string literal, which both
/// leave alone.
const MIXED: &str = "SELECT a,b FROM t WHERE a=1 AND b IS NULL AND c=TRUE AND d='X';";

/// Start with one untitled tab holding `script`, the formatter's casing as
/// given.
fn launch(script: &str, keyword_case: &str, constant_case: &str) -> App {
    App::launch_prepared(|_, seed| {
        seed["tabs"] = serde_json::json!([{ "script": script, "file": null }]);
        seed["keyword_case"] = Value::from(keyword_case);
        seed["constant_case"] = Value::from(constant_case);
    })
}

/// Click Edit ▸ Format until the buffer changes, and return its text.
fn format(app: &mut App, original: &str) -> String {
    let config = app.retry_until(
        "the formatted script",
        harness::DB_TIMEOUT,
        |app| app.click_menu("Edit", "Format"),
        |config| harness::active_script(config).is_some_and(|script| script != original),
    );
    harness::active_script(&config)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn format_menu_formats_every_statement() {
    let original = "SELECT a,b FROM t WHERE a=1;\nUPDATE t SET a=2 WHERE b='X';";
    let mut app = launch(original, "lower", "lower");

    assert_eq!(
        format(&mut app, original),
        "select a, b from t where a = 1;\nupdate t set a = 2 where b = 'X';",
        "both statements spaced and lower-cased, the string literal kept"
    );

    app.quit();
}

#[test]
fn upper_keywords_lower_constants() {
    let mut app = launch(MIXED, "upper", "lower");

    assert_eq!(
        format(&mut app, MIXED),
        "SELECT a, b FROM t WHERE a = 1 AND b IS NULL AND c = true AND d = 'X';"
    );

    app.quit();
}

#[test]
fn lower_keywords_upper_constants() {
    let mut app = launch(MIXED, "lower", "upper");

    assert_eq!(
        format(&mut app, MIXED),
        "select a, b from t where a = 1 and b is null and c = TRUE and d = 'X';"
    );

    app.quit();
}
