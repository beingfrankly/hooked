//! Implementation of the `provenance` subcommand.
//!
//! ## Phase 0 Provenance Metrics
//!
//! This subcommand computes two Phase 0 success metrics from live JSONL
//! telemetry.  It does NOT depend on SQLite ingest — it reads raw
//! `hook_logs_*.jsonl` files directly from `~/.claude/telemetry/logs/`.
//!
//! ### METRIC A — Re-exploration overlap
//! For each session that had BOTH explorer reads AND worker reads:
//! ```text
//! overlap = |worker_reads ∩ explorer_reads| / |worker_reads|
//! ```
//! Interpretation: fraction of files the worker read that an explorer already
//! read.  HIGH value → worker is wasting time re-reading.  After the provenance
//! contract is adopted we expect this metric to **DROP** (workers can trust the
//! explorer's read-set instead of re-reading from scratch).
//!
//! ### METRIC B — Coverage-edge incidents
//! Per session: number of file paths the worker **edited** that NO explorer had
//! previously **read**.  HIGH value → worker is modifying files that were never
//! examined by a specialist search agent — risky blind edits.  After the
//! provenance contract is adopted we expect this metric to **DROP** too.
//!
//! ### Phase 0 success gate
//! Run `hooked provenance` before the provenance contract rolls out to capture
//! a baseline.  Run again after adoption.
//! **Success = both metrics trend DOWN.**
//!
//! Only `PreToolUse` events are considered (avoids double-counting with
//! `PostToolUse`).
//!
//! EXPLORER agents: `agent_type` in {"search", "ast-search", "lsp-search"}.
//! WORKER agents:   `agent_type` == "worker".

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use serde_json::Value;

use crate::cli::{OutputFormat, ProvenanceArgs};
use crate::envelope::{Envelope, parse_jsonl_file};
use crate::paths::log_dir;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const EXPLORER_AGENT_TYPES: &[&str] = &["search", "ast-search", "lsp-search"];
const WORKER_AGENT_TYPE: &str = "worker";
const PREFERRED_HOOK_EVENT: &str = "PreToolUse";

// ---------------------------------------------------------------------------
// Public data structures
// ---------------------------------------------------------------------------

/// Per-session provenance metrics.
#[derive(Debug, PartialEq)]
pub struct SessionMetrics {
    pub session_id: String,
    /// Unique file paths read by any EXPLORER agent in this session.
    pub explorer_read_count: usize,
    /// Unique file paths read by any WORKER agent in this session.
    pub worker_read_count: usize,
    /// Metric A: |worker_reads ∩ explorer_reads| / |worker_reads|.
    /// `None` when either set is empty.
    pub reexploration_overlap: Option<f64>,
    /// Unique file paths edited/written by any WORKER agent in this session.
    pub worker_edit_count: usize,
    /// Metric B: worker edits NOT in explorer_reads.
    pub coverage_edge_incidents: usize,
}

/// Full provenance report for a time window.
#[derive(Debug)]
pub struct ProvenanceReport {
    pub days: u32,
    pub log_files_read: Vec<String>,
    pub sessions: Vec<SessionMetrics>,
    pub sessions_with_both: usize,
    /// Mean of per-session `reexploration_overlap` (sessions that had both
    /// explorer and worker reads only).  `None` when no such session exists.
    pub mean_reexploration_overlap: Option<f64>,
    pub total_worker_edits: usize,
    pub total_coverage_edge_incidents: usize,
}

// ---------------------------------------------------------------------------
// Pure computation logic (testable, no I/O)
// ---------------------------------------------------------------------------

