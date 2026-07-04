-- Sample data for exercising pg-gui: joinable tables with various column
-- types (including NULLs), generated at a size that makes scrolling,
-- paging and slow-ish queries observable, plus stored procedures and
-- functions for testing CALL / SELECT function() flows.

CREATE TABLE customers (
    id serial PRIMARY KEY,
    name text NOT NULL,
    email text,
    country char(2) NOT NULL,
    signed_up date NOT NULL DEFAULT current_date
);

CREATE TABLE orders (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id),
    amount numeric(10, 2) NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE order_events (
    id bigserial PRIMARY KEY,
    order_id integer NOT NULL REFERENCES orders (id),
    event_type text NOT NULL,
    payload jsonb,
    occurred_at timestamptz NOT NULL DEFAULT now()
);

-- ~10k customers with deterministic pseudo-random attributes; every 7th
-- customer has no email.
INSERT INTO customers (name, email, country, signed_up)
SELECT
    'Customer ' || i,
    CASE WHEN i % 7 = 0 THEN NULL ELSE 'customer' || i || '@example.com' END,
    (ARRAY['LV', 'DE', 'US', 'GB', 'FR', 'EE', 'LT'])[1 + i % 7],
    date '2024-01-01' + (i % 900)
FROM generate_series(1, 10_000) AS i;

-- ~100k orders spread over the customers and the last two years.
INSERT INTO orders (customer_id, amount, status, created_at)
SELECT
    1 + (i * 37) % 10_000,
    round((random() * 990 + 10)::numeric, 2),
    (ARRAY['pending', 'paid', 'paid', 'paid', 'shipped', 'cancelled'])[1 + i % 6],
    now() - (random() * interval '730 days')
FROM generate_series(1, 100_000) AS i;

-- ~300k events, 1-5 per order, with jsonb payloads.
INSERT INTO order_events (order_id, event_type, payload, occurred_at)
SELECT
    o.id,
    (ARRAY['created', 'payment_attempted', 'paid', 'shipped', 'note_added'])[e],
    jsonb_build_object('step', e, 'source', CASE WHEN e % 2 = 0 THEN 'api' ELSE 'web' END),
    o.created_at + e * interval '1 hour'
FROM orders AS o
CROSS JOIN LATERAL generate_series(1, 1 + o.id % 5) AS e;

CREATE INDEX idx_orders_customer_id ON orders (customer_id);
CREATE INDEX idx_orders_created_at ON orders (created_at);
CREATE INDEX idx_order_events_order_id ON order_events (order_id);

ANALYZE customers, orders, order_events;

-- A procedure with INOUT parameters: place an order and return its id.
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

-- A procedure that commits in batches: cancel stale pending orders.
CREATE PROCEDURE cancel_stale_orders(IN p_older_than interval DEFAULT '90 days')
LANGUAGE plpgsql
AS $$
DECLARE
    v_batch integer;
BEGIN
    LOOP
        WITH stale AS (
            SELECT id
            FROM orders
            WHERE status = 'pending' AND created_at < now() - p_older_than
            LIMIT 1000
            FOR UPDATE SKIP LOCKED
        )
        UPDATE orders
        SET status = 'cancelled'
        FROM stale
        WHERE orders.id = stale.id;

        GET DIAGNOSTICS v_batch = ROW_COUNT;
        EXIT WHEN v_batch = 0;
        COMMIT;
    END LOOP;
END;
$$;

-- A set-returning function: per-customer order summary.
CREATE FUNCTION customer_order_summary(p_customer_id integer)
RETURNS TABLE (
    order_count bigint,
    total_spent numeric,
    last_order_at timestamptz
)
LANGUAGE sql
STABLE
AS $$
    SELECT count(*), coalesce(sum(amount), 0), max(created_at)
    FROM orders
    WHERE customer_id = p_customer_id AND status <> 'cancelled';
$$;
