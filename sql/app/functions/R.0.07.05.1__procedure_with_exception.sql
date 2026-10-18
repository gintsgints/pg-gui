CREATE OR REPLACE PROCEDURE app.procedure_with_exception()
LANGUAGE plpgsql
AS $procedure$
DECLARE
  total int := 0;
BEGIN
    total := total + 1;
    RAISE EXCEPTION 'TESTING';
END;
$procedure$;
