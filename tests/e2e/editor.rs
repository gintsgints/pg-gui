//! The SQL editor: typing, undo/redo, comment toggling, the clipboard,
//! formatting, snippet completion and saving. No database needed.
//!
//! The editor cannot be read from outside the window, but the app mirrors
//! every buffer change into the tab's `script` in config.json (half a
//! second later, debounced), so that is what the scenarios check — plus
//! the clipboard for copy, and the file on disk for save.
//!
//! The editor has the keyboard focus from launch. Typed text avoids quotes
//! and brackets, which the editor may pair on its own.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::harness::{self, App, Key, Mod};

/// Start with one untitled tab holding `script`.
fn launch_with_script(script: &str) -> App {
    App::launch_prepared(|_, seed| {
        seed["tabs"] = serde_json::json!([{ "script": script, "file": null }]);
    })
}

/// Start with one tab open on a file `q.sql` holding `content`, with
/// format-on-save as given. Returns the app and the file's path.
fn launch_with_file(content: &str, format_on_save: bool) -> (App, PathBuf) {
    let mut path = PathBuf::new();
    let app = App::launch_prepared(|dir: &Path, seed: &mut Value| {
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("create e2e working folder");
        let file = work.join("q.sql");
        std::fs::write(&file, content).expect("write seeded script");
        path = file.canonicalize().expect("canonicalize seeded script");
        seed["tabs"] = serde_json::json!([{
            "script": content,
            "file": path.to_str().expect("utf-8 temp path"),
        }]);
        seed["format_on_save"] = Value::from(format_on_save);
        seed["files_panel_visible"] = Value::from(false);
    });
    (app, path)
}

/// Wait until the active tab's text is exactly `expected`.
fn wait_for_script(app: &mut App, what: &str, expected: &str) {
    app.wait_for_config(&format!("{what}: {expected:?}"), |config| {
        harness::active_script(config) == Some(expected)
    });
}

#[test]
fn typing_updates_tab_script() {
    let mut app = App::launch();

    app.type_text("select 1 as one;");
    // Close the completion menu the typing may have opened, so nothing is
    // left pending on top of the buffer.
    app.press(&[], Key::Escape);
    wait_for_script(&mut app, "the typed text", "select 1 as one;");

    app.quit();
}

#[test]
fn undo_and_redo() {
    let mut app = launch_with_script("select 1;");

    // One edit: the whole buffer replaced by one character.
    app.send_keys(&[Mod::Cmd], 'a');
    app.type_text("x");
    app.press(&[], Key::Escape);
    wait_for_script(&mut app, "the replacement", "x");

    app.send_keys(&[Mod::Cmd], 'z');
    wait_for_script(&mut app, "undo", "select 1;");

    app.send_keys(&[Mod::Cmd, Mod::Shift], 'z');
    wait_for_script(&mut app, "redo", "x");

    app.quit();
}

#[test]
fn toggle_comment_on_selected_lines() {
    let mut app = launch_with_script("select 1;\n\n  select 2;");

    // Every line the selection touches; blank lines stay blank and the
    // marker goes after the indent.
    app.send_keys(&[Mod::Cmd], 'a');
    app.send_keys(&[Mod::Cmd], '/');
    wait_for_script(&mut app, "commented", "-- select 1;\n\n  -- select 2;");

    app.send_keys(&[Mod::Cmd], 'a');
    app.send_keys(&[Mod::Cmd], '/');
    wait_for_script(&mut app, "uncommented", "select 1;\n\n  select 2;");

    app.quit();
}

#[test]
fn copy_and_paste() {
    let mut app = launch_with_script("select 42;");

    harness::set_clipboard("e2e: nothing copied");
    app.send_keys(&[Mod::Cmd], 'a');
    app.send_keys(&[Mod::Cmd], 'c');
    app.wait_for(
        "the buffer on the clipboard",
        harness::REACT_TIMEOUT,
        |_| harness::clipboard() == "select 42;",
    );

    harness::set_clipboard("select 7;");
    app.send_keys(&[Mod::Cmd], 'a');
    app.send_keys(&[Mod::Cmd], 'v');
    wait_for_script(&mut app, "the pasted text", "select 7;");

    app.quit();
}

