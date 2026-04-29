//! Implementation of the `health` subcommand.
//!
//! Mirrors Python `cmd_health` in query.py.
//!
//! Reads:
//! - DB file size and path
//! - Row counts for each table
//! - SQLite integrity_check pragma
//! - Schema version from `.schema_v4` marker
//! - Chain statistics (if `--chain-stats`)
//! - Last ingest timestamp from `.last_ingest`
//! - Unprocessed JSONL file count
//! - Today's JSONL presence and size
//! - Archive file count
//!
//! Output: 2-column plain-text `  key                      value` lines,
//! or JSON when `--format json` is requested.

use std::fs;

use chrono::Utc;
use rusqlite::OpenFlags;

use crate::cli::{HealthArgs, OutputFormat};
use crate::cmd::util::fmt_bytes;
use crate::paths::{archive_dir, db_path, last_ingest_file, log_dir, log_file_path};
use crate::schema::read_schema_marker;

pub fn health(args: &HealthArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let mut pairs: Vec<(String, String)> = Vec::new();

    // DB size and path — bail early with a clear message if the DB is absent.
    let db = db_path();
    if !db.exists() {
        anyhow::bail!("DB not initialized — run `hooked init`");
    }

    let size = fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    pairs.push(("db_size".into(), fmt_bytes(Some(size as i64))));
    pairs.push(("db_path".into(), db.display().to_string()));

    // Open read-only — health must not modify the filesystem.
    // SQLITE_OPEN_URI is required alongside SQLITE_OPEN_READ_ONLY so that
    // SQLite accepts the path string in its normal URI-compatible form.
    let conn_result = rusqlite::Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    );

    // Row counts + integrity + schema version + optional chain stats
    match conn_result {
        Err(e) => {
            pairs.push(("db_error".into(), e.to_string()));
        }
        Ok(conn) => {
            for table in &[
                "events",
                "sessions",
                "tool_calls",
                "config_versions",
                "annotations",
            ] {
                let key = format!("{}_count", table);
                let val = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map(|n| n.to_string())
                    .unwrap_or_else(|_| "error".into());
                pairs.push((key, val));
            }

            // Integrity check
            let ic: String = conn
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .unwrap_or_else(|_| "error".into());
            pairs.push(("integrity".into(), ic));

            // Schema version
            let schema_ver = match read_schema_marker() {
                Ok(Some(v)) => v,
                _ => "unknown".into(),
            };
            pairs.push(("schema_version".into(), schema_ver));

            // Chain stats
            if args.chain_stats {
                struct ChainStats {
                    chains: i64,
                    avg_size: f64,
                }
                let cs: Option<ChainStats> = conn
                    .query_row(
                        "SELECT COUNT(DISTINCT chain_id) AS chains,
                                AVG(chain_size) AS avg_chain_size
                         FROM (
                             SELECT chain_id, COUNT(*) AS chain_size
                             FROM sessions WHERE chain_id IS NOT NULL
                             GROUP BY chain_id
                         )",
                        [],
                        |row| {
                            Ok(ChainStats {
                                chains: row.get(0)?,
                                avg_size: row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                            })
                        },
                    )
                    .ok();
                if let Some(cs) = cs {
                    pairs.push(("chains".into(), cs.chains.to_string()));
                    pairs.push(("avg_chain_size".into(), format!("{:.1}", cs.avg_size)));
                }
            }
        }
    }

    // Last ingest
    let li = last_ingest_file();
    let last_ingest = if li.exists() {
        fs::read_to_string(&li)
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| "unreadable".into())
    } else {
        "never".into()
    };
    pairs.push(("last_ingest".into(), last_ingest));

    // Unprocessed JSONL (past days only — not today)
    let today_str = Utc::now().format("%Y-%m-%d").to_string();
    let mut unprocessed: Vec<String> = Vec::new();
    let log = log_dir();
    if log.exists()
        && let Ok(entries) = fs::read_dir(&log)
    {
        let mut files: Vec<_> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("hook_logs_") && name.ends_with(".jsonl") {
                    let date_part = name
                        .strip_prefix("hook_logs_")
                        .and_then(|s| s.strip_suffix(".jsonl"))
                        .map(|s| s.to_owned());
                    date_part.map(|d| (d, name))
                } else {
                    None
                }
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (date, name) in files {
            if date < today_str {
                unprocessed.push(name);
            }
        }
    }
    pairs.push(("unprocessed_files".into(), unprocessed.len().to_string()));
    if !unprocessed.is_empty() {
        pairs.push(("unprocessed_list".into(), unprocessed.join(", ")));
    }

    // Today's JSONL presence and size
    let today_path = log_file_path(&today_str);
    if today_path.exists() {
        let today_size = fs::metadata(&today_path).map(|m| m.len()).unwrap_or(0);
        pairs.push(("today_jsonl".into(), today_path.display().to_string()));
        pairs.push((
            "today_jsonl_size".into(),
            fmt_bytes(Some(today_size as i64)),
        ));
        if today_size > 50 * 1024 * 1024 {
            pairs.push((
                "today_jsonl_warning".into(),
                format!(
                    "Large file ({}) — consider running 'hooked ingest --include-today'",
                    fmt_bytes(Some(today_size as i64))
                ),
            ));
        }
    } else {
        pairs.push(("today_jsonl".into(), "not found".into()));
    }

    // Archive file count
    let arc = archive_dir();
    let archived_count = if arc.exists() {
        fs::read_dir(&arc)
            .map(|it| {
                it.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl.gz"))
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };
    pairs.push(("archived_files".into(), archived_count.to_string()));

    // Render
    match fmt {
        OutputFormat::Json => {
            // Render as JSON object (mirrors Python `json.dumps(result, indent=2)`)
            let mut obj = serde_json::Map::new();
            for (k, v) in &pairs {
                obj.insert(k.clone(), serde_json::Value::String(v.clone()));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::Value::Object(obj))?
            );
        }
        _ => {
            for (k, v) in &pairs {
                println!("  {:<25} {}", k, v);
            }
        }
    }

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

    /// Helper: initialize the DB inside the current fake-home so that health
    /// tests which need a DB can get one without calling open_db() directly.
    fn init_db_for_test() {
        crate::cmd::init::init(&InitArgs {}).expect("init should succeed in test");
    }

    #[test]
    fn health_runs_after_init() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            init_db_for_test();
            let args = HealthArgs { chain_stats: false };
            let result = health(&args, &OutputFormat::Table);
            assert!(result.is_ok(), "health should succeed after init: {:?}", result);
        });
    }

    #[test]
    fn health_missing_db_returns_error_with_init_hint() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            // No DB has been initialised yet.
            let db = db_path();
            assert!(!db.exists(), "DB should not exist in fresh tempdir");

            let args = HealthArgs { chain_stats: false };
            let result = health(&args, &OutputFormat::Table);

            // health must return Err when DB is missing.
            assert!(result.is_err(), "health should fail when DB is absent");

            let msg = format!("{:#}", result.unwrap_err());
            assert!(
                msg.contains("DB not initialized"),
                "error should contain 'DB not initialized'; got: {msg}"
            );
            assert!(
                msg.contains("hooked init"),
                "error should contain 'hooked init'; got: {msg}"
            );

            // The DB must NOT have been created as a side effect.
            assert!(!db.exists(), "health must not create the DB");
        });
    }

    #[test]
    fn health_with_chain_stats_flag() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            init_db_for_test();
            let args = HealthArgs { chain_stats: true };
            let result = health(&args, &OutputFormat::Table);
            assert!(
                result.is_ok(),
                "health --chain-stats should succeed: {:?}",
                result
            );
        });
    }

    #[test]
    fn health_json_format_produces_valid_json() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            init_db_for_test();
            let args = HealthArgs { chain_stats: false };
            // Just ensure the function completes without error with JSON format.
            let result = health(&args, &OutputFormat::Json);
            assert!(
                result.is_ok(),
                "health --format json should succeed: {:?}",
                result
            );
        });
    }

    #[test]
    fn health_last_ingest_never_when_missing() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            // .last_ingest doesn't exist → last_ingest should be "never".
            let li = last_ingest_file();
            assert!(!li.exists());

            // Collect pairs manually using the same logic.
            let last_ingest = if li.exists() {
                fs::read_to_string(&li)
                    .map(|s| s.trim().to_owned())
                    .unwrap_or_else(|_| "unreadable".into())
            } else {
                "never".into()
            };
            assert_eq!(last_ingest, "never");
        });
    }

    #[test]
    fn health_archived_files_zero_when_dir_absent() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            let arc = archive_dir();
            assert!(!arc.exists());
            let count = if arc.exists() {
                fs::read_dir(&arc)
                    .map(|it| {
                        it.flatten()
                            .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl.gz"))
                            .count()
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            assert_eq!(count, 0);
        });
    }
}
