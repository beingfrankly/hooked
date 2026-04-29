//! Database-handle helpers, auto-ingest trigger, common query helpers, and
//! today's JSONL loaders.
//!
//! This module is the Rust mirror of the database-related helpers in
//! `~/.claude/telemetry/query.py`:
//!
//! - [`open_db`] / [`open_db_at`] — mirror Python `_open_db`
//! - [`auto_ingest`] / [`auto_ingest_if_stale`] — mirror Python `_auto_ingest`
//! - [`find_session_events`] — mirrors Python `_find_session_events`
//! - [`load_todays_envelopes`] / [`load_envelopes_for_date`] — mirror Python
//!   `_today_jsonl_path` + `_load_today_events` (parse step only; enrichment
//!   is handled by the ingest pipeline)
//!
//! ## Python verbatim — `_open_db`
//!
//! ```python
//! def _open_db() -> sqlite3.Connection:
//!     """Open (and initialize if needed) the SQLite database."""
//!     DB_PATH.parent.mkdir(parents=True, exist_ok=True)
//!     if not INGEST_AVAILABLE:
//!         conn = sqlite3.connect(str(DB_PATH), timeout=5.0)
//!         conn.row_factory = sqlite3.Row
//!         conn.execute("PRAGMA journal_mode=WAL;")
//!         conn.execute("PRAGMA busy_timeout=5000;")
//!         return conn
//!     # If DB has no tables (e.g. newly created), force schema init by removing marker
//!     schema_marker = TELEMETRY_DIR / ".schema_v4"
//!     if DB_PATH.exists() and DB_PATH.stat().st_size < 8192:
//!         try:
//!             conn_check = sqlite3.connect(str(DB_PATH))
//!             tables = conn_check.execute(
//!                 "SELECT name FROM sqlite_master WHERE type='table'"
//!             ).fetchall()
//!             conn_check.close()
//!             if not tables and schema_marker.exists():
//!                 schema_marker.unlink()
//!         except Exception:
//!             pass
//!     return _init_db(str(DB_PATH))
//! ```
//!
//! ## Python verbatim — `_auto_ingest`
//!
//! ```python
//! def _auto_ingest() -> int:
//!     """Auto-ingest stale JSONL files before running a SQLite query."""
//!     if not INGEST_AVAILABLE:
//!         return 0
//!     LOG_DIR.mkdir(parents=True, exist_ok=True)
//!     try:
//!         n = ingest_all_unprocessed(str(DB_PATH), str(LOG_DIR))
//!         if n > 0:
//!             print(f"[auto-ingest] {n} new rows ingested", file=sys.stderr)
//!         return n
//!     except Exception as exc:
//!         print(f"[auto-ingest] WARNING: {exc}", file=sys.stderr)
//!         return 0
//! ```
//!
//! ## Python verbatim — `_find_session_events`
//!
//! ```python
//! def _find_session_events(id_prefix: str) -> list[dict]:
//!     """Find events for a session by prefix. Checks today's JSONL first, then SQLite."""
//!     # Check today
//!     today_evs = _load_today_events()
//!     matches = [e for e in today_evs if e.get("session_id", "").startswith(id_prefix)]
//!     if matches:
//!         return sorted(matches, key=lambda e: (e.get("sequence_num", 0), e.get("timestamp", "")))
//!
//!     # Check SQLite
//!     _auto_ingest()
//!     conn = _open_db()
//!     rows = _rows_as_dicts(conn, """
//!         SELECT session_id FROM sessions WHERE session_id LIKE ? LIMIT 5
//!     """, (id_prefix + "%",))
//!     conn.close()
//!
//!     if not rows:
//!         return []
//!
//!     session_id = rows[0]["session_id"]
//!     conn = _open_db()
//!     events = _rows_as_dicts(conn, """
//!         SELECT * FROM events WHERE session_id = ? ORDER BY sequence_num, timestamp
//!     """, (session_id,))
//!     conn.close()
//!     return events
//! ```
//!
//! ## Python verbatim — today's JSONL helpers
//!
//! ```python
//! def _today_jsonl_path() -> Optional[Path]:
//!     today_str = datetime.now(timezone.utc).strftime("%Y-%m-%d")
//!     p = LOG_DIR / f"hook_logs_{today_str}.jsonl"
//!     return p if p.exists() else None
//!
//! def _load_today_events() -> list[dict]:
//!     """Parse + enrich today's JSONL in-memory."""
//!     p = _today_jsonl_path()
//!     if not p or not INGEST_AVAILABLE:
//!         return []
//!     try:
//!         events = _parse_jsonl_file(str(p))
//!         events = enrich_session_events(events)
//!         _apply_git_and_config(events)
//!         by_session: dict[str, list[dict]] = {}
//!         for ev in events:
//!             by_session.setdefault(ev["session_id"], []).append(ev)
//!         enrich_cross_session(by_session)
//!         return events
//!     except Exception as exc:
//!         print(f"[query] WARNING loading today's JSONL: {exc}", file=sys.stderr)
//!         return []
//! ```

