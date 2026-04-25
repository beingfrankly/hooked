//! Implementation of the `diff` subcommand.
//!
//! Mirrors Python `cmd_diff` in query.py.
//!
//! For each of two session prefixes, resolves the full session_id from the
//! `sessions` table, fetches all events, then computes aggregate stats:
//! tool_calls, failures, prompts, subagents, duration, top_tools, cwd, branch.
//! Renders a side-by-side comparison table with columns: metric, session_a, session_b.

use std::collections::HashMap;

use crate::cli::{DiffArgs, OutputFormat};
use crate::cmd::util::fmt_duration;
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct SessionStats {
    session_id: String,
    started_at: String,
    duration: String,
    tool_calls: i64,
    failures: i64,
    prompts: i64,
    subagents: i64,
    top_tools: String,
    cwd: String,
    branch: String,
}

/// Resolve a session ID prefix to a full session ID, then compute stats
/// from its events.
fn get_session_stats(conn: &Connection, prefix: &str) -> anyhow::Result<Option<SessionStats>> {
    // Mirror Python: resolve prefix via sessions table
    let session_id: Option<String> = {
        let mut stmt =
            conn.prepare("SELECT session_id FROM sessions WHERE session_id LIKE ?1 LIMIT 5")?;
        let mut rows = stmt.query(rusqlite::params![format!("{}%", prefix)])?;
        if let Some(row) = rows.next()? {
            Some(row.get(0)?)
        } else {
            None
        }
    };

    let session_id = match session_id {
        Some(sid) => sid,
        None => return Ok(None),
    };

    // Fetch events for this session
    let mut stmt = conn.prepare(
        "SELECT event_type, timestamp, tool_name, cwd, git_branch
         FROM events
         WHERE session_id = ?1
         ORDER BY sequence_num, timestamp",
    )?;

    struct EventMini {
        event_type: String,
        timestamp: String,
        tool_name: Option<String>,
        cwd: Option<String>,
        branch: Option<String>,
    }

    let events: Vec<EventMini> = stmt
        .query_map(rusqlite::params![session_id], |row| {
            Ok(EventMini {
                event_type: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                timestamp: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                tool_name: row.get(2)?,
                cwd: row.get(3)?,
                branch: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if events.is_empty() {
        return Ok(None);
    }

    // Mirror Python: aggregate event counts
    let tool_calls = events
        .iter()
        .filter(|e| e.event_type == "PreToolUse")
        .count() as i64;
    let failures = events
        .iter()
        .filter(|e| e.event_type == "PostToolUseFailure")
        .count() as i64;
    let prompts = events
        .iter()
        .filter(|e| e.event_type == "UserPromptSubmit")
        .count() as i64;
    let subagents = events
        .iter()
        .filter(|e| e.event_type == "SubagentStart")
        .count() as i64;

    // Duration: first event to last SessionEnd (or last event)
    let start_ts = &events[0].timestamp;
    let end_ts = events
        .iter()
        .rfind(|e| e.event_type == "SessionEnd")
        .or_else(|| events.last())
        .map(|e| e.timestamp.as_str())
        .unwrap_or("");

    let duration_ms = compute_duration_ms(start_ts, end_ts);

    // Top 5 tools by frequency
    let mut tool_freq: HashMap<String, i64> = HashMap::new();
    for ev in &events {
        if let Some(ref tn) = ev.tool_name
            && !tn.is_empty()
        {
            *tool_freq.entry(tn.clone()).or_insert(0) += 1;
        }
    }
    let mut tool_vec: Vec<(String, i64)> = tool_freq.into_iter().collect();
    tool_vec.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    let top_tools = tool_vec
        .iter()
        .take(5)
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(", ");

    let cwd = events[0].cwd.as_deref().unwrap_or("").to_string();
    let branch = events[0].branch.as_deref().unwrap_or("").to_string();

    // Mirror Python: session_id[:8], started_at[:19].replace("T", " ")
    let sid_short = if session_id.len() > 8 {
        session_id[..8].to_string()
    } else {
        session_id.clone()
    };
    let started_at = if start_ts.len() >= 19 {
        start_ts[..19].replace('T', " ")
    } else {
        start_ts.replace('T', " ")
    };

    Ok(Some(SessionStats {
        session_id: sid_short,
        started_at,
        duration: fmt_duration(duration_ms),
        tool_calls,
        failures,
        prompts,
        subagents,
        top_tools,
        cwd,
        branch,
    }))
}

fn compute_duration_ms(start_ts: &str, end_ts: &str) -> Option<i64> {
    if start_ts.is_empty() || end_ts.is_empty() {
        return None;
    }
    let t0 = chrono::DateTime::parse_from_rfc3339(&start_ts.replace('Z', "+00:00")).ok()?;
    let t1 = chrono::DateTime::parse_from_rfc3339(&end_ts.replace('Z', "+00:00")).ok()?;
    Some((t1 - t0).num_milliseconds())
}

fn build_table(stats_a: &SessionStats, stats_b: &SessionStats) -> Table {
    let headers = vec![
        "metric".to_string(),
        "session_a".to_string(),
        "session_b".to_string(),
    ];

    // Mirror Python: iterate over metric names in order
    let metric_names = [
        "session_id",
        "started_at",
        "duration",
        "tool_calls",
        "failures",
        "prompts",
        "subagents",
        "top_tools",
        "cwd",
        "branch",
    ];
    let values_a = stats_to_values(stats_a);
    let values_b = stats_to_values(stats_b);

    let data_rows: Vec<Row> = metric_names
        .iter()
        .zip(values_a.iter())
        .zip(values_b.iter())
        .map(|((name, val_a), val_b)| {
            vec![
                Cell::Str(name.to_string()),
                Cell::Str(val_a.clone()),
                Cell::Str(val_b.clone()),
            ]
        })
        .collect();

    Table::new(headers, data_rows)
}

/// Convert SessionStats to a vec of string values in metric_names order.
fn stats_to_values(s: &SessionStats) -> Vec<String> {
    vec![
        s.session_id.clone(),
        s.started_at.clone(),
        s.duration.clone(),
        s.tool_calls.to_string(),
        s.failures.to_string(),
        s.prompts.to_string(),
        s.subagents.to_string(),
        s.top_tools.clone(),
        s.cwd.clone(),
        s.branch.clone(),
    ]
}

pub fn diff(args: &DiffArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let _ = dbh::auto_ingest()?;
    let conn = dbh::open_db()?;

    let stats_a = get_session_stats(&conn, &args.session_a)?;
    let stats_b = get_session_stats(&conn, &args.session_b)?;

    match (stats_a, stats_b) {
        (None, None) => {
            eprintln!("Session not found: {}", args.session_a);
            eprintln!("Session not found: {}", args.session_b);
        }
        (None, Some(_)) => {
            eprintln!("Session not found: {}", args.session_a);
        }
        (Some(_), None) => {
            eprintln!("Session not found: {}", args.session_b);
        }
        (Some(a), Some(b)) => {
            let table = build_table(&a, &b);
            print!("{}", table.render(fmt));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
        timestamp: &str,
        tool_name: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp, tool_name)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, event_type, timestamp, tool_name],
        )
        .expect("insert event");
    }

    #[test]
    fn returns_none_for_unknown_prefix() {
        let conn = in_memory_conn();
        let result = get_session_stats(&conn, "nonexistent").expect("get_session_stats");
        assert!(result.is_none());
    }

    #[test]
    fn computes_stats_correctly() {
        let conn = in_memory_conn();
        let sid = "session-diff-test-001";
        insert_session(&conn, sid, "2024-01-15T10:00:00Z");
        insert_event(
            &conn,
            sid,
            "PreToolUse",
            "2024-01-15T10:00:01Z",
            Some("Read"),
        );
        insert_event(
            &conn,
            sid,
            "PreToolUse",
            "2024-01-15T10:00:02Z",
            Some("Write"),
        );
        insert_event(
            &conn,
            sid,
            "PostToolUseFailure",
            "2024-01-15T10:00:03Z",
            Some("Write"),
        );
        insert_event(&conn, sid, "UserPromptSubmit", "2024-01-15T10:00:04Z", None);

        let stats = get_session_stats(&conn, "session-diff").expect("get_session_stats");
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.tool_calls, 2);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.prompts, 1);
    }

    #[test]
    fn builds_comparison_table_with_expected_structure() {
        let conn = in_memory_conn();
        let sid_a = "session-a-001";
        let sid_b = "session-b-002";
        insert_session(&conn, sid_a, "2024-01-15T10:00:00Z");
        insert_session(&conn, sid_b, "2024-01-15T11:00:00Z");
        insert_event(
            &conn,
            sid_a,
            "PreToolUse",
            "2024-01-15T10:00:01Z",
            Some("Read"),
        );
        insert_event(
            &conn,
            sid_b,
            "PreToolUse",
            "2024-01-15T11:00:01Z",
            Some("Write"),
        );

        let stats_a = get_session_stats(&conn, "session-a").unwrap().unwrap();
        let stats_b = get_session_stats(&conn, "session-b").unwrap().unwrap();
        let table = build_table(&stats_a, &stats_b);

        assert_eq!(table.headers, vec!["metric", "session_a", "session_b"]);
        // Should have 10 rows (one per metric)
        assert_eq!(table.rows.len(), 10);
        // First row metric should be "session_id"
        assert_eq!(table.rows[0][0].display(), "session_id");
        // tool_calls row
        let tc_row = table.rows.iter().find(|r| r[0].display() == "tool_calls");
        assert!(tc_row.is_some());
        let tc_row = tc_row.unwrap();
        assert_eq!(tc_row[1].display(), "1"); // session_a has 1 tool call
        assert_eq!(tc_row[2].display(), "1"); // session_b has 1 tool call
    }
}
