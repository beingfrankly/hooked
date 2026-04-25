//! Implementation of the `compactions` subcommand.
//!
//! Mirrors Python `cmd_compactions` in query.py.

use crate::cli::{CompactionsArgs, OutputFormat};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

/// Format bytes as a human-readable string.
/// Mirrors Python `_fmt_bytes`.
fn fmt_bytes(b: Option<i64>) -> String {
    match b {
        None => String::new(),
        Some(b) if b < 1024 => format!("{}B", b),
        Some(b) if b < 1024 * 1024 => format!("{:.1}K", b as f64 / 1024.0),
        Some(b) => format!("{:.1}M", b as f64 / (1024.0 * 1024.0)),
    }
}

struct CompactionRow {
    session_id: String,
    timestamp: String,
    compact_trigger: Option<String>,
    sequence_num: Option<i64>,
    git_branch: Option<String>,
    context_cumulative_bytes: Option<i64>,
}

fn run_query(conn: &Connection) -> anyhow::Result<Vec<CompactionRow>> {
    let mut stmt = conn.prepare(
        "SELECT
            e.session_id,
            e.timestamp,
            e.compact_trigger,
            e.sequence_num,
            e.git_branch,
            e.context_cumulative_bytes
        FROM events e
        WHERE e.event_type = 'PreCompact'
        ORDER BY e.timestamp DESC
        LIMIT 100",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(CompactionRow {
                session_id: row.get(0)?,
                timestamp: row.get(1)?,
                compact_trigger: row.get(2)?,
                sequence_num: row.get(3)?,
                git_branch: row.get(4)?,
                context_cumulative_bytes: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<CompactionRow>) -> Table {
    let headers = vec![
        "session_id".to_string(),
        "timestamp".to_string(),
        "trigger".to_string(),
        "seq".to_string(),
        "branch".to_string(),
        "context_bytes".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            let session_id: String = r.session_id.chars().take(8).collect();
            let timestamp = r
                .timestamp
                .chars()
                .take(19)
                .collect::<String>()
                .replace('T', " ");
            vec![
                Cell::Str(session_id),
                Cell::Str(timestamp),
                Cell::Str(r.compact_trigger.unwrap_or_default()),
                r.sequence_num
                    .map(Cell::Int)
                    .unwrap_or(Cell::Str(String::new())),
                Cell::Str(r.git_branch.unwrap_or_default()),
                Cell::Str(fmt_bytes(r.context_cumulative_bytes)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn compactions(_args: &CompactionsArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // 1. Auto-ingest
    let _ = dbh::auto_ingest()?;
    // 2. Open DB
    let conn = dbh::open_db()?;
    // 3. Run query
    let rows = run_query(&conn)?;
    // 4. Build Table
    let table = build_table(rows);
    // 5. Render
    print!("{}", table.render(fmt));
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::OutputFormat;
    use crate::schema::SCHEMA_V4_DDL;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema DDL");
        conn
    }

    fn insert_compact_event(
        conn: &Connection,
        session_id: &str,
        timestamp: &str,
        compact_trigger: Option<&str>,
        sequence_num: Option<i64>,
        git_branch: Option<&str>,
        context_bytes: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp, compact_trigger, sequence_num, git_branch, context_cumulative_bytes)
             VALUES (?1, 'PreCompact', ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                session_id,
                timestamp,
                compact_trigger,
                sequence_num,
                git_branch,
                context_bytes
            ],
        )
        .expect("insert event");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_compact_event(
            &conn,
            "s1",
            "2024-01-15T10:00:00Z",
            Some("auto"),
            Some(42),
            Some("main"),
            Some(512000),
        );

        let rows = run_query(&conn).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec![
                "session_id",
                "timestamp",
                "trigger",
                "seq",
                "branch",
                "context_bytes"
            ]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let rows = run_query(&conn).expect("run_query");
        let table = build_table(rows);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn only_returns_precompact_events() {
        let conn = in_memory_conn();
        // Insert a non-PreCompact event
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp)
             VALUES ('s1', 'SessionStart', '2024-01-15T10:00:00Z')",
            [],
        )
        .expect("insert event");
        // Insert a PreCompact event
        insert_compact_event(
            &conn,
            "s2",
            "2024-01-15T10:01:00Z",
            Some("manual"),
            Some(5),
            None,
            Some(200000),
        );

        let rows = run_query(&conn).expect("run_query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].compact_trigger.as_deref(), Some("manual"));
    }

    #[test]
    fn context_bytes_formatted_correctly() {
        assert_eq!(fmt_bytes(None), "");
        assert_eq!(fmt_bytes(Some(100)), "100B");
        assert_eq!(fmt_bytes(Some(1024)), "1.0K");
        assert_eq!(fmt_bytes(Some(2 * 1024 * 1024)), "2.0M");
    }
}
