# Connection manager

Connections are created and edited in a per-field dialog — host, port, user,
password, database — with an inline **Test Connection** check, so a typo is
caught in the dialog instead of on the first query.

A connection can be given a **name**. Switch between saved connections from
the combobox in the title bar or from Connection ▸ Recent.

## A connection is its name

A saved connection is identified by its name (or by its URL when it has none),
not by its connection string. Editing a connection is how its URL changes, so
re-pointing a connection at another server updates the one entry instead of
adding a second, and renaming it keeps everything that belongs to it. Two
entries pointing at the same server stay distinguishable in the combobox.

## What switching connection does

Untitled tabs and query output belong to the connection, not to the window:

- **Untitled tabs are parked.** They have no file, so nothing can be lost and
  nothing needs prompting; they come back — in the tab-bar slot they were
  taken from — when you return to that connection. The scripts are persisted
  in `config.json`, because for an untitled tab that is the only copy of the
  text.
- **Every tab's results are parked** too — rows, log and plan alike — so a
  tab's [message log](message-log.md) never describes a server it was not run
  against.
- **A tab backed by a file stays open** throughout. Only its results swap.

Editing the connection you are currently on is not a switch: the tabs and the
output stay put.

Closing an untitled tab *does* lose its script, so that gets a prompt — with
Save As — even though parking one does not. A quit keeps every untitled
buffer, parked or not.
