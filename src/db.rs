use std::fmt::Write as _;
use std::io::Write as _;
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use postgres::error::ErrorPosition;
use postgres::{Client, NoTls, SimpleQueryMessage, SimpleQueryRow};

use crate::{export, statement};

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

/// One statement's result set within a run.
#[derive(Debug)]
pub struct ResultSet {
    /// 1-based position of the statement that produced it within the run;
    /// shown on the result selector.
    pub statement: usize,
    /// That statement collapsed onto one line, for the selector's tooltip.
    pub label: String,
    pub columns: Vec<String>,
    pub rows: Rows,
}

/// What a run reports as it goes, one message per statement (two when the
/// statement returned rows), pushed the moment that statement finishes so
/// the UI can show it while the rest of the block is still running.
#[derive(Debug)]
pub enum RunEvent {
    /// A statement finished: its log line.
    Log(String),
    /// A statement returned rows.
    Result(ResultSet),
}

/// The channel a run reports its [`RunEvent`]s on.
pub type Progress = futures::channel::mpsc::UnboundedSender<RunEvent>;

/// Result of executing a SQL script: the last result set plus per-statement messages.
#[derive(Debug)]
pub struct QueryOutcome {
    pub columns: Vec<String>,
    pub rows: Rows,
    pub messages: Vec<String>,
}

fn parse_row(row: &SimpleQueryRow) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| row.get(i).map(std::string::ToString::to_string))
        .collect()
}

/// Build a [`QueryOutcome`] from simple-query messages, keeping the most
/// recent result set that produced columns.
fn collect_outcome(results: Vec<SimpleQueryMessage>) -> QueryOutcome {
    let mut outcome = QueryOutcome {
        columns: Vec::new(),
        rows: Vec::new(),
        messages: Vec::new(),
    };
    let mut current_cols: Vec<String> = Vec::new();
    let mut current_rows: Rows = Vec::new();
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
    outcome
}

/// A live database session: one persistent connection owned by a single
/// editor tab, so temp tables, `SET`, and an open transaction survive
/// across Runs within that tab and stay isolated from other tabs.
///
/// A single SELECT is paged through a server-side cursor declared on this
/// same connection; the transaction wrapping that cursor is either the
/// user's (autocommit off) or an implicit one opened just to hold the
/// cursor (autocommit on).
pub struct Session {
    client: Client,
    /// `_pg_gui_results` is declared and not yet closed.
    cursor_open: bool,
    /// A user transaction (autocommit off) is open.
    in_txn: bool,
    /// A cursor-only transaction (autocommit on) is open, committed when the
    /// cursor is closed.
    implicit_txn: bool,
}

/// What a finished Run reports back: how many statements it ran and whether
/// the cursor was left open (more rows available via
/// [`Session::fetch_more`]). The rows and log lines themselves have already
/// gone out over the run's [`Progress`] channel.
#[derive(Debug)]
pub struct RunResult {
    pub statements: usize,
    pub more: bool,
}

/// How much of a statement a log line shows before it is elided.
const LOG_SQL_LEN: usize = 120;

/// A statement collapsed onto a single line for the log: runs of whitespace
/// (including comments' newlines) become single spaces and anything past
/// [`LOG_SQL_LEN`] characters is elided.
fn one_line(sql: &str) -> String {
    let mut out = String::new();
    for (i, word) in sql.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > LOG_SQL_LEN {
            let cut = out
                .char_indices()
                .nth(LOG_SQL_LEN)
                .map_or(out.len(), |(at, _)| at);
            out.truncate(cut);
            out.push('…');
            break;
        }
    }
    out
}

/// One log line for the `n`th statement of a batch: the statement itself,
/// what it returned (or the first line of the error that ended it), and how
/// long it took.
fn log_line(n: usize, sql: &str, summary: &str, elapsed: Duration) -> String {
    format!("{n}. {} — {summary} in {elapsed:.0?}", one_line(sql))
}

