# sql/

Sample data for exercising pg-gui: joinable tables with various column types
(including NULLs), generated at a size that makes scrolling, paging and
slow-ish queries observable, plus stored procedures and functions for testing
CALL / SELECT function() flows.

`docker-compose.yml` mounts this folder as the container's
`/docker-entrypoint-initdb.d`, so it is both the checked-in source of the
sample schema and the seed that creates it. One folder per schema, a folder
per object type inside it, one file per object:

```
app/
  schemas/     V.0.03.01.1__app.sql
  tables/      V.0.06.01.1__customers.sql, V.0.06.02.1__orders.sql, …
  functions/   R.0.07.01.1__add.sql, …
  views/       R.0.08.01.1__customer_open_order_total_ammount.sql
  upgrade/     V.2026.09.06.10.42__customers.sql, …
public/
  extensions/  R.0.01.01.1__pg_stat_statements.sql, R.0.01.02.1__pldbgapi.sql
grants/        R.0.15.01.1__pgui_search_path.sql
```

The object types, in run order:

```
01 extensions/   pg_stat_statements, pldbgapi
02 roles/        CREATE ROLE only — a role must exist before it can own or be granted
03 schemas/      app
04 types/        enums, domains, composites used by table columns
05 sequences/    standalone sequences (serial/identity columns need none)
06 tables/       customers, orders, order_events, feature_flags
07 functions/    add, place_order, cancel_stale_orders, customer_order_summary
08 views/        views over those tables
09 matviews/     materialized views, created WITH NO DATA
10 constraints/  FK/CHECK/UNIQUE as ALTER TABLE ADD, so a circular FK is expressible
11 triggers/     needs both the table and the trigger function
12 upgrade/      the rows loaded into those tables, and every later change to
                 an object already created above — `ALTER TABLE`, `UPDATE`,
                 backfills
13 indexes/      built once over the loaded data, not maintained row by row
14 refresh/      REFRESH MATERIALIZED VIEW, once the data is in
15 grants/       privileges, and the pgui search_path setting
   examples/     loose scratch queries, not run at init
```

Only the folders that hold a file need to exist; the script skips the rest.

The schema folder names the schema the object lives in; the file's own name
then drops it (`app/tables/…__feature_flags.sql`, not `…__app_feature_flags`).
The sample objects all live in `app`; `public` holds only the extensions,
whose `pldbg_*` functions pg-gui's debugger calls unqualified.

Every script qualifies the names it creates and touches (`app.orders`), body
of a routine included. The `pgui` search_path that would let it write `orders`
is set in `grants/`, the last folder to run, and only takes effect for later
sessions anyway; a `LANGUAGE sql` body is resolved at creation, and a plpgsql
body at call time against the *caller's* search_path, so neither may rely on
it.
Objects that belong to no schema — roles, and role settings such as the
`pgui` search_path in `grants/` — sit in a type folder at the top level
instead, with no schema above them.

Upgrades go under the schema they change (`app/upgrade/`). Their timestamps
are still one global order: the script sorts every schema's `upgrade/`
together by file name, so an `app` upgrade written before a `billing` one runs
first. An upgrade that spans schemas goes under the one it mainly changes. The
file panel's Run Selected Scripts does not merge like that — it runs folder by
folder — so when replaying upgrades by hand across several schemas, run each
schema's in timestamp order yourself.

The script runs one object type at a time across every schema: it gathers
`<type>/` and every `<schema>/<type>/`, and sorts them by file name, so the
schema folders never decide run order — `dependency_order` does (or, in
`upgrade/`, the timestamp). A table in one schema that references a table in
another just takes a higher number, as `orders` does over `customers`.

Naming — one scheme, `V` or `R` picking how the file is applied:

`<V|R>.<reserved>.<object_order>.<dependency_order>.<n>__<name>.sql`

- `V` — versioned, applied once (schemas, types, sequences, tables,
  constraints, upgrades, indexes).
- `R` — repeatable, safe to re-apply (extensions, routines, views, triggers,
  grants). Every `V` in a folder runs before that folder's first `R`.
- `reserved` — always `0` for now; held back for a release or branch number.
- `object_order` — the two-digit folder number above, so the name still carries
  its position once the file is read out of its folder.
- `dependency_order` — order within the folder: `orders` (02) is created after
  the `customers` (01) it references.
- `n` — successive migrations of that one object, starting at `1`. `R` scripts
  stay at `1`: a repeatable script is revised in place, since re-running it is
  the whole point.
- Numbers are zero-padded to two digits because the scripts are sorted
  `LC_ALL=C`, where `10` sorts before `2`.

`<schema>/upgrade/` is the one folder that names its files differently — see below.

`00-run-init.sh` is the entry point. The postgres entrypoint globs
`/docker-entrypoint-initdb.d/*` and **ignores directories**, so it never sees
these folders; the script walks them itself, in the dependency order listed
above, and runs each folder's `V*` scripts before its `R*` ones. Adding a file
— or a whole new schema folder — needs no change to the script; a new object
type does.

`<schema>/upgrade/` is where a schema stops being a fresh create. A `CREATE`
file under `<schema>/tables/`, `<schema>/types/`, `<schema>/views/`, …
describes the object as it was first written, and is never edited afterwards once a database exists that already ran it;
every later change to that object — `ALTER TABLE ADD COLUMN`, a type widening,
a backfill `UPDATE`, as well as the initial `INSERT`s — is a new `V` file in
that schema's `upgrade/`:

`V.<YYYY>.<MM>.<DD>.<HH>.<MI>__<name>.sql`

Nothing but a UTC timestamp, minute precision — no `reserved`, no
`object_order`, no `n`:

```
V.2026.09.06.10.42__customers.sql
```

- The other folders pick the next free `dependency_order` by looking at what is
  already there, which is exactly what two branches do independently before
  both claiming `05`. A timestamp is read off the clock instead, so parallel
  work lands on distinct names and merges without a rename. `upgrade/` is the
  only folder that grows on every branch, which is why only it needs this.
- The dropped fields carried no information here. `object_order` recorded the
  folder, and there is only one folder whose files look like this; `n` numbered
  successive migrations of one object, and the timestamp already orders and
  separates them.
- Timestamps sort chronologically under `LC_ALL=C` (every field fixed width and
  all digits), and chronological is the correct run order: an upgrade can only
  touch objects that already exist when it is written. Two upgrades in the same
  minute — bump one by a minute.

`R` files in `upgrade/` would be a contradiction — a one-time change re-applied
— so the folder holds `V` only. A routine or view that is *rewritten* is not an
upgrade at all: edit its `R` file under `<schema>/functions/` or `<schema>/views/` in place.

Two placements are deliberate and worth keeping:

- `constraints` before `upgrade`, so the seed rows are validated on the way in.
  Flip the two only if loading gets slow.
- `functions` before `views`, because a view may call a function. A
  `LANGUAGE sql` function body *is* parsed at creation, so such a function over
  a view breaks; write it `LANGUAGE plpgsql` (bodies are never checked) or give
  it a folder after `views`.

Anything that is *not* meant to run at init must stay inside a folder the
script does not walk — that is what `examples/` is for. Since every top-level
folder is read as a schema, `examples/` must not hold a subfolder named after
an object type, and a schema cannot be named after one either. A loose `*.sql` or
`*.sh` at the top level of this directory would be picked up and executed by
the postgres entrypoint itself.

These run on first start only — `docker compose down -v` resets the volume and
replays them.
