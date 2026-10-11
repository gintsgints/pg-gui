# Snippets

`cmd-p` opens a searchable picker of SQL snippets — built-in queries plus
`New:` templates for tables, views, triggers, sequences and more.

## Your own snippets

Drop `.sql` files into the `snippets/` folder of the config directory (next to
`config.json`) and they join the picker. No registration step, no manifest.

## Tab stops

A snippet can carry numbered tab stops — `$1`, `${2:placeholder}`. After
inserting, `tab` visits each stop in order with its placeholder text already
selected, so typing replaces it.

## In the completion menu

Snippets also show up in the editor's own completion list, ahead of the
[language server's](language-server.md) items. They match on words of the
snippet name or on the SQL's leading words, so typing `create seq…` surfaces
the sequence template; accept it like any other completion.

Snippet suggestions keep working while the language server is offline — a
snippets-only provider stands in for it — so a disconnected editor is not an
editor with no completions at all.