/// Push one event to the run's listener. The UI drops the receiver when its
/// tab goes away mid-run; the statements still have to finish, so a closed
/// channel is not an error.
fn send(progress: &Progress, event: RunEvent) {
    let _ = progress.unbounded_send(event);
}

/// The statements of `sql`, dropping segments that hold nothing but stray
/// semicolons and whitespace.
fn statement_ranges(sql: &str) -> Vec<Range<usize>> {
    statement::ranges(sql)
        .into_iter()
        .filter(|range| {
            sql[range.clone()]
                .chars()
                .any(|c| c != ';' && !c.is_whitespace())
        })
        .collect()
}

impl Session {
    /// Open a session over `conn_str`. The connection stays open until the
    /// session is dropped.
    pub fn connect(conn_str: &str) -> Result<Self, postgres::Error> {
        Ok(Self {
            client: connect(conn_str)?,
            cursor_open: false,
            in_txn: false,
            implicit_txn: false,
        })
    }

    /// A token usable from another thread to cancel the query currently
    /// running on this session.
    pub fn cancel_token(&self) -> postgres::CancelToken {
        self.client.cancel_token()
    }

    /// Whether a user transaction (autocommit off) is currently open.
    pub fn in_txn(&self) -> bool {
        self.in_txn
    }

    /// Whether the underlying connection has been closed (e.g. the server
    /// dropped it); a closed session should be discarded and reopened.
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Close any open cursor, committing its implicit transaction
    /// (autocommit on) or leaving the user transaction intact (autocommit
    /// off).
    fn end_cursor(&mut self) -> Result<(), String> {
        if !self.cursor_open {
            return Ok(());
        }
        let sql = if self.implicit_txn {
            "COMMIT"
        } else {
            "CLOSE _pg_gui_results"
        };
        self.client.batch_execute(sql).map_err(|e| describe(&e))?;
        self.cursor_open = false;
        self.implicit_txn = false;
        Ok(())
    }

    /// Execute `sql` one statement at a time. Every statement reports a log
    /// line — and, when it returned rows, a result set — over `progress` the
    /// moment it finishes, so the UI shows a block's output while the rest of
    /// it is still running, and a failure names the statement that caused it.
    /// Sending the block as one simple query would instead give back bare row
    /// counts, only the last result set, and nothing at all once one
    /// statement failed.
    ///
    /// A lone SELECT-style statement is paged through a server-side cursor
    /// (first `batch_size` rows sent, `more` set when the cursor is left
    /// open). With `autocommit` off, statements run inside a transaction
    /// started on the first Run and ended only by
    /// [`Self::commit`]/[`Self::rollback`]. Postgres wraps a multi-statement
    /// simple query in an implicit transaction; with autocommit on a block is
    /// wrapped in an explicit one here so it stays all-or-nothing either way.
    pub fn run(
        &mut self,
        sql: &str,
        batch_size: usize,
        autocommit: bool,
        progress: &Progress,
    ) -> Result<RunResult, String> {
        self.end_cursor()?;
        if !autocommit && !self.in_txn {
            self.client
                .batch_execute("BEGIN")
                .map_err(|e| describe(&e))?;
            self.in_txn = true;
        }
        let ranges = statement_ranges(sql);
        // Only a lone statement pages through a cursor: within a block the
        // cursor would have to stay open across the statements that follow.
        let single = ranges.len() == 1;
        let wrap = ranges.len() > 1 && !self.in_txn;
        if wrap {
            self.client
                .batch_execute("BEGIN")
                .map_err(|e| describe(&e))?;
        }
        let mut more = false;
        for (n, range) in ranges.iter().enumerate() {
            let paged = single.then_some(batch_size);
            match self.run_statement(n + 1, &sql[range.clone()], paged, progress) {
                Ok(left_open) => more = left_open,
                Err(error) => {
                    if wrap {
                        let _ = self.client.batch_execute("ROLLBACK");
                    }
                    return Err(error);
                }
            }
        }
        if wrap {
            self.client
                .batch_execute("COMMIT")
                .map_err(|e| describe(&e))?;
        }
        Ok(RunResult {
            statements: ranges.len(),
            more,
        })
    }

