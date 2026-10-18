CREATE TABLE app.orders (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES app.customers (id),
    amount numeric(10, 2) NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_orders_customer_id ON app.orders (customer_id);
CREATE INDEX idx_orders_created_at ON app.orders (created_at);
