//! Phase 2 parity diff tooling.
//!
//! Compares two SQLite databases produced by the Python `ingest.py` reference
//! implementation and the Rust `hooked` ingest over the same JSONL fixture.
//!
//! ## Table strategies
//!
//! | Table            | Natural join key                  | Special fields                                      |
//! |------------------|-----------------------------------|-----------------------------------------------------|
//! | `events`         | `event_hash`                      | `raw_payload` → structural JSON; `timestamp` → normalized |
//! | `sessions`       | `session_id`                      | `chain_id` → topology isomorphism (not UUID equality) |
//! | `tool_calls`     | `(session_id, tool_use_id)`       | `started_at`, `completed_at` → timestamp normalization |
//! | `config_versions`| `version_hash`                    | direct equality                                     |
//! | `annotations`    | `(session_id, label, created_at)` | both sides often empty — treat as OK                |
//! | `events_fts`     | n/a                               | row-count only (FTS5 internal layout varies)        |
//!
//! ## Chain topology isomorphism
//!
//! `chain_id` is a UUID assigned during ingest; it may differ between Python
//! and Rust even when the chain structure is identical.  Instead of comparing
//! UUIDs directly we compare the *topology*: a directed graph where each node
//! is a `session_id` and each edge is `(parent_session_id → session_id)`.
//!
//! Two graphs are **isomorphic for our purposes** iff for every session present
//! in both databases, its relative parent/child relationship is identical.
//! Concretely: we build a map `session_id → Option<parent_session_id>` from
//! each database and assert they agree on all sessions present in both.
//!
//! UUIDs that appear in `chain_id` but differ between databases are **not**
//! reported as divergence; only structural parent/child differences are.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Context;
use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Top-level report returned by [`diff_databases`].
#[derive(Debug, Default)]
pub struct ParityReport {
    /// Tables that were identical (or equivalent) in both databases.
    pub tables_ok: Vec<String>,
    /// Tables where divergence was found.
    pub tables_diverged: Vec<TableDiff>,
}

/// Divergence found in one table.
#[derive(Debug)]
pub struct TableDiff {
    /// Table name.
    pub table: String,
    /// Rows present in the Python DB but missing from the Rust DB (by natural key).
    pub only_in_python: Vec<RowSummary>,
    /// Rows present in the Rust DB but missing from the Python DB (by natural key).
    pub only_in_rust: Vec<RowSummary>,
    /// Rows present in both DBs but with one or more differing field values.
    pub field_level_diffs: Vec<RowFieldDiff>,
}

/// Brief description of a row by its natural key.
#[derive(Debug, Clone)]
pub struct RowSummary {
    /// Human-readable natural key (e.g. event_hash, session_id).
    pub key: String,
    /// Short preview of the row content.
    pub preview: String,
}

