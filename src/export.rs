//! Rendering query results for export: sanitizing a statement so it can be
//! embedded in `COPY (…) TO STDOUT`, and generating SQL INSERT scripts.

use std::fmt::Write as _;

use crate::statement;

/// Trim whitespace and trailing semicolons so `sql` can be embedded in
/// `COPY (…) TO STDOUT`, and reject statements COPY cannot run. Checking
/// here gives a clear message instead of a server syntax error whose
/// character position is shifted by the `COPY (` prefix.
pub fn copyable(sql: &str) -> Result<&str, String> {
    // Stray semicolons split into segments of their own; only segments
    // with actual content count as statements.
    let statements = statement::ranges(sql)
        .into_iter()
        .filter(|range| {
            sql[range.clone()]
                .chars()
                .any(|c| c != ';' && !c.is_whitespace())
        })
        .count();
    if statements > 1 {
        return Err("Export runs a single statement — select just one".to_string());
    }
    let mut sql = sql.trim();
    while let Some(rest) = sql.strip_suffix(';') {
        sql = rest.trim_end();
    }
    let first_word = sql
        .split_whitespace()
        .next()
        .ok_or_else(|| "Nothing to export".to_string())?;
    let allowed = ["select", "values", "with", "table"];
    if !allowed.iter().any(|kw| first_word.eq_ignore_ascii_case(kw)) {
        return Err(format!(
            "Only SELECT-style statements can be exported (found \"{first_word}\")"
        ));
    }
    Ok(sql)
}

/// Best-effort table lookup: the identifier following the first `FROM`,
/// as written in the query (so schema qualification and quoting are
/// preserved). None when there is no usable FROM (e.g. `SELECT 1` or a
/// subquery).
fn find_table(sql: &str) -> Option<&str> {
    let mut tokens = sql.split_whitespace();
    while let Some(token) = tokens.next() {
        if token.eq_ignore_ascii_case("from")
            && let Some(next) = tokens.next()
        {
            let name = next.trim_end_matches([';', ',', ')']);
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '"'))
            {
                return Some(name);
            }
        }
    }
    None
}

/// The table name generated INSERTs target: the query's FROM table, or a
/// placeholder the user can search-and-replace.
pub fn table_name(sql: &str) -> String {
    find_table(sql).unwrap_or("my_table").to_string()
}

/// Default file name offered by the export save dialog: the query's table
/// name (sanitized for the filesystem) plus today's date, e.g.
/// `orders_2026-07-12.csv`.
pub fn default_file_name(sql: &str, extension: &str) -> String {
    let stem = find_table(sql).map_or_else(
        || "export".to_string(),
        |table| {
            table
                .chars()
                .filter(|c| *c != '"')
                .map(|c| {
                    if c.is_alphanumeric() || matches!(c, '_' | '-') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        },
    );
    let date = chrono::Local::now().format("%Y-%m-%d");
    format!("{stem}_{date}.{extension}")
}

/// Render a result set as one `INSERT INTO … VALUES (…);` line per row.
/// `None` renders as NULL; everything else as a single-quoted literal
/// (values come from the simple query protocol as text, and Postgres
/// casts string literals to the column type on INSERT).
pub fn insert_statements(table: &str, columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    let columns = columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = String::new();
    for row in rows {
        let values = row
            .iter()
            .map(|v| literal(v.as_deref()))
            .collect::<Vec<_>>()
            .join(", ");
        // Writing to a String cannot fail.
        let _ = writeln!(out, "INSERT INTO {table} ({columns}) VALUES ({values});");
    }
    out
}

/// Double-quote an identifier, doubling any embedded quotes.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A SQL literal: NULL, or single-quoted text with embedded quotes doubled.
fn literal(value: Option<&str>) -> String {
    value.map_or_else(
        || "NULL".to_string(),
        |v| format!("'{}'", v.replace('\'', "''")),
    )
}

#[cfg(test)]
mod tests {
    use super::{copyable, default_file_name, insert_statements, table_name};

    #[test]
    fn copyable_strips_trailing_semicolons() {
        assert_eq!(copyable("  SELECT 1;; \n"), Ok("SELECT 1"));
        assert_eq!(
            copyable("with t as (select 1) table t"),
            Ok("with t as (select 1) table t")
        );
    }

    #[test]
    fn copyable_rejects_non_select() {
        let err = copyable("UPDATE t SET x = 1").unwrap_err();
        assert!(err.contains("UPDATE"), "{err}");
        assert!(copyable("").is_err());
    }

    #[test]
    fn copyable_rejects_multiple_statements() {
        let err = copyable("SELECT 1; SELECT 2;").unwrap_err();
        assert!(err.contains("single statement"), "{err}");
        // Semicolons hidden in strings are not statement separators.
        assert!(copyable("SELECT 'a;b';").is_ok());
    }

    #[test]
    fn table_name_takes_the_first_from() {
        assert_eq!(
            table_name("select * from public.users where id = 1"),
            "public.users"
        );
        assert_eq!(table_name("SELECT * FROM orders;"), "orders");
        assert_eq!(table_name("select 1"), "my_table");
        // A subquery is not a usable name.
        assert_eq!(table_name("select * from (values (1)) t"), "my_table");
    }

    #[test]
    fn default_file_name_holds_table_and_date() {
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(
            default_file_name("select * from public.users", "csv"),
            format!("public_users_{date}.csv")
        );
        assert_eq!(
            default_file_name("SELECT * FROM \"Order Items\" LIMIT 1", "sql"),
            // `"Order Items"` has a space, so tokenizing stops at `"Order` —
            // quotes are dropped and the rest sanitized.
            format!("Order_{date}.sql")
        );
        assert_eq!(
            default_file_name("select 1", "csv"),
            format!("export_{date}.csv")
        );
    }

    #[test]
    fn insert_statements_escape_quotes_and_nulls() {
        let out = insert_statements(
            "people",
            &["id".to_string(), "name".to_string()],
            &[
                vec![Some("1".to_string()), Some("O'Brien".to_string())],
                vec![Some("2".to_string()), None],
            ],
        );
        assert_eq!(
            out,
            "INSERT INTO people (\"id\", \"name\") VALUES ('1', 'O''Brien');\n\
             INSERT INTO people (\"id\", \"name\") VALUES ('2', NULL);\n"
        );
    }
}
