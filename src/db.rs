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
fn describe(error: &postgres::Error) -> String {
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