use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use rusqlite::Connection;

use crate::envelope::{Envelope, parse_jsonl_file};
use crate::ingest::{IngestAllStats, ingest_all_unprocessed};
use crate::paths::{db_path, last_ingest_file, log_dir};
use crate::{info, warn_};

// ---------------------------------------------------------------------------
// Default auto-ingest threshold
// ---------------------------------------------------------------------------

/// Default staleness threshold used by [`auto_ingest`].
///
/// Python's `_auto_ingest` has no threshold — it always runs.  In the Rust
/// implementation we use 60 seconds as a sensible default so that rapid
/// successive CLI invocations do not re-run ingestion every time.
pub const DEFAULT_AUTO_INGEST_THRESHOLD: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// open_with_pragmas / open_db / open_db_at
// ---------------------------------------------------------------------------

/// Open a file-backed SQLite connection and apply the WAL + busy_timeout
/// PRAGMAs that every production code path needs.
///
/// This is the single authoritative place those PRAGMAs are applied.
/// Any other code that opens a SQLite connection should call this
/// function, except for read-only diff opens (e.g., `parity::diff_databases`)
/// which deliberately do not need WAL.
///
/// Do NOT call this for in-memory connections (`Connection::open_in_memory()`):
/// WAL is meaningless for in-memory DBs (SQLite forces journal_mode=MEMORY),
/// and `open_with_pragmas` takes a `&Path` anyway.
pub fn open_with_pragmas(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open database at {}", path.display()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
        .context("failed to set WAL/busy_timeout PRAGMAs")?;
    Ok(conn)
}

/// Open the user's sessions DB at [`crate::paths::db_path()`] with WAL and
/// `busy_timeout` pragmas.  Initialises the schema (idempotent) if the marker
/// is missing.
///
/// Mirrors Python `_open_db` in `query.py`.
pub fn open_db() -> anyhow::Result<Connection> {
    open_db_at(&db_path())
}