    /// Run the `n`th statement of a run, pushing its log line and — when it
    /// returned rows — its result set to `progress`. `paged` carries the
    /// batch size for a lone statement, which may page its rows through a
    /// cursor; the returned flag says whether that cursor was left open.
    fn run_statement(
        &mut self,
        n: usize,
        statement: &str,
        paged: Option<usize>,
        progress: &Progress,
    ) -> Result<bool, String> {
        let started = Instant::now();
        let run = match paged {
            Some(batch_size) => self.run_paged(statement, batch_size),
            None => self
                .client
                .simple_query(statement)
                .map(|results| (collect_outcome(results), false))
                .map_err(|e| describe(&e)),
        };
        let (outcome, more) = match run {
            Ok(run) => run,
            Err(error) => {
                // The dialog gets the full cause; the log line keeps to one
                // line, like every other entry.
                let first_line = error.lines().next().unwrap_or("failed");
                let summary = format!("failed: {first_line}");
                send(
                    progress,
                    RunEvent::Log(log_line(n, statement, &summary, started.elapsed())),
                );
                return Err(error);
            }
        };
        let summary = if outcome.messages.is_empty() {
            "ok".to_string()
        } else {
            outcome.messages.join("; ")
        };
        send(
            progress,
            RunEvent::Log(log_line(n, statement, &summary, started.elapsed())),
        );
        if !outcome.columns.is_empty() {
            send(
                progress,
                RunEvent::Result(ResultSet {
                    statement: n,
                    label: one_line(statement),
                    columns: outcome.columns,
                    rows: outcome.rows,
                }),
            );
        }
        Ok(more)
    }

    /// Run a lone statement, paging a plain SELECT through a cursor. Falls
    /// back to a direct execute for anything the cursor rejects (DML, DDL, a
    /// data-modifying CTE).
    fn run_paged(
        &mut self,
        statement: &str,
        batch_size: usize,
    ) -> Result<(QueryOutcome, bool), String> {
        if let Ok(select) = export::copyable(statement)
            && let Some(page) = self.try_cursor(select, batch_size)?
        {
            return Ok(page);
        }
        let results = self
            .client
            .simple_query(statement)
            .map_err(|e| describe(&e))?;
        Ok((collect_outcome(results), false))
    }

    /// Try to page `select` through a cursor. Returns `None` (the caller
    /// falls back to a plain execute) when `DECLARE CURSOR` is rejected —
    /// e.g. a data-modifying CTE. A savepoint keeps a rejected DECLARE from
    /// aborting an open user transaction. The flag says whether the cursor
    /// was left open.
    fn try_cursor(
        &mut self,
        select: &str,
        batch_size: usize,
    ) -> Result<Option<(QueryOutcome, bool)>, String> {
        let implicit = !self.in_txn && !self.implicit_txn;
        if implicit {
            self.client
                .batch_execute("BEGIN")
                .map_err(|e| describe(&e))?;
            self.implicit_txn = true;
        }
        // Inside a user transaction a failed DECLARE would abort it; guard
        // with a savepoint so the fallback path can still run.
        let savepoint = self.in_txn;
        if savepoint {
            self.client
                .batch_execute("SAVEPOINT _pg_gui_sp")
                .map_err(|e| describe(&e))?;
        }
        let declared = self.client.batch_execute(&format!(
            "DECLARE _pg_gui_results NO SCROLL CURSOR FOR {select}"
        ));
        if declared.is_err() {
            if implicit {
                let _ = self.client.batch_execute("ROLLBACK");
                self.implicit_txn = false;
            } else if savepoint {
                self.client
                    .batch_execute("ROLLBACK TO SAVEPOINT _pg_gui_sp")
                    .map_err(|e| describe(&e))?;
            }
            return Ok(None);
        }
        if savepoint {
            self.client
                .batch_execute("RELEASE SAVEPOINT _pg_gui_sp")
                .map_err(|e| describe(&e))?;
        }
        self.cursor_open = true;
        let (columns, rows) = self.fetch_batch(batch_size)?;
        let more = rows.len() == batch_size;
        let n = rows.len();
        if !more {
            self.end_cursor()?;
        }
        Ok(Some((
            QueryOutcome {
                columns,
                rows,
                messages: vec![format!("ok ({n} rows)")],
            },
            more,
        )))
    }

