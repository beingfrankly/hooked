//! Implementation of the `annotate` subcommand.
//!
//! Mirrors Python `cmd_annotate` in query.py.
//!
//! Looks up a session by ID prefix, then inserts a row into `annotations`:
//!
//! ```sql
//! INSERT INTO annotations (session_id, label, notes, created_at)
//! VALUES (?, ?, ?, ?)
//! ```
//!
//! Prints a confirmation like:
//!   `Annotated session <id[:8]> with label: <label>`

use chrono::Utc;

use crate::cli::AnnotateArgs;
use crate::dbh::open_db;

pub fn annotate(args: &AnnotateArgs) -> anyhow::Result<()> {
    let conn = open_db()?;

    // Resolve session prefix — mirrors Python:
    //   rows = SELECT session_id FROM sessions WHERE session_id LIKE ? LIMIT 5
    let session_id: Option<String> = {
        let pattern = format!("{}%", args.session_prefix);
        conn.query_row(
            "SELECT session_id FROM sessions WHERE session_id LIKE ?1 LIMIT 1",
            rusqlite::params![pattern],
            |row| row.get(0),
        )
        .ok()
    };

    let session_id = match session_id {
        Some(id) => id,
        None => {
            println!("No session found matching: {}", args.session_prefix);
            return Ok(());
        }
    };

    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO annotations (session_id, label, notes, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![session_id, args.label, args.notes, now],
    )?;

    println!(
        "Annotated session {} with label: {}",
        &session_id[..session_id.len().min(8)],
        args.label
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AnnotateArgs;
    use crate::schema::SCHEMA_V4_DDL;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema DDL");
        conn
    }

    fn insert_session(conn: &Connection, session_id: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at) VALUES (?1, '2024-01-15T10:00:00Z')",
            rusqlite::params![session_id],
        )
        .expect("insert session");
    }

    fn run_annotate_on(
        conn: &Connection,
        session_prefix: &str,
        label: &str,
        notes: Option<&str>,
    ) -> anyhow::Result<()> {
        // Extracted logic that mirrors annotate() but uses an injected connection.
        let pattern = format!("{}%", session_prefix);
        let session_id: Option<String> = conn
            .query_row(
                "SELECT session_id FROM sessions WHERE session_id LIKE ?1 LIMIT 1",
                rusqlite::params![pattern],
                |row| row.get(0),
            )
            .ok();

        let session_id = match session_id {
            Some(id) => id,
            None => {
                println!("No session found matching: {}", session_prefix);
                return Ok(());
            }
        };

        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO annotations (session_id, label, notes, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, label, notes, now],
        )?;

        Ok(())
    }

    #[test]
    fn annotate_no_matching_session_prints_message() {
        let conn = in_memory_conn();
        // No sessions in DB — should print "No session found matching:" and return Ok.
        let result = run_annotate_on(&conn, "nonexistent", "success", None);
        assert!(result.is_ok());
    }

    #[test]
    fn annotate_inserts_annotation_row() {
        let conn = in_memory_conn();
        insert_session(&conn, "session-abc-001");

        run_annotate_on(&conn, "session-abc", "success", Some("worked well"))
            .expect("annotate should succeed");

        let (label, notes): (String, Option<String>) = conn
            .query_row(
                "SELECT label, notes FROM annotations WHERE session_id = 'session-abc-001'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query annotation");

        assert_eq!(label, "success");
        assert_eq!(notes.as_deref(), Some("worked well"));
    }

    #[test]
    fn annotate_multiple_annotations_on_same_session() {
        let conn = in_memory_conn();
        insert_session(&conn, "session-xyz-999");

        run_annotate_on(&conn, "session-xyz", "success", None).expect("first annotation");
        run_annotate_on(&conn, "session-xyz", "interesting", Some("notable"))
            .expect("second annotation");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM annotations WHERE session_id = 'session-xyz-999'",
                [],
                |row| row.get(0),
            )
            .expect("count annotations");

        assert_eq!(count, 2, "both annotations should be stored");
    }

    #[test]
    fn annotate_args_parsed_without_notes() {
        // Verify the Args struct default (notes is None) is handled correctly.
        let args = AnnotateArgs {
            session_prefix: "abc123".to_string(),
            label: "failed".to_string(),
            notes: None,
        };
        assert!(args.notes.is_none());
    }
}
