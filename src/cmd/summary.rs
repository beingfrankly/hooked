//! Implementation of the `summary` subcommand.
//!
//! Mirrors Python `cmd_summary` in query.py.
//!
//! SQL: aggregate sessions table by date(started_at) for the last N days,
//! grouping on date to produce daily totals.

use crate::cli::{OutputFormat, SummaryArgs};
use crate::clock::Clock;
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct SummaryRow {
    date: String,
    sessions: i64,
    tool_calls: Option<i64>,
    failures: Option<i64>,
    prompts: Option<i64>,
    subagents: Option<i64>,
    config_versions: i64,
}

fn run_query(conn: &Connection, days: u32, clock: &dyn Clock) -> anyhow::Result<Vec<SummaryRow>> {
    // Python: since = (now - timedelta(days=days)).strftime("%Y-%m-%d")
    let since = {
        let d = clock.now_utc() - chrono::Duration::days(i64::from(days));
        d.format("%Y-%m-%d").to_string()
    };

    let mut stmt = conn.prepare(
        "SELECT
            date(s.started_at) AS date,
            COUNT(DISTINCT s.session_id) AS sessions,
            SUM(s.total_tool_calls) AS tool_calls,
            SUM(s.total_failures) AS failures,
            SUM(s.total_prompts) AS prompts,
            SUM(s.total_subagents) AS subagents,
            COUNT(DISTINCT s.config_version) AS config_versions
        FROM sessions s
        WHERE date(s.started_at) >= ?1
        GROUP BY date(s.started_at)
        ORDER BY date(s.started_at) DESC",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![since], |row| {
            Ok(SummaryRow {
                date: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                sessions: row.get(1)?,
                tool_calls: row.get(2)?,
                failures: row.get(3)?,
                prompts: row.get(4)?,
                subagents: row.get(5)?,
                config_versions: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<SummaryRow>) -> Table {
    let headers = vec![
        "date".to_string(),
        "sessions".to_string(),
        "tool_calls".to_string(),
        "failures".to_string(),
        "prompts".to_string(),
        "subagents".to_string(),
        "config_versions".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            vec![
                Cell::Str(r.date),
                Cell::Int(r.sessions),
                Cell::Int(r.tool_calls.unwrap_or(0)),
                Cell::Int(r.failures.unwrap_or(0)),
                Cell::Int(r.prompts.unwrap_or(0)),
                Cell::Int(r.subagents.unwrap_or(0)),
                Cell::Int(r.config_versions),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn summary(args: &SummaryArgs, fmt: &OutputFormat, clock: &dyn Clock) -> anyhow::Result<()> {
    let _ = dbh::auto_ingest()?;
    let conn = dbh::open_db()?;
    let rows = run_query(&conn, args.days, clock)?;
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
    use crate::clock::SystemClock;
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
        started_at: &str,
        tool_calls: i64,
        failures: i64,
        prompts: i64,
        config_version: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at, total_tool_calls, total_failures, total_prompts, config_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![session_id, started_at, tool_calls, failures, prompts, config_version],
        )
        .expect("insert session");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-15T10:00:00Z", 5, 1, 2, Some("v1"));

        let clock = SystemClock;
        let rows = run_query(&conn, 9999, &clock).expect("run_query");
        let table = build_table(rows);
        assert_eq!(
            table.headers,
            vec![
                "date",
                "sessions",
                "tool_calls",
                "failures",
                "prompts",
                "subagents",
                "config_versions"
            ]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let clock = SystemClock;
        let rows = run_query(&conn, 7, &clock).expect("run_query");
        let table = build_table(rows);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn aggregates_by_date() {
        let conn = in_memory_conn();
        // Two sessions on the same day
        insert_session(&conn, "s1", "2024-01-15T08:00:00Z", 3, 0, 1, Some("v1"));
        insert_session(&conn, "s2", "2024-01-15T14:00:00Z", 7, 2, 2, Some("v1"));
        // One session on a different day
        insert_session(&conn, "s3", "2024-01-16T09:00:00Z", 5, 1, 1, Some("v1"));

        let clock = SystemClock;
        let rows = run_query(&conn, 9999, &clock).expect("run_query");
        // Should be ordered DESC by date, so 2024-01-16 first
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].date, "2024-01-16");
        assert_eq!(rows[0].sessions, 1);
        assert_eq!(rows[1].date, "2024-01-15");
        assert_eq!(rows[1].sessions, 2);
        assert_eq!(rows[1].tool_calls.unwrap_or(0), 10); // 3 + 7
    }
}
