//! Implementation of the `skills` subcommand.
//!
//! Mirrors Python `cmd_skills` in query.py.

use crate::cli::{OutputFormat, SkillsArgs};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

struct SkillRow {
    skill_name: String,
    skill_type: Option<String>,
    usage_count: i64,
    sessions_count: i64,
    agent_types: i64,
}

fn run_query(conn: &Connection, args: &SkillsArgs) -> anyhow::Result<Vec<SkillRow>> {
    let rows = if let Some(prefix) = &args.session_id {
        let mut stmt = conn.prepare(
            "SELECT
                e.skill_name,
                e.skill_type,
                COUNT(*) AS usage_count,
                COUNT(DISTINCT e.session_id) AS sessions_count,
                COUNT(DISTINCT e.agent_type) AS agent_types
            FROM events e
            WHERE e.skill_name IS NOT NULL
            AND e.session_id LIKE ?1
            GROUP BY e.skill_name, e.skill_type
            ORDER BY usage_count DESC",
        )?;
        let pattern = format!("{}%", prefix);
        stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(SkillRow {
                skill_name: row.get(0)?,
                skill_type: row.get(1)?,
                usage_count: row.get(2)?,
                sessions_count: row.get(3)?,
                agent_types: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT
                e.skill_name,
                e.skill_type,
                COUNT(*) AS usage_count,
                COUNT(DISTINCT e.session_id) AS sessions_count,
                COUNT(DISTINCT e.agent_type) AS agent_types
            FROM events e
            WHERE e.skill_name IS NOT NULL
            GROUP BY e.skill_name, e.skill_type
            ORDER BY usage_count DESC",
        )?;
        stmt.query_map([], |row| {
            Ok(SkillRow {
                skill_name: row.get(0)?,
                skill_type: row.get(1)?,
                usage_count: row.get(2)?,
                sessions_count: row.get(3)?,
                agent_types: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

fn build_table(rows: Vec<SkillRow>) -> Table {
    let headers = vec![
        "skill_name".to_string(),
        "skill_type".to_string(),
        "usage_count".to_string(),
        "sessions_count".to_string(),
        "agent_types".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            vec![
                Cell::Str(r.skill_name),
                r.skill_type.map(Cell::Str).unwrap_or(Cell::Null),
                Cell::Int(r.usage_count),
                Cell::Int(r.sessions_count),
                Cell::Int(r.agent_types),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn skills(args: &SkillsArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
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

    fn insert_event(
        conn: &Connection,
        session_id: &str,
        skill_name: &str,
        skill_type: Option<&str>,
        agent_type: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp, skill_name, skill_type, agent_type)
             VALUES (?1, 'PreToolUse', '2024-01-15T10:00:00Z', ?2, ?3, ?4)",
            rusqlite::params![session_id, skill_name, skill_type, agent_type],
        )
        .expect("insert event");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_event(&conn, "s1", "file-edit", Some("write"), Some("worker"));

        let args = SkillsArgs { session_id: None };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec![
                "skill_name",
                "skill_type",
                "usage_count",
                "sessions_count",
                "agent_types"
            ]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let args = SkillsArgs { session_id: None };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn respects_session_id_filter() {
        let conn = in_memory_conn();
        insert_event(&conn, "session-abc", "file-read", Some("read"), None);
        insert_event(&conn, "session-xyz", "file-write", Some("write"), None);

        let args = SkillsArgs {
            session_id: Some("session-abc".to_string()),
        };
        let rows = run_query(&conn, &args).expect("run_query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].skill_name, "file-read");
    }
}
