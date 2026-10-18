# PL/pgSQL step debugging

`cmd-shift-d` starts a debug session on a PL/pgSQL routine, driving the
server's [pldebugger](https://github.com/EnterpriseDB/pldebugger) extension.

## Requirements

- The `pldbgapi` extension installed in the database
  (`sql/public/extensions/R.0.01.02.1__pldbgapi.sql`).
- `plugin_debugger` in the server's `shared_preload_libraries`.
- The routine must be `LANGUAGE plpgsql`.

Both of the setups that can never trap — a non-plpgsql routine, and a server
without `plugin_debugger` — are rejected up front with an explanation, rather
than arming a session that would silently run to completion. The test database
in `docker-compose.yml` satisfies all of it out of the box.

## Starting a session

The launch dialog takes the routine (pre-filled from the statement at the
cursor), its arguments, and a stop-on-entry toggle. pg-gui builds the right
call for what the routine is: `CALL` for a procedure, `SELECT * FROM f(..)`
for a set-returning function, a scalar `SELECT` otherwise.

Debugging needs two connections that both block — a controller that arms
breakpoints and steps, and a target that runs the routine and stays trapped in
the backend — so it all happens off the UI thread. Before the first stop, a
progress pane shows what the session is doing, so a run that never traps is
visible rather than a frozen panel.

## Stepping

| Key | Action |
| --- | --- |
| `F10` | Step over |
| `F11` | Step into |
| `F5` | Continue |
| `shift-F5` | Stop debugging |

Breakpoints are set by clicking the editor's gutter — that is the only
breakpoint UI — and toggling one mid-run arms or drops it on the live session.

## The debug panel

While a session is active, the panel under the editor replaces the results
table and shows:

- **Variables** — click one to change its value in the running frame.
- **Call stack** — click a frame to inspect it. The variables shown are
  always the *selected* frame's, not necessarily the innermost one.
- A **Step Over / Into / Continue / Stop** toolbar, with a spinner marking the
  session as working rather than parked (between stops the previous stop's
  variables and stack stay on screen, so the spinner is what tells them
  apart).

The stepped source is read in the **editor**, not in the panel: each stop
selects the executing line in the tab the session was launched from and
scrolls it into view. A routine stepped into from elsewhere highlights
nothing rather than highlighting an unrelated line — line numbers are
body-relative to the routine's source as the server reports it, with no
on-disk source mapping yet. See
[routine-relative gutter numbering](sql-editor.md#routine-relative-line-numbers),
which makes the editor's numbers agree with the debugger's.

Server `RAISE` output from the debugged routine goes to the launching tab's
[message log](message-log.md) as it happens.

## Also available from

The Debug menu, with the same four commands.

## Reference

[`docs/debug/EXAMPLE.md`](../debug/EXAMPLE.md) walks the raw `pldbg_*` call
flow used to debug the sample `place_order` procedure.
