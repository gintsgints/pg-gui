SELECT
  schemaname AS schema,
  relname AS "table",
  n_live_tup AS approx_rows,
  pg_size_pretty(pg_total_relation_size(relid))
  AS total_size
FROM
  pg_stat_user_tables
WHERE
  relname ILIKE '%%'
ORDER BY n_live_tup DESC;

SELECT * FROM sim_orders;

SELECT * FROM salesmans;
INSERT INTO salesmans (name) VALUES ('John Doe');

CREATE TABLE salesmans (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  name text NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE
  NOT NULL
  DEFAULT NOW()
);

ALTER TABLE sim_orders
  ADD COLUMN salesman_id bigint,
  ADD CONSTRAINT "fk_sim_orders_salesman"
  FOREIGN KEY
  (salesman_id)
  REFERENCES salesmans (id);
       