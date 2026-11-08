# Changelog

## v0.10.0

### New features

- **Multi-mode palette** (`cmd-p`) — find a file in the working folder; `>` switches to snippets, `:` to commands (`cmd-shift-p`)
- **Next / previous problem** (`cmd-j` / `cmd-k`) — jump between diagnostics, errors first
- **Multi-row result selection** (`shift-click`, `cmd-click`, `cmd-a`) — select a range, toggle rows or the whole result across pages, and copy it
- **Connection schema** — a Schema field in the connection dialog sets the session's `search_path`
- **Copy Connection** — create a new connection seeded from the current one
- **Masked connection string** — the dialog shows the URL with the password hidden; the copy button still copies it in full
- **Connections keyed by name** — editing or renaming a connection updates one entry instead of adding another
- **Tabs and output per connection** — switching connection parks untitled tabs and every tab's results, and brings them back on return
- **Tab overflow menu** (`» N`) — lists tabs clipped by a narrow window
- **Files panel context menu** — New Script…, New Folder…, Rename…, Delete…, Open and Run Script on each row
- **New Folder and in-app script naming** — create folders and name new scripts without a native dialog
- **Filter clear buttons** — one click empties the database and files panel filters
- **Table columns in the object browser** — expand a table to list its columns
- **Schema-aware language server** — the status bar reports whether the schema cache loaded, and the cache reloads after a refresh or committed DDL
- **Richer completion list** — each item shows its kind, type and schema beside the label
- **Running indicator** — a live indicator while SQL executes, and the log follows its last line during a script run
- **Result size cap** — non-paged result sets stop at `max_result_rows`
- **AI completion retries** — rate-limited requests are retried; empty result sets get a hint
- **Diagnostics log** — `pg-gui.log` next to `config.json`, filtered with `PG_GUI_LOG`; language-server requests and answers can be traced

### Fixes

- Pass the workspace project key to every language-server request
- Keep the database browser's selection, and a filtered match's subtree, across filter changes
- Complete `create view` to the view template
- Stopping the debugger reaches a controller blocked in `pldbg_*` calls
- Highlight positional parameters like `$1`, and PL/pgSQL keywords the SQL grammar misses
- Log text is selectable and copyable
- Show the log when a run returns no rows
- Show the full query in the long-running query snippet
- Completions no longer go silent when the schema cache fails to load
- Log only errors by default

### Other

- `PgGuiApp` split into per-domain modules; one String/anyhow error boundary
- End-to-end UI test suite (`cargo e2e`) covering editor, results, connections, files panel, debugger, explain, export and more
- gpui-component moved to the gpui-kit fork (`my_fixes4`) on gpui-pre 0.3.11
- Feature documentation split into `docs/features/`
