//! Implementation of the `tokens` subcommand.
//!
//! Mirrors Python `cmd_tokens` in query.py.

use crate::cli::{OutputFormat, TokensArgs};
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

struct TokenRow {
    session_id: String,
    started_at: Option<String>,
    context_total_bytes: Option<i64>,
    est_tokens: Option<i64>,
}

fn run_query(conn: &Connection, args: &TokensArgs) -> anyhow::Result<Vec<TokenRow>> {
    let rows = if let Some(prefix) = &args.session_id {
        let mut stmt = conn.prepare(
            "SELECT session_id, context_total_bytes,
                   CAST(context_total_bytes / 4 AS INTEGER) AS est_tokens,
                   started_at
            FROM sessions WHERE session_id LIKE ?1
            ORDER BY started_at DESC",
        )?;
        let pattern = format!("{}%", prefix);
        stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(TokenRow {
                session_id: row.get(0)?,
                context_total_bytes: row.get(1)?,
                est_tokens: row.get(2)?,
                started_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT session_id, context_total_bytes,
                   CAST(context_total_bytes / 4 AS INTEGER) AS est_tokens,
                   started_at
            FROM sessions
            WHERE context_total_bytes > 0
            ORDER BY context_total_bytes DESC
            LIMIT 20",
        )?;
        stmt.query_map([], |row| {
            Ok(TokenRow {
                session_id: row.get(0)?,
                context_total_bytes: row.get(1)?,
                est_tokens: row.get(2)?,
                started_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

fn build_table(rows: Vec<TokenRow>) -> Table {
    let headers = vec![
        "session_id".to_string(),
        "started_at".to_string(),
        "context_bytes".to_string(),
        "est_tokens".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            let session_id: String = r.session_id.chars().take(8).collect();
            let started_at = r
                .started_at
                .as_deref()
                .map(|s| s.chars().take(10).collect::<String>())
                .unwrap_or_default();
            // Python: f"{r.get('est_tokens') or 0:,}" — comma-separated thousands
            let est_tokens_str = format_with_commas(r.est_tokens.unwrap_or(0));
            vec![
                Cell::Str(session_id),
                Cell::Str(started_at),
                Cell::Str(fmt_bytes(r.context_total_bytes)),
                Cell::Str(est_tokens_str),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

/// Format an integer with comma separators, mirroring Python's `f"{n:,}"`.
fn format_with_commas(n: i64) -> String {
    let s = n.abs().to_string();
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::new();
    let len = chars.len();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(*c);
    }
    if n < 0 {
        format!("-{}", result)
    } else {
        result
    }
}

pub fn tokens(args: &TokensArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
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

    fn insert_session(
        conn: &Connection,
        session_id: &str,
        started_at: &str,
        context_total_bytes: i64,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at, context_total_bytes)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![session_id, started_at, context_total_bytes],
        )
        .expect("insert session");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-15T10:00:00Z", 40000);

        let args = TokensArgs { session_id: None };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);

        assert_eq!(
            table.headers,
            vec!["session_id", "started_at", "context_bytes", "est_tokens"]
        );
    }

    #[test]
    fn handles_empty_result() {
        let conn = in_memory_conn();
        let args = TokensArgs { session_id: None };
        let rows = run_query(&conn, &args).expect("run_query");
        let table = build_table(rows);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn respects_session_id_filter() {
        let conn = in_memory_conn();
        insert_session(&conn, "session-abc123", "2024-01-15T10:00:00Z", 80000);
        insert_session(&conn, "session-xyz999", "2024-01-16T10:00:00Z", 120000);

        let args = TokensArgs {
            session_id: Some("session-abc".to_string()),
        };
        let rows = run_query(&conn, &args).expect("run_query");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].session_id.starts_with("session-abc"));
    }

    #[test]
    fn est_tokens_is_bytes_div_4() {
        let conn = in_memory_conn();
        insert_session(&conn, "s1", "2024-01-15T10:00:00Z", 40000);

        let args = TokensArgs { session_id: None };
        let rows = run_query(&conn, &args).expect("run_query");
        assert_eq!(rows[0].est_tokens, Some(10000));
    }

    #[test]
    fn format_with_commas_works() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(1000), "1,000");
        assert_eq!(format_with_commas(1000000), "1,000,000");
        assert_eq!(format_with_commas(123), "123");
        assert_eq!(format_with_commas(1234), "1,234");
    }
}
