CREATE OR REPLACE PROCEDURE
public.place_order(
  IN p_customer_id int,
  IN p_amount numeric,
  INOUT p_order_id int DEFAULT CAST(NULL AS int)
)
LANGUAGE plpgsql
AS $procedure$
BEGIN
    INSERT INTO orders (customer_id, amount)
    VALUES (p_customer_id, p_amount)
    RETURNING id INTO p_order_id;

    INSERT INTO order_events (order_id, event_type, payload)
    VALUES (p_order_id, 'created', jsonb_build_object('source', 'procedure'));
END;
$procedure$;
