# pg-gui

A PostgreSQL GUI client built with GPUI (Zed's UI framework) and gpui-component. Rust 2024 edition.

## Formatting and linting — required after every change

After ANY change to Rust code or `Cargo.toml`, run both of these and fix every finding before considering the change done:

```sh
cargo fmt
cargo clippy --all-targets -- -W clippy::pedantic -D warnings
```

- Clippy pedantic is the project lint level. Do not silence findings with broad `#[allow(...)]`; fix the code. A targeted `#[allow]` with a short justification comment is acceptable only when a pedantic lint is a genuine false positive.
- Never commit code that fails either command.

## Build & run

```sh
cargo run              # dev build (deps are built with opt-level 3; keep it that way, gpui is unusable otherwise)
docker compose up -d   # local PostgreSQL (host port 5433, user/pass/db: pgui/pgui/pgui_test)
```

Never run Docker or database actions yourself — no starting the daemon, no `docker compose up/down`, no `docker exec`, no psql/DDL/DML against the database. The user does all of that manually. If a task needs the database in some state, tell the user what is needed and wait.

PL/pgSQL step debugging (`src/debug.rs`) drives EnterpriseDB's pldebugger (the `pldbg_*` proxy API) through the [`pgdap`](https://github.com/gintsgints/pgdap) crate, embedded as a **library** (`pgdap::debugger::DebugSession`) rather than spawned as a DAP server binary — same approach as the LSP below. pldebugger needs **two** connections that both block: a persistent *controller* (arms breakpoints, steps, reads stack/vars) and a *target* that runs the debugged routine and stays trapped in the backend until continued. `debug::Session::start` spawns a controller thread which, after arming the entry global breakpoint, spawns the target thread (so the trap is set before the routine runs); commands go in over a `std::sync::mpsc` channel, `DebugEvent`s come out over a `futures` `UnboundedReceiver` the UI awaits like LSP diagnostics — never the UI thread. The target statement is built here (not by pgdap, whose binary only emits `SELECT`): procedures use `CALL`, set-returning functions `SELECT * FROM f(..)`, else scalar `SELECT`, via a `prokind`/`proretset` lookup. Breakpoint line numbers are body-relative to `pldbg_get_source` (no disk source mapping yet). Requires the `pldbgapi` extension (`docker/init/03-pldebugger.sql`) and `plugin_debugger` in `shared_preload_libraries`. UI: the debug panel (`render_debug_panel`) replaces the results table while a session is active — stepped source with a breakpoint gutter, variables (click to `deposit_value`), call stack (click to `select_frame`), and a Step Over/Into/Continue/Stop toolbar; also the Debug menu and F5/F10/F11/Shift-F5. `docs/debug/EXAMPLE.md` documents the raw `pldbg_*` flow the sample `place_order` procedure is debugged with. Note: pgdap needs a `[lib]` target (`src/lib.rs` exposing `pub mod debugger;`) for this dependency — until that lands upstream, `Cargo.toml` points at a local path checkout.

SQL editor language support (completions, hover, diagnostics, formatting) comes from the Postgres Language Server, embedded as a **library** rather than spawned as a binary. `src/lsp.rs` depends on the `pgls_*` crates (git dependency on the fork <https://github.com/gintsgints/postgres-language-server>, branch `preserve_comments`) and drives its `Workspace` trait directly: `server_sync()` → `register_project_folder` + `update_settings` (db credentials derived from the connection string, plus formatter casing) → `open_file`. Completions and hover call the workspace synchronously on a background executor; every write to the workspace document (`change_file`, formatting, `close_file`) goes through a dedicated document-worker thread fed by a channel — never the UI thread, because diagnostics hold the workspace's document lock across database connection attempts (seconds when the server is unreachable). The worker also pulls diagnostics on a debounce after each change and pushes them to the editor through a channel. Completion results are prefixed with snippet suggestions (`snippets::suggestions`, matched against the words before the cursor), and a snippets-only provider (`lsp::SnippetCompletions`) is installed while the server is offline. There is no external process, config file, or `postgrestools` binary anymore, and no `~/Library/Caches/pg-gui/lsp-workspace/` config to write. Because `pgls_query` builds `libpg_query` from source (via `bindgen`/`cc`), the first build is slow and needs a working C toolchain + libclang. Format-on-save (`format_on_save` in config.json, **on** by default), toggled at runtime from the clickable `fmt: on`/`fmt: off` segment in the status bar or the check-marked Edit ▸ Format on Save menu item (both dispatch `ToggleFormatOnSave`, which persists the flag and rebuilds the menus for the check-mark); `keyword_case` and `constant_case` (`"lower"`/`"upper"`, default lower) set the formatter's casing. An `#[ignore]`d integration test (`lsp::tests::embedded_server_*`) exercises completions/diagnostics/formatting against the docker DB — run with `cargo test -- --ignored embedded_server`.
