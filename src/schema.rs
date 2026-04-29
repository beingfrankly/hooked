//! Schema DDL v4 and database initialisation.
//!
//! The DDL constant [`SCHEMA_V4_DDL`] is ported verbatim from the Python
//! `DDL` constant in `~/.claude/telemetry/ingest.py`, minus the leading
//! PRAGMA lines (which are now applied at connection time via
//! [`crate::dbh::open_with_pragmas`]).  Whitespace and comments inside the
//! SQL are preserved character-for-character.  The test
//! `rust_ddl_matches_python_ddl_source` reads `ingest.py` at test-time and
//! asserts structural equality after stripping PRAGMA lines from both sides
//! (T04 reconciliation option b — `ingest.py` is left untouched).
//!
//! # Marker file
//!
//! Python writes `"v4"` (no newline) into `.schema_v4` after a successful
//! schema initialisation and checks `marker.read_text().strip() == SCHEMA_VERSION`
//! on startup.  This module mirrors that behaviour exactly.

use std::fs;

use anyhow::Context;
use rusqlite::Connection;

use crate::paths::{SCHEMA_VERSION, schema_marker};

// ---------------------------------------------------------------------------
// DDL — ported verbatim from Python `ingest.py` lines 55-204
// ---------------------------------------------------------------------------

/// The complete v4 schema DDL, ported verbatim from Python `ingest.py`.
///
/// This string is fed to [`Connection::execute_batch`] which handles the
/// semicolon-separated statements in one call.  The content must remain
/// byte-for-byte identical to the Python source so that SQLite stores
/// exactly the same `sql` text in `sqlite_schema`, preserving schema-hash
/// parity between the Python and Rust implementations.
///
/// PRAGMAs intentionally not in DDL — applied at connection time via
/// `dbh::open_with_pragmas`.
pub const SCHEMA_V4_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id               TEXT NOT NULL,
    event_type               TEXT NOT NULL,
    timestamp                TEXT NOT NULL,
    sequence_num             INTEGER,
    event_hash               TEXT,

    -- Tool lifecycle
    tool_name                TEXT,
    tool_use_id              TEXT,
    tool_input               TEXT,
    tool_result              TEXT,
    result_size              INTEGER,
    duration_ms              INTEGER,
    error                    TEXT,
    is_interrupt             INTEGER,

    -- User prompt
    prompt_text              TEXT,
    prompt_length            INTEGER,

    -- Agent context
    agent_id                 TEXT,
    agent_type               TEXT,

    -- Session lifecycle
    source                   TEXT,
    reason                   TEXT,
    model                    TEXT,
    permission_mode          TEXT,
    cwd                      TEXT,

    -- Event-specific (field-isolated by event_type)
    notification_type        TEXT,
    compact_trigger          TEXT,
    config_source            TEXT,

    -- Enrichment (computed during ingestion)
    config_version           TEXT,
    git_branch               TEXT,
    git_commit               TEXT,

    -- Context budget (computed during ingestion)
    input_bytes              INTEGER,
    output_bytes             INTEGER,
    context_cumulative_bytes INTEGER,

    -- Skill detection (computed during ingestion)
    skill_name               TEXT,
    skill_type               TEXT,

    -- Task tracking
    task_id                  TEXT,
    task_subject             TEXT,
    teammate_name            TEXT,

    -- Insurance
    raw_payload              TEXT,

    -- Flags
    is_slash_command         INTEGER DEFAULT 0,
    stop_hook_active         INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sessions (
    session_id           TEXT PRIMARY KEY,
    started_at           TEXT,
    ended_at             TEXT,
    source               TEXT,
    chain_id             TEXT,
    parent_session_id    TEXT,
    end_reason           TEXT,
    model                TEXT,
    permission_mode      TEXT,
    cwd                  TEXT,
    config_version       TEXT,
    git_branch           TEXT,
    git_commit           TEXT,
    total_events         INTEGER DEFAULT 0,
    total_tool_calls     INTEGER DEFAULT 0,
    total_failures       INTEGER DEFAULT 0,
    total_prompts        INTEGER DEFAULT 0,
    total_subagents      INTEGER DEFAULT 0,
    total_tasks          INTEGER DEFAULT 0,
    compaction_count     INTEGER DEFAULT 0,
    auto_compact_count   INTEGER DEFAULT 0,
    permission_prompts   INTEGER DEFAULT 0,
    context_total_bytes  INTEGER DEFAULT 0,
    context_at_compact   INTEGER
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT NOT NULL,
    tool_use_id   TEXT NOT NULL,
    tool_name     TEXT NOT NULL,
    agent_id      TEXT,
    agent_type    TEXT,
    started_at    TEXT NOT NULL,
    completed_at  TEXT,
    duration_ms   INTEGER,
    input_summary TEXT,
    output_bytes  INTEGER,
    error         TEXT,
    succeeded     INTEGER DEFAULT 1,
    skill_name    TEXT,
    skill_type    TEXT,
    UNIQUE(session_id, tool_use_id)
);

