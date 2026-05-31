//! Implementation of the `gate` subcommand.
//!
//! Runs as a synchronous PreToolUse hook.  Reads the hook payload from stdin,
//! checks whether the target file is within the session's observed exploration
//! coverage, and emits a `hookSpecificOutput` warning (once per file, per
//! session) when the worker is editing a file that no explorer agent has
//! previously examined.
//!
//! ## Behavior contract
//! - NEVER blocks. Always exits 0.
//! - On ANY error (bad stdin, unreadable logs, etc.) → exits 0 with no output
//!   (fail-open).
//! - On `Warn`: emits the `hookSpecificOutput` JSON to stdout, then exits 0.
//! - On `Allow`: no stdout output.
//! - Only `agent_type == "worker"` triggers evaluation; all others are silently
//!   allowed.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::cli::{GateArgs, OutputFormat};
use crate::envelope::{Envelope, parse_jsonl_file};
use crate::paths::{log_dir, telemetry_dir};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const EXPLORER_AGENT_TYPES: &[&str] = &["search", "ast-search", "lsp-search"];
const WORKER_AGENT_TYPE: &str = "worker";
const EXPLORER_READ_TOOLS: &[&str] = &["Read", "NotebookRead", "Glob", "Grep"];

// ---------------------------------------------------------------------------
// Pure decision types
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum GateDecision {
    Allow,
    Warn(String),
}

// ---------------------------------------------------------------------------
// Pure decision function (testable, no I/O)
// ---------------------------------------------------------------------------

/// Determine whether a worker edit of `target` should be warned about.
///
/// Arguments:
/// - `target`: the file path the worker intends to edit/write.
/// - `covered_files`: set of file paths that any explorer agent touched.
/// - `covered_dirs`: set of parent directories of covered files.
/// - `already_warned`: set of paths already warned this session (warn-once).
/// - `any_explorer_activity`: true when at least one explorer read was logged.
pub fn gate_decision(
    target: &str,
    covered_files: &BTreeSet<String>,
    covered_dirs: &BTreeSet<String>,
    already_warned: &BTreeSet<String>,
    any_explorer_activity: bool,
) -> GateDecision {
    // Gate is inactive when no explorer has done anything this session.
    if !any_explorer_activity {
        return GateDecision::Allow;
    }

    // File is covered directly.
    if covered_files.contains(target) {
        return GateDecision::Allow;
    }

    // File is under a covered directory.
    let target_path = Path::new(target);
    for dir in covered_dirs {
        if target_path.starts_with(dir) {
            return GateDecision::Allow;
        }
    }

    // Already warned about this file — don't repeat.
    if already_warned.contains(target) {
        return GateDecision::Allow;
    }

    // Not covered, not warned → emit warning.
    let message = format!(
        "Coverage gate: worker is editing '{}' which is outside the session's observed \
exploration coverage. No explorer agent (search/ast-search/lsp-search) read this file \
or its parent directory during this session. Per the handoff-provenance contract, \
unknown/not-clear files should be explored first. If this is intentional, proceed.",
        target
    );
    GateDecision::Warn(message)
}

// ---------------------------------------------------------------------------
// File-system helpers
// ---------------------------------------------------------------------------

/// Return up to `n` most recent `hook_logs_*.jsonl` paths in the log dir,
/// sorted by filename descending (filename embeds the date: YYYY-MM-DD).
fn recent_log_files(n: usize) -> Vec<std::path::PathBuf> {
    let dir = log_dir();
    if !dir.exists() {
        return vec![];
    }

    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return vec![];
    };

    let mut paths: Vec<std::path::PathBuf> = read_dir
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("hook_logs_") && n.ends_with(".jsonl"))
                    .unwrap_or(false)
        })
        .collect();

    // Descending by filename (date embedded as YYYY-MM-DD → lexicographic works)
    paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    paths.truncate(n);
    paths
}

/// Parse envelopes from a list of JSONL paths. Silently ignores failures.
fn load_envelopes(paths: &[std::path::PathBuf]) -> Vec<Envelope> {
    let mut all = Vec::new();
    for path in paths {
        if let Ok(result) = parse_jsonl_file(path) {
            all.extend(result.envelopes);
        }
    }
    all
}

