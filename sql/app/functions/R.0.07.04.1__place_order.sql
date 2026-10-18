-- A procedure with INOUT parameters: place an order and return its id.
-- This is the routine docs/debug/EXAMPLE.md steps through with pldebugger.
CREATE OR REPLACE PROCEDURE app.place_order(
    IN p_customer_id integer,
    IN p_amount numeric,
    INOUT p_order_id integer DEFAULT NULL
)
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO app.orders (customer_id, amount)
    VALUES (p_customer_id, p_amount)
    RETURNING id INTO p_order_id;

    INSERT INTO app.order_events (order_id, event_type, payload)
    VALUES (p_order_id, 'created', jsonb_build_object('source', 'procedure'));
END;
$$;
