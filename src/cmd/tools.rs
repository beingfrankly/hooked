//! Implementation of the `tools` subcommand.
//!
//! Mirrors Python `cmd_tools` in query.py.

use crate::cli::{OutputFormat, ToolsArgs};
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

struct ToolRow {
    tool_name: String,
    count: i64,
    avg_duration_ms: f64,
    failure_rate: f64,
    total_bytes: Option<i64>,
}

fn run_query(conn: &Connection, args: &ToolsArgs) -> anyhow::Result<Vec<ToolRow>> {
    let rows = if args.all_time {
        // All config versions
        let mut stmt = conn.prepare(
            "SELECT
                tc.tool_name,
                COUNT(*) AS count,
                AVG(tc.duration_ms) AS avg_duration_ms,
                ROUND(100.0 * SUM(CASE WHEN tc.succeeded = 0 THEN 1 ELSE 0 END) / COUNT(*), 1) AS failure_rate,
                SUM(tc.output_bytes) AS total_bytes
            FROM tool_calls tc
            GROUP BY tc.tool_name
            ORDER BY count DESC",
        )?;
        stmt.query_map([], |row| {
            Ok(ToolRow {
                tool_name: row.get(0)?,
                count: row.get(1)?,
                avg_duration_ms: row.get::<_, f64>(2).unwrap_or(0.0),
                failure_rate: row.get::<_, f64>(3).unwrap_or(0.0),
                total_bytes: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        // Current config version only
        let cur_ver: Option<String> = conn
            .query_row(
                "SELECT config_version FROM sessions
                 WHERE config_version IS NOT NULL
                 ORDER BY started_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        if let Some(ver) = cur_ver {
            let mut stmt = conn.prepare(
                "SELECT
                    tc.tool_name,
                    COUNT(*) AS count,
                    AVG(tc.duration_ms) AS avg_duration_ms,
                    ROUND(100.0 * SUM(CASE WHEN tc.succeeded = 0 THEN 1 ELSE 0 END) / COUNT(*), 1) AS failure_rate,
                    SUM(tc.output_bytes) AS total_bytes
                FROM tool_calls tc
                JOIN sessions s ON tc.session_id = s.session_id WHERE s.config_version = ?1
                GROUP BY tc.tool_name
                ORDER BY count DESC",
            )?;
            stmt.query_map(rusqlite::params![ver], |row| {
                Ok(ToolRow {
                    tool_name: row.get(0)?,
                    count: row.get(1)?,
                    avg_duration_ms: row.get::<_, f64>(2).unwrap_or(0.0),
                    failure_rate: row.get::<_, f64>(3).unwrap_or(0.0),
                    total_bytes: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            // No config version found — fall back to all-time query
            let mut stmt = conn.prepare(
                "SELECT
                    tc.tool_name,
                    COUNT(*) AS count,
                    AVG(tc.duration_ms) AS avg_duration_ms,
                    ROUND(100.0 * SUM(CASE WHEN tc.succeeded = 0 THEN 1 ELSE 0 END) / COUNT(*), 1) AS failure_rate,
                    SUM(tc.output_bytes) AS total_bytes
                FROM tool_calls tc
                GROUP BY tc.tool_name
                ORDER BY count DESC",
            )?;
            stmt.query_map([], |row| {
                Ok(ToolRow {
                    tool_name: row.get(0)?,
                    count: row.get(1)?,
                    avg_duration_ms: row.get::<_, f64>(2).unwrap_or(0.0),
                    failure_rate: row.get::<_, f64>(3).unwrap_or(0.0),
                    total_bytes: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(rows)
}

fn build_table(rows: Vec<ToolRow>) -> Table {
    let headers = vec![
        "tool_name".to_string(),
        "count".to_string(),
        "avg_duration_ms".to_string(),
        "failure_rate".to_string(),
        "total_bytes".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            vec![
                Cell::Str(r.tool_name),
                Cell::Int(r.count),
                Cell::Str(format!("{:.0}ms", r.avg_duration_ms)),
                Cell::Str(format!("{:.1}%", r.failure_rate)),
                Cell::Str(fmt_bytes(r.total_bytes)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn tools(args: &ToolsArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
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
        tool_name: &str,
        duration_ms: Option<i64>,
        succeeded: i64,
        output_bytes: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO tool_calls (session_id, tool_use_id, tool_name, started_at, duration_ms, succeeded, output_bytes)
             VALUES (?1, ?2, ?3, '2024-01-15T10:00:00Z', ?4, ?5, ?6)",
            rusqlite::params![
                session_id,
                format!("tid-{}-{}", session_id, tool_name),
                tool_name,
                duration_ms,
                succeeded,
                output_bytes
            ],
        )
        .expect("insert tool_call");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_tool_call(&conn, "s1", "Read", Some(100), 1, Some(1024));
        insert_tool_call(&conn, "s1", "Write", Some(200), 1, Some(512));

        let args = ToolsArgs { all_time: true };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec![
                "tool_name",
                "count",
                "avg_duration_ms",
                "failure_rate",
                "total_bytes"
            ]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let args = ToolsArgs { all_time: true };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn respects_all_time_flag() {
        // With all_time=false and no config_version → falls back to all-time query
        let conn = in_memory_conn();
        insert_tool_call(&conn, "s1", "Read", Some(300), 1, None);

        let args = ToolsArgs { all_time: false };
        let rows = run_query(&conn, &args).expect("run_query");
        // No config_version in sessions table, so should fall back and return data
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tool_name, "Read");
    }

    #[test]
    fn fmt_bytes_formats_correctly() {
        assert_eq!(fmt_bytes(None), "");
        assert_eq!(fmt_bytes(Some(500)), "500B");
        assert_eq!(fmt_bytes(Some(2048)), "2.0K");
        assert_eq!(fmt_bytes(Some(2 * 1024 * 1024)), "2.0M");
    }
}
