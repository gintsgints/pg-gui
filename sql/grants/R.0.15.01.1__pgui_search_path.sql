-- Puts the `app` schema on the role's search_path, so unqualified names
-- resolve to the sample objects there, and still to the extensions in
-- `public`.
ALTER ROLE pgui SET search_path TO app, public;
