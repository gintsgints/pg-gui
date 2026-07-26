use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use postgres::error::ErrorPosition;
use postgres::{Client, NoTls, SimpleQueryMessage, SimpleQueryRow};

use crate::export;

/// How long a connection attempt may take before it fails, so an
/// unreachable server errors out quickly instead of hanging on the
/// OS-level TCP timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// Connect with [`CONNECT_TIMEOUT`] applied.
fn connect(conn_str: &str) -> Result<Client, postgres::Error> {
    let mut config = conn_str.parse::<postgres::Config>()?;
    config.connect_timeout(CONNECT_TIMEOUT);
    config.connect(NoTls)
}

/// Render an error with its full cause. `postgres::Error`'s `Display` is
/// just "db error" — the message, detail, and hint live in the underlying
/// `DbError`, and connection failures bury the cause in the source chain.
pub(crate) fn describe(error: &postgres::Error) -> String {
    let Some(db) = error.as_db_error() else {
        let mut out = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(err) = source {
            out.push_str(": ");
            out.push_str(&err.to_string());
            source = err.source();
        }
        return out;
    };

    let mut out = format!(
        "{}: {} (SQLSTATE {})",
        db.severity(),
        db.message(),
        db.code().code()
    );
    if let Some(detail) = db.detail() {
        out.push_str("\nDetail: ");
        out.push_str(detail);
    }
    if let Some(hint) = db.hint() {
        out.push_str("\nHint: ");
        out.push_str(hint);
    }
    if let Some(where_) = db.where_() {
        out.push_str("\nWhere: ");
        out.push_str(where_);
    }
    if let Some(&ErrorPosition::Original(position)) = db.position() {
        // Writing to a String cannot fail.
        let _ = write!(out, "\nAt character {position}");
    }
    out
}

/// Rows as returned by the simple query protocol: every value is text,
/// NULL is `None`.
pub type Rows = Vec<Vec<Option<String>>>;

/// Result of executing a SQL script: the last result set plus per-statement messages.
pub struct QueryOutcome {
    pub columns: Vec<String>,
    pub rows: Rows,
    pub messages: Vec<String>,
}

/// A server-side cursor held open between fetches so a large SELECT is
/// pulled in batches instead of all at once. Owns its connection; dropping
/// it closes the connection, which aborts the transaction and with it the
/// cursor.
pub struct Cursor {
    client: Client,
    batch_size: usize,
}

/// The first batch of a cursor-backed SELECT.
pub struct CursorPage {
    pub columns: Vec<String>,
    pub rows: Rows,
    /// `None` when the first batch already exhausted the result set.
    pub cursor: Option<Cursor>,
}

/// Why [`open_cursor`] failed, so the caller knows whether re-running the
/// statement without a cursor is safe.
#[derive(Debug)]
pub enum CursorError {
    /// `DECLARE CURSOR` was rejected (e.g. a data-modifying CTE) — nothing
    /// was executed, so the caller may retry via [`run_script`], which also
    /// reports the error without the `DECLARE` prefix shifting its position.
    Declare,
    /// Connecting or fetching failed; retrying could execute the statement
    /// a second time.
    Fetch(String),
}

fn parse_row(row: &SimpleQueryRow) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| row.get(i).map(std::string::ToString::to_string))
        .collect()
}

/// Open a cursor over a single SELECT-style statement (`sql` must not end
/// with a semicolon) and pull the first `batch_size` rows.
pub fn open_cursor(
    conn_str: &str,
    sql: &str,
    batch_size: usize,
) -> Result<CursorPage, CursorError> {
    let mut client = connect(conn_str)
        .map_err(|e| CursorError::Fetch(format!("connection failed: {}", describe(&e))))?;
    // One batch so a DECLARE failure rolls the transaction back implicitly.
    client
        .batch_execute(&format!(
            "BEGIN; DECLARE _pg_gui_results NO SCROLL CURSOR FOR {sql}"
        ))
        .map_err(|_| CursorError::Declare)?;
    let mut cursor = Cursor { client, batch_size };
    let (columns, rows) = cursor.fetch_batch().map_err(CursorError::Fetch)?;
    let more = rows.len() == batch_size;
    Ok(CursorPage {
        columns,
        rows,
        cursor: more.then_some(cursor),
    })
}

