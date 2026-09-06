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
01 extensions/   pg_stat_statements, pldbgapi
02 roles/        CREATE ROLE only — a role must exist before it can own or be granted
03 schemas/      app
04 types/        enums, domains, composites used by table columns
05 sequences/    standalone sequences (serial/identity columns need none)
06 tables/       customers, orders, order_events, app.feature_flags
07 functions/    add, place_order, cancel_stale_orders, customer_order_summary
08 views/        views over those tables
09 matviews/     materialized views, created WITH NO DATA
10 constraints/  FK/CHECK/UNIQUE as ALTER TABLE ADD, so a circular FK is expressible
11 triggers/     needs both the table and the trigger function
12 data/         the rows loaded into those tables
13 indexes/      built once over the loaded data, not maintained row by row
14 refresh/      REFRESH MATERIALIZED VIEW, once the data is in
15 grants/       privileges, and the pgui search_path setting
   examples/     loose scratch queries, not run at init
```

Only the folders that hold a file need to exist; the script skips the rest.

Naming — one scheme, `V` or `R` picking how the file is applied:

`<V|R>.<reserved>.<object_order>.<dependency_order>.<n>__<name>.sql`

- `V` — versioned, applied once (schemas, types, sequences, tables,
  constraints, data, indexes).
- `R` — repeatable, safe to re-apply (extensions, routines, views, triggers,
  grants). Every `V` in a folder runs before that folder's first `R`.
- `reserved` — always `0` for now; held back for a release or branch number.
- `object_order` — the two-digit folder number above, so the name still carries
  its position once the file is read out of its folder.
- `dependency_order` — order within the folder: `orders` (02) is created after
  the `customers` (01) it references.
- `n` — successive migrations of that one object, starting at `1`. Altering the
  `customers` table later adds `V.0.06.01.2__customers.sql` beside it. `R`
  scripts stay at `1`: a repeatable script is revised in place, since re-running
  it is the whole point.
- Numbers are zero-padded to two digits because the scripts are sorted
  `LC_ALL=C`, where `10` sorts before `2`.

`00-run-init.sh` is the entry point. The postgres entrypoint globs
`/docker-entrypoint-initdb.d/*` and **ignores directories**, so it never sees
these folders; the script walks them itself, in the dependency order listed
above, and runs each folder's `V*` scripts before its `R*` ones. Adding a file
to an existing folder needs no change to the script; a new folder does.

Two placements are deliberate and worth keeping:

- `constraints` before `data`, so the seed rows are validated on the way in.
  Flip the two only if loading gets slow.
- `functions` before `views`, because a view may call a function. A
  `LANGUAGE sql` function body *is* parsed at creation, so such a function over
  a view breaks; write it `LANGUAGE plpgsql` (bodies are never checked) or give
  it a folder after `views`.

Anything that is *not* meant to run at init must stay inside a folder the
script does not walk — that is what `examples/` is for. A loose `*.sql` or
`*.sh` at the top level of this directory would be picked up and executed by
the postgres entrypoint itself.

These run on first start only — `docker compose down -v` resets the volume and
replays them.
