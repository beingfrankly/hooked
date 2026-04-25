//! Implementation of the `before-stop` subcommand.
//!
//! Mirrors Python `cmd_before_stop` in query.py.

use crate::cli::{BeforeStopArgs, OutputFormat};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct BeforeStopRow {
    session_id: String,
    timestamp: String,
    m5: Option<String>,
    m4: Option<String>,
    m3: Option<String>,
    m2: Option<String>,
    m1: Option<String>,
}

fn run_query(conn: &Connection) -> anyhow::Result<Vec<BeforeStopRow>> {
    let mut stmt = conn.prepare(
        "WITH ordered AS (
            SELECT
                e.session_id,
                e.event_type,
                e.tool_name,
                e.timestamp,
                e.sequence_num,
                LAG(e.event_type, 1) OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_1_type,
                LAG(e.tool_name, 1)  OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_1_tool,
                LAG(e.event_type, 2) OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_2_type,
                LAG(e.tool_name, 2)  OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_2_tool,
                LAG(e.event_type, 3) OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_3_type,
                LAG(e.tool_name, 3)  OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_3_tool,
                LAG(e.event_type, 4) OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_4_type,
                LAG(e.tool_name, 4)  OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_4_tool,
                LAG(e.event_type, 5) OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_5_type,
                LAG(e.tool_name, 5)  OVER (PARTITION BY e.session_id ORDER BY e.sequence_num) AS prev_5_tool
            FROM events e
        )
        SELECT
            session_id,
            timestamp,
            prev_5_type || '/' || COALESCE(prev_5_tool, '') AS m5,
            prev_4_type || '/' || COALESCE(prev_4_tool, '') AS m4,
            prev_3_type || '/' || COALESCE(prev_3_tool, '') AS m3,
            prev_2_type || '/' || COALESCE(prev_2_tool, '') AS m2,
            prev_1_type || '/' || COALESCE(prev_1_tool, '') AS m1
        FROM ordered
        WHERE event_type = 'Stop'
        ORDER BY timestamp DESC
        LIMIT 50",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(BeforeStopRow {
                session_id: row.get(0)?,
                timestamp: row.get(1)?,
                m5: row.get(2)?,
                m4: row.get(3)?,
                m3: row.get(4)?,
                m2: row.get(5)?,
                m1: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<BeforeStopRow>) -> Table {
    let headers = vec![
        "session_id".to_string(),
        "timestamp".to_string(),
        "-5".to_string(),
        "-4".to_string(),
        "-3".to_string(),
        "-2".to_string(),
        "-1".to_string(),
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
                Cell::Str(r.m5.unwrap_or_default()),
                Cell::Str(r.m4.unwrap_or_default()),
                Cell::Str(r.m3.unwrap_or_default()),
                Cell::Str(r.m2.unwrap_or_default()),
                Cell::Str(r.m1.unwrap_or_default()),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn before_stop(_args: &BeforeStopArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
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

    fn insert_event(
        conn: &Connection,
        session_id: &str,
        event_type: &str,
        tool_name: Option<&str>,
        timestamp: &str,
        sequence_num: i64,
    ) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, tool_name, timestamp, sequence_num)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![session_id, event_type, tool_name, timestamp, sequence_num],
        )
        .expect("insert event");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_event(
            &conn,
            "s1",
            "PreToolUse",
            Some("Read"),
            "2024-01-15T10:00:01Z",
            1,
        );
        insert_event(&conn, "s1", "Stop", None, "2024-01-15T10:00:02Z", 2);

        let rows = run_query(&conn).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec!["session_id", "timestamp", "-5", "-4", "-3", "-2", "-1"]
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
    fn captures_events_before_stop() {
        let conn = in_memory_conn();
        // Insert a sequence of events ending in Stop
        insert_event(
            &conn,
            "sess-abc",
            "SessionStart",
            None,
            "2024-01-15T10:00:00Z",
            1,
        );
        insert_event(
            &conn,
            "sess-abc",
            "PreToolUse",
            Some("Read"),
            "2024-01-15T10:00:01Z",
            2,
        );
        insert_event(
            &conn,
            "sess-abc",
            "PostToolUse",
            Some("Read"),
            "2024-01-15T10:00:02Z",
            3,
        );
        insert_event(&conn, "sess-abc", "Stop", None, "2024-01-15T10:00:03Z", 4);

        let rows = run_query(&conn).expect("run_query");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].session_id.starts_with("sess-abc"));
        // m1 should be PostToolUse/Read (1 before Stop)
        let m1 = rows[0].m1.as_deref().unwrap_or("");
        assert!(
            m1.contains("PostToolUse"),
            "m1 should contain PostToolUse, got: {}",
            m1
        );
    }
}
