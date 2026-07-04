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

SQL editor language support (completions, hover, diagnostics) comes from the Postgres Language Server: the app spawns `postgrestools lsp-proxy` (`brew install postgrestools`). Without the binary the app still runs, just without language support. The LSP client lives in `src/lsp.rs`; it writes the server's config (including db credentials, derived from the connection string) to `~/Library/Caches/pg-gui/lsp-workspace/`.
