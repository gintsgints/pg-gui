use postgres::{Client, NoTls, SimpleQueryMessage};

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
        .map_err(|e| format!("connection failed: {e}"))?;

    let results = client.simple_query(sql).map_err(|e| format!("{e}"))?;

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
                    current_cols = row
                        .columns()
                        .iter()
                        .map(|c| c.name().to_string())
                        .collect();
                }
                current_rows.push(
                    (0..row.len())
                        .map(|i| row.get(i).map(|s| s.to_string()))
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
