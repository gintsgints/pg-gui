-- A set-returning function: per-customer order summary.
CREATE OR REPLACE FUNCTION customer_order_summary(p_customer_id integer)
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
