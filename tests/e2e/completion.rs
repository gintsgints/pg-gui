//! Completions in the SQL editor: database object names from the embedded
//! language server's schema cache, and snippets matched by the words typed.
//! The object scenarios need `PG_GUI_E2E_DATABASE`; the snippet one does
//! not.
//!
//! The completion menu cannot be read from outside the window, so each
//! scenario types a prefix, waits for the menu, and takes its first item
//! with Enter, then reads the buffer back from the tab's `script`. The
//! server loads the schema in the background and completions arrive
//! asynchronously, and without a menu the Enter is only a newline, so every
//! attempt starts over from a selected buffer and repeats until the text
//! shows the completion. Typed text avoids quotes and brackets, which the
//! editor may pair on its own.

use std::time::Duration;

use crate::harness::{self, App, Key, Mod, Schema};

/// How long a typed prefix gets for its completions to arrive.
const MENU_WAIT: Duration = Duration::from_millis(1500);

/// Replace the buffer with `typed`, wait for the completion menu, and take
/// its first item — repeatedly, until the active tab satisfies `done`.
fn complete_until(app: &mut App, what: &str, typed: &str, done: impl Fn(&str) -> bool) {
    app.retry_until(
        what,
        harness::DB_TIMEOUT,
        |app| {
            // Shut a menu a previous attempt left open.
            app.press(&[], Key::Escape);
            app.send_keys(&[Mod::Cmd], 'a');
            app.type_text(typed);
            std::thread::sleep(MENU_WAIT);
            app.press(&[], Key::Return);
        },
        |config| harness::active_script(config).is_some_and(&done),
    );
}

/// A table whose name and columns start with a prefix nothing else in the
/// database shares, so it is the first completion for it.
fn create_zebra_table(schema: &mut Schema) {
    schema.execute("create table {schema}.zebra_orders (id int, zebra_total numeric)");
}

#[test]
fn completes_table_name() {
    let Some(mut schema) = Schema::create("completion::completes_table_name") else {
        return;
    };
    create_zebra_table(&mut schema);
    let mut app = App::launch_with_database(&schema, "");

    let prefix = format!("select * from {}.", schema.name());
    let expected = format!("{prefix}zebra_orders");
    complete_until(
        &mut app,
        "the table name completed",
        &format!("{prefix}zebra_o"),
        |script| script.trim_end() == expected,
    );

    app.quit();
}

#[test]
fn completes_column_name() {
    let Some(mut schema) = Schema::create("completion::completes_column_name") else {
        return;
    };
    create_zebra_table(&mut schema);
    let mut app = App::launch_with_database(&schema, "");

    // The FROM clause tells the server whose columns to offer.
    let prefix = format!("select * from {}.zebra_orders where ", schema.name());
    let expected = format!("{prefix}zebra_total");
    complete_until(
        &mut app,
        "the column name completed",
        &format!("{prefix}zebra_to"),
        |script| script.trim_end() == expected,
    );

    app.quit();
}

#[test]
fn completes_builtin_snippet_from_its_name() {
    // No database: the snippet suggestions come first in the menu, ahead
    // of anything the server offers.
    let mut app = App::launch();

    complete_until(
        &mut app,
        "the built-in snippet inserted",
        "largest tables",
        |script| script.contains("ORDER BY pg_total_relation_size(c.oid) DESC"),
    );

    app.quit();
}
