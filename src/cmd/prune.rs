//! Implementation of the `prune` subcommand.
//!
//! Mirrors Python `cmd_prune` in query.py.
//!
//! Deletes rows from `tool_calls`, `events`, and `sessions` older than
//! `<days>` days, then runs VACUUM. Optionally deletes matching JSONL
//! archive files under `paths::archive_dir()`.
//!
//! SQL (per table):
//! ```sql
//! DELETE FROM tool_calls WHERE date(started_at) < ?
//! DELETE FROM events     WHERE date(timestamp) < ?
//! DELETE FROM sessions   WHERE date(started_at) < ?
//! ```
//!
//! Output: plain text, one line per table deleted plus a summary.

use std::io::{self, Write};

use chrono::Utc;

use crate::cli::PruneArgs;
use crate::dbh::open_db;
use crate::paths::archive_dir;

pub fn prune(args: &PruneArgs) -> anyhow::Result<()> {
    let cutoff = {
        let d = Utc::now() - chrono::Duration::days(i64::from(args.days));
        d.format("%Y-%m-%d").to_string()
    };

    // Confirmation prompt — mirrors Python unless --yes is passed.
    if !args.yes {
        print!("Delete all data before {}? [y/N] ", cutoff);
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        if line.trim().to_lowercase() != "y" {
            println!("Aborted.");
            return Ok(());
        }
    }

    let conn = open_db()?;

    // Count before deletion — mirrors Python.
    let count_evs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE date(timestamp) < ?1",
        rusqlite::params![cutoff],
        |row| row.get(0),
    )?;
    let count_sess: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE date(started_at) < ?1",
        rusqlite::params![cutoff],
        |row| row.get(0),
    )?;
    let count_tc: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tool_calls WHERE date(started_at) < ?1",
        rusqlite::params![cutoff],
        |row| row.get(0),
    )?;

    // Deletions — same order as Python.
    conn.execute(
        "DELETE FROM tool_calls WHERE date(started_at) < ?1",
        rusqlite::params![cutoff],
    )?;
    conn.execute(
        "DELETE FROM events WHERE date(timestamp) < ?1",
        rusqlite::params![cutoff],
    )?;
    conn.execute(
        "DELETE FROM sessions WHERE date(started_at) < ?1",
        rusqlite::params![cutoff],
    )?;

    conn.execute_batch("VACUUM")?;

    println!(
        "Deleted: {} events, {} sessions, {} tool calls (before {})",
        count_evs, count_sess, count_tc, cutoff
    );

    // Optional archive deletion — mirrors Python.
    if args.archive {
        if !args.yes {
            print!("Also delete JSONL archives? This is IRREVERSIBLE. [y/N] ");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            if line.trim().to_lowercase() != "y" {
                println!("Skipping archive deletion.");
                return Ok(());
            }
        }

        let archive = archive_dir();
        let mut deleted_archives = 0u32;

        if archive.exists() {
            // Mirrors Python: for gz in ARCHIVE_DIR.glob("hook_logs_*.jsonl.gz")
            //   m = re.search(r"hook_logs_(\d{4}-\d{2}-\d{2})\.jsonl\.gz$", gz.name)
            //   if m and m.group(1) < cutoff: gz.unlink()
            let entries = std::fs::read_dir(&archive)?;
            let mut gz_files: Vec<std::path::PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("hook_logs_") && n.ends_with(".jsonl.gz"))
                        .unwrap_or(false)
                })
                .collect();
            gz_files.sort();

            for gz in gz_files {
                // Extract date portion: hook_logs_YYYY-MM-DD.jsonl.gz
                if let Some(date_part) = gz
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix("hook_logs_"))
                    .and_then(|s| s.strip_suffix(".jsonl.gz"))
                    && date_part < cutoff.as_str()
                {
                    if let Err(e) = std::fs::remove_file(&gz) {
                        eprintln!("WARNING: could not delete {}: {}", gz.display(), e);
                    } else {
                        deleted_archives += 1;
                    }
                }
            }
        }

        println!("Deleted {} archive files.", deleted_archives);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::schema::SCHEMA_V4_DDL;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema DDL");
        conn
    }

    fn insert_session_at(conn: &Connection, session_id: &str, started_at: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, started_at) VALUES (?1, ?2)",
            rusqlite::params![session_id, started_at],
        )
        .expect("insert session");
    }

    fn insert_event_at(conn: &Connection, session_id: &str, ts: &str) {
        conn.execute(
            "INSERT INTO events (session_id, event_type, timestamp) VALUES (?1, 'PreToolUse', ?2)",
            rusqlite::params![session_id, ts],
        )
        .expect("insert event");
    }

    fn insert_tool_call_at(conn: &Connection, session_id: &str, started_at: &str) {
        conn.execute(
            "INSERT INTO tool_calls (session_id, tool_use_id, tool_name, started_at)
             VALUES (?1, ?2, 'Read', ?3)",
            rusqlite::params![session_id, format!("uid-{}", started_at), started_at],
        )
        .expect("insert tool_call");
    }

    fn run_prune_on(conn: &Connection, cutoff: &str) -> (i64, i64, i64) {
        // Count before.
        let count_evs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE date(timestamp) < ?1",
                rusqlite::params![cutoff],
                |r| r.get(0),
            )
            .unwrap();
        let count_sess: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE date(started_at) < ?1",
                rusqlite::params![cutoff],
                |r| r.get(0),
            )
            .unwrap();
        let count_tc: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_calls WHERE date(started_at) < ?1",
                rusqlite::params![cutoff],
                |r| r.get(0),
            )
            .unwrap();

        conn.execute(
            "DELETE FROM tool_calls WHERE date(started_at) < ?1",
            rusqlite::params![cutoff],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM events WHERE date(timestamp) < ?1",
            rusqlite::params![cutoff],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM sessions WHERE date(started_at) < ?1",
            rusqlite::params![cutoff],
        )
        .unwrap();

        (count_evs, count_sess, count_tc)
    }

    #[test]
    fn prune_deletes_old_rows_only() {
        let conn = in_memory_conn();

        insert_session_at(&conn, "old-session", "2020-01-01T00:00:00Z");
        insert_session_at(&conn, "new-session", "2024-06-01T00:00:00Z");
        insert_event_at(&conn, "old-session", "2020-01-01T00:00:00Z");
        insert_event_at(&conn, "new-session", "2024-06-01T00:00:00Z");
        insert_tool_call_at(&conn, "old-session", "2020-01-01T00:00:00Z");
        insert_tool_call_at(&conn, "new-session", "2024-06-01T00:00:00Z");

        let (evs, sess, tc) = run_prune_on(&conn, "2023-01-01");
        assert_eq!(evs, 1, "1 old event should be deleted");
        assert_eq!(sess, 1, "1 old session should be deleted");
        assert_eq!(tc, 1, "1 old tool_call should be deleted");

        // new rows remain
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn prune_nothing_to_delete() {
        let conn = in_memory_conn();
        insert_session_at(&conn, "recent", "2025-01-01T00:00:00Z");

        let (evs, sess, tc) = run_prune_on(&conn, "2020-01-01");
        assert_eq!(evs, 0);
        assert_eq!(sess, 0);
        assert_eq!(tc, 0);
    }

    #[test]
    fn prune_all_rows() {
        let conn = in_memory_conn();
        for i in 0..5 {
            insert_session_at(&conn, &format!("s{}", i), "2019-01-01T00:00:00Z");
        }

        let (_, sess, _) = run_prune_on(&conn, "2024-01-01");
        assert_eq!(sess, 5, "all 5 sessions should be counted for deletion");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