/// Compute provenance metrics from an already-parsed slice of envelopes.
///
/// This function performs NO filesystem I/O and never panics.
/// `log_files_read` and `days` are passed through verbatim to the report
/// so the caller can supply them from the filesystem layer.
pub fn compute_metrics(
    envelopes: &[Envelope],
    days: u32,
    log_files_read: Vec<String>,
) -> ProvenanceReport {
    // session_id → (explorer_reads, worker_reads, worker_edits)
    let mut sessions: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)> =
        BTreeMap::new();

    for env in envelopes {
        // Only PreToolUse events.
        match env.p.get("hook_event_name") {
            Some(Value::String(s)) if s == PREFERRED_HOOK_EVENT => {}
            _ => continue,
        }

        let session_id = match env.p.get("session_id") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };

        let agent_type = match env.p.get("agent_type") {
            Some(Value::String(s)) => s.as_str(),
            _ => continue, // absent / null → main thread / orchestrator → skip
        };

        let tool_name = match env.p.get("tool_name") {
            Some(Value::String(s)) => s.as_str(),
            _ => continue,
        };

        // tool_input must be a proper JSON object; skip truncation markers.
        let tool_input = match env.p.get("tool_input") {
            Some(Value::Object(map)) => map,
            _ => continue,
        };
        // Truncation marker: {"_t":…,"_b":…}
        if tool_input.contains_key("_t") || tool_input.contains_key("_b") {
            continue;
        }

        let file_path = match tool_input.get("file_path") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };

        let entry = sessions
            .entry(session_id)
            .or_insert_with(|| (BTreeSet::new(), BTreeSet::new(), BTreeSet::new()));

        let is_explorer = EXPLORER_AGENT_TYPES.contains(&agent_type);
        let is_worker = agent_type == WORKER_AGENT_TYPE;

        if is_explorer {
            match tool_name {
                "Read" | "NotebookRead" => {
                    entry.0.insert(file_path);
                }
                _ => {}
            }
        } else if is_worker {
            match tool_name {
                "Read" | "NotebookRead" => {
                    entry.1.insert(file_path);
                }
                "Edit" | "Write" | "MultiEdit" => {
                    entry.2.insert(file_path);
                }
                _ => {}
            }
        }
        // Other agent types (main thread, orchestrator) → ignored.
    }

    // --- Build per-session metrics ---
    let mut session_rows: Vec<SessionMetrics> = Vec::with_capacity(sessions.len());
    let mut sessions_with_both = 0usize;
    let mut overlap_values: Vec<f64> = Vec::new();
    let mut total_worker_edits = 0usize;
    let mut total_coverage_edge_incidents = 0usize;

    for (session_id, (explorer_reads, worker_reads, worker_edits)) in sessions {
        // Metric A
        let reexploration_overlap = if !explorer_reads.is_empty() && !worker_reads.is_empty() {
            sessions_with_both += 1;
            let intersection = worker_reads.intersection(&explorer_reads).count();
            let overlap = intersection as f64 / worker_reads.len() as f64;
            overlap_values.push(overlap);
            Some(overlap)
        } else {
            None
        };

        // Metric B
        let coverage_edge_incidents = worker_edits.difference(&explorer_reads).count();

        total_worker_edits += worker_edits.len();
        total_coverage_edge_incidents += coverage_edge_incidents;

        session_rows.push(SessionMetrics {
            session_id,
            explorer_read_count: explorer_reads.len(),
            worker_read_count: worker_reads.len(),
            reexploration_overlap,
            worker_edit_count: worker_edits.len(),
            coverage_edge_incidents,
        });
    }

    let mean_reexploration_overlap = if overlap_values.is_empty() {
        None
    } else {
        Some(overlap_values.iter().sum::<f64>() / overlap_values.len() as f64)
    };

    ProvenanceReport {
        days,
        log_files_read,
        sessions: session_rows,
        sessions_with_both,
        mean_reexploration_overlap,
        total_worker_edits,
        total_coverage_edge_incidents,
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

/// Return log file paths whose embedded date falls within the last `days`
/// calendar days (inclusive today), sorted ascending by filename.
fn log_files_for_days(days: u32) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let dir = log_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }

    // Compute the ISO cutoff date string (YYYY-MM-DD).
    // We use the system clock via `chrono` if available; here we compute it
    // from `std::time::SystemTime` to avoid an extra dependency.
    let cutoff_date = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // days_since_epoch for today
        let today_days = secs / 86400;
        let cutoff_days = today_days.saturating_sub((days as u64).saturating_sub(1));
        // Convert back to YYYY-MM-DD
        days_to_date_string(cutoff_days)
    };

    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("read log dir {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| {
            if !p.is_file() {
                return false;
            }
            let name = match p.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => return false,
            };
            if !name.starts_with("hook_logs_") || !name.ends_with(".jsonl") {
                return false;
            }
            // Extract the date portion and compare against the cutoff.
            let date_str = &name["hook_logs_".len()..name.len() - ".jsonl".len()];
            date_str >= cutoff_date.as_str()
        })
        .collect();

    // Ascending by filename (date is lexicographically sortable as YYYY-MM-DD).
    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(paths)
}

