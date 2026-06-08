//! Ingest pipeline: JSONL → enrichment → SQLite writes.
//!
//! This module is the top-level orchestration layer. It mirrors the Python
//! `ingest_file` and `ingest_all_unprocessed` functions from
//! `~/.claude/telemetry/ingest.py`.
//!
//! ## Pipeline overview
//!
//! ```text
//! JSONL file
//!   → parse_jsonl_file       (envelope parsing, malformed collection)
//!   → group by session_id
//!   → enrich_session         (4-pass per-session enrichment)
//!   → apply_git_and_config   (I/O: git rev-parse + config hash, injected into payload)
//!   → cross-session lineage  (DB query for candidates + find_parent_session)
//!   → build_session_rows     (aggregate per-session metadata)
//!   → build_tool_call_rows   (pair PreToolUse with PostToolUse)
//!   → DB transaction:
//!       insert_event (INSERT OR IGNORE)
//!       upsert_session
//!       recompute_counters
//!       upsert_tool_call
//!   → FTS5 rebuild
//!   → WAL checkpoint
//! ```
//!
//! ## Cross-session lineage with DB
//!
//! After upserting a session, we query the DB for recently-ended sessions
//! (last 30 minutes) as `SessionSummary` candidates, call
//! [`crate::enrich::lineage::find_parent_session`], and if a parent is found
//! we set `parent_session_id` and `chain_id` on the session row and re-upsert.
//!
//! The chain-id rule: use parent's `chain_id` if set; otherwise use parent's
//! `session_id`. This makes `chain_id` a 1-hop pointer to the chain root.

pub mod archive;
pub mod writes;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::enrich::gitcfg::{GitContext, config_hash, git_context};
use crate::enrich::lineage::{SessionSummary, find_parent_session};
use crate::enrich::session::{EnrichedEvent, enrich_session};
use crate::envelope::parse_jsonl_file;
use crate::ingest::archive::{IngestLock, archive_jsonl};
use crate::ingest::writes::{
    SessionRow, ToolCallRow, insert_event, recompute_counters, upsert_session, upsert_tool_call,
};
use crate::paths::{db_path, last_ingest_file, log_dir};
use crate::{info, warn_};

// ---------------------------------------------------------------------------
// Public stats types
// ---------------------------------------------------------------------------

/// Statistics returned by a single-file ingest.
///
/// Mirrors Python `ingest_file`'s return semantics: the integer count of new
/// rows actually inserted (duplicates via `INSERT OR IGNORE` contribute 0).
#[derive(Debug, Clone, Default)]
pub struct IngestStats {
    /// Path of the ingested file.
    pub file: PathBuf,
    /// Total envelopes present in the file (good + malformed).
    pub events_seen: u64,
    /// Number of rows actually inserted (0 for duplicates).
    pub events_inserted: u64,
    /// Number of malformed lines (unparseable / missing required fields).
    pub events_malformed: u64,
    /// Number of unique session IDs touched.
    pub sessions_touched: u64,
}

/// Statistics returned by a full unprocessed-files ingest run.
#[derive(Debug, Clone, Default)]
pub struct IngestAllStats {
    /// Number of JSONL files processed.
    pub files_processed: u64,
    /// Total events inserted across all files.
    pub total_events_inserted: u64,
    /// Total malformed lines across all files.
    pub total_events_malformed: u64,
}

// ---------------------------------------------------------------------------
// apply_git_and_config (private)
// ---------------------------------------------------------------------------

