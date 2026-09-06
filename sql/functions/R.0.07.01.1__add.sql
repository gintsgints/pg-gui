-- Sample PL/pgSQL function to debug with pgdap.
--
-- Setup (run once, as superuser on the server side):
--   shared_preload_libraries = 'plugin_debugger'   (postgresql.conf, then restart)
--   CREATE EXTENSION pldbgapi;                      (in the target database)
--
-- Then launch pgdap with:
--   function = "public.add(int, int)"   args = [2, 3]   stopOnEntry = true

CREATE OR REPLACE FUNCTION public.add(a int, b int)
RETURNS int
LANGUAGE plpgsql
AS $$
DECLARE
    total int := 0;
    i     int;
BEGIN
    total := a;                 -- set a breakpoint here
    FOR i IN 1..b LOOP
        total := total + 1;     -- step through the loop, watch `total`
    END LOOP;
    RETURN total;
END;
$$;
