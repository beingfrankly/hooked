//! Parameterized SQLite writes for events, sessions, and tool_calls.
//!
//! Mirrors the Python SQL constants and write functions from
//! `~/.claude/telemetry/ingest.py`:
//!
//! - [`insert_event`]        ← Python `_INSERT_EVENT` / `conn.execute(_INSERT_EVENT, ev)`
//! - [`upsert_session`]      ← Python `_UPSERT_SESSION` / `conn.execute(_UPSERT_SESSION, srow)`
//! - [`upsert_tool_call`]    ← Python `_UPSERT_TOOL_CALL` / `conn.execute(_UPSERT_TOOL_CALL, tc)`
//! - [`recompute_counters`]  ← Python `_RECOMPUTE_SESSION_COUNTERS` (dynamic IN-list)
//!
//! ## `INSERT OR IGNORE` row-count semantics
//!
//! Python uses `cursor.rowcount` to count actually-inserted events (0 when the
//! `event_hash` unique index fires).  `rusqlite::Connection::execute` returns
//! `Result<usize>` with the same count; we cast it to `u64` for consistency.
//!
//! ## `IN (?,?,...)` dynamic binding
//!
//! `rusqlite` does not support array parameter binding.  We build the
//! placeholder string at runtime (`?` repeated `ids.len()` times, joined by
//! commas) and use `rusqlite::params_from_iter` to bind all values in a single
//! call.
//!
//! ## Column ordering
//!
//! Column lists match the DDL in `crate::schema::SCHEMA_V4_DDL` exactly.
//! Python verbatim SQL is reproduced in the constant doc-comments below.

use rusqlite::Connection;

use crate::enrich::session::EnrichedEvent;

// ---------------------------------------------------------------------------
// Row structs
// ---------------------------------------------------------------------------

/// A row to be written to the `sessions` table.
///
/// Field names and types mirror the `sessions` DDL in
/// `crate::schema::SCHEMA_V4_DDL`.  All nullable columns use `Option<T>`.
/// Integer counter columns (`total_events`, etc.) are intentionally absent —
/// they are initialised to `0` by the INSERT and then updated by
/// [`recompute_counters`].
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_id: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub source: Option<String>,
    pub chain_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub end_reason: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub cwd: Option<String>,
    pub config_version: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
    pub context_at_compact: Option<i64>,
}

/// A row to be written to the `tool_calls` table.
///
/// Field names and types mirror the `tool_calls` DDL in
/// `crate::schema::SCHEMA_V4_DDL`.
#[derive(Debug, Clone)]
pub struct ToolCallRow {
    pub session_id: String,
    pub tool_use_id: String,
    pub tool_name: String,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub input_summary: Option<String>,
    pub output_bytes: Option<i64>,
    pub error: Option<String>,
    pub succeeded: i32,
    pub skill_name: Option<String>,
    pub skill_type: Option<String>,
}

// ---------------------------------------------------------------------------
// insert_event
// ---------------------------------------------------------------------------