/// For each unique `session_id` in `events`, fetch git context once from the
/// session's `cwd` (taken from the `SessionStart` event, or the first event)
/// and inject `git_branch`, `git_commit`, and `config_version` directly into
/// each event's `envelope.p` payload JSON.
///
/// Returns `(git_branch, git_commit, config_version)` derived from the first
/// session group (used by callers that need a summary for the session row).
///
/// ## Why inject into `envelope.p`?
///
/// `writes::insert_event` already reads `git_branch`, `git_commit`, and
/// `config_version` from `event.envelope.p` via `p_str("git_branch")` etc.
/// Injecting the enriched values into the payload is therefore the **least
/// invasive** approach — it does not require changes to `EnrichedEvent`,
/// `writes.rs`, or any other module.
///
/// This also matches Python's approach: `_apply_git_and_config` mutates the
/// event dict in place before the DB write loop.
///
/// ## Python verbatim — `_apply_git_and_config` (lines 956–989)
///
/// ```python
/// def _apply_git_and_config(events: list[dict]) -> None:
///     config_ver = _compute_config_version()
///     _git_cache: dict[str, tuple[Optional[str], Optional[str]]] = {}
///     by_session: dict[str, list[dict]] = {}
///     for ev in events:
///         by_session.setdefault(ev["session_id"], []).append(ev)
///     for sid, evs in by_session.items():
///         start_ev = next(
///             (e for e in evs if e.get("event_type") == "SessionStart"),
///             evs[0] if evs else None,
///         )
///         if not start_ev:
///             continue
///         cwd = start_ev.get("cwd")
///         if cwd not in _git_cache:
///             _git_cache[cwd] = _git_context(cwd)
///         branch, commit = _git_cache[cwd]
///         for ev in evs:
///             ev["config_version"] = config_ver
///             if branch:
///                 ev["git_branch"] = branch
///             if commit:
///                 ev["git_commit"] = commit
/// ```
fn apply_git_and_config(by_session: &mut HashMap<String, Vec<EnrichedEvent>>) {
    let config_ver = config_hash().unwrap_or_default();

    // Cache git results per cwd (mirrors Python `_git_cache`).
    let mut git_cache: HashMap<String, Option<GitContext>> = HashMap::new();

    for (_sid, evs) in by_session.iter_mut() {
        if evs.is_empty() {
            continue;
        }

        // Find SessionStart, fall back to first event — mirrors Python.
        let cwd: Option<String> = {
            let start_ev = evs
                .iter()
                .find(|e| {
                    e.envelope.p.get("hook_event_name").and_then(|v| v.as_str())
                        == Some("SessionStart")
                })
                .unwrap_or(&evs[0]);

            start_ev
                .envelope
                .p
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };

        let cwd_key = cwd.clone().unwrap_or_default();

        if !git_cache.contains_key(&cwd_key) {
            let ctx = cwd
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|c| git_context(Path::new(c)));
            git_cache.insert(cwd_key.clone(), ctx);
        }

        let git_ctx = git_cache.get(&cwd_key).and_then(|o| o.as_ref());

        // T09: write typed values onto each event's enriched payload (no JSON mutation).
        // The merge into envelope.p happens at the persistence boundary via
        // merge_enriched_into_payloads.
        for ev in evs.iter_mut() {
            ev.enriched.config_version = Some(config_ver.clone());
            if let Some(ctx) = git_ctx {
                if let Some(branch) = &ctx.branch {
                    ev.enriched.git_branch = Some(branch.clone());
                }
                if let Some(commit) = &ctx.commit_sha {
                    ev.enriched.git_commit = Some(commit.clone());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// merge_enriched_into_payloads (private)
// ---------------------------------------------------------------------------

/// T09: single persistence-boundary merge of typed enriched fields back into
/// `envelope.p`, in the order that preserves byte-identical parity with the
/// Python reference.
///
/// This is the ONLY place that mutates `envelope.p` after initial construction.
/// It must be called immediately after `apply_git_and_config` and BEFORE any
/// code reads `envelope.p` for persistence (insert_event / build_session_rows /
/// build_tool_call_rows).
fn merge_enriched_into_payloads(by_session: &mut HashMap<String, Vec<EnrichedEvent>>) {
    for (_sid, evs) in by_session.iter_mut() {
        for ev in evs.iter_mut() {
            if let Some(obj) = ev.envelope.p.as_object_mut() {
                ev.enriched.merge_into(obj);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// build_session_rows (private)
// ---------------------------------------------------------------------------

/// Build per-session metadata rows from a map of enriched events.
///
/// Mirrors Python `_build_session_rows`.  Counter columns are omitted — they
/// are initialised to `0` by the INSERT and recomputed via
/// `recompute_counters` after event inserts.
fn build_session_rows(by_session: &HashMap<String, Vec<EnrichedEvent>>) -> Vec<SessionRow> {
    let mut rows = Vec::new();

    for (sid, evs) in by_session {
        if evs.is_empty() {
            continue;
        }

        // Sort by timestamp + raw_index — mirrors Python's `sorted(evs, key=...)`.
        let mut sorted: Vec<&EnrichedEvent> = evs.iter().collect();
        sorted.sort_by(|a, b| {
            a.envelope
                .ts
                .cmp(&b.envelope.ts)
                .then_with(|| a.envelope.raw_index.cmp(&b.envelope.raw_index))
        });

        // First SessionStart, or first event.
        let start_ev = sorted
            .iter()
            .find(|e| {
                e.envelope.p.get("hook_event_name").and_then(|v| v.as_str()) == Some("SessionStart")
            })
            .copied()
            .unwrap_or(sorted[0]);

        // Last SessionEnd, or None.
        let end_ev = sorted
            .iter()
            .rev()
            .find(|e| {
                e.envelope.p.get("hook_event_name").and_then(|v| v.as_str()) == Some("SessionEnd")
            })
            .copied();

        // context_at_compact: last auto PreCompact event's cumulative bytes.
        let context_at_compact: Option<i64> = sorted
            .iter()
            .filter(|e| {
                e.envelope.p.get("hook_event_name").and_then(|v| v.as_str()) == Some("PreCompact")
                    && e.isolation.compact_trigger.as_deref() == Some("auto")
            })
            .max_by(|a, b| a.envelope.ts.cmp(&b.envelope.ts))
            .map(|e| e.context_cumulative_bytes);

        let p_str = |ev: &EnrichedEvent, key: &str| -> Option<String> {
            ev.envelope
                .p
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };

        rows.push(SessionRow {
            session_id: sid.clone(),
            started_at: Some(start_ev.envelope.ts.clone()),
            ended_at: end_ev.map(|e| e.envelope.ts.clone()),
            source: start_ev.isolation.source.clone(),
            chain_id: None,          // filled in by lineage
            parent_session_id: None, // filled in by lineage
            end_reason: end_ev.and_then(|e| e.isolation.reason.clone()),
            model: p_str(start_ev, "model"),
            permission_mode: p_str(start_ev, "permission_mode"),
            cwd: p_str(start_ev, "cwd"),
            config_version: p_str(start_ev, "config_version"),
            git_branch: p_str(start_ev, "git_branch"),
            git_commit: p_str(start_ev, "git_commit"),
            context_at_compact,
        });
    }

    rows
}

// ---------------------------------------------------------------------------
// build_tool_call_rows (private)
// ---------------------------------------------------------------------------

/// Build `tool_calls` rows by pairing `PreToolUse` with
/// `PostToolUse`/`PostToolUseFailure` events via `tool_use_id`.
///
/// Mirrors Python `_build_tool_call_rows`.
fn build_tool_call_rows(by_session: &HashMap<String, Vec<EnrichedEvent>>) -> Vec<ToolCallRow> {
    let mut pre: HashMap<(String, String), &EnrichedEvent> = HashMap::new();
    let mut rows: Vec<ToolCallRow> = Vec::new();

    // Flatten all events into a single sorted iterator.
    let mut all_events: Vec<&EnrichedEvent> = by_session.values().flatten().collect();
    all_events.sort_by(|a, b| {
        a.envelope
            .ts
            .cmp(&b.envelope.ts)
            .then_with(|| a.envelope.raw_index.cmp(&b.envelope.raw_index))
    });

    for ev in all_events {
        let et = ev
            .envelope
            .p
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let sid = ev
            .envelope
            .p
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let tuid = ev.envelope.p.get("tool_use_id").and_then(|v| v.as_str());

        let Some(tuid) = tuid else { continue };

        let key = (sid.to_owned(), tuid.to_owned());

        if et == "PreToolUse" {
            pre.insert(key, ev);
        } else if et == "PostToolUse" || et == "PostToolUseFailure" {
            let pre_ev = pre.get(&key).copied();

            let started_at = pre_ev
                .map(|p| p.envelope.ts.clone())
                .unwrap_or_else(|| ev.envelope.ts.clone());

            let input_raw = pre_ev.and_then(|p| {
                p.envelope
                    .p
                    .get("tool_input")
                    .map(crate::envelope::python_json_compact)
            });
            let input_summary: Option<String> = input_raw.map(|s| {
                if s.len() > 200 {
                    s[..200].to_owned()
                } else {
                    s
                }
            });

            let p_str = |e: &EnrichedEvent, k: &str| -> Option<String> {
                e.envelope
                    .p
                    .get(k)
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            };

            let tool_name = p_str(ev, "tool_name")
                .or_else(|| pre_ev.and_then(|p| p_str(p, "tool_name")))
                .unwrap_or_default();

            let agent_id =
                p_str(ev, "agent_id").or_else(|| pre_ev.and_then(|p| p_str(p, "agent_id")));
            let agent_type =
                p_str(ev, "agent_type").or_else(|| pre_ev.and_then(|p| p_str(p, "agent_type")));

            let error = if et == "PostToolUseFailure" {
                p_str(ev, "error")
            } else {
                None
            };
            let succeeded = if et == "PostToolUseFailure" { 0 } else { 1 };

            let skill_name =
                p_str(ev, "skill_name").or_else(|| pre_ev.and_then(|p| p_str(p, "skill_name")));
            let skill_type =
                p_str(ev, "skill_type").or_else(|| pre_ev.and_then(|p| p_str(p, "skill_type")));

            rows.push(ToolCallRow {
                session_id: sid.to_owned(),
                tool_use_id: tuid.to_owned(),
                tool_name,
                agent_id,
                agent_type,
                started_at,
                completed_at: Some(ev.envelope.ts.clone()),
                duration_ms: ev.duration_ms,
                input_summary,
                output_bytes: Some(ev.output_bytes),
                error,
                succeeded,
                skill_name,
                skill_type,
            });
        }
    }

    rows
}

// ---------------------------------------------------------------------------
// Lineage helpers (private)
// ---------------------------------------------------------------------------

/// Query the DB for recently-ended sessions (within last 30 minutes) to use
/// as lineage candidates.
///
/// Returns candidates ordered oldest-to-newest (ascending `ended_at`), as
/// required by [`find_parent_session`].
///
/// ## SQL query (mirrors Python's candidate window)
///
/// ```sql
/// SELECT session_id, ended_at, cwd, git_branch, model, end_reason, source
///   FROM sessions
///  WHERE ended_at IS NOT NULL
///    AND ended_at > datetime('now', '-30 minutes')
///  ORDER BY ended_at ASC
///  LIMIT 20
/// ```
fn query_lineage_candidates(conn: &Connection) -> anyhow::Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, ended_at, cwd, git_branch, model, end_reason, source
           FROM sessions
          WHERE ended_at IS NOT NULL
            AND ended_at > datetime('now', '-30 minutes')
          ORDER BY ended_at ASC
          LIMIT 20",
    )?;

    let candidates = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,         // session_id
                row.get::<_, Option<String>>(1)?, // ended_at
                row.get::<_, Option<String>>(2)?, // cwd
                row.get::<_, Option<String>>(3)?, // git_branch
                row.get::<_, Option<String>>(4)?, // model
                row.get::<_, Option<String>>(5)?, // end_reason
                row.get::<_, Option<String>>(6)?, // source
            ))
        })?
        .filter_map(|r| r.ok())
        .map(
            |(session_id, ended_at, cwd, git_branch, model, end_reason, source)| {
                let last_event_ts = ended_at.as_deref().and_then(parse_ts);
                SessionSummary {
                    session_id,
                    first_event_ts: None,
                    last_event_ts,
                    end_reason,
                    source,
                    cwd,
                    git_branch,
                    model,
                }
            },
        )
        .collect();

    Ok(candidates)
}

/// Parse an ISO-8601 timestamp string into `DateTime<Utc>`.
fn parse_ts(ts: &str) -> Option<DateTime<Utc>> {
    let normalized = if let Some(stripped) = ts.strip_suffix('Z') {
        format!("{}+00:00", stripped)
    } else {
        ts.to_owned()
    };
    chrono::DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// ingest_file
// ---------------------------------------------------------------------------

/// Parse a JSONL file, enrich all events, and batch-insert into SQLite.
///
/// Returns [`IngestStats`] with counts of events seen, inserted, malformed, and
/// sessions touched.  On duplicate insert the `event_hash` unique index fires
/// `INSERT OR IGNORE`, contributing 0 to `events_inserted` — exactly mirroring
/// Python's `cursor.rowcount` semantics.
///
/// ## Python verbatim — `ingest_file` (lines 996–1064)
///
/// ```python
/// def ingest_file(db_path: str, jsonl_path: str) -> int:
///     TELEMETRY_DIR.mkdir(parents=True, exist_ok=True)
///     ARCHIVE_DIR.mkdir(parents=True, exist_ok=True)
///     events = _parse_jsonl_file(jsonl_path)
///     if not events:
///         return 0
///     events = enrich_session_events(events)
///     _apply_git_and_config(events)
///     by_session: dict[str, list[dict]] = {}
///     for ev in events:
///         by_session.setdefault(ev["session_id"], []).append(ev)
///     enrich_cross_session(by_session)
///     session_rows = _build_session_rows(events)
///     tool_call_rows = _build_tool_call_rows(events)
///     conn = _init_db(db_path)
///     inserted = 0
///     try:
///         with conn:
///             for ev in events:
///                 cur = conn.execute(_INSERT_EVENT, ev)
///                 inserted += cur.rowcount
///             for srow in session_rows:
///                 conn.execute(_UPSERT_SESSION, srow)
///             session_ids = list({ev["session_id"] for ev in events})
///             if session_ids:
///                 placeholders = ",".join("?" * len(session_ids))
///                 conn.execute(
///                     _RECOMPUTE_SESSION_COUNTERS.format(placeholders=placeholders),
///                     session_ids,
///                 )
///             for tc in tool_call_rows:
///                 conn.execute(_UPSERT_TOOL_CALL, tc)
///         with conn:
///             conn.execute("INSERT INTO events_fts(events_fts) VALUES('rebuild')")
///         conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
///     finally:
///         conn.close()
///     return inserted
/// ```
///
/// ## Differences from Python
///
/// - The Rust version does **not** open or close the DB — the caller provides
///   `conn`.  This allows `ingest_all_unprocessed` to reuse a single connection
///   across all files and run FTS rebuild + WAL checkpoint only once at the end.
/// - Cross-session lineage uses a DB query for candidates rather than in-memory
///   session history (because files are processed serially and DB state persists
///   between files).
pub fn ingest_file(conn: &mut Connection, path: &Path) -> anyhow::Result<IngestStats> {
    let mut stats = IngestStats {
        file: path.to_path_buf(),
        ..IngestStats::default()
    };

    // 1. Parse JSONL
    let parse_result = parse_jsonl_file(path)?;

    // 2. Log malformed lines
    stats.events_malformed = parse_result.malformed.len() as u64;
    for m in &parse_result.malformed {
        crate::warn_!(
            "ingest",
            "malformed line {} in {}: {}",
            m.raw_index,
            path.display(),
            m.error
        );
    }

    if parse_result.envelopes.is_empty() {
        return Ok(stats);
    }

    stats.events_seen = parse_result.envelopes.len() as u64 + parse_result.malformed.len() as u64;

    // 3. Group by session_id
    let mut by_session: HashMap<String, Vec<crate::envelope::Envelope>> = HashMap::new();
    for env in parse_result.envelopes {
        let sid = env
            .p
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        by_session.entry(sid).or_default().push(env);
    }

    // 4. Per-session enrichment (4 passes)
    let mut enriched_by_session: HashMap<String, Vec<EnrichedEvent>> = HashMap::new();
    for (sid, envelopes) in by_session {
        let enriched = enrich_session(envelopes);
        enriched_by_session.insert(sid, enriched);
    }

    // 5. Git + config enrichment (I/O) — writes typed fields to enriched carrier
    apply_git_and_config(&mut enriched_by_session);

    // 5b. T09: persistence-boundary merge — merge typed enriched fields into
    //     envelope.p exactly once, in parity-preserving insertion order.
    //     This must happen BEFORE build_session_rows / build_tool_call_rows /
    //     insert_event, all of which read from envelope.p.
    merge_enriched_into_payloads(&mut enriched_by_session);

    // 6. Build session and tool-call rows BEFORE lineage (so we have cwd/model etc.)
    let mut session_rows = build_session_rows(&enriched_by_session);
    let tool_call_rows = build_tool_call_rows(&enriched_by_session);

    // 7. Cross-session lineage: query DB for recent candidates, score each session
    let candidates = query_lineage_candidates(conn).unwrap_or_default();

    for srow in &mut session_rows {
        // Build a SessionSummary for the current session.
        let new_summary = SessionSummary {
            session_id: srow.session_id.clone(),
            first_event_ts: srow.started_at.as_deref().and_then(parse_ts),
            last_event_ts: srow.ended_at.as_deref().and_then(parse_ts),
            end_reason: srow.end_reason.clone(),
            source: srow.source.clone(),
            cwd: srow.cwd.clone(),
            git_branch: srow.git_branch.clone(),
            model: srow.model.clone(),
        };

        if let Some(parent_match) = find_parent_session(&new_summary, &candidates) {
            srow.parent_session_id = Some(parent_match.parent_session_id.clone());

            // Chain-id rule: use parent's chain_id if set, otherwise parent's session_id.
            let parent_chain_id: Option<String> = conn
                .query_row(
                    "SELECT chain_id FROM sessions WHERE session_id = ?1",
                    rusqlite::params![&parent_match.parent_session_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();

            srow.chain_id =
                Some(parent_chain_id.unwrap_or_else(|| parent_match.parent_session_id.clone()));
        }
    }

    // 8. DB transaction: insert events, upsert sessions, recompute, upsert tool_calls
    let inserted = {
        let tx = conn.transaction()?;

        let mut inserted = 0u64;

        // Collect all enriched events in a flat vec for the insert loop.
        let mut all_events: Vec<&EnrichedEvent> = enriched_by_session.values().flatten().collect();
        all_events.sort_by(|a, b| {
            a.envelope
                .ts
                .cmp(&b.envelope.ts)
                .then_with(|| a.envelope.raw_index.cmp(&b.envelope.raw_index))
        });

        for ev in all_events {
            let n = insert_event(&tx, ev)?;
            inserted += n;
        }

        for srow in &session_rows {
            upsert_session(&tx, srow)?;
        }

        // Recompute counters for all touched sessions.
        let session_ids: Vec<String> = enriched_by_session.keys().cloned().collect();
        let session_id_refs: Vec<&str> = session_ids.iter().map(String::as_str).collect();
        recompute_counters(&tx, &session_id_refs)?;

        for tc in &tool_call_rows {
            upsert_tool_call(&tx, tc)?;
        }

        tx.commit()?;
        inserted
    };

    stats.events_inserted = inserted;
    stats.sessions_touched = enriched_by_session.len() as u64;

    Ok(stats)
}

// ---------------------------------------------------------------------------
// FTS5 rebuild (private)
// ---------------------------------------------------------------------------

/// Rebuild the `events_fts` FTS5 index.
///
/// Mirrors Python (line 1056):
/// ```python
/// conn.execute("INSERT INTO events_fts(events_fts) VALUES('rebuild')")
/// ```
///
/// For an in-memory DB the FTS5 virtual table still accepts the `rebuild`
/// command; it is always safe to call.
fn rebuild_fts(conn: &Connection) -> anyhow::Result<()> {
    conn.execute("INSERT INTO events_fts(events_fts) VALUES('rebuild')", [])
        .context("FTS5 rebuild failed")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// WAL checkpoint (private)
// ---------------------------------------------------------------------------

/// Run a TRUNCATE WAL checkpoint to keep the WAL file small.
///
/// Mirrors Python (line 1059):
/// ```python
/// conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
/// ```
///
/// For an in-memory DB, WAL mode is not active so the PRAGMA is a no-op
/// rather than an error.
fn wal_checkpoint(conn: &Connection) -> anyhow::Result<()> {
    // WAL checkpoint returns one row: (busy, log, checkpointed).
    // Using execute() fails with "Execute returned results" on newer SQLite,
    // so we consume the row via query_row.  On in-memory DBs the PRAGMA is a
    // no-op but still returns a row; we silently ignore any error (e.g. when
    // WAL mode is not active).
    let _: Result<(i64, i64, i64), _> =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        });
    Ok(())
}

// ---------------------------------------------------------------------------
// ingest_all_unprocessed
// ---------------------------------------------------------------------------

/// Find all JSONL files with dates strictly before today, ingest each,
/// archive (gzip) the processed files, and update `.last_ingest`.
///
/// Uses [`IngestLock::try_acquire`] to prevent concurrent ingestion.  If the
/// lock cannot be acquired (another process holds it), logs a warning and
/// returns immediately with `files_processed = 0`.
///
/// ## Python verbatim — `ingest_all_unprocessed` (lines 1067–1127)
///
/// ```python
/// def ingest_all_unprocessed(db_path: str, log_dir: str) -> int:
///     lock_path = TELEMETRY_DIR / ".ingest.lock"
///     TELEMETRY_DIR.mkdir(parents=True, exist_ok=True)
///     lock_fd = open(lock_path, "w")
///     try:
///         try:
///             fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
///         except BlockingIOError:
///             import time
///             deadline = time.monotonic() + 5.0
///             while time.monotonic() < deadline:
///                 try:
///                     fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
///                     break
///                 except BlockingIOError:
///                     time.sleep(0.1)
///             else:
///                 print("[ingest] Another ingestion is running; skipping.")
///                 return 0
///         total = 0
///         log_path = Path(log_dir)
///         today_str = datetime.now(timezone.utc).strftime("%Y-%m-%d")
///         for f in sorted(log_path.glob("hook_logs_*.jsonl")):
///             m = re.search(r"hook_logs_(\d{4}-\d{2}-\d{2})\.jsonl$", f.name)
///             if not m:
///                 continue
///             file_date = m.group(1)
///             if file_date >= today_str:
///                 continue
///             try:
///                 n = ingest_file(db_path, str(f))
///                 total += n
///                 _archive_file(f)
///                 print(f"[ingest] {f.name}: {n} new rows", file=sys.stderr)
///             except Exception as exc:
///                 print(f"[ingest] ERROR processing {f}: {exc}", file=sys.stderr)
///         LAST_INGEST_FILE.write_text(datetime.now(timezone.utc).isoformat())
///         return total
///     finally:
///         fcntl.flock(lock_fd, fcntl.LOCK_UN)
///         lock_fd.close()
/// ```
///
/// ## Differences from Python
///
/// - The Rust version uses [`IngestLock::try_acquire`] (non-blocking single
///   attempt) rather than a 5-second retry loop.  The retry loop is
///   available via [`IngestLock::acquire`] but the task spec says
///   `try_acquire` here.
/// - FTS5 rebuild and WAL checkpoint are deferred to the end of the full run
///   (rather than per-file) for efficiency.
pub fn ingest_all_unprocessed() -> anyhow::Result<IngestAllStats> {
    let mut all_stats = IngestAllStats::default();

    // 1. Acquire exclusive lock
    let _lock = match IngestLock::try_acquire()? {
        Some(lock) => lock,
        None => {
            warn_!("ingest", "Another ingestion is running; skipping.");
            return Ok(all_stats);
        }
    };

    // 2. Open DB and initialise schema (single open via open_db_at).
    let db = db_path();
    let mut conn =
        crate::dbh::open_db_at(&db).with_context(|| format!("open database {:?}", db))?;

    // 3. Determine today's date string (UTC) — files with this date are skipped.
    let today_str = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // 4. Find all unprocessed JSONL files matching hook_logs_YYYY-MM-DD.jsonl
    let log_path = log_dir();

    let candidates: Vec<PathBuf> = {
        let read_dir = std::fs::read_dir(&log_path);
        match read_dir {
            Err(_) => Vec::new(),
            Ok(entries) => {
                let mut paths: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| {
                                n.starts_with("hook_logs_")
                                    && n.ends_with(".jsonl")
                                    && !n.ends_with(".gz")
                            })
                            .unwrap_or(false)
                    })
                    .collect();
                paths.sort();
                paths
            }
        }
    };

    // 5. Process each file
    for path in &candidates {
        // Extract date from filename.
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Match hook_logs_YYYY-MM-DD.jsonl
        let file_date = if file_name.starts_with("hook_logs_") && file_name.ends_with(".jsonl") {
            let inner = &file_name["hook_logs_".len()..file_name.len() - ".jsonl".len()];
            // Validate: must be YYYY-MM-DD (10 chars, digits and dashes)
            if inner.len() == 10 && inner.chars().all(|c| c.is_ascii_digit() || c == '-') {
                Some(inner)
            } else {
                None
            }
        } else {
            None
        };

        let Some(file_date) = file_date else {
            continue;
        };

        // Never ingest today's file — mirrors Python `if file_date >= today_str: continue`.
        if file_date >= today_str.as_str() {
            continue;
        }

        match ingest_file(&mut conn, path) {
            Ok(stats) => {
                info!(
                    "ingest",
                    "{}: {} new rows", file_name, stats.events_inserted
                );
                all_stats.total_events_inserted += stats.events_inserted;
                all_stats.total_events_malformed += stats.events_malformed;
                all_stats.files_processed += 1;

                // Archive the processed file.
                if let Err(e) = archive_jsonl(path) {
                    warn_!("ingest", "archive failed for {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                crate::error_!("ingest", "ERROR processing {}: {}", path.display(), e);
            }
        }
    }

    // 6. FTS5 rebuild (once, after all inserts)
    if let Err(e) = rebuild_fts(&conn) {
        warn_!("ingest", "FTS5 rebuild failed: {}", e);
    }

    // 7. WAL checkpoint
    if let Err(e) = wal_checkpoint(&conn) {
        warn_!("ingest", "WAL checkpoint failed: {}", e);
    }

    // 8. Update .last_ingest marker
    let last_ingest = last_ingest_file();
    if let Some(parent) = last_ingest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now_iso = chrono::Utc::now().to_rfc3339();
    if let Err(e) = std::fs::write(&last_ingest, &now_iso) {
        warn_!("ingest", "failed to write .last_ingest: {}", e);
    }

    // 9. Lock is released when `_lock` drops at end of scope.
    Ok(all_stats)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::TempDir;

    use super::*;
    use crate::schema::SCHEMA_V4_DDL;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Open a fresh in-memory SQLite DB and apply the full v4 schema.
    fn open_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory DB");
        conn.execute_batch(SCHEMA_V4_DDL).expect("apply DDL");
        conn
    }

    /// Build a minimal JSONL line for a given session/event/timestamp.
    fn make_line(session_id: &str, event_type: &str, ts: &str) -> String {
        format!(
            r#"{{"v":1,"ts":"{ts}","p":{{"hook_event_name":"{event_type}","session_id":"{session_id}"}}}}"#
        )
    }

    /// Write lines to a temp `.jsonl` file and return it.
    fn write_jsonl(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create jsonl");
        for line in lines {
            writeln!(f, "{}", line).expect("write line");
        }
        path
    }

    // -----------------------------------------------------------------------
    // insert_or_ignore_rowcount_matches_python
    // -----------------------------------------------------------------------

    /// Plan-explicit test: ingest a fixture file twice.
    /// First run inserts N events (stats.events_inserted == N),
    /// second run inserts 0 (all hashes already exist).
    #[test]
    fn insert_or_ignore_rowcount_matches_python() {
        let tmp = TempDir::new().expect("tempdir");

        let lines = vec![
            make_line("sess-rc", "SessionStart", "2024-01-01T00:00:00.000Z"),
            make_line("sess-rc", "PreToolUse", "2024-01-01T00:00:01.000Z"),
            make_line("sess-rc", "PostToolUse", "2024-01-01T00:00:02.000Z"),
        ];
        let path = write_jsonl(tmp.path(), "test.jsonl", &lines);

        let mut conn = open_db();

        let stats1 = ingest_file(&mut conn, &path).expect("first ingest");
        assert_eq!(stats1.events_inserted, 3, "first run must insert 3 events");

        // Re-create the file (ingest_file doesn't delete it, but we need
        // to re-parse it since it was already read).
        let path2 = write_jsonl(tmp.path(), "test2.jsonl", &lines);
        let stats2 = ingest_file(&mut conn, &path2).expect("second ingest");
        assert_eq!(
            stats2.events_inserted, 0,
            "second run must insert 0 (all hashes already exist)"
        );
    }

    // -----------------------------------------------------------------------
    // ingest_file_happy_path
    // -----------------------------------------------------------------------

    /// 3 envelopes in one session → stats show 3 events, 1 session touched.
    #[test]
    fn ingest_file_happy_path() {
        let tmp = TempDir::new().expect("tempdir");

        let lines = vec![
            make_line("sess-happy", "SessionStart", "2024-01-01T00:00:00.000Z"),
            make_line("sess-happy", "PreToolUse", "2024-01-01T00:00:01.000Z"),
            make_line("sess-happy", "PostToolUse", "2024-01-01T00:00:02.000Z"),
        ];
        let path = write_jsonl(tmp.path(), "happy.jsonl", &lines);

        let mut conn = open_db();
        let stats = ingest_file(&mut conn, &path).expect("ingest");

        assert_eq!(stats.events_inserted, 3, "should insert 3 events");
        assert_eq!(stats.sessions_touched, 1, "should touch 1 session");
        assert_eq!(stats.events_malformed, 0, "no malformed lines");
    }

    // -----------------------------------------------------------------------
    // ingest_file_multi_session
    // -----------------------------------------------------------------------

    /// 3 envelopes across 2 session_ids → 2 sessions touched.
    #[test]
    fn ingest_file_multi_session() {
        let tmp = TempDir::new().expect("tempdir");

        let lines = vec![
            make_line("sess-a", "SessionStart", "2024-01-01T00:00:00.000Z"),
            make_line("sess-b", "SessionStart", "2024-01-01T00:01:00.000Z"),
            make_line("sess-b", "PreToolUse", "2024-01-01T00:01:01.000Z"),
        ];
        let path = write_jsonl(tmp.path(), "multi.jsonl", &lines);

        let mut conn = open_db();
        let stats = ingest_file(&mut conn, &path).expect("ingest");

        assert_eq!(stats.sessions_touched, 2, "should touch 2 sessions");
        assert_eq!(stats.events_inserted, 3, "should insert 3 events");
    }

    // -----------------------------------------------------------------------
    // ingest_file_malformed_tolerated
    // -----------------------------------------------------------------------

    /// Mix of good + malformed lines → good lines inserted, malformed counted,
    /// no error.
    #[test]
    fn ingest_file_malformed_tolerated() {
        let tmp = TempDir::new().expect("tempdir");

        let good1 = make_line("sess-m", "SessionStart", "2024-01-01T00:00:00.000Z");
        let bad = "{bad json line here";
        let good2 = make_line("sess-m", "PreToolUse", "2024-01-01T00:00:01.000Z");

        let path = tmp.path().join("mixed.jsonl");
        {
            let mut f = std::fs::File::create(&path).expect("create");
            writeln!(f, "{}", good1).unwrap();
            writeln!(f, "{}", bad).unwrap();
            writeln!(f, "{}", good2).unwrap();
        }

        let mut conn = open_db();
        let stats = ingest_file(&mut conn, &path).expect("ingest should not error");

        assert_eq!(stats.events_inserted, 2, "good lines inserted");
        assert_eq!(stats.events_malformed, 1, "malformed line counted");
        assert_eq!(stats.sessions_touched, 1, "session touched");
    }

    // -----------------------------------------------------------------------
    // fts_rebuild_executes_without_error
    // -----------------------------------------------------------------------

    /// In-memory DB with full schema; call FTS rebuild; no panic.
    #[test]
    fn fts_rebuild_executes_without_error() {
        let conn = open_db();
        rebuild_fts(&conn).expect("FTS rebuild must not error");
    }

    // -----------------------------------------------------------------------
    // wal_checkpoint_on_in_memory
    // -----------------------------------------------------------------------

    /// WAL checkpoint on in-memory DB is a no-op (not an error).
    #[test]
    fn wal_checkpoint_on_in_memory() {
        let conn = open_db();
        wal_checkpoint(&conn).expect("WAL checkpoint must not error on in-memory DB");
    }

    // -----------------------------------------------------------------------
    // ingest_all_unprocessed_with_lock_held
    // -----------------------------------------------------------------------

    /// Acquire lock manually, call `ingest_all_unprocessed` → should return
    /// gracefully with 0 files processed.
    #[test]
    fn ingest_all_unprocessed_with_lock_held() {
        let tmp = TempDir::new().expect("tempdir");
        let mut result = None;

        crate::test_utils::with_fake_home(tmp.path(), || {
            // Acquire the lock before calling ingest_all_unprocessed.
            let _lock = IngestLock::try_acquire()
                .expect("acquire")
                .expect("should succeed on first");

            result = Some(ingest_all_unprocessed());
        });

        let stats = result
            .unwrap()
            .expect("ingest_all_unprocessed must not error when lock is held");
        assert_eq!(
            stats.files_processed, 0,
            "must process 0 files when lock is already held"
        );
    }

    // -----------------------------------------------------------------------
    // last_ingest_marker_updated
    // -----------------------------------------------------------------------

    /// Before call → marker missing; after call → marker has a current timestamp.
    #[test]
    fn last_ingest_marker_updated() {
        let tmp = TempDir::new().expect("tempdir");
        let mut exists_after = false;

        crate::test_utils::with_fake_home(tmp.path(), || {
            // Ensure the last_ingest file does not exist before the call.
            let marker = crate::paths::last_ingest_file();
            assert!(
                !marker.exists(),
                "last_ingest marker must not exist before ingest run"
            );

            let _ = ingest_all_unprocessed();

            exists_after = marker.exists();
        });

        assert!(
            exists_after,
            "last_ingest marker must be written after ingest run"
        );
    }

    // -----------------------------------------------------------------------
    // ingest_file_sessions_written_to_db
    // -----------------------------------------------------------------------

    /// After ingesting a file, the sessions table should have the expected rows.
    #[test]
    fn ingest_file_sessions_written_to_db() {
        let tmp = TempDir::new().expect("tempdir");

        let lines = vec![make_line(
            "sess-dbwrite",
            "SessionStart",
            "2024-06-01T10:00:00.000Z",
        )];
        let path = write_jsonl(tmp.path(), "dbwrite.jsonl", &lines);

        let mut conn = open_db();
        ingest_file(&mut conn, &path).expect("ingest");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE session_id = 'sess-dbwrite'",
                [],
                |r| r.get(0),
            )
            .expect("query sessions");

        assert_eq!(count, 1, "sessions table must have 1 row for sess-dbwrite");
    }

    // -----------------------------------------------------------------------
    // ingest_file_empty_file
    // -----------------------------------------------------------------------

    /// An empty JSONL file returns stats with all-zero counts, no error.
    #[test]
    fn ingest_file_empty_file() {
        let tmp = TempDir::new().expect("tempdir");
        let path = write_jsonl(tmp.path(), "empty.jsonl", &[]);

        let mut conn = open_db();
        let stats = ingest_file(&mut conn, &path).expect("ingest empty file");

        assert_eq!(stats.events_inserted, 0);
        assert_eq!(stats.events_malformed, 0);
        assert_eq!(stats.sessions_touched, 0);
    }
}
