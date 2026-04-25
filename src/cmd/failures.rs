//! Implementation of the `failures` subcommand.
//!
//! Mirrors Python `cmd_failures` in query.py.
//!
//! SQL: SELECT events where event_type = 'PostToolUseFailure' within the last N days,
//! ordered by timestamp DESC, limit 200 (after dedup with any live data).
//! Columns: timestamp, session_id, tool_name, agent_type, duration_ms, error.

use crate::cli::{FailuresArgs, OutputFormat};
use crate::cmd::util::truncate;
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct FailureRow {
    timestamp: String,
    session_id: String,
    tool_name: Option<String>,
    agent_type: Option<String>,
    duration_ms: Option<i64>,
    error: Option<String>,
}

fn run_query(conn: &Connection, days: u32) -> anyhow::Result<Vec<FailureRow>> {
    let since = {
        let d = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
        d.format("%Y-%m-%d").to_string()
    };

    let mut stmt = conn.prepare(
        "SELECT
            e.timestamp,
            e.session_id,
            e.tool_name,
            e.agent_type,
            e.duration_ms,
            e.error
        FROM events e
        WHERE e.event_type = 'PostToolUseFailure'
          AND date(e.timestamp) >= ?1
        ORDER BY e.timestamp DESC
        LIMIT 200",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![since], |row| {
            Ok(FailureRow {
                timestamp: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                session_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                tool_name: row.get(2)?,
                agent_type: row.get(3)?,
                duration_ms: row.get(4)?,
                error: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<FailureRow>) -> Table {
    // Python headers: ["timestamp", "session_id", "tool_name", "agent_type", "duration_ms", "error"]
    let headers = vec![
        "timestamp".to_string(),
        "session_id".to_string(),
        "tool_name".to_string(),
        "agent_type".to_string(),
        "duration_ms".to_string(),
        "error".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            // Mirror Python: session_id[:8], timestamp[:19].replace("T", " ")
            let sid = if r.session_id.len() > 8 {
                r.session_id[..8].to_string()
            } else {
                r.session_id.clone()
            };
            let ts = if r.timestamp.len() >= 19 {
                r.timestamp[..19].replace('T', " ")
            } else {
                r.timestamp.replace('T', " ")
            };
            vec![
                Cell::Str(ts),
                Cell::Str(sid),
                Cell::Str(r.tool_name.unwrap_or_default()),
                Cell::Str(r.agent_type.unwrap_or_default()),
                // Mirror Python: duration_ms is shown as raw value ("" if None)
                match r.duration_ms {
                    Some(ms) => Cell::Int(ms),
                    None => Cell::Str(String::new()),
                },
                Cell::Str(truncate(r.error.as_deref(), 80)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn failures(args: &FailuresArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let _ = dbh::auto_ingest()?;
    let conn = dbh::open_db()?;
    let rows = run_query(&conn, args.days)?;
    let table = build_table(rows);
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

    fn insert_failure(
        conn: &Connection,
        session_id: &str,
        timestamp: &str,
        tool_name: &str,
        error: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp, tool_name, error)
             VALUES (?1, 'PostToolUseFailure', ?2, ?3, ?4)",
            rusqlite::params![session_id, timestamp, tool_name, error],
        )
        .expect("insert failure event");
    }

    fn insert_non_failure(conn: &Connection, session_id: &str, timestamp: &str) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp)
             VALUES (?1, 'PreToolUse', ?2)",
            rusqlite::params![session_id, timestamp],
        )
        .expect("insert non-failure event");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_failure(
            &conn,
            "session-abc",
            "2024-01-15T10:00:00Z",
            "Read",
            Some("error msg"),
        );

        let rows = run_query(&conn, 9999).expect("run_query");
        let table = build_table(rows);
        assert_eq!(
            table.headers,
            vec![
                "timestamp",
                "session_id",
                "tool_name",
                "agent_type",
                "duration_ms",
                "error"
            ]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let rows = run_query(&conn, 7).expect("run_query");
        let table = build_table(rows);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn filters_only_failure_events() {
        let conn = in_memory_conn();
        insert_failure(&conn, "s1", "2024-01-15T10:00:00Z", "Read", Some("oops"));
        insert_failure(&conn, "s2", "2024-01-15T11:00:00Z", "Write", None);
        // Non-failure should be excluded
        insert_non_failure(&conn, "s3", "2024-01-15T12:00:00Z");

        let rows = run_query(&conn, 9999).expect("run_query");
        assert_eq!(rows.len(), 2);
        // Should be ordered DESC by timestamp, so s2 first
        assert!(rows[0].session_id.contains("s2"));
        assert!(rows[1].session_id.contains("s1"));
    }
}
