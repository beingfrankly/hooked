//! Implementation of the `import-legacy` subcommand.
//!
//! Mirrors Python `cmd_import_legacy`, `_import_legacy_jsonl`, and
//! `_import_legacy_sqlite` in `query.py` (lines 1877–2096).
//!
//! ## What it does
//!
//! 1. Scans a fixed list of "project" root directories under `$HOME`
//!    for per-project `.claude/logs/*.jsonl` (and `.jsonl.gz`) files.
//! 2. Checks `~/.claude/logs/` and `~/.local/share/claude/logs/` for legacy
//!    JSONL files.
//! 3. For each JSONL file: normalises legacy format (if needed) into the v1
//!    envelope format (`{"v":1,"ts":"...","p":{...}}`), writes a temp JSONL,
//!    and calls `ingest_file`.
//! 4. Checks a fixed list of old SQLite DB paths.  For each: reads the
//!    `events` table (up to 10,000 rows), converts each row into an envelope,
//!    writes a temp JSONL, and calls `ingest_file`.
//!
//! ## Legacy JSONL normalisation (mirrors `_import_legacy_jsonl`)
//!
//! Already-valid v1 envelopes (`"v"`, `"p"`, `"ts"` all present) are passed
//! through unchanged.  Legacy lines with `"phase": "pre"|"post"` are
//! translated:
//!
//! | phase | event_type     | payload keys used                          |
//! |-------|----------------|--------------------------------------------|
//! | pre   | PreToolUse     | tool, tool_use_id, input_summary, cwd      |
//! | post  | PostToolUse    | tool, tool_use_id, response_summary, cwd   |
//! | other | (from obj)     | event_type/hook_event_name, session_id     |
//!
//! Lines without `session_id` in the "other" branch are skipped.
//!
//! ## Old SQLite import (mirrors `_import_legacy_sqlite`)
//!
//! If the DB has an `events` table the first 10,000 rows are read and
//! converted to v1 envelopes.  Rows missing `session_id` or `event_type` are
//! skipped.  Any error returns 0 (mirrors Python `except Exception ... return 0`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use chrono::Utc;
use flate2::read::GzDecoder;
use serde_json::{Map, Value};

use crate::cli::ImportLegacyArgs;
use crate::dbh::open_db;
use crate::ingest::ingest_file;
use crate::paths::db_path;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Backfill from existing per-project JSONL logs and old SQLite databases.
///
/// Mirrors Python `cmd_import_legacy` (lines 1877–1944).
pub fn import_legacy(_args: &ImportLegacyArgs) -> anyhow::Result<()> {
    let home = home_dir();
    let mut total_rows: u64 = 0;

    // -------------------------------------------------------------------------
    // 1. Scan per-project `.claude/logs/` directories.
    //    Mirrors Python lines 1885–1928.
    // -------------------------------------------------------------------------
    println!("Scanning for legacy JSONL logs...");

    let project_roots: Vec<PathBuf> = [
        "Projects",
        "projects",
        "code",
        "Code",
        "dev",
        "Dev",
        "workspace",
        "repos",
    ]
    .iter()
    .map(|d| home.join(d))
    .collect();

    let mut found_files: Vec<PathBuf> = Vec::new();

    for root in &project_roots {
        if !root.exists() {
            continue;
        }
        // `*/.claude/logs` — one level deep under root.
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let logs_dir = entry.path().join(".claude").join("logs");
            if !logs_dir.is_dir() {
                continue;
            }
            let Ok(log_entries) = std::fs::read_dir(&logs_dir) else {
                continue;
            };
            // Mirrors Python: sorted(logs_dir.glob("*.jsonl")) then sorted(*.jsonl.gz).
            let mut plain: Vec<PathBuf> = Vec::new();
            let mut gz: Vec<PathBuf> = Vec::new();
            for e in log_entries.flatten() {
                let p = e.path();
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_owned();
                if name.ends_with(".jsonl.gz") {
                    gz.push(p);
                } else if name.ends_with(".jsonl") {
                    plain.push(p);
                }
            }
            plain.sort();
            gz.sort();
            found_files.extend(plain);
            found_files.extend(gz);
        }
    }

    // Also check the user's own legacy directories (only plain .jsonl).
    // Mirrors Python lines 1908–1917.
    let legacy_dirs: Vec<PathBuf> = vec![
        home.join(".claude").join("logs"),
        home.join(".local")
            .join("share")
            .join("claude")
            .join("logs"),
    ];
    for d in &legacy_dirs {
        if !d.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(d) else {
            continue;
        };
        let mut plain: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".jsonl") && !n.ends_with(".jsonl.gz"))
                    .unwrap_or(false)
            })
            .collect();
        plain.sort();
        found_files.extend(plain);
    }

    if !found_files.is_empty() {
        println!("Found {} legacy JSONL files.", found_files.len());
        for f in &found_files {
            match import_legacy_jsonl(f) {
                Ok(n) if n > 0 => {
                    println!("  {}: {} rows", f.display(), n);
                    total_rows += n;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("  ERROR {}: {}", f.display(), e);
                }
            }
        }
    } else {
        println!("No legacy JSONL files found in common project directories.");
    }

    // -------------------------------------------------------------------------
    // 2. Check old SQLite databases — mirrors Python lines 1931–1943.
    // -------------------------------------------------------------------------
    let old_dbs: Vec<PathBuf> = vec![
        home.join(".claude").join("telemetry.db"),
        home.join(".local")
            .join("share")
            .join("claude")
            .join("telemetry.db"),
    ];
    let current_db = db_path();

    for old_db in &old_dbs {
        if !old_db.exists() || old_db == &current_db {
            continue;
        }
        println!("\nFound old database: {}", old_db.display());
        let n = import_legacy_sqlite(old_db);
        if n > 0 {
            println!(
                "  Imported {} rows from {}",
                n,
                old_db.file_name().unwrap_or_default().to_string_lossy()
            );
            total_rows += n;
        }
    }

    println!("\nImport complete. Total new rows: {}", total_rows);
    Ok(())
}

