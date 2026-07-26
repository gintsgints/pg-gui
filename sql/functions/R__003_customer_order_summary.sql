CREATE OR REPLACE FUNCTION public.customer_order_summary(p_customer_id int)
RETURNS TABLE (
  order_count bigint,
  total_spent numeric,
  last_order_at TIMESTAMP WITH TIME ZONE
)
LANGUAGE sql
STABLE
AS $function$
SELECT
  COUNT(*),
  COALESCE(SUM(amount), 0),
  MAX(created_at)
FROM
  orders
WHERE
  customer_id = p_customer_id AND
  status <> 'cancelled';
$function$;
