-- ~10k customers with deterministic pseudo-random attributes; every 7th
-- customer has no email.
INSERT INTO customers (name, email, country, signed_up)
SELECT
    'Customer ' || i,
    CASE WHEN i % 7 = 0 THEN NULL ELSE 'customer' || i || '@example.com' END,
    (ARRAY['LV', 'DE', 'US', 'GB', 'FR', 'EE', 'LT'])[1 + i % 7],
    date '2024-01-01' + (i % 900)
FROM generate_series(1, 10_000) AS i;

ANALYZE customers;
