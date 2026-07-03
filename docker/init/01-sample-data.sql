-- Sample data for exercising pg-gui: a couple of joinable tables with
-- various column types, including NULLs.
CREATE TABLE customers (
    id serial PRIMARY KEY,
    name text NOT NULL,
    email text,
    signed_up date NOT NULL DEFAULT current_date
);

CREATE TABLE orders (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id),
    amount numeric(10, 2) NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    created_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO customers (name, email, signed_up) VALUES
    ('Alice Ozol', 'alice@example.com', '2025-11-03'),
    ('Bruno Kalns', 'bruno@example.com', '2026-01-18'),
    ('Chiara Meier', NULL, '2026-02-27'),
    ('Dainis Berzins', 'dainis@example.com', '2026-05-09');

INSERT INTO orders (customer_id, amount, status) VALUES
    (1, 129.99, 'paid'),
    (1, 15.50, 'pending'),
    (2, 999.00, 'paid'),
    (3, 42.00, 'cancelled'),
    (4, 310.25, 'paid'),
    (4, 8.75, 'pending');
