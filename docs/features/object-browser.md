# Database object browser

`cmd-1` shows or hides the browser panel. It lists the connected database's
schemas and, under each, its objects:

- **Tables**, each expanding to its `CREATE TABLE` DDL, its columns with their
  type and PK / not-null / default markers, and its indexes, constraints and
  triggers
- **Views** and **materialized views**
- **Functions**
- **Sequences**
- **Types**

Everything is loaded lazily — expanding a node is what queries the catalog —
and the filter box narrows the tree as you type. The **System** checkbox adds
the catalog schemas (`pg_catalog`, `information_schema`), which are hidden by
default.

## Opening an object

Clicking an object opens its source in a tab. pg-gui prefers the real file:
if the working folder holds a script matching the object
(`definition_file_mask` in `config.json`), that file opens, so your edits go
to the thing under version control. Otherwise the definition is fetched from
the catalog and opened as a read-through of what the server actually has.

[Go to Definition](go-to-definition.md) in the editor lands on the same code
path, so a cmd-click and a click in the browser behave identically.

## Refreshing

Refreshing the browser re-reads the schemas and drops the cached definition
lookups, as does reconnecting.
