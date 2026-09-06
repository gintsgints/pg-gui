-- Puts the `app` schema on the role's search_path, so unqualified table names
-- resolve there as well as in `public`.
ALTER ROLE pgui SET search_path TO app, public;
