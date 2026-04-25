//! Implementation of the `sql` subcommand.
//!
//! Mirrors Python `cmd_sql` in query.py (lines 1772–1805).
//!
//! Runs an arbitrary SQL query against the SQLite database.  By default the
//! connection is read-only by convention (mutation guard); mutations require
//! `--write`.
//!
//! ## Mutation guard
//!
//! Python's exact mutation check (lines 1785–1786):
//!
//! ```python
//! stripped = query.strip().upper()
//! is_mutation = any(stripped.startswith(kw) for kw in
//!     ("INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "REPLACE"))
//! ```
//!
//! This is a simple `startswith` check on the uppercased, stripped query —
//! NOT a regex.  We replicate it 1:1 as documented in plan risk #4.
//! The heuristic is intentionally brittle: CTEs, comments, and leading
//! parentheses will defeat it.
//!
//! ## Column polymorphism
//!
//! Column names are read from `Statement::column_names()` after `prepare()`.
//! Cell values are read with dynamic type dispatch via `rusqlite`'s
//! `types::ValueRef` so that INTEGER, REAL, TEXT, BLOB, and NULL all render
//! correctly.

use anyhow::Context as _;
use rusqlite::types::ValueRef;

use crate::cli::{OutputFormat, SqlArgs};
use crate::dbh::{auto_ingest, open_db};
use crate::render::{Cell, Row, Table};

// ---------------------------------------------------------------------------
// Mutation guard
// ---------------------------------------------------------------------------

/// The ordered list of SQL keyword prefixes that indicate a mutation.
///
/// Python verbatim (line 1786):
/// ```python
/// is_mutation = any(stripped.startswith(kw) for kw in
///     ("INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "REPLACE"))
/// ```
///
/// NOTE: this is intentionally a brittle heuristic (plan risk #4).  It is a
/// 1:1 port of Python's `startswith` check.
const MUTATION_PREFIXES: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "REPLACE",
];

/// Returns `true` if `query` (after stripping leading/trailing whitespace and
/// upper-casing) starts with any of the [`MUTATION_PREFIXES`].
///
/// Mirrors Python:
/// ```python
/// stripped = query.strip().upper()
/// is_mutation = any(stripped.startswith(kw) for kw in (...))
/// ```
fn is_mutation(query: &str) -> bool {
    let stripped = query.trim().to_uppercase();
    MUTATION_PREFIXES.iter().any(|kw| stripped.starts_with(kw))
}

// ---------------------------------------------------------------------------
// Cell conversion
// ---------------------------------------------------------------------------

