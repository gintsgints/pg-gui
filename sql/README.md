# sql/

Sample data for exercising pg-gui: joinable tables with various column types
(including NULLs), generated at a size that makes scrolling, paging and
slow-ish queries observable, plus stored procedures and functions for testing
CALL / SELECT function() flows.

`docker-compose.yml` mounts this folder as the container's
`/docker-entrypoint-initdb.d`, so it is both the checked-in source of the
sample schema and the seed that creates it. One folder per object type, one
file per object:

```
extensions/  pg_stat_statements, pldbgapi
schemas/     app
tables/      customers, orders, order_events, app.feature_flags
data/        the rows loaded into those tables
functions/   add, place_order, cancel_stale_orders, customer_order_summary
roles/       the pgui search_path setting
examples/    loose scratch queries, not run at init
```

Naming:

- `V.<version>__<name>.sql` — versioned, applied once (schemas, tables, data).
- `R__<nnn>_<name>.sql` — repeatable, safe to re-apply (extensions, routines,
  role settings). The number only fixes the order within a folder.

`00-run-init.sh` is the entry point. The postgres entrypoint globs
`/docker-entrypoint-initdb.d/*` and **ignores directories**, so it never sees
these folders; the script walks them itself, in dependency order
(`extensions`, `schemas`, `tables`, `data`, `functions`, `roles`) and runs each
folder's `V*` scripts before its `R*` ones. Adding a file to an existing folder
needs no change to the script; a new folder does.

Anything that is *not* meant to run at init must stay inside a folder the
script does not walk — that is what `examples/` is for. A loose `*.sql` or
`*.sh` at the top level of this directory would be picked up and executed by
the postgres entrypoint itself.

These run on first start only — `docker compose down -v` resets the volume and
replays them.