/// Insert an enriched event into the `events` table.
///
/// Uses `INSERT OR IGNORE` on the `event_hash` unique index conflict, mirroring
/// Python's `_INSERT_EVENT` constant and `cursor.rowcount` semantics.
///
/// Returns the number of rows actually inserted: `1` for a new event, `0` when
/// the `event_hash` already exists.
///
/// # Python verbatim SQL (`_INSERT_EVENT`)
///
/// ```text
/// INSERT OR IGNORE INTO events (
///     session_id, event_type, timestamp, sequence_num, event_hash,
///     tool_name, tool_use_id, tool_input, tool_result, result_size,
///     duration_ms, error, is_interrupt,
///     prompt_text, prompt_length,
///     agent_id, agent_type,
///     source, reason, model, permission_mode, cwd,
///     notification_type, compact_trigger, config_source,
///     config_version, git_branch, git_commit,
///     input_bytes, output_bytes, context_cumulative_bytes,
///     skill_name, skill_type,
///     task_id, task_subject, teammate_name,
///     raw_payload, is_slash_command, stop_hook_active
/// ) VALUES (
///     :session_id, :event_type, :timestamp, :sequence_num, :event_hash,
///     :tool_name, :tool_use_id, :tool_input, :tool_result, :result_size,
///     :duration_ms, :error, :is_interrupt,
///     :prompt_text, :prompt_length,
///     :agent_id, :agent_type,
///     :source, :reason, :model, :permission_mode, :cwd,
///     :notification_type, :compact_trigger, :config_source,
///     :config_version, :git_branch, :git_commit,
///     :input_bytes, :output_bytes, :context_cumulative_bytes,
///     :skill_name, :skill_type,
///     :task_id, :task_subject, :teammate_name,
///     :raw_payload, :is_slash_command, :stop_hook_active
/// )
/// ```
pub fn insert_event(conn: &Connection, event: &EnrichedEvent) -> anyhow::Result<u64> {
    let env = &event.envelope;
    let p = &env.p;
    let iso = &event.isolation;

    // Helper closures to extract optional string fields from the payload.
    let p_str = |key: &str| -> Option<String> {
        p.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let p_i64 = |key: &str| -> Option<i64> { p.get(key).and_then(serde_json::Value::as_i64) };

    // Payload fields that map directly to DB columns.
    let tool_name: Option<String> = p_str("tool_name");
    let tool_use_id: Option<String> = p_str("tool_use_id");

    // tool_input: serialized to compact JSON string if an object/array,
    // kept as-is if already a string, or NULL.
    let tool_input: Option<String> = p.get("tool_input").and_then(|v| match v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(crate::envelope::python_json_compact(other)),
    });

    // tool_result maps to `tool_response` in the wire payload (PostToolUse).
    let tool_result: Option<String> = p
        .get("tool_result")
        .or_else(|| p.get("tool_response"))
        .and_then(|v| match v {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(crate::envelope::python_json_compact(other)),
        });

    let result_size: Option<i64> = p_i64("result_size");
    let error: Option<String> = p_str("error");
    // Python: `1 if payload.get("is_interrupt") else 0`
    // The wire value may be a JSON boolean or a JSON integer; both are truthy
    // when non-zero / true.
    let is_interrupt: i32 = match p.get("is_interrupt") {
        Some(serde_json::Value::Bool(true)) => 1,
        Some(serde_json::Value::Number(n)) if n.as_i64().unwrap_or(0) != 0 => 1,
        _ => 0,
    };

    let prompt_text: Option<String> = p_str("prompt").or_else(|| p_str("prompt_text"));
    let prompt_length: Option<i64> =
        p_i64("prompt_length").or_else(|| prompt_text.as_deref().map(|s| s.len() as i64));

    let agent_id: Option<String> = p_str("agent_id");
    let agent_type: Option<String> = p_str("agent_type");
    let model: Option<String> = p_str("model");
    let permission_mode: Option<String> = p_str("permission_mode");
    let cwd: Option<String> = p_str("cwd");
    let notification_type: Option<String> = p_str("notification_type");
    // Python: `1 if payload.get("stop_hook_active") else 0`
    let stop_hook_active: i32 = match p.get("stop_hook_active") {
        Some(serde_json::Value::Bool(true)) => 1,
        Some(serde_json::Value::Number(n)) if n.as_i64().unwrap_or(0) != 0 => 1,
        _ => 0,
    };

    // Enrichment-derived fields.  After T09, typed values are written to
    // `EnrichedEvent::enriched` during enrichment and merged into
    // `envelope.p` by `merge_enriched_into_payloads` at the persistence
    // boundary, before `insert_event` runs.  Reading from `envelope.p`
    // here lets us treat hook-injected and enrichment-derived fields
    // uniformly.
    let config_version: Option<String> = p_str("config_version");
    let git_branch: Option<String> = p_str("git_branch");
    let git_commit: Option<String> = p_str("git_commit");

    // Skill detection — these may have been applied to the payload by the
    // enricher (not yet wired in this task).
    let skill_name: Option<String> = p_str("skill_name");
    let skill_type: Option<String> = p_str("skill_type");

    // raw_payload is the verbatim JSONL line.
    let raw_payload: Option<String> = if env.raw_line.is_empty() {
        None
    } else {
        Some(env.raw_line.clone())
    };

    let rows = conn.execute(
        "INSERT OR IGNORE INTO events (
            session_id, event_type, timestamp, sequence_num, event_hash,
            tool_name, tool_use_id, tool_input, tool_result, result_size,
            duration_ms, error, is_interrupt,
            prompt_text, prompt_length,
            agent_id, agent_type,
            source, reason, model, permission_mode, cwd,
            notification_type, compact_trigger, config_source,
            config_version, git_branch, git_commit,
            input_bytes, output_bytes, context_cumulative_bytes,
            skill_name, skill_type,
            task_id, task_subject, teammate_name,
            raw_payload, is_slash_command, stop_hook_active
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13,
            ?14, ?15,
            ?16, ?17,
            ?18, ?19, ?20, ?21, ?22,
            ?23, ?24, ?25,
            ?26, ?27, ?28,
            ?29, ?30, ?31,
            ?32, ?33,
            ?34, ?35, ?36,
            ?37, ?38, ?39
        )",
        rusqlite::params![
            // Identify
            p_str("session_id"),
            p_str("hook_event_name"),
            env.ts,
            event.sequence_num,
            event.event_hash,
            // Tool lifecycle
            tool_name,
            tool_use_id,
            tool_input,
            tool_result,
            result_size,
            event.duration_ms,
            error,
            is_interrupt,
            // User prompt
            prompt_text,
            prompt_length,
            // Agent context
            agent_id,
            agent_type,
            // Session lifecycle
            iso.source,
            iso.reason,
            model,
            permission_mode,
            cwd,
            // Event-specific
            notification_type,
            iso.compact_trigger,
            iso.config_source,
            // Enrichment
            config_version,
            git_branch,
            git_commit,
            // Context budget
            event.input_bytes,
            event.output_bytes,
            event.context_cumulative_bytes,
            // Skill
            skill_name,
            skill_type,
            // Task tracking
            iso.task_id,
            iso.task_subject,
            iso.teammate_name,
            // Insurance + flags
            raw_payload,
            iso.is_slash_command,
            stop_hook_active,
        ],
    )?;

    Ok(rows as u64)
}

