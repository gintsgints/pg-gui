# Design

## Context

See proposal.md — Why. The mechanics that shape the approach:

- Execution already streams `db::RunEvent`s (`Log`, `Notice`, `Result`) over an
  unbounded channel that `app.rs` drains on the UI thread while the run
  proceeds on a background thread. The channel exists; it just carries nothing
  about work *starting*.
- `db::Session::run` splits the SQL into statement ranges up front, so the total
  is known before the first statement executes; today that number surfaces only
  in the final `RunResult`.
- Script batches are driven by `run_files` in `app.rs` (not `db.rs`), which
  loops files and calls `Session::run` per file. `db.rs` has no concept of a
  file.
- Running state is already per tab (`TabState.running`, `TabState.cancel`), but
  the message the user sees is one app-wide `self.status: SharedString` that any
  run overwrites.
- gpui-component ships `spinner::Spinner`, which animates itself through gpui's
  `Animation`; it needs no timer from us. The elapsed-time *text* does need one.
- The app already has the timer idiom: `cx.background_executor().timer(..)` in a
  detached `cx.spawn_in` loop (`watch_files`, `schedule_save`).

## Goals / Non-Goals

**Goals:**

- One indicator model that every execution path feeds (statement run, script
  batch, `EXPLAIN`, fetch-more, export), so none of them can silently show
  nothing.
- Progress reported *before* work runs, not after it completes.
- Progress owned by the tab, so the status bar is a view of the active tab.

**Non-Goals:**

- Intra-statement progress. Postgres gives no row-level progress for a running
  statement over this protocol; a long single statement shows a spinner and a
  clock, nothing finer.
- Cancelling, transaction handling, or connection lifetime — untouched.
- New fork patches on `my_fixes3`; this uses gpui-component as published.

## Decisions

### Report statement starts as a new `RunEvent`, emitted from `db.rs`

Add a variant carrying the 1-based index and the run's total
(`Statement { n: usize, total: usize }`), sent from `Session::run`'s loop
immediately before each `run_statement` call.

*Why here:* `run` already owns the split and the ordering, so the number
reported is by construction the statement that is about to execute.

*Alternative rejected:* have `app.rs` pre-split the SQL with
`statement::ranges` and count locally. That duplicates the split and would drift
from what `db.rs` actually executes (they are separate splitters), producing a
counter that can disagree with the log.

### Report the script file from `run_files`, not from `db.rs`

Add a second variant (`File(String)`) that `run_files` sends before each file's
`Session::run`, carrying the same label the `▶ <path>` log line uses.

*Why:* `db.rs` deliberately knows nothing about files. Keeping the file event in
`app.rs` leaves that boundary intact and costs one variant.

*Alternative rejected:* deriving the file from the existing `▶ ` log line by
parsing it back out. Fragile, and couples display text to control flow.

### Per-tab progress state, with the status bar as a view of the active tab

Give `TabState` an `Option<RunProgress>` holding: the scope label, the
`Instant` the run started, the current file (script batches only), and the
current `(n, total)` statement pair. Set on start, updated by the new events in
`on_run_event`, cleared on every terminal path (`on_query_ok`,
`on_query_error`, the explain/fetch/export completions).

`self.status` stays as it is for one-shot messages ("Nothing to run", "Cancel
failed: …"); when the active tab has a `RunProgress`, the status bar renders the
indicator instead.

*Why:* the spec requires a background tab's run not to hijack the status line,
and requires the elapsed time to survive switching tabs — both fall out of
storing the start `Instant` on the tab rather than formatting a string at kick-off.

*Alternative rejected:* a single app-level "current run" slot. Simpler, but
wrong the moment two tabs run at once, which is already possible today.

### One shared 200 ms tick, alive only while something runs

When a run starts and no tick is running, spawn a detached loop that waits 200 ms
and calls `cx.notify()`; it exits when no tab has a `RunProgress` (guarded by a
flag on the app so starts do not stack loops).

*Why 200 ms:* the spec's floor is one update per second; 200 ms makes a
tenth-of-a-second clock read smoothly. `notify` alone is cheap — it re-renders
the window, which gpui already does per frame while the spinner animates anyway.

*Alternative rejected:* reusing the existing 1 s `watch_files` loop. It does file
IO per tick, its period is exactly the spec's floor, and it runs forever;
coupling the clock to it would mean either doing file IO five times more often or
a clock that visibly stutters.

### Elapsed time formatted for reading, not `Duration`'s `Debug`

Render as `0.4s`, `12.3s`, `1m 05s`. The completion lines keep their existing
`{elapsed:.0?}` formatting — those are log records, not a ticking clock.

### Cancel prominence by button variant, not a new control

While the active tab runs, the cancel button renders `.danger()` (filled);
when idle it renders `.ghost()` and stays disabled. Same id, same glyph, same
tooltip, same action.

*Why:* today it is always `.danger()` and the only difference is the dimming
that disabling applies — which reads as "greyed out" in both states. Swapping the
variant makes the running state unmistakable without adding UI.

### Debug panel: a `busy` flag on the session state

`DebugState` gains `busy: bool`, set when the session starts and when a
step/continue command is sent, cleared on the next stop, on termination, and on
error. The spinner renders in `render_debug_status` before the first stop and in
the debug toolbar row afterwards, so stepping shows it too.

*Why a flag rather than deriving it:* after the first stop, "working" is not
derivable from `stop`/`terminated` — the previous stop's state is still on
screen while the next step is in flight. `src/debug.rs`'s protocol is unchanged;
the flag is set where commands are sent and cleared where events arrive.

## Risks / Trade-offs

- **New `RunEvent` variants break every exhaustive match** (`on_run_event`, the
  `db.rs` test harness) → compile-time failure, not a runtime surprise; both
  sites are known and updated in the same change.
- **A 200 ms notify loop re-renders while a big result table is on screen** →
  the loop exists only while a run is in flight, and during a run the table is
  empty or being replaced anyway. If it proves costly, the period is a single
  constant.
- **Statement counts come from `db.rs`'s splitter, the log's numbering from the
  same place** → they agree by construction; the risk is only that the count
  disagrees with the editor's own `statement::ranges` view of the buffer, which
  is pre-existing and unchanged.
- **Clock and spinner keep animating if a run leaks without a terminal event**
  → every exit path already flips `TabState.running`; the progress field is
  cleared at exactly those points, and the tick loop exits when no tab has
  progress, so a leak shows as a stuck clock rather than a busy loop.
- **`Instant`-based elapsed ignores wall-clock jumps** → intended; monotonic is
  the right clock for "how long has this been running".

## Migration Plan

Not applicable: no persisted state, no config keys, no protocol shared with
anything outside the process. The change is additive to `RunEvent` and to
`TabState`; reverting it is a revert.
