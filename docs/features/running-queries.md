# Executing scripts

`cmd-enter` (or Connection ▸ Run Query) runs SQL against the connection the
window is on.

## What runs

- If text is **selected** in the editor, only the selection runs.
- Otherwise the **statement under the cursor** runs, and it gets selected so
  you can see what went to the server.

Multi-statement selections are supported through the simple query protocol.
Each statement gets its own line in the [message log](message-log.md); the
last result set is the one shown in the table.

## The results table

The table is virtualized, so a large result set does not cost anything to
draw. Columns are sized to their content — the header plus the first rows of
each — and a result of four columns or fewer stretches to fill the pane
instead of leaving an empty strip beside it.

See [Copying from the results grid](results-copy.md) for getting values back
out.

## Cursors and Fetch more

A single `SELECT` runs through a server-side cursor: only the first batch of
rows is transferred (`fetch_size` in `config.json`, default 500) and a **Fetch
more** button under the table pulls the next batch on demand. Nothing is
loaded that you did not ask to see.

Anything else that returns rows — DML with `RETURNING`, a `SELECT` inside a
multi-statement block, a `SELECT` the cursor rejects — arrives in one piece,
so it is capped instead: at most `max_result_rows` rows are kept (in
`config.json`, default 100000; `0` keeps every row). A capped set says so
twice — its log line ends in `note: result truncated at N rows`, and the
status bar reads `showing N of M row(s)` with the server's full count. The cap
limits what a result set holds for as long as its tab shows it; the rows still
cross the wire, so a huge statement is better narrowed (or run alone, through
the cursor) than capped.

## Cancelling

A long-running statement can be stopped with Connection ▸ Cancel Query, which
cancels on the server rather than just abandoning the result. The same cancel
token covers [EXPLAIN runs](query-plan.md), so a slow `EXPLAIN (ANALYZE)` is
cancellable too.

## Sessions

Each tab holds its own live session, so its transaction state and autocommit
setting are its own — see [Transactions](transactions.md).

Query output belongs to the connection, not to the window. Switching
connection parks every tab's rows, log and plan under the outgoing
connection's key and brings back whatever the incoming one last had, so a
tab's log never describes a server it was not run against.
