# pg-gui

A small desktop app for editing and executing PostgreSQL scripts, built with
[GPUI](https://www.gpui.rs) (Zed's UI framework) and
[gpui-component](https://github.com/longbridge/gpui-component).

![pg-gui editing a SQL script](assets/screen1.png)

## Features

- **[SQL editor](docs/features/sql-editor.md)** — tree-sitter highlighting,
  line numbers, find and replace
- **[Language support](docs/features/language-server.md)** — completions,
  hover, diagnostics and formatting from the embedded Postgres Language Server
- **[Executing scripts](docs/features/running-queries.md)** — `cmd-enter` runs
  the selection or the statement at the cursor into a virtualized result table
- **[Copying from the results grid](docs/features/results-copy.md)** — `cmd-c`
  on a cell, row or column
- **[Message log](docs/features/message-log.md)** — per-statement timings and
  the server's own `RAISE NOTICE` output
- **[Transactions](docs/features/transactions.md)** — per-tab autocommit,
  Commit / Rollback, Cancel Query
- **[Exporting results](docs/features/export.md)** — every row as CSV or as a
  runnable `INSERT` script
- **[Database object browser](docs/features/object-browser.md)** — `cmd-1` for
  schemas, tables, views, routines, sequences and types
- **[Go to Definition](docs/features/go-to-definition.md)** — cmd-click a name
  in the editor to open its source
- **[Query plan flame graph](docs/features/query-plan.md)** — `cmd-e`, or
  `cmd-shift-e` with `EXPLAIN (ANALYZE, BUFFERS)`
- **[PL/pgSQL step debugging](docs/features/debugging.md)** — `cmd-shift-d`,
  breakpoints, variables and call stack via pldebugger
- **[Connection manager](docs/features/connections.md)** — named connections,
  inline Test Connection, tabs and output parked per connection
- **[Opening and saving scripts](docs/features/files.md)** — native dialogs,
  one tab per file, dirty and changed-on-disk prompts
- **[Palette](docs/features/palette.md)** — `cmd-p` finds a file in the
  working folder; `>` switches it to snippets, `:` to commands
- **[Snippets](docs/features/snippets.md)** — `New:` templates with tab stops,
  extendable with your own `.sql` files
- **[Creating scripts in place](docs/features/new-scripts.md)** — the "+" on
  the files panel's title row or on a folder
- **[Running scripts from the files panel](docs/features/run-scripts.md)** —
  `cmd-shift-enter` for a batch that stops at the first failure
- **[Themes](docs/features/themes.md)** — Catppuccin Mocha and Latte, or
  follow the system
- **[AI completion](docs/features/ai-completion.md)** (optional) — complete
  the SQL at the cursor with the Claude API

## Running

For a guided tour of every feature against the sample database, see
[scenario.md](scenario.md).


```sh
cargo run
```

The first build is slow: the embedded language server compiles `libpg_query`
from source, which needs a working C toolchain and libclang.

## Logging

The app writes a diagnostics log next to `config.json` —
`~/Library/Application Support/pg-gui/pg-gui.log` on macOS, `~/.config/pg-gui/pg-gui.log`
on Linux — and the same lines to stderr, so a `cargo run` shows them in the terminal
and a launch from Finder still leaves a file to read.

By default it records errors only, from the app and its libraries alike.

`PG_GUI_LOG` widens or narrows that, in the usual `target=level` syntax —
a bare level sets the default, `target=level` overrides one module tree, and the
levels are `error`, `warn`, `info`, `debug` and `trace`:

```sh
PG_GUI_LOG=debug cargo run                      # everything from debug up
PG_GUI_LOG=warn,pg_gui=trace cargo run          # all of pg-gui, warnings elsewhere
PG_GUI_LOG=pgls_workspace=debug cargo run       # just the language server
PG_GUI_LOG=off cargo run                        # nothing at all
```

## Test database

A disposable Postgres with sample data (customers/orders) is included. It is a
custom image (`docker/Dockerfile`) — `postgres:17` plus the
[pldebugger](https://github.com/EnterpriseDB/pldebugger) extension compiled from
source — so the first start builds the image:

```sh
docker compose up -d --build --wait
```

It listens on port **5433** (to avoid clashing with a local server on 5432).
Connect with:

```
postgres://pgui:pgui@localhost:5433/pgui_test
```

Preloaded libraries: `pg_stat_statements` and `plugin_debugger`. Seed scripts
live in `sql/`, which is mounted as the container's init directory and runs on
first start — they enable the `pg_stat_statements` and `pldbgapi` extensions and
create the sample schema. One folder per object type, one file per object,
`V.<version>__<name>.sql` for what is applied once and `R__<nnn>_<name>.sql` for
what is safe to re-apply, driven by `sql/00-run-init.sh` (see `sql/README.md`).
`docker compose down -v` resets the data.

## AI completion

Set an API key to enable Edit ▸ AI Complete: either `ai_api_key` in
`config.json` or the `ANTHROPIC_API_KEY` environment variable:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cargo run
```

Place the cursor where you want a completion and press `cmd-i`. Model and
prompt settings are in [docs/features/ai-completion.md](docs/features/ai-completion.md).

## Keybindings

Every command also lives in the application menu bar. Press `cmd-h` (macOS)
or `F1` (Linux) in the app to see this list in a dialog. On Linux, `cmd` is
`ctrl` throughout.

| Key | Action |
| --- | --- |
| `cmd-enter` / `ctrl-enter` | Run the selection or the statement at the cursor |
| `cmd-shift-enter` / `ctrl-shift-enter` | Run the scripts selected in the files panel |
| `cmd-i` / `ctrl-space` | AI complete at cursor |
| `cmd-e` / `cmd-shift-e` | Explain the statement / explain it with ANALYZE (runs it) |
| `cmd-shift-f` | Format the script |
| `cmd-/` | Comment or uncomment the line / selection |
| `cmd-f` / `ctrl-f` | Find in the script |
| `cmd-r` / `ctrl-r` | Find and replace in the script |
| `cmd-p` | Go to a file; type `>` for snippets, `:` for commands |
| `cmd-shift-p` | Run a command |
| `cmd-t` | New script tab |
| `cmd-w` | Close the tab |
| `ctrl-tab` / `ctrl-shift-tab` | Next / previous tab |
| `cmd-o` | Open a `.sql` file |
| `cmd-s` | Save (Save As on first save) |
| `cmd-1` | Show or hide the database browser |
| `cmd-b` / `cmd-2` | Show or hide the files panel |
| `cmd-3` | Show or hide the results panel |
| `cmd-c` (results grid) | Copy the selected cell, row or column |
| `cmd-shift-d` | Start debugging a routine |
| `F10` / `F11` | Step over / step into |
| `F5` / `shift-F5` | Continue / stop debugging |
| `cmd-,` | Open `config.json` in the system editor |
| `cmd-plus` / `cmd-minus` | Zoom in / out |
| `cmd-0` | Reset zoom |
| `cmd-h` / `F1` | Show the command help dialog |
| `cmd-q` | Quit |