CREATE TABLE IF NOT EXISTS config_versions (
    version_hash  TEXT PRIMARY KEY,
    captured_at   TEXT NOT NULL,
    description   TEXT,
    files_snapshot TEXT
);

CREATE TABLE IF NOT EXISTS annotations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL,
    label       TEXT NOT NULL,
    notes       TEXT,
    created_at  TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
    prompt_text,
    error,
    tool_input,
    content=events,
    content_rowid=id,
    tokenize='porter unicode61'
);

CREATE INDEX IF NOT EXISTS idx_events_session     ON events(session_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_events_type        ON events(event_type, timestamp);
CREATE INDEX IF NOT EXISTS idx_events_tool        ON events(tool_name) WHERE tool_name IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_agent       ON events(agent_type) WHERE agent_type IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_tool_use_id ON events(tool_use_id) WHERE tool_use_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_skill       ON events(skill_name) WHERE skill_name IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_sessions_chain     ON sessions(chain_id);
CREATE INDEX IF NOT EXISTS idx_annotations_session ON annotations(session_id);
CREATE INDEX IF NOT EXISTS idx_annotations_label   ON annotations(label);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_dedup ON events(event_hash);
";

// ---------------------------------------------------------------------------
// Required tables (must be kept in sync with SCHEMA_V4_DDL)
// ---------------------------------------------------------------------------

/// The set of tables (and virtual tables) that must be present in the DB for
/// the schema to be considered fully initialised.  `events_fts` is registered
/// in `sqlite_master` as a virtual table with `type = 'table'`, so it is
/// included here alongside the regular tables.
///
/// **Keep in sync with `SCHEMA_V4_DDL`.**  If a new table is added to the DDL
/// it must be added here so that the integrity check in `init_db` catches any
/// divergence between the marker file and the actual DB shape.
const REQUIRED_TABLES: &[&str] = &[
    "events",
    "sessions",
    "tool_calls",
    "config_versions",
    "annotations",
    "events_fts",
];

// ---------------------------------------------------------------------------
// Database initialisation
// ---------------------------------------------------------------------------

/// Initialize the v4 schema on the given (already PRAGMA'd) connection.
///
/// The caller is responsible for opening the connection via
/// [`crate::dbh::open_with_pragmas`] before calling this function.  This
/// avoids the `schema → dbh → schema` import cycle and the redundant
/// double-open that the old path-based signature required.
///
/// Mirrors Python `_init_db` in `ingest.py`:
/// - Runs the DDL only when the marker is absent or its content (stripped)
///   does not equal `"v4"`.
/// - Writes `"v4"` (no trailing newline, matching Python's `write_text`)
///   into `.schema_v4` after successful DDL execution.
///
/// # 3-way decision
///
/// 1. Marker missing **or** marker content != `SCHEMA_VERSION`:
///    → run DDL, write marker (normal first-run path).
/// 2. Marker matches `SCHEMA_VERSION` **and** all [`REQUIRED_TABLES`] are
///    present in `sqlite_master`:
///    → fast-path; nothing more to do.
/// 3. Marker matches `SCHEMA_VERSION` **but** at least one required table is
///    absent from `sqlite_master`:
///    → return `Err` with an actionable message naming the missing tables and
///      the marker path so the operator knows what to inspect and how to
///      recover.
pub fn init_db(conn: &Connection) -> anyhow::Result<()> {
    assert!(
        !REQUIRED_TABLES.is_empty(),
        "REQUIRED_TABLES must not be empty"
    );

    let marker = schema_marker();

    // Mirror Python: `not marker.exists() or marker.read_text().strip() != SCHEMA_VERSION`
    let marker_matches = marker.exists()
        && fs::read_to_string(&marker)
            .unwrap_or_default()
            .trim()
            .eq(SCHEMA_VERSION);

    if !marker_matches {
        // Case 1: marker absent or stale — run DDL and write marker.
        conn.execute_batch(SCHEMA_V4_DDL)
            .context("failed to execute schema DDL")?;

        // Write marker — Python uses `marker.write_text(SCHEMA_VERSION)` which
        // writes the string with no trailing newline on CPython's pathlib.
        if let Some(parent) = marker.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        fs::write(&marker, SCHEMA_VERSION)
            .with_context(|| format!("failed to write schema marker at {}", marker.display()))?;
    } else {
        // Marker matches SCHEMA_VERSION — cross-check against sqlite_master.
        // Fetch the full set of table/view names in one query.
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')",
            )
            .context("failed to prepare sqlite_master query")?;

        let existing: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to query sqlite_master")?
            .filter_map(|r| r.ok())
            .collect();

        let missing: Vec<&str> = REQUIRED_TABLES
            .iter()
            .copied()
            .filter(|t| !existing.contains(*t))
            .collect();

        if !missing.is_empty() {
            // Case 3: marker claims schema is ready, but DB is incomplete.
            let db_display = conn.path().unwrap_or("<unknown>");
            let missing_list = missing.join(", ");
            anyhow::bail!(
                "schema marker at '{}' claims version '{}' but the database '{}' is \
                 missing required tables: {missing_list}. \
                 Either delete the schema marker at '{}' to force re-init, \
                 or run `hooked rebuild` to drop and recreate the database.",
                marker.display(),
                SCHEMA_VERSION,
                db_display,
                marker.display(),
            );
        }
        // Case 2: marker matches and all tables present — fast-path, nothing to do.
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Schema marker helpers
// ---------------------------------------------------------------------------

/// Ensure the `.schema_v4` marker exists and contains `"v4"`.
///
/// Calls [`init_db`] with the canonical DB path if the marker is missing or
/// stale.  Intended for use at CLI startup (mirrors the check in Python's
/// `_init_db` gate).
pub fn ensure_schema_marker() -> anyhow::Result<()> {
    let path = crate::paths::db_path();
    let conn = crate::dbh::open_with_pragmas(&path)?;
    init_db(&conn)
}

/// Read the content of the `.schema_v4` marker file.
///
/// Returns `Ok(Some(version))` if the file exists, `Ok(None)` if it does not,
/// or `Err` if the file exists but cannot be read.
pub fn read_schema_marker() -> anyhow::Result<Option<String>> {
    let marker = schema_marker();
    if !marker.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&marker)
        .with_context(|| format!("failed to read schema marker at {}", marker.display()))?;
    Ok(Some(content.trim().to_owned()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    // -----------------------------------------------------------------------
    // Private home helper (mirrors paths::home() which is private there too)
    // -----------------------------------------------------------------------

    fn dirs_home() -> std::path::PathBuf {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .expect("HOME environment variable must be set")
    }

    // -----------------------------------------------------------------------
    // DDL text-parity test
    // -----------------------------------------------------------------------

    /// Assert that `SCHEMA_V4_DDL` is structurally identical to the `DDL`
    /// string extracted from `~/.claude/telemetry/ingest.py`, after stripping
    /// PRAGMA lines from both sides (T04 reconciliation option b).
    ///
    /// The Python source still contains leading PRAGMA lines; the Rust constant
    /// no longer does (PRAGMAs are applied at connection time via
    /// `dbh::open_with_pragmas`).  We canonicalize both before comparing so
    /// the test stays green without modifying the Python source.
    ///
    /// If `ingest.py` is not present (e.g. CI without the dev env), the test
    /// is silently skipped.  If the file is present but the DDL diverges, the
    /// test fails with a diagnostic showing the first differing byte and a
    /// ±40-byte window from both sides of the canonicalized strings.
    #[test]
    fn rust_ddl_matches_python_ddl_source() {
        /// Canonicalize a DDL string for comparison: strip lines that begin
        /// with `PRAGMA ` (case-insensitive, after leading whitespace) and trim
        /// leading/trailing whitespace from the result.
        ///
        /// Internal blank lines between CREATE blocks are preserved so the test
        /// remains sensitive to real structural drift.  Only cosmetic leading-
        /// and trailing-whitespace differences (e.g. a blank line left between
        /// the last PRAGMA and the first CREATE) are ignored.
        ///
        /// This is the T04 reconciliation "option b" canonicalization: keep
        /// `ingest.py` untouched, normalize the comparison instead.
        fn canonicalize_ddl(ddl: &str) -> String {
            ddl.lines()
                .filter(|line| {
                    !line.trim_start().to_ascii_uppercase().starts_with("PRAGMA ")
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned()
        }

        let py_path = dirs_home().join(".claude/telemetry/ingest.py");
        if !py_path.exists() {
            eprintln!("skipping: {} not present", py_path.display());
            return;
        }

        let py_src = std::fs::read_to_string(&py_path).expect("read ingest.py");

        let start_marker = "DDL = \"\"\"";
        let end_marker = "\"\"\"";

        let start = py_src
            .find(start_marker)
            .expect("ingest.py has no `DDL = \"\"\"`");
        let after_open = start + start_marker.len();
        let end = py_src[after_open..]
            .find(end_marker)
            .expect("ingest.py `DDL` has no closing triple-quote")
            + after_open;

        let py_ddl = &py_src[after_open..end];

        // Canonicalize both sides: strip PRAGMA lines and trim outer
        // whitespace (T04 reconciliation option b).
        let py_canon = canonicalize_ddl(py_ddl);
        let rs_canon = canonicalize_ddl(SCHEMA_V4_DDL);

        if py_canon != rs_canon {
            // Diagnostic: show the first differing byte and a ±40-byte window
            // from both canonicalized strings.  Note that byte positions refer
            // to the PRAGMA-stripped DDL, not the raw Python source.
            let first_diff = py_canon
                .as_bytes()
                .iter()
                .zip(rs_canon.as_bytes().iter())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| py_canon.len().min(rs_canon.len()));
            let lo = first_diff.saturating_sub(40);
            let hi_py = (first_diff + 40).min(py_canon.len());
            let hi_rs = (first_diff + 40).min(rs_canon.len());
            panic!(
                "DDL divergence at byte {first_diff} \
                 (comparison performed on canonicalized DDL — PRAGMA lines stripped and outer whitespace trimmed; T04 reconciliation option b)\n\
                 Python: {:?}\n\
                 Rust:   {:?}\n\
                 (py_canon.len={}, rs_canon.len={})",
                &py_canon[lo..hi_py],
                &rs_canon[lo..hi_rs],
                py_canon.len(),
                rs_canon.len(),
            );
        }
    }

    #[test]
    fn init_db_creates_expected_tables() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let db_path = tmp.path().join("test.db");

        // Point schema_marker at a tempdir path so init_db doesn't touch real FS.
        // We test init_db in isolation by using a custom marker-aware wrapper.
        // Since init_db uses crate::paths::schema_marker() (the real path),
        // we exercise the DDL portion directly to avoid mutating production state.
        let conn = Connection::open(&db_path).expect("failed to open DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("DDL failed");

        // Verify core tables exist.
        for table in &[
            "events",
            "sessions",
            "tool_calls",
            "config_versions",
            "annotations",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |row| row.get(0),
                )
                .unwrap_or_else(|e| panic!("failed to query for table {table}: {e}"));
            assert_eq!(count, 1, "table '{table}' not found in schema");
        }
    }

    #[test]
    fn init_db_creates_expected_indexes() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let db_path = tmp.path().join("test.db");
        let conn = Connection::open(&db_path).expect("failed to open DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("DDL failed");

        let expected_indexes = [
            "idx_events_session",
            "idx_events_type",
            "idx_events_tool",
            "idx_events_agent",
            "idx_events_tool_use_id",
            "idx_events_skill",
            "idx_sessions_chain",
            "idx_annotations_session",
            "idx_annotations_label",
            "idx_events_dedup",
        ];

        for idx in &expected_indexes {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    rusqlite::params![idx],
                    |row| row.get(0),
                )
                .unwrap_or_else(|e| panic!("failed to query for index {idx}: {e}"));
            assert_eq!(count, 1, "index '{idx}' not found in schema");
        }
    }

    #[test]
    fn schema_marker_content_is_schema_version() {
        // Mirror Python: marker.write_text(SCHEMA_VERSION) → content is "v4" (no newline).
        assert_eq!(SCHEMA_VERSION, "v4");
    }

    /// Regression guard for the upstream Python `_init_db` marker bug:
    /// when the marker file is present and matches `SCHEMA_VERSION` but the
    /// actual SQLite DB is missing required tables, `init_db` must return an
    /// explicit `Err` rather than silently succeeding or re-running DDL.
    #[test]
    fn init_db_errors_when_marker_present_but_tables_missing() {
        let fake_home_dir = tempfile::tempdir().expect("failed to create tempdir for fake home");
        let fake_home = fake_home_dir.path().to_str().expect("tempdir path is UTF-8");

        // Redirect HOME so schema_marker() points into our tempdir.
        crate::test_utils::with_fake_home(fake_home, || {
            // Create the telemetry directory tree under fake $HOME.
            let telemetry_dir = fake_home_dir
                .path()
                .join(".claude")
                .join("telemetry");
            std::fs::create_dir_all(&telemetry_dir)
                .expect("failed to create fake telemetry dir");

            // Write the marker file with the current SCHEMA_VERSION.
            let marker_path = telemetry_dir.join(".schema_v4");
            std::fs::write(&marker_path, SCHEMA_VERSION)
                .expect("failed to write fake marker");

            // Create an empty SQLite file — no DDL applied, so no tables exist.
            let db_dir = tempfile::tempdir().expect("failed to create tempdir for db");
            let db_path = db_dir.path().join("empty.db");
            // Open a connection (this creates an empty SQLite DB with no tables).
            let conn = Connection::open(&db_path).expect("failed to open empty DB");

            // init_db must return Err because the marker is present but tables are missing.
            let result = init_db(&conn);
            assert!(
                result.is_err(),
                "expected init_db to return Err when marker is present but tables are missing"
            );

            let err_msg = format!("{:#}", result.unwrap_err());
            assert!(
                err_msg.contains("missing required tables"),
                "error message should contain 'missing required tables', got: {err_msg}"
            );
            // Verify that at least one of the required table names is mentioned.
            let names_any = REQUIRED_TABLES
                .iter()
                .any(|t| err_msg.contains(t));
            assert!(
                names_any,
                "error message should name at least one required table, got: {err_msg}"
            );
        });
    }
}
