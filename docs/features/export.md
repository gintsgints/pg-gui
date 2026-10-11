# Exporting results

Connection ▸ **Export as CSV…** / **Export as INSERT…** writes a query's
result to a file.

An export does not write what the table happens to be holding: it re-runs the
selection, or the statement at the cursor, and writes **every** row. A result
that was fetched one 500-row batch at a time
([cursors](running-queries.md)) exports in full without you pressing Fetch
more first.

## The two formats

- **CSV** is produced server-side via `COPY (…) TO STDOUT`, so Postgres does
  the quoting and escaping — the file matches what any other Postgres tool
  would produce for the same rows.
- **INSERT** generates a runnable SQL script, which is the format you want for
  seeding another database or committing a fixture.

## The save dialog

The dialog suggests `<table>_<date>` as the file name, taking the table from
the query's `FROM` clause, and opens in the last directory you used.
