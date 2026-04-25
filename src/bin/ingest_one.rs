//! `ingest_one` binary — ingest a single JSONL fixture into a fresh SQLite DB.
//!
//! Usage:
//!   ingest_one <fixture-path> <db-path>
//!
//! Creates the DB schema at `<db-path>`, then calls `hooked::ingest::ingest_file`.
//! This auxiliary binary is used by `tests/parity/run_parity.sh` to produce the
//! Rust-side database for each fixture before diffing against the Python-side DB.

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let fixture = args.next().ok_or_else(|| {
        anyhow::anyhow!(
            "Usage: ingest_one <fixture-path> <db-path>\narg 1: fixture path is required"
        )
    })?;
    let db_path = args.next().ok_or_else(|| {
        anyhow::anyhow!("Usage: ingest_one <fixture-path> <db-path>\narg 2: db path is required")
    })?;

    let db_path = Path::new(&db_path);
    let fixture_path = Path::new(&fixture);

    // Initialise the schema into a fresh database file.
    // We open the connection directly (bypassing schema_marker side-effects)
    // because parity runs are ephemeral and must not touch the production DB.
    let conn = hooked::parity::open_db(db_path)?;

    // Drop the shared read-only connection; ingest_file needs &mut Connection.
    drop(conn);

    let mut conn = rusqlite::Connection::open(db_path)
        .map_err(|e| anyhow::anyhow!("failed to re-open {}: {e}", db_path.display()))?;

    let stats = hooked::ingest::ingest_file(&mut conn, fixture_path)?;
    eprintln!("ingest_one: {:?}", stats);

    Ok(())
}
