//! Implementation of the `slow` subcommand.
//!
//! Mirrors Python `cmd_slow` in query.py.

use crate::cli::{OutputFormat, SlowArgs};
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

struct SlowRow {
    session_id: Option<String>,
    tool_name: String,
    agent_type: Option<String>,
    duration_ms: Option<i64>,
    started_at: Option<String>,
    error: Option<String>,
    input_summary: Option<String>,
}

fn run_query(conn: &Connection, args: &SlowArgs) -> anyhow::Result<Vec<SlowRow>> {
    let rows = if let Some(tool_filter) = &args.tool {
        let mut stmt = conn.prepare(
            "SELECT
                tc.session_id,
                tc.tool_name,
                tc.agent_type,
                tc.duration_ms,
                tc.started_at,
                tc.error,
                tc.input_summary
            FROM tool_calls tc
            WHERE tc.duration_ms > ?1 AND tc.tool_name = ?2
            ORDER BY tc.duration_ms DESC
            LIMIT 50",
        )?;
        stmt.query_map(
            rusqlite::params![args.threshold as i64, tool_filter],
            |row| {
                Ok(SlowRow {
                    session_id: row.get(0)?,
                    tool_name: row.get(1)?,
                    agent_type: row.get(2)?,
                    duration_ms: row.get(3)?,
                    started_at: row.get(4)?,
                    error: row.get(5)?,
                    input_summary: row.get(6)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT
                tc.session_id,
                tc.tool_name,
                tc.agent_type,
                tc.duration_ms,
                tc.started_at,
                tc.error,
                tc.input_summary
            FROM tool_calls tc
            WHERE tc.duration_ms > ?1
            ORDER BY tc.duration_ms DESC
            LIMIT 50",
        )?;
        stmt.query_map(rusqlite::params![args.threshold as i64], |row| {
            Ok(SlowRow {
                session_id: row.get(0)?,
                tool_name: row.get(1)?,
                agent_type: row.get(2)?,
                duration_ms: row.get(3)?,
                started_at: row.get(4)?,
                error: row.get(5)?,
                input_summary: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

fn build_table(rows: Vec<SlowRow>) -> Table {
    let headers = vec![
        "session_id".to_string(),
        "tool_name".to_string(),
        "agent_type".to_string(),
        "duration".to_string(),
        "started_at".to_string(),
        "error".to_string(),
        "input".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            let session_id = r
                .session_id
                .as_deref()
                .map(|s| s.chars().take(8).collect::<String>())
                .unwrap_or_default();
            let started_at = r
                .started_at
                .as_deref()
                .map(|s| s.chars().take(19).collect::<String>().replace('T', " "))
                .unwrap_or_default();
            vec![
                Cell::Str(session_id),
                Cell::Str(r.tool_name),
                Cell::Str(r.agent_type.unwrap_or_default()),
                Cell::Str(fmt_duration(r.duration_ms)),
                Cell::Str(started_at),
                Cell::Str(truncate(r.error.as_deref(), 50)),
                Cell::Str(truncate(r.input_summary.as_deref(), 50)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn slow(args: &SlowArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // 1. Auto-ingest
    let _ = dbh::auto_ingest()?;
    // 2. Open DB
    let conn = dbh::open_db()?;
    // 3. Run query
    let rows = run_query(&conn, args)?;
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

    fn insert_tool_call(
        conn: &Connection,
        session_id: &str,
        tool_use_id: &str,
        tool_name: &str,
        duration_ms: i64,
    ) {
        conn.execute(
            "INSERT INTO tool_calls (session_id, tool_use_id, tool_name, started_at, duration_ms)
             VALUES (?1, ?2, ?3, '2024-01-15T10:00:00Z', ?4)",
            rusqlite::params![session_id, tool_use_id, tool_name, duration_ms],
        )
        .expect("insert tool_call");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_tool_call(&conn, "s1", "tid1", "Read", 6000);

        let args = SlowArgs {
            threshold: 5000,
            tool: None,
        };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec![
                "session_id",
                "tool_name",
                "agent_type",
                "duration",
                "started_at",
                "error",
                "input"
            ]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let args = SlowArgs {
            threshold: 5000,
            tool: None,
        };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn respects_threshold_argument() {
        let conn = in_memory_conn();
        insert_tool_call(&conn, "s1", "tid1", "Read", 3000);
        insert_tool_call(&conn, "s1", "tid2", "Write", 7000);
        insert_tool_call(&conn, "s1", "tid3", "Bash", 12000);

        let args = SlowArgs {
            threshold: 5000,
            tool: None,
        };
        let rows = run_query(&conn, &args).expect("run_query");
        // Only those with duration_ms > 5000
        assert_eq!(rows.len(), 2);
        // Ordered DESC by duration
        assert_eq!(rows[0].duration_ms, Some(12000));
        assert_eq!(rows[1].duration_ms, Some(7000));
    }

    #[test]
    fn respects_tool_filter() {
        let conn = in_memory_conn();
        insert_tool_call(&conn, "s1", "tid1", "Read", 8000);
        insert_tool_call(&conn, "s1", "tid2", "Write", 9000);

        let args = SlowArgs {
            threshold: 5000,
            tool: Some("Read".to_string()),
        };
        let rows = run_query(&conn, &args).expect("run_query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tool_name, "Read");
    }

    #[test]
    fn fmt_duration_formats_correctly() {
        assert_eq!(fmt_duration(None), "");
        assert_eq!(fmt_duration(Some(500)), "500ms");
        assert_eq!(fmt_duration(Some(1500)), "1.5s");
        assert_eq!(fmt_duration(Some(65000)), "1m5s");
    }
}