impl Cursor {
    /// Pull the next batch, consuming the cursor. Returns the rows plus the
    /// cursor when more rows may remain; once exhausted the cursor is
    /// dropped, closing its connection.
    pub fn fetch_more(mut self) -> Result<(Rows, Option<Self>), String> {
        let (_, rows) = self.fetch_batch()?;
        let more = rows.len() == self.batch_size;
        Ok((rows, more.then_some(self)))
    }

    fn fetch_batch(&mut self) -> Result<(Vec<String>, Rows), String> {
        let results = self
            .client
            .simple_query(&format!(
                "FETCH FORWARD {} FROM _pg_gui_results",
                self.batch_size
            ))
            .map_err(|e| describe(&e))?;
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        for msg in results {
            match msg {
                SimpleQueryMessage::RowDescription(cols) => {
                    columns = cols.iter().map(|c| c.name().to_string()).collect();
                }
                SimpleQueryMessage::Row(row) => {
                    if columns.is_empty() {
                        columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    rows.push(parse_row(&row));
                }
                _ => {}
            }
        }
        Ok((columns, rows))
    }
}

/// The connection's effective schema search path (including what `ALTER
/// ROLE/DATABASE … SET search_path` configured server-side), resolved to
/// schemas that actually exist. `None` when the server is unreachable or
/// the path could not be read.
pub fn search_path(conn_str: &str) -> Option<Vec<String>> {
    let mut client = connect(conn_str).ok()?;
    let results = client
        .simple_query("SELECT unnest(current_schemas(false))")
        .ok()?;
    let schemas: Vec<String> = results
        .into_iter()
        .filter_map(|msg| match msg {
            SimpleQueryMessage::Row(row) => row.get(0).map(ToString::to_string),
            _ => None,
        })
        .collect();
    (!schemas.is_empty()).then_some(schemas)
}

/// Open (and immediately drop) a connection to check that the connection
/// string points at a reachable server that accepts the credentials. Used
/// by the New Connection dialog's Test Connection button.
pub fn test_connection(conn_str: &str) -> Result<(), String> {
    connect(conn_str).map(|_| ()).map_err(|e| describe(&e))
}

// --- Catalog introspection for the database object browser ---------------
//
// Each function opens a fresh connection (the browser loads objects lazily,
// one level per node expansion — see [`crate::db_tree`]), runs one
// `pg_catalog` query through the simple query protocol (every value comes
// back as text), and maps the rows to a small `Send` result type. Object
// names are always fetched from the catalog, but are still quoted defensively
// with [`quote_literal`] before being spliced into the SQL.

/// An index, as shown under a table's Indexes folder.
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    pub primary: bool,
}

/// A constraint, as shown under a table's Constraints folder. `kind` is the
/// raw `pg_constraint.contype` code (`p`/`f`/`u`/`c`/`x`/…).
pub struct ConstraintInfo {
    pub name: String,
    pub kind: char,
}

/// Quote a string as a SQL literal, doubling embedded single quotes.
fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Quote a string as a SQL identifier, doubling embedded double quotes, so a
/// name with mixed case or special characters round-trips in generated DDL.
fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Whether a simple-query text boolean (`"t"`/`"f"`) is true.
fn is_true(value: Option<&str>) -> bool {
    value == Some("t")
}

/// Run a query and return the first column of the first row, `None` when the
/// query yields no rows or that value is NULL.
fn catalog_scalar(conn_str: &str, sql: &str) -> Result<Option<String>, String> {
    Ok(catalog_rows(conn_str, sql)?
        .first()
        .and_then(|row| row.get(0).map(ToString::to_string)))
}

