# Go to Definition

Cmd-click a routine, table or view name in the editor — or right-click ▸ **Go
to Definition** — to open its source.

This is pg-gui's own catalog lookup, not a language-server feature: the
Postgres Language Server has no definition support, and its hover only returns
markdown. So it works even while [the language server](language-server.md) is
down or still loading its schema cache.

## How a name is resolved

The identifier under the pointer is resolved against `pg_proc` and `pg_class`
(tables, partitioned tables, views and materialized views) in one query,
preferring routines and ordering by visibility — so an **unqualified** name
resolves the way `search_path` would, and a `schema.object` qualifier is
honoured even though the editor's word scan does not include it.

## What opens

The hit opens exactly the way a click in the [object browser](object-browser.md)
does: the matching `.sql` file from the working folder when there is one,
otherwise the definition fetched from the catalog.

## Caching

Each lookup opens its own connection, and the editor asks again on every
mouse move while cmd is held, so results — misses included — are cached per
connection. A browser refresh or a reconnect drops the cache.

## No keybinding

Cmd-click and the context-menu item are the triggers. There is no key for it,
because the underlying `GoToDefinition` action only fires after a cmd-hover
has already resolved the symbol.
