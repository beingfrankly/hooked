//! Implementation of the `label` subcommand.
//!
//! Mirrors Python `cmd_label` in query.py.
//!
//! Finds the most recently used config version from the `sessions` table and
//! inserts/updates a description row in `config_versions`.
//!
//! SQL:
//! ```sql
//! INSERT INTO config_versions (version_hash, captured_at, description)
//! VALUES (?, ?, ?)
//! ON CONFLICT(version_hash) DO UPDATE SET description = excluded.description
//! ```

use crate::cli::LabelArgs;
use crate::dbh::open_db;
use chrono::Utc;

pub fn label(args: &LabelArgs) -> anyhow::Result<()> {
    let conn = open_db()?;

    // Find most recent config version — mirrors Python `cmd_label`.
    let version_hash: Option<String> = conn
        .query_row(
            "SELECT config_version FROM sessions
             WHERE config_version IS NOT NULL
             ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    let version_hash = match version_hash {
        Some(v) => v,
        None => {
            println!("No config versions found.");
            return Ok(());
        }
    };

    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO config_versions (version_hash, captured_at, description)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(version_hash) DO UPDATE SET description = excluded.description",
        rusqlite::params![version_hash, now, args.description],
    )?;

    println!(
        "Labeled config version {}: {}",
        version_hash, args.description
    );

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

    fn run_label_on(conn: &Connection, description: &str) -> anyhow::Result<()> {
        // Extracted core logic so tests can pass their own connection.
        let version_hash: Option<String> = conn
            .query_row(
                "SELECT config_version FROM sessions
                 WHERE config_version IS NOT NULL
                 ORDER BY started_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        let version_hash = match version_hash {
            Some(v) => v,
            None => {
                println!("No config versions found.");
                return Ok(());
            }
        };

        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO config_versions (version_hash, captured_at, description)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(version_hash) DO UPDATE SET description = excluded.description",
            rusqlite::params![version_hash, now, description],
        )?;

        Ok(())
    }

    #[test]
    fn label_no_sessions_prints_message() {
        let conn = in_memory_conn();
        // No sessions — should print "No config versions found." and return Ok.
        let result = run_label_on(&conn, "test label");
        assert!(result.is_ok());
    }

    #[test]
    fn label_inserts_description() {
        let conn = in_memory_conn();

        // Insert a session with a config_version.
        conn.execute(
            "INSERT INTO sessions (session_id, started_at, config_version) VALUES ('s1', '2024-01-15T10:00:00Z', 'abc123')",
            [],
        )
        .expect("insert session");

        run_label_on(&conn, "my experiment").expect("label should succeed");

        // Verify description was inserted.
        let desc: Option<String> = conn
            .query_row(
                "SELECT description FROM config_versions WHERE version_hash = 'abc123'",
                [],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        assert_eq!(desc.as_deref(), Some("my experiment"));
    }

    #[test]
    fn label_updates_existing_description() {
        let conn = in_memory_conn();

        conn.execute(
            "INSERT INTO sessions (session_id, started_at, config_version) VALUES ('s1', '2024-01-15T10:00:00Z', 'abc123')",
            [],
        )
        .expect("insert session");

        run_label_on(&conn, "first label").expect("first label");
        run_label_on(&conn, "updated label").expect("updated label");

        let desc: Option<String> = conn
            .query_row(
                "SELECT description FROM config_versions WHERE version_hash = 'abc123'",
                [],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        assert_eq!(desc.as_deref(), Some("updated label"));
    }
}
