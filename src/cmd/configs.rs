//! Implementation of the `configs` subcommand.
//!
//! Mirrors Python `cmd_configs` in query.py.

use crate::cli::{ConfigsArgs, OutputFormat};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct ConfigRow {
    config_version: String,
    label: Option<String>,
    sessions: i64,
    avg_tool_calls: f64,
    avg_failures: f64,
    first_seen: Option<String>,
    last_seen: Option<String>,
}

fn run_query(conn: &Connection) -> anyhow::Result<Vec<ConfigRow>> {
    let mut stmt = conn.prepare(
        "SELECT
            s.config_version,
            cv.description AS label,
            COUNT(DISTINCT s.session_id) AS sessions,
            AVG(s.total_tool_calls) AS avg_tool_calls,
            AVG(s.total_failures) AS avg_failures,
            MIN(s.started_at) AS first_seen,
            MAX(s.started_at) AS last_seen
        FROM sessions s
        LEFT JOIN config_versions cv ON cv.version_hash = s.config_version
        WHERE s.config_version IS NOT NULL
        GROUP BY s.config_version
        ORDER BY last_seen DESC",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(ConfigRow {
                config_version: row.get(0)?,
                label: row.get(1)?,
                sessions: row.get(2)?,
                avg_tool_calls: row.get::<_, f64>(3).unwrap_or(0.0),
                avg_failures: row.get::<_, f64>(4).unwrap_or(0.0),
                first_seen: row.get(5)?,
                last_seen: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<ConfigRow>) -> Table {
    let headers = vec![
        "version_hash".to_string(),
        "label".to_string(),
        "sessions".to_string(),
        "avg_tool_calls".to_string(),
        "avg_failures".to_string(),
        "first_seen".to_string(),
        "last_seen".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            // Truncate dates to first 10 chars (YYYY-MM-DD)
            let first_seen = r
                .first_seen
                .as_deref()
                .map(|s| s.chars().take(10).collect::<String>())
                .unwrap_or_default();
            let last_seen = r
                .last_seen
                .as_deref()
                .map(|s| s.chars().take(10).collect::<String>())
                .unwrap_or_default();
            vec![
                Cell::Str(r.config_version),
                Cell::Str(r.label.unwrap_or_default()),
                Cell::Int(r.sessions),
                Cell::Str(format!("{:.1}", r.avg_tool_calls)),
                Cell::Str(format!("{:.1}", r.avg_failures)),
                Cell::Str(first_seen),
                Cell::Str(last_seen),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn configs(_args: &ConfigsArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
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

    fn insert_session(
        conn: &Connection,
        session_id: &str,
        config_version: &str,
        started_at: &str,
        total_tool_calls: i64,
        total_failures: i64,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, config_version, started_at, total_tool_calls, total_failures)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                session_id,
                config_version,
                started_at,
                total_tool_calls,
                total_failures
            ],
        )
        .expect("insert session");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "v1hash", "2024-01-15T10:00:00Z", 10, 1);

        let rows = run_query(&conn).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec![
                "version_hash",
                "label",
                "sessions",
                "avg_tool_calls",
                "avg_failures",
                "first_seen",
                "last_seen"
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
    fn groups_by_config_version() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "v1hash", "2024-01-10T10:00:00Z", 5, 0);
        insert_session(&conn, "s2", "v1hash", "2024-01-15T10:00:00Z", 15, 2);
        insert_session(&conn, "s3", "v2hash", "2024-01-20T10:00:00Z", 8, 1);

        let rows = run_query(&conn).expect("run_query");
        // v2hash should be first (last_seen DESC)
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].config_version, "v2hash");
        assert_eq!(rows[1].config_version, "v1hash");
        assert_eq!(rows[1].sessions, 2);
        // avg_tool_calls for v1hash = (5+15)/2 = 10
        assert!((rows[1].avg_tool_calls - 10.0).abs() < 0.1);
    }

    #[test]
    fn first_and_last_seen_truncated_to_date() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "v1hash", "2024-01-10T10:00:00Z", 5, 0);
        insert_session(&conn, "s2", "v1hash", "2024-01-20T10:00:00Z", 5, 0);

        let rows = run_query(&conn).expect("run_query");
        let table = build_table(rows);
        // first_seen row value should be "2024-01-10" (10 chars)
        let first_seen_val = table.rows[0][5].display();
        assert_eq!(first_seen_val, "2024-01-10");
        let last_seen_val = table.rows[0][6].display();
        assert_eq!(last_seen_val, "2024-01-20");
    }
}