/// Open a SQLite database at an explicit path with WAL and `busy_timeout`
/// pragmas.  Initialises the schema (idempotent) if the marker is missing.
///
/// Used by tests and by `--db` path overrides.
///
/// ## Steps (mirrors Python `_open_db`)
/// 1. Ensure the parent directory exists.
/// 2. Open the connection via [`open_with_pragmas`].
/// 3. Call [`init_db`] on that connection — runs the DDL only when the schema
///    marker is absent or stale.
/// 4. Return the connection (single open, no redundant second open).
pub fn open_db_at(path: &Path) -> anyhow::Result<Connection> {
    // Mirror Python: `DB_PATH.parent.mkdir(parents=True, exist_ok=True)`
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let conn = open_with_pragmas(path)?;
    crate::schema::init_db(&conn)?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// auto_ingest / auto_ingest_if_stale
// ---------------------------------------------------------------------------

/// Unconditionally trigger a full [`ingest_all_unprocessed`] run.
///
/// Mirrors Python `_auto_ingest` in `query.py`: always calls
/// `ingest_all_unprocessed`; logs a warning (not an error) on failure.
///
/// If the ingest lock is held by another process, `ingest_all_unprocessed`
/// returns with `files_processed = 0` — this is not an error; the in-flight
/// ingest will update the DB before it releases the lock.
pub fn auto_ingest() -> anyhow::Result<IngestAllStats> {
    // Mirror Python: `LOG_DIR.mkdir(parents=True, exist_ok=True)`
    let log = log_dir();
    if let Err(e) = std::fs::create_dir_all(&log) {
        warn_!(
            "auto-ingest",
            "could not create log dir {}: {}",
            log.display(),
            e
        );
    }

    let stats = ingest_all_unprocessed()?;
    if stats.total_events_inserted > 0 {
        info!(
            "auto-ingest",
            "{} new rows ingested", stats.total_events_inserted
        );
    }
    Ok(stats)
}

/// Trigger [`ingest_all_unprocessed`] only when the `.last_ingest` marker is
/// older than `threshold` (or absent).
///
/// Returns `Some(IngestAllStats)` when ingestion was triggered, `None` when
/// the marker was fresh enough to skip.
///
/// If the lock is held by another process, `ingest_all_unprocessed` will
/// return `files_processed = 0` — this is treated as success (not an error).
pub fn auto_ingest_if_stale(threshold: Duration) -> anyhow::Result<Option<IngestAllStats>> {
    let marker = last_ingest_file();

    let is_stale = match std::fs::metadata(&marker) {
        Err(_) => {
            // File does not exist → treat as stale.
            true
        }
        Ok(meta) => match meta.modified() {
            Ok(mtime) => {
                let elapsed = SystemTime::now().duration_since(mtime).unwrap_or(threshold);
                elapsed >= threshold
            }
            Err(_) => true,
        },
    };

    if is_stale {
        let stats = auto_ingest()?;
        Ok(Some(stats))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// EventRow
// ---------------------------------------------------------------------------

/// A row from the `events` table, containing the columns used by the
/// `session`, `chain`, `replay`, and related query subcommands.
///
/// Mirrors the columns selected by Python's `SELECT * FROM events` queries
/// in `_find_session_events` and `cmd_last`.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub session_id: String,
    pub event_type: String,
    pub timestamp: String,
    pub sequence_num: Option<i64>,
    pub event_hash: Option<String>,

    // Tool lifecycle
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_input: Option<String>,
    pub tool_result: Option<String>,
    pub result_size: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub is_interrupt: Option<i64>,

    // User prompt
    pub prompt_text: Option<String>,
    pub prompt_length: Option<i64>,

    // Agent context
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,

    // Session lifecycle
    pub source: Option<String>,
    pub reason: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub cwd: Option<String>,

    // Event-specific
    pub notification_type: Option<String>,
    pub compact_trigger: Option<String>,
    pub config_source: Option<String>,

    // Enrichment
    pub config_version: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,

    // Context budget
    pub input_bytes: Option<i64>,
    pub output_bytes: Option<i64>,
    pub context_cumulative_bytes: Option<i64>,

    // Skill detection
    pub skill_name: Option<String>,
    pub skill_type: Option<String>,

    // Task tracking
    pub task_id: Option<String>,
    pub task_subject: Option<String>,
    pub teammate_name: Option<String>,

    // Insurance
    pub raw_payload: Option<String>,

    // Flags
    pub is_slash_command: Option<i64>,
    pub stop_hook_active: Option<i64>,
}

// ---------------------------------------------------------------------------
// find_session_events
// ---------------------------------------------------------------------------

/// Fetch all events for a session, ordered by `sequence_num ASC, timestamp ASC`.
///
/// Mirrors Python `_find_session_events` (DB path only — today's JSONL lookup
/// is handled at the call site by the command layer using
/// [`load_todays_envelopes`]).
///
/// ## SQL (mirrors Python)
/// ```sql
/// SELECT * FROM events WHERE session_id = ? ORDER BY sequence_num, timestamp
/// ```
pub fn find_session_events(conn: &Connection, session_id: &str) -> anyhow::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(
        "SELECT
             id, session_id, event_type, timestamp, sequence_num, event_hash,
             tool_name, tool_use_id, tool_input, tool_result, result_size, duration_ms, error, is_interrupt,
             prompt_text, prompt_length,
             agent_id, agent_type,
             source, reason, model, permission_mode, cwd,
             notification_type, compact_trigger, config_source,
             config_version, git_branch, git_commit,
             input_bytes, output_bytes, context_cumulative_bytes,
             skill_name, skill_type,
             task_id, task_subject, teammate_name,
             raw_payload,
             is_slash_command, stop_hook_active
         FROM events
         WHERE session_id = ?1
         ORDER BY sequence_num, timestamp",
    )?;

    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            Ok(EventRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                event_type: row.get(2)?,
                timestamp: row.get(3)?,
                sequence_num: row.get(4)?,
                event_hash: row.get(5)?,
                tool_name: row.get(6)?,
                tool_use_id: row.get(7)?,
                tool_input: row.get(8)?,
                tool_result: row.get(9)?,
                result_size: row.get(10)?,
                duration_ms: row.get(11)?,
                error: row.get(12)?,
                is_interrupt: row.get(13)?,
                prompt_text: row.get(14)?,
                prompt_length: row.get(15)?,
                agent_id: row.get(16)?,
                agent_type: row.get(17)?,
                source: row.get(18)?,
                reason: row.get(19)?,
                model: row.get(20)?,
                permission_mode: row.get(21)?,
                cwd: row.get(22)?,
                notification_type: row.get(23)?,
                compact_trigger: row.get(24)?,
                config_source: row.get(25)?,
                config_version: row.get(26)?,
                git_branch: row.get(27)?,
                git_commit: row.get(28)?,
                input_bytes: row.get(29)?,
                output_bytes: row.get(30)?,
                context_cumulative_bytes: row.get(31)?,
                skill_name: row.get(32)?,
                skill_type: row.get(33)?,
                task_id: row.get(34)?,
                task_subject: row.get(35)?,
                teammate_name: row.get(36)?,
                raw_payload: row.get(37)?,
                is_slash_command: row.get(38)?,
                stop_hook_active: row.get(39)?,
            })
        })
        .context("failed to query events for session")?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to collect event rows")?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// load_todays_envelopes / load_envelopes_for_date
