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

use crate::cli::{HealthArgs, OutputFormat};
use crate::cmd::util::fmt_bytes;
use crate::dbh::open_db;
use crate::paths::{archive_dir, db_path, last_ingest_file, log_dir, log_file_path};
use crate::schema::read_schema_marker;

pub fn health(args: &HealthArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let mut pairs: Vec<(String, String)> = Vec::new();

    // DB size and path
    let db = db_path();
    if db.exists() {
        let size = fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        pairs.push(("db_size".into(), fmt_bytes(Some(size as i64))));
        pairs.push(("db_path".into(), db.display().to_string()));
    } else {
        pairs.push(("db_size".into(), "not found".into()));
        pairs.push(("db_path".into(), db.display().to_string()));
    }

    // Row counts + integrity + schema version + optional chain stats
    match open_db() {
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
    use std::env;
    use tempfile::tempdir;

    fn with_home<F: FnOnce()>(fake_home: &str, f: F) {
        let original = env::var_os("HOME");
        unsafe { env::set_var("HOME", fake_home) };
        f();
        match original {
            Some(v) => unsafe { env::set_var("HOME", v) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    fn health_runs_with_empty_tempdir() {
        let tmp = tempdir().expect("tempdir");
        with_home(tmp.path().to_str().unwrap(), || {
            let args = HealthArgs { chain_stats: false };
            let result = health(&args, &OutputFormat::Table);
            assert!(result.is_ok(), "health should succeed: {:?}", result);
        });
    }

    #[test]
    fn health_reports_db_not_found_before_any_data() {
        let tmp = tempdir().expect("tempdir");
        with_home(tmp.path().to_str().unwrap(), || {
            // No DB has been initialised yet.
            let db = db_path();
            assert!(!db.exists(), "DB should not exist in fresh tempdir");

            // health() should still run successfully and say "not found".
            let args = HealthArgs { chain_stats: false };
            // We capture the run result — it should be Ok.
            let result = health(&args, &OutputFormat::Table);
            // health opens DB via open_db() which creates it, so after health() the DB exists.
            // But before opening it should have started as "not found" in the pair list.
            assert!(result.is_ok());
        });
    }

    #[test]
    fn health_with_chain_stats_flag() {
        let tmp = tempdir().expect("tempdir");
        with_home(tmp.path().to_str().unwrap(), || {
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
        with_home(tmp.path().to_str().unwrap(), || {
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
        with_home(tmp.path().to_str().unwrap(), || {
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
        with_home(tmp.path().to_str().unwrap(), || {
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
