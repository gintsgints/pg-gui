# Transactions

Each tab holds its own live session, so transaction state is per tab. The
Session menu drives it.

## Autocommit

Autocommit is a **per-tab** toggle:

- **On** (the default): every Run commits on its own.
- **Off**: Run opens a transaction and leaves it open. **Commit** or
  **Rollback** in the Session menu ends it.

The current mode seeds new tabs and survives a restart, so a window you work
in with autocommit off comes back that way.

Everything that executes on the tab's session honours the setting, not just
Run: an `EXPLAIN (ANALYZE)` of a DML statement
([query plans](query-plan.md)) lands in the open transaction and is
Rollback's to undo, and a [batch script run](run-scripts.md) uses the active
tab's session, so it sees that tab's autocommit setting and any transaction it
is already holding open.

## Cancel Query

A statement that is taking too long can be stopped with Session ▸ Cancel
Query. The cancellation goes to the server, and an open transaction stays open
for you to commit or roll back.
