# pg-gui

A small desktop app for editing and executing PostgreSQL scripts, built with
[GPUI](https://www.gpui.rs) (Zed's UI framework) and
[gpui-component](https://github.com/longbridge/gpui-component).

![pg-gui editing a SQL script](assets/screen1.png)

## Features

- **SQL editor** with tree-sitter syntax highlighting, line numbers, and a monospace theme
- **Language support** from the embedded
  [Postgres Language Server](https://github.com/supabase-community/postgres-language-server):
  schema-aware completions, hover documentation, diagnostics, and script
  formatting (`cmd-shift-f`), all driven in-process — no external binary
- **Execute scripts** against any PostgreSQL server (`cmd-enter` or
  Connection ▸ Run Query); if text is selected in the editor, only the
  selection runs, otherwise the statement under the cursor (which gets
  selected so you can see what ran). Multi-statement selections are supported
  via the simple query protocol, and the last result set is shown in a
  virtualized table (handles large result sets). A single SELECT runs
  through a server-side cursor: only the first batch of rows is
  transferred (`fetch_size` in config.json, default 500) and a Fetch more
  button under the table pulls the next batch on demand
- **Export results** to a file (Connection ▸ Export as CSV… / Export as
  INSERT…): re-runs the selection or the statement at the cursor and writes
  every row — CSV is produced server-side via `COPY (…) TO STDOUT` so the
  server does the quoting, INSERT generates a runnable SQL script. The save
  dialog suggests `<table>_<date>` from the query's `FROM` table
- **Connection manager**: create and edit connections in a per-field dialog
  with an inline Test Connection check, give them names, and switch between
  them from the combobox in the title bar or the Connection ▸ Recent menu
- **Open / save** `.sql` files with native file dialogs (`cmd-o` / `cmd-s`)
  that start in the last used directory,
  each open script in its own editor tab; a tab with unsaved edits is marked
  with a `•` and prompts to save before it's closed (and before quitting),
  and an open script that changes on disk offers to reload
- **Snippets** (`cmd-p`): a searchable picker of built-in queries — including
  `New:` templates for tables, views, triggers, sequences and more — extendable
  with your own `.sql` files in the config directory's `snippets/` folder.
  Snippets can carry numbered tab stops (`$1`, `${2:placeholder}`); after
  inserting, `tab` visits each stop in order with its placeholder selected.
  Snippets also appear in the editor's completion menu: type words of the
  name or the SQL's leading words ("create seq…") and accept the suggestion
- **Themes**: Catppuccin Mocha (dark) and Latte (light), switchable from
  View ▸ Theme (light / dark / follow the system)
- **AI completion** (optional): completes the SQL at the cursor using the Claude API
  (`cmd-i` or `ctrl-space`)

## Running

```sh
cargo run
```

The first build is slow: the embedded language server compiles `libpg_query`
from source, which needs a working C toolchain and libclang.

The connection and the open editor tabs are remembered between launches:
both are saved to `~/Library/Application Support/pg-gui/config.json` (the platform
config directory) as you type — the scripts with a short debounce — and restored on
the next start, unsaved edits included. Each tab's `.sql` file path is remembered
too, so `cmd-s` keeps writing to the same file after a restart. The config also
holds the recent-connections list, the theme, the zoom level, and a few
options: `format_on_save` (off by default), `keyword_case` / `constant_case`
(`"lower"`/`"upper"`, used by the formatter), and the AI settings
(`ai_api_key`, `ai_model`, `ai_prompt`).
`cmd-,` (Preferences…) opens the file in the system editor.
A `DATABASE_URL` environment variable, if set, overrides the saved connection
string for that launch; with neither present the field defaults to
`postgres://$USER@localhost:5432/postgres`. TLS connections are not supported yet
(the client connects with `NoTls`).

Only one instance runs at a time (two would race for `config.json`): launching
the app while it's already open just raises the running window.

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

Set an API key to enable Edit ▸ AI Complete: either `ai_api_key` in
`config.json` or the `ANTHROPIC_API_KEY` environment variable:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cargo run
```

Place the cursor where you want a completion and press `cmd-i`. The model receives the
text before and after the cursor and inserts the completion at the cursor.

- Default model: `claude-opus-4-8` — override with `ai_model` in `config.json`
  or the `PG_GUI_AI_MODEL` environment variable (the config wins;
  e.g. `claude-haiku-4-5` for lower latency).
- `ai_prompt` in `config.json`, when set, is appended to the system prompt —
  useful for schema hints or style preferences
  (e.g. `"Prefer explicit JOINs and snake_case aliases."`).

## Keybindings

Every command also lives in the application menu bar. Press `cmd-h` (macOS)
or `F1` (Linux) in the app to see this list in a dialog. On Linux, `cmd` is
`ctrl` throughout.

| Key | Action |
| --- | --- |
| `cmd-enter` / `ctrl-enter` | Run the selection or the statement at the cursor |
| `cmd-i` / `ctrl-space` | AI complete at cursor |
| `cmd-shift-f` | Format the script |
| `cmd-/` | Comment or uncomment the line / selection |
| `cmd-p` | Insert a snippet |
| `cmd-t` | New script tab |
| `cmd-w` | Close the tab |
| `ctrl-tab` / `ctrl-shift-tab` | Next / previous tab |
| `cmd-o` | Open a `.sql` file |
| `cmd-s` | Save (Save As on first save) |
| `cmd-,` | Open `config.json` in the system editor |
| `cmd-plus` / `cmd-minus` | Zoom in / out |
| `cmd-0` | Reset zoom |
| `cmd-h` / `F1` | Show the command help dialog |
| `cmd-q` | Quit |

## Code layout

- `src/main.rs` — app entry, actions, keybindings, and theme loading
- `src/app.rs` — main window: menu bar, editor, results table, status bar, dialogs
- `src/config.rs` — `config.json` load/save (connections, tabs, options)
- `src/db.rs` — Postgres execution (blocking `postgres` client on a background thread)
- `src/lsp.rs` — embedded Postgres Language Server (completions, hover, diagnostics, formatting)
- `src/export.rs` — rendering results for export (CSV via `COPY`, INSERT scripts)
- `src/results.rs` — table delegate rendering the result set
- `src/statement.rs` — locating the SQL statement under the cursor
- `src/snippets.rs` — built-in and user snippet library
- `src/ai.rs` — Claude Messages API client for completions
