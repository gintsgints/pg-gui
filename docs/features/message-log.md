# Message log

`cmd-3` shows the panel under the editor; the **Log** switch selects the
message log beside the results table and the [query plan](query-plan.md). The
log is per tab, like the results are.

It carries one line per statement with its timing, so a multi-statement run
is accountable statement by statement even though only the last result set is
shown in the table.

## Server notices

Postgres sends `RAISE NOTICE` / `WARNING` / `INFO` output and deprecation
warnings **out of band** — result sets never carry them, so a script whose
whole output is `RAISE NOTICE` looks silent. pg-gui installs a notice callback
on every connection that runs user SQL and drains it after each statement,
including failed ones, so those lines appear in the log next to the statement
that raised them.

A routine being [stepped in the debugger](debugging.md) reports each `RAISE`
as it happens, into the log of the tab the session was launched from — the
debug panel has replaced the results table by then.

## Script runs

A [batch run from the files panel](run-scripts.md) heads each file with a
`▶ <path>` line, because statement numbering restarts per file and the lines
below it would otherwise be unattributable.

## Not the same as the diagnostics log

This log is the server's and the session's own output. The app's diagnostics
log — `pg-gui.log` next to `config.json`, controlled by `PG_GUI_LOG` — is a
different thing, described in the README's Logging section.