// ---------------------------------------------------------------------------
// upsert_session
// ---------------------------------------------------------------------------

/// Upsert a row in the `sessions` table.
///
/// Mirrors Python's `_UPSERT_SESSION` constant.  On conflict for `session_id`,
/// uses `COALESCE(excluded.col, sessions.col)` so that a new value only
/// overwrites `NULL` — matching Python's partial-update semantics.
///
/// Counter columns (`total_events`, etc.) are always initialised to `0` on
/// INSERT and are not touched on UPDATE; call [`recompute_counters`] after
/// inserting events to fill them.
///
/// # Python verbatim SQL (`_UPSERT_SESSION`)
///
/// ```text
/// INSERT INTO sessions (
///     session_id, started_at, ended_at, source,
///     chain_id, parent_session_id, end_reason,
///     model, permission_mode, cwd,
///     config_version, git_branch, git_commit,
///     total_events, total_tool_calls, total_failures, total_prompts,
///     total_subagents, total_tasks, compaction_count, auto_compact_count,
///     permission_prompts, context_total_bytes, context_at_compact
/// ) VALUES (
///     :session_id, :started_at, :ended_at, :source,
///     :chain_id, :parent_session_id, :end_reason,
///     :model, :permission_mode, :cwd,
///     :config_version, :git_branch, :git_commit,
///     0, 0, 0, 0,
///     0, 0, 0, 0,
///     0, 0, :context_at_compact
/// )
/// ON CONFLICT(session_id) DO UPDATE SET
///     ended_at           = COALESCE(excluded.ended_at, sessions.ended_at),
///     source             = COALESCE(excluded.source, sessions.source),
///     chain_id           = COALESCE(excluded.chain_id, sessions.chain_id),
///     parent_session_id  = COALESCE(excluded.parent_session_id, sessions.parent_session_id),
///     end_reason         = COALESCE(excluded.end_reason, sessions.end_reason),
///     model              = COALESCE(excluded.model, sessions.model),
///     permission_mode    = COALESCE(excluded.permission_mode, sessions.permission_mode),
///     cwd                = COALESCE(excluded.cwd, sessions.cwd),
///     config_version     = COALESCE(excluded.config_version, sessions.config_version),
///     git_branch         = COALESCE(excluded.git_branch, sessions.git_branch),
///     git_commit         = COALESCE(excluded.git_commit, sessions.git_commit),
///     context_at_compact = COALESCE(excluded.context_at_compact, sessions.context_at_compact)
/// ```
pub fn upsert_session(conn: &Connection, session: &SessionRow) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO sessions (
            session_id, started_at, ended_at, source,
            chain_id, parent_session_id, end_reason,
            model, permission_mode, cwd,
            config_version, git_branch, git_commit,
            total_events, total_tool_calls, total_failures, total_prompts,
            total_subagents, total_tasks, compaction_count, auto_compact_count,
            permission_prompts, context_total_bytes, context_at_compact
        ) VALUES (
            ?1, ?2, ?3, ?4,
            ?5, ?6, ?7,
            ?8, ?9, ?10,
            ?11, ?12, ?13,
            0, 0, 0, 0,
            0, 0, 0, 0,
            0, 0, ?14
        )
        ON CONFLICT(session_id) DO UPDATE SET
            ended_at           = COALESCE(excluded.ended_at, sessions.ended_at),
            source             = COALESCE(excluded.source, sessions.source),
            chain_id           = COALESCE(excluded.chain_id, sessions.chain_id),
            parent_session_id  = COALESCE(excluded.parent_session_id, sessions.parent_session_id),
            end_reason         = COALESCE(excluded.end_reason, sessions.end_reason),
            model              = COALESCE(excluded.model, sessions.model),
            permission_mode    = COALESCE(excluded.permission_mode, sessions.permission_mode),
            cwd                = COALESCE(excluded.cwd, sessions.cwd),
            config_version     = COALESCE(excluded.config_version, sessions.config_version),
            git_branch         = COALESCE(excluded.git_branch, sessions.git_branch),
            git_commit         = COALESCE(excluded.git_commit, sessions.git_commit),
            context_at_compact = COALESCE(excluded.context_at_compact, sessions.context_at_compact)",
        rusqlite::params![
            session.session_id,
            session.started_at,
            session.ended_at,
            session.source,
            session.chain_id,
            session.parent_session_id,
            session.end_reason,
            session.model,
            session.permission_mode,
            session.cwd,
            session.config_version,
            session.git_branch,
            session.git_commit,
            session.context_at_compact,
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// upsert_tool_call
// ---------------------------------------------------------------------------

/// Upsert a row in the `tool_calls` table.
///
/// Mirrors Python's `_UPSERT_TOOL_CALL` constant.  The unique constraint is
/// `(session_id, tool_use_id)`.  On conflict, non-null values from the new
/// row overwrite existing NULLs, and `succeeded` is set to `0` whenever the
/// new row carries an error.
///
/// # Python verbatim SQL (`_UPSERT_TOOL_CALL`)
///
/// ```text
/// INSERT INTO tool_calls (
///     session_id, tool_use_id, tool_name, agent_id, agent_type,
///     started_at, completed_at, duration_ms, input_summary, output_bytes,
///     error, succeeded, skill_name, skill_type
/// ) VALUES (
///     :session_id, :tool_use_id, :tool_name, :agent_id, :agent_type,
///     :started_at, :completed_at, :duration_ms, :input_summary, :output_bytes,
///     :error, :succeeded, :skill_name, :skill_type
/// )
/// ON CONFLICT(session_id, tool_use_id) DO UPDATE SET
///     completed_at  = COALESCE(excluded.completed_at, tool_calls.completed_at),
///     duration_ms   = COALESCE(excluded.duration_ms, tool_calls.duration_ms),
///     output_bytes  = COALESCE(excluded.output_bytes, tool_calls.output_bytes),
///     error         = COALESCE(excluded.error, tool_calls.error),
///     succeeded     = CASE WHEN excluded.error IS NOT NULL THEN 0 ELSE tool_calls.succeeded END,
///     skill_name    = COALESCE(excluded.skill_name, tool_calls.skill_name),
///     skill_type    = COALESCE(excluded.skill_type, tool_calls.skill_type)
/// ```
pub fn upsert_tool_call(conn: &Connection, tool_call: &ToolCallRow) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO tool_calls (
            session_id, tool_use_id, tool_name, agent_id, agent_type,
            started_at, completed_at, duration_ms, input_summary, output_bytes,
            error, succeeded, skill_name, skill_type
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14
        )
        ON CONFLICT(session_id, tool_use_id) DO UPDATE SET
            completed_at  = COALESCE(excluded.completed_at, tool_calls.completed_at),
            duration_ms   = COALESCE(excluded.duration_ms, tool_calls.duration_ms),
            output_bytes  = COALESCE(excluded.output_bytes, tool_calls.output_bytes),
            error         = COALESCE(excluded.error, tool_calls.error),
            succeeded     = CASE WHEN excluded.error IS NOT NULL THEN 0 ELSE tool_calls.succeeded END,
            skill_name    = COALESCE(excluded.skill_name, tool_calls.skill_name),
            skill_type    = COALESCE(excluded.skill_type, tool_calls.skill_type)",
        rusqlite::params![
            tool_call.session_id,
            tool_call.tool_use_id,
            tool_call.tool_name,
            tool_call.agent_id,
            tool_call.agent_type,
            tool_call.started_at,
            tool_call.completed_at,
            tool_call.duration_ms,
            tool_call.input_summary,
            tool_call.output_bytes,
            tool_call.error,
            tool_call.succeeded,
            tool_call.skill_name,
            tool_call.skill_type,
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// recompute_counters
// ---------------------------------------------------------------------------

/// Recompute aggregate counter columns on the `sessions` table for the given
/// session IDs.
///
/// This is idempotent — safe to call multiple times.  Uses a dynamic
/// `IN (?, ?, ...)` clause built at runtime with one `?` per id.
/// Returns immediately (no-op) when `session_ids` is empty.
///
/// Mirrors Python's `_RECOMPUTE_SESSION_COUNTERS` constant, which is formatted
/// with `{placeholders}` and executed as:
///
/// ```python
/// placeholders = ",".join("?" * len(session_ids))
/// conn.execute(
///     _RECOMPUTE_SESSION_COUNTERS.format(placeholders=placeholders),
///     session_ids,
/// )
/// ```
///
/// # Python verbatim SQL (`_RECOMPUTE_SESSION_COUNTERS`)
///
/// ```text
/// UPDATE sessions SET
///     total_events       = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id),
///     total_tool_calls   = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PreToolUse'),
///     total_failures     = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PostToolUseFailure'),
///     total_prompts      = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'UserPromptSubmit'),
///     total_subagents    = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'SubagentStart'),
///     total_tasks        = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'TaskCompleted'),
///     compaction_count   = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'SessionStart' AND e.source = 'compact'),
///     auto_compact_count = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PreCompact' AND e.compact_trigger = 'auto'),
///     permission_prompts = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PermissionRequest'),
///     context_total_bytes = (SELECT MAX(COALESCE(e.context_cumulative_bytes, 0)) FROM events e WHERE e.session_id = sessions.session_id)
/// WHERE sessions.session_id IN ({placeholders})
/// ```
pub fn recompute_counters(conn: &Connection, session_ids: &[&str]) -> anyhow::Result<()> {
    if session_ids.is_empty() {
        return Ok(());
    }

    let placeholders = std::iter::repeat_n("?", session_ids.len())
        .collect::<Vec<_>>()
        .join(",");

    let sql = format!(
        "UPDATE sessions SET
            total_events        = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id),
            total_tool_calls    = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PreToolUse'),
            total_failures      = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PostToolUseFailure'),
            total_prompts       = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'UserPromptSubmit'),
            total_subagents     = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'SubagentStart'),
            total_tasks         = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'TaskCompleted'),
            compaction_count    = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'SessionStart' AND e.source = 'compact'),
            auto_compact_count  = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PreCompact' AND e.compact_trigger = 'auto'),
            permission_prompts  = (SELECT COUNT(*) FROM events e WHERE e.session_id = sessions.session_id AND e.event_type = 'PermissionRequest'),
            context_total_bytes = (SELECT MAX(COALESCE(e.context_cumulative_bytes, 0)) FROM events e WHERE e.session_id = sessions.session_id)
        WHERE sessions.session_id IN ({placeholders})"
    );

    let params_vec: Vec<&dyn rusqlite::ToSql> = session_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    conn.execute(&sql, rusqlite::params_from_iter(params_vec.iter()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::enrich::payload::EnrichedPayload;
    use crate::enrich::session::{EnrichedEvent, FieldIsolation};
    use crate::envelope::Envelope;
    use crate::schema::SCHEMA_V4_DDL;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Open a fresh in-memory SQLite DB and apply the v4 schema.
    fn open_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply DDL");
        conn
    }

    /// Build a minimal `EnrichedEvent` with the given session_id and event_hash.
    fn make_enriched(session_id: &str, event_hash: &str, event_type: &str) -> EnrichedEvent {
        let envelope = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": event_type,
                "session_id": session_id,
            }),
            h: Some(event_hash.to_owned()),
            raw_index: 0,
            raw_line: format!(
                r#"{{"v":1,"ts":"2024-01-01T00:00:00.000Z","p":{{"hook_event_name":"{event_type}","session_id":"{session_id}"}}}}"#
            ),
        };
        EnrichedEvent {
            envelope,
            enriched: EnrichedPayload::default(),
            sequence_num: 0,
            isolation: FieldIsolation::default(),
            duration_ms: None,
            input_bytes: 0,
            output_bytes: 0,
            context_cumulative_bytes: 0,
            event_hash: event_hash.to_owned(),
        }
    }

    /// Build a minimal `SessionRow`.
    fn make_session(session_id: &str) -> SessionRow {
        SessionRow {
            session_id: session_id.to_owned(),
            started_at: Some("2024-01-01T00:00:00.000Z".to_owned()),
            ended_at: None,
            source: None,
            chain_id: None,
            parent_session_id: None,
            end_reason: None,
            model: None,
            permission_mode: None,
            cwd: None,
            config_version: None,
            git_branch: None,
            git_commit: None,
            context_at_compact: None,
        }
    }

    /// Build a minimal `ToolCallRow`.
    fn make_tool_call(session_id: &str, tool_use_id: &str) -> ToolCallRow {
        ToolCallRow {
            session_id: session_id.to_owned(),
            tool_use_id: tool_use_id.to_owned(),
            tool_name: "Bash".to_owned(),
            agent_id: None,
            agent_type: None,
            started_at: "2024-01-01T00:00:00.000Z".to_owned(),
            completed_at: Some("2024-01-01T00:00:01.000Z".to_owned()),
            duration_ms: Some(1000),
            input_summary: Some("echo hello".to_owned()),
            output_bytes: Some(11),
            error: None,
            succeeded: 1,
            skill_name: None,
            skill_type: None,
        }
    }

    // -----------------------------------------------------------------------
    // insert_event_new_row
    // -----------------------------------------------------------------------

    #[test]
    fn insert_event_new_row() {
        let conn = open_db();
        let ev = make_enriched("s1", "hash0001deadbeef", "SessionStart");

        let rows = insert_event(&conn, &ev).expect("insert_event");
        assert_eq!(rows, 1, "first insert must return 1");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "events table must have 1 row");
    }

    // -----------------------------------------------------------------------
    // insert_event_duplicate_ignored
    // -----------------------------------------------------------------------

    #[test]
    fn insert_event_duplicate_ignored() {
        let conn = open_db();
        let ev = make_enriched("s1", "hash0001deadbeef", "SessionStart");

        let r1 = insert_event(&conn, &ev).expect("first insert");
        let r2 = insert_event(&conn, &ev).expect("second insert");

        assert_eq!(r1, 1, "first insert returns 1");
        assert_eq!(r2, 0, "duplicate insert returns 0");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "row count must remain 1");
    }

    // -----------------------------------------------------------------------
    // insert_or_ignore_rowcount
    // -----------------------------------------------------------------------
    /// Explicit plan-required test: mirrors Python `cursor.rowcount` semantics.
    /// Insert once → 1, insert again → 0.
    #[test]
    fn insert_or_ignore_rowcount() {
        let conn = open_db();
        let ev = make_enriched("sess-rc", "rowcount_test_hash", "PreToolUse");

        assert_eq!(
            insert_event(&conn, &ev).expect("first insert"),
            1,
            "first insert: rowcount must be 1"
        );
        assert_eq!(
            insert_event(&conn, &ev).expect("second insert"),
            0,
            "duplicate insert: rowcount must be 0"
        );
    }

    // -----------------------------------------------------------------------
    // upsert_session_new
    // -----------------------------------------------------------------------

    #[test]
    fn upsert_session_new() {
        let conn = open_db();
        let session = SessionRow {
            session_id: "sess-new".to_owned(),
            started_at: Some("2024-06-01T10:00:00.000Z".to_owned()),
            ended_at: Some("2024-06-01T10:30:00.000Z".to_owned()),
            source: Some("startup".to_owned()),
            chain_id: Some("chain-abc".to_owned()),
            parent_session_id: None,
            end_reason: Some("logout".to_owned()),
            model: Some("claude-opus-4".to_owned()),
            permission_mode: Some("bypassPermissions".to_owned()),
            cwd: Some("/home/user/project".to_owned()),
            config_version: Some("abcd1234".to_owned()),
            git_branch: Some("main".to_owned()),
            git_commit: Some("abc1234".to_owned()),
            context_at_compact: Some(12345),
        };

        upsert_session(&conn, &session).expect("upsert_session");

        let (started_at, ended_at, model, git_branch): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT started_at, ended_at, model, git_branch FROM sessions WHERE session_id = ?1",
                rusqlite::params!["sess-new"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("query session");

        assert_eq!(started_at.as_deref(), Some("2024-06-01T10:00:00.000Z"));
        assert_eq!(ended_at.as_deref(), Some("2024-06-01T10:30:00.000Z"));
        assert_eq!(model.as_deref(), Some("claude-opus-4"));
        assert_eq!(git_branch.as_deref(), Some("main"));
    }

    // -----------------------------------------------------------------------
    // upsert_session_update
    // -----------------------------------------------------------------------

    #[test]
    fn upsert_session_update() {
        let conn = open_db();

        // First upsert: no ended_at or model.
        let first = SessionRow {
            session_id: "sess-upd".to_owned(),
            started_at: Some("2024-06-01T10:00:00.000Z".to_owned()),
            ended_at: None,
            source: Some("startup".to_owned()),
            model: None,
            ..make_session("sess-upd")
        };
        upsert_session(&conn, &first).expect("first upsert");

        // Second upsert: now has ended_at and model.
        let second = SessionRow {
            session_id: "sess-upd".to_owned(),
            started_at: Some("2024-06-01T10:00:00.000Z".to_owned()),
            ended_at: Some("2024-06-01T11:00:00.000Z".to_owned()),
            source: None,
            model: Some("claude-3-5-sonnet".to_owned()),
            ..make_session("sess-upd")
        };
        upsert_session(&conn, &second).expect("second upsert");

        let (ended_at, model, source): (Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT ended_at, model, source FROM sessions WHERE session_id = ?1",
                rusqlite::params!["sess-upd"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("query session");

        // COALESCE: ended_at was NULL → now updated.
        assert_eq!(ended_at.as_deref(), Some("2024-06-01T11:00:00.000Z"));
        // COALESCE: model was NULL → now updated.
        assert_eq!(model.as_deref(), Some("claude-3-5-sonnet"));
        // COALESCE: source was "startup" → excluded.source is NULL → kept "startup".
        assert_eq!(source.as_deref(), Some("startup"));
    }

    // -----------------------------------------------------------------------
    // upsert_tool_call_new
    // -----------------------------------------------------------------------

    #[test]
    fn upsert_tool_call_new() {
        let conn = open_db();

        // Need a session row first (FK-like integrity, though SQLite doesn't enforce FKs by default).
        upsert_session(&conn, &make_session("sess-tc")).expect("session");

        let tc = make_tool_call("sess-tc", "tuid-001");
        upsert_tool_call(&conn, &tc).expect("upsert_tool_call");

        let (tool_name, duration_ms, succeeded): (String, Option<i64>, i32) = conn
            .query_row(
                "SELECT tool_name, duration_ms, succeeded FROM tool_calls WHERE session_id = ?1 AND tool_use_id = ?2",
                rusqlite::params!["sess-tc", "tuid-001"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("query tool_call");

        assert_eq!(tool_name, "Bash");
        assert_eq!(duration_ms, Some(1000));
        assert_eq!(succeeded, 1);
    }

    // -----------------------------------------------------------------------
    // recompute_counters_single
    // -----------------------------------------------------------------------

    #[test]
    fn recompute_counters_single() {
        let conn = open_db();

        upsert_session(&conn, &make_session("sess-cnt")).expect("session");

        // Insert 3 events: SessionStart, PreToolUse x2.
        let events = [
            make_enriched("sess-cnt", "h001", "SessionStart"),
            make_enriched("sess-cnt", "h002", "PreToolUse"),
            make_enriched("sess-cnt", "h003", "PreToolUse"),
        ];
        for ev in &events {
            insert_event(&conn, ev).expect("insert_event");
        }

        // Insert 2 tool_calls.
        for (i, tuid) in ["tuid-a", "tuid-b"].iter().enumerate() {
            let mut tc = make_tool_call("sess-cnt", tuid);
            tc.tool_use_id = tuid.to_string();
            // Make IDs unique
            let _ = i; // suppress unused warning
            upsert_tool_call(&conn, &tc).expect("tool_call");
        }

        recompute_counters(&conn, &["sess-cnt"]).expect("recompute");

        let (total_events, total_tool_calls): (i64, i64) = conn
            .query_row(
                "SELECT total_events, total_tool_calls FROM sessions WHERE session_id = ?1",
                rusqlite::params!["sess-cnt"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query counters");

        assert_eq!(total_events, 3, "total_events must count all events");
        assert_eq!(
            total_tool_calls, 2,
            "total_tool_calls counts PreToolUse events"
        );
    }

    // -----------------------------------------------------------------------
    // recompute_counters_multiple_ids
    // -----------------------------------------------------------------------

    #[test]
    fn recompute_counters_multiple_ids() {
        let conn = open_db();

        for sid in ["sess-a", "sess-b", "sess-c"] {
            upsert_session(&conn, &make_session(sid)).expect("session");
        }

        // sess-a: 1 event
        insert_event(&conn, &make_enriched("sess-a", "ha001", "SessionStart")).expect("insert");
        // sess-b: 2 events
        insert_event(&conn, &make_enriched("sess-b", "hb001", "SessionStart")).expect("insert");
        insert_event(&conn, &make_enriched("sess-b", "hb002", "PreToolUse")).expect("insert");
        // sess-c: 3 events
        insert_event(&conn, &make_enriched("sess-c", "hc001", "SessionStart")).expect("insert");
        insert_event(&conn, &make_enriched("sess-c", "hc002", "PreToolUse")).expect("insert");
        insert_event(&conn, &make_enriched("sess-c", "hc003", "UserPromptSubmit")).expect("insert");

        recompute_counters(&conn, &["sess-a", "sess-b", "sess-c"]).expect("recompute");

        let count_for = |sid: &str| -> (i64, i64, i64) {
            conn.query_row(
                "SELECT total_events, total_tool_calls, total_prompts FROM sessions WHERE session_id = ?1",
                rusqlite::params![sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("query")
        };

        let (a_ev, a_tc, a_pr) = count_for("sess-a");
        let (b_ev, b_tc, b_pr) = count_for("sess-b");
        let (c_ev, c_tc, c_pr) = count_for("sess-c");

        assert_eq!(a_ev, 1);
        assert_eq!(a_tc, 0);
        assert_eq!(a_pr, 0);

        assert_eq!(b_ev, 2);
        assert_eq!(b_tc, 1);
        assert_eq!(b_pr, 0);

        assert_eq!(c_ev, 3);
        assert_eq!(c_tc, 1);
        assert_eq!(c_pr, 1);
    }

    // -----------------------------------------------------------------------
    // recompute_counters_empty_list
    // -----------------------------------------------------------------------

    #[test]
    fn recompute_counters_empty_list() {
        let conn = open_db();
        // Must not panic or return an error.
        recompute_counters(&conn, &[]).expect("empty list must be a no-op");
    }
}
