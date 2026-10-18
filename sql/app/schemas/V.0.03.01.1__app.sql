-- The schema every sample object lives in. It is reachable unqualified only
-- through the role's search_path (see grants/R.0.15.01.1__pgui_search_path.sql),
-- which is what tests that language-server diagnostics resolve unqualified
-- table names the same way query execution does. The scripts here run before
-- that setting exists, so they qualify every name with `app.`.
CREATE SCHEMA app;
