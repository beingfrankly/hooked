//! Implementation of the `rebuild` subcommand.
//!
//! Mirrors Python `cmd_rebuild` in query.py (lines 1664–1747).
//!
//! Behaviour:
//! 1. Acquire ingest lock (non-blocking). If held, return early with a message.
//! 2. Delete the current DB at `paths::db_path()` and remove the schema marker.
//! 3. Re-init schema via `schema::init_db`.
//! 4. Iterate every `*.jsonl.gz` in `paths::archive_dir()`, optionally filtered
//!    by `--since DATE`.
//! 5. For each archive: decompress into a `tempfile::TempDir`-owned file, then
//!    call `ingest::ingest_file` with that path.
//! 6. FTS5 rebuild + WAL checkpoint once at the end (same pattern as
//!    `ingest_all_unprocessed`).
//! 7. Print stats: total files processed, total events ingested.
//!
//! ## TempDir guard
//!
//! A single `tempfile::TempDir` is held for the entire function body. Each
//! decompressed file is written inside it. When the function returns (or
//! errors via `?`), the `TempDir` is dropped and all temp files are removed
//! automatically.

use std::io;
use std::path::PathBuf;

use anyhow::Context as _;
use flate2::read::GzDecoder;

use crate::cli::RebuildArgs;
use crate::ingest::archive::IngestLock;
use crate::ingest::ingest_file;
use crate::paths;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn rebuild(args: &RebuildArgs) -> anyhow::Result<()> {
    // 1. Acquire ingest lock (non-blocking). Return early if held.
    let _lock = match IngestLock::try_acquire().context("acquire ingest lock")? {
        Some(lock) => lock,
        None => {
            println!("Another ingest is running; skipping rebuild.");
            return Ok(());
        }
    };

    // 2. Confirmation prompt (skipped with --yes).
    if !args.yes {
        print!("This will drop all SQLite tables and re-ingest all JSONL. Continue? [y/N] ");
        io::Write::flush(&mut io::stdout())?;
        let mut line = String::new();
        io::BufRead::read_line(&mut io::stdin().lock(), &mut line)?;
        if line.trim().to_lowercase() != "y" {
            println!("Aborted.");
            return Ok(());
        }
    }

    // 3. Delete DB and schema marker so init_db performs a full re-init.
    let db = paths::db_path();
    println!("Dropping existing database...");
    if db.exists() {
        std::fs::remove_file(&db).with_context(|| format!("remove database {:?}", db))?;
    }
    let marker = paths::schema_marker();
    if marker.exists() {
        std::fs::remove_file(&marker)
            .with_context(|| format!("remove schema marker {:?}", marker))?;
    }

    // 4. Re-init schema and open a single connection for the whole rebuild run.
    let mut conn = crate::dbh::open_db_at(&db).context("open_db_at after drop")?;

    // 6. TempDir guard — all decompressed gz files live here until function exit.
    let tmp = tempfile::TempDir::new().context("create tempdir")?;

    // 7. Collect and filter archives.
    let archive_dir = paths::archive_dir();
    let mut archives: Vec<PathBuf> = if archive_dir.exists() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&archive_dir)
            .with_context(|| format!("read archive dir {:?}", archive_dir))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("hook_logs_") && n.ends_with(".jsonl.gz"))
                    .unwrap_or(false)
            })
            .collect();
        v.sort();
        v
    } else {
        Vec::new()
    };

    // Apply --since filter (mirrors Python: `if m.group(1) < since_date: continue`).
    if let Some(since) = &args.since {
        archives.retain(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let date = name
                .strip_prefix("hook_logs_")
                .and_then(|s| s.strip_suffix(".jsonl.gz"))
                .unwrap_or("");
            date >= since.as_str()
        });
    }

    // 8. Decompress and ingest each archive.
    let mut total_files: u64 = 0;
    let mut total_events: u64 = 0;

    for archive in &archives {
        let name = archive
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // Derive decompressed basename by stripping ".gz".
        let decompressed_name = name.strip_suffix(".gz").unwrap_or(name);
        let dst = tmp.path().join(decompressed_name);

        // Decompress following the TempDir guard pattern from the task spec.
        let src_file =
            std::fs::File::open(archive).with_context(|| format!("open archive {:?}", archive))?;
        let mut decoder = GzDecoder::new(src_file);
        let mut dst_file =
            std::fs::File::create(&dst).with_context(|| format!("create temp file {:?}", dst))?;
        io::copy(&mut decoder, &mut dst_file)
            .with_context(|| format!("decompress {:?}", archive))?;

        // Ingest the decompressed file.
        match ingest_file(&mut conn, &dst) {
            Ok(stats) => {
                println!("  {}: {} rows", decompressed_name, stats.events_inserted);
                total_events += stats.events_inserted;
                total_files += 1;
            }
            Err(e) => {
                eprintln!("  ERROR {}: {}", decompressed_name, e);
            }
        }
    }

    // 9. FTS5 rebuild (once, after all inserts) — mirrors Python.
    match conn.execute("INSERT INTO events_fts(events_fts) VALUES('rebuild')", []) {
        Ok(_) => println!("FTS5 index rebuilt."),
        Err(e) => eprintln!("FTS5 rebuild warning: {}", e),
    }

    // 10. WAL checkpoint — mirrors Python `PRAGMA wal_checkpoint(TRUNCATE)`.
    let _: Result<(i64, i64, i64), _> =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        });

    // 11. Stats.
    println!(
        "\nRebuild complete. Files: {}, total rows ingested: {}",
        total_files, total_events
    );

    // `tmp` drops here — decompressed files are deleted automatically.
    // `_lock` drops here — ingest lock is released.
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use flate2::write::GzEncoder;
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;
    use crate::cli::RebuildArgs;
    use crate::ingest::archive::IngestLock;

    // -----------------------------------------------------------------------
    // rebuild_with_no_archives
    // -----------------------------------------------------------------------

    /// Empty archive dir → no error, DB initialized empty, prints stats with 0
    /// files.
    #[test]
    fn rebuild_with_no_archives() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            let args = RebuildArgs {
                since: None,
                yes: true,
            };
            let result = rebuild(&args);
            assert!(result.is_ok(), "rebuild should succeed: {:?}", result);

            // DB must exist now with the schema initialized.
            let db = paths::db_path();
            assert!(db.exists(), "DB must be created by rebuild");

            // events table must exist and be empty.
            let conn = Connection::open(&db).expect("open db");
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                .expect("count events");
            assert_eq!(
                count, 0,
                "events table should be empty after rebuild with no archives"
            );
        });
    }

    // -----------------------------------------------------------------------
    // rebuild_with_one_archive
    // -----------------------------------------------------------------------

    /// Write a known gzipped JSONL to archive_dir, call rebuild, verify the
    /// events table has the expected row count.
    #[test]
    fn rebuild_with_one_archive() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // Create archive dir.
            let arc_dir = paths::archive_dir();
            std::fs::create_dir_all(&arc_dir).expect("create archive dir");

            // Two-event JSONL payload (each line must be valid standalone JSON).
            let plain = concat!(
                "{\"v\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"p\":{\"hook_event_name\":\"SessionStart\",\"session_id\":\"rebuild-test\"}}\n",
                "{\"v\":1,\"ts\":\"2026-01-01T00:01:00Z\",\"p\":{\"hook_event_name\":\"SessionEnd\",\"session_id\":\"rebuild-test\"}}\n",
            ).as_bytes();

            let archive_path = arc_dir.join("hook_logs_2026-01-01.jsonl.gz");
            {
                let f = std::fs::File::create(&archive_path).expect("create gz");
                let mut enc = GzEncoder::new(f, flate2::Compression::default());
                enc.write_all(plain).expect("write plain");
                enc.finish().expect("finish gz");
            }

            let args = RebuildArgs {
                since: None,
                yes: true,
            };
            let result = rebuild(&args);
            assert!(result.is_ok(), "rebuild should succeed: {:?}", result);

            // Verify events table has 2 rows.
            let db = paths::db_path();
            let conn = Connection::open(&db).expect("open db");
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                .expect("count events");
            assert_eq!(count, 2, "events table should have 2 rows after rebuild");
        });
    }

    // -----------------------------------------------------------------------
    // rebuild_acquires_lock
    // -----------------------------------------------------------------------

    /// Hold the ingest lock manually, call rebuild → returns early without
    /// processing any files or creating the DB.
    #[test]
    fn rebuild_acquires_lock() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // Pre-acquire the lock.
            let _held = IngestLock::try_acquire()
                .expect("try_acquire")
                .expect("should succeed on first call");

            let args = RebuildArgs {
                since: None,
                yes: true,
            };
            let result = rebuild(&args);
            assert!(
                result.is_ok(),
                "rebuild should return Ok when lock is held: {:?}",
                result
            );

            // DB must NOT have been created (we returned early from the lock check).
            let db = paths::db_path();
            assert!(
                !db.exists(),
                "DB must not be created when lock is held (rebuild returned early)"
            );
        });
    }
}
