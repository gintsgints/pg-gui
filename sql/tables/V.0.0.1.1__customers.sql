CREATE TABLE public.customers (
  id int
  NOT NULL
  DEFAULT nextval(CAST('customers_id_seq' AS regclass)),
  name text NOT NULL,
  email text,
  country char(2) NOT NULL,
  signed_up date NOT NULL DEFAULT CURRENT_DATE,
  CONSTRAINT "customers_pkey" PRIMARY KEY (id)
);