/// Run a simple query and keep only the data rows.
fn catalog_rows(conn_str: &str, sql: &str) -> Result<Vec<SimpleQueryRow>, String> {
    let mut client = connect(conn_str).map_err(|e| describe(&e))?;
    let messages = client.simple_query(sql).map_err(|e| describe(&e))?;
    Ok(messages
        .into_iter()
        .filter_map(|msg| match msg {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .collect())
}

/// Schema names, ordered. System schemas (`pg_*`, `information_schema`) are
/// excluded unless `show_system`.
pub fn list_schemas(conn_str: &str, show_system: bool) -> Result<Vec<String>, String> {
    let filter = if show_system {
        String::new()
    } else {
        " WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema'".to_string()
    };
    let sql = format!("SELECT nspname FROM pg_catalog.pg_namespace{filter} ORDER BY nspname");
    Ok(catalog_rows(conn_str, &sql)?
        .iter()
        .filter_map(|row| row.get(0).map(ToString::to_string))
        .collect())
}

/// Relation names in `schema` of a given `pg_class.relkind` (`r` tables,
/// `v` views, `m` materialized views, `S` sequences), ordered.
pub fn list_relations(conn_str: &str, schema: &str, relkind: char) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT c.relname \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema} AND c.relkind = {kind} \
         ORDER BY c.relname",
        schema = quote_literal(schema),
        kind = quote_literal(&relkind.to_string()),
    );
    Ok(catalog_rows(conn_str, &sql)?
        .iter()
        .filter_map(|row| row.get(0).map(ToString::to_string))
        .collect())
}

/// Function/procedure signatures in `schema`, ordered. Each entry is
/// `name(identity arguments)` so overloads stay distinct.
pub fn list_functions(conn_str: &str, schema: &str) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT p.proname \
             || '(' || pg_catalog.pg_get_function_identity_arguments(p.oid) || ')' \
         FROM pg_catalog.pg_proc p \
         JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = {schema} \
         ORDER BY p.proname, 1",
        schema = quote_literal(schema),
    );
    Ok(catalog_rows(conn_str, &sql)?
        .iter()
        .filter_map(|row| row.get(0).map(ToString::to_string))
        .collect())
}

/// User-defined type names in `schema` (composite, enum, domain, base),
/// ordered. Array and implicit relation row types are excluded, matching
/// psql's `\dT`.
pub fn list_types(conn_str: &str, schema: &str) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT t.typname \
         FROM pg_catalog.pg_type t \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
         WHERE n.nspname = {schema} \
           AND (t.typrelid = 0 \
                OR (SELECT c.relkind FROM pg_catalog.pg_class c WHERE c.oid = t.typrelid) = 'c') \
           AND NOT EXISTS ( \
                SELECT 1 FROM pg_catalog.pg_type el \
                WHERE el.oid = t.typelem AND el.typarray = t.oid) \
         ORDER BY t.typname",
        schema = quote_literal(schema),
    );
    Ok(catalog_rows(conn_str, &sql)?
        .iter()
        .filter_map(|row| row.get(0).map(ToString::to_string))
        .collect())
}

/// Indexes on `schema.relation`, ordered by name.
pub fn list_indexes(
    conn_str: &str,
    schema: &str,
    relation: &str,
) -> Result<Vec<IndexInfo>, String> {
    let sql = format!(
        "SELECT ic.relname, ix.indisunique, ix.indisprimary \
         FROM pg_catalog.pg_index ix \
         JOIN pg_catalog.pg_class ic ON ic.oid = ix.indexrelid \
         JOIN pg_catalog.pg_class tc ON tc.oid = ix.indrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = tc.relnamespace \
         WHERE n.nspname = {schema} AND tc.relname = {relation} \
         ORDER BY ic.relname",
        schema = quote_literal(schema),
        relation = quote_literal(relation),
    );
    Ok(catalog_rows(conn_str, &sql)?
        .iter()
        .map(|row| IndexInfo {
            name: row.get(0).unwrap_or_default().to_string(),
            unique: is_true(row.get(1)),
            primary: is_true(row.get(2)),
        })
        .collect())
}

/// Constraints on `schema.relation`, ordered by name.
pub fn list_constraints(
    conn_str: &str,
    schema: &str,
    relation: &str,
) -> Result<Vec<ConstraintInfo>, String> {
    let sql = format!(
        "SELECT con.conname, con.contype \
         FROM pg_catalog.pg_constraint con \
         JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema} AND c.relname = {relation} \
         ORDER BY con.conname",
        schema = quote_literal(schema),
        relation = quote_literal(relation),
    );
    Ok(catalog_rows(conn_str, &sql)?
        .iter()
        .map(|row| ConstraintInfo {
            name: row.get(0).unwrap_or_default().to_string(),
            kind: row.get(1).and_then(|s| s.chars().next()).unwrap_or('?'),
        })
        .collect())
}

/// Runnable `CREATE` statement reconstructing a view or materialized view.
/// The body comes from `pg_get_viewdef`; the header is generated so the whole
/// thing re-creates the object.
pub fn view_definition(
    conn_str: &str,
    schema: &str,
    name: &str,
    materialized: bool,
) -> Result<String, String> {
    let sql = format!(
        "SELECT pg_catalog.pg_get_viewdef(c.oid, true) \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema} AND c.relname = {name}",
        schema = quote_literal(schema),
        name = quote_literal(name),
    );
    let body =
        catalog_scalar(conn_str, &sql)?.ok_or_else(|| format!("view {schema}.{name} not found"))?;
    let keyword = if materialized {
        "CREATE MATERIALIZED VIEW"
    } else {
        "CREATE OR REPLACE VIEW"
    };
    Ok(format!(
        "{keyword} {}.{} AS\n{body}",
        quote_ident(schema),
        quote_ident(name),
    ))
}

/// Full `CREATE OR REPLACE FUNCTION`/`PROCEDURE` text from
/// `pg_get_functiondef`. `signature` is the `name(identity arguments)` form
/// carried by the browser leaf, so overloads resolve to the right routine.
pub fn function_definition(
    conn_str: &str,
    schema: &str,
    signature: &str,
) -> Result<String, String> {
    // Match the `name(identity arguments)` string the browser leaf was built
    // from (see `list_functions`) rather than casting to `regprocedure`: the
    // identity-argument text does not always parse back as a `regprocedure`
    // (argument modes, `VARIADIC`, quoting), which raises a hard error.
    let sql = format!(
        "SELECT pg_catalog.pg_get_functiondef(p.oid) \
         FROM pg_catalog.pg_proc p \
         JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = {schema} \
           AND p.proname || '(' || pg_catalog.pg_get_function_identity_arguments(p.oid) || ')' \
               = {signature}",
        schema = quote_literal(schema),
        signature = quote_literal(signature),
    );
    catalog_scalar(conn_str, &sql)?
        .ok_or_else(|| format!("function {schema}.{signature} not found"))
}

/// Best-effort `CREATE SEQUENCE` reconstructed from `information_schema`.
pub fn sequence_definition(conn_str: &str, schema: &str, name: &str) -> Result<String, String> {
    let sql = format!(
        "SELECT data_type, start_value, increment, minimum_value, maximum_value, cycle_option \
         FROM information_schema.sequences \
         WHERE sequence_schema = {schema} AND sequence_name = {name}",
        schema = quote_literal(schema),
        name = quote_literal(name),
    );
    let row = catalog_rows(conn_str, &sql)?
        .into_iter()
        .next()
        .ok_or_else(|| format!("sequence {schema}.{name} not found"))?;
    let col = |i: usize| row.get(i).unwrap_or_default().to_string();
    let cycle = if row.get(5) == Some("YES") {
        "CYCLE"
    } else {
        "NO CYCLE"
    };
    Ok(format!(
        "CREATE SEQUENCE {}.{}\n    AS {}\n    START WITH {}\n    INCREMENT BY {}\n    MINVALUE {}\n    MAXVALUE {}\n    {cycle};",
        quote_ident(schema),
        quote_ident(name),
        col(0),
        col(1),
        col(2),
        col(3),
        col(4),
    ))
}

/// Best-effort DDL for a user-defined type. Composite, enum and domain types
/// are reconstructed fully; other kinds (base, range, …) return a comment
/// noting the kind is not supported.
pub fn type_definition(conn_str: &str, schema: &str, name: &str) -> Result<String, String> {
    let sql = format!(
        "SELECT t.typtype, t.oid, t.typrelid, \
                pg_catalog.format_type(t.typbasetype, t.typtypmod), \
                t.typnotnull, t.typdefault \
         FROM pg_catalog.pg_type t \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
         WHERE n.nspname = {schema} AND t.typname = {name}",
        schema = quote_literal(schema),
        name = quote_literal(name),
    );
    let row = catalog_rows(conn_str, &sql)?
        .into_iter()
        .next()
        .ok_or_else(|| format!("type {schema}.{name} not found"))?;
    let typtype = row.get(0).and_then(|s| s.chars().next()).unwrap_or('?');
    let oid = row.get(1).unwrap_or_default().to_string();
    let typrelid = row.get(2).unwrap_or_default().to_string();
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident(name));

    match typtype {
        // Composite type: reconstruct from its columns.
        'c' => {
            let cols_sql = format!(
                "SELECT string_agg(quote_ident(a.attname) || ' ' \
                            || pg_catalog.format_type(a.atttypid, a.atttypmod), ', ' \
                            ORDER BY a.attnum) \
                 FROM pg_catalog.pg_attribute a \
                 WHERE a.attrelid = {typrelid} AND a.attnum > 0 AND NOT a.attisdropped"
            );
            let cols = catalog_scalar(conn_str, &cols_sql)?.unwrap_or_default();
            Ok(format!("CREATE TYPE {qualified} AS ({cols});"))
        }
        // Enum: reconstruct from its labels.
        'e' => {
            let labels_sql = format!(
                "SELECT string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder) \
                 FROM pg_catalog.pg_enum e WHERE e.enumtypid = {oid}"
            );
            let labels = catalog_scalar(conn_str, &labels_sql)?.unwrap_or_default();
            Ok(format!("CREATE TYPE {qualified} AS ENUM ({labels});"))
        }
        // Domain: base type plus NOT NULL / DEFAULT / CHECK modifiers.
        'd' => {
            let base = row.get(3).unwrap_or_default().to_string();
            let mut out = format!("CREATE DOMAIN {qualified} AS {base}");
            if is_true(row.get(4)) {
                out.push_str(" NOT NULL");
            }
            if let Some(default) = row.get(5) {
                let _ = write!(out, " DEFAULT {default}");
            }
            let checks_sql = format!(
                "SELECT string_agg(pg_catalog.pg_get_constraintdef(con.oid), ' ') \
                 FROM pg_catalog.pg_constraint con WHERE con.contypid = {oid}"
            );
            if let Some(checks) = catalog_scalar(conn_str, &checks_sql)? {
                let _ = write!(out, " {checks}");
            }
            out.push(';');
            Ok(out)
        }
        other => Ok(format!(
            "-- Definition of type {schema}.{name} (typtype '{other}') is not supported."
        )),
    }
}

/// `CREATE INDEX` text from `pg_get_indexdef`.
pub fn index_definition(conn_str: &str, schema: &str, name: &str) -> Result<String, String> {
    let sql = format!(
        "SELECT pg_catalog.pg_get_indexdef(c.oid) \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema} AND c.relname = {name} AND c.relkind IN ('i', 'I')",
        schema = quote_literal(schema),
        name = quote_literal(name),
    );
    let def = catalog_scalar(conn_str, &sql)?
        .ok_or_else(|| format!("index {schema}.{name} not found"))?;
    Ok(format!("{def};"))
}

