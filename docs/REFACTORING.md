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

### Suggested order

1. **`tab.rs`** — move `EditorTab`, `TabResult`, `RunProgress`, `build_tab`, `activate_tab`, `close_tab_at`, `refresh_dirty`, `show_tab_result`, `add_tab`/`insert_tab_at`. Lowest coupling, highest clarity gain.
2. **One render panel at a time** — e.g. `render_files_panel` + helpers into `ui/panels.rs`. Keep `render` in `app.rs` until the end.
3. **`queries.rs`** — `run_query`, `run_scripts`, `start_explain` and their `on_*` settle helpers.
4. **`commands.rs`** — remaining action handlers.
5. Delete nothing until the split compiles and tests pass; then remove the old code from `app.rs`.

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

---

## Priority 3: Cap non-paged result-set memory — `src/db.rs`, `src/results.rs`

`db::Rows = Vec<Vec<Option<String>>>` holds every row of every statement as owned `String`s. Lone SELECTs page through a server-side cursor (`fetch_size`), but anything else (DML with `RETURNING`, multi-statement blocks, non-SELECT) materializes the full result set in memory. A large `INSERT … RETURNING *` or a cursor-rejected SELECT can consume hundreds of MB.

### Suggested change

- Add a config option `max_result_rows` (default 50k–100k).
- Truncate non-paged result sets at the cap and push a log line like `"note: result truncated at N rows"`.
- Keep the cursor-paged path for lone SELECTs unchanged.
- Mention the cap in the status bar/log so users aren't surprised by missing rows.

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

---

## Priority 5: Standardize error handling

`anyhow::Result` in `lsp.rs`/`debug.rs`/`definitions.rs`; `Result<_, String>` throughout `db.rs` and `app.rs`. `db::describe` returns `String`; callers format it. The `?` operator can't cross the `String`/`anyhow` boundary without `.map_err(anyhow::Error::msg)` or `.context(...)`, which is easy to miss and yields less context.

### Suggested change

Pick one idiom and standardize:

- Option A: `anyhow::Result` in `db.rs` and `app.rs`, attaching context at call sites (`with_context(|| format!("running {sql}"))`), keeping `db::describe` as the source of the user-visible message.
- Option B: keep `Result<_, String>` and wrap at FFI/boundary points.

Consistency matters more than the choice. Do not silence with broad `#[allow(...)]`; a targeted allow with a justification comment is acceptable only for a genuine pedantic false positive (per `CLAUDE.md`).

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
