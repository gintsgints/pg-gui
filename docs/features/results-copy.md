# Copying from the results grid

`cmd-c` copies out of the results table. What lands on the clipboard depends
on what you clicked first:

| Click | `cmd-c` copies |
| --- | --- |
| A cell | That cell's value |
| A row header | The whole row, cells joined by tabs |
| A column header | The whole column, one value per line — the current page only |

A column copy covers the rows currently fetched, not the rows a cursor has
not pulled yet; use **Fetch more** first, or
[export the result](export.md) instead, which re-runs the query and writes
every row.

A SQL `NULL` copies as the empty string, so a tab-joined row stays aligned
with its columns.
