CREATE TABLE app.feature_flags (
    id serial PRIMARY KEY,
    name text NOT NULL,
    enabled boolean NOT NULL DEFAULT FALSE
);
