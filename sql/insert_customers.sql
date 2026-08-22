-- Multi-statement insert script: every statement runs on its own, so the
-- log fills in one line at a time and each RETURNING gives the results
-- panel its own selectable result set. The pg_sleep calls make the
-- streaming visible.

INSERT INTO customers (name, email, country, signed_up)
VALUES ('Ada Lovelace', 'ada@example.com', 'GB', date '2024-02-01')
RETURNING id, name, email, country, signed_up;

SELECT pg_sleep(2);

INSERT INTO customers (name, email, country, signed_up)
VALUES
    ('Grace Hopper', 'grace@example.com', 'US', date '2024-03-15'),
    ('Alan Turing', NULL, 'GB', date '2024-04-20'),
    ('Karlis Ulmanis', 'karlis@example.com', 'LV', date '2024-05-05')
RETURNING id, name, country;

SELECT pg_sleep(2);

-- No RETURNING: a log line only, no result set of its own.
INSERT INTO customers (name, country)
SELECT 'Batch Customer ' || i, 'EE'
FROM generate_series(1, 25) AS i;

SELECT pg_sleep(2);

SELECT country, count(*) AS customers
FROM customers
WHERE signed_up >= date '2024-02-01'
GROUP BY country
ORDER BY customers DESC;
