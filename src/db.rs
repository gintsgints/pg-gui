use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use postgres::error::ErrorPosition;
use postgres::{Client, NoTls, SimpleQueryMessage};

use crate::export;

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

/// Result of executing a SQL script: the last result set plus per-statement messages.
pub struct QueryOutcome {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub messages: Vec<String>,
}

/// Open (and immediately drop) a connection to check that the connection
/// string points at a reachable server that accepts the credentials. Used
/// by the New Connection dialog's Test Connection button.
pub fn test_connection(conn_str: &str) -> Result<(), String> {
    Client::connect(conn_str, NoTls)
        .map(|_| ())
        .map_err(|e| describe(&e))
}

/// Execute a SQL script (one or more statements) using the simple query protocol,
/// which returns every value as text and supports multi-statement scripts.
pub fn run_script(conn_str: &str, sql: &str) -> Result<QueryOutcome, String> {
    let mut client = Client::connect(conn_str, NoTls)
        .map_err(|e| format!("connection failed: {}", describe(&e)))?;

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
                current_rows.push(
                    (0..row.len())
                        .map(|i| row.get(i).map(std::string::ToString::to_string))
                        .collect(),
                );
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
    let mut client = Client::connect(conn_str, NoTls)
        .map_err(|e| format!("connection failed: {}", describe(&e)))?;
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
    use super::{export_csv, export_inserts};

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
    fn export_csv_rejects_non_select() {
        let err = export_csv(CONN, "UPDATE t SET x = 1", &temp_path("reject.csv")).unwrap_err();
        assert!(err.contains("SELECT"), "{err}");
    }
}
