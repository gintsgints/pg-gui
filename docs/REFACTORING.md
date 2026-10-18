# pg-gui Refactoring Todo

Derived from a codebase review of the current `main` (0.9.1). `cargo fmt --check` and `cargo clippy --all-targets -- -W clippy::pedantic -D warnings` both pass, so this is about structure, robustness, and test coverage — not about fixing lint failures.

## How to use this file

Each item is a self-contained task. Work in small PRs; after every Rust change run:

```sh
cargo fmt
cargo clippy --all-targets -- -W clippy::pedantic -D warnings
cargo test
```

Do **not** touch the related forks (gpui-kit `my_fixes3`, pgls `pg_gui_2`, pgdap). All items below can be done inside pg-gui alone. See `CLAUDE.md` ("Related crates are out of scope").

---

## Priority 0: UI end-to-end testing on macOS (first task)

pg-gui currently has **no UI end-to-end tests** — coverage is unit tests in `app.rs` and one ignored docker-backed LSP integration test. Because this is a GPUI desktop app with no out-of-process test harness, E2E means driving the running application through the macOS window server and observing its UI state.

### Status (2026-10-17)

First cut landed: `tests/e2e/harness.rs` + `tests/e2e/main.rs`, run with `cargo test --features e2e --test e2e` (the target is gated by the `e2e` Cargo feature, so a plain `cargo test` never builds it). Four smoke scenarios pass: launch (window + menus), cmd-t / cmd-w tab open/close, cmd-1/2/3 panel toggles, Edit ▸ Format on Save check mark — each ending in a clean cmd-q.

Findings that change the plan below:

- **GPUI exposes no AX tree for window content.** The window's only AX children are the traffic-light buttons, so `read_status_bar()` / `read_results_table()` via AXUIElement are not possible without adding accessibility support to gpui (a fork change, out of scope). Observable instead: native menus (check marks appear as a `✓ ` title prefix), window titles, and `config.json` (rewritten on tab/panel/toggle changes) — that file is the state oracle.
- osascript + System Events covers launch/keys/menus/screenshots with zero new crates; a CGEvent/AX Rust binding is not needed yet.
- New tab is **cmd-t**, not cmd-n.
- `PG_GUI_CONFIG_DIR` was added so test launches are isolated from the user's config and running instance.
- Still open: DB-backed scenarios (results content is not readable from outside — options are screenshot/OCR, or a debug-only state dump the app writes on request), theme switch scenario, CI workflow (needs Accessibility permission granted on the runner, which GitHub-hosted macOS runners do not do for arbitrary binaries out of the box — verify before relying on it).

### Tooling options (macOS-native)

| Approach | How it works | Pros | Cons |
|---|---|---|---|
| **Accessibility API (AXUIElement)** | Native macOS accessibility tree; inspect elements, read text, click menu items/buttons | No extra tools, works on stock macOS, can read results grid/table contents | Verbose API, GPUI element roles/attributes must be probed first |
| **CGEvent key/mouse simulation** | `CGEventCreateKeyboardEvent` / mouse events to drive cmd-shortcuts and clicks | Simple, reliable for menu/keyboard-driven flows | Can't assert UI state by itself; pair with AX reads |
| **cliclick / ydotool** | Third-party CLI for synthetic input | Easy to script from shell | Extra dependency, not Rust-native |
| **Screenshots + image diff** | Capture window, compare to baseline | Catches visual regressions | Brittle to fonts/theme/window size |

**Recommended approach:** a small Rust (or Python) harness that launches `cargo run --bin pg-gui` (or `./target/debug/pg-gui`), waits for the main window, drives it with CGEvent key sequences (cmd-n, cmd-enter, cmd-1/2/3, cmd-w, cmd-q), and asserts state by reading the Accessibility tree (AXUIElement) — e.g. menu item titles, the status bar text, results table row counts. Take a screenshot on failure for debugging.

### What to build (contained to pg-gui, no fork changes)

1. **`tests/e2e/` harness crate** (or a script under `scripts/`):
   - `launch()` — start the app binary, wait for the main window to appear (poll AX or window title).
   - `send_keys(modifiers, key)` — CGEvent key down/up for the app's cmd/ctrl shortcuts.
   - `click_menu(path: &[&str])` — navigate the menu bar via AX.
   - `read_status_bar() -> String` — AX read of the status bar static text.
   - `read_results_table() -> Vec<Vec<String>>` — AX read of the data grid (cells/rows).
   - `screenshot(path)` on failure.
   - `quit()` — cmd-q with close-guard handling (prompt if dirty).

