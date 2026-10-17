# UI end-to-end tests

These tests launch the real `pg-gui` binary and drive it the way a user would.
They press shortcuts, click menu items and quit with cmd-q, then check what the
app did. They run on macOS only. Most need no database; the few that do are
opt-in and run against one you have already started (see
[Database-backed scenarios](#database-backed-scenarios)).

```sh
cargo e2e
```

`cargo e2e` is an alias (`.cargo/config.toml`) for
`cargo test --features e2e --test e2e -- --test-threads=1`. The test target is
built only with the `e2e` Cargo feature (`required-features` in `Cargo.toml`),
because the tests take over the keyboard focus. Without the
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
- **Leave the machine alone while the tests run** (about a minute for the
  full set, about two with the database scenarios). Keystrokes go to whatever app is frontmost, so if you type or click
  somewhere else, the keystrokes land there.

## Running

```sh
cargo e2e                                # all scenarios
cargo e2e new_tab_and_close_tab          # one scenario
cargo e2e files_panel::                  # one module
cargo e2e --nocapture                    # show harness output
cargo e2e --list                         # list scenarios
PG_GUI_E2E_DATABASE=docker cargo e2e     # include database scenarios
```

The alias already ends with `--`, so filters and test flags (`--nocapture`,
`--list`, `--exact`) go straight after `cargo e2e`, without another `--`.

Cargo builds the binary before the tests run, so the tests always drive the
current code. The tests run one at a time even without `--test-threads=1`,
because they share one screen and one menu bar. A mutex in the harness takes
care of that. The alias still passes `--test-threads=1`: with several threads,
tests queue on that mutex while libtest's clock runs, and its hard-coded
"has been running for over 60 seconds" notice fires for tests that are only
waiting their turn. The total time is the same either way.

Running cargo directly works too. Leaving out `--test e2e`
(`cargo test --features e2e`) also runs the unit tests, which is harmless but
slower.

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
| `theme_menu_switches_theme` | View ▸ Theme ▸ Dark, Light and System each move the check mark to themselves alone and store `theme`; Dark paints the window dark and Light paints it light, read off a pixel of a window capture. |

### `completion`: completions in the editor

Database object names come from the embedded language server's schema cache,
which loads in the background once the app is connected; snippets are matched
against the words typed and come first in the menu. The menu cannot be read
from outside the window, so each scenario replaces the buffer with a prefix,
waits for the menu and takes its first item with Enter, then reads the buffer
back. Without a menu the Enter is only a newline, so every attempt starts over
and repeats (`retry_until`) until the completion shows. The object scenarios
need a database and create a `zebra_orders` table with a `zebra_total` column,
a prefix nothing else in the database shares, so it is the first item; the
snippet one needs none.

| Test | What it checks |
|---|---|
| `completes_table_name` | `select * from <schema>.zebra_o` completes to the table `zebra_orders`. |
| `completes_column_name` | `select * from <schema>.zebra_orders where zebra_to` completes to the column `zebra_total`, offered from the FROM clause's table. |
| `completes_builtin_snippet_from_its_name` | No database: typing `largest tables` offers the built-in "Size: largest tables" snippet, and Enter inserts its SQL. |

### `connections`: saved connections and switching

No database needed: both connections point at `127.0.0.1:1`, where nothing
listens, which is all a switch needs. Untitled tabs belong to the connection,
so switching parks them in `config.json`'s `parked_tabs` under the outgoing
connection's name and brings back the incoming one's; the scenarios check
both lists. Connection ▸ Recent is a native submenu, clicked with
`click_submenu`. The New Connection dialog's fields have no tab order, so
`fill_connection_dialog` clicks the Name and Connection string fields and the
Connect button at offsets measured from a capture (`ConnectionDialog` in
`harness.rs`): from the window's centre horizontally, and from the dialog's
top edge, which gpui-component puts a tenth of the window's height down.

| Test | What it checks |
|---|---|
| `recent_menu_switches_untitled_tabs` | With an untitled and a file tab on `first`, Recent ▸ `second` parks the untitled tab and keeps the file tab; a tab typed on `second` is parked when switching back, `first`'s returns to its old slot, and switching again restores `second`'s. |
| `new_connection_dialog_creates_and_switches` | New Connection… with a name and connection string saves and selects it on a blank tab, parking `first`'s script; Recent ▸ `first` brings the script back. |

### `editor`: the SQL editor

No database needed. The editor cannot be read from outside the window, but the
app copies every buffer change into the tab's `script` in `config.json` about
half a second later, so the scenarios wait on that. Copy is checked through the
clipboard and save through the file on disk. `App::launch_prepared` gives a
scenario the scratch directory before launch, to seed a script file or a user
snippet under `snippets/`. Typed text avoids quotes and brackets, which the
editor may pair on its own.

| Test | What it checks |
|---|---|
| `typing_updates_tab_script` | Typed text lands in the active tab. |
| `undo_and_redo` | cmd-a and one typed character replace the buffer; cmd-z brings the old text back, cmd-shift-z the new. |
| `toggle_comment_on_selected_lines` | cmd-a then cmd-/ puts `-- ` on every line (after the indent, blank lines left alone); again takes it off. |
| `copy_and_paste` | cmd-a, cmd-c puts the buffer on the clipboard; cmd-a, cmd-v replaces it with the clipboard. |
| `format_script_lowercases_and_lays_out` | cmd-shift-f formats through the embedded language server: keywords lower-cased, every name kept. The shortcut repeats until the server is up. |
| `snippet_completion_inserts_user_snippet` | Typing the start of a user snippet's name opens the completion menu, and Enter inserts the snippet's SQL. |
| `save_writes_file_with_format_on_save` | On a file tab, cmd-s formats and writes; the file holds the formatted text and the tab matches it. |
| `save_writes_file_verbatim_without_format_on_save` | With format-on-save off, cmd-s writes exactly what was typed. |

### `format`: the SQL formatter

No database needed. Formatting goes through the embedded language server,
which starts in the background; a format sent before it is up only reports
that it is unavailable, so each scenario clicks Edit ▸ Format until the buffer
changes (formatting formatted text is a no-op) and then compares the exact
text. The casing settings are seeded into `config.json`. The cmd-shift-f
shortcut and format-on-save are covered in `editor`.

| Test | What it checks |
|---|---|
| `format_menu_formats_every_statement` | Two statements are both spaced (`a, b`, `a = 1`) and lower-cased, the string literal left as it was. |
| `upper_keywords_lower_constants` | `keyword_case` upper, `constant_case` lower: keywords (`IS NULL` among them) upper-cased, `TRUE` lower-cased, `'X'` untouched. |
| `lower_keywords_upper_constants` | The other way round: keywords lower-cased, `TRUE` upper-cased. |

### `snippets`: the snippet picker

No database needed. Edit ▸ Snippets (cmd-p) opens a searchable picker over the
built-in snippets and the user's own `.sql` files under `snippets/` in the
config directory; `App::pick_snippet(filter)` opens it, types the filter and
inserts the first match with Enter. User snippets are written into the scratch
config directory before launch, and the buffer is read back through the tab's
`script`, as in `editor`.

| Test | What it checks |
|---|---|
| `picker_inserts_user_snippet_as_own_statement` | A user snippet picked with the cursor at the start of `select 1;` lands there on a line of its own. |
| `filter_snippet_leaves_caret_between_percents` | A built-in whose filter defaults to `ILIKE '%%'` leaves the caret between the two `%`, so typed text narrows the filter. |
| `tab_stops_take_typed_values_in_order` | A user snippet with `${1:columns}` and `${2:source}`: the first marker turns into its selected placeholder and typing replaces it, the second stays a marker until Tab reaches it, and the result reads `select id from orders;`. |

### `editor_syntax`: SQL syntax checking

The embedded language server's diagnostics, reached the way a user reaches
them: Next Problem (cmd-j) leaves the cursor on the start of the problem it
lands on, so the scenario types a `#` there and reads the tab's `script` back.
With no problem the cursor stays put and the `#` lands where it was. Every walk
starts from the buffer's end (cmd-a, Right), where the first press wraps round
to the first error. Diagnostics are published on a debounce, and a press before
they arrive finds nothing, so each scenario first waits for the
`server diagnostics count=N` line the app logs per published run
(`App::log`). The offline scenarios run on a connection string the driver
cannot parse, which the server takes as "no database": it still parses and
lints, without spending a run on a failed connection attempt.

| Test | What it checks |
|---|---|
| `valid_script_reports_no_problems` | Two valid statements: the run reports nothing and cmd-j leaves the cursor where it was. |
| `syntax_error_is_located_at_its_statement` | A broken statement between two valid ones: cmd-j lands on the start of the broken one. |
| `next_problem_walks_every_error` | Two broken statements around a valid one: the first press lands on the first, the second on the next, past the valid one. |
| `typed_error_is_flagged_and_its_fix_clears_it` | Typing a broken statement gets it flagged; replacing it with a valid one clears it. |
| `syntax_error_is_flagged_with_database_connected` | Database needed: with the schema cache loaded, a syntax error is still flagged and a valid query on an existing table is not. |

### `files_panel`: scripts in the working folder

| Test | What it checks |
|---|---|
| `new_script_from_header` | The files panel's header "+" opens the save panel; the typed name is created in the working folder and opened in the active tab. |
| `filter_finds_nested_script` | Typing in the files filter reduces the tree to a script inside collapsed folders (plus its expanded parents); clicking its row opens it. |
| `rename_from_context_menu` | Filter to one file, right-click ▸ Rename…, type a new name: the file is renamed on disk (`.sql` forced back on), neighbours untouched. |
| `delete_from_context_menu` | Filter to one file, right-click ▸ Delete…, Enter confirms: only that file is removed. |

### `hover`: hover in the editor (needs a database)

Resting the pointer on a name asks the embedded language server about it,
which answers from its schema cache. The popup cannot be read from outside the
window, but the app logs every answer (`server hover blocks=N`, the number of
markdown blocks returned) to `pg-gui.log`, which `App::log` reads.
`App::hover_text(row, col)` moves the pointer onto an editor character (from a
blank spot further right, so the editor always sees a fresh move), on the
`launch_for_results` layout `cmd_click_text` uses. The cache loads in the
background, so the scenarios re-hover until an answer with content arrives.

| Test | What it checks |
|---|---|
| `hover_on_table_name_answers` | Hovering the table in `select zebra_total from <schema>.zebra_orders;` gets an answer with content. |
| `hover_on_column_name_answers` | Hovering the column `zebra_total` gets an answer with content. |
| `hover_on_keyword_answers_nothing` | Once the table has been answered (so the cache is loaded), hovering `select` gets only empty answers. |

### `query`: running SQL (needs a database)

| Test | What it checks |
|---|---|
| `run_query_executes_against_database` | A tab holding `create table …`; cmd-enter runs it, and the table appears in the database. |
| `cancel_query_stops_running_statement` | `insert … select 1 from pg_sleep(60)` runs (the app's backend is `active` on it); Session ▸ Cancel Query puts the backend back to `idle` within seconds, and the row never lands. |
| `run_script_from_files_panel_menu` | Two scripts in the working folder; filter to one, right-click ▸ Run Script, Enter confirms: its table is created and holds the three rows it inserts, and the other script's table does not exist. |
| `run_selected_scripts_in_tree_order` | Four scripts; a click, a shift-click and two cmd-clicks pick the first, second and fourth. Connection ▸ Run Selected Scripts, Enter confirms: the fourth's insert lands after the first's `create table`, so the batch ran in tree order, and the dropped third script never ran. |
| `run_bar_runs_cmd_picked_scripts` | A click and a cmd-click pick two of three scripts; the files panel's "Run N script(s)" bar runs them, and only them. |

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

### `debugger`: PL/pgSQL step debugging (needs a database)

Needs a server with `plugin_debugger` in `shared_preload_libraries` and the
`pldbgapi` extension, which the project's `docker compose` server has. The
debug panel cannot be read from outside the window, so the scenarios watch
the routine's effects: each statement of the procedure calls `nextval` on a
sequence, and a sequence is not transactional, so the scenario's own
connection sees each call (`Schema::sequence_calls`) exactly when the
debugger lets that statement run. The routine holds no `COMMIT`: continuing
from a breakpoint after one hung the target backend inside pldebugger, where
not even `pg_terminate_backend` reaches it, until the server was restarted.

Each launch goes through `App::launch_for_results_with`, with the editor tall
enough to show the whole procedure, so `App::toggle_breakpoint(row)` can click
a line's breakpoint dot at a fixed offset. `App::start_debug(stop_on_entry)`
opens the launch dialog with cmd-shift-d (the routine's name is filled in from
the `CREATE` under the cursor), unticks "Stop on entry" unless asked for it,
and starts with Enter. Step Over and Continue are pressed as F10 and F5
(`Key::F10`, `Key::F5`). After each stop the scenario checks the count holds
for a moment, because a routine that ran straight through could be caught
between two statements.

| Test | What it checks |
|---|---|
| `step_over_and_continue_procedure` | Stopped on entry: Step Over is pressed until the first statement runs, the second stays off while the session holds, and Continue runs the rest. |
| `breakpoints_stop_where_set` | Gutter breakpoints on statements 2 and 3, no stop on entry: the session runs to 2 by itself, Continue stops at 3, and Continue again runs to the end. |
| `breakpoints_toggled_mid_session` | Breakpoints on 2 and 3; stopped at 2, the one on 3 is cleared and one on 4 set in the gutter. Continue runs through 3 and stops at 4, then runs to the end. |

### `results`: the results grid and the message log

The grid and the log cannot be read from outside the window, so every scenario
selects something, copies it with cmd-c and reads the clipboard (`pbpaste`).
Before each copy the clipboard is set to a sentinel, so a copy that never
happened cannot pass, and **the suite leaves your clipboard overwritten**.
Each launch goes through `App::launch_for_results`, which hides both side
panels and pins the editor to `EDITOR_HEIGHT`, so the grid's rows and the
log's lines sit at fixed offsets (`results_row_y`, `log_line_y`).

The grid scenarios run `GRID_QUERY`, five rows of `r<row>c<col>` over
`generate_series` with a NULL in the third column of every even row, so they
need a database (`PG_GUI_E2E_DATABASE`) but no objects. The only cell clicked is
(0, 0), which is just right of the row-header gutter whatever the columns'
widths; the arrows walk from there. Escape then Right does not work for this,
because clearing the selection puts the table back in row mode, and in row mode
Right moves a column selection. Nothing says when the rows have arrived, so each copy repeats its
whole attempt until the clipboard matches (`copy_until`).

The paging scenarios launch through `App::launch_for_results_with`, which seeds
`page_size` 3 (and a small `fetch_size` for the cursor) on top of the same
layout, and run a one-column `generate_series`. The pager under the grid has
buttons but no shortcuts, so `App::click_pager` clicks Prev, Next or Fetch
more at offsets measured from a capture: fixed from the left, and a fixed
distance above the window's bottom edge. Which page is showing is read off
its first cell; what the grid holds in all, off cmd-a and cmd-c.

| Test | What it checks |
|---|---|
| `grid_copies_selected_cell` | A click on the first cell and cmd-c copies its value; the arrows walk to another cell; Left wraps to the last column; a NULL copies as the empty string. |
| `grid_select_all_copies_every_row` | cmd-a in the grid selects every row and cmd-c copies them as tab-separated lines; Escape drops that selection, so the next cmd-c leaves the rows off the clipboard. |
| `grid_shift_and_cmd_click_select_rows` | A click then a shift-click selects a range of rows; cmd-click toggles one row out and another in, and the copy keeps result-set order. |
| `grid_pages_through_long_result` | Eight rows at three per page: Next and Prev move between pages, the last page holds only the two rows left over, Next on the last page stays put, and cmd-a still copies all eight rows. |
| `grid_fetch_more_appends_rows` | Twelve rows fetched five at a time: Fetch more appends the next batch without leaving the current page, the fetched rows page like the first ones, and a second Fetch more drains the cursor. |
| `log_shows_failed_run_and_copies` | No database needed: a run against the seeded dead connection fails, the panel switches to the log, and a mouse sweep plus cmd-c copies the `Connection refused` line. |
| `log_shows_server_notices` | A `DO` block raising a notice and a warning returns no rows, so the panel shows the log by itself; both messages are there, in the order raised. |

### `definitions`: Go to Definition (needs a database)

cmd-click on a name in the editor resolves it in the catalog and opens its
definition: the object's `.sql` file from the working folder when one matches
`definition_file_mask` (default `*_{object}.sql`), else the definition fetched
from the server, in a tab. The tab holds `select * from <schema>.orders;`,
launched through `App::launch_for_results` (or `_prepared`, which also hands
over the scratch directory for a working folder), so the editor's text starts
at a fixed offset and `App::cmd_click_text(row, col, dwell)` can point at a
character: it moves the pointer there with cmd in the event's flags, rests,
and clicks. The editor only resolves the link on that cmd-hover, through a
lookup that opens a connection of its own, and nothing outside the window says
when it has, so each scenario repeats the hover-and-click with `retry_until`.

| Test | What it checks |
|---|---|
| `cmd_click_opens_fetched_definition` | With no working folder, cmd-click on `orders` opens a new untitled tab holding its `CREATE TABLE`, beside the script's tab. |
| `cmd_click_opens_matching_file` | With `tables/01_orders.sql` in the working folder, cmd-click on `orders` opens that file instead. |

### `explain`: query plans (needs a database)

The flame graph cannot be read from outside the window, so the scenarios
watch the tab's session from the database instead: once an explain has
finished, the app's backend sits idle with the `EXPLAIN (…)` it sent as its
last statement (`Schema::backends`). The statement explained is an insert,
which tells the two kinds apart: a plain plan never runs it, `ANALYZE` does.

| Test | What it checks |
|---|---|
| `explain_plan_does_not_run_statement` | cmd-e sends `EXPLAIN (FORMAT JSON)` over the statement under the cursor, and the insert never runs. |
| `explain_analyze_runs_statement` | cmd-shift-e sends `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`, which runs the insert; with autocommit on, the row is committed. |
| `explain_analyze_rolls_back_without_autocommit` | With autocommit off, the analyzed insert lands in the tab's open transaction, uncommitted, and Session ▸ Rollback undoes it. |

### `export`: exporting a result set (needs a database)

Connection ▸ Export as CSV… and Export as INSERT… re-run the statement under
the cursor and write its result to the file the platform's save panel names:
CSV through `COPY … TO STDOUT WITH (FORMAT csv, HEADER)`, INSERT as one
statement per row. The scenarios seed `last_dir` with their scratch
directory, so the panel opens there, and check the file on disk. Their query
returns four rows whose values an export has to quote or spell out: a comma,
a double quote, a single quote, and a NULL.

| Test | What it checks |
|---|---|
| `export_csv_writes_result_set` | The CSV holds exactly the header line plus one line per row: the comma and double-quote values quoted (the quote doubled), the NULL left empty. |
| `export_inserts_writes_replayable_script` | The script holds exactly one `INSERT INTO <schema>.items ("id", "label") VALUES (…);` per row, every value a literal (the single quote doubled) and the NULL spelled out; run against the emptied table, it puts all four rows back. |

### `transactions`: autocommit, Commit and Rollback (needs a database)

Each scenario runs `insert into <schema>.t values (1);` from the editor and
watches the row from the scenario's own connection, which sees it only once the
app's transaction has committed. To tell "ran but not committed" from "not run
yet", it waits for the app's backend to show up as `idle in transaction` in
`pg_stat_activity` (`Schema::open_transactions`). Autocommit is turned off and
on with Session ▸ Toggle Autocommit, and the change is confirmed through
`config.json`'s `autocommit`.

| Test | What it checks |
|---|---|
| `autocommit_commits_each_run` | With autocommit on (the default), the row is visible right after the run and no transaction is left open. |
| `manual_commit_needs_commit` | With autocommit off, the run leaves the row in an open transaction, invisible; Session ▸ Commit makes it visible and ends the transaction. |
| `manual_rollback_discards` | With autocommit off, Session ▸ Rollback ends the transaction and the row never lands. |
| `autocommit_on_commits_open_transaction` | With autocommit off and a run left open, turning autocommit back on commits it. |

## Database-backed scenarios

The `completion` (but its snippet one), `query`, `db_viewer`, `debugger`, `definitions`, `explain`, `export`, `hover` and `transactions` scenarios, all of `results` but the failed-run log one, and the last `editor_syntax` one need a running PostgreSQL. The harness never starts
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
PG_GUI_E2E_DATABASE=docker cargo e2e
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
| `pg-gui.log` | The app's diagnostics log (`PG_GUI_LOG=warn,pg_gui::lsp=debug`). |
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
- **Pixels.** `brightness_at` reads one pixel of a window capture, which is
  how the theme scenario sees the colours GPUI painted.
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
| `Schema::create(test)` | Creates the scenario's scratch schema, or returns `None` (the scenario returns early) when `PG_GUI_E2E_DATABASE` is unset. `name()`, `execute(sql)` (`{schema}` is replaced by the name), `table_exists(table)`, `row_count(table)`, `sequence_calls(sequence)`, `open_transactions()`, `backends()`. Before dropping, it ends any other backend whose query names the schema (a debug target left trapped). Dropped with `CASCADE` on drop. |
| `App::launch_with_database(&Schema, script)` | Starts the app connected to the schema's database, the database panel showing, with one tab holding `script`. |
| `click_db_row(Button, row)`, `expand_db_row(row)` | Database panel: click tree row `row` (0 = top); make sure folder row `row` is expanded (safe to repeat). |
| `retry_until(what, timeout, attempt, pred)` | Repeats `attempt` until `config.json` satisfies `pred`, for steps nothing outside the window reports on. Each attempt must be safe to repeat. |
| `harness::active_script(&config)` | Returns the active tab's text. |
| `App::launch_with_folder(&[&str])` | Starts the app with the files panel on a fresh working folder holding those (empty) files; a path ending in `/` is a folder. `work_dir()` returns it. |
| `App::launch_with_scripts(&Schema, &[(&str, &str)])` | Like `launch_with_folder`, with each file holding the given script and the app connected to the schema's database. |
| `send_keys(&[Mod], char)` | Brings the app to the front and types the key with modifiers held. |
| `type_text(str)`, `press(&[Mod], Key)` | Types text into the focused element; presses a non-printing key (`Key::Return`, `Key::Up`). |
| `click_at(Button, x, y)` | Left- or right-clicks at points from the window's top-left corner. |
| `click_files_new_script()`, `filter_files(text)`, `click_files_row(Button, row)` | Files panel: the header "+", the filter box (text replaced), tree row `row` (0 = top). |
| `click_files_row_with(&[Mod], row, picked)`, `click_files_run_bar()` | Files panel: a shift- or cmd-click on tree row `row`, and the "Run N script(s)" bar. The bar shows above the tree while two or more scripts are picked and pushes the rows down, so `picked` says whether it is up before the click. |
| `wait_for_extra_window(what)` | Waits for a second window, such as a save panel. |
| `click_menu(menu, item)` | Clicks a menu item. The item matches with or without its check mark. |
| `start_debug(stop_on_entry)`, `toggle_breakpoint(row)` | Debugger: start a session on the routine under the cursor; click editor row `row`'s breakpoint dot (a `launch_for_results` layout). |
| `cmd_click_text(row, col, dwell)` | Editor (a `launch_for_results` layout): hover the character at `row`, `col` with cmd held for `dwell`, then click — Go to Definition. |
| `hover_text(row, col)` | Editor (a `launch_for_results` layout): move the pointer onto the character at `row`, `col`, coming in from a blank spot. |
| `pick_snippet(filter)` | Opens the snippet picker (cmd-p), types `filter` and inserts the first match with Enter. |
| `App::launch_for_results_prepared(url, script, prepare)` | `launch_for_results` with the scratch directory handed to `prepare` first, to write files the seed points at. |
| `click_submenu(menu, submenu, item)`, `submenu_items(menu, submenu)`, `submenu_checked(menu, submenu, item)` | Click, list or check an item in a submenu (Connection ▸ Recent, View ▸ Theme); items match with or without their check mark. |
| `brightness_at(x, y)` | Captures the window and returns the brightness (0 black, 1 white) of the pixel at that point, through `NSBitmapImageRep` in JXA — what GPUI actually painted. |
| `fill_connection_dialog(name, url)` | In the open New Connection dialog, types the name and connection string and presses Connect. |
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
- **CI.** A runner would need Accessibility permission for the terminal process.
  GitHub-hosted macOS runners do not grant it by default, so check whether they
  allow it before adding a workflow.
