# Running scripts from the files panel

`cmd-shift-enter` (or Connection ▸ **Run Selected Scripts**) runs a set of
`.sql` files from the files panel against the current connection — the way you
would apply a folder of migrations.

## Selecting the files

The panel's selection is multi-select:

| Gesture | Effect |
| --- | --- |
| Click a `.sql` file | Opens it and anchors the selection there |
| Shift-click another | Takes the whole range from the anchor |
| Cmd / ctrl-click | Adds or drops that one row |

Switching working folder clears the selection, and a file the periodic
re-scan no longer finds drops out of it.

## How the batch runs

- **In tree order** — which, for timestamp-named migration files, is the
  order they were written. A script inside a folder you collapsed after
  picking it still runs in its place.
- **From disk.** A tab's unsaved edits are *not* run; the
  [message log](message-log.md) notes that they were skipped.
- **Each file headed by its own `▶ <path>` log line**, because statement
  numbering restarts per file.
- **Stopping at the first failure** — the same rule as
  `psql -v ON_ERROR_STOP=1`, which is what `sql/00-run-init.sh` uses.
- **On the active tab's session**, so it sees that tab's
  [autocommit setting](transactions.md) and any transaction it is holding
  open.

## One script

**Run Script** in a file row's
[right-click menu](files.md#right-click-menu) runs that one file the same way,
confirmation included. Inside a multi-row selection the item runs the whole
selection instead.

## The confirmation

A batch is the only action that executes files you are not looking at, several
at once, against whatever connection the window happens to be on — so it
confirms first. The dialog names the target connection (its saved name, or the
masked URL) and lists the files, the first dozen in full and the rest
summarised.
