# Opening and saving scripts

`cmd-o` opens a `.sql` file and `cmd-s` saves one, both through native file
dialogs that start in the last directory you used. `cmd-s` on a never-saved
tab is Save As.

Each open script gets its own editor tab. `cmd-t` makes a new one, `cmd-w`
closes the current one, and `ctrl-tab` / `ctrl-shift-tab` cycle through them.

## Unsaved work

- A tab with unsaved edits is marked with a `•`.
- Closing it prompts to save first, and so does quitting.
- Closing an **untitled** tab with text in it prompts as well, offering Save
  As — an untitled buffer has no file to fall back on.

## Changed on disk

An open script that changes on disk — a `git checkout`, another editor, a
formatter — offers to reload rather than silently keeping the stale buffer or
silently overwriting the new file.

## The files panel

`cmd-b` (or `cmd-2`) shows the files panel, a tree of the working folder. It
is re-scanned every few seconds, so files appearing from outside the app show
up on their own.

From there you can also [create scripts in place](new-scripts.md) and
[run a selection of scripts](run-scripts.md) as a batch.

### Right-click menu

Right-clicking a row opens a context menu:

| Row | Items |
| --- | --- |
| A `.sql` file | **Open**, **Run Script**, **New Script…**, **Rename…**, **Delete…** |
| A folder | **New Script…**, **Rename…**, **Delete…** |

- **Run Script** runs just that file, through the same
  [confirmation](run-scripts.md#the-confirmation) a batch gets. Right-clicking
  inside a multi-row selection says **Run N Selected Scripts** instead and
  runs the whole selection.
- **New Script…** lands inside a folder row, and beside a file row.
- **Rename…** asks for a new name in the same folder (not a new location),
  forces `.sql` on a file, and never overwrites an existing entry. Tabs on the
  renamed file — or on anything under a renamed folder — follow it.
- **Delete…** confirms first, and is permanent: nothing goes to a trash can.
  A folder is deleted only when it is empty. A tab on a deleted file keeps its
  text but becomes untitled, so saving it again cannot silently re-create the
  file.

Files that are not `.sql` are greyed out in the panel and have no menu.

Saving may reformat the script first — see
[format-on-save](language-server.md#formatting), which is on by default.
