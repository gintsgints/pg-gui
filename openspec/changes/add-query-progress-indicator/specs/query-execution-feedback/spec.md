# Spec Delta

## Purpose

Defines what the application shows while SQL or a debug session is executing, so
a user can always tell that work is in flight, roughly how far it has got, how
long it has been running, and how to stop it.

## ADDED Requirements

### Requirement: Live running indicator

While a tab has SQL execution in flight — a run of the statement under the
cursor, a script batch, an `EXPLAIN`, a fetch of further rows, or an export of
the result set — the status bar SHALL show a running indicator consisting of an
animated element, a description of what is running, and the time elapsed since
that work started. The elapsed time SHALL advance at least once per second while
the work is in flight, without user interaction.

#### Scenario: Long single statement
- **WHEN** a statement that takes 30 seconds is run
- **THEN** the status bar shows the animated element and a description of the
  statement being run from the moment the run starts
- **AND** the displayed elapsed time increases at least once per second for the
  whole 30 seconds

#### Scenario: Fast statement
- **WHEN** a statement completes in well under a second
- **THEN** the indicator is replaced by the run's completion message, and no
  stale indicator remains

#### Scenario: Nothing running
- **WHEN** no execution is in flight in the active tab
- **THEN** the status bar shows no animated element and no elapsed time

### Requirement: Statement progress within a run

When a run contains more than one statement, the indicator SHALL report the
statement about to be executed and the total number of statements in the run,
and SHALL update at the moment each statement starts rather than when it
finishes. A run of a single statement SHALL NOT display a count.

#### Scenario: Multi-statement run
- **WHEN** a run of 12 statements is started and the third statement is
  executing
- **THEN** the indicator identifies the running statement as the 3rd of 12

#### Scenario: Progress precedes completion
- **WHEN** a statement in a multi-statement run takes 10 seconds
- **THEN** the indicator identifies that statement as the running one for the
  whole 10 seconds, before any log line for it is produced

#### Scenario: Single statement
- **WHEN** a run contains exactly one statement
- **THEN** the indicator shows no statement count

### Requirement: Script batch progress

When a batch of script files is run, the indicator SHALL name the file being
executed in addition to the statement progress within that file.

#### Scenario: Running a batch of scripts
- **WHEN** three selected script files are run and the second file is executing
  its first statement
- **THEN** the indicator names the second file and reports its statement
  progress

### Requirement: Indicator is per tab

Execution progress SHALL be tracked per tab. The status bar SHALL show the
progress of the tab currently in view, and work in flight in another tab SHALL
NOT replace it.

#### Scenario: Switching away from a running tab
- **WHEN** a long run is started in one tab and the user switches to another tab
  where nothing is running
- **THEN** the status bar no longer shows a running indicator
- **AND** switching back to the running tab shows its indicator again, with the
  elapsed time still counting from when that run started

#### Scenario: Two tabs running
- **WHEN** runs are in flight in two tabs at once
- **THEN** each tab's indicator reports its own progress and its own elapsed
  time, and the visible one is the active tab's

### Requirement: Cancel affordance is visible while running

The control that cancels the running query SHALL be visually distinguishable as
available while execution is in flight, not merely enabled, and SHALL return to
its resting appearance when nothing is running.

#### Scenario: Query in flight
- **WHEN** a statement is executing in the active tab
- **THEN** the cancel control is presented as active and remains operable

#### Scenario: Idle tab
- **WHEN** no execution is in flight in the active tab
- **THEN** the cancel control is presented at rest and is not operable

### Requirement: Indicator clears on every outcome

The running indicator SHALL be cleared when the work ends, whichever way it
ends: success, server or connection error, or cancellation. The message left in
its place SHALL describe that outcome.

#### Scenario: Statement fails
- **WHEN** a running statement fails with a server error
- **THEN** the indicator is cleared and the error is reported

#### Scenario: User cancels
- **WHEN** the user cancels a running query
- **THEN** the indicator reports that cancellation is in progress, and is
  cleared once the run has actually ended

#### Scenario: Connection lost
- **WHEN** the connection drops while a statement is running
- **THEN** the indicator is cleared and the failure is reported

### Requirement: Debug session activity indicator

While a debug session is working and not stopped at a line — establishing the
session, waiting for the target routine to be entered, or resuming after a step
or continue — the debug panel SHALL show an animated element alongside its
status text. The animated element SHALL disappear once the session stops at a
line, ends, or terminates.

#### Scenario: Waiting for the target to trap
- **WHEN** a debug session has been started and the debugged routine has not yet
  been entered
- **THEN** the debug panel shows an animated element next to its progress text

#### Scenario: Stopped at a line
- **WHEN** the session stops at a line and its variables and call stack are shown
- **THEN** no animated element is shown

#### Scenario: Stepping
- **WHEN** a step or continue is issued and the session has not yet reached the
  next stop
- **THEN** the debug panel shows the animated element again until it stops

#### Scenario: Session ended
- **WHEN** the session terminates without stopping
- **THEN** no animated element is shown and the panel reports how the session
  ended
