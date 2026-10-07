-- Long script, no result sets.
--
-- Every statement here returns nothing: DDL, plain INSERT/UPDATE/DELETE
-- without RETURNING, and DO blocks (pg_sleep goes through PERFORM, so even
-- the pauses produce no rows). So a full run fills the log with ~120 lines
-- and leaves the results table empty the whole time — which is the point:
-- statement numbering, log scrolling, the progress indicator and Cancel
-- all have to work with nothing on the result selector.
--
-- Self-contained: everything lives in a scratch schema the last statement
-- drops, so the script re-runs as often as needed.

CREATE SCHEMA IF NOT EXISTS long_script;

SET search_path TO long_script, public;

DO $$
BEGIN
    RAISE NOTICE 'long_script: starting at %', clock_timestamp();
END;
$$;

-- ---------------------------------------------------------------------
-- 1. Tables and indexes.
-- ---------------------------------------------------------------------

CREATE TABLE long_script.regions (
    code char(2) PRIMARY KEY,
    label text NOT NULL
);

CREATE TABLE long_script.sales (
    id bigserial PRIMARY KEY,
    region char(2) NOT NULL REFERENCES long_script.regions (code),
    sold_on date NOT NULL,
    amount numeric(10, 2) NOT NULL,
    note text
);

CREATE TABLE long_script.audit (
    id bigserial PRIMARY KEY,
    step text NOT NULL,
    noted_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_long_script_sales_region ON long_script.sales (region);

CREATE INDEX idx_long_script_sales_sold_on ON long_script.sales (sold_on);

COMMENT ON TABLE long_script.sales IS 'scratch rows for the long-script run';

-- ---------------------------------------------------------------------
-- 2. One INSERT per region: eight log lines, no rows back.
-- ---------------------------------------------------------------------

INSERT INTO long_script.regions (code, label) VALUES ('LV', 'Latvia');

INSERT INTO long_script.regions (code, label) VALUES ('EE', 'Estonia');

INSERT INTO long_script.regions (code, label) VALUES ('LT', 'Lithuania');

INSERT INTO long_script.regions (code, label) VALUES ('SE', 'Sweden');

INSERT INTO long_script.regions (code, label) VALUES ('FI', 'Finland');

INSERT INTO long_script.regions (code, label) VALUES ('DE', 'Germany');

INSERT INTO long_script.regions (code, label) VALUES ('GB', 'United Kingdom');

INSERT INTO long_script.regions (code, label) VALUES ('US', 'United States');

DO $$
BEGIN
    PERFORM pg_sleep(1);
    RAISE NOTICE 'long_script: regions loaded';
END;
$$;

-- ---------------------------------------------------------------------
-- 3. Bulk load, then a long run of single-row INSERTs so the log gets a
--    stretch of near-identical lines to scroll through.
-- ---------------------------------------------------------------------

INSERT INTO long_script.sales (region, sold_on, amount, note)
SELECT
    (ARRAY['LV', 'EE', 'LT', 'SE', 'FI', 'DE', 'GB', 'US'])[1 + (i % 8)],
    date '2024-01-01' + (i % 540),
    round((random() * 900 + 10)::numeric, 2),
    'bulk row ' || i
FROM generate_series(1, 20000) AS i;

ANALYZE long_script.sales;

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('EE', date '2024-06-01' + 1, 107.50, 'single row 1');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('LT', date '2024-06-01' + 2, 114.50, 'single row 2');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('SE', date '2024-06-01' + 3, 121.50, 'single row 3');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('FI', date '2024-06-01' + 4, 128.50, 'single row 4');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('DE', date '2024-06-01' + 5, 135.50, 'single row 5');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('GB', date '2024-06-01' + 6, 142.50, 'single row 6');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('US', date '2024-06-01' + 7, 149.50, 'single row 7');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('LV', date '2024-06-01' + 8, 156.50, 'single row 8');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('EE', date '2024-06-01' + 9, 163.50, 'single row 9');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('LT', date '2024-06-01' + 10, 170.50, 'single row 10');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('SE', date '2024-06-01' + 11, 177.50, 'single row 11');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('FI', date '2024-06-01' + 12, 184.50, 'single row 12');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('DE', date '2024-06-01' + 13, 191.50, 'single row 13');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('GB', date '2024-06-01' + 14, 198.50, 'single row 14');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('US', date '2024-06-01' + 15, 205.50, 'single row 15');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('LV', date '2024-06-01' + 16, 212.50, 'single row 16');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('EE', date '2024-06-01' + 17, 219.50, 'single row 17');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('LT', date '2024-06-01' + 18, 226.50, 'single row 18');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('SE', date '2024-06-01' + 19, 233.50, 'single row 19');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('FI', date '2024-06-01' + 20, 240.50, 'single row 20');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('DE', date '2024-06-01' + 21, 247.50, 'single row 21');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('GB', date '2024-06-01' + 22, 254.50, 'single row 22');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('US', date '2024-06-01' + 23, 261.50, 'single row 23');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('LV', date '2024-06-01' + 24, 268.50, 'single row 24');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('EE', date '2024-06-01' + 25, 275.50, 'single row 25');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('LT', date '2024-06-01' + 26, 282.50, 'single row 26');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('SE', date '2024-06-01' + 27, 289.50, 'single row 27');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('FI', date '2024-06-01' + 28, 296.50, 'single row 28');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('DE', date '2024-06-01' + 29, 303.50, 'single row 29');

INSERT INTO long_script.sales (region, sold_on, amount, note)
VALUES ('GB', date '2024-06-01' + 30, 310.50, 'single row 30');

INSERT INTO long_script.audit (step) VALUES ('inserts done');

DO $$
DECLARE
    total bigint;
BEGIN
    SELECT count(*) INTO total FROM long_script.sales;
    RAISE NOTICE 'long_script: % sales rows loaded', total;
    PERFORM pg_sleep(2);
END;
$$;

-- ---------------------------------------------------------------------
-- 4. Updates and deletes: row counts in the log, nothing on the table.
-- ---------------------------------------------------------------------

UPDATE long_script.sales SET note = note || ' (reviewed)' WHERE amount > 800;

UPDATE long_script.sales SET amount = round(amount * 1.05, 2) WHERE region = 'LV';

UPDATE long_script.sales SET amount = round(amount * 1.05, 2) WHERE region = 'EE';

UPDATE long_script.sales SET amount = round(amount * 1.05, 2) WHERE region = 'LT';

UPDATE long_script.sales SET amount = round(amount * 0.95, 2) WHERE region = 'SE';

UPDATE long_script.sales SET amount = round(amount * 0.95, 2) WHERE region = 'FI';

UPDATE long_script.sales SET note = 'cheap' WHERE amount < 50;

UPDATE long_script.sales SET sold_on = sold_on + 1 WHERE sold_on < date '2024-02-01';

DELETE FROM long_script.sales WHERE amount < 15;

DELETE FROM long_script.sales WHERE note IS NULL;

-- Matches nothing: a zero-row log line.
DELETE FROM long_script.sales WHERE region = 'ZZ';

INSERT INTO long_script.audit (step) VALUES ('updates done');

-- ---------------------------------------------------------------------
-- 5. Twenty throwaway tables, each created, altered, filled and dropped.
--    Sixty more log lines, all of them silent.
-- ---------------------------------------------------------------------

CREATE TABLE long_script.chunk_01 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 0;

ALTER TABLE long_script.chunk_01 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_01 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_02 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 1;

ALTER TABLE long_script.chunk_02 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_02 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_03 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 2;

ALTER TABLE long_script.chunk_03 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_03 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_04 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 3;

ALTER TABLE long_script.chunk_04 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_04 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_05 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 4;

ALTER TABLE long_script.chunk_05 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_05 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_06 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 5;

ALTER TABLE long_script.chunk_06 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_06 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_07 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 6;

ALTER TABLE long_script.chunk_07 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_07 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_08 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 7;

ALTER TABLE long_script.chunk_08 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_08 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_09 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 8;

ALTER TABLE long_script.chunk_09 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_09 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_10 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 9;

ALTER TABLE long_script.chunk_10 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_10 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_11 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 10;

ALTER TABLE long_script.chunk_11 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_11 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_12 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 11;

ALTER TABLE long_script.chunk_12 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_12 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_13 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 12;

ALTER TABLE long_script.chunk_13 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_13 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_14 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 13;

ALTER TABLE long_script.chunk_14 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_14 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_15 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 14;

ALTER TABLE long_script.chunk_15 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_15 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_16 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 15;

ALTER TABLE long_script.chunk_16 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_16 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_17 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 16;

ALTER TABLE long_script.chunk_17 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_17 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_18 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 17;

ALTER TABLE long_script.chunk_18 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_18 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_19 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 18;

ALTER TABLE long_script.chunk_19 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_19 SET flagged = TRUE WHERE amount > 500;

CREATE TABLE long_script.chunk_20 AS
SELECT id, region, amount FROM long_script.sales WHERE (id % 20) = 19;

ALTER TABLE long_script.chunk_20 ADD COLUMN flagged boolean NOT NULL DEFAULT FALSE;

UPDATE long_script.chunk_20 SET flagged = TRUE WHERE amount > 500;

DO $$
BEGIN
    RAISE NOTICE 'long_script: chunk tables built';
    PERFORM pg_sleep(2);
END;
$$;

DROP TABLE long_script.chunk_01;

DROP TABLE long_script.chunk_02;

DROP TABLE long_script.chunk_03;

DROP TABLE long_script.chunk_04;

DROP TABLE long_script.chunk_05;

DROP TABLE long_script.chunk_06;

DROP TABLE long_script.chunk_07;

DROP TABLE long_script.chunk_08;

DROP TABLE long_script.chunk_09;

DROP TABLE long_script.chunk_10;

DROP TABLE long_script.chunk_11;

DROP TABLE long_script.chunk_12;

DROP TABLE long_script.chunk_13;

DROP TABLE long_script.chunk_14;

DROP TABLE long_script.chunk_15;

DROP TABLE long_script.chunk_16;

DROP TABLE long_script.chunk_17;

DROP TABLE long_script.chunk_18;

DROP TABLE long_script.chunk_19;

DROP TABLE long_script.chunk_20;

INSERT INTO long_script.audit (step) VALUES ('chunks dropped');

-- ---------------------------------------------------------------------
-- 6. Other object kinds, so the run is not only tables.
-- ---------------------------------------------------------------------

CREATE VIEW long_script.busy_regions AS
SELECT region, count(*) AS sales, sum(amount) AS total
FROM long_script.sales
GROUP BY region
HAVING count(*) > 1000;

CREATE MATERIALIZED VIEW long_script.region_totals AS
SELECT region, sum(amount) AS total
FROM long_script.sales
GROUP BY region;

REFRESH MATERIALIZED VIEW long_script.region_totals;

CREATE FUNCTION long_script.note_step(p_step text)
RETURNS void
LANGUAGE plpgsql
AS $function$
BEGIN
    INSERT INTO long_script.audit (step) VALUES (p_step);
    RAISE NOTICE 'long_script: step %', p_step;
END;
$function$;

-- Called through PERFORM, so even the void return value never reaches
-- the results table.
DO $$
BEGIN
    PERFORM long_script.note_step('function called');
END;
$$;

CREATE PROCEDURE long_script.bump_region(p_region char(2))
LANGUAGE plpgsql
AS $procedure$
BEGIN
    UPDATE long_script.sales
    SET amount = round(amount * 1.01, 2)
    WHERE region = p_region;

    RAISE NOTICE 'long_script: bumped %', p_region;
END;
$procedure$;

CALL long_script.bump_region('LV');

CALL long_script.bump_region('EE');

CALL long_script.bump_region('DE');

CALL long_script.bump_region('US');

CREATE FUNCTION long_script.audit_trigger()
RETURNS trigger
LANGUAGE plpgsql
AS $trigger$
BEGIN
    INSERT INTO long_script.audit (step) VALUES ('sales insert');
    RETURN NULL;
END;
$trigger$;

CREATE TRIGGER trg_long_script_audit
AFTER INSERT ON long_script.sales
FOR EACH STATEMENT
EXECUTE FUNCTION long_script.audit_trigger();

-- ---------------------------------------------------------------------
-- 7. Chatty DO blocks: many notices out of single statements.
-- ---------------------------------------------------------------------

DO $$
DECLARE
    rec record;
BEGIN
    FOR rec IN SELECT region, total FROM long_script.region_totals ORDER BY region
    LOOP
        RAISE NOTICE 'long_script: region % totals %', rec.region, rec.total;
    END LOOP;
    RAISE WARNING 'long_script: halfway marker';
    RAISE INFO 'long_script: info lines come through the same sink';
END;
$$;

DO $$
BEGIN
    FOR i IN 1..20 LOOP
        RAISE NOTICE 'long_script: tick %/20', i;
        PERFORM pg_sleep(0.1);
    END LOOP;
END;
$$;

-- A slow statement with nothing to show: worth cancelling mid-run.
DO $$
BEGIN
    PERFORM count(*)
    FROM long_script.sales AS a
    CROSS JOIN generate_series(1, 200) AS g;

    RAISE NOTICE 'long_script: heavy scan finished';
END;
$$;

DO $$
BEGIN
    PERFORM pg_sleep(5);
    RAISE NOTICE 'long_script: long pause over';
END;
$$;

-- ---------------------------------------------------------------------
-- 8. Tear down, one object at a time.
-- ---------------------------------------------------------------------

DROP TRIGGER trg_long_script_audit ON long_script.sales;

DROP PROCEDURE long_script.bump_region(char(2));

DROP FUNCTION long_script.note_step(text);

DROP FUNCTION long_script.audit_trigger();

DROP MATERIALIZED VIEW long_script.region_totals;

DROP VIEW long_script.busy_regions;

TRUNCATE long_script.sales;

DROP INDEX long_script.idx_long_script_sales_sold_on;

DROP INDEX long_script.idx_long_script_sales_region;

DROP TABLE long_script.sales;

DROP TABLE long_script.regions;

DROP TABLE long_script.audit;

DO $$
BEGIN
    RAISE NOTICE 'long_script: done at %', clock_timestamp();
END;
$$;

DROP SCHEMA long_script CASCADE;

RESET search_path;
