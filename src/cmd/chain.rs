//! Implementation of the `chain` subcommand.
//!
//! Mirrors Python `cmd_chain` in query.py.

use crate::cli::{ChainArgs, OutputFormat};
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

struct ChainSessionRow {
    session_id: String,
    started_at: Option<String>,
    ended_at: Option<String>,
    tool_calls: Option<i64>,
    failures: Option<i64>,
    prompts: Option<i64>,
    branch: Option<String>,
    cwd: Option<String>,
}

/// Resolve session prefix to chain_id, then fetch all sessions in the chain.
fn run_query(
    conn: &Connection,
    session_prefix: &str,
) -> anyhow::Result<(Option<String>, Vec<ChainSessionRow>)> {
    // Step 1: resolve prefix → session_id + chain_id
    // Mirrors Python: SELECT session_id, chain_id FROM sessions WHERE session_id LIKE ? LIMIT 5
    let (resolved_id, chain_id) = {
        let mut stmt = conn.prepare(
            "SELECT session_id, chain_id FROM sessions WHERE session_id LIKE ?1 LIMIT 5",
        )?;
        let mut rows = stmt.query(rusqlite::params![format!("{}%", session_prefix)])?;
        if let Some(row) = rows.next()? {
            let sid: String = row.get(0)?;
            let cid: Option<String> = row.get(1)?;
            (sid, cid)
        } else {
            return Ok((None, Vec::new()));
        }
    };

    let chain_id = match chain_id {
        Some(cid) => cid,
        None => {
            // session has no chain
            return Ok((Some(resolved_id), Vec::new()));
        }
    };

    // Step 2: fetch all sessions in the chain
    // Mirrors Python: SELECT ... FROM sessions WHERE chain_id = ? ORDER BY started_at
    let mut stmt = conn.prepare(
        "SELECT
             s.session_id,
             s.started_at,
             s.ended_at,
             s.total_tool_calls,
             s.total_failures,
             s.total_prompts,
             s.git_branch,
             s.cwd
         FROM sessions s
         WHERE s.chain_id = ?1
         ORDER BY s.started_at",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![chain_id], |row| {
            Ok(ChainSessionRow {
                session_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                tool_calls: row.get(3)?,
                failures: row.get(4)?,
                prompts: row.get(5)?,
                branch: row.get(6)?,
                cwd: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok((Some(chain_id), rows))
}

/// Build the Table from chain session rows.
/// Mirrors Python `cmd_chain` display logic.
fn build_table(rows: Vec<ChainSessionRow>) -> Table {
    let headers = vec![
        "session_id".to_string(),
        "started_at".to_string(),
        "duration".to_string(),
        "tool_calls".to_string(),
        "failures".to_string(),
        "prompts".to_string(),
        "branch".to_string(),
        "cwd".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            // Compute duration_ms from started_at and ended_at
            let duration_ms: Option<i64> = match (&r.started_at, &r.ended_at) {
                (Some(start), Some(end)) => parse_duration_ms(start, end),
                _ => None,
            };

            let session_id_short = r.session_id.chars().take(8).collect::<String>();
            let started_at = r
                .started_at
                .as_deref()
                .map(|s| {
                    let s = if s.len() >= 19 { &s[..19] } else { s };
                    s.replace('T', " ")
                })
                .unwrap_or_default();

            vec![
                Cell::Str(session_id_short),
                Cell::Str(started_at),
                Cell::Str(fmt_duration(duration_ms)),
                Cell::Int(r.tool_calls.unwrap_or(0)),
                Cell::Int(r.failures.unwrap_or(0)),
                Cell::Int(r.prompts.unwrap_or(0)),
                Cell::Str(r.branch.unwrap_or_default()),
                Cell::Str(truncate(r.cwd.as_deref(), 40)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

/// Parse ISO timestamp strings and compute duration in milliseconds.
fn parse_duration_ms(start: &str, end: &str) -> Option<i64> {
    // Parse as chrono DateTime<Utc> — handle both "Z" and "+00:00" suffixes
    let start_norm = start.replace('Z', "+00:00");
    let end_norm = end.replace('Z', "+00:00");
    let t0 = chrono::DateTime::parse_from_rfc3339(&start_norm).ok()?;
    let t1 = chrono::DateTime::parse_from_rfc3339(&end_norm).ok()?;
    let diff = t1.signed_duration_since(t0);
    Some(diff.num_milliseconds())
}

pub fn chain(args: &ChainArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // 1. Auto-ingest
    let _ = dbh::auto_ingest()?;
    // 2. Open DB
    let conn = dbh::open_db()?;
    // 3. Run query
    let (chain_id_or_sid, rows) = run_query(&conn, &args.session_prefix)?;

    match &chain_id_or_sid {
        None => {
            anyhow::bail!("No session matching prefix: {}", args.session_prefix);
        }
        Some(id) if rows.is_empty() => {
            // Session has no chain
            let short = id.chars().take(8).collect::<String>();
            eprintln!("Session {} has no chain.", short);
            return Ok(());
        }
        _ => {}
    }

    // Compute totals for footer
    let total_tool_calls: i64 = rows.iter().map(|r| r.tool_calls.unwrap_or(0)).sum();
    let total_failures: i64 = rows.iter().map(|r| r.failures.unwrap_or(0)).sum();
    let session_count = rows.len();

    // 4. Build Table
    let table = build_table(rows);
    // 5. Render
    print!("{}", table.render(fmt));
    // 6. Footer (mirrors Python: `if sys.stdout.isatty()`)
    if let Some(cid) = chain_id_or_sid {
        eprintln!("\nChain: {}", cid);
    }
    eprintln!(
        "Sessions: {}, Total tool calls: {}, Total failures: {}",
        session_count, total_tool_calls, total_failures
    );
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

    fn insert_session_with_chain(
        conn: &Connection,
        session_id: &str,
        chain_id: &str,
        started_at: &str,
        ended_at: Option<&str>,
        tool_calls: i64,
        failures: i64,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, chain_id, started_at, ended_at, total_tool_calls, total_failures)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![session_id, chain_id, started_at, ended_at, tool_calls, failures],
        )
        .expect("insert session");
    }

    #[test]
    fn finds_sessions_in_chain() {
        let conn = in_memory_conn();
        insert_session_with_chain(
            &conn,
            "session-aaa-001",
            "chain-xyz",
            "2024-01-15T10:00:00Z",
            Some("2024-01-15T10:30:00Z"),
            5,
            1,
        );
        insert_session_with_chain(
            &conn,
            "session-bbb-002",
            "chain-xyz",
            "2024-01-15T11:00:00Z",
            Some("2024-01-15T11:30:00Z"),
            3,
            0,
        );

        let (chain_id, rows) = run_query(&conn, "session-aaa").expect("run_query");
        assert!(chain_id.is_some());
        assert_eq!(rows.len(), 2);
        // Ordered by started_at ASC
        assert!(rows[0].session_id.starts_with("session-aaa"));
        assert!(rows[1].session_id.starts_with("session-bbb"));
    }

    #[test]
    fn returns_empty_for_unknown_prefix() {
        let conn = in_memory_conn();
        let (chain_id, rows) = run_query(&conn, "nonexistent").expect("run_query");
        assert!(chain_id.is_none());
        assert!(rows.is_empty());
    }

    #[test]
    fn returns_empty_for_session_without_chain() {
        let conn = in_memory_conn();
        conn.execute(
            "INSERT INTO sessions (session_id, started_at) VALUES ('no-chain-001', '2024-01-15T10:00:00Z')",
            [],
        ).expect("insert");
        let (id, rows) = run_query(&conn, "no-chain").expect("run_query");
        // session found but no chain
        assert!(id.is_some());
        assert!(rows.is_empty());
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let table = build_table(vec![]);
        assert_eq!(
            table.headers,
            vec![
                "session_id",
                "started_at",
                "duration",
                "tool_calls",
                "failures",
                "prompts",
                "branch",
                "cwd"
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
    fn parse_duration_ms_computes_correctly() {
        let ms = parse_duration_ms("2024-01-15T10:00:00Z", "2024-01-15T10:30:00Z");
        assert_eq!(ms, Some(30 * 60 * 1000)); // 30 minutes in ms
    }
}
