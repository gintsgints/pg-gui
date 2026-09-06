-- A second schema reachable only through the role's search_path
-- (see roles/R__001_pgui_search_path.sql), for testing that language-server
-- diagnostics resolve unqualified table names the same way query execution
-- does.
CREATE SCHEMA app;