/// Convert a [`ValueRef`] obtained from a result row into a [`Cell`] for rendering.
///
/// Blob values are rendered as `<N bytes>` — matches the task spec for
/// `cell_for_value`.
fn cell_for_value_ref(v: ValueRef<'_>) -> Cell {
    match v {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(i) => Cell::Int(i),
        ValueRef::Real(f) => Cell::Float(f),
        ValueRef::Text(s) => Cell::Str(String::from_utf8_lossy(s).into_owned()),
        ValueRef::Blob(b) => Cell::Str(format!("<{} bytes>", b.len())),
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn sql(args: &SqlArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // Mutation guard — reject write queries unless --write is set.
    // Mirror Python: checked before opening connection.
    if is_mutation(&args.query) && !args.write {
        anyhow::bail!("Error: mutation detected. Use --write to allow mutations.");
    }

    // Auto-ingest stale files — mirrors Python `_auto_ingest()` at top of cmd_sql.
    let _ = auto_ingest();

    // Open DB.
    let conn = open_db().context("failed to open database")?;

    // Mutation + --write: execute without fetching rows, then print rowcount.
    if is_mutation(&args.query) {
        let rowcount = conn
            .execute(&args.query, [])
            .with_context(|| format!("SQL error: {}", args.query))?;
        println!("Rows affected: {}", rowcount);
        return Ok(());
    }

    // SELECT or non-mutation: prepare, collect column names, stream rows.
    let mut stmt = conn
        .prepare(&args.query)
        .with_context(|| format!("failed to prepare SQL: {}", args.query))?;

    let headers: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let col_count = headers.len();

    let data_rows: Vec<Row> = stmt
        .query_map([], |row| {
            let mut cells = Vec::with_capacity(col_count);
            for i in 0..col_count {
                cells.push(cell_for_value_ref(row.get_ref(i)?));
            }
            Ok(cells)
        })
        .with_context(|| format!("SQL error executing: {}", args.query))?
        .collect::<Result<Vec<_>, _>>()
        .context("error reading SQL result rows")?;

    let table = Table::new(headers, data_rows);
    print!("{}", table.render(fmt));

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use crate::cli::OutputFormat;
    use crate::render::Cell;
    use crate::schema::SCHEMA_V4_DDL;

    use super::{cell_for_value_ref, is_mutation};
    use rusqlite::types::ValueRef;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema");
        conn
    }

    fn insert_event(conn: &Connection, session_id: &str, event_type: &str) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp)
             VALUES (?1, ?2, '2024-01-15T10:00:00Z')",
            rusqlite::params![session_id, event_type],
        )
        .expect("insert event");
    }

    // Run the core query+render logic against an in-memory connection.
    // This avoids the `dbh::open_db()` filesystem dependency in unit tests.
    fn run_on_conn(
        conn: &Connection,
        query: &str,
        allow_write: bool,
        fmt: &OutputFormat,
    ) -> anyhow::Result<String> {
        if is_mutation(query) && !allow_write {
            anyhow::bail!("Error: mutation detected. Use --write to allow mutations.");
        }

        if is_mutation(query) {
            // Mutation + write: execute and return rowcount message.
            let n = conn
                .execute(query, [])
                .map_err(|e| anyhow::anyhow!("SQL error: {}", e))?;
            return Ok(format!("Rows affected: {}", n));
        }

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| anyhow::anyhow!("SQL error: {}", e))?;

        let headers: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let col_count = headers.len();

        let rows: Vec<crate::render::Row> = stmt
            .query_map([], |row| {
                let mut cells = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    cells.push(cell_for_value_ref(row.get_ref(i)?));
                }
                Ok(cells)
            })
            .map_err(|e| anyhow::anyhow!("SQL error: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("row error: {}", e))?;

        let table = crate::render::Table::new(headers, rows);
        Ok(table.render(fmt))
    }

    // -----------------------------------------------------------------------
    // is_mutation unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn mutation_guard_detects_insert() {
        assert!(is_mutation("INSERT INTO events VALUES (1)"));
        assert!(is_mutation("  insert into events values (1)")); // leading ws + lowercase
    }

    #[test]
    fn mutation_guard_detects_all_keywords() {
        for kw in &["UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "REPLACE"] {
            assert!(is_mutation(&format!("{} TABLE foo", kw)), "keyword: {kw}");
        }
    }

    #[test]
    fn mutation_guard_allows_select() {
        assert!(!is_mutation("SELECT * FROM events"));
        assert!(!is_mutation("  SELECT 1"));
        assert!(!is_mutation("WITH cte AS (SELECT 1) SELECT * FROM cte"));
    }

    // -----------------------------------------------------------------------
    // sql_select_returns_rows
    // -----------------------------------------------------------------------

    #[test]
    fn sql_select_returns_rows() {
        let conn = in_memory_db();
        insert_event(&conn, "session-aaa", "SessionStart");
        insert_event(&conn, "session-bbb", "PreToolUse");

        let out = run_on_conn(
            &conn,
            "SELECT session_id, event_type FROM events ORDER BY session_id",
            false,
            &OutputFormat::Table,
        )
        .expect("query should succeed");

        assert!(
            out.contains("session-aaa"),
            "should contain first session_id"
        );
        assert!(
            out.contains("session-bbb"),
            "should contain second session_id"
        );
        assert!(
            out.contains("SessionStart"),
            "should contain first event_type"
        );
        assert!(
            out.contains("PreToolUse"),
            "should contain second event_type"
        );

        // header + separator + 2 data rows = 4 lines
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "expected header + sep + 2 data rows");
    }

    // -----------------------------------------------------------------------
    // sql_mutation_blocked_by_default
    // -----------------------------------------------------------------------

    #[test]
    fn sql_mutation_blocked_by_default() {
        let conn = in_memory_db();
        let result = run_on_conn(
            &conn,
            "INSERT INTO events (session_id, event_type, timestamp) VALUES ('x', 'y', 'z')",
            false, // allow_write = false
            &OutputFormat::Table,
        );

        assert!(result.is_err(), "INSERT without --write should return Err");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mutation detected"),
            "error should mention mutation; got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // sql_mutation_allowed_with_flag
    // -----------------------------------------------------------------------

    #[test]
    fn sql_mutation_allowed_with_flag() {
        let conn = in_memory_db();

        let result = run_on_conn(
            &conn,
            "INSERT INTO events (session_id, event_type, timestamp) \
             VALUES ('s1', 'SessionStart', '2024-01-15T10:00:00Z')",
            true, // allow_write = true
            &OutputFormat::Table,
        );

        assert!(result.is_ok(), "INSERT with --write should succeed");

        // Verify the row was actually inserted.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .expect("count query");
        assert_eq!(count, 1, "one row should have been inserted");
    }

    // -----------------------------------------------------------------------
    // sql_dynamic_column_types
    // -----------------------------------------------------------------------

    #[test]
    fn sql_dynamic_column_types() {
        let conn = in_memory_db();
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp, sequence_num) \
             VALUES ('s1', 'PreToolUse', '2024-01-15T10:00:00Z', 42)",
            [],
        )
        .expect("insert");

        let out = run_on_conn(
            &conn,
            "SELECT event_type, sequence_num FROM events",
            false,
            &OutputFormat::Table,
        )
        .expect("query");

        assert!(out.contains("PreToolUse"), "TEXT column should render");
        assert!(out.contains("42"), "INTEGER column should render");
    }

    // -----------------------------------------------------------------------
    // sql_null_values
    // -----------------------------------------------------------------------

    #[test]
    fn sql_null_values() {
        let conn = in_memory_db();
        // Insert event without tool_name (NULL).
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp) \
             VALUES ('s1', 'SessionStart', '2024-01-15T10:00:00Z')",
            [],
        )
        .expect("insert");

        let out = run_on_conn(
            &conn,
            "SELECT session_id, tool_name FROM events",
            false,
            &OutputFormat::Table,
        )
        .expect("query");

        // NULL renders as empty string in Table format — not "None", not "null".
        assert!(out.contains("s1"), "session_id should appear");
        assert!(out.contains("tool_name"), "header should be present");
        assert!(!out.contains("None"), "NULL must not render as 'None'");
        assert!(!out.contains("null"), "NULL must not render as 'null'");
    }

    // -----------------------------------------------------------------------
    // cell_for_value_ref — blob renders as "<N bytes>"
    // -----------------------------------------------------------------------

    #[test]
    fn blob_renders_as_byte_count() {
        let blob: &[u8] = &[0u8, 1, 2, 3, 4];
        let cell = cell_for_value_ref(ValueRef::Blob(blob));
        assert!(
            matches!(&cell, Cell::Str(s) if s == "<5 bytes>"),
            "blob should render as '<5 bytes>', got: {:?}",
            cell
        );
    }
}
