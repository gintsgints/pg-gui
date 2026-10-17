# UI end-to-end tests

These tests launch the real `pg-gui` binary and drive it the way a user would.
They press shortcuts, click menu items and quit with cmd-q, then check what the
app did. They run on macOS only. Most need no database; the few that do are
opt-in and run against one you have already started (see
[Database-backed scenarios](#database-backed-scenarios)).

```sh
cargo test --features e2e --test e2e
```

The test target is built only with the `e2e` Cargo feature (`required-features`
in `Cargo.toml`), because the tests take over the keyboard focus. Without the
feature, `cargo test` skips the target completely: it is not built, not run and
not listed. That includes `cargo test -- --ignored`, so these tests never run
alongside the database-backed tests in `src/`. The feature changes nothing in
the app itself.

## Before the first run

- **Grant Accessibility permission** to the app you run `cargo test` from
  (Terminal, iTerm, your IDE). It is under System Settings ▸ Privacy & Security
  ▸ Accessibility. Without it, every test fails with an `osascript` error that
  includes a hint about this permission. After you grant it, restart the
  terminal app.
- **Leave the machine alone while the tests run** (about 40 s for the full
  set). Keystrokes go to whatever app is frontmost, so if you type or click
  somewhere else, the keystrokes land there.

## Running

```sh
cargo test --features e2e --test e2e                          # all scenarios
cargo test --features e2e --test e2e new_tab_and_close_tab    # one scenario
cargo test --features e2e --test e2e files_panel::            # one module
cargo test --features e2e --test e2e -- --nocapture           # show harness output
cargo test --features e2e --test e2e -- --list                # list scenarios
PG_GUI_E2E_DATABASE=docker cargo test --features e2e --test e2e   # include database scenarios
```

Cargo builds the binary before the tests run, so the tests always drive the
current code. The tests run one at a time even without `--test-threads=1`,
because they share one screen and one menu bar. A mutex in the harness takes
care of that.

Leaving out `--test e2e` (`cargo test --features e2e`) also runs the unit
tests, which is harmless but slower.

## Scenarios

`main.rs` only declares the modules. Each kind of scenario lives in its own
file, and a test's full name is `module::test`.

### `general`: the app as a whole

| Test | What it checks |
|---|---|
| `launches_with_main_window_and_menus` | The window titled `pg-gui` appears, the main menus are present, and cmd-q exits cleanly. |
| `new_tab_and_close_tab` | cmd-t adds a tab and makes it active; cmd-w on that blank tab closes it with no prompt. |
| `panel_shortcuts_toggle_panels` | cmd-1, cmd-2 and cmd-3 each flip the database, files and results panels, and pressing again flips them back. |
| `format_on_save_menu_toggles_check_mark` | Clicking Edit ▸ Format on Save flips the menu's check mark and the stored `format_on_save` setting. |

### `files_panel`: scripts in the working folder

| Test | What it checks |
|---|---|
| `new_script_from_header` | The files panel's header "+" opens the save panel; the typed name is created in the working folder and opened in the active tab. |
| `filter_finds_nested_script` | Typing in the files filter reduces the tree to a script inside collapsed folders (plus its expanded parents); clicking its row opens it. |
| `rename_from_context_menu` | Filter to one file, right-click ▸ Rename…, type a new name: the file is renamed on disk (`.sql` forced back on), neighbours untouched. |
| `delete_from_context_menu` | Filter to one file, right-click ▸ Delete…, Enter confirms: only that file is removed. |

### `query`: running SQL (needs a database)

| Test | What it checks |
|---|---|
| `run_query_executes_against_database` | A tab holding `create table …`; cmd-enter runs it, and the table appears in the database. |

### `db_viewer`: the database panel (needs a database)

Each scenario creates one table (with a primary key), one view and one
function in its scratch schema. It then walks the panel's tree to one of them
and clicks it, and checks the definition that opens in the active tab.

| Test | Path in the tree | The tab holds |
|---|---|---|
| `opens_table_definition` | schema ▸ Tables ▸ `orders` ▸ definition | `CREATE TABLE` with the column and the primary key. The blank seeded tab is reused, not a second one opened. |
| `opens_view_definition` | schema ▸ Views ▸ `orders_view` | `CREATE OR REPLACE VIEW "orders_view"`. |
| `opens_function_definition` | schema ▸ Functions ▸ `answer()` | `CREATE OR REPLACE FUNCTION` with the body. |
| `opens_index_definition` | schema ▸ Tables ▸ `orders` ▸ Indexes ▸ `orders_pkey` | `CREATE UNIQUE INDEX orders_pkey`. |

The tree cannot be read from outside the window, so rows are addressed by
position. That works because the scratch schema is always the first row: its
name, `_e2e_<pid>_<n>`, sorts before every lowercase schema name. A schema
whose name starts with an upper-case letter or punctuation would sort ahead of
it and throw the positions off.

A folder loads its objects in the background the first time it is expanded,
and nothing outside the window shows when that load is done. So each scenario
repeats one attempt with `retry_until` until the definition shows up. The
attempt expands every folder on the path, then clicks the object row.
`expand_db_row` clicks the row and then presses Right, which expands only a
shut folder, so it is safe to repeat. A click that lands on a loading
placeholder or on a folder further down cannot move the rows above it, so the
next attempt still finds them.

## Database-backed scenarios

The `query` and `db_viewer` scenarios need a running PostgreSQL. The harness never starts
or provisions one; it uses a database that already exists. Choose it with
`PG_GUI_E2E_DATABASE`:

| Value | Database |
|---|---|
| unset or empty | None. The database scenarios print a "skipped" line and pass. |
| `docker` | The project's `docker compose` server: `postgres://pgui:pgui@127.0.0.1:5433/pgui_test`. |
| any other value | Used as the connection string. |

With the project database:

```sh
docker compose up -d
PG_GUI_E2E_DATABASE=docker cargo test --features e2e --test e2e
```

When the variable is set, an unreachable server fails the scenario instead of
skipping it, because you asked for those scenarios to run.

Each scenario works in a scratch schema of its own, `harness::Schema`. It
creates `_e2e_<pid>_<n>` on a connection of the test's own, and drops it with
`CASCADE` when the scenario ends, pass or fail. Everything the scenario creates
goes inside that schema, so runs never collide and the project's own objects
are never touched. Only one scratch schema exists at a time: the
database scenarios run one after another, and each drops any `_e2e_*` schema
an interrupted earlier run (ctrl-c) left behind before creating its own. Two
at once would both be listed in the panel, and the wrong one could take the
first row. The same connection checks what the app did, since query
output cannot be read from outside the window.
`App::launch_with_database(&schema, script)` starts the app connected to that
database, saved under the name `e2e`, with one tab holding `script`. The
database panel is shown and pinned to `DB_PANEL_WIDTH`, and the files panel is
hidden.

## Isolation

Each launch gets its own scratch config directory at
`$TMPDIR/pg-gui-e2e-<pid>-<n>`, passed to the app as `PG_GUI_CONFIG_DIR`. As a
result:

- Your own `config.json`, snippets and log are never read or changed.
- A pg-gui you already have open is left alone. Its single-instance socket lives
  in the real config directory, so the test instance does not find it.
- No database server is contacted unless you opt in with
  `PG_GUI_E2E_DATABASE`. The seeded `config.json` points at `127.0.0.1:1`,
  where nothing listens, so connection attempts fail immediately.

When a test passes, its directory is removed.

## When a test fails

The failure message names the condition that timed out and shows the last
`config.json` it read. The test output also gives the path of the scratch
directory, which is kept and holds:

| File | Contents |
|---|---|
| `failure.png` | A capture of the app window at the moment of failure. |
| `stdio.log` | The app's stdout and stderr. |
| `pg-gui.log` | The app's diagnostics log (`PG_GUI_LOG=warn`). |
| `config.json` | The last state the app saved. |

Delete these directories when you are done with them; nothing else cleans them
up.

## How it works

The harness (`harness.rs`) talks to the app through `osascript` and System
Events. `keystroke` sends key presses to the frontmost app, and System Events'
accessibility bridge reads and clicks the menu bar. It needs no extra crates.

GPUI draws everything inside the window itself and publishes no accessibility
tree for it. To macOS Accessibility, the window contains only the three
traffic-light buttons. The tab bar, editor, status bar and results grid
**cannot be read from outside**, so the tests check these instead:

- **The menu bar.** It is a native `NSMenu`. gpui puts a check mark into the
  item title as a `✓ ` prefix, which `menu_checked` looks for.
- **Window titles.**
- **`config.json`.** The app rewrites it whenever a tab, panel or toggle
  changes, so it shows the state of everything drawn inside the window. Tests
  wait on it with `wait_for_config`.
- **The file system.** The files-panel scenarios check the working folder
  itself: what was created, renamed or deleted.

Input inside the window has nothing to address but a position. `click_at`
posts `CoreGraphics` mouse events from JXA (System Events' `click at` cannot
right-click), moving the pointer there first so hover state settles. The
side panels are reached at fixed offsets measured from a capture of the files
panel (`PANEL_*` constants in `harness.rs`). The database panel is built the
same way (header row, small filter box, tree rows of the same height), so it
uses the same offsets. They only hold because `launch_with_folder` hides the
database panel and pins the files panel to `FILES_PANEL_WIDTH`,
`launch_with_database` hides the files panel and pins the database panel to
`DB_PANEL_WIDTH`, and the zoom stays at 100%. To hit a row regardless of
what else the folder holds, the scenarios filter the tree down first, so the
target is a known row. In-window menus and dialogs are then driven by
keyboard: gpui-component's popup menu takes Up/Down/Enter (Up with nothing
selected wraps to the last item), and its dialogs confirm on Enter. If the
panel's layout changes, re-measure from a `screenshot` and update the
constants.

## Writing a test

Add a function to the module for its kind, or to a new module declared in
`main.rs`:

```rust
#[test]
fn my_scenario() {
    let mut app = App::launch();               // or App::launch_with(&seed)

    app.send_keys(&[Mod::Cmd], 't');           // press a shortcut
    app.wait_for_config("a second tab", |c| {  // poll until the state shows it
        c["tabs"].as_array().map_or(0, Vec::len) == 2
    });

    app.click_menu("Edit", "Format on Save");  // click a menu item
    assert!(!app.menu_checked("Edit", "Format on Save"));

    assert!(app.quit().success());             // cmd-q and wait for exit
}
```

Harness API:

| Call | Purpose |
|---|---|
| `App::launch()` / `App::launch_with(&Value)` | Starts the app on a scratch config seeded with `default_seed()` or your own JSON, and waits for the window. |
| `Schema::create(test)` | Creates the scenario's scratch schema, or returns `None` (the scenario returns early) when `PG_GUI_E2E_DATABASE` is unset. `name()`, `execute(sql)` (`{schema}` is replaced by the name), `table_exists(table)`. Dropped with `CASCADE` on drop. |
| `App::launch_with_database(&Schema, script)` | Starts the app connected to the schema's database, the database panel showing, with one tab holding `script`. |
| `click_db_row(Button, row)`, `expand_db_row(row)` | Database panel: click tree row `row` (0 = top); make sure folder row `row` is expanded (safe to repeat). |
| `retry_until(what, timeout, attempt, pred)` | Repeats `attempt` until `config.json` satisfies `pred`, for steps nothing outside the window reports on. Each attempt must be safe to repeat. |
| `harness::active_script(&config)` | Returns the active tab's text. |
| `App::launch_with_folder(&[&str])` | Starts the app with the files panel on a fresh working folder holding those (empty) files; a path ending in `/` is a folder. `work_dir()` returns it. |
| `send_keys(&[Mod], char)` | Brings the app to the front and types the key with modifiers held. |
| `type_text(str)`, `press(&[Mod], Key)` | Types text into the focused element; presses a non-printing key (`Key::Return`, `Key::Up`). |
| `click_at(Button, x, y)` | Left- or right-clicks at points from the window's top-left corner. |
| `click_files_new_script()`, `filter_files(text)`, `click_files_row(Button, row)` | Files panel: the header "+", the filter box (text replaced), tree row `row` (0 = top). |
| `wait_for_extra_window(what)` | Waits for a second window, such as a save panel. |
| `click_menu(menu, item)` | Clicks a menu item. The item matches with or without its check mark. |
| `menu_checked(menu, item)` | Returns whether the item carries a check mark. |
| `menu_bar()`, `menu_items(menu)`, `window_titles()` | Return the titles shown by the native UI. |
| `config()` | Returns the `config.json` the app last wrote, as `serde_json::Value`. |
| `wait_for_config(what, pred)` / `wait_for(what, timeout, cond)` | Polls until the condition holds. On timeout it panics with `what` and the last config. |
| `screenshot(path)` | Captures the window as a PNG. |
| `quit()` | Sends cmd-q and returns the exit status. |

Guidelines:

- **Always wait; never sleep.** The app reacts asynchronously, so assert with
  `wait_for_config` or `wait_for`. The default timeout is `REACT_TIMEOUT` (5 s).
- **Seed the state you need** with `launch_with` (tabs, panel flags, settings)
  instead of building it up with key presses.
- **End with `quit()`** when the scenario should also prove a clean exit.
  Otherwise the drop handler kills the process.
- **Database scenarios start with `Schema::create`** and return when it gives
  `None`. Create their objects inside the schema, never elsewhere.
- **Only `Mod::Cmd` exists for now.** When a scenario needs another modifier,
  add the variant and its System Events name in `Mod::applescript`. Clippy
  rejects a variant that nothing uses.
- `#![cfg(target_os = "macos")]` in `main.rs` makes the target empty on other
  platforms.
- **Lint with the feature on.** The project's clippy command does not see this
  target unless the feature is enabled:
  `cargo clippy --all-targets --features e2e -- -W clippy::pedantic -D warnings`.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `osascript failed: … not allowed assistive access` (`-25211`) | The terminal has no Accessibility permission. Grant it, then restart the terminal. |
| `timed out … waiting for app to become frontmost` | Another app keeps focus, such as a modal dialog or a full-screen app. Close it and rerun. |
| `pg-gui exited early with …` | The app crashed or refused to start. Check `stdio.log` in the kept directory. |
| Keys typed into another window | You used the keyboard or mouse during the run. Rerun and leave the machine alone. |

## Not covered yet

- **Reading query output** (the results grid, the log, the plan). It cannot be
  read from outside the window, so database scenarios check side effects in the
  database instead. Checking the output itself would need OCR on `screenshot`,
  or a debug-only state dump the app writes on request.
- **The theme-switch scenario.**
- **CI.** A runner would need Accessibility permission for the terminal process.
  GitHub-hosted macOS runners do not grant it by default, so check whether they
  allow it before adding a workflow.
