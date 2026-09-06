-- ~300k events, 1-5 per order, with jsonb payloads.
INSERT INTO order_events (order_id, event_type, payload, occurred_at)
SELECT
    o.id,
    (ARRAY['created', 'payment_attempted', 'paid', 'shipped', 'note_added'])[e],
    jsonb_build_object('step', e, 'source', CASE WHEN e % 2 = 0 THEN 'api' ELSE 'web' END),
    o.created_at + e * interval '1 hour'
FROM orders AS o
CROSS JOIN LATERAL generate_series(1, 1 + o.id % 5) AS e;

ANALYZE order_events;
