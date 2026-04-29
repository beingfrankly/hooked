//! Implementation of the `init` subcommand.
//!
//! Creates the v4 SQLite database and writes the schema marker if they
//! don't already exist.  Idempotent: re-running on an already-initialized
//! DB is a no-op (init_db's marker fast-path skips DDL when the schema
//! is current).
//!
//! On a corrupted state (marker present but tables missing), init_db
//! returns an Err naming the missing tables.  This subcommand surfaces
//! that error verbatim — the user can recover by deleting the marker
//! or running `hooked rebuild`.

use anyhow::Context;

use crate::cli::InitArgs;

pub fn init(_args: &InitArgs) -> anyhow::Result<()> {
    let path = crate::paths::db_path();
    let _conn = crate::dbh::open_db_at(&path)
        .with_context(|| format!("failed to initialize database at {}", path.display()))?;
    println!("Initialized database at {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::cli::InitArgs;

    #[test]
    fn init_creates_fresh_db() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            // Before init: db and marker should not exist.
            let db = crate::paths::db_path();
            let marker = crate::paths::schema_marker();
            assert!(!db.exists(), "DB should not exist before init");
            assert!(!marker.exists(), "schema marker should not exist before init");

            // Run init.
            let result = init(&InitArgs {});
            assert!(result.is_ok(), "init should succeed: {:?}", result);

            // After init: db and marker should exist.
            assert!(db.exists(), "DB should exist after init");
            assert!(marker.exists(), "schema marker should exist after init");

            // Verify required tables are present.
            let conn = rusqlite::Connection::open(&db).expect("open db");
            for table in &["events", "sessions", "tool_calls", "config_versions", "annotations"] {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        rusqlite::params![table],
                        |row| row.get(0),
                    )
                    .unwrap_or_else(|e| panic!("querying for table {table}: {e}"));
                assert_eq!(count, 1, "table '{table}' should exist after init");
            }
        });
    }

    #[test]
    fn init_is_idempotent() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            // First call.
            let result1 = init(&InitArgs {});
            assert!(result1.is_ok(), "first init should succeed: {:?}", result1);

            // Second call.
            let result2 = init(&InitArgs {});
            assert!(result2.is_ok(), "second init should succeed: {:?}", result2);

            // Marker content should still be the current schema version.
            let marker = crate::paths::schema_marker();
            let content = std::fs::read_to_string(&marker).expect("read schema marker");
            assert_eq!(
                content.trim(),
                crate::paths::SCHEMA_VERSION,
                "schema marker content should equal SCHEMA_VERSION after two inits"
            );

            // Tables should still all be present.
            let db = crate::paths::db_path();
            let conn = rusqlite::Connection::open(&db).expect("open db");
            for table in &["events", "sessions", "tool_calls", "config_versions", "annotations"] {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        rusqlite::params![table],
                        |row| row.get(0),
                    )
                    .unwrap_or_else(|e| panic!("querying for table {table}: {e}"));
                assert_eq!(count, 1, "table '{table}' should still exist after second init");
            }
        });
    }
}
