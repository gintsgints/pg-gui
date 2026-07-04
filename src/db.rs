use std::fmt::Write as _;

use postgres::error::ErrorPosition;
use postgres::{Client, NoTls, SimpleQueryMessage};

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
