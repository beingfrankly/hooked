//! Implementation of the `ingest` subcommand.
//!
//! Mirrors Python `cmd_ingest` in query.py.
//!
//! Behaviour:
//!   - With no arguments: calls `ingest_all_unprocessed()` and prints total.
//!   - With specific files: opens the DB, calls `ingest_file()` for each path,
//!     and prints per-file counts.
//!   - `--include-today`: after the normal bulk run, also ingests today's JSONL.
//!
//! Output format: plain text (no Table), mirrors Python's `print()` style.

use std::path::Path;

use crate::cli::IngestArgs;
use crate::clock::Clock;
use crate::dbh::open_db;
use crate::ingest::{ingest_all_unprocessed, ingest_file};
use crate::paths::log_dir;

pub fn ingest(args: &IngestArgs, clock: &dyn Clock) -> anyhow::Result<()> {
    if !args.files.is_empty() {
        // Per-file mode — mirrors Python: for f in files: n = ingest_file(...)
        let mut conn = open_db()?;
        let mut total = 0u64;
        for f in &args.files {
            match ingest_file(&mut conn, Path::new(f)) {
                Ok(stats) => {
                    println!("  {}: {} new rows", f, stats.events_inserted);
                    total += stats.events_inserted;
                }
                Err(e) => {
                    eprintln!("  ERROR {}: {}", f, e);
                }
            }
        }
        println!("Total: {} new rows", total);
    } else {
        // Bulk mode — mirrors Python: total = ingest_all_unprocessed(...)
        let stats = ingest_all_unprocessed()?;
        let mut total = stats.total_events_inserted;

        if args.include_today {
            let today_str = clock.now_utc().format("%Y-%m-%d").to_string();
            let today_p = log_dir().join(format!("hook_logs_{}.jsonl", today_str));
            if today_p.exists() {
                let mut conn = open_db()?;
                match ingest_file(&mut conn, &today_p) {
                    Ok(s) => {
                        let name = today_p
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        println!("  {}: {} new rows (today, forced)", name, s.events_inserted);
                        total += s.events_inserted;
                    }
                    Err(e) => {
                        eprintln!("  ERROR today's file: {}", e);
                    }
                }
            }
        }

        println!("Total: {} new rows ingested", total);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::IngestArgs;
    use crate::clock::SystemClock;
    use tempfile::tempdir;

    #[test]
    fn ingest_no_files_runs_without_error() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            let args = IngestArgs {
                files: vec![],
                include_today: false,
            };
            let clock = SystemClock;
            // Should succeed even with empty DB/log dir (nothing to process).
            let result = ingest(&args, &clock);
            assert!(
                result.is_ok(),
                "ingest with no files should succeed: {:?}",
                result
            );
        });
    }

    #[test]
    fn ingest_nonexistent_file_reports_error_but_continues() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            let args = IngestArgs {
                files: vec!["/nonexistent/path/file.jsonl".to_string()],
                include_today: false,
            };
            let clock = SystemClock;
            // Should succeed (errors are reported per-file, not propagated).
            let result = ingest(&args, &clock);
            assert!(result.is_ok());
        });
    }

    #[test]
    fn ingest_include_today_no_file_runs_cleanly() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            let args = IngestArgs {
                files: vec![],
                include_today: true,
            };
            let clock = SystemClock;
            // Today's file doesn't exist — should not error.
            let result = ingest(&args, &clock);
            assert!(result.is_ok());
        });
    }
}
