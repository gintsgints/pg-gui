# Query plan flame graph

Plans are drawn as a flame graph under the editor, not as a tree grid.

| Key | What it runs |
| --- | --- |
| `cmd-e` | `EXPLAIN (FORMAT JSON)` on the statement under the cursor |
| `cmd-shift-e` | `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` — which **executes** the statement |

The chart is a third view beside the results table and the
[message log](message-log.md), per tab like they are.

## Reading it

Each node is a frame, as wide as its measure:

- Without ANALYZE, the measure is the node's estimated **Total Cost**.
- With ANALYZE, it is **Actual Total Time × Actual Loops** — Postgres reports
  the per-loop average, so a node visited a thousand times would otherwise
  draw a thousandth of what it actually spent.

Hover a frame for its measure and its share of the plan, click one to zoom
into it, and click an ancestor row (or **Reset zoom**) to come back out.

Nothing is aggregated or re-scaled, because plan values are already inclusive
— which is exactly the shape a flame graph wants. One consequence is worth
knowing: a `Gather` whose parallel workers out-run the wall clock, or an
`InitPlan`/CTE the planner does not fold into its parent, draws as a child
**wider** than the frame above it. That is the truth about those numbers, not
a drawing bug.

A plan whose every node measured zero — a trivial query under ANALYZE — is
re-valued by subtree size instead, so the shape still draws.

## Transactions

The explain runs on the tab's own session and honours its autocommit setting
the way Run does, so with [autocommit off](transactions.md) an
`EXPLAIN (ANALYZE)` of a DML statement is still Rollback's to undo. It goes
through the same cancel token as a query, so a long `ANALYZE` can be stopped
with Cancel Query.
