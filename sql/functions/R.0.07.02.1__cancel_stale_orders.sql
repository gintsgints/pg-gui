-- A procedure that commits in batches: cancel stale pending orders.
CREATE OR REPLACE PROCEDURE cancel_stale_orders(IN p_older_than interval DEFAULT '90 days')
LANGUAGE plpgsql
AS $$
DECLARE
    v_batch integer;
BEGIN
    LOOP
        WITH stale AS (
            SELECT id
            FROM orders
            WHERE status = 'pending' AND created_at < now() - p_older_than
            LIMIT 1000
            FOR UPDATE SKIP LOCKED
        )
        UPDATE orders
        SET status = 'cancelled'
        FROM stale
        WHERE orders.id = stale.id;

        GET DIAGNOSTICS v_batch = ROW_COUNT;
        EXIT WHEN v_batch = 0;
        COMMIT;
    END LOOP;
END;
$$;