/// `ALTER TABLE … ADD CONSTRAINT` text from `pg_get_constraintdef`.
pub fn constraint_definition(
    conn_str: &str,
    schema: &str,
    relation: &str,
    name: &str,
) -> Result<String, String> {
    let sql = format!(
        "SELECT pg_catalog.pg_get_constraintdef(con.oid) \
         FROM pg_catalog.pg_constraint con \
         JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema} AND c.relname = {relation} AND con.conname = {name}",
        schema = quote_literal(schema),
        relation = quote_literal(relation),
        name = quote_literal(name),
    );
    let def = catalog_scalar(conn_str, &sql)?
        .ok_or_else(|| format!("constraint {name} on {schema}.{relation} not found"))?;
    Ok(format!(
        "ALTER TABLE {}.{} ADD CONSTRAINT {} {def};",
        quote_ident(schema),
        quote_ident(relation),
        quote_ident(name),
    ))
}

/// Execute a SQL script (one or more statements) using the simple query protocol,
/// which returns every value as text and supports multi-statement scripts.
pub fn run_script(conn_str: &str, sql: &str) -> Result<QueryOutcome, String> {
    let mut client =
        connect(conn_str).map_err(|e| format!("connection failed: {}", describe(&e)))?;

    let results = client.simple_query(sql).map_err(|e| describe(&e))?;

    let mut outcome = QueryOutcome {
        columns: Vec::new(),
        rows: Vec::new(),
        messages: Vec::new(),
    };

    let mut current_cols: Vec<String> = Vec::new();
    let mut current_rows: Vec<Vec<Option<String>>> = Vec::new();

    for msg in results {
        match msg {
            SimpleQueryMessage::RowDescription(cols) => {
                current_cols = cols.iter().map(|c| c.name().to_string()).collect();
                current_rows.clear();
            }
            SimpleQueryMessage::Row(row) => {
                if current_cols.is_empty() {
                    current_cols = row.columns().iter().map(|c| c.name().to_string()).collect();
                }
                current_rows.push(parse_row(&row));
            }
            SimpleQueryMessage::CommandComplete(n) => {
                outcome.messages.push(format!("ok ({n} rows)"));
                // Keep the most recent result set that produced columns.
                if !current_cols.is_empty() {
                    outcome.columns = std::mem::take(&mut current_cols);
                    outcome.rows = std::mem::take(&mut current_rows);
                }
            }
            _ => {}
        }
    }

    Ok(outcome)
}

/// Re-run `sql` server-side wrapped in `COPY (…) TO STDOUT WITH (FORMAT
/// csv, HEADER)` and stream the output to `path` — the server does all the
/// CSV quoting and the rows never accumulate in memory. Returns the number
/// of bytes written.
pub fn export_csv(conn_str: &str, sql: &str, path: &Path) -> Result<u64, String> {
    let sql = export::copyable(sql)?;
    let mut client =
        connect(conn_str).map_err(|e| format!("connection failed: {}", describe(&e)))?;
    let mut reader = client
        .copy_out(&format!("COPY ({sql}) TO STDOUT WITH (FORMAT csv, HEADER)"))
        .map_err(|e| describe(&e))?;
    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    let bytes =
        std::io::copy(&mut reader, &mut writer).map_err(|e| format!("write failed: {e}"))?;
    writer.flush().map_err(|e| format!("write failed: {e}"))?;
    Ok(bytes)
}