/// Scan envelopes for a given session_id and collect the set of files any
/// explorer agent touched via read-family tools, plus their parent dirs.
/// Returns (covered_files, covered_dirs, any_explorer_activity).
fn compute_coverage(
    envelopes: &[Envelope],
    session_id: &str,
) -> (BTreeSet<String>, BTreeSet<String>, bool) {
    let mut covered_files: BTreeSet<String> = BTreeSet::new();

    for env in envelopes {
        // Only PreToolUse events.
        match env.p.get("hook_event_name") {
            Some(Value::String(s)) if s == "PreToolUse" => {}
            _ => continue,
        }

        // Only events from this session.
        match env.p.get("session_id") {
            Some(Value::String(s)) if s == session_id => {}
            _ => continue,
        }

        // Only explorer agent types.
        let agent_type = match env.p.get("agent_type") {
            Some(Value::String(s)) => s.as_str(),
            _ => continue,
        };
        if !EXPLORER_AGENT_TYPES.contains(&agent_type) {
            continue;
        }

        // Only read-family tools.
        let tool_name = match env.p.get("tool_name") {
            Some(Value::String(s)) => s.as_str(),
            _ => continue,
        };
        if !EXPLORER_READ_TOOLS.contains(&tool_name) {
            continue;
        }

        // Extract file_path from tool_input.
        let tool_input = match env.p.get("tool_input") {
            Some(Value::Object(map)) => map,
            _ => continue,
        };

        if let Some(Value::String(path)) = tool_input.get("file_path") {
            if !path.is_empty() {
                covered_files.insert(path.clone());
            }
        }
    }

    let any_explorer_activity = !covered_files.is_empty();

    let covered_dirs: BTreeSet<String> = covered_files
        .iter()
        .filter_map(|p| {
            Path::new(p)
                .parent()
                .and_then(|d| d.to_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
        .collect();

    (covered_files, covered_dirs, any_explorer_activity)
}

/// Path to the warned-state file for a given session.
fn warned_file_path(session_id: &str) -> std::path::PathBuf {
    telemetry_dir()
        .join("coverage")
        .join(format!("gate-warned-{}.json", session_id))
}

/// Load the set of already-warned paths from disk. Returns empty set on any
/// error (treat missing/unparseable as empty).
fn load_warned(session_id: &str) -> BTreeSet<String> {
    let path = warned_file_path(session_id);
    let Ok(bytes) = std::fs::read(&path) else {
        return BTreeSet::new();
    };
    let Ok(val) = serde_json::from_slice::<Value>(&bytes) else {
        return BTreeSet::new();
    };
    match val {
        Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| {
                if let Value::String(s) = v {
                    Some(s)
                } else {
                    None
                }
            })
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// Persist the warned set back to disk. Silently ignores failures.
fn save_warned(session_id: &str, warned: &BTreeSet<String>) {
    let path = warned_file_path(session_id);
    // Create coverage dir if needed.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let arr: Vec<Value> = warned
        .iter()
        .map(|s| Value::String(s.clone()))
        .collect();
    let Ok(json) = serde_json::to_string(&Value::Array(arr)) else {
        return;
    };
    let _ = std::fs::write(&path, json);
}

// ---------------------------------------------------------------------------
// Public entry-point
// ---------------------------------------------------------------------------

pub fn gate(_args: &GateArgs, _fmt: &OutputFormat) -> anyhow::Result<()> {
    // Wrap entire body so any error → silent allow (fail open).
    let result = gate_inner();
    if let Err(_e) = result {
        // Fail open: exit 0, no stdout.
    }
    Ok(())
}

fn gate_inner() -> anyhow::Result<()> {
    // 1. Read PreToolUse payload from stdin.
    let stdin = std::io::stdin();
    let payload: Value = serde_json::from_reader(stdin.lock())?;

    // 2. Extract fields we need.
    let tool_name = match payload.get("tool_name") {
        Some(Value::String(s)) => s.clone(),
        _ => return Ok(()), // allow silently
    };
    // We only care about edit-family tools; guard against unexpected invocations.
    let _ = tool_name; // present in payload; retained for clarity

    let agent_type = match payload.get("agent_type") {
        Some(Value::String(s)) => s.clone(),
        _ => return Ok(()), // allow silently (non-worker)
    };

    // 3. Fast path: only worker agents are evaluated.
    if agent_type != WORKER_AGENT_TYPE {
        return Ok(());
    }

    let session_id = match payload.get("session_id") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Ok(()), // no session_id → allow
    };

    let file_path = match payload
        .get("tool_input")
        .and_then(|v| v.get("file_path"))
    {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Ok(()), // no file_path → allow
    };

    // 4. Load coverage from recent JSONL logs (today + previous 1 day → 2 files).
    let log_paths = recent_log_files(2);
    let envelopes = load_envelopes(&log_paths);
    let (covered_files, covered_dirs, any_explorer_activity) =
        compute_coverage(&envelopes, &session_id);

    // 5. Load warned state.
    let mut warned = load_warned(&session_id);

    // 6. Pure decision.
    let decision = gate_decision(
        &file_path,
        &covered_files,
        &covered_dirs,
        &warned,
        any_explorer_activity,
    );

    // 7. Act on decision.
    match decision {
        GateDecision::Allow => {
            // No output.
        }
        GateDecision::Warn(message) => {
            // Record so we only warn once.
            warned.insert(file_path.clone());
            save_warned(&session_id, &warned);

            // Emit hookSpecificOutput JSON.
            let output = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "additionalContext": message
                }
            });
            println!("{}", serde_json::to_string(&output)?);
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

    fn btree(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn target_in_covered_files_allows() {
        let covered_files = btree(&["/src/lib.rs", "/src/main.rs"]);
        let covered_dirs = btree(&["/src"]);
        let already_warned = btree(&[]);

        let decision = gate_decision(
            "/src/lib.rs",
            &covered_files,
            &covered_dirs,
            &already_warned,
            true,
        );
        assert_eq!(decision, GateDecision::Allow);
    }

    #[test]
    fn target_under_covered_dir_allows() {
        let covered_files = btree(&["/src/lib.rs"]);
        let covered_dirs = btree(&["/src"]);
        let already_warned = btree(&[]);

        // /src/new_file.rs is under /src which is covered
        let decision = gate_decision(
            "/src/new_file.rs",
            &covered_files,
            &covered_dirs,
            &already_warned,
            true,
        );
        assert_eq!(decision, GateDecision::Allow);
    }

    #[test]
    fn target_outside_coverage_with_explorer_activity_warns() {
        let covered_files = btree(&["/src/lib.rs"]);
        let covered_dirs = btree(&["/src"]);
        let already_warned = btree(&[]);

        let decision = gate_decision(
            "/other/module.rs",
            &covered_files,
            &covered_dirs,
            &already_warned,
            true,
        );
        match decision {
            GateDecision::Warn(msg) => {
                assert!(
                    msg.contains("/other/module.rs"),
                    "warning should mention the target path"
                );
            }
            GateDecision::Allow => panic!("expected Warn, got Allow"),
        }
    }

    #[test]
    fn target_outside_but_already_warned_allows() {
        let covered_files = btree(&["/src/lib.rs"]);
        let covered_dirs = btree(&["/src"]);
        let already_warned = btree(&["/other/module.rs"]);

        let decision = gate_decision(
            "/other/module.rs",
            &covered_files,
            &covered_dirs,
            &already_warned,
            true,
        );
        assert_eq!(decision, GateDecision::Allow, "should not warn twice");
    }

    #[test]
    fn no_explorer_activity_gate_inactive_allows() {
        // Even if file is "outside" (nothing covered), gate is inactive.
        let covered_files = btree(&[]);
        let covered_dirs = btree(&[]);
        let already_warned = btree(&[]);

        let decision = gate_decision(
            "/any/file.rs",
            &covered_files,
            &covered_dirs,
            &already_warned,
            false, // no explorer activity
        );
        assert_eq!(decision, GateDecision::Allow);
    }

    #[test]
    fn no_explorer_activity_even_with_warned_state_allows() {
        // any_explorer_activity=false is the gate-inactive guard, takes precedence.
        let covered_files = btree(&[]);
        let covered_dirs = btree(&[]);
        let already_warned = btree(&[]);

        let decision = gate_decision(
            "/brand/new.rs",
            &covered_files,
            &covered_dirs,
            &already_warned,
            false,
        );
        assert_eq!(decision, GateDecision::Allow);
    }
}
