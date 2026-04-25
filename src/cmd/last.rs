//! Implementation of the `last` subcommand.
//!
//! Mirrors Python `cmd_last` in query.py.
//!
//! SQL: fetch the most recent session_id from sessions ORDER BY started_at DESC LIMIT 1,
//! then SELECT all events for that session ordered by sequence_num, timestamp.
//! Displays the same columns as `session`: seq, timestamp, event_type, tool_name,
//! duration, agent_type, skill_name, error.

use crate::cli::{LastArgs, OutputFormat};
use crate::cmd::util::{fmt_duration, truncate};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct LastEventRow {
    sequence_num: Option<i64>,
    timestamp: String,
    event_type: String,
    tool_name: Option<String>,
    duration_ms: Option<i64>,
    agent_type: Option<String>,
    skill_name: Option<String>,
    error: Option<String>,
    session_id: String,
}

fn run_query(conn: &Connection) -> anyhow::Result<Vec<LastEventRow>> {
    // Mirror Python: find most recent session from SQLite
    let latest_session_id: Option<String> = conn
        .query_row(
            "SELECT session_id FROM sessions ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    let session_id = match latest_session_id {
        Some(sid) => sid,
        None => return Ok(Vec::new()),
    };

    // Mirror Python: SELECT * FROM events WHERE session_id = ? ORDER BY sequence_num, timestamp
    let mut stmt = conn.prepare(
        "SELECT
             sequence_num, timestamp, event_type, tool_name, duration_ms,
             agent_type, skill_name, error, session_id
         FROM events
         WHERE session_id = ?1
         ORDER BY sequence_num, timestamp",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            Ok(LastEventRow {
                sequence_num: row.get(0)?,
                timestamp: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                event_type: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                tool_name: row.get(3)?,
                duration_ms: row.get(4)?,
                agent_type: row.get(5)?,
                skill_name: row.get(6)?,
                error: row.get(7)?,
                session_id: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<LastEventRow>) -> Table {
    let headers = vec![
        "seq".to_string(),
        "timestamp".to_string(),
        "event_type".to_string(),
        "tool_name".to_string(),
        "duration".to_string(),
        "agent_type".to_string(),
        "skill_name".to_string(),
        "error".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            let ts = if r.timestamp.len() >= 19 {
                r.timestamp[..19].replace('T', " ")
            } else {
                r.timestamp.replace('T', " ")
            };
            vec![
                match r.sequence_num {
                    Some(n) => Cell::Int(n),
                    None => Cell::Str(String::new()),
                },
                Cell::Str(ts),
                Cell::Str(r.event_type),
                Cell::Str(r.tool_name.unwrap_or_default()),
                Cell::Str(fmt_duration(r.duration_ms)),
                Cell::Str(r.agent_type.unwrap_or_default()),
                Cell::Str(r.skill_name.unwrap_or_default()),
                Cell::Str(truncate(r.error.as_deref(), 50)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn last(_args: &LastArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let _ = dbh::auto_ingest()?;
    let conn = dbh::open_db()?;
    let rows = run_query(&conn)?;
    if rows.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    let session_id = rows[0].session_id.clone();
    let event_count = rows.len();
    let table = build_table(rows);
    print!("{}", table.render(fmt));
    eprintln!("\nSession: {}", session_id);
    eprintln!("Events: {}", event_count);
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

    fn insert_session(conn: &Connection, session_id: &str, started_at: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at) VALUES (?1, ?2)",
            rusqlite::params![session_id, started_at],
        )
        .expect("insert session");
    }

    fn insert_event(
        conn: &Connection,
        session_id: &str,
        event_type: &str,
        seq: i64,
        tool_name: Option<&str>,
        duration_ms: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp, sequence_num, tool_name, duration_ms)
             VALUES (?1, ?2, '2024-01-15T10:00:01Z', ?3, ?4, ?5)",
            rusqlite::params![session_id, event_type, seq, tool_name, duration_ms],
        )
        .expect("insert event");
    }

    #[test]
    fn returns_empty_when_no_sessions() {
        let conn = in_memory_conn();
        let rows = run_query(&conn).expect("run_query");
        assert!(rows.is_empty());
    }

    #[test]
    fn returns_events_for_most_recent_session() {
        let conn = in_memory_conn();
        // Insert two sessions — s2 is newer
        insert_session(&conn, "s1", "2024-01-14T10:00:00Z");
        insert_session(&conn, "s2", "2024-01-15T10:00:00Z");
        insert_event(&conn, "s1", "PreToolUse", 1, Some("Read"), Some(100));
        insert_event(&conn, "s2", "SessionStart", 1, None, None);
        insert_event(&conn, "s2", "PreToolUse", 2, Some("Write"), Some(200));

        let rows = run_query(&conn).expect("run_query");
        // Should return events for s2 (most recent)
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "s2");
        assert_eq!(rows[0].event_type, "SessionStart");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-15T10:00:00Z");
        insert_event(&conn, "s1", "PreToolUse", 1, Some("Read"), Some(300));

        let rows = run_query(&conn).expect("run_query");
        let table = build_table(rows);
        assert_eq!(
            table.headers,
            vec![
                "seq",
                "timestamp",
                "event_type",
                "tool_name",
                "duration",
                "agent_type",
                "skill_name",
                "error"
            ]
        );
    }

    #[test]
    fn handles_empty_build_table() {
        let table = build_table(vec![]);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }
}
