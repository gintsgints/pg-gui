# Proposal

## Why

While SQL is executing, pg-gui shows almost nothing. The status bar gets one
static line (`Running statement…`) written once at kick-off, the ⊘ Cancel
button flips from disabled to enabled — a change most users never notice — and
the per-statement log lines only appear *after* each statement finishes, in a
view (cmd-3 Log) that is usually not the one on screen. A single long statement
therefore produces a frozen-looking window with an emptied results table, and a
user cannot tell a slow query from a hung app, nor how long it has been going.

## What Changes

- The status bar shows a **live** running indicator while a statement, script
  run, EXPLAIN, page fetch or export is in flight: an animated spinner, the
  scope being run, and an elapsed time that ticks while it runs.
- For a multi-statement run, the indicator counts **statements as they start**
  (`statement 3/12`), not after they finish. This needs a new progress event —
  `db::run` already knows the statement count up front but never reports it.
- Script batches additionally name the file currently executing.
- The **Cancel** control becomes visually active while a query runs rather than
  merely enabled, so the way to stop a long query is obvious.
- The **debug panel** gets the same animated spinner while a debug session is
  working and not stopped at a line (connecting, waiting for the target to
  trap, and after each Step/Continue until the next stop).
- Indicators are **per tab**: a run started in a background tab keeps its own
  progress, and the status bar reflects the tab the user is looking at rather
  than being overwritten by whichever run reported last.

Not in scope (considered and left out): a tab-bar running marker, and a busy
placeholder in the results panel.

## Capabilities

### New Capabilities
- `query-execution-feedback`: what the UI must show while SQL (or a debug
  session) is executing — the running indicator's content, its liveness, its
  per-tab scoping, progress counting across statements and script files, the
  Cancel affordance, and how the indicator is cleared on success, failure and
  cancellation.

### Modified Capabilities
<!-- None: the project has no existing specs. -->

## Impact

- `src/db.rs` — a new `RunEvent` variant reporting a statement (and, for script
  runs, a file) as it starts, emitted from `run`/`run_statement`; the run's
  total statement count travels with it. Existing `RunEvent` consumers
  (`app.rs`, `db.rs` tests) must handle the new variant.
- `src/app.rs` — per-tab progress state on `TabState`, a ticking timer while any
  run is in flight, `render_status_bar` rebuilt around the indicator,
  `render_session_toolbar`'s Cancel button styling, `on_run_event`, and the
  starts/finishes of `run_query`, `start_script_run`, `start_explain`,
  `fetch_more` and the export path.
- Debug panel rendering (`render_debug_status`, the debug toolbar) in
  `src/app.rs`; no change to `src/debug.rs`'s protocol.
- UI dependency: uses gpui-component's existing spinner/loading affordances
  (`spinner::Spinner`, `Button::loading`) — no new fork patch on `my_fixes3`.
- No change to connection handling, transactions, cancellation semantics, or
  the LSP.
