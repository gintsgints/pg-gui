# Debugging a stored procedure with pldebugger

The test database ships with the [pldebugger](https://github.com/EnterpriseDB/pldebugger)
extension (`pldbgapi`). This walkthrough debugs the sample `place_order`
procedure step by step.

## The target

`place_order` (from `sql/functions/R.0.07.04.1__place_order.sql`) is a plpgsql procedure with
two statements: it inserts a row into `orders`, then a row into `order_events`.

```sql
CREATE PROCEDURE place_order(
    IN p_customer_id integer,
    IN p_amount numeric,
    INOUT p_order_id integer DEFAULT NULL
)
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO orders (customer_id, amount)
    VALUES (p_customer_id, p_amount)
    RETURNING id INTO p_order_id;

    INSERT INTO order_events (order_id, event_type, payload)
    VALUES (p_order_id, 'created', jsonb_build_object('source', 'procedure'));
END;
$$;
```

## Prerequisites

- Test database running: `docker compose up -d --build --wait`
- Two psql sessions against it:

  ```sh
  psql postgres://pgui:pgui@localhost:5433/pgui_test
  ```

pldebugger needs **two connections**: one runs the procedure (the *target*), the
other drives the debugger (the *controller*). The flow below uses a global
(out-of-band) breakpoint — the same mechanism a GUI uses to attach to a live
backend.

## Notes

- **Line numbers** from `pldbg_get_source` are body-relative; only executable
  statement lines accept breakpoints. Entry is `-1`.
- **OID resolution** — `'place_order'::regproc::oid` works because the name is
  unique. If a function is overloaded, use the full signature
  (`'place_order(integer,numeric,integer)'::regprocedure::oid`) or
  `pldbg_get_target_info('place_order', 'f')`.
- **Direct mode** — instead of a global breakpoint, session B can run
  `SELECT pldbg_oid_debug('place_order'::regproc::oid);` before the `CALL`; the
  backend then waits for a debugger to attach. Global mode is what a GUI uses.
- **Schema** — the extension installs into `public` (no `SCHEMA` clause in
  `sql/extensions/R.0.01.02.1__pldbgapi.sql`), so unqualified `pldbg_*` names
  resolve. If
  moved to a dedicated schema, qualify every call (`debug.pldbg_*`).
- **Cleanup** — `SELECT pldbg_abort_target(1);` kills a trapped execution instead
  of continuing it.
