-- ~100k orders spread over the customers and the last two years.
INSERT INTO orders (customer_id, amount, status, created_at)
SELECT
    1 + (i * 37) % 10_000,
    round((random() * 990 + 10)::numeric, 2),
    (ARRAY['pending', 'paid', 'paid', 'paid', 'shipped', 'cancelled'])[1 + i % 6],
    now() - (random() * interval '730 days')
FROM generate_series(1, 100_000) AS i;

ANALYZE orders;