/// Convert days-since-Unix-epoch to a "YYYY-MM-DD" string using only std.
fn days_to_date_string(days: u64) -> String {
    // Adapted from the proleptic Gregorian calendar algorithm (Fliegel-van
    // Flandern algorithm via Howard Hinnant's chrono-compatible derivation).
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

// ---------------------------------------------------------------------------
// Public entry-point
// ---------------------------------------------------------------------------

pub fn provenance(args: &ProvenanceArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // 1. Collect log files within the requested window.
    let log_paths = log_files_for_days(args.days)?;

    // 2. Parse all envelopes.
    let mut all_envelopes: Vec<Envelope> = Vec::new();
    for path in &log_paths {
        let result = parse_jsonl_file(path)
            .with_context(|| format!("parse {}", path.display()))?;
        all_envelopes.extend(result.envelopes);
    }

    let log_files_read: Vec<String> = log_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    // 3. Compute metrics (pure, no I/O).
    let report = compute_metrics(&all_envelopes, args.days, log_files_read);

    // 4. Output.
    match fmt {
        OutputFormat::Json => {
            let sessions_json: Vec<serde_json::Value> = report
                .sessions
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "session_id": s.session_id,
                        "explorer_read_count": s.explorer_read_count,
                        "worker_read_count": s.worker_read_count,
                        "reexploration_overlap": s.reexploration_overlap,
                        "worker_edit_count": s.worker_edit_count,
                        "coverage_edge_incidents": s.coverage_edge_incidents,
                    })
                })
                .collect();

            let mean_overlap_rounded = report
                .mean_reexploration_overlap
                .map(|v| (v * 10000.0).round() / 10000.0);

            let output = serde_json::json!({
                "days": report.days,
                "log_files_read": report.log_files_read,
                "sessions": sessions_json,
                "aggregate": {
                    "sessions_with_both": report.sessions_with_both,
                    "mean_reexploration_overlap": mean_overlap_rounded,
                    "total_worker_edits": report.total_worker_edits,
                    "total_coverage_edge_incidents": report.total_coverage_edge_incidents,
                }
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            println!("Provenance metrics (last {} day(s))", report.days);
            println!("Log files read : {}", report.log_files_read.len());
            println!("Sessions found : {}", report.sessions.len());
            println!();
            println!("--- Aggregate ---");
            println!(
                "Sessions with both explorer+worker reads : {}",
                report.sessions_with_both
            );
            match report.mean_reexploration_overlap {
                Some(v) => println!("Mean reexploration overlap (Metric A)    : {:.4}", v),
                None => println!("Mean reexploration overlap (Metric A)    : n/a"),
            }
            println!(
                "Total worker edits                       : {}",
                report.total_worker_edits
            );
            println!(
                "Total coverage-edge incidents (Metric B) : {}",
                report.total_coverage_edge_incidents
            );
            if !report.sessions.is_empty() {
                println!();
                println!("--- Per-session ---");
                for s in &report.sessions {
                    let overlap_str = match s.reexploration_overlap {
                        Some(v) => format!("{:.4}", v),
                        None => "n/a".to_string(),
                    };
                    println!(
                        "  {} | exp_reads={} wrk_reads={} overlap={} wrk_edits={} edge_incidents={}",
                        &s.session_id[..s.session_id.len().min(8)],
                        s.explorer_read_count,
                        s.worker_read_count,
                        overlap_str,
                        s.worker_edit_count,
                        s.coverage_edge_incidents,
                    );
                }
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
    use serde_json::json;

    fn make_envelope(p: serde_json::Value) -> Envelope {
        Envelope {
            v: 1,
            ts: "2024-01-15T10:00:00.000Z".to_string(),
            p,
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        }
    }

    fn pre_tool_use(session_id: &str, agent_type: &str, tool_name: &str, file_path: &str) -> Envelope {
        make_envelope(json!({
            "hook_event_name": "PreToolUse",
            "session_id": session_id,
            "agent_type": agent_type,
            "tool_name": tool_name,
            "tool_input": { "file_path": file_path }
        }))
    }

    #[test]
    fn explorer_and_worker_reads_overlap_computed() {
        // Explorer reads /a, /b, /c.  Worker reads /a, /b, /d.
        // intersection = {/a, /b} → overlap = 2/3
        let envs = vec![
            pre_tool_use("sess-1", "search", "Read", "/a"),
            pre_tool_use("sess-1", "search", "Read", "/b"),
            pre_tool_use("sess-1", "search", "Read", "/c"),
            pre_tool_use("sess-1", "worker", "Read", "/a"),
            pre_tool_use("sess-1", "worker", "Read", "/b"),
            pre_tool_use("sess-1", "worker", "Read", "/d"),
        ];
        let report = compute_metrics(&envs, 7, vec![]);
        assert_eq!(report.sessions.len(), 1);
        let s = &report.sessions[0];
        assert_eq!(s.explorer_read_count, 3);
        assert_eq!(s.worker_read_count, 3);
        let overlap = s.reexploration_overlap.expect("should have overlap");
        let expected = 2.0 / 3.0;
        assert!((overlap - expected).abs() < 1e-9, "overlap={overlap}, expected={expected}");
        assert_eq!(report.sessions_with_both, 1);
    }

    #[test]
    fn coverage_edge_incidents_worker_edit_not_in_explorer_reads() {
        // Explorer reads /a.  Worker edits /a (in explorer reads) and /b (NOT in
        // explorer reads).  coverage_edge_incidents should be 1.
        let envs = vec![
            pre_tool_use("sess-2", "search", "Read", "/a"),
            pre_tool_use("sess-2", "worker", "Edit", "/a"),
            pre_tool_use("sess-2", "worker", "Edit", "/b"),
        ];
        let report = compute_metrics(&envs, 7, vec![]);
        assert_eq!(report.sessions.len(), 1);
        let s = &report.sessions[0];
        assert_eq!(s.coverage_edge_incidents, 1);
        assert_eq!(s.worker_edit_count, 2);
        assert_eq!(report.total_coverage_edge_incidents, 1);
    }

    #[test]
    fn no_explorer_reads_overlap_is_null() {
        // Only worker reads — no explorers.
        let envs = vec![
            pre_tool_use("sess-3", "worker", "Read", "/x"),
            pre_tool_use("sess-3", "worker", "Edit", "/y"),
        ];
        let report = compute_metrics(&envs, 7, vec![]);
        assert_eq!(report.sessions.len(), 1);
        let s = &report.sessions[0];
        assert!(s.reexploration_overlap.is_none(), "overlap should be null");
        assert_eq!(report.sessions_with_both, 0);
        assert!(report.mean_reexploration_overlap.is_none());
    }

    #[test]
    fn dedup_file_paths_within_session() {
        // Same path read twice by explorer and twice by worker → counted once each.
        let envs = vec![
            pre_tool_use("sess-4", "search", "Read", "/dup"),
            pre_tool_use("sess-4", "search", "Read", "/dup"),
            pre_tool_use("sess-4", "worker", "Read", "/dup"),
            pre_tool_use("sess-4", "worker", "Read", "/dup"),
        ];
        let report = compute_metrics(&envs, 7, vec![]);
        let s = &report.sessions[0];
        assert_eq!(s.explorer_read_count, 1);
        assert_eq!(s.worker_read_count, 1);
        let overlap = s.reexploration_overlap.expect("should have overlap");
        assert!((overlap - 1.0).abs() < 1e-9);
    }

    #[test]
    fn truncated_tool_input_skipped() {
        // Truncation marker {"_t":..., "_b":...} — should produce no data.
        let envs = vec![make_envelope(json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-5",
            "agent_type": "worker",
            "tool_name": "Read",
            "tool_input": { "_t": "truncated", "_b": "123" }
        }))];
        let report = compute_metrics(&envs, 7, vec![]);
        assert!(report.sessions.is_empty(), "truncated input should produce no sessions");
    }

    #[test]
    fn post_tool_use_events_ignored() {
        // PostToolUse events must NOT be counted.
        let envs = vec![make_envelope(json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-6",
            "agent_type": "search",
            "tool_name": "Read",
            "tool_input": { "file_path": "/src/foo.rs" }
        }))];
        let report = compute_metrics(&envs, 7, vec![]);
        assert!(report.sessions.is_empty());
    }

    #[test]
    fn multi_session_aggregates() {
        let envs = vec![
            // sess-a: explorer reads /a; worker reads /a, /b → overlap=0.5; worker edits /c → edge=1
            pre_tool_use("sess-a", "ast-search", "Read", "/a"),
            pre_tool_use("sess-a", "worker", "Read", "/a"),
            pre_tool_use("sess-a", "worker", "Read", "/b"),
            pre_tool_use("sess-a", "worker", "Write", "/c"),
            // sess-b: no explorer; worker edits /x, /y → edge=2
            pre_tool_use("sess-b", "worker", "Edit", "/x"),
            pre_tool_use("sess-b", "worker", "Edit", "/y"),
        ];
        let report = compute_metrics(&envs, 7, vec![]);
        assert_eq!(report.sessions.len(), 2);
        assert_eq!(report.sessions_with_both, 1);
        assert_eq!(report.total_worker_edits, 3);
        assert_eq!(report.total_coverage_edge_incidents, 3); // 1 from sess-a + 2 from sess-b
        assert!(report.mean_reexploration_overlap.is_some());
    }

    #[test]
    fn days_to_date_string_known_values() {
        // 1970-01-01 = day 0
        assert_eq!(super::days_to_date_string(0), "1970-01-01");
        // 2024-01-01: days from Unix epoch = 19723
        assert_eq!(super::days_to_date_string(19723), "2024-01-01");
        // 2024-02-29 (leap year): day 19782
        assert_eq!(super::days_to_date_string(19782), "2024-02-29");
    }
}
