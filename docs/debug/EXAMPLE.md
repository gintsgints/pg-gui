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

## Step 1 — Controller: create a session and arm a breakpoint

In **session A** (the debugger):

```sql
-- Open a debug session; returns a sessionid (e.g. 1).
SELECT pldbg_create_listener();

-- Arm a global breakpoint on place_order, at entry (-1), for any backend (NULL).
SELECT pldbg_set_global_breakpoint(1, 'place_order'::regproc::oid, -1, NULL);

-- Block until some backend calls place_order; returns the target PID.
SELECT pldbg_wait_for_target(1);
```

The last call hangs, waiting for the procedure to be called elsewhere.

## Step 2 — Target: call the procedure

In **session B** (the target):

```sql
CALL place_order(p_customer_id => 1, p_amount => 99.50);
```

This hangs too: the backend is trapped at the entry of `place_order`, waiting for
the debugger. Back in session A, `pldbg_wait_for_target` now returns the PID.

## Step 3 — Controller: inspect state at entry

Back in **session A**:

```sql
-- Confirm we are parked at a breakpoint (function oid, line, etc.).
SELECT * FROM pldbg_wait_for_breakpoint(1);

-- Show the source, so you can read the body-relative line numbers.
SELECT * FROM pldbg_get_source(1, 'place_order'::regproc::oid);

-- Show the call stack.
SELECT * FROM pldbg_get_stack(1);

-- Show the variables. At entry: p_customer_id=1, p_amount=99.50, p_order_id=NULL.
SELECT * FROM pldbg_get_variables(1);
```

## Step 4 — Controller: set a line breakpoint and step

```sql
-- Break on the order_events INSERT (use the line number from pldbg_get_source).
SELECT pldbg_set_breakpoint(1, 'place_order'::regproc::oid, 9);

-- Step over the orders INSERT.
SELECT pldbg_step_over(1);

-- Inspect again: p_order_id is now populated by RETURNING ... INTO.
SELECT * FROM pldbg_get_variables(1);
```

## Step 5 — Controller: mutate a variable and continue

```sql
-- Change a variable mid-execution (line -1 = current line).
SELECT pldbg_deposit_value(1, 'p_amount', -1, '123.45');

-- Run to the next breakpoint, or to completion.
SELECT pldbg_continue(1);
```

Session B now unblocks and `CALL` returns with `p_order_id` set.

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

## Function reference

| Function | Purpose |
| --- | --- |
| `pldbg_create_listener()` | Open a debug session, return sessionid. |
| `pldbg_set_global_breakpoint(session, oid, line, pid)` | Trap the next matching call from any (or a given) backend. |
| `pldbg_wait_for_target(session)` | Block until a backend attaches; return its PID. |
| `pldbg_wait_for_breakpoint(session)` | Block until execution stops at a breakpoint. |
| `pldbg_set_breakpoint(session, oid, line)` | Set a local line breakpoint. |
| `pldbg_drop_breakpoint(session, oid, line)` | Remove a breakpoint. |
| `pldbg_get_breakpoints(session)` | List active breakpoints. |
| `pldbg_get_source(session, oid)` | Source of the function being debugged. |
| `pldbg_get_stack(session)` | Current call stack. |
| `pldbg_select_frame(session, frame)` | Switch active stack frame. |
| `pldbg_get_variables(session)` | Variables in the current frame. |
| `pldbg_deposit_value(session, name, line, value)` | Assign a new value to a variable. |
| `pldbg_step_into(session)` | Step, descending into called functions. |
| `pldbg_step_over(session)` | Step, skipping over calls. |
| `pldbg_continue(session)` | Run to the next breakpoint or to completion. |
| `pldbg_abort_target(session)` | Abort the debugged execution. |
| `pldbg_oid_debug(oid)` | Flag a function for direct-mode debugging. |
| `pldbg_get_target_info(signature, type)` | Resolve a name/signature to OID and metadata. |