/// Run `sql` and write its result set to `path` as one `INSERT` statement
/// per row (see `export::insert_statements`). Returns the number of rows
/// written.
pub fn export_inserts(conn_str: &str, sql: &str, path: &Path) -> Result<usize, String> {
    // Validate like the CSV path so both formats behave identically on
    // multi-statement or non-SELECT input.
    let sql = export::copyable(sql)?;
    let outcome = run_script(conn_str, sql)?;
    if outcome.columns.is_empty() {
        return Err("the statement returned no result set".to_string());
    }
    let script =
        export::insert_statements(&export::table_name(sql), &outcome.columns, &outcome.rows);
    std::fs::write(path, script).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(outcome.rows.len())
}

#[cfg(test)]
mod tests {
    use super::{CursorError, export_csv, export_inserts, open_cursor, search_path};

    const CONN: &str = "postgres://pgui:pgui@localhost:5433/pgui_test";

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("pg_gui_export_{}_{name}", std::process::id()))
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn export_csv_streams_header_and_quoting() {
        let path = temp_path("test.csv");
        let bytes = export_csv(
            CONN,
            "SELECT 1 AS id, E'a,b\\nc' AS v, NULL::text AS n;",
            &path,
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(u64::try_from(content.len()).unwrap(), bytes);
        assert!(content.starts_with("id,v,n\n"), "{content}");
        // The embedded comma and newline force server-side quoting; the
        // trailing NULL is an empty field.
        assert!(content.contains("1,\"a,b\nc\",\n"), "{content}");
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn export_inserts_renders_rows() {
        let path = temp_path("test.sql");
        let rows = export_inserts(
            CONN,
            "SELECT 'O''Brien' AS name, NULL::text AS note FROM generate_series(1, 2)",
            &path,
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(rows, 2);
        // `generate_series(1, 2)` is not a usable table name, so the
        // placeholder is used.
        assert_eq!(
            content.lines().next().unwrap(),
            "INSERT INTO my_table (\"name\", \"note\") VALUES ('O''Brien', NULL);"
        );
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn cursor_fetches_in_batches() {
        let page = open_cursor(CONN, "SELECT g FROM generate_series(1, 12) g", 5).unwrap();
        assert_eq!(page.columns, vec!["g"]);
        assert_eq!(page.rows.len(), 5);
        assert_eq!(page.rows[0][0].as_deref(), Some("1"));

        let (rows, cursor) = page.cursor.unwrap().fetch_more().unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0][0].as_deref(), Some("6"));

        // The last, short batch exhausts the cursor.
        let (rows, cursor) = cursor.unwrap().fetch_more().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0].as_deref(), Some("12"));
        assert!(cursor.is_none());
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn cursor_exact_multiple_ends_with_empty_fetch() {
        let page = open_cursor(CONN, "SELECT g FROM generate_series(1, 4) g", 4).unwrap();
        assert_eq!(page.rows.len(), 4);
        // A full first batch keeps the cursor open; the next fetch is empty.
        let (rows, cursor) = page.cursor.unwrap().fetch_more().unwrap();
        assert!(rows.is_empty());
        assert!(cursor.is_none());
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn cursor_rejects_statements_declare_cannot_run() {
        // SELECT INTO passes the first-word check but DECLARE refuses it;
        // the caller falls back to run_script on this variant.
        let Err(err) = open_cursor(CONN, "SELECT 1 INTO TEMP _pg_gui_t", 10) else {
            panic!("expected DECLARE to reject SELECT INTO");
        };
        assert!(matches!(err, CursorError::Declare), "{err:?}");
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn search_path_reflects_role_setting() {
        // The docker role is configured with `SET search_path TO app, public`.
        let schemas = search_path(CONN).unwrap();
        assert_eq!(schemas, vec!["app", "public"]);
    }

    #[test]
    fn search_path_is_none_when_unreachable() {
        assert!(search_path("postgres://nobody:nope@127.0.0.1:1/none").is_none());
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn export_csv_rejects_non_select() {
        let err = export_csv(CONN, "UPDATE t SET x = 1", &temp_path("reject.csv")).unwrap_err();
        assert!(err.contains("SELECT"), "{err}");
    }
}
