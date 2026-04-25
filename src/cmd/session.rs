//! Implementation of the `session` subcommand.
//!
//! Mirrors Python `cmd_session` in query.py.

use crate::cli::{OutputFormat, SessionArgs};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

/// Format duration in ms as human-readable string.
/// Mirrors Python `_fmt_duration`.
fn fmt_duration(ms: Option<i64>) -> String {
    match ms {
        None => String::new(),
        Some(ms) if ms < 1000 => format!("{}ms", ms),
        Some(ms) if ms < 60000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => format!("{}m{}s", ms / 60000, (ms % 60000) / 1000),
    }
}

/// Truncate string to n chars with ellipsis.
/// Mirrors Python `_truncate`.
fn truncate(s: Option<&str>, n: usize) -> String {
    match s {
        None => String::new(),
        Some(s) if s.chars().count() > n => {
            let truncated: String = s.chars().take(n).collect();
            format!("{}…", truncated)
        }
        Some(s) => s.to_string(),
    }
}

struct SessionEventRow {
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

/// Resolve session_id prefix to a full session_id, then fetch all events.
fn run_query(conn: &Connection, id_prefix: &str) -> anyhow::Result<Vec<SessionEventRow>> {
    // Mirror Python: SELECT session_id FROM sessions WHERE session_id LIKE ? LIMIT 5
    let session_id: Option<String> = {
        let mut stmt =
            conn.prepare("SELECT session_id FROM sessions WHERE session_id LIKE ?1 LIMIT 5")?;
        let mut rows = stmt.query(rusqlite::params![format!("{}%", id_prefix)])?;
        if let Some(row) = rows.next()? {
            Some(row.get(0)?)
        } else {
            None
        }
    };

    let session_id = match session_id {
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
            Ok(SessionEventRow {
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

/// Build the Table from event rows.
/// Mirrors Python's `cmd_session` display logic.
fn build_table(rows: Vec<SessionEventRow>) -> Table {
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

pub fn session(args: &SessionArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // 1. Auto-ingest
    let _ = dbh::auto_ingest()?;
    // 2. Open DB
    let conn = dbh::open_db()?;
    // 3. Run query
    let rows = run_query(&conn, &args.id_prefix)?;
    if rows.is_empty() {
        anyhow::bail!("No session found matching prefix: {}", args.id_prefix);
    }
    // 4. Summary info
    let session_id = rows[0].session_id.clone();
    let event_count = rows.len();
    // 5. Build Table
    let table = build_table(rows);
    // 6. Render
    print!("{}", table.render(fmt));
    // 7. Footer (mirrors Python: `if sys.stdout.isatty()`)
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

    fn insert_session(conn: &Connection, session_id: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at) VALUES (?1, '2024-01-15T10:00:00Z')",
            rusqlite::params![session_id],
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
    fn finds_events_by_prefix() {
        let conn = in_memory_conn();
        let full_id = "abc123def456789012345678";
        insert_session(&conn, full_id);
        insert_event(&conn, full_id, "SessionStart", 1, None, None);
        insert_event(&conn, full_id, "PreToolUse", 2, Some("Read"), Some(500));

        let rows = run_query(&conn, "abc123").expect("run_query");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].event_type, "SessionStart");
        assert_eq!(rows[1].event_type, "PreToolUse");
        assert_eq!(rows[1].tool_name.as_deref(), Some("Read"));
    }

    #[test]
    fn returns_empty_for_unknown_prefix() {
        let conn = in_memory_conn();
        let rows = run_query(&conn, "nonexistent").expect("run_query");
        assert!(rows.is_empty());
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        let full_id = "session-full-001";
        insert_session(&conn, full_id);
        insert_event(&conn, full_id, "PreToolUse", 1, Some("Write"), Some(200));

        let rows = run_query(&conn, "session-full").expect("run_query");
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
    fn handles_empty_result() {
        let table = build_table(vec![]);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn fmt_duration_formats_correctly() {
        assert_eq!(fmt_duration(None), "");
        assert_eq!(fmt_duration(Some(500)), "500ms");
        assert_eq!(fmt_duration(Some(1500)), "1.5s");
        assert_eq!(fmt_duration(Some(65000)), "1m5s");
    }
}
