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

SQL editor language support (completions, hover, diagnostics, formatting) comes from the Postgres Language Server, embedded as a **library** rather than spawned as a binary. `src/lsp.rs` depends on the `pgls_*` crates (git dependency on the fork <https://github.com/gintsgints/postgres-language-server>, branch `preserve_comments`) and drives its `Workspace` trait directly: `server_sync()` → `register_project_folder` + `update_settings` (db credentials derived from the connection string, plus formatter casing) → `open_file`. Completions and hover call the workspace synchronously on a background executor; diagnostics are pulled on a debounced background thread after each `change_file` and pushed to the editor through a channel. Completion results are prefixed with snippet suggestions (`snippets::suggestions`, matched against the words before the cursor), and a snippets-only provider (`lsp::SnippetCompletions`) is installed while the server is offline. There is no external process, config file, or `postgrestools` binary anymore, and no `~/Library/Caches/pg-gui/lsp-workspace/` config to write. Because `pgls_query` builds `libpg_query` from source (via `bindgen`/`cc`), the first build is slow and needs a working C toolchain + libclang. Format-on-save (`format_on_save` in config.json, off by default); `keyword_case` and `constant_case` (`"lower"`/`"upper"`, default lower) set the formatter's casing. An `#[ignore]`d integration test (`lsp::tests::embedded_server_*`) exercises completions/diagnostics/formatting against the docker DB — run with `cargo test -- --ignored embedded_server`.
