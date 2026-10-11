# Language support

Completions, hover documentation, diagnostics and script formatting come from
the [Postgres Language Server](https://github.com/supabase-community/postgres-language-server),
embedded as a **library** rather than spawned as a binary. There is no
external process, no `postgrestools` binary and no language-server config file
to maintain.

Because the server is in-process, it is configured from the connection you are
on: pg-gui derives the host, port, user and database from the active
connection string and hands them to the server, which uses them to load the
schema cache that schema-aware completions need.

## What you get

- **Completions** as you type — tables, columns, schemas, functions and
  keywords, plus [snippet](snippets.md) suggestions matched against the words
  before the cursor. While the server has no database, the snippet suggestions
  are still offered on their own.
- **Hover** documentation for the object under the pointer.
- **Diagnostics** — syntax and type errors, pulled on a debounce after each
  edit and drawn in the editor.
- **Formatting** with `cmd-shift-f`, or automatically on save.

## Schema status in the status bar

A completion list of keywords with no schema items looks exactly like a
working server, so the status bar reports what the schema cache is actually
doing:

| Segment | Meaning |
| --- | --- |
| `SQL LSP: schema loading` | The cache has not finished loading yet |
| `SQL LSP: schema loaded` | Schema-aware completions are live |
| `SQL LSP: no schema — database unreachable` | The server could not connect |
| `SQL LSP: no schema — no database` | The connection string has no usable database |

The check repeats until the schema loads, so starting the database after the
app still flips the segment without a reconnect.

## Formatting

Format-on-save is **on** by default (`format_on_save` in `config.json`) and
can be toggled at runtime from the clickable `fmt: on` / `fmt: off` segment in
the status bar or from Edit ▸ Format on Save.

Casing is configurable: `keyword_case` and `constant_case` in `config.json`
take `"lower"` (the default) or `"upper"`.

## Diagnosing it

`src/lsp.rs` logs the boundary itself — the derived configuration (never the
password), each document sync and every completion, hover and diagnostics
answer, with completions tallied by item kind. Pair it with the server's own
events:

```sh
PG_GUI_LOG=warn,pg_gui=debug,pgls_workspace=debug,pgls_completions=debug cargo run
```

`pg_gui=trace` additionally logs the text before the cursor and the labels
that came back.

## Build note

The embedded server builds `libpg_query` from source via `bindgen`/`cc`, so
the first build is slow and needs a working C toolchain and libclang.
