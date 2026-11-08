# Connection manager

Connections are created and edited in a per-field dialog — host, port, user,
password, database, schema — with an inline **Test Connection** check, so a typo is
caught in the dialog instead of on the first query.

The **Schema** field sets the session's `search_path` (comma-separated, e.g.
`app,public`; empty keeps the server default). It is stored in the connection
string as `options=-c search_path=…`, so every connection the app opens —
queries, the browser, the language server — sees the same schemas. A pasted
string in the `--search_path=…` form is read into the field too.

Under the fields the dialog shows the connection string they add up to, with
the password replaced by `****`. The copy button at its right puts the full
string — password included — on the clipboard, ready for `psql` or another
tool.

A connection can be given a **name**. Switch between saved connections from
the combobox in the title bar or from Connection ▸ Recent.

Connection ▸ **Copy Connection…** opens the same dialog seeded with the current
connection's details — password included — and saves the result as a new
entry, so a second database on the same server, or the same database as
another user, is one field away. A named connection's copy starts as
`<name> (copy)`, since keeping the original name would overwrite the
original.

## A connection is its name

A saved connection is identified by its name (or by its URL when it has none),
not by its connection string. Editing a connection is how its URL changes, so
re-pointing a connection at another server updates the one entry instead of
adding a second, and renaming it keeps everything that belongs to it. Two
entries pointing at the same server stay distinguishable in the combobox.

## What switching connection does

Untitled tabs and query output belong to the connection, not to the window:

- **Untitled tabs are parked.** They have no file, so nothing can be lost and
  nothing needs prompting; they come back — in the tab-bar slot they were
  taken from — when you return to that connection. The scripts are persisted
  in `config.json`, because for an untitled tab that is the only copy of the
  text.
- **Every tab's results are parked** too — rows, log and plan alike — so a
  tab's [message log](message-log.md) never describes a server it was not run
  against.
- **A tab backed by a file stays open** throughout. Only its results swap.

Editing the connection you are currently on is not a switch: the tabs and the
output stay put.

Closing an untitled tab *does* lose its script, so that gets a prompt — with
Save As — even though parking one does not. A quit keeps every untitled
buffer, parked or not.
