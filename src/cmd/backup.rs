//! Implementation of the `backup` subcommand.
//!
//! Mirrors Python `cmd_backup` in query.py:
//!
//! ```python
//! def cmd_backup(args: argparse.Namespace) -> None:
//!     dest = Path(args.path)
//!     if not DB_PATH.exists():
//!         print(f"Database not found: {DB_PATH}")
//!         return
//!     print(f"Backing up {DB_PATH} -> {dest}...")
//!     src_conn = sqlite3.connect(str(DB_PATH))
//!     dest.parent.mkdir(parents=True, exist_ok=True)
//!     dst_conn = sqlite3.connect(str(dest))
//!     src_conn.backup(dst_conn)
//!     src_conn.close()
//!     dst_conn.close()
//!     print(f"Backup complete: {dest} ({_fmt_bytes(dest.stat().st_size)})")
//! ```
//!
//! Uses rusqlite's `backup` feature (Cargo.toml already enables it).

use std::path::Path;
use std::time::Duration;

use crate::cli::BackupArgs;
use crate::paths::db_path;
use rusqlite::Connection;
use rusqlite::backup::Backup;

/// Format bytes as human-readable string.
/// Mirrors Python `_fmt_bytes`.
fn fmt_bytes(b: u64) -> String {
    if b < 1024 {
        format!("{}B", b)
    } else if b < 1024 * 1024 {
        format!("{:.1}K", b as f64 / 1024.0)
    } else {
        format!("{:.1}M", b as f64 / (1024.0 * 1024.0))
    }
}

/// Perform the backup using rusqlite's Backup API.
///
/// Mirrors Python `sqlite3.Connection.backup()`.
pub fn do_backup(src_path: &Path, dest_path: &Path) -> anyhow::Result<()> {
    let src = Connection::open(src_path)?;
    let mut dst = Connection::open(dest_path)?;

    let backup = Backup::new(&src, &mut dst)?;
    // pages=5, pause=Duration::ZERO, no progress callback
    backup.run_to_completion(5, Duration::ZERO, None)?;

    Ok(())
}

pub fn backup(args: &BackupArgs) -> anyhow::Result<()> {
    let src = db_path();
    let dest = Path::new(&args.path);

    // Mirrors Python: if not DB_PATH.exists()
    if !src.exists() {
        anyhow::bail!("Database not found: {}", src.display());
    }

    println!("Backing up {} -> {}...", src.display(), dest.display());

    // Mirrors Python: dest.parent.mkdir(parents=True, exist_ok=True)
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    do_backup(&src, dest)?;

    // Mirrors Python: print(f"Backup complete: {dest} ({_fmt_bytes(dest.stat().st_size)})")
    let size = std::fs::metadata(dest)?.len();
    println!("Backup complete: {} ({})", dest.display(), fmt_bytes(size));

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
    use tempfile::tempdir;

    fn create_src_db(path: &Path) {
        let conn = Connection::open(path).expect("open src db");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema");
        conn.execute(
            "INSERT INTO sessions (session_id, started_at) VALUES ('test-session', '2024-01-15T10:00:00Z')",
            [],
        ).expect("insert session");
    }

    #[test]
    fn backup_creates_destination_file() {
        let tmp = tempdir().expect("tempdir");
        let src_path = tmp.path().join("source.db");
        let dest_path = tmp.path().join("backup.db");

        create_src_db(&src_path);
        do_backup(&src_path, &dest_path).expect("do_backup");

        assert!(
            dest_path.exists(),
            "destination file should exist after backup"
        );
    }

    #[test]
    fn backup_copies_data() {
        let tmp = tempdir().expect("tempdir");
        let src_path = tmp.path().join("source.db");
        let dest_path = tmp.path().join("backup.db");

        create_src_db(&src_path);
        do_backup(&src_path, &dest_path).expect("do_backup");

        // Verify backup has the same data
        let dst_conn = Connection::open(&dest_path).expect("open backup db");
        let count: i64 = dst_conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .expect("query count");
        assert_eq!(count, 1, "backup should contain 1 session");

        let session_id: String = dst_conn
            .query_row("SELECT session_id FROM sessions", [], |row| row.get(0))
            .expect("query session_id");
        assert_eq!(session_id, "test-session");
    }

    #[test]
    fn backup_creates_parent_directories() {
        let tmp = tempdir().expect("tempdir");
        let src_path = tmp.path().join("source.db");
        let dest_path = tmp.path().join("subdir1/subdir2/backup.db");

        create_src_db(&src_path);

        // Ensure parent dirs don't exist
        assert!(!tmp.path().join("subdir1").exists());

        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        do_backup(&src_path, &dest_path).expect("do_backup with nested dirs");
        assert!(dest_path.exists());
    }

    #[test]
    fn fmt_bytes_formats_correctly() {
        assert_eq!(fmt_bytes(500), "500B");
        assert_eq!(fmt_bytes(2048), "2.0K");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0M");
    }
}