    /// Pull the next `batch_size` rows from the open cursor, closing it when
    /// exhausted. Returns the rows and whether more may remain.
    pub fn fetch_more(&mut self, batch_size: usize) -> Result<(Rows, bool), String> {
        if !self.cursor_open {
            return Ok((Vec::new(), false));
        }
        let (_, rows) = self.fetch_batch(batch_size)?;
        let more = rows.len() == batch_size;
        if !more {
            self.end_cursor()?;
        }
        Ok((rows, more))
    }

    fn fetch_batch(&mut self, batch_size: usize) -> Result<(Vec<String>, Rows), String> {
        let results = self
            .client
            .simple_query(&format!("FETCH FORWARD {batch_size} FROM _pg_gui_results"))
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

    /// Commit the open transaction (and any cursor within it).
    pub fn commit(&mut self) -> Result<(), String> {
        self.finish_txn("COMMIT")
    }

    /// Roll the open transaction back (and any cursor within it).
    pub fn rollback(&mut self) -> Result<(), String> {
        self.finish_txn("ROLLBACK")
    }

    fn finish_txn(&mut self, sql: &str) -> Result<(), String> {
        if self.in_txn || self.implicit_txn || self.cursor_open {
            self.client.batch_execute(sql).map_err(|e| describe(&e))?;
        }
        self.in_txn = false;
        self.implicit_txn = false;
        self.cursor_open = false;
        Ok(())
    }
}

/// Cancel the query running on the session the token came from, over a
/// side connection. Safe to call when nothing is running (a no-op server
/// side).
pub fn cancel(token: &postgres::CancelToken) -> Result<(), String> {
    token.cancel_query(NoTls).map_err(|e| describe(&e))
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

/// A trigger, as shown under a table's Triggers folder.
pub struct TriggerInfo {
    pub name: String,
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

/// One object a bare name in the editor could refer to: its kind (as the
/// browser's `relkind`-style tag), schema, and the name the definition
/// queries expect — a `name(identity arguments)` signature for a routine,
/// the plain name for a relation.
pub struct ObjectRef {
    pub kind: String,
    pub schema: String,
    pub object: String,
}

/// Resolve an identifier written in a script to the routine or relation it
/// names, for Go to Definition. `schema` is the qualifier the identifier
/// carried, if any; without one, `search_path` visibility decides between
/// same-named objects (`pg_*_is_visible`), so a call resolves the way the
/// server would resolve it.
///
/// Routines win over relations of the same name, matching what a click on a
/// call site means. An unquoted identifier is folded to lower case by the
/// server, so both the text as written and its lowercased form are tried.
pub fn find_object(
    conn_str: &str,
    schema: Option<&str>,
    name: &str,
) -> Result<Option<ObjectRef>, String> {
    let names = format!(
        "({}, {})",
        quote_literal(name),
        quote_literal(&name.to_lowercase())
    );
    let schema_filter = schema.map_or(String::new(), |schema| {
        format!(
            " AND n.nspname IN ({}, {})",
            quote_literal(schema),
            quote_literal(&schema.to_lowercase())
        )
    });
    let sql = format!(
        "SELECT kind, schema, object FROM ( \
           SELECT 'function' AS kind, n.nspname AS schema, \
                  p.proname || '(' \
                    || pg_catalog.pg_get_function_identity_arguments(p.oid) || ')' AS object, \
                  pg_catalog.pg_function_is_visible(p.oid) AS visible, 0 AS rank \
           FROM pg_catalog.pg_proc p \
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
           WHERE p.proname IN {names} \
             AND n.nspname NOT IN ('pg_catalog', 'information_schema'){schema_filter} \
           UNION ALL \
           SELECT CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'matview' ELSE 'table' END, \
                  n.nspname, c.relname, \
                  pg_catalog.pg_table_is_visible(c.oid), 1 \
           FROM pg_catalog.pg_class c \
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
           WHERE c.relname IN {names} \
             AND c.relkind IN ('r', 'p', 'v', 'm') \
             AND n.nspname NOT IN ('pg_catalog', 'information_schema'){schema_filter} \
         ) candidates \
         ORDER BY visible DESC, rank, schema, object \
         LIMIT 1"
    );
    Ok(catalog_rows(conn_str, &sql)?.first().and_then(|row| {
        Some(ObjectRef {
            kind: row.get(0)?.to_string(),
            schema: row.get(1)?.to_string(),
            object: row.get(2)?.to_string(),
        })
    }))
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

/// Triggers on `schema.relation`, ordered by name. Internal triggers (the
/// ones backing foreign-key constraints) are excluded, matching psql's `\d`.
pub fn list_triggers(
    conn_str: &str,
    schema: &str,
    relation: &str,
) -> Result<Vec<TriggerInfo>, String> {
    let sql = format!(
        "SELECT t.tgname \
         FROM pg_catalog.pg_trigger t \
         JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema} AND c.relname = {relation} AND NOT t.tgisinternal \
         ORDER BY t.tgname",
        schema = quote_literal(schema),
        relation = quote_literal(relation),
    );
    Ok(catalog_rows(conn_str, &sql)?
        .iter()
        .map(|row| TriggerInfo {
            name: row.get(0).unwrap_or_default().to_string(),
        })
        .collect())
}

/// `CREATE TRIGGER` text from `pg_get_triggerdef`.
pub fn trigger_definition(
    conn_str: &str,
    schema: &str,
    relation: &str,
    name: &str,
) -> Result<String, String> {
    let sql = format!(
        "SELECT pg_catalog.pg_get_triggerdef(t.oid, true) \
         FROM pg_catalog.pg_trigger t \
         JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema} AND c.relname = {relation} AND t.tgname = {name}",
        schema = quote_literal(schema),
        relation = quote_literal(relation),
        name = quote_literal(name),
    );
    let def = catalog_scalar(conn_str, &sql)?
        .ok_or_else(|| format!("trigger {name} on {schema}.{relation} not found"))?;
    Ok(format!("{def};"))
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

/// Best-effort `CREATE TABLE` DDL: columns (type, NOT NULL, DEFAULT), then
/// table constraints (`pg_get_constraintdef`), then any indexes that do not
/// back a constraint (`pg_get_indexdef`). Postgres has no single "get table
/// definition" function, so this is reconstructed from the catalog.
pub fn table_definition(conn_str: &str, schema: &str, name: &str) -> Result<String, String> {
    let oid = catalog_scalar(
        conn_str,
        &format!(
            "SELECT c.oid FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {schema} AND c.relname = {name} AND c.relkind IN ('r', 'p')",
            schema = quote_literal(schema),
            name = quote_literal(name),
        ),
    )?
    .ok_or_else(|| format!("table {schema}.{name} not found"))?;

    // Columns, in attribute order.
    let column_rows = catalog_rows(
        conn_str,
        &format!(
            "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, pg_catalog.pg_get_expr(d.adbin, d.adrelid) \
             FROM pg_catalog.pg_attribute a \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = {oid} AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum"
        ),
    )?;
    let mut items: Vec<String> = column_rows
        .iter()
        .map(|row| {
            let mut line = format!(
                "{} {}",
                quote_ident(row.get(0).unwrap_or_default()),
                row.get(1).unwrap_or_default(),
            );
            if is_true(row.get(2)) {
                line.push_str(" NOT NULL");
            }
            if let Some(default) = row.get(3) {
                let _ = write!(line, " DEFAULT {default}");
            }
            line
        })
        .collect();

    // Table constraints (primary key first, then the rest by name).
    let constraint_rows = catalog_rows(
        conn_str,
        &format!(
            "SELECT con.conname, pg_catalog.pg_get_constraintdef(con.oid) \
             FROM pg_catalog.pg_constraint con \
             WHERE con.conrelid = {oid} \
             ORDER BY con.contype = 'p' DESC, con.conname"
        ),
    )?;
    for row in &constraint_rows {
        items.push(format!(
            "CONSTRAINT {} {}",
            quote_ident(row.get(0).unwrap_or_default()),
            row.get(1).unwrap_or_default(),
        ));
    }

    let mut out = format!(
        "CREATE TABLE {}.{} (\n    {}\n);",
        quote_ident(schema),
        quote_ident(name),
        items.join(",\n    "),
    );

    // Indexes that are not the implementation of a constraint.
    let index_rows = catalog_rows(
        conn_str,
        &format!(
            "SELECT pg_catalog.pg_get_indexdef(ix.indexrelid) \
             FROM pg_catalog.pg_index ix \
             WHERE ix.indrelid = {oid} \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                               WHERE con.conindid = ix.indexrelid) \
             ORDER BY 1"
        ),
    )?;
    for row in &index_rows {
        if let Some(def) = row.get(0) {
            let _ = write!(out, "\n{def};");
        }
    }
    Ok(out)
}

/// Execute a SQL script (one or more statements) using the simple query protocol,
/// which returns every value as text and supports multi-statement scripts.
pub fn run_script(conn_str: &str, sql: &str) -> Result<QueryOutcome, String> {
    let mut client =
        connect(conn_str).map_err(|e| format!("connection failed: {}", describe(&e)))?;

    let results = client.simple_query(sql).map_err(|e| describe(&e))?;
    Ok(collect_outcome(results))
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
    use super::{
        LOG_SQL_LEN, ResultSet, RunEvent, Session, export_csv, export_inserts, one_line,
        search_path, statement_ranges,
    };

    const CONN: &str = "postgres://pgui:pgui@localhost:5433/pgui_test";

    /// What a run reported: the log lines and result sets it streamed, plus
    /// whether the cursor was left open. `Err` carries the streamed log lines
    /// alongside the error, as the UI sees them.
    #[derive(Debug)]
    struct Run {
        log: Vec<String>,
        sets: Vec<ResultSet>,
        more: bool,
    }

    impl Run {
        /// The rows of the run's last result set (what the results table
        /// shows once the run finishes).
        fn rows(&self) -> &super::Rows {
            &self.sets.last().expect("a result set").rows
        }
    }

    /// Run `sql`, draining the progress channel the way the UI does.
    fn run(
        session: &mut Session,
        sql: &str,
        batch_size: usize,
    ) -> Result<Run, (String, Vec<String>)> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let result = session.run(sql, batch_size, true, &tx);
        drop(tx);
        let mut log = Vec::new();
        let mut sets = Vec::new();
        for event in futures::executor::block_on_stream(rx) {
            match event {
                RunEvent::Log(line) => log.push(line),
                RunEvent::Result(set) => sets.push(set),
            }
        }
        match result {
            Ok(run) => Ok(Run {
                log,
                sets,
                more: run.more,
            }),
            Err(error) => Err((error, log)),
        }
    }

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
        let mut session = Session::connect(CONN).unwrap();
        let page = run(&mut session, "SELECT g FROM generate_series(1, 12) g", 5).unwrap();
        assert_eq!(page.sets[0].columns, vec!["g"]);
        assert_eq!(page.rows().len(), 5);
        assert_eq!(page.rows()[0][0].as_deref(), Some("1"));
        assert!(page.more);

        let (rows, more) = session.fetch_more(5).unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0][0].as_deref(), Some("6"));
        assert!(more);

        // The last, short batch exhausts the cursor.
        let (rows, more) = session.fetch_more(5).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0].as_deref(), Some("12"));
        assert!(!more);
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn cursor_exact_multiple_ends_with_empty_fetch() {
        let mut session = Session::connect(CONN).unwrap();
        let page = run(&mut session, "SELECT g FROM generate_series(1, 4) g", 4).unwrap();
        assert_eq!(page.rows().len(), 4);
        assert!(page.more);
        // A full first batch keeps the cursor open; the next fetch is empty.
        let (rows, more) = session.fetch_more(4).unwrap();
        assert!(rows.is_empty());
        assert!(!more);
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn cursor_falls_back_when_declare_rejects() {
        let mut session = Session::connect(CONN).unwrap();
        // SELECT INTO passes copyable's first-word check but DECLARE refuses
        // it; run falls back to a plain execute (no cursor left open).
        let page = run(&mut session, "SELECT 1 INTO TEMP _pg_gui_t", 10).unwrap();
        assert!(!page.more);
        // The temp table was created by the fallback execute on this session.
        let check = run(&mut session, "SELECT count(*) FROM _pg_gui_t", 10).unwrap();
        assert_eq!(check.rows()[0][0].as_deref(), Some("1"));
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
    fn one_line_collapses_whitespace_and_elides() {
        assert_eq!(
            one_line("SELECT 1,\n       2 -- note\n"),
            "SELECT 1, 2 -- note"
        );
        let long = one_line(&format!("SELECT '{}'", "x".repeat(400)));
        assert!(long.ends_with('…'), "{long}");
        assert_eq!(long.chars().count(), LOG_SQL_LEN + 1);
    }

    #[test]
    fn statement_ranges_drops_stray_semicolons() {
        let sql = "SELECT 1;;\n\n; SELECT 2;";
        let ranges = statement_ranges(sql);
        let statements: Vec<&str> = ranges.iter().map(|r| &sql[r.clone()]).collect();
        assert_eq!(statements, vec!["SELECT 1;", "SELECT 2;"]);
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn batch_logs_every_statement() {
        let mut session = Session::connect(CONN).unwrap();
        let batch = run(
            &mut session,
            "CREATE TEMP TABLE _pg_gui_batch (x int);
             INSERT INTO _pg_gui_batch VALUES (1), (2);
             SELECT x FROM _pg_gui_batch ORDER BY x;
             SELECT 'a' AS c;",
            10,
        )
        .unwrap();
        let log = &batch.log;
        assert_eq!(log.len(), 4, "{log:?}");
        assert!(
            log[0].starts_with("1. CREATE TEMP TABLE _pg_gui_batch (x int);"),
            "{log:?}"
        );
        assert!(log[1].contains("ok (2 rows)"), "{log:?}");
        assert!(
            log[2].starts_with("3. SELECT x FROM _pg_gui_batch"),
            "{log:?}"
        );
        // Each statement that returned rows keeps its own result set.
        assert_eq!(batch.sets.len(), 2, "{:?}", batch.sets);
        assert_eq!(batch.sets[0].statement, 3);
        assert_eq!(batch.sets[0].columns, vec!["x"]);
        assert_eq!(batch.sets[0].rows.len(), 2);
        assert_eq!(batch.sets[1].statement, 4);
        assert_eq!(batch.sets[1].rows[0][0].as_deref(), Some("a"));
        assert!(!batch.more);
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn batch_failure_keeps_the_log_and_rolls_back() {
        let mut session = Session::connect(CONN).unwrap();
        let (error, log) = run(
            &mut session,
            "CREATE TEMP TABLE _pg_gui_rollback (x int);
             SELECT no_such_function();",
            10,
        )
        .unwrap_err();
        // Both statements were reported before the run gave up.
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(log[1].contains("failed:"), "{log:?}");
        assert!(error.contains("no_such_function"), "{error}");
        // Autocommit wrapped the block in a transaction, so the table the
        // first statement created is gone again.
        let check = run(
            &mut session,
            "SELECT to_regclass('pg_temp._pg_gui_rollback') IS NULL",
            10,
        )
        .unwrap();
        assert_eq!(check.rows()[0][0].as_deref(), Some("t"));
    }

    #[test]
    #[ignore = "requires the docker compose database on localhost:5433"]
    fn export_csv_rejects_non_select() {
        let err = export_csv(CONN, "UPDATE t SET x = 1", &temp_path("reject.csv")).unwrap_err();
        assert!(err.contains("SELECT"), "{err}");
    }
}
