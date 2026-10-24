# Palette

`cmd-p` opens one searchable list. What it searches depends on the first
character of the query, so switching mode is a single keystroke:

| Query | Lists | Enter |
| --- | --- | --- |
| plain text | the working folder's `.sql` files | opens the file in a tab |
| `>` … | [snippets](snippets.md) — yours, then the built-ins | inserts it at the cursor |
| `:` … | the app's commands, each with its shortcut | runs the command |

`cmd-shift-p` (View ▸ Command Palette…) opens it with the `:` already typed,
and Edit ▸ Snippets with the `>`. File ▸ Go to File… is the plain `cmd-p`.

## Finding files

The files come from the [files panel](files.md)'s last scan of the working
folder, so nothing extra is walked when the palette opens; with no folder
open the list says so. Matching is fuzzy: every word of the query must appear
in the file's path *in order*, not necessarily adjacent — `ordday` finds
`reports/orders_by_day.sql`. Files whose name holds the query verbatim come
first, then shorter paths.

## Commands

Commands are labelled with the menu they live in (`Session: Rollback`,
`View: Theme Dark`), so the menu name doubles as a search word. A picked
command runs exactly as its keybinding would. Commands that only make sense
with the results grid focused (copy, select all) are not listed.
