//! Implementation of the `export` subcommand.
//!
//! Mirrors Python `cmd_export` in query.py:
//!
//! ```python
//! def cmd_export(args: argparse.Namespace) -> None:
//!     _auto_ingest()
//!     conn = _open_db()
//!
//!     from_date = getattr(args, "from_date", None)
//!     to_date = getattr(args, "to_date", None)
//!
//!     where_clauses = []
//!     params = []
//!     if from_date:
//!         where_clauses.append("date(s.started_at) >= ?")
//!         params.append(from_date)
//!     if to_date:
//!         where_clauses.append("date(s.started_at) <= ?")
//!         params.append(to_date)
//!
//!     where_sql = ("WHERE " + " AND ".join(where_clauses)) if where_clauses else ""
//!
//!     rows = _rows_as_dicts(conn, f"""
//!         SELECT s.* FROM sessions s {where_sql} ORDER BY s.started_at
//!     """, tuple(params))
//!     conn.close()
//!
//!     for row in rows:
//!         print(json.dumps(row, default=str))
//! ```
//!
//! Output: one JSON object per line (JSONL), to stdout.
//! Python uses `from_date` (not `from`) because `from` is a Python keyword.
//! Our CLI uses `from` as the field name; the clap argument is `--from`.

use crate::cli::ExportArgs;
use crate::dbh;
use rusqlite::Connection;

struct SessionExportRow {
    session_id: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    total_tool_calls: Option<i64>,
    total_failures: Option<i64>,
    total_prompts: Option<i64>,
    total_subagents: Option<i64>,
    cwd: Option<String>,
    git_branch: Option<String>,
    git_commit: Option<String>,
    chain_id: Option<String>,
    config_version: Option<String>,
    source: Option<String>,
    model: Option<String>,
    context_total_bytes: Option<i64>,
}