2. **Initial E2E test scenarios (no database needed for the smoke set):**
   - App launches → title bar shows "pg-gui", editor placeholder visible.
   - cmd-n opens a new tab → tab bar gains a tab.
   - cmd-w closes the tab → tab removed (or quit prompt if dirty).
   - cmd-1 / cmd-2 / cmd-3 toggle database / files / results panels.
   - View → Theme switches change the status bar / theme segment.
   - Edit → Format on Save toggles the check mark.

3. **Database-backed scenarios (need `docker compose up -d` local PG on port 5433):**
   - Connection ▸ Recent → connect to `pgui/pgui/pgui_test` on localhost:5433.
   - Type `select 1;` → cmd-enter → results table shows one row/one column.
   - cmd-e on `select 1;` → EXPLAIN plan view opens (flame graph).
   - Toggle results panel → log view shows the run line.
   - Disconnect / switch connection → parked tabs preserved.

4. **CI (macOS):** GitHub Actions `macos-latest` runner.
   - macOS GUI tests need access to the window server. On GitHub Actions macOS runners this typically works out of the box for `cargo run` launched apps; if headless, look into launching with `launchctl`/`osascript` or an XQuartz-free approach — GPUI on macOS uses CoreGraphics directly.
   - Add a workflow `.github/workflows/e2e.yml` running `cargo test --test e2e` after `cargo build`.

### Definition of done

- A runnable harness under `tests/e2e/` (or `scripts/e2e/`) that can launch, drive, and assert on the app from a macOS terminal.
- At least the smoke scenarios (launch, new tab, toggle panels, quit) pass on macOS without a database.
- `cargo test` (including the e2e test target when gated behind `--features e2e` or a `--test e2e` binary) passes.
- Documented in `CLAUDE.md` / README how a maintainer runs E2E locally (`cargo test --test e2e` plus docker for DB-backed scenarios).

---

## Priority 1: Split the god object — `src/app.rs` (9036 lines)

`app.rs` holds the app state, tab lifecycle, query running, LSP wiring, ~40 action handlers, all rendering, menus, dialogs, and 500+ lines of unit tests. Split it into cohesive modules, following the pattern already used elsewhere in the crate.

### Suggested module map

| New module | Responsibility | Approx. lines to move |
|---|---|---|
| `src/tab.rs` | `EditorTab`, `TabResult`, `RunProgress`, tab build/activate/close/dirty logic | ~400 |
| `src/session.rs` | Per-tab `db::Session` lifecycle, connect/cancel, `settle_session`, `on_query_ok`/`on_query_error` | ~150 |
| `src/queries.rs` | `run_query`, `run_scripts`, `start_explain`, `run_files`, progress/event draining | ~250 |
| `src/ui/panels.rs` | All `render_*` functions (title bar, status bar, files panel, db panel, debug panel, results views) | ~700 |
| `src/ui/menus.rs` | `build_menus`, `refresh_menus`, menu/item builders | ~200 |
| `src/ui/dialogs.rs` | Connection form, new-file prompt, run-scripts confirmation, quit prompt | ~200 |
| `src/commands.rs` | All action handler impls (`run_query`, `toggle_autocommit`, `start_debug`, …) | ~400 |

### Status

- Step 1 done: `src/app/tab.rs`. New modules are *child* modules of `app` (`src/app/<name>.rs`, declared in `app.rs`) rather than crate-level siblings, so their `impl PgGuiApp` blocks keep reading `PgGuiApp`'s private fields; only the moved methods and the moved types' fields become `pub(super)`. Besides the items listed below it also took the run-progress clock (`start_progress*`, `PROGRESS_TICK`), the close/quit guards, and `tab_label`/`set_tab_path`/`mark_saved`.