#[test]
fn format_script_lowercases_and_lays_out() {
    let original = "SELECT a,b FROM t WHERE a=1;";
    let mut app = launch_with_script(original);

    // The embedded language server starts in the background, and formatting
    // before it is up only reports that it is unavailable, so repeat the
    // shortcut until the text changes. Formatting formatted text is a no-op.
    let config = app.retry_until(
        "the formatted script",
        harness::DB_TIMEOUT,
        |app| app.send_keys(&[Mod::Cmd, Mod::Shift], 'f'),
        |config| harness::active_script(config).is_some_and(|script| script != original),
    );
    let formatted = harness::active_script(&config).unwrap_or_default();
    // Keywords in the default lower case, every name kept.
    assert!(
        !formatted.contains("SELECT") && formatted.contains("select"),
        "keywords lower-cased: {formatted:?}"
    );
    for name in ["a", "b", "t"] {
        assert!(
            formatted
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w == name),
            "{name:?} kept: {formatted:?}"
        );
    }

    app.quit();
}

#[test]
fn snippet_completion_inserts_user_snippet() {
    // A user snippet in the scratch config directory's `snippets/`; its
    // name is unlike any SQL keyword, so it is the only suggestion.
    let mut app = App::launch_prepared(|dir, _| {
        let snippets = dir.join("snippets");
        std::fs::create_dir_all(&snippets).expect("create snippets dir");
        std::fs::write(snippets.join("Zebra probe.sql"), "select 4242;").expect("write snippet");
    });

    // Typing opens the completion menu once the prefix is two characters;
    // Enter takes the first item. Without the menu the Enter is a newline,
    // so each attempt starts over from a selected buffer.
    app.retry_until(
        "the snippet inserted",
        harness::DB_TIMEOUT,
        |app| {
            // Shut a menu a previous attempt left open.
            app.press(&[], Key::Escape);
            app.send_keys(&[Mod::Cmd], 'a');
            app.type_text("zebr");
            // Completions arrive asynchronously, through the language
            // server's worker thread.
            std::thread::sleep(std::time::Duration::from_millis(1500));
            app.press(&[], Key::Return);
        },
        |config| harness::active_script(config) == Some("select 4242;"),
    );

    app.quit();
}

#[test]
fn save_writes_file_with_format_on_save() {
    let (mut app, path) = launch_with_file("select 1;\n", true);

    app.send_keys(&[Mod::Cmd], 'a');
    app.type_text("SELECT 2;");
    app.press(&[], Key::Escape);
    wait_for_script(&mut app, "the edit", "SELECT 2;");

    // The save formats first, then writes; the buffer takes the formatted
    // text too, so file and tab agree.
    app.send_keys(&[Mod::Cmd], 's');
    app.wait_for("the formatted file", harness::DB_TIMEOUT, |_| {
        std::fs::read_to_string(&path).is_ok_and(|text| text.contains("select 2"))
    });
    let on_disk = std::fs::read_to_string(&path).expect("read saved script");
    app.wait_for_config("the tab to match the file", |config| {
        harness::active_script(config) == Some(on_disk.as_str())
    });

    app.quit();
}

#[test]
fn save_writes_file_verbatim_without_format_on_save() {
    let (mut app, path) = launch_with_file("select 1;\n", false);

    app.send_keys(&[Mod::Cmd], 'a');
    app.type_text("SELECT   2;");
    app.press(&[], Key::Escape);
    wait_for_script(&mut app, "the edit", "SELECT   2;");

    app.send_keys(&[Mod::Cmd], 's');
    app.wait_for("the file written as typed", harness::REACT_TIMEOUT, |_| {
        std::fs::read_to_string(&path).is_ok_and(|text| text == "SELECT   2;")
    });

    app.quit();
}
