CREATE TABLE order_events (
    id bigserial PRIMARY KEY,
    order_id integer NOT NULL REFERENCES orders (id),
    event_type text NOT NULL,
    payload jsonb,
    occurred_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_order_events_order_id ON order_events (order_id);
