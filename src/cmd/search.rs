//! Implementation of the `search` subcommand.
//!
//! Mirrors Python `cmd_search` in query.py.
//!
//! Uses FTS5 via `events_fts` virtual table for full-text search.

use crate::cli::{OutputFormat, SearchArgs};
use crate::dbh;
use crate::render::{Cell, Row, Table};
use rusqlite::Connection;

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

struct SearchRow {
    session_id: Option<String>,
    timestamp: Option<String>,
    event_type: Option<String>,
    tool_name: Option<String>,
    prompt_text: Option<String>,
    error: Option<String>,
    tool_input: Option<String>,
}

/// Run FTS5 search query.
///
/// Mirrors Python:
/// ```python
/// SELECT e.session_id, e.timestamp, e.event_type, e.tool_name,
///        e.prompt_text, e.error, e.tool_input
/// FROM events e
/// JOIN events_fts ON events_fts.rowid = e.id
/// WHERE events_fts MATCH ?
/// ORDER BY rank
/// LIMIT 50
/// ```
fn run_query(conn: &Connection, query: &str) -> anyhow::Result<Vec<SearchRow>> {
    let mut stmt = conn.prepare(
        "SELECT
             e.session_id,
             e.timestamp,
             e.event_type,
             e.tool_name,
             e.prompt_text,
             e.error,
             e.tool_input
         FROM events e
         JOIN events_fts ON events_fts.rowid = e.id
         WHERE events_fts MATCH ?1
         ORDER BY rank
         LIMIT 50",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![query], |row| {
            Ok(SearchRow {
                session_id: row.get(0)?,
                timestamp: row.get(1)?,
                event_type: row.get(2)?,
                tool_name: row.get(3)?,
                prompt_text: row.get(4)?,
                error: row.get(5)?,
                tool_input: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Build the Table from search result rows.
/// Mirrors Python `cmd_search` display logic.
fn build_table(rows: Vec<SearchRow>) -> Table {
    let headers = vec![
        "session_id".to_string(),
        "timestamp".to_string(),
        "event_type".to_string(),
        "tool_name".to_string(),
        "context".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            let session_id_short = r
                .session_id
                .as_deref()
                .map(|s| s.chars().take(8).collect::<String>())
                .unwrap_or_default();
            let ts = r
                .timestamp
                .as_deref()
                .map(|s| {
                    let s = if s.len() >= 19 { &s[..19] } else { s };
                    s.replace('T', " ")
                })
                .unwrap_or_default();

            // Show the most relevant text field — mirrors Python:
            // context = r.get("prompt_text") or r.get("error") or r.get("tool_input") or ""
            let context = r
                .prompt_text
                .as_deref()
                .filter(|s| !s.is_empty())
                .or_else(|| r.error.as_deref().filter(|s| !s.is_empty()))
                .or_else(|| r.tool_input.as_deref().filter(|s| !s.is_empty()));

            vec![
                Cell::Str(session_id_short),
                Cell::Str(ts),
                Cell::Str(r.event_type.unwrap_or_default()),
                Cell::Str(r.tool_name.unwrap_or_default()),
                Cell::Str(truncate(context, 100)),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn search(args: &SearchArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // 1. Auto-ingest
    let _ = dbh::auto_ingest()?;
    // 2. Open DB
    let conn = dbh::open_db()?;
    // 3. Run FTS5 query — mirrors Python's try/except for OperationalError
    let rows = match run_query(&conn, &args.query) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("Search error: {}", e);
            eprintln!("Hint: FTS5 index may need rebuilding. Run: hooked ingest");
            return Ok(());
        }
    };
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

    /// Insert an event and sync it into the FTS index.
    ///
    /// events_fts is a content=events FTS5 table. For content tables, external
    /// content must be kept in sync manually.  We insert the event row first,
    /// then insert into events_fts using the same rowid.
    fn insert_event_with_prompt(
        conn: &Connection,
        session_id: &str,
        event_type: &str,
        prompt_text: Option<&str>,
        tool_input: Option<&str>,
        error: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp, prompt_text, tool_input, error)
             VALUES (?1, ?2, '2024-01-15T10:00:00Z', ?3, ?4, ?5)",
            rusqlite::params![session_id, event_type, prompt_text, tool_input, error],
        )
        .expect("insert event");
        // Sync into FTS content table. For a content FTS5 table, we must
        // explicitly insert matching rows. Use the rowid of the just-inserted event.
        conn.execute(
            "INSERT INTO events_fts (rowid, prompt_text, tool_input, error)
             VALUES (last_insert_rowid(), ?1, ?2, ?3)",
            rusqlite::params![prompt_text, tool_input, error],
        )
        .expect("insert into events_fts");
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let conn = in_memory_conn();
        insert_event_with_prompt(
            &conn,
            "s1",
            "UserPromptSubmit",
            Some("hello world"),
            None,
            None,
        );

        let rows = run_query(&conn, "hello").expect("run_query");
        let table = build_table(rows);
        assert_eq!(
            table.headers,
            vec![
                "session_id",
                "timestamp",
                "event_type",
                "tool_name",
                "context"
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
    fn search_returns_matches() {
        let conn = in_memory_conn();
        insert_event_with_prompt(
            &conn,
            "s1",
            "UserPromptSubmit",
            Some("implement a feature"),
            None,
            None,
        );
        insert_event_with_prompt(
            &conn,
            "s2",
            "UserPromptSubmit",
            Some("something else entirely"),
            None,
            None,
        );

        let rows = run_query(&conn, "implement").expect("run_query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_type.as_deref(), Some("UserPromptSubmit"));
    }

    #[test]
    fn context_prefers_prompt_over_error() {
        let row = SearchRow {
            session_id: Some("s1".to_string()),
            timestamp: Some("2024-01-15T10:00:00Z".to_string()),
            event_type: Some("UserPromptSubmit".to_string()),
            tool_name: None,
            prompt_text: Some("my prompt".to_string()),
            error: Some("some error".to_string()),
            tool_input: None,
        };
        let table = build_table(vec![row]);
        let out = table.render(&OutputFormat::Table);
        assert!(
            out.contains("my prompt"),
            "should prefer prompt_text over error"
        );
    }

    #[test]
    fn truncate_handles_long_strings() {
        let long = "a".repeat(110);
        let result = truncate(Some(&long), 100);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 101); // 100 + ellipsis
    }
}