// ---------------------------------------------------------------------------

/// Load envelopes from today's not-yet-ingested JSONL file (UTC date).
///
/// Falls back gracefully to an empty vec if the file does not exist.
/// Mirrors Python `_today_jsonl_path` + `_parse_jsonl_file` in `query.py`.
pub fn load_todays_envelopes() -> anyhow::Result<Vec<Envelope>> {
    let today = chrono::Utc::now().date_naive();
    load_envelopes_for_date(today)
}

/// Load envelopes from the JSONL file for an explicit date.
///
/// Uses [`crate::paths::log_file_path`] to resolve the path.
/// Falls back gracefully to an empty vec if the file does not exist.
///
/// Mirrors Python `_load_today_events` (parse step):
/// ```python
/// events = _parse_jsonl_file(str(p))
/// ```
pub fn load_envelopes_for_date(date: chrono::NaiveDate) -> anyhow::Result<Vec<Envelope>> {
    let date_str = date.format("%Y-%m-%d").to_string();
    let path = crate::paths::log_file_path(&date_str);

    if !path.exists() {
        return Ok(Vec::new());
    }

    let result = parse_jsonl_file(&path)
        .with_context(|| format!("failed to parse JSONL file {}", path.display()))?;

    if !result.malformed.is_empty() {
        warn_!(
            "query",
            "WARNING loading {}: {} malformed line(s)",
            path.display(),
            result.malformed.len()
        );
    }

    Ok(result.envelopes)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use rusqlite::params;

    use crate::schema::SCHEMA_V4_DDL;

    // -----------------------------------------------------------------------
    // open_db tests
    // -----------------------------------------------------------------------

    #[test]
    fn open_db_creates_schema_when_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            let conn = open_db().expect("open_db should succeed");

            // Verify core tables exist.
            for table in &[
                "events",
                "sessions",
                "tool_calls",
                "annotations",
                "config_versions",
            ] {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        params![table],
                        |row| row.get(0),
                    )
                    .unwrap_or_else(|e| panic!("querying for table {table}: {e}"));
                assert_eq!(count, 1, "table '{table}' should exist after open_db");
            }
        });
    }

    #[test]
    fn open_db_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            let conn1 = open_db().expect("first open_db call");
            drop(conn1);
            // Second call should not error.
            let conn2 = open_db().expect("second open_db call should succeed");
            // Verify WAL pragma is set (should return "wal" journal mode).
            let mode: String = conn2
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .expect("pragma journal_mode");
            assert_eq!(mode, "wal", "journal mode should be WAL");
        });
    }

    // -----------------------------------------------------------------------
    // auto_ingest_if_stale tests
    // -----------------------------------------------------------------------

    #[test]
    fn auto_ingest_if_stale_when_no_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // No .last_ingest file exists — should trigger auto-ingest.
            let result =
                auto_ingest_if_stale(DEFAULT_AUTO_INGEST_THRESHOLD).expect("auto_ingest_if_stale");
            // Should return Some(...) since marker is absent.
            assert!(
                result.is_some(),
                "expected Some(IngestAllStats) when no marker"
            );
        });
    }

    #[test]
    fn auto_ingest_if_stale_skips_when_fresh() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // Create the telemetry dir and a fresh .last_ingest marker.
            let telemetry = crate::paths::telemetry_dir();
            std::fs::create_dir_all(&telemetry).expect("create telemetry dir");
            let marker = last_ingest_file();
            std::fs::write(&marker, chrono::Utc::now().to_rfc3339()).expect("write .last_ingest");

            // Threshold of 60 s — marker was just written, so should be fresh.
            let result =
                auto_ingest_if_stale(DEFAULT_AUTO_INGEST_THRESHOLD).expect("auto_ingest_if_stale");
            assert!(result.is_none(), "expected None when .last_ingest is fresh");
        });
    }

    // -----------------------------------------------------------------------
    // Helper: open an in-memory DB with the full schema applied.
    // -----------------------------------------------------------------------

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply schema DDL");
        conn
    }

    // -----------------------------------------------------------------------
    // find_session_events tests
    // -----------------------------------------------------------------------

    #[test]
    fn find_session_events_orders_by_sequence_num() {
        let conn = in_memory_conn();

        let session_id = "test-session-001";

        // Insert 3 events out of order.
        for (seq, ts) in &[
            (3i64, "2024-01-15T12:00:03Z"),
            (1, "2024-01-15T12:00:01Z"),
            (2, "2024-01-15T12:00:02Z"),
        ] {
            conn.execute(
                "INSERT INTO events (session_id, event_type, timestamp, sequence_num, event_hash)
                 VALUES (?1, 'PreToolUse', ?2, ?3, ?4)",
                params![session_id, ts, seq, format!("hash-{seq}")],
            )
            .expect("insert event");
        }

        let rows = find_session_events(&conn, session_id).expect("find_session_events");
        assert_eq!(rows.len(), 3, "expected 3 rows");

        let seqs: Vec<i64> = rows.iter().map(|r| r.sequence_num.unwrap_or(0)).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3],
            "rows should be ordered by sequence_num ASC"
        );
    }

    #[test]
    fn find_session_events_empty() {
        let conn = in_memory_conn();
        let rows = find_session_events(&conn, "nonexistent-session-xyz")
            .expect("find_session_events should succeed");
        assert!(
            rows.is_empty(),
            "unknown session_id should return empty vec"
        );
    }

    // -----------------------------------------------------------------------
    // load_envelopes_for_date tests
    // -----------------------------------------------------------------------

    #[test]
    fn load_envelopes_for_date_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // No JSONL file exists for 2020-01-01.
            let date = chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
            let result = load_envelopes_for_date(date).expect("should not error on missing file");
            assert!(result.is_empty(), "missing file should yield empty vec");
        });
    }

    #[test]
    fn load_envelopes_for_date_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path().to_str().unwrap(), || {
            // Create the log dir and a small JSONL file.
            let log = crate::paths::log_dir();
            std::fs::create_dir_all(&log).expect("create log dir");

            let date = chrono::NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
            let path = crate::paths::log_file_path("2024-01-15");

            let jsonl = concat!(
                r#"{"v":1,"ts":"2024-01-15T10:00:00Z","p":{"hook_event_name":"SessionStart","session_id":"abc123"}}"#,
                "\n",
                r#"{"v":1,"ts":"2024-01-15T10:01:00Z","p":{"hook_event_name":"PreToolUse","session_id":"abc123","tool_name":"Read"}}"#,
                "\n",
            );
            std::fs::write(&path, jsonl).expect("write JSONL file");

            let envelopes = load_envelopes_for_date(date).expect("load_envelopes_for_date");
            assert_eq!(envelopes.len(), 2, "expected 2 envelopes");
            assert_eq!(envelopes[0].v, 1);
            assert_eq!(envelopes[0].ts, "2024-01-15T10:00:00Z");
            assert_eq!(envelopes[1].ts, "2024-01-15T10:01:00Z");
        });
    }
}
