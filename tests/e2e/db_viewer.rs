//! The database panel (object browser): browsing the scratch schema's
//! folders and opening an object's definition in a tab. Needs
//! `PG_GUI_E2E_DATABASE`; skipped without it.
//!
//! The tree cannot be read from outside the window, so each scenario walks
//! to a row it knows by position and checks the tab the click opened, in
//! config.json. Positions are fixed because the scratch schema is the
//! panel's first row (see [`Schema`]) and holds one object of each kind,
//! so under it the rows are:
//!
//! ```text
//! 0 _e2e_<pid>_<n>
//! 1   Tables
//! 2     orders            (once Tables has loaded)
//! 3       definition
//! 4       Columns
//! 5       Indexes
//! 6         orders_pkey   (once Indexes has loaded)
//! ```
//!
//! and with Tables shut, Views is row 2 and Functions row 4, each with its
//! one object right below.
//!
//! A folder loads its objects in the background when first expanded, and
//! nothing outside the window says when that is done, so the walk is one
//! repeatable attempt ([`App::retry_until`]): every folder on the path is
//! made expanded ([`App::expand_db_row`], a no-op when it already is) and
//! the object row clicked, until the definition shows up in the active tab.
//! A click that lands on a placeholder or on a folder below the path while a
//! load is still running cannot shift the rows above it, so the next attempt
//! still finds them.

use serde_json::Value;

use crate::harness::{self, App, Button, Schema};

/// The objects every scenario browses: one table (with a primary key, so it
/// has an index), one view and one function.
const FIXTURE: &str = "
    create table {schema}.orders (id int primary key, note text not null);
    create view {schema}.orders_view as select id from {schema}.orders;
    create function {schema}.answer() returns int language sql as 'select 42';
";

/// Create the scratch schema with [`FIXTURE`] in it and launch the app on
/// its database, or `None` when the database scenarios are off.
fn launch(test: &str) -> Option<(Schema, App)> {
    let mut schema = Schema::create(test)?;
    schema.execute(FIXTURE);
    let app = App::launch_with_database(&schema, "");
    Some((schema, app))
}

/// Expand the folders at `path` in order, then click the object row
/// `object`, until the active tab holds text containing every one of
/// `expected`.
fn open_definition(app: &mut App, path: &[u32], object: u32, expected: &[&str]) -> Value {
    let what = format!("the definition holding {expected:?}");
    app.retry_until(
        &what,
        harness::DB_TIMEOUT,
        |app| {
            for &row in path {
                app.expand_db_row(row);
            }
            app.click_db_row(Button::Left, object);
        },
        |config| {
            harness::active_script(config)
                .is_some_and(|script| expected.iter().all(|text| script.contains(text)))
        },
    )
}

#[test]
fn opens_table_definition() {
    let Some((_schema, mut app)) = launch("db_viewer::opens_table_definition") else {
        return;
    };

    // Schema, Tables, the table; its first child is the DDL leaf.
    let config = open_definition(
        &mut app,
        &[0, 1, 2],
        3,
        &[
            "CREATE TABLE",
            // The DDL quotes every identifier.
            "\"orders\"",
            "\"note\" text NOT NULL",
            "PRIMARY KEY (id)",
        ],
    );
    // The blank seeded tab is reused rather than a second one opened.
    assert_eq!(config["tabs"].as_array().map_or(0, Vec::len), 1);

    app.quit();
}

#[test]
fn opens_view_definition() {
    let Some((_schema, mut app)) = launch("db_viewer::opens_view_definition") else {
        return;
    };

    open_definition(
        &mut app,
        &[0, 2],
        3,
        &["CREATE OR REPLACE VIEW", "\"orders_view\"", "SELECT"],
    );

    app.quit();
}

#[test]
fn opens_function_definition() {
    let Some((_schema, mut app)) = launch("db_viewer::opens_function_definition") else {
        return;
    };

    open_definition(
        &mut app,
        &[0, 4],
        5,
        &["CREATE OR REPLACE FUNCTION", "answer()", "select 42"],
    );

    app.quit();
}

#[test]
fn opens_index_definition() {
    let Some((_schema, mut app)) = launch("db_viewer::opens_index_definition") else {
        return;
    };

    // Schema, Tables, the table, its Indexes folder; the primary key's
    // index is the one entry.
    open_definition(
        &mut app,
        &[0, 1, 2, 5],
        6,
        &["CREATE UNIQUE INDEX", "orders_pkey"],
    );

    app.quit();
}
