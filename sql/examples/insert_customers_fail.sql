-- Failure path: the third statement violates NOT NULL on country, so the
-- run stops there. With autocommit on the whole block is rolled back, but
-- the log still shows the two statements that ran plus the error, and the
-- result sets they produced stay on the selector.

INSERT INTO customers (name, email, country, signed_up)
VALUES ('Rollback One', 'one@example.com', 'LV', current_date)
RETURNING id, name;

INSERT INTO customers (name, email, country, signed_up)
VALUES ('Rollback Two', 'two@example.com', 'DE', current_date)
RETURNING id, name;

-- country is NOT NULL: this fails.
INSERT INTO customers (name, email, country)
VALUES ('Rollback Three', 'three@example.com', NULL);

-- Never reached.
SELECT count(*) AS rollback_rows
FROM customers
WHERE name LIKE 'Rollback %';
