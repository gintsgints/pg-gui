//! Exporting a result set: Connection ▸ Export as CSV… re-runs the statement
//! under the cursor through `COPY … TO STDOUT` and writes it to the file the
//! save panel names. Needs `PG_GUI_E2E_DATABASE`; skipped without it.

use std::path::PathBuf;

use crate::harness::{self, App, Key, Mod, Schema};

#[test]
fn export_csv_writes_result_set() {
    let Some(mut schema) = Schema::create("export::export_csv_writes_result_set") else {
        return;
    };
    // Values CSV has to quote or leave empty: a comma, a double quote, a
    // NULL.
    schema.execute(
        "create table {schema}.items (id int, label text);
         insert into {schema}.items values
             (1, 'plain'), (2, 'with, comma'), (3, 'say \"hi\"'), (4, null);",
    );
    let script = format!("select id, label from {}.items order by id;", schema.name());
    let mut out = PathBuf::new();
    let mut app = App::launch_prepared(|dir, seed| {
        *seed = harness::database_seed(schema.url(), &script);
        // The save panel opens on the last folder a file dialog used.
        let dir = dir.canonicalize().expect("canonicalize scratch dir");
        seed["last_dir"] = serde_json::Value::from(dir.to_str().expect("utf-8 temp path"));
        out = dir.join("exported.csv");
    });

    // The panel's name field starts focused, holding a suggested name.
    app.click_menu("Connection", "Export as CSV…");
    app.wait_for_extra_window("the export save panel");
    app.send_keys(&[Mod::Cmd], 'a');
    app.type_text("exported.csv");
    app.press(&[], Key::Return);

    let expected = "id,label\n1,plain\n2,\"with, comma\"\n3,\"say \"\"hi\"\"\"\n4,\n";
    app.wait_for("the exported file", harness::DB_TIMEOUT, |_| {
        std::fs::read_to_string(&out).is_ok_and(|csv| csv == expected)
    });

    app.quit();
}