- Step 2 done: rendering is in `src/app/ui/`, one file per panel rather than one `ui/panels.rs` — `files.rs` (files panel), `db_browser.rs` (object browser), `bottom.rs` (result table, log, plan view, pager, result selector), `chrome.rs` (title bar, tab bar, session toolbar, status bar), `debugger.rs` (debug panel), `workspace.rs` (the resizable layout that composes them). The tree-row helpers the files panel and the object browser share (`tree_row_height`, `clip_label`, `tree_row_action`) sit in `src/app/ui.rs`. Methods there are `pub(in crate::app)`. Only `impl Render for PgGuiApp` stays in `app.rs`, as planned.
- Step 3 done: `src/app/queries.rs` holds the runners (`run_query`, `run_scripts`/`run_one_script`/`prompt_run_scripts`/`start_script_run`, `explain_*`/`start_explain`), what they share once started (`on_run_event`, `settle_session`, `on_query_ok`/`on_query_error`/`on_plan_ok`, `show_query_error`, `reveal_bottom_view`), `cancel_query`, `fetch_more_rows`, and the free `run_files`. The planned separate `session.rs` folded into it: `settle_session`/`on_query_*` exist for these paths (transaction control is their only other caller), so a module of their own would have been three methods. Transaction control (`toggle_autocommit`, `commit_txn`/`rollback_txn`/`end_txn`) is left for `commands.rs`.
- Step 4 done, split by domain instead of one `commands.rs` (which would have been ~2000 lines, a second god file): `connection.rs` (picker, new/edit dialog + `ConnectionParts`/`ConnectionFields`/`ConnectionTest`, switching and parking), `file_ops.rs` (new/open/save, diverged guard, create/rename/delete, working folder, reload on external change), `editing.rs` (snippets, format, comment toggle, problem navigation, AI completion), `view.rs` (panel toggles, theme, zoom, help, open config/GitHub), `output.rs` (result-set switch, paging, cell/row selection and copy, export), `debugger.rs` (debug session commands and events); transaction control joined `queries.rs`. Free items used by one module moved with it; ones the `app.rs` tests cover (`record_recent`, `mask_credentials`, `toggle_line_comments`, `diagnostic_target`, …) stay in `app.rs` and are imported. Moved methods are private unless another module calls them.
- Step 5 done. Every split was a cut, never a copy, so there was no old code left to delete; instead the two clusters step 4 left behind moved out too: `menus.rs` (`build_menus` and its item builders, `refresh_menus`) and `objects.rs` (opening a database object from the browser or Go to Definition: `open_object`, the definition cache, `find_sql_file` and its name/content ranking — with their unit tests, which moved into the module). `app.rs` is now ~1800 lines: the struct and its construction, config persistence and the external-config watcher, file-tree and object-browser loading, LSP wiring, `impl Render`, and the small free helpers several modules share (whose tests stay there). The non-macOS menu-bar code (`cfg(not(target_os = "macos"))`) was moved by hand and is not compiled on macOS — check it on Linux/Windows before release.

### Suggested order

1. ~~**`tab.rs`**~~ (done) — move `EditorTab`, `TabResult`, `RunProgress`, `build_tab`, `activate_tab`, `close_tab_at`, `refresh_dirty`, `show_tab_result`, `add_tab`/`insert_tab_at`. Lowest coupling, highest clarity gain.
2. ~~**One render panel at a time**~~ (done) — e.g. `render_files_panel` + helpers into `ui/panels.rs`. Keep `render` in `app.rs` until the end.
3. ~~**`queries.rs`**~~ (done) — `run_query`, `run_scripts`, `start_explain` and their `on_*` settle helpers.
4. ~~**`commands.rs`**~~ (done, as domain modules) — remaining action handlers.
5. ~~Delete nothing until the split compiles and tests pass; then remove the old code from `app.rs`.~~ (done)

---

## Priority 2: Reuse connections for catalog introspection — `src/db.rs`

Every catalog query (`list_schemas`, `list_relations`, `list_functions`, `list_columns`, `list_indexes`, `list_constraints`, `list_triggers`, `find_object`, …) goes through `catalog_rows`, which calls `connect()` per invocation. Expanding one browser node fires several catalog queries, each with its own TCP handshake + auth.

### Suggested change

Add a short-lived catalog client that holds one connection for a batch of introspection queries:

```rust
pub struct Catalog {
    client: Client,
}

impl Catalog {
    pub fn connect(conn_str: &str) -> Result<Self, String>;
    pub fn rows(&mut self, sql: &str) -> Result<Vec<SimpleQueryRow>, String>;
    pub fn scalar(&mut self, sql: &str) -> Result<Option<String>, String>;
}
```

Refactor the `list_*` functions to take `&mut Catalog` (or add `Catalog::schemas(...)`, etc.). Keep the existing free functions as thin one-shot wrappers for the one-off paths (`test_connection`, `search_path`). Call sites in `db_tree.rs` create one `Catalog` per browse operation instead of one per query.

No behavior change; faster object browser, especially on remote/slow servers.

### Status

