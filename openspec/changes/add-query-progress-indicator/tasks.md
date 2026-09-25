# Tasks

## 1. Progress events from the execution layer

- [x] 1.1 Add a `RunEvent::Statement { n, total }` variant in `src/db.rs` and send it from `Session::run`'s loop immediately before each `run_statement` call, carrying the 1-based index and `ranges.len()`; verify `cargo clippy --all-targets -- -W clippy::pedantic -D warnings` passes with no non-exhaustive-match errors left anywhere.
- [x] 1.2 Add a `RunEvent::File(String)` variant and send it from `run_files` in `src/app.rs` before each file's `Session::run`, using the same label as the existing `▶ <label>` log line; verify the batch still logs one `▶` line per file and clippy passes.
- [x] 1.3 Update the `db::tests::run` harness to collect the two new variants (into a `statements: Vec<(usize, usize)>` and `files: Vec<String>` on its `Run` struct) rather than ignoring them; verify `cargo test` compiles the test module (the DB-backed tests themselves need the local Postgres running — ask the user to start it before relying on their results).
- [x] 1.4 Add a DB-backed test asserting a 3-statement run reports `(1,3) (2,3) (3,3)` in order and before the corresponding log lines; verify by asking the user to run `cargo test` with the docker Postgres up, and record the outcome.

## 2. Per-tab progress state and its lifecycle

- [x] 2.1 Add a `RunProgress` struct (scope label, `started: Instant`, `file: Option<String>`, `statement: Option<(usize, usize)>`) and a `progress: Option<RunProgress>` field on `TabState`; verify it compiles and clippy passes.
- [x] 2.2 Set `progress` at the start of `run_query`, `start_script_run`, `start_explain`, `fetch_more` and the export path with the scope label each already passes to `set_status`, and clear it in `on_query_ok`, `on_query_error`, the explain/fetch/export completion handlers and `cancel_query`'s terminal path; verify by grepping that every site setting `running = true`/`set_tab_running(.., true)` has a matching progress set, and every site clearing `running` clears progress.
- [x] 2.3 Handle `RunEvent::Statement` and `RunEvent::File` in `on_run_event` by updating the tab's `RunProgress` (and not appending to the message log); verify the message log after a multi-statement run contains exactly the lines it contains today.
- [x] 2.4 Make the tab switch path re-render the status bar so the indicator follows the active tab; verify by starting a long run in one tab, switching away and back, and confirming the elapsed time continues from the original start rather than restarting.

## 3. Status bar indicator

- [x] 3.1 Add an elapsed-time formatting helper (`0.4s`, `12.3s`, `1m 05s`) in `src/app.rs`; verify with unit tests in the existing `app::tests` module covering sub-second, seconds, and past-a-minute values (no DB needed).
- [x] 3.2 Rebuild `render_status_bar` so that when the active tab has a `RunProgress` it renders `spinner::Spinner` + scope + `file` (when set) + `n/total` (only when `total > 1`) + elapsed, and otherwise renders `self.status` exactly as today; verify the idle status bar is unchanged and clippy passes.
- [x] 3.3 Add the shared tick: a detached `cx.spawn_in` loop waiting 200 ms and calling `cx.notify()`, started when a run begins if not already running, exiting once no tab has a `RunProgress`, guarded by a flag so concurrent starts do not stack loops; verify by starting two runs in two tabs and confirming exactly one loop is alive (log or debug-assert on the flag) and that it exits after both finish.
- [x] 3.4 Verify end to end against the spec's indicator scenarios: with the docker Postgres up (user-run), `select pg_sleep(30)` shows spinner and a clock advancing at least once a second, and a fast statement leaves no stale indicator.

## 4. Cancel affordance

- [x] 4.1 In `render_session_toolbar`, render the cancel button `.danger()` while the active tab runs and `.ghost()` + disabled when it does not, keeping its id, glyph, tooltip and action; verify visually that the two states are distinguishable and that cancelling a `pg_sleep` still works.

## 5. Debug panel activity indicator

- [x] 5.1 Add `busy: bool` to `DebugState`, set it when the session is started and when a step/continue command is sent, and clear it on every stop, on termination and on error; verify by reading the debug event handlers that no path leaves it set after the session settles.
- [x] 5.2 Render `Spinner` next to the status text in `render_debug_status` and in the debug toolbar row while `busy`; verify against the spec's debug scenarios — spinner while waiting for the target to trap, gone at a stop, back while stepping, gone once terminated (needs `pldbgapi` and `plugin_debugger`, so ask the user to run it).
- [x] 5.3 Update the debug section of `CLAUDE.md` if the busy flag changes what that section describes about the panel's pre-first-stop pane; verify the section still matches the code.

## 6. Integration

- [x] 6.1 Run `cargo fmt` and `cargo clippy --all-targets -- -W clippy::pedantic -D warnings` over the finished change and fix every finding without broad `#[allow]`; verify both commands exit clean.
- [x] 6.2 Walk the remaining spec scenarios that span groups — two tabs running at once showing independent clocks, a script batch naming its current file with per-file statement counts, and indicator clearing on error and on cancellation — with the docker Postgres up (user-run); record each outcome.
