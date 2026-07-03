# pg-gui

A small desktop app for editing and executing PostgreSQL scripts, built with
[GPUI](https://www.gpui.rs) (Zed's UI framework) and
[gpui-component](https://github.com/longbridge/gpui-component).

## Features

- **SQL editor** with tree-sitter syntax highlighting, line numbers, and a monospace theme
- **Execute scripts** against any PostgreSQL server (`cmd-enter` or the Run button);
  multi-statement scripts are supported via the simple query protocol, and the last
  result set is shown in a virtualized table (handles large result sets)
- **Open / save** `.sql` files with native file dialogs (`cmd-o` / `cmd-s`)
- **AI completion** (optional): completes the SQL at the cursor using the Claude API
  (`cmd-i` or `ctrl-space`)

## Running

```sh
cargo run
```

The connection string and the SQL editor buffer are remembered between launches:
both are saved to `~/Library/Application Support/pg-gui/config.json` (the platform
config directory) as you type — the script with a short debounce — and restored on
the next start, unsaved edits included. The path of the last opened/saved `.sql`
file is remembered too, so `cmd-s` keeps writing to the same file after a restart.
A `DATABASE_URL` environment variable, if set, overrides the saved connection
string for that launch; with neither present the field defaults to
`postgres://$USER@localhost:5432/postgres`. TLS connections are not supported yet
(the client connects with `NoTls`).

## Test database

A disposable Postgres with sample data (customers/orders) is included:

```sh
docker compose up -d --wait
```

It listens on port **5433** (to avoid clashing with a local server on 5432).
Connect with:

```
postgres://pgui:pgui@localhost:5433/pgui_test
```

Seed scripts live in `docker/init/` and run on first start. `docker compose down -v`
resets the data.

## AI completion

Set an API key before launching to enable the **AI Complete** button:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cargo run
```

Place the cursor where you want a completion and press `cmd-i`. The model receives the
text before and after the cursor and inserts the completion at the cursor.

- Default model: `claude-opus-4-8` — override with `PG_GUI_AI_MODEL`
  (e.g. `claude-haiku-4-5` for lower latency).

## Keybindings

| Key | Action |
| --- | --- |
| `cmd-enter` / `ctrl-enter` | Run script |
| `cmd-i` / `ctrl-space` | AI complete at cursor |
| `cmd-o` | Open a `.sql` file |
| `cmd-s` | Save (Save As on first save) |
| `cmd-q` | Quit |

## Code layout

- `src/main.rs` — app entry, actions, and keybindings
- `src/app.rs` — main window: toolbar, editor, results table, status bar
- `src/db.rs` — Postgres execution (blocking `postgres` client on a background thread)
- `src/results.rs` — table delegate rendering the result set
- `src/ai.rs` — Claude Messages API client for completions