- Done. `db::Catalog` wraps one `Client` (`connect`/`rows`/`scalar`), and every catalog query — `list_*`, `find_object`, the `*_definition` builders — is a `Catalog` method; the per-query `catalog_rows`/`catalog_scalar` free functions are gone. No one-shot wrappers were kept: the callers that make one query write `Catalog::connect(conn)?.method(..)` instead, which is no longer than a wrapper call; `search_path` goes through `Catalog` the same way, and `test_connection` still only connects. The plan's premise was partly off: expanding one folder was already one query. The connections actually saved are in two places. `table_definition` used 4 connections and `type_definition` 2–3; each now uses one. A schema/table refresh reloaded each expanded folder with its own connection; `PgGuiApp::load_db_folders` now loads them all in one background task over one `Catalog` (a failed connect fails each folder with the same error, as before). `db_tree::load_schemas`/`load_children` take `&mut Catalog`.

---

## Priority 3: Cap non-paged result-set memory — `src/db.rs`, `src/results.rs`

`db::Rows = Vec<Vec<Option<String>>>` holds every row of every statement as owned `String`s. Lone SELECTs page through a server-side cursor (`fetch_size`), but anything else (DML with `RETURNING`, multi-statement blocks, non-SELECT) materializes the full result set in memory. A large `INSERT … RETURNING *` or a cursor-rejected SELECT can consume hundreds of MB.

### Suggested change

- Add a config option `max_result_rows` (default 50k–100k).
- Truncate non-paged result sets at the cap and push a log line like `"note: result truncated at N rows"`.
- Keep the cursor-paged path for lone SELECTs unchanged.
- Mention the cap in the status bar/log so users aren't surprised by missing rows.

### Status

- Done. `max_result_rows` (config.json, default 100 000, `0` = no cap) reaches `Session::run` through `db::RowLimits { batch_size, max_rows }`, which replaced the bare `batch_size` argument (also in `run_files`). `collect_outcome` keeps the first `max_rows` rows of each set and skips converting the rest, records the server's full count in `ResultSet::truncated_from`, and adds `note: result truncated at N rows` to that statement's log line; the status bar then reads `showing N of M row(s)`. The cursor-paged lone SELECT, EXPLAIN, and the export path (`run_script`) are uncapped. One limit the plan did not foresee: the synchronous `postgres` client has no streaming simple query (`simple_query_raw` is `tokio-postgres`-only, and `postgres::Client` keeps its runtime private), so the driver has buffered a statement's whole answer before the cap sees it. The cap therefore bounds what a result set *retains* — which is what piles up, since every tab keeps its sets across runs and parked connections keep theirs — not a single statement's peak. Bounding the peak would mean driving `tokio-postgres` directly. Covered by two ignored docker tests (`a_result_set_past_the_cap_is_truncated_and_logged`, `a_paged_select_is_not_capped`).

---

## Priority 4: Add unit tests outside `app.rs`

Current test coverage: unit tests in `app.rs` (find_sql_file ranking, etc.) and one ignored docker-backed integration test (`lsp::tests::embedded_server_*`). `db.rs`, `lsp.rs` worker, `debug.rs`, `plan.rs`, `file_tree.rs`, `statement.rs`, `snippets.rs` have no tests.

### Suggested additions (no docker needed)

- `db.rs`: `describe` for connection errors vs. `DbError`; `statement_ranges` comment/string/dollar-quote edge cases; `collect_outcome` row/column handling.
- `plan.rs`: `parse` with a zero-cost plan (triggers `Unit::Nodes`); `Gather`/plan shape; `node` extraction.
- `file_tree.rs`: `scan_dir` dot-entry skipping and budget cap; `to_tree_items` filter/projection; `signature` stability.
- `statement.rs`: body-line offset for a single routine definition.
- `debug.rs`: `body_line_of` mapping (editor row → body line) with synthetic source.
- `lsp.rs`: `probe_schema` inference (Loading/Loaded/Offline/Disabled); `completion_detail`.

For parser-heavy tests follow the existing `tests::on_worker_stack` pattern (64 MiB stack, same as the document worker) because `libpg_query`/`prost` decoding can overflow the default 2 MiB.

### Status

