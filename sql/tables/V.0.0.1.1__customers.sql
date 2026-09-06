CREATE TABLE customers (
    id serial PRIMARY KEY,
    name text NOT NULL,
    email text,
    country char(2) NOT NULL,
    signed_up date NOT NULL DEFAULT current_date
);
