//! Exporting a result set: Connection ▸ Export as CSV… and Export as
//! INSERT… re-run the statement under the cursor and write its result to
//! the file the save panel names. Needs `PG_GUI_E2E_DATABASE`; skipped
//! without it.

use std::path::{Path, PathBuf};

use crate::harness::{self, App, Key, Mod, Schema};

/// Create `items` with values an export has to quote or spell out — a
/// comma, both kinds of quote, a NULL — and start the app with a tab
/// selecting them, its save panels opening on the scratch directory.
/// Returns the app and where a file named `file` lands there.
fn launch(schema: &mut Schema, file: &str) -> (App, PathBuf) {
    schema.execute(
        "create table {schema}.items (id int, label text);
         insert into {schema}.items values
             (1, 'with, comma'), (2, 'say \"hi\"'), (3, 'O''Brien'), (4, null);",
    );
    let script = format!("select id, label from {}.items order by id;", schema.name());
    let mut out = PathBuf::new();
    let app = App::launch_prepared(|dir, seed| {
        *seed = harness::database_seed(schema.url(), &script);
        // A save panel opens on the last folder a file dialog used.
        let dir = dir.canonicalize().expect("canonicalize scratch dir");
        seed["last_dir"] = serde_json::Value::from(dir.to_str().expect("utf-8 temp path"));
        out = dir.join(file);
    });
    (app, out)
}

/// Click Connection ▸ `item` and save as `file` in the panel it opens, whose
/// name field starts focused, holding a suggested name.
fn export_as(app: &mut App, item: &str, file: &str) {
    app.click_menu("Connection", item);
    app.wait_for_extra_window("the export save panel");
    app.send_keys(&[Mod::Cmd], 'a');
    app.type_text(file);
    app.press(&[], Key::Return);
}

/// Wait until the file at `path` holds exactly `expected`.
fn wait_for_file(app: &mut App, path: &Path, expected: &str) {
    app.wait_for("the exported file", harness::DB_TIMEOUT, |_| {
        std::fs::read_to_string(path).is_ok_and(|text| text == expected)
    });
}

#[test]
fn export_csv_writes_result_set() {
    let Some(mut schema) = Schema::create("export::export_csv_writes_result_set") else {
        return;
    };
    let (mut app, out) = launch(&mut schema, "exported.csv");

    export_as(&mut app, "Export as CSV…", "exported.csv");
    // A header line, then a line per row: the comma and the double quote
    // quoted (the quote doubled), the NULL left empty.
    wait_for_file(
        &mut app,
        &out,
        "id,label\n\
         1,\"with, comma\"\n\
         2,\"say \"\"hi\"\"\"\n\
         3,O'Brien\n\
         4,\n",
    );

    app.quit();
}

#[test]
fn export_inserts_writes_replayable_script() {
    let Some(mut schema) = Schema::create("export::export_inserts_writes_replayable_script") else {
        return;
    };
    let (mut app, out) = launch(&mut schema, "exported.sql");
    let table = format!("{}.items", schema.name());

    export_as(&mut app, "Export as INSERT…", "exported.sql");
    // One INSERT per row into the query's table, every value a literal
    // (the single quote doubled), the NULL spelled out.
    let script = format!(
        "INSERT INTO {table} (\"id\", \"label\") VALUES ('1', 'with, comma');\n\
         INSERT INTO {table} (\"id\", \"label\") VALUES ('2', 'say \"hi\"');\n\
         INSERT INTO {table} (\"id\", \"label\") VALUES ('3', 'O''Brien');\n\
         INSERT INTO {table} (\"id\", \"label\") VALUES ('4', NULL);\n"
    );
    wait_for_file(&mut app, &out, &script);

    // And it replays: emptied, the table gets every row back from it.
    schema.execute("delete from {schema}.items");
    schema.execute(&script);
    assert_eq!(schema.row_count("items"), 4, "the script restores the rows");

    app.quit();
}
