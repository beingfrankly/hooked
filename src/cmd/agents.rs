//! Implementation of the `agents` subcommand.
//!
//! Mirrors Python `cmd_agents` in query.py.

use crate::cli::{AgentsArgs, OutputFormat};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct AgentRow {
    agent_type: String,
    instance_count: i64,
    avg_tool_calls: f64,
    failure_rate: f64,
    avg_duration_ms: f64,
}

fn run_query(conn: &Connection) -> anyhow::Result<Vec<AgentRow>> {
    let mut stmt = conn.prepare(
        "SELECT
            tc.agent_type,
            COUNT(DISTINCT tc.agent_id) AS instance_count,
            AVG(tc_counts.tc_per_agent) AS avg_tool_calls,
            ROUND(100.0 * SUM(CASE WHEN tc.succeeded = 0 THEN 1 ELSE 0 END) / COUNT(*), 1) AS failure_rate,
            AVG(tc.duration_ms) AS avg_duration_ms
        FROM tool_calls tc
        LEFT JOIN (
            SELECT agent_id, COUNT(*) AS tc_per_agent
            FROM tool_calls
            WHERE agent_id IS NOT NULL
            GROUP BY agent_id
        ) tc_counts ON tc.agent_id = tc_counts.agent_id
        WHERE tc.agent_type IS NOT NULL
        GROUP BY tc.agent_type
        ORDER BY instance_count DESC",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(AgentRow {
                agent_type: row.get(0)?,
                instance_count: row.get(1)?,
                avg_tool_calls: row.get::<_, f64>(2).unwrap_or(0.0),
                failure_rate: row.get::<_, f64>(3).unwrap_or(0.0),
                avg_duration_ms: row.get::<_, f64>(4).unwrap_or(0.0),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn build_table(rows: Vec<AgentRow>) -> Table {
    let headers = vec![
        "agent_type".to_string(),
        "instance_count".to_string(),
        "avg_tool_calls".to_string(),
        "failure_rate".to_string(),
        "avg_duration_ms".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            vec![
                Cell::Str(r.agent_type),
                Cell::Int(r.instance_count),
                Cell::Str(format!("{:.1}", r.avg_tool_calls)),
                Cell::Str(format!("{:.1}%", r.failure_rate)),
                Cell::Str(format!("{:.0}ms", r.avg_duration_ms)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn agents(_args: &AgentsArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
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

    fn insert_tool_call(
        conn: &Connection,
        session_id: &str,
        tool_use_id: &str,
        agent_id: &str,
        agent_type: &str,
        succeeded: i64,
        duration_ms: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO tool_calls (session_id, tool_use_id, tool_name, agent_id, agent_type, started_at, duration_ms, succeeded)
             VALUES (?1, ?2, 'Read', ?3, ?4, '2024-01-15T10:00:00Z', ?5, ?6)",
            rusqlite::params![session_id, tool_use_id, agent_id, agent_type, duration_ms, succeeded],
        )
        .expect("insert tool_call");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_tool_call(&conn, "s1", "tid1", "agent-1", "build-runner", 1, Some(100));

        let rows = run_query(&conn).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec![
                "agent_type",
                "instance_count",
                "avg_tool_calls",
                "failure_rate",
                "avg_duration_ms"
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
    fn counts_instances_correctly() {
        let conn = in_memory_conn();
        // Two distinct agent_ids with agent_type "explore"
        insert_tool_call(&conn, "s1", "tid1", "agent-a", "explore", 1, Some(200));
        insert_tool_call(&conn, "s1", "tid2", "agent-b", "explore", 1, Some(400));
        // One agent_id with agent_type "build-runner"
        insert_tool_call(&conn, "s1", "tid3", "agent-c", "build-runner", 0, Some(100));

        let rows = run_query(&conn).expect("run_query");
        // explore should come first (2 instances > 1)
        assert_eq!(rows[0].agent_type, "explore");
        assert_eq!(rows[0].instance_count, 2);
        assert_eq!(rows[1].agent_type, "build-runner");
        assert_eq!(rows[1].instance_count, 1);
        // build-runner has 1 failure out of 1 = 100%
        assert_eq!(rows[1].failure_rate, 100.0);
    }
}