- Done, against a stale premise: by now every module the plan lists already had unit tests — `plan` covers `Unit::Nodes`, `statement` the body-line offset, `debug` the `body_line_of` mapping, `file_tree` dot-entry skipping, the budget cap, projection and `signature`, `lsp` `completion_detail` and the `Disabled` schema status. The pass therefore filled the gaps that were left (16 tests, all docker-free):
  - `db`: `describe` appends a connection error's cause chain; `test_connection` rejects unreachable and unparseable strings; `quote_literal`/`quote_ident`, `is_true`, `log_line`.
  - `lsp`: `build_configuration` (host/port/user/password/database taken from the URL, database defaulting to the user, an unparseable string disabling the connection, casing mapped onto the formatter) and `is_trigger`.
  - `debug`: `describe_addr` (per-host ports, one shared port, no port, no host).
  - `plan`: node labels (join type, parallel, CTE, function scan), a `Gather` child wider than its parent left unclamped, several roots, `Unit::format`, header elision.
- Still uncovered, because each needs a server: `collect_outcome` (its `SimpleQueryMessage` input has no public constructor; the docker tests exercise it through `Session::run`), `probe_schema`'s Loading/Loaded/Offline inference (needs a live `Workspace`), and `debug::render_messages`.

---

## Priority 5: Standardize error handling

`anyhow::Result` in `lsp.rs`/`debug.rs`/`definitions.rs`; `Result<_, String>` throughout `db.rs` and `app.rs`. `db::describe` returns `String`; callers format it. The `?` operator can't cross the `String`/`anyhow` boundary without `.map_err(anyhow::Error::msg)` or `.context(...)`, which is easy to miss and yields less context.

### Suggested change

Pick one idiom and standardize:

- Option A: `anyhow::Result` in `db.rs` and `app.rs`, attaching context at call sites (`with_context(|| format!("running {sql}"))`), keeping `db::describe` as the source of the user-visible message.
- Option B: keep `Result<_, String>` and wrap at FFI/boundary points.

Consistency matters more than the choice. Do not silence with broad `#[allow(...)]`; a targeted allow with a justification comment is acceptable only for a genuine pedantic false positive (per `CLAUDE.md`).

### Status

- Done, as Option B, written down as a rule in `CLAUDE.md`. Option A was not on the table: gpui-component's completion, hover, definition and semantic-token provider traits return `anyhow::Result`, so `lsp`/`definitions`/`highlight` cannot leave `anyhow`, and moving `db`/`app` onto it would not have removed the boundary. It would only have moved it, wrapping messages `describe` had already made user-ready. The premise that `?` crosses `String`→`anyhow` with `map_err(Error::msg)` turned out not to hold: no code converts a `String` error into `anyhow` (`definitions` drops a failed `find_object` to a cached miss and never propagates it). The crossing that does exist runs the other way: an `anyhow::Error` shown in the status bar or log. Four sites printed one with `{err}`, which shows only the outermost context: the language-server start failure (`app.rs`), Format and Format on Save (`editing.rs`, `file_ops.rs`), and the theme load warning (`main.rs`). They now use `{err:#}`, as the debugger's start failure already did. No other change.

---

## Priority 6: Debugger edge cases — `src/debug.rs`

The controller-thread + target-thread design (two pldebugger connections, `mpsc` command channel, `futures` event receiver, `aborting` flag, `target_done`) is appropriate for pldebugger's blocking model. Known risky edges:

- On `Session::drop` without an explicit `Stop`, if the controller is blocked in `wait_for_target` and the target already ran straight through, `target_done` may never be set → `wait_for_target` parks forever.
- `cancel_backend` is called on `Stop`/drop; there is a window where `aborting` is set before `cancel_backend` lands.
- Mid-run breakpoint arming (`sync_debug_breakpoint`) sends the raw editor row; `body_line_of` silently skips rows when the buffer doesn't hold the installed body verbatim, so a "Continue" can run the whole routine with no feedback.

### Suggested change

- Add a timeout/graceful second `cancel_backend` on the drop path so `wait_for_target` cannot park forever.
- `tracing` log (debug level) when a breakpoint row fails to map to a body line, so users get feedback instead of silent failure.

---

## Priority 7: Small UX polish (optional)

- Empty-state overlay in the results grid when a statement returns zero rows (currently an empty table with no hint).
- The AI completion path (`src/ai.rs`) has no visible 429/retry handling; consider retry-after if polishing this area.

---

## Definition of done per item

- Change compiles (`cargo check`).
- `cargo fmt --check` passes.
- `cargo clippy --all-targets -- -W clippy::pedantic -D warnings` passes.
- `cargo test` passes (existing tests plus any new ones).
- Behavior unchanged unless the item explicitly says otherwise.