/// Run the sessions export query with optional date filters.
///
/// Mirrors Python: `SELECT s.* FROM sessions s [WHERE ...] ORDER BY s.started_at`
fn run_query(
    conn: &Connection,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> anyhow::Result<Vec<SessionExportRow>> {
    // Build WHERE clause dynamically
    let mut where_clauses: Vec<&str> = Vec::new();
    if from_date.is_some() {
        where_clauses.push("date(s.started_at) >= ?");
    }
    if to_date.is_some() {
        where_clauses.push("date(s.started_at) <= ?");
    }
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    let sql = format!(
        "SELECT
             s.session_id,
             s.started_at,
             s.ended_at,
             s.total_tool_calls,
             s.total_failures,
             s.total_prompts,
             s.total_subagents,
             s.cwd,
             s.git_branch,
             s.git_commit,
             s.chain_id,
             s.config_version,
             s.source,
             s.model,
             s.context_total_bytes
         FROM sessions s
         {}
         ORDER BY s.started_at",
        where_sql
    );

    let mut stmt = conn.prepare(&sql)?;

    // Build params list in the same order as the WHERE clause placeholders
    let mut bound_params: Vec<String> = Vec::new();
    if let Some(fd) = from_date {
        bound_params.push(fd.to_string());
    }
    if let Some(td) = to_date {
        bound_params.push(td.to_string());
    }

    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(bound_params.iter().map(|s| s.as_str())),
            |row| {
                Ok(SessionExportRow {
                    session_id: row.get(0)?,
                    started_at: row.get(1)?,
                    ended_at: row.get(2)?,
                    total_tool_calls: row.get(3)?,
                    total_failures: row.get(4)?,
                    total_prompts: row.get(5)?,
                    total_subagents: row.get(6)?,
                    cwd: row.get(7)?,
                    git_branch: row.get(8)?,
                    git_commit: row.get(9)?,
                    chain_id: row.get(10)?,
                    config_version: row.get(11)?,
                    source: row.get(12)?,
                    model: row.get(13)?,
                    context_total_bytes: row.get(14)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Convert an optional String to a serde_json::Value.
fn opt_str_val(v: &Option<String>) -> serde_json::Value {
    match v {
        Some(s) => serde_json::Value::String(s.clone()),
        None => serde_json::Value::Null,
    }
}

/// Convert an optional i64 to a serde_json::Value.
fn opt_int_val(v: Option<i64>) -> serde_json::Value {
    match v {
        Some(n) => serde_json::Value::Number(serde_json::Number::from(n)),
        None => serde_json::Value::Null,
    }
}

/// Convert a SessionExportRow to a serde_json::Value (object).
fn row_to_json(r: &SessionExportRow) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("session_id".to_string(), opt_str_val(&r.session_id));
    map.insert("started_at".to_string(), opt_str_val(&r.started_at));
    map.insert("ended_at".to_string(), opt_str_val(&r.ended_at));
    map.insert(
        "total_tool_calls".to_string(),
        opt_int_val(r.total_tool_calls),
    );
    map.insert("total_failures".to_string(), opt_int_val(r.total_failures));
    map.insert("total_prompts".to_string(), opt_int_val(r.total_prompts));
    map.insert(
        "total_subagents".to_string(),
        opt_int_val(r.total_subagents),
    );
    map.insert("cwd".to_string(), opt_str_val(&r.cwd));
    map.insert("git_branch".to_string(), opt_str_val(&r.git_branch));
    map.insert("git_commit".to_string(), opt_str_val(&r.git_commit));
    map.insert("chain_id".to_string(), opt_str_val(&r.chain_id));
    map.insert("config_version".to_string(), opt_str_val(&r.config_version));
    map.insert("source".to_string(), opt_str_val(&r.source));
    map.insert("model".to_string(), opt_str_val(&r.model));
    map.insert(
        "context_total_bytes".to_string(),
        opt_int_val(r.context_total_bytes),
    );
    serde_json::Value::Object(map)
}

pub fn export(args: &ExportArgs) -> anyhow::Result<()> {
    // 1. Auto-ingest
    let _ = dbh::auto_ingest()?;
    // 2. Open DB
    let conn = dbh::open_db()?;
    // 3. Run query
    let from_date = args.from.as_deref();
    let to_date = args.to.as_deref();
    let rows = run_query(&conn, from_date, to_date)?;
    // 4. Output one JSON object per line (JSONL)
    // Mirrors Python: for row in rows: print(json.dumps(row, default=str))
    for row in &rows {
        let json_val = row_to_json(row);
        println!("{}", serde_json::to_string(&json_val)?);
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

    #[test]
    fn exports_all_sessions_without_filter() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-10T10:00:00Z");
        insert_session(&conn, "s2", "2024-01-20T10:00:00Z");
        insert_session(&conn, "s3", "2024-02-01T10:00:00Z");

        let rows = run_query(&conn, None, None).expect("run_query");
        assert_eq!(rows.len(), 3);
        // Ordered by started_at ASC
        assert_eq!(rows[0].session_id.as_deref(), Some("s1"));
        assert_eq!(rows[2].session_id.as_deref(), Some("s3"));
    }

    #[test]
    fn respects_from_date_filter() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-10T10:00:00Z");
        insert_session(&conn, "s2", "2024-01-20T10:00:00Z");
        insert_session(&conn, "s3", "2024-02-01T10:00:00Z");

        let rows = run_query(&conn, Some("2024-01-15"), None).expect("run_query");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id.as_deref(), Some("s2"));
    }

    #[test]
    fn respects_to_date_filter() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-10T10:00:00Z");
        insert_session(&conn, "s2", "2024-01-20T10:00:00Z");
        insert_session(&conn, "s3", "2024-02-01T10:00:00Z");

        let rows = run_query(&conn, None, Some("2024-01-31")).expect("run_query");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].session_id.as_deref(), Some("s2"));
    }

    #[test]
    fn row_to_json_produces_correct_structure() {
        let row = SessionExportRow {
            session_id: Some("test-session".to_string()),
            started_at: Some("2024-01-15T10:00:00Z".to_string()),
            ended_at: None,
            total_tool_calls: Some(5),
            total_failures: Some(1),
            total_prompts: Some(3),
            total_subagents: None,
            cwd: Some("/home/user/project".to_string()),
            git_branch: Some("main".to_string()),
            git_commit: None,
            chain_id: None,
            config_version: None,
            source: None,
            model: None,
            context_total_bytes: None,
        };

        let val = row_to_json(&row);
        assert_eq!(
            val["session_id"],
            serde_json::Value::String("test-session".to_string())
        );
        assert_eq!(val["total_tool_calls"], serde_json::json!(5));
        assert_eq!(val["ended_at"], serde_json::Value::Null);
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let rows = run_query(&conn, None, None).expect("run_query");
        assert!(rows.is_empty());
    }
}
