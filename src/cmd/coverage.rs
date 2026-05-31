//! Implementation of the `coverage` subcommand.
//!
//! Derives an agent's observed file/pattern coverage from LIVE JSONL telemetry
//! (does NOT depend on SQLite ingest).  Reads the 3 most recent
//! `hook_logs_*.jsonl` files from `~/.claude/telemetry/logs/`, filters
//! envelopes by `agent_id`, and aggregates:
//!
//! - `reads`  — unique `file_path` values from Read / NotebookRead tool calls
//! - `globs`  — unique `pattern` values from Glob tool calls
//! - `greps`  — unique `pattern` values from Grep tool calls
//! - `dirs`   — unique parent directories of every path in `reads`

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use serde_json::Value;

use crate::cli::{CoverageArgs, OutputFormat};
use crate::envelope::{Envelope, parse_jsonl_file};
use crate::paths::log_dir;

// ---------------------------------------------------------------------------
// Public data structure
// ---------------------------------------------------------------------------

/// Aggregated coverage data for a single agent.
#[derive(Debug, Default, PartialEq)]
pub struct Coverage {
    pub reads: Vec<String>,
    pub globs: Vec<String>,
    pub greps: Vec<String>,
    pub dirs: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pure extraction logic (testable, no I/O)
// ---------------------------------------------------------------------------

/// Extract coverage from a slice of envelopes, filtering by `agent_id`.
///
/// Deduplicates each set and sorts for deterministic output.  Defensive
/// against missing / non-object `tool_input` fields.
pub fn extract_coverage(envelopes: &[Envelope], agent_id: &str) -> Coverage {
    let mut reads: BTreeSet<String> = BTreeSet::new();
    let mut globs: BTreeSet<String> = BTreeSet::new();
    let mut greps: BTreeSet<String> = BTreeSet::new();

    for env in envelopes {
        // Filter by agent_id
        match env.p.get("agent_id") {
            Some(Value::String(id)) if id == agent_id => {}
            _ => continue,
        }

        let tool_name = match env.p.get("tool_name") {
            Some(Value::String(s)) => s.as_str(),
            _ => continue,
        };

        // tool_input must be a JSON object (not absent, not a string, not a
        // truncation marker like {"_t":…,"_b":…}).
        let tool_input = match env.p.get("tool_input") {
            Some(Value::Object(map)) => map,
            _ => continue,
        };

        match tool_name {
            "Read" | "NotebookRead" => {
                if let Some(Value::String(path)) = tool_input.get("file_path") {
                    reads.insert(path.clone());
                }
            }
            "Glob" => {
                if let Some(Value::String(pat)) = tool_input.get("pattern") {
                    globs.insert(pat.clone());
                }
            }
            "Grep" => {
                if let Some(Value::String(pat)) = tool_input.get("pattern") {
                    greps.insert(pat.clone());
                }
            }
            _ => {}
        }
    }

    // Derive dirs from reads
    let dirs: BTreeSet<String> = reads
        .iter()
        .filter_map(|p| {
            Path::new(p)
                .parent()
                .and_then(|d| d.to_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
        .collect();

    Coverage {
        reads: reads.into_iter().collect(),
        globs: globs.into_iter().collect(),
        greps: greps.into_iter().collect(),
        dirs: dirs.into_iter().collect(),
    }
}

// ---------------------------------------------------------------------------
// File-system helpers
// ---------------------------------------------------------------------------

/// Return up to `n` most recent `hook_logs_*.jsonl` paths in the log dir,
/// sorted by filename descending (filename embeds the date: YYYY-MM-DD).
fn recent_log_files(n: usize) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let dir = log_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("read log dir {}", dir.display()))?
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
    Ok(paths)
}

// ---------------------------------------------------------------------------
// Public entry-point
// ---------------------------------------------------------------------------

pub fn coverage(args: &CoverageArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    // 1. Collect envelopes from the 3 most recent JSONL files.
    let log_files = recent_log_files(3)?;

    let mut all_envelopes: Vec<Envelope> = Vec::new();
    for path in &log_files {
        let result = parse_jsonl_file(path)
            .with_context(|| format!("parse {}", path.display()))?;
        all_envelopes.extend(result.envelopes);
    }

    // 2. Extract coverage for the requested agent_id.
    let cov = extract_coverage(&all_envelopes, &args.agent_id);

    // 3. Build the output JSON object.
    let json_val = serde_json::json!({
        "agent_id": args.agent_id,
        "reads": cov.reads,
        "globs": cov.globs,
        "greps": cov.greps,
        "dirs": cov.dirs,
    });

    // 4. Output.
    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&json_val)?);
        }
        _ => {
            println!("agent_id : {}", args.agent_id);
            println!("reads    : {} file(s)", cov.reads.len());
            for r in &cov.reads {
                println!("  {r}");
            }
            println!("globs    : {} pattern(s)", cov.globs.len());
            for g in &cov.globs {
                println!("  {g}");
            }
            println!("greps    : {} pattern(s)", cov.greps.len());
            for g in &cov.greps {
                println!("  {g}");
            }
            println!("dirs     : {} director(ies)", cov.dirs.len());
            for d in &cov.dirs {
                println!("  {d}");
            }
        }
    }

    // 5. Optionally write JSON to disk.
    if args.write {
        let coverage_dir = crate::paths::telemetry_dir().join("coverage");
        std::fs::create_dir_all(&coverage_dir)
            .with_context(|| format!("create coverage dir {}", coverage_dir.display()))?;
        let out_path = coverage_dir.join(format!("{}.json", args.agent_id));
        std::fs::write(&out_path, serde_json::to_string_pretty(&json_val)?)
            .with_context(|| format!("write coverage file {}", out_path.display()))?;
        eprintln!("wrote {}", out_path.display());
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

    #[test]
    fn matching_read_collected() {
        let envs = vec![make_envelope(json!({
            "agent_id": "agent-abc",
            "tool_name": "Read",
            "tool_input": { "file_path": "/home/user/foo.rs" }
        }))];
        let cov = extract_coverage(&envs, "agent-abc");
        assert_eq!(cov.reads, vec!["/home/user/foo.rs"]);
        assert!(cov.globs.is_empty());
        assert!(cov.greps.is_empty());
    }

    #[test]
    fn non_matching_agent_excluded() {
        let envs = vec![make_envelope(json!({
            "agent_id": "agent-xyz",
            "tool_name": "Read",
            "tool_input": { "file_path": "/home/user/bar.rs" }
        }))];
        let cov = extract_coverage(&envs, "agent-abc");
        assert!(cov.reads.is_empty());
        assert!(cov.dirs.is_empty());
    }

    #[test]
    fn truncated_tool_input_skipped() {
        // A truncation marker object: {"_t": "...", "_b": "..."} is still an
        // object, but it won't have "file_path" → we just get no result (no panic).
        let envs = vec![make_envelope(json!({
            "agent_id": "agent-abc",
            "tool_name": "Read",
            "tool_input": { "_t": "truncated", "_b": "123" }
        }))];
        let cov = extract_coverage(&envs, "agent-abc");
        assert!(cov.reads.is_empty());

        // A string tool_input (another degenerate form) is also skipped.
        let envs2 = vec![make_envelope(json!({
            "agent_id": "agent-abc",
            "tool_name": "Read",
            "tool_input": "some string"
        }))];
        let cov2 = extract_coverage(&envs2, "agent-abc");
        assert!(cov2.reads.is_empty());
    }

    #[test]
    fn dedup_and_dir_derivation() {
        let envs = vec![
            make_envelope(json!({
                "agent_id": "agent-abc",
                "tool_name": "Read",
                "tool_input": { "file_path": "/src/lib.rs" }
            })),
            // duplicate — should appear only once
            make_envelope(json!({
                "agent_id": "agent-abc",
                "tool_name": "Read",
                "tool_input": { "file_path": "/src/lib.rs" }
            })),
            make_envelope(json!({
                "agent_id": "agent-abc",
                "tool_name": "Read",
                "tool_input": { "file_path": "/src/main.rs" }
            })),
        ];
        let cov = extract_coverage(&envs, "agent-abc");
        assert_eq!(cov.reads, vec!["/src/lib.rs", "/src/main.rs"]);
        // Both share the same parent dir → dirs should deduplicate to one entry.
        assert_eq!(cov.dirs, vec!["/src"]);
    }

    #[test]
    fn glob_and_grep_collected() {
        let envs = vec![
            make_envelope(json!({
                "agent_id": "agent-abc",
                "tool_name": "Glob",
                "tool_input": { "pattern": "**/*.rs" }
            })),
            make_envelope(json!({
                "agent_id": "agent-abc",
                "tool_name": "Grep",
                "tool_input": { "pattern": "fn main" }
            })),
            // duplicate glob
            make_envelope(json!({
                "agent_id": "agent-abc",
                "tool_name": "Glob",
                "tool_input": { "pattern": "**/*.rs" }
            })),
        ];
        let cov = extract_coverage(&envs, "agent-abc");
        assert_eq!(cov.globs, vec!["**/*.rs"]);
        assert_eq!(cov.greps, vec!["fn main"]);
    }

    #[test]
    fn notebook_read_collected() {
        let envs = vec![make_envelope(json!({
            "agent_id": "agent-abc",
            "tool_name": "NotebookRead",
            "tool_input": { "file_path": "/notebooks/analysis.ipynb" }
        }))];
        let cov = extract_coverage(&envs, "agent-abc");
        assert_eq!(cov.reads, vec!["/notebooks/analysis.ipynb"]);
        assert_eq!(cov.dirs, vec!["/notebooks"]);
    }

    #[test]
    fn missing_tool_input_skipped() {
        let envs = vec![make_envelope(json!({
            "agent_id": "agent-abc",
            "tool_name": "Read"
            // no tool_input key
        }))];
        let cov = extract_coverage(&envs, "agent-abc");
        assert!(cov.reads.is_empty());
    }
}
