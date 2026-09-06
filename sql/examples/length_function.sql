CREATE OR REPLACE FUNCTION length(value text)
RETURNS int
LANGUAGE plpgsql
AS $function$
BEGIN 
RETURN char_length(value);
END;
$function$;