/// A single field-level difference between two otherwise-matched rows.
#[derive(Debug)]
pub struct RowFieldDiff {
    /// Natural key identifying the row.
    pub key: String,
    /// Field name.
    pub field: String,
    /// Value in the Python DB.
    pub python_value: String,
    /// Value in the Rust DB.
    pub rust_value: String,
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Compare two SQLite databases row-by-row with topology-aware semantics.
///
/// `python_db` is the database produced by Python `ingest.py`.
/// `rust_db` is the database produced by the Rust `hooked` ingest.
///
/// Returns a [`ParityReport`] describing any divergence found.
pub fn diff_databases(python_db: &Path, rust_db: &Path) -> anyhow::Result<ParityReport> {
    let py_conn = Connection::open(python_db)
        .with_context(|| format!("failed to open python DB at {}", python_db.display()))?;
    let rs_conn = Connection::open(rust_db)
        .with_context(|| format!("failed to open rust DB at {}", rust_db.display()))?;

    let mut report = ParityReport::default();

    // --- events -----------------------------------------------------------
    match diff_events(&py_conn, &rs_conn) {
        Ok(Some(diff)) => report.tables_diverged.push(diff),
        Ok(None) => report.tables_ok.push("events".to_string()),
        Err(e) => return Err(e.context("diff_events")),
    }

    // --- sessions ---------------------------------------------------------
    match diff_sessions(&py_conn, &rs_conn) {
        Ok(Some(diff)) => report.tables_diverged.push(diff),
        Ok(None) => report.tables_ok.push("sessions".to_string()),
        Err(e) => return Err(e.context("diff_sessions")),
    }

    // --- tool_calls -------------------------------------------------------
    match diff_tool_calls(&py_conn, &rs_conn) {
        Ok(Some(diff)) => report.tables_diverged.push(diff),
        Ok(None) => report.tables_ok.push("tool_calls".to_string()),
        Err(e) => return Err(e.context("diff_tool_calls")),
    }

    // --- config_versions --------------------------------------------------
    match diff_config_versions(&py_conn, &rs_conn) {
        Ok(Some(diff)) => report.tables_diverged.push(diff),
        Ok(None) => report.tables_ok.push("config_versions".to_string()),
        Err(e) => return Err(e.context("diff_config_versions")),
    }

    // --- annotations ------------------------------------------------------
    match diff_annotations(&py_conn, &rs_conn) {
        Ok(Some(diff)) => report.tables_diverged.push(diff),
        Ok(None) => report.tables_ok.push("annotations".to_string()),
        Err(e) => return Err(e.context("diff_annotations")),
    }

    // --- events_fts (row count only) -------------------------------------
    match diff_events_fts_count(&py_conn, &rs_conn) {
        Ok(Some(diff)) => report.tables_diverged.push(diff),
        Ok(None) => report.tables_ok.push("events_fts".to_string()),
        Err(e) => return Err(e.context("diff_events_fts")),
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// ParityReport helpers
// ---------------------------------------------------------------------------

impl ParityReport {
    /// Returns `true` when no divergence was found in any table.
    pub fn is_ok(&self) -> bool {
        self.tables_diverged.is_empty()
    }

    /// Returns a human-readable summary of the report.
    pub fn summary(&self) -> String {
        let mut out = String::new();

        if self.is_ok() {
            out.push_str("PARITY OK\n");
            out.push_str(&format!(
                "  {} table(s) checked, all identical\n",
                self.tables_ok.len()
            ));
            for t in &self.tables_ok {
                out.push_str(&format!("  [OK] {t}\n"));
            }
            return out;
        }

        out.push_str("PARITY DIVERGED\n");
        if !self.tables_ok.is_empty() {
            out.push_str(&format!("  {} table(s) OK:\n", self.tables_ok.len()));
            for t in &self.tables_ok {
                out.push_str(&format!("  [OK] {t}\n"));
            }
        }

        out.push_str(&format!(
            "  {} table(s) DIVERGED:\n",
            self.tables_diverged.len()
        ));
        for diff in &self.tables_diverged {
            out.push_str(&format!("\n  [DIVERGED] {}\n", diff.table));
            if !diff.only_in_python.is_empty() {
                out.push_str(&format!(
                    "    Only in Python ({}):\n",
                    diff.only_in_python.len()
                ));
                for row in &diff.only_in_python {
                    out.push_str(&format!("      key={} | {}\n", row.key, row.preview));
                }
            }
            if !diff.only_in_rust.is_empty() {
                out.push_str(&format!(
                    "    Only in Rust ({}):\n",
                    diff.only_in_rust.len()
                ));
                for row in &diff.only_in_rust {
                    out.push_str(&format!("      key={} | {}\n", row.key, row.preview));
                }
            }
            if !diff.field_level_diffs.is_empty() {
                out.push_str(&format!(
                    "    Field-level diffs ({}):\n",
                    diff.field_level_diffs.len()
                ));
                for fd in &diff.field_level_diffs {
                    out.push_str(&format!(
                        "      row={} field={}\n        python: {}\n        rust:   {}\n",
                        fd.key, fd.field, fd.python_value, fd.rust_value
                    ));
                }
            }
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Timestamp normalisation
// ---------------------------------------------------------------------------

/// Normalise a timestamp string to `YYYY-MM-DDTHH:MM:SS.ffffff+00:00`.
///
/// Handles the following input forms that Python and Rust may each produce:
/// - `2026-04-24T10:00:00.000Z`        (millisecond, Z suffix)
/// - `2026-04-24T10:00:00.000000Z`     (microsecond, Z suffix)
/// - `2026-04-24T10:00:00.000+00:00`   (millisecond, explicit UTC offset)
/// - `2026-04-24T10:00:00.000000+00:00`(microsecond, explicit UTC offset)
///
/// All are normalised to microsecond precision with explicit `+00:00` suffix.
/// A timestamp that cannot be parsed is returned unchanged (for resilience).
pub fn normalize_timestamp(ts: &str) -> String {
    // Strip trailing Z or +00:00 / -00:00 to get the datetime portion.
    let bare = ts
        .trim_end_matches('Z')
        .trim_end_matches("+00:00")
        .trim_end_matches("-00:00");

    // Split into date/time parts.
    let parts: Vec<&str> = bare.splitn(2, 'T').collect();
    if parts.len() != 2 {
        return ts.to_string();
    }
    let date = parts[0];
    let time = parts[1];

    // Split time into HH:MM:SS and fractional seconds.
    let (hms, frac) = if let Some(dot) = time.find('.') {
        (&time[..dot], &time[dot + 1..])
    } else {
        (time, "")
    };

    // Pad or truncate fractional seconds to exactly 6 digits.
    let frac6 = match frac.len() {
        0 => "000000".to_string(),
        n if n < 6 => format!("{:0<6}", frac),
        _ => frac[..6].to_string(),
    };

    format!("{date}T{hms}.{frac6}+00:00")
}

// ---------------------------------------------------------------------------
// JSON structural comparison
// ---------------------------------------------------------------------------

/// Compare two JSON strings structurally (deep-equal via `serde_json::Value`).
///
/// Returns `true` if both parse to the same value, or if both are `NULL`/empty,
/// or if they are byte-for-byte identical after failing to parse.
fn json_structurally_equal(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) if x == y => true,
        (Some(x), Some(y)) => {
            let va: Result<Value, _> = serde_json::from_str(x);
            let vb: Result<Value, _> = serde_json::from_str(y);
            match (va, vb) {
                (Ok(va), Ok(vb)) => va == vb,
                _ => false,
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// SQLite helpers
// ---------------------------------------------------------------------------

/// Convert a single [`SqlValue`] to its canonical string representation.
///
/// `NULL` becomes `None`; all other types become `Some(string)`.
fn sql_value_to_opt_string(v: SqlValue) -> Option<String> {
    match v {
        SqlValue::Null => None,
        SqlValue::Integer(i) => Some(i.to_string()),
        SqlValue::Real(f) => Some(f.to_string()),
        SqlValue::Text(s) => Some(s),
        SqlValue::Blob(b) => Some(format!("<{} bytes>", b.len())),
    }
}

/// Query all rows from a table as a `Vec<HashMap<String, Option<String>>>`.
///
/// Each column is read as [`SqlValue`] (the polymorphic rusqlite enum) so that
/// INTEGER, REAL, TEXT, BLOB, and NULL columns are all handled without an
/// "Invalid column type" error.
fn fetch_rows(
    conn: &Connection,
    sql: &str,
) -> anyhow::Result<Vec<HashMap<String, Option<String>>>> {
    let mut stmt = conn.prepare(sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let col_count = col_names.len();

    let rows = stmt.query_map([], |row| {
        let mut map = HashMap::new();
        for (i, col) in col_names.iter().enumerate().take(col_count) {
            let v: SqlValue = row.get(i)?;
            map.insert(col.clone(), sql_value_to_opt_string(v));
        }
        Ok(map)
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Count rows in a table.
fn count_rows(conn: &Connection, table: &str) -> anyhow::Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .with_context(|| format!("count rows in {table}"))
}

// ---------------------------------------------------------------------------
// events diff
// ---------------------------------------------------------------------------

fn diff_events(py: &Connection, rs: &Connection) -> anyhow::Result<Option<TableDiff>> {
    // Fetch all events keyed by event_hash.
    // Skip rows with NULL event_hash (they can't participate in natural join).
    let py_rows = fetch_events_map(py)?;
    let rs_rows = fetch_events_map(rs)?;

    let py_keys: HashSet<&String> = py_rows.keys().collect();
    let rs_keys: HashSet<&String> = rs_rows.keys().collect();

    let mut only_in_python: Vec<RowSummary> = py_keys
        .difference(&rs_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(py_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_python.sort_by(|a, b| a.key.cmp(&b.key));

    let mut only_in_rust: Vec<RowSummary> = rs_keys
        .difference(&py_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(rs_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_rust.sort_by(|a, b| a.key.cmp(&b.key));

    let mut field_level_diffs: Vec<RowFieldDiff> = Vec::new();

    // Fields compared with direct string equality (after normalization where needed).
    const DIRECT_FIELDS: &[&str] = &[
        "session_id",
        "event_type",
        "sequence_num",
        "tool_name",
        "tool_use_id",
        "result_size",
        "duration_ms",
        "error",
        "is_interrupt",
        "prompt_text",
        "prompt_length",
        "agent_id",
        "agent_type",
        "source",
        "reason",
        "model",
        "permission_mode",
        "cwd",
        "notification_type",
        "compact_trigger",
        "config_source",
        "config_version",
        "git_branch",
        "git_commit",
        "input_bytes",
        "output_bytes",
        "context_cumulative_bytes",
        "skill_name",
        "skill_type",
        "task_id",
        "task_subject",
        "teammate_name",
        "is_slash_command",
        "stop_hook_active",
    ];

    // Fields treated as JSON (structural equality).
    const JSON_FIELDS: &[&str] = &["raw_payload", "tool_input", "tool_result"];

    // Fields treated as timestamps (normalized before comparison).
    const TIMESTAMP_FIELDS: &[&str] = &["timestamp"];

    for key in py_keys.intersection(&rs_keys) {
        let py_row = py_rows.get(*key).unwrap();
        let rs_row = rs_rows.get(*key).unwrap();

        // Timestamp fields
        for &field in TIMESTAMP_FIELDS {
            let py_val = py_row.get(field).and_then(|v| v.as_deref());
            let rs_val = rs_row.get(field).and_then(|v| v.as_deref());
            let py_norm = py_val.map(normalize_timestamp);
            let rs_norm = rs_val.map(normalize_timestamp);
            if py_norm != rs_norm {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: py_norm.unwrap_or_default(),
                    rust_value: rs_norm.unwrap_or_default(),
                });
            }
        }

        // JSON fields
        for &field in JSON_FIELDS {
            let py_val = py_row.get(field).and_then(|v| v.as_deref());
            let rs_val = rs_row.get(field).and_then(|v| v.as_deref());
            if !json_structurally_equal(py_val, rs_val) {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: py_val.unwrap_or("NULL").to_string(),
                    rust_value: rs_val.unwrap_or("NULL").to_string(),
                });
            }
        }

        // Direct equality fields
        for &field in DIRECT_FIELDS {
            let py_val = py_row.get(field);
            let rs_val = rs_row.get(field);
            if py_val != rs_val {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: fmt_opt(py_val.and_then(|v| v.as_deref())),
                    rust_value: fmt_opt(rs_val.and_then(|v| v.as_deref())),
                });
            }
        }
    }

    field_level_diffs.sort_by(|a, b| a.key.cmp(&b.key).then(a.field.cmp(&b.field)));

    if only_in_python.is_empty() && only_in_rust.is_empty() && field_level_diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(TableDiff {
            table: "events".to_string(),
            only_in_python,
            only_in_rust,
            field_level_diffs,
        }))
    }
}

/// Fetch events as `event_hash → row map`.  Rows with NULL event_hash are skipped.
fn fetch_events_map(
    conn: &Connection,
) -> anyhow::Result<HashMap<String, HashMap<String, Option<String>>>> {
    let rows = fetch_rows(conn, "SELECT * FROM events WHERE event_hash IS NOT NULL")?;
    let mut map = HashMap::new();
    for row in rows {
        if let Some(Some(hash)) = row.get("event_hash") {
            map.insert(hash.clone(), row);
        }
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// sessions diff
// ---------------------------------------------------------------------------

fn diff_sessions(py: &Connection, rs: &Connection) -> anyhow::Result<Option<TableDiff>> {
    let py_rows = fetch_sessions_map(py)?;
    let rs_rows = fetch_sessions_map(rs)?;

    let py_keys: HashSet<&String> = py_rows.keys().collect();
    let rs_keys: HashSet<&String> = rs_rows.keys().collect();

    let mut only_in_python: Vec<RowSummary> = py_keys
        .difference(&rs_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(py_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_python.sort_by(|a, b| a.key.cmp(&b.key));

    let mut only_in_rust: Vec<RowSummary> = rs_keys
        .difference(&py_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(rs_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_rust.sort_by(|a, b| a.key.cmp(&b.key));

    // Fields compared directly (excluding chain_id which uses topology check).
    const DIRECT_FIELDS: &[&str] = &[
        "source",
        "end_reason",
        "model",
        "permission_mode",
        "cwd",
        "config_version",
        "git_branch",
        "git_commit",
        "total_events",
        "total_tool_calls",
        "total_failures",
        "total_prompts",
        "total_subagents",
        "total_tasks",
        "compaction_count",
        "auto_compact_count",
        "permission_prompts",
        "context_total_bytes",
        "context_at_compact",
    ];

    // Timestamp fields in sessions.
    const TIMESTAMP_FIELDS: &[&str] = &["started_at", "ended_at"];

    let mut field_level_diffs: Vec<RowFieldDiff> = Vec::new();

    // Build parent-graph topology for isomorphism check.
    let py_topology = build_session_topology(&py_rows);
    let rs_topology = build_session_topology(&rs_rows);

    for key in py_keys.intersection(&rs_keys) {
        let py_row = py_rows.get(*key).unwrap();
        let rs_row = rs_rows.get(*key).unwrap();

        // Topology check: compare parent_session_id (not chain_id UUID).
        let py_parent = py_topology.get(*key).and_then(|p| p.as_deref());
        let rs_parent = rs_topology.get(*key).and_then(|p| p.as_deref());
        if py_parent != rs_parent {
            field_level_diffs.push(RowFieldDiff {
                key: key.to_string(),
                field: "parent_session_id (topology)".to_string(),
                python_value: fmt_opt(py_parent),
                rust_value: fmt_opt(rs_parent),
            });
        }
        // chain_id: NOT compared by UUID value — topology above handles it.

        // Timestamp fields
        for &field in TIMESTAMP_FIELDS {
            let py_val = py_row.get(field).and_then(|v| v.as_deref());
            let rs_val = rs_row.get(field).and_then(|v| v.as_deref());
            let py_norm = py_val.map(normalize_timestamp);
            let rs_norm = rs_val.map(normalize_timestamp);
            if py_norm != rs_norm {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: py_norm.unwrap_or_default(),
                    rust_value: rs_norm.unwrap_or_default(),
                });
            }
        }

        // Direct equality fields
        for &field in DIRECT_FIELDS {
            let py_val = py_row.get(field);
            let rs_val = rs_row.get(field);
            if py_val != rs_val {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: fmt_opt(py_val.and_then(|v| v.as_deref())),
                    rust_value: fmt_opt(rs_val.and_then(|v| v.as_deref())),
                });
            }
        }
    }

    field_level_diffs.sort_by(|a, b| a.key.cmp(&b.key).then(a.field.cmp(&b.field)));

    if only_in_python.is_empty() && only_in_rust.is_empty() && field_level_diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(TableDiff {
            table: "sessions".to_string(),
            only_in_python,
            only_in_rust,
            field_level_diffs,
        }))
    }
}

/// Fetch sessions as `session_id → row map`.
fn fetch_sessions_map(
    conn: &Connection,
) -> anyhow::Result<HashMap<String, HashMap<String, Option<String>>>> {
    let rows = fetch_rows(conn, "SELECT * FROM sessions")?;
    let mut map = HashMap::new();
    for row in rows {
        if let Some(Some(sid)) = row.get("session_id") {
            map.insert(sid.clone(), row);
        }
    }
    Ok(map)
}

/// Build a `session_id → parent_session_id` topology map.
///
/// This is the basis for chain isomorphism checking.  Two databases are
/// chain-equivalent if for every common `session_id` the `parent_session_id`
/// value agrees (both `None`, or both `Some(same_session_id)`).
///
/// Note: `chain_id` UUID values are intentionally excluded from this map —
/// they are assigned independently by each implementation and are NOT expected
/// to be equal.
fn build_session_topology(
    sessions: &HashMap<String, HashMap<String, Option<String>>>,
) -> HashMap<String, Option<String>> {
    sessions
        .iter()
        .map(|(sid, row)| {
            let parent = row.get("parent_session_id").and_then(|v| v.clone());
            (sid.clone(), parent)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// tool_calls diff
// ---------------------------------------------------------------------------

fn diff_tool_calls(py: &Connection, rs: &Connection) -> anyhow::Result<Option<TableDiff>> {
    let py_rows = fetch_tool_calls_map(py)?;
    let rs_rows = fetch_tool_calls_map(rs)?;

    let py_keys: HashSet<&String> = py_rows.keys().collect();
    let rs_keys: HashSet<&String> = rs_rows.keys().collect();

    let mut only_in_python: Vec<RowSummary> = py_keys
        .difference(&rs_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(py_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_python.sort_by(|a, b| a.key.cmp(&b.key));

    let mut only_in_rust: Vec<RowSummary> = rs_keys
        .difference(&py_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(rs_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_rust.sort_by(|a, b| a.key.cmp(&b.key));

    const DIRECT_FIELDS: &[&str] = &[
        "tool_name",
        "agent_id",
        "agent_type",
        "duration_ms",
        "input_summary",
        "output_bytes",
        "error",
        "succeeded",
        "skill_name",
        "skill_type",
    ];
    const TIMESTAMP_FIELDS: &[&str] = &["started_at", "completed_at"];

    let mut field_level_diffs: Vec<RowFieldDiff> = Vec::new();

    for key in py_keys.intersection(&rs_keys) {
        let py_row = py_rows.get(*key).unwrap();
        let rs_row = rs_rows.get(*key).unwrap();

        for &field in TIMESTAMP_FIELDS {
            let py_val = py_row.get(field).and_then(|v| v.as_deref());
            let rs_val = rs_row.get(field).and_then(|v| v.as_deref());
            let py_norm = py_val.map(normalize_timestamp);
            let rs_norm = rs_val.map(normalize_timestamp);
            if py_norm != rs_norm {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: py_norm.unwrap_or_default(),
                    rust_value: rs_norm.unwrap_or_default(),
                });
            }
        }

        for &field in DIRECT_FIELDS {
            let py_val = py_row.get(field);
            let rs_val = rs_row.get(field);
            if py_val != rs_val {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: fmt_opt(py_val.and_then(|v| v.as_deref())),
                    rust_value: fmt_opt(rs_val.and_then(|v| v.as_deref())),
                });
            }
        }
    }

    field_level_diffs.sort_by(|a, b| a.key.cmp(&b.key).then(a.field.cmp(&b.field)));

    if only_in_python.is_empty() && only_in_rust.is_empty() && field_level_diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(TableDiff {
            table: "tool_calls".to_string(),
            only_in_python,
            only_in_rust,
            field_level_diffs,
        }))
    }
}

/// Fetch tool_calls as `"<session_id>/<tool_use_id>" → row map`.
fn fetch_tool_calls_map(
    conn: &Connection,
) -> anyhow::Result<HashMap<String, HashMap<String, Option<String>>>> {
    let rows = fetch_rows(conn, "SELECT * FROM tool_calls")?;
    let mut map = HashMap::new();
    for row in rows {
        let sid = row
            .get("session_id")
            .and_then(|v| v.as_deref())
            .unwrap_or("")
            .to_string();
        let tid = row
            .get("tool_use_id")
            .and_then(|v| v.as_deref())
            .unwrap_or("")
            .to_string();
        let composite_key = format!("{sid}/{tid}");
        map.insert(composite_key, row);
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// config_versions diff
// ---------------------------------------------------------------------------

fn diff_config_versions(py: &Connection, rs: &Connection) -> anyhow::Result<Option<TableDiff>> {
    let py_rows = fetch_config_versions_map(py)?;
    let rs_rows = fetch_config_versions_map(rs)?;

    let py_keys: HashSet<&String> = py_rows.keys().collect();
    let rs_keys: HashSet<&String> = rs_rows.keys().collect();

    let mut only_in_python: Vec<RowSummary> = py_keys
        .difference(&rs_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(py_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_python.sort_by(|a, b| a.key.cmp(&b.key));

    let mut only_in_rust: Vec<RowSummary> = rs_keys
        .difference(&py_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(rs_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_rust.sort_by(|a, b| a.key.cmp(&b.key));

    const DIRECT_FIELDS: &[&str] = &["description", "files_snapshot"];
    const TIMESTAMP_FIELDS: &[&str] = &["captured_at"];

    let mut field_level_diffs: Vec<RowFieldDiff> = Vec::new();

    for key in py_keys.intersection(&rs_keys) {
        let py_row = py_rows.get(*key).unwrap();
        let rs_row = rs_rows.get(*key).unwrap();

        for &field in TIMESTAMP_FIELDS {
            let py_val = py_row.get(field).and_then(|v| v.as_deref());
            let rs_val = rs_row.get(field).and_then(|v| v.as_deref());
            let py_norm = py_val.map(normalize_timestamp);
            let rs_norm = rs_val.map(normalize_timestamp);
            if py_norm != rs_norm {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: py_norm.unwrap_or_default(),
                    rust_value: rs_norm.unwrap_or_default(),
                });
            }
        }

        for &field in DIRECT_FIELDS {
            let py_val = py_row.get(field);
            let rs_val = rs_row.get(field);
            if py_val != rs_val {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: fmt_opt(py_val.and_then(|v| v.as_deref())),
                    rust_value: fmt_opt(rs_val.and_then(|v| v.as_deref())),
                });
            }
        }
    }

    field_level_diffs.sort_by(|a, b| a.key.cmp(&b.key).then(a.field.cmp(&b.field)));

    if only_in_python.is_empty() && only_in_rust.is_empty() && field_level_diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(TableDiff {
            table: "config_versions".to_string(),
            only_in_python,
            only_in_rust,
            field_level_diffs,
        }))
    }
}

fn fetch_config_versions_map(
    conn: &Connection,
) -> anyhow::Result<HashMap<String, HashMap<String, Option<String>>>> {
    let rows = fetch_rows(conn, "SELECT * FROM config_versions")?;
    let mut map = HashMap::new();
    for row in rows {
        if let Some(Some(hash)) = row.get("version_hash") {
            map.insert(hash.clone(), row);
        }
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// annotations diff
// ---------------------------------------------------------------------------

fn diff_annotations(py: &Connection, rs: &Connection) -> anyhow::Result<Option<TableDiff>> {
    // Natural key: composite (session_id, label, created_at) — no stable surrogate.
    // If both sides are empty, report OK immediately.
    let py_count = count_rows(py, "annotations")?;
    let rs_count = count_rows(rs, "annotations")?;

    if py_count == 0 && rs_count == 0 {
        return Ok(None);
    }

    let py_rows = fetch_annotations_map(py)?;
    let rs_rows = fetch_annotations_map(rs)?;

    let py_keys: HashSet<&String> = py_rows.keys().collect();
    let rs_keys: HashSet<&String> = rs_rows.keys().collect();

    let mut only_in_python: Vec<RowSummary> = py_keys
        .difference(&rs_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(py_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_python.sort_by(|a, b| a.key.cmp(&b.key));

    let mut only_in_rust: Vec<RowSummary> = rs_keys
        .difference(&py_keys)
        .map(|k| RowSummary {
            key: k.to_string(),
            preview: row_preview(rs_rows.get(*k).unwrap()),
        })
        .collect();
    only_in_rust.sort_by(|a, b| a.key.cmp(&b.key));

    let mut field_level_diffs: Vec<RowFieldDiff> = Vec::new();

    for key in py_keys.intersection(&rs_keys) {
        let py_row = py_rows.get(*key).unwrap();
        let rs_row = rs_rows.get(*key).unwrap();
        {
            let field = "notes";
            let py_val = py_row.get(field);
            let rs_val = rs_row.get(field);
            if py_val != rs_val {
                field_level_diffs.push(RowFieldDiff {
                    key: key.to_string(),
                    field: field.to_string(),
                    python_value: fmt_opt(py_val.and_then(|v| v.as_deref())),
                    rust_value: fmt_opt(rs_val.and_then(|v| v.as_deref())),
                });
            }
        }
    }

    field_level_diffs.sort_by(|a, b| a.key.cmp(&b.key).then(a.field.cmp(&b.field)));

    if only_in_python.is_empty() && only_in_rust.is_empty() && field_level_diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(TableDiff {
            table: "annotations".to_string(),
            only_in_python,
            only_in_rust,
            field_level_diffs,
        }))
    }
}

fn fetch_annotations_map(
    conn: &Connection,
) -> anyhow::Result<HashMap<String, HashMap<String, Option<String>>>> {
    let rows = fetch_rows(conn, "SELECT * FROM annotations")?;
    let mut map = HashMap::new();
    for row in rows {
        let sid = row
            .get("session_id")
            .and_then(|v| v.as_deref())
            .unwrap_or("")
            .to_string();
        let label = row
            .get("label")
            .and_then(|v| v.as_deref())
            .unwrap_or("")
            .to_string();
        let created = row
            .get("created_at")
            .and_then(|v| v.as_deref())
            .unwrap_or("")
            .to_string();
        let key = format!("{sid}|{label}|{created}");
        map.insert(key, row);
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// events_fts diff (row count only)
// ---------------------------------------------------------------------------

fn diff_events_fts_count(py: &Connection, rs: &Connection) -> anyhow::Result<Option<TableDiff>> {
    // events_fts is a virtual FTS5 table; internal storage varies between SQLite
    // builds and isn't meaningful to compare row-by-row.  We check only that
    // the row counts match.
    let py_count = py
        .query_row("SELECT COUNT(*) FROM events_fts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0);
    let rs_count = rs
        .query_row("SELECT COUNT(*) FROM events_fts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0);

    if py_count == rs_count {
        return Ok(None);
    }

    let diff = TableDiff {
        table: "events_fts".to_string(),
        only_in_python: if py_count > rs_count {
            vec![RowSummary {
                key: "count".to_string(),
                preview: format!(
                    "python={py_count} rust={rs_count} (diff: {})",
                    py_count - rs_count
                ),
            }]
        } else {
            vec![]
        },
        only_in_rust: if rs_count > py_count {
            vec![RowSummary {
                key: "count".to_string(),
                preview: format!(
                    "python={py_count} rust={rs_count} (diff: {})",
                    rs_count - py_count
                ),
            }]
        } else {
            vec![]
        },
        field_level_diffs: vec![],
    };

    Ok(Some(diff))
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn fmt_opt(v: Option<&str>) -> String {
    match v {
        None => "NULL".to_string(),
        Some(s) => s.to_string(),
    }
}

/// Build a short human-readable preview of a row for display in diff output.
fn row_preview(row: &HashMap<String, Option<String>>) -> String {
    // Show a handful of identifying fields.
    let candidates = &[
        "event_type",
        "session_id",
        "tool_name",
        "timestamp",
        "source",
        "label",
    ];
    let parts: Vec<String> = candidates
        .iter()
        .filter_map(|k| {
            row.get(*k)
                .and_then(|v| v.as_deref())
                .map(|v| format!("{k}={v}"))
        })
        .take(4)
        .collect();
    if parts.is_empty() {
        "(no preview fields)".to_string()
    } else {
        parts.join(", ")
    }
}

// ---------------------------------------------------------------------------
// DB initialisation helper (for tests and ingest_one binary)
// ---------------------------------------------------------------------------

/// Open (or create) a database and apply the v4 DDL.
///
/// Convenience wrapper around [`crate::schema::SCHEMA_V4_DDL`].
pub fn open_db(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("open_db: cannot open {}", path.display()))?;
    conn.execute_batch(crate::schema::SCHEMA_V4_DDL)
        .context("open_db: DDL failed")?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use rusqlite::params;

    // -----------------------------------------------------------------------
    // Helper: create an in-memory DB with the v4 schema applied.
    // -----------------------------------------------------------------------

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(crate::schema::SCHEMA_V4_DDL)
            .expect("DDL");
        conn
    }

    // -----------------------------------------------------------------------
    // Helper: insert a minimal event row.
    // -----------------------------------------------------------------------

    fn insert_event(
        conn: &Connection,
        session_id: &str,
        event_type: &str,
        event_hash: &str,
        timestamp: &str,
        raw_payload: Option<&str>,
        extra_fields: &[(&str, &str)],
    ) {
        let raw_payload_val = raw_payload.unwrap_or("{}");
        conn.execute(
            "INSERT INTO events (session_id, event_type, event_hash, timestamp, raw_payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, event_type, event_hash, timestamp, raw_payload_val],
        )
        .expect("insert event");

        for (col, val) in extra_fields {
            conn.execute(
                &format!("UPDATE events SET {col} = ?1 WHERE event_hash = ?2"),
                params![val, event_hash],
            )
            .expect("update field");
        }
    }

    // -----------------------------------------------------------------------
    // Helper: insert a minimal session row.
    // -----------------------------------------------------------------------

    fn insert_session(
        conn: &Connection,
        session_id: &str,
        chain_id: &str,
        parent_session_id: Option<&str>,
        started_at: &str,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, chain_id, parent_session_id, started_at) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, chain_id, parent_session_id, started_at],
        )
        .expect("insert session");
    }

    // -----------------------------------------------------------------------
    // identical_dbs_pass
    // -----------------------------------------------------------------------

    #[test]
    fn identical_dbs_pass() {
        let py = in_memory_db();
        let rs = in_memory_db();

        insert_event(
            &py,
            "s1",
            "SessionStart",
            "hash001",
            "2026-04-24T10:00:00.000000+00:00",
            None,
            &[],
        );
        insert_event(
            &rs,
            "s1",
            "SessionStart",
            "hash001",
            "2026-04-24T10:00:00.000000+00:00",
            None,
            &[],
        );

        // Write to temp files since diff_databases takes paths.
        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        // Re-open as file-backed DBs.
        let py_file = Connection::open(py_tmp.path()).unwrap();
        py_file.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &py_file,
            "s1",
            "SessionStart",
            "hash001",
            "2026-04-24T10:00:00.000000+00:00",
            None,
            &[],
        );

        let rs_file = Connection::open(rs_tmp.path()).unwrap();
        rs_file.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &rs_file,
            "s1",
            "SessionStart",
            "hash001",
            "2026-04-24T10:00:00.000000+00:00",
            None,
            &[],
        );

        drop(py_file);
        drop(rs_file);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        assert!(report.is_ok(), "Expected OK, got:\n{}", report.summary());
    }

    // -----------------------------------------------------------------------
    // missing_row_in_rust
    // -----------------------------------------------------------------------

    #[test]
    fn missing_row_in_rust() {
        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        let py = Connection::open(py_tmp.path()).unwrap();
        py.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &py,
            "s1",
            "SessionStart",
            "hash001",
            "2026-04-24T10:00:00.000000+00:00",
            None,
            &[],
        );

        let rs = Connection::open(rs_tmp.path()).unwrap();
        rs.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        // Rust DB has no events.

        drop(py);
        drop(rs);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        assert!(!report.is_ok());
        let events_diff = report
            .tables_diverged
            .iter()
            .find(|d| d.table == "events")
            .unwrap();
        assert_eq!(events_diff.only_in_python.len(), 1);
        assert_eq!(events_diff.only_in_python[0].key, "hash001");
    }

    // -----------------------------------------------------------------------
    // extra_row_in_rust
    // -----------------------------------------------------------------------

    #[test]
    fn extra_row_in_rust() {
        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        let py = Connection::open(py_tmp.path()).unwrap();
        py.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();

        let rs = Connection::open(rs_tmp.path()).unwrap();
        rs.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &rs,
            "s1",
            "SessionStart",
            "hash001",
            "2026-04-24T10:00:00.000000+00:00",
            None,
            &[],
        );

        drop(py);
        drop(rs);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        assert!(!report.is_ok());
        let events_diff = report
            .tables_diverged
            .iter()
            .find(|d| d.table == "events")
            .unwrap();
        assert_eq!(events_diff.only_in_rust.len(), 1);
        assert_eq!(events_diff.only_in_rust[0].key, "hash001");
    }

    // -----------------------------------------------------------------------
    // field_diff_on_event
    // -----------------------------------------------------------------------

    #[test]
    fn field_diff_on_event() {
        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        let py = Connection::open(py_tmp.path()).unwrap();
        py.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &py,
            "s1",
            "PreToolUse",
            "hash002",
            "2026-04-24T10:00:01.000000+00:00",
            None,
            &[("context_cumulative_bytes", "100")],
        );

        let rs = Connection::open(rs_tmp.path()).unwrap();
        rs.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &rs,
            "s1",
            "PreToolUse",
            "hash002",
            "2026-04-24T10:00:01.000000+00:00",
            None,
            &[("context_cumulative_bytes", "200")],
        );

        drop(py);
        drop(rs);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        assert!(!report.is_ok());
        let events_diff = report
            .tables_diverged
            .iter()
            .find(|d| d.table == "events")
            .unwrap();
        assert_eq!(events_diff.field_level_diffs.len(), 1);
        assert_eq!(
            events_diff.field_level_diffs[0].field,
            "context_cumulative_bytes"
        );
        assert_eq!(events_diff.field_level_diffs[0].python_value, "100");
        assert_eq!(events_diff.field_level_diffs[0].rust_value, "200");
    }

    // -----------------------------------------------------------------------
    // chain_topology_equivalent_despite_uuid_diff
    // -----------------------------------------------------------------------

    #[test]
    fn chain_topology_equivalent_despite_uuid_diff() {
        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        let py = Connection::open(py_tmp.path()).unwrap();
        py.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        // Python assigns chain_id = "chain-py-uuid"
        insert_session(
            &py,
            "sess-A",
            "chain-py-uuid",
            None,
            "2026-04-24T10:00:00.000000+00:00",
        );
        insert_session(
            &py,
            "sess-B",
            "chain-py-uuid",
            Some("sess-A"),
            "2026-04-24T10:00:01.000000+00:00",
        );

        let rs = Connection::open(rs_tmp.path()).unwrap();
        rs.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        // Rust assigns a different chain_id UUID, but same parent graph.
        insert_session(
            &rs,
            "sess-A",
            "chain-rs-uuid",
            None,
            "2026-04-24T10:00:00.000000+00:00",
        );
        insert_session(
            &rs,
            "sess-B",
            "chain-rs-uuid",
            Some("sess-A"),
            "2026-04-24T10:00:01.000000+00:00",
        );

        drop(py);
        drop(rs);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        // sessions table should be OK (chain_id UUID difference is ignored;
        // parent_session_id topology is identical).
        let sessions_diff = report
            .tables_diverged
            .iter()
            .find(|d| d.table == "sessions");
        assert!(
            sessions_diff.is_none(),
            "Expected sessions OK, got diffs: {:?}",
            sessions_diff
        );
    }

    // -----------------------------------------------------------------------
    // chain_topology_diverged
    // -----------------------------------------------------------------------

    #[test]
    fn chain_topology_diverged() {
        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        let py = Connection::open(py_tmp.path()).unwrap();
        py.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        // Python: sess-B's parent is sess-A
        insert_session(
            &py,
            "sess-A",
            "chain-1",
            None,
            "2026-04-24T10:00:00.000000+00:00",
        );
        insert_session(
            &py,
            "sess-B",
            "chain-1",
            Some("sess-A"),
            "2026-04-24T10:00:01.000000+00:00",
        );

        let rs = Connection::open(rs_tmp.path()).unwrap();
        rs.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        // Rust: sess-B has no parent (different topology!)
        insert_session(
            &rs,
            "sess-A",
            "chain-1",
            None,
            "2026-04-24T10:00:00.000000+00:00",
        );
        insert_session(
            &rs,
            "sess-B",
            "chain-1",
            None,
            "2026-04-24T10:00:01.000000+00:00",
        );

        drop(py);
        drop(rs);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        let sessions_diff = report
            .tables_diverged
            .iter()
            .find(|d| d.table == "sessions");
        assert!(sessions_diff.is_some(), "Expected sessions diverged");
        let diff = sessions_diff.unwrap();
        let topology_diff = diff
            .field_level_diffs
            .iter()
            .find(|f| f.key == "sess-B" && f.field.contains("parent_session_id"));
        assert!(
            topology_diff.is_some(),
            "Expected parent_session_id topology diff for sess-B"
        );
    }

    // -----------------------------------------------------------------------
    // timestamp_precision_normalized
    // -----------------------------------------------------------------------

    #[test]
    fn timestamp_precision_normalized() {
        // "2026-04-24T10:00:00.000Z" vs "2026-04-24T10:00:00.000000+00:00"
        // should be treated as equal after normalization.
        let ts1 = normalize_timestamp("2026-04-24T10:00:00.000Z");
        let ts2 = normalize_timestamp("2026-04-24T10:00:00.000000+00:00");
        assert_eq!(
            ts1, ts2,
            "millisecond Z and microsecond +00:00 should normalize identically"
        );

        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        let py = Connection::open(py_tmp.path()).unwrap();
        py.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &py,
            "s1",
            "SessionStart",
            "hash_ts",
            "2026-04-24T10:00:00.000Z",
            None,
            &[],
        );

        let rs = Connection::open(rs_tmp.path()).unwrap();
        rs.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &rs,
            "s1",
            "SessionStart",
            "hash_ts",
            "2026-04-24T10:00:00.000000+00:00",
            None,
            &[],
        );

        drop(py);
        drop(rs);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        assert!(
            report.is_ok(),
            "Timestamp variants should be treated as equal:\n{}",
            report.summary()
        );
    }

    // -----------------------------------------------------------------------
    // raw_payload_structural_equality
    // -----------------------------------------------------------------------

    #[test]
    fn raw_payload_structural_equality() {
        // Two JSON strings differing in key order but structurally equal.
        let py_payload = r#"{"b":2,"a":1}"#;
        let rs_payload = r#"{"a":1,"b":2}"#;

        let py_tmp = tempfile::NamedTempFile::new().unwrap();
        let rs_tmp = tempfile::NamedTempFile::new().unwrap();

        let py = Connection::open(py_tmp.path()).unwrap();
        py.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &py,
            "s1",
            "PreToolUse",
            "hash_json",
            "2026-04-24T10:00:00.000000+00:00",
            Some(py_payload),
            &[],
        );

        let rs = Connection::open(rs_tmp.path()).unwrap();
        rs.execute_batch(crate::schema::SCHEMA_V4_DDL).unwrap();
        insert_event(
            &rs,
            "s1",
            "PreToolUse",
            "hash_json",
            "2026-04-24T10:00:00.000000+00:00",
            Some(rs_payload),
            &[],
        );

        drop(py);
        drop(rs);

        let report = diff_databases(py_tmp.path(), rs_tmp.path()).unwrap();
        assert!(
            report.is_ok(),
            "Structurally equal JSON payloads should be treated as equal:\n{}",
            report.summary()
        );
    }

    // -----------------------------------------------------------------------
    // normalize_timestamp unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_timestamp_variants() {
        let cases = &[
            (
                "2026-04-24T10:00:00.000Z",
                "2026-04-24T10:00:00.000000+00:00",
            ),
            (
                "2026-04-24T10:00:00.000000Z",
                "2026-04-24T10:00:00.000000+00:00",
            ),
            (
                "2026-04-24T10:00:00.000+00:00",
                "2026-04-24T10:00:00.000000+00:00",
            ),
            (
                "2026-04-24T10:00:00.000000+00:00",
                "2026-04-24T10:00:00.000000+00:00",
            ),
            ("2026-04-24T10:00:00.5Z", "2026-04-24T10:00:00.500000+00:00"),
        ];
        for (input, expected) in cases {
            let got = normalize_timestamp(input);
            assert_eq!(got, *expected, "normalize_timestamp({input:?}) wrong");
        }
    }

    // -----------------------------------------------------------------------
    // json_structurally_equal unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn json_structural_equal_key_order() {
        assert!(json_structurally_equal(
            Some(r#"{"b":2,"a":1}"#),
            Some(r#"{"a":1,"b":2}"#)
        ));
    }

    #[test]
    fn json_structural_unequal_values() {
        assert!(!json_structurally_equal(
            Some(r#"{"a":1}"#),
            Some(r#"{"a":2}"#)
        ));
    }

    #[test]
    fn json_structural_both_null() {
        assert!(json_structurally_equal(None, None));
    }

    #[test]
    fn json_structural_one_null() {
        assert!(!json_structurally_equal(Some("{}"), None));
    }
}