// ---------------------------------------------------------------------------
// import_legacy_jsonl (private)
// ---------------------------------------------------------------------------

/// Normalise a legacy JSONL (or `.jsonl.gz`) file and ingest it.
///
/// Mirrors Python `_import_legacy_jsonl` (lines 1947–2027).
fn import_legacy_jsonl(path: &Path) -> anyhow::Result<u64> {
    let is_gz = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".gz"))
        .unwrap_or(false);

    // Read all lines from either gzip or plain.
    let raw_content: String = if is_gz {
        let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut decoder = GzDecoder::new(f);
        let mut s = String::new();
        decoder
            .read_to_string(&mut s)
            .with_context(|| format!("decompress {}", path.display()))?;
        s
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    };

    let mut normalized_lines: Vec<String> = Vec::new();

    for line in raw_content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let obj: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // Mirrors Python: `except json.JSONDecodeError: continue`
            Err(_) => continue,
        };

        // Already a v1 envelope: has "v", "p", "ts" keys.
        if obj.get("v").is_some() && obj.get("p").is_some() && obj.get("ts").is_some() {
            normalized_lines.push(line.to_owned());
            continue;
        }

        // Legacy format normalisation.
        // Mirrors Python lines 1972–2008.
        let phase = obj.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        let ts = obj
            .get("timestamp")
            .or_else(|| obj.get("ts"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        let session_id = obj
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let tool_name = obj
            .get("tool")
            .or_else(|| obj.get("tool_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        // Python: `tool_use_id = obj.get("tool_use_id") or f"legacy_{hash(line)}"`
        let tool_use_id = obj
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("legacy_{:x}", hash_str(line)));
        let cwd = obj
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        let payload: Map<String, Value> = match phase {
            "pre" => {
                // Mirrors Python lines 1981–1989.
                let input_summary = obj
                    .get("input_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let mut m = Map::new();
                m.insert("hook_event_name".into(), Value::String("PreToolUse".into()));
                m.insert("session_id".into(), Value::String(session_id));
                m.insert("tool_name".into(), Value::String(tool_name));
                m.insert("tool_use_id".into(), Value::String(tool_use_id));
                m.insert(
                    "tool_input".into(),
                    Value::Object({
                        let mut ti = Map::new();
                        ti.insert("_legacy_summary".into(), Value::String(input_summary));
                        ti
                    }),
                );
                m.insert("cwd".into(), Value::String(cwd));
                m
            }
            "post" => {
                // Mirrors Python lines 1990–1999.
                let response_summary = obj
                    .get("response_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let mut m = Map::new();
                m.insert(
                    "hook_event_name".into(),
                    Value::String("PostToolUse".into()),
                );
                m.insert("session_id".into(), Value::String(session_id));
                m.insert("tool_name".into(), Value::String(tool_name));
                m.insert("tool_use_id".into(), Value::String(tool_use_id));
                m.insert("tool_response".into(), Value::String(response_summary));
                m.insert("cwd".into(), Value::String(cwd));
                m
            }
            _ => {
                // Unknown format — extract event_type and re-wrap.
                // Mirrors Python lines 2001–2006.
                let event_type = obj
                    .get("event_type")
                    .or_else(|| obj.get("hook_event_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("UnknownEvent")
                    .to_owned();

                // Python: `if "session_id" not in payload: continue`
                if session_id.is_empty() {
                    continue;
                }

                let mut m = Map::new();
                // Copy all fields from obj into payload (mirrors Python `payload = dict(obj)`).
                if let Value::Object(orig) = &obj {
                    for (k, v) in orig {
                        m.insert(k.clone(), v.clone());
                    }
                }
                m.insert("hook_event_name".into(), Value::String(event_type));
                m
            }
        };

        let envelope = serde_json::json!({
            "v": 1,
            "ts": ts,
            "p": payload,
        });
        normalized_lines.push(serde_json::to_string(&envelope)?);
    }

    if normalized_lines.is_empty() {
        return Ok(0);
    }

    // Mirrors Python lines 2016–2026.
    ingest_normalized_lines(&normalized_lines)
}

// ---------------------------------------------------------------------------
// import_legacy_sqlite (private)
// ---------------------------------------------------------------------------

/// Import from an old SQLite database.
///
/// Mirrors Python `_import_legacy_sqlite` (lines 2030–2096).
/// Returns 0 on any error (mirrors `except Exception ... return 0`).
fn import_legacy_sqlite(old_db: &Path) -> u64 {
    match try_import_legacy_sqlite(old_db) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("  ERROR reading {}: {}", old_db.display(), e);
            0
        }
    }
}

fn try_import_legacy_sqlite(old_db: &Path) -> anyhow::Result<u64> {
    let conn =
        rusqlite::Connection::open(old_db).with_context(|| format!("open {}", old_db.display()))?;

    // Check what tables exist — mirrors Python lines 2036–2038.
    let tables: Vec<String> = {
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect()
    };

    println!(
        "  Tables in {}: {}",
        old_db.file_name().unwrap_or_default().to_string_lossy(),
        tables.join(", ")
    );

    if !tables.iter().any(|t| t == "events") {
        // Mirrors Python line 2091.
        println!(
            "  No 'events' table in {} — cannot import.",
            old_db.file_name().unwrap_or_default().to_string_lossy()
        );
        return Ok(0);
    }

    // Read up to 10,000 rows — mirrors Python line 2042.
    let rows: Vec<HashMap<String, Value>> = {
        let mut stmt = conn.prepare("SELECT * FROM events ORDER BY timestamp LIMIT 10000")?;
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

        stmt.query_map([], |row| {
            let mut map = HashMap::new();
            for (i, name) in col_names.iter().enumerate() {
                let val: Value = match row.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => Value::Number(n.into()),
                    rusqlite::types::ValueRef::Real(f) => Value::Number(
                        serde_json::Number::from_f64(f)
                            .unwrap_or_else(|| serde_json::Number::from(0i64)),
                    ),
                    rusqlite::types::ValueRef::Text(s) => {
                        Value::String(String::from_utf8_lossy(s).into_owned())
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        Value::String(format!("[blob:{} bytes]", b.len()))
                    }
                };
                map.insert(name.clone(), val);
            }
            Ok(map)
        })?
        .filter_map(|r| r.ok())
        .collect()
    };
    drop(conn);

    if rows.is_empty() {
        return Ok(0);
    }

    // Convert each row to a v1 envelope — mirrors Python lines 2051–2072.
    let mut normalized: Vec<String> = Vec::new();
    for row in &rows {
        let ts = row
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        let session_id = row
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        let event_type = row
            .get("event_type")
            .or_else(|| row.get("hook_event_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        // Mirrors Python: `if not session_id or not event_type: continue`
        if session_id.is_empty() || event_type.is_empty() {
            continue;
        }

        // Build payload — mirrors Python dict comprehension removing None values.
        let mut payload = Map::new();
        payload.insert("hook_event_name".into(), Value::String(event_type));
        payload.insert("session_id".into(), Value::String(session_id));

        for key in &[
            "tool_name",
            "tool_use_id",
            "cwd",
            "model",
            "agent_id",
            "agent_type",
        ] {
            if let Some(val) = row.get(*key)
                && !val.is_null()
            {
                payload.insert(key.to_string(), val.clone());
            }
        }

        let envelope = serde_json::json!({
            "v": 1,
            "ts": ts,
            "p": payload,
        });
        normalized.push(serde_json::to_string(&envelope)?);
    }

    if normalized.is_empty() {
        return Ok(0);
    }

    // Mirrors Python lines 2077–2088.
    ingest_normalized_lines(&normalized)
}

// ---------------------------------------------------------------------------
// Shared helper: write normalized lines to a temp file and ingest.
// ---------------------------------------------------------------------------

/// Write `lines` to a temporary `.jsonl` file, call [`ingest_file`], then
/// delete the temp file.
///
/// Mirrors the temp-file pattern shared by both Python helpers:
/// ```python
/// with tempfile.NamedTemporaryFile(mode="w", suffix=".jsonl", delete=False, ...) as tmp:
///     tmp.write("\n".join(normalized_lines) + "\n")
/// try:
///     n = ingest_file(...)
/// finally:
///     Path(tmp_path).unlink()
/// ```
fn ingest_normalized_lines(lines: &[String]) -> anyhow::Result<u64> {
    // Use a named temp file whose path we can pass to ingest_file.
    let mut tmp_file = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .context("create temp file for normalized JSONL")?;

    // Write content and flush before ingestion.
    let content = lines.join("\n") + "\n";
    tmp_file
        .write_all(content.as_bytes())
        .context("write normalized JSONL to temp file")?;
    tmp_file.flush().context("flush temp file")?;

    let tmp_path = tmp_file.path().to_path_buf();

    // Open the destination v4 DB.
    let mut conn = open_db().context("open destination DB")?;

    let stats = ingest_file(&mut conn, &tmp_path)
        .with_context(|| format!("ingest_file {}", tmp_path.display()))?;

    // `tmp_file` is dropped here, which deletes the temp file on all platforms.
    drop(tmp_file);

    Ok(stats.events_inserted)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple hash for a string — used to generate a `tool_use_id` for legacy
/// events that lack one.  Mirrors Python `f"legacy_{hash(line)}"`.
fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    Hasher::finish(&h)
}

/// Returns the user's home directory path.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME must be set")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::cli::ImportLegacyArgs;

    // -----------------------------------------------------------------------
    // import_legacy_no_legacy_db
    // — no legacy DBs / JSONL at all → Ok(()) with 0 rows.
    // -----------------------------------------------------------------------
    #[test]
    fn import_legacy_no_legacy_db() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            let args = ImportLegacyArgs {};
            let result = super::import_legacy(&args);
            assert!(
                result.is_ok(),
                "import_legacy with no legacy DB should return Ok: {:?}",
                result
            );
        });
    }

    // -----------------------------------------------------------------------
    // import_legacy_unrecognized_schema
    // — SQLite DB that has no `events` table → 0 rows, no error.
    // -----------------------------------------------------------------------
    #[test]
    fn import_legacy_unrecognized_schema() {
        let tmp = tempdir().expect("tempdir");
        // Build a DB with a random table, not `events`.
        let db_path = tmp.path().join("random.db");
        {
            let conn = Connection::open(&db_path).expect("open db");
            conn.execute_batch("CREATE TABLE foo (id INTEGER PRIMARY KEY);")
                .expect("create table");
        }
        // import_legacy_sqlite returns 0, does not panic.
        let n = super::import_legacy_sqlite(&db_path);
        assert_eq!(n, 0, "DB without events table should yield 0 rows");
    }

    // -----------------------------------------------------------------------
    // import_legacy_v3_session_to_v4
    // — minimal v3-shaped sessions table → v4 sessions exist after import.
    // -----------------------------------------------------------------------
    #[test]
    fn import_legacy_v3_session_to_v4() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // Build a legacy DB with an `events` table shaped like old schema.
            let legacy_db = tmp.path().join("legacy_sessions.db");
            {
                let conn = Connection::open(&legacy_db).expect("open legacy db");
                conn.execute_batch(
                    "CREATE TABLE events (
                         id         INTEGER PRIMARY KEY,
                         session_id TEXT,
                         event_type TEXT,
                         timestamp  TEXT,
                         cwd        TEXT
                     );",
                )
                .expect("DDL");
                conn.execute(
                    "INSERT INTO events (session_id, event_type, timestamp, cwd)
                     VALUES ('v3-sess-001', 'SessionStart', '2024-03-01T10:00:00Z', '/home/u/proj')",
                    [],
                )
                .expect("insert SessionStart");
                conn.execute(
                    "INSERT INTO events (session_id, event_type, timestamp, cwd)
                     VALUES ('v3-sess-001', 'PreToolUse', '2024-03-01T10:00:05Z', '/home/u/proj')",
                    [],
                )
                .expect("insert PreToolUse");
            }

            let n = super::import_legacy_sqlite(&legacy_db);
            assert!(n > 0, "expected at least 1 imported row, got {}", n);

            // Verify v4 sessions table has the session.
            let v4 = crate::dbh::open_db().expect("open v4 db");
            let count: i64 = v4
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE session_id = 'v3-sess-001'",
                    [],
                    |r| r.get(0),
                )
                .expect("count v4 sessions");
            assert_eq!(count, 1, "v4 sessions must contain v3-sess-001");
        });
    }

    // -----------------------------------------------------------------------
    // import_legacy_v3_event_to_v4
    // — multiple events from a v3 DB → all appear in v4 events table.
    // -----------------------------------------------------------------------
    #[test]
    fn import_legacy_v3_event_to_v4() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            let legacy_db = tmp.path().join("legacy_events.db");
            {
                let conn = Connection::open(&legacy_db).expect("open legacy db");
                conn.execute_batch(
                    "CREATE TABLE events (
                         id         INTEGER PRIMARY KEY,
                         session_id TEXT,
                         event_type TEXT,
                         timestamp  TEXT
                     );",
                )
                .expect("DDL");
                for (et, ts) in &[
                    ("SessionStart", "2024-04-01T09:00:00Z"),
                    ("PreToolUse", "2024-04-01T09:00:10Z"),
                    ("PostToolUse", "2024-04-01T09:00:15Z"),
                ] {
                    conn.execute(
                        "INSERT INTO events (session_id, event_type, timestamp) VALUES ('v3-ev-sess', ?1, ?2)",
                        rusqlite::params![et, ts],
                    )
                    .expect("insert event");
                }
            }

            let n = super::import_legacy_sqlite(&legacy_db);
            assert!(n > 0, "expected imported rows, got {}", n);

            let v4 = crate::dbh::open_db().expect("open v4 db");
            let count: i64 = v4
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE session_id = 'v3-ev-sess'",
                    [],
                    |r| r.get(0),
                )
                .expect("count v4 events");
            assert_eq!(count, 3, "all 3 events should appear in v4 events table");
        });
    }

    // -----------------------------------------------------------------------
    // import_legacy_v3_session_to_v4 via full import_legacy
    // — legacy JSONL in a project dir → ends up in v4 DB.
    // -----------------------------------------------------------------------
    #[test]
    fn import_legacy_jsonl_via_project_dir() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // Create a per-project legacy logs dir.
            let legacy_logs = tmp
                .path()
                .join("code")
                .join("my-project")
                .join(".claude")
                .join("logs");
            std::fs::create_dir_all(&legacy_logs).expect("create legacy logs dir");

            // Write a legacy phase: pre + post pair.
            let jsonl_path = legacy_logs.join("session.jsonl");
            let content = concat!(
                r#"{"phase":"pre","session_id":"proj-sess-01","tool":"Bash","input_summary":"ls","cwd":"/tmp","timestamp":"2024-01-10T10:00:00Z"}"#,
                "\n",
                r#"{"phase":"post","session_id":"proj-sess-01","tool":"Bash","response_summary":"ok","cwd":"/tmp","timestamp":"2024-01-10T10:00:01Z"}"#,
                "\n",
            );
            std::fs::write(&jsonl_path, content).expect("write legacy jsonl");

            let args = ImportLegacyArgs {};
            let result = super::import_legacy(&args);
            assert!(result.is_ok(), "import_legacy: {:?}", result);

            // Verify at least 2 events made it into the v4 DB.
            let db = crate::paths::db_path();
            if db.exists() {
                let conn = Connection::open(&db).expect("open v4 db");
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM events WHERE session_id = 'proj-sess-01'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                assert!(count >= 2, "expected >=2 imported events, got {count}");
            }
        });
    }
}
