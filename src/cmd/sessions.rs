//! Implementation of the `sessions` subcommand.
//!
//! Mirrors Python `cmd_sessions` in query.py.
//!
//! SQL: select sessions from the last N days with optional cwd/branch/label
//! filters; computes duration from started_at / ended_at; truncates cwd.

use rusqlite::Connection;
use rusqlite::types::Value;

use crate::cli::{OutputFormat, SessionsArgs};
use crate::cmd::util::{fmt_duration, truncate};
use crate::dbh;
use crate::render::{Cell, Row, Table};

struct SessionListRow {
    session_id: String,
    started_at: String,
    duration_ms: Option<i64>,
    tool_calls: Option<i64>,
    failures: Option<i64>,
    cwd: Option<String>,
    branch: Option<String>,
    chain_id: Option<String>,
    source: Option<String>,
}

fn run_query(conn: &Connection, args: &SessionsArgs) -> anyhow::Result<Vec<SessionListRow>> {
    let since = {
        let d = chrono::Utc::now() - chrono::Duration::days(i64::from(args.days));
        d.format("%Y-%m-%d").to_string()
    };

    // Build dynamic WHERE clauses mirroring Python's approach.
    // ?1 = since; additional params get indices ?2, ?3, ?4 in order.
    let mut where_clauses: Vec<String> = vec!["date(s.started_at) >= ?1".to_string()];
    let mut next_idx = 2usize;
    let mut extra_params: Vec<String> = Vec::new();

    if args.cwd.is_some() {
        where_clauses.push(format!("s.cwd LIKE ?{next_idx}"));
        next_idx += 1;
    }
    if args.branch.is_some() {
        where_clauses.push(format!("s.git_branch = ?{next_idx}"));
        next_idx += 1;
    }
    if args.label.is_some() {
        where_clauses.push(format!(
            "EXISTS (SELECT 1 FROM annotations a WHERE a.session_id = s.session_id AND a.label = ?{next_idx})"
        ));
        next_idx += 1;
    }
    // next_idx is only needed during construction; suppress the "assigned but not read" warning.
    let _ = next_idx;

    // Collect extra param values in the same order as the WHERE clauses above.
    if let Some(ref cwd) = args.cwd {
        extra_params.push(format!("%{cwd}%"));
    }
    if let Some(ref branch) = args.branch {
        extra_params.push(branch.clone());
    }
    if let Some(ref label) = args.label {
        extra_params.push(label.clone());
    }

    let where_sql = where_clauses.join(" AND ");

    let sql = format!(
        "SELECT
            s.session_id,
            s.started_at,
            s.ended_at,
            s.total_tool_calls AS tool_calls,
            s.total_failures AS failures,
            s.cwd,
            s.git_branch AS branch,
            s.chain_id,
            s.source
        FROM sessions s
        WHERE {where_sql}
        ORDER BY s.started_at DESC
        LIMIT 100"
    );

    let mut stmt = conn.prepare(&sql)?;

    // Build the full parameter list: ?1 = since, then extra_params in order.
    let mut params: Vec<Value> = vec![Value::Text(since)];
    for ep in &extra_params {
        params.push(Value::Text(ep.clone()));
    }

    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let started_at: Option<String> = row.get(1)?;
            let ended_at: Option<String> = row.get(2)?;
            let duration_ms = compute_duration_ms(started_at.as_deref(), ended_at.as_deref());
            Ok(SessionListRow {
                session_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                started_at: started_at.unwrap_or_default(),
                duration_ms,
                tool_calls: row.get(3)?,
                failures: row.get(4)?,
                cwd: row.get(5)?,
                branch: row.get(6)?,
                chain_id: row.get(7)?,
                source: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Compute duration in ms from two ISO-8601 timestamp strings.
fn compute_duration_ms(started: Option<&str>, ended: Option<&str>) -> Option<i64> {
    let s = started?;
    let e = ended?;
    let t0 = chrono::DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00")).ok()?;
    let t1 = chrono::DateTime::parse_from_rfc3339(&e.replace('Z', "+00:00")).ok()?;
    Some((t1 - t0).num_milliseconds())
}

fn build_table(rows: Vec<SessionListRow>, show_chain: bool) -> Table {
    let mut headers = vec![
        "session_id".to_string(),
        "started_at".to_string(),
        "duration".to_string(),
        "tool_calls".to_string(),
        "failures".to_string(),
        "cwd".to_string(),
        "branch".to_string(),
    ];
    if show_chain {
        headers.push("chain_id".to_string());
    }
    headers.push("source".to_string());

    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            // Mirror Python: session_id[:8], started_at[:19].replace("T", " ")
            let sid = if r.session_id.len() > 8 {
                r.session_id[..8].to_string()
            } else {
                r.session_id.clone()
            };
            let ts = if r.started_at.len() >= 19 {
                r.started_at[..19].replace('T', " ")
            } else {
                r.started_at.replace('T', " ")
            };
            let chain_short = r.chain_id.as_deref().map(|c| {
                if c.len() > 8 {
                    c[..8].to_string()
                } else {
                    c.to_string()
                }
            });

            let mut row = vec![
                Cell::Str(sid),
                Cell::Str(ts),
                Cell::Str(fmt_duration(r.duration_ms)),
                Cell::Int(r.tool_calls.unwrap_or(0)),
                Cell::Int(r.failures.unwrap_or(0)),
                Cell::Str(truncate(r.cwd.as_deref(), 40)),
                Cell::Str(r.branch.unwrap_or_default()),
            ];
            if show_chain {
                row.push(Cell::Str(chain_short.unwrap_or_default()));
            }
            row.push(Cell::Str(r.source.unwrap_or_default()));
            row
        })
        .collect();

    Table::new(headers, data_rows)
}

pub fn sessions(args: &SessionsArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let _ = dbh::auto_ingest()?;
    let conn = dbh::open_db()?;
    let rows = run_query(&conn, args)?;
    let table = build_table(rows, args.chain);
    print!("{}", table.render(fmt));
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, SessionsArgs};
    use crate::schema::SCHEMA_V4_DDL;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema DDL");
        conn
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_session(
        conn: &Connection,
        session_id: &str,
        started_at: &str,
        ended_at: Option<&str>,
        cwd: Option<&str>,
        branch: Option<&str>,
        tool_calls: i64,
        failures: i64,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at, ended_at, cwd, git_branch, total_tool_calls, total_failures)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![session_id, started_at, ended_at, cwd, branch, tool_calls, failures],
        )
        .expect("insert session");
    }

    fn default_args() -> SessionsArgs {
        SessionsArgs {
            cwd: None,
            branch: None,
            label: None,
            days: 9999,
            chain: false,
        }
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_session(
            &conn,
            "session-abc-001",
            "2024-01-15T10:00:00Z",
            Some("2024-01-15T10:30:00Z"),
            Some("/home/user/project"),
            Some("main"),
            5,
            0,
        );

        let args = default_args();
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows, false);
        assert_eq!(
            table.headers,
            vec![
                "session_id",
                "started_at",
                "duration",
                "tool_calls",
                "failures",
                "cwd",
                "branch",
                "source"
            ]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let args = default_args();
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows, false);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn filters_by_branch() {
        let conn = in_memory_conn();
        insert_session(
            &conn,
            "s1",
            "2024-01-15T10:00:00Z",
            None,
            None,
            Some("main"),
            3,
            0,
        );
        insert_session(
            &conn,
            "s2",
            "2024-01-15T11:00:00Z",
            None,
            None,
            Some("feature/xyz"),
            2,
            0,
        );

        let args = SessionsArgs {
            branch: Some("main".to_string()),
            ..default_args()
        };
        let rows = run_query(&conn, &args).expect("run_query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "s1");
    }

    #[test]
    fn chain_column_included_when_requested() {
        let conn = in_memory_conn();
        insert_session(
            &conn,
            "session-chain-001",
            "2024-01-15T10:00:00Z",
            None,
            None,
            None,
            1,
            0,
        );
        conn.execute(
            "UPDATE sessions SET chain_id = 'chain-abc-123' WHERE session_id = 'session-chain-001'",
            [],
        )
        .expect("update chain_id");

        let args = SessionsArgs {
            chain: true,
            ..default_args()
        };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows, true);
        assert!(table.headers.contains(&"chain_id".to_string()));
    }
}
