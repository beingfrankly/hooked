//! Implementation of the `replay` subcommand.
//!
//! Mirrors Python `cmd_replay` in query.py:
//!
//! ```python
//! def cmd_replay(args: argparse.Namespace) -> None:
//!     fmt = _resolve_format(args)
//!     failed_p = TELEMETRY_DIR / "failed_events.jsonl"
//!     if not failed_p.exists():
//!         print("No failed_events.jsonl found.")
//!         return
//!
//!     rows = []
//!     with open(failed_p, "r") as fh:
//!         for i, line in enumerate(fh):
//!             line = line.strip()
//!             if not line:
//!                 continue
//!             try:
//!                 envelope = json.loads(line)
//!                 payload = envelope.get("p", {})
//!                 rows.append({
//!                     "line": i + 1,
//!                     "ts": envelope.get("ts", "")[:19],
//!                     "session_id": (payload.get("session_id") or "")[:8],
//!                     "event_type": payload.get("hook_event_name", ""),
//!                     "raw": _truncate(line, 80),
//!                 })
//!             except json.JSONDecodeError:
//!                 rows.append({
//!                     "line": i + 1,
//!                     "ts": "",
//!                     "session_id": "",
//!                     "event_type": "MALFORMED",
//!                     "raw": _truncate(line, 80),
//!                 })
//!
//!     print(f"failed_events.jsonl: {len(rows)} entries")
//!     headers = ["line", "ts", "session_id", "event_type", "raw"]
//!     _render(rows, fmt, headers)
//! ```
//!
//! This command inspects `~/.claude/telemetry/failed_events.jsonl` — the
//! fallback file used when DB writes fail during ingestion.

use crate::cli::{OutputFormat, ReplayArgs};
use crate::paths::telemetry_dir;
use crate::render::{Cell, Row, Table};

/// Truncate string to n chars with ellipsis.
/// Mirrors Python `_truncate`.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let truncated: String = s.chars().take(n).collect();
        format!("{}…", truncated)
    } else {
        s.to_string()
    }
}

/// A parsed row from failed_events.jsonl.
pub struct ReplayRow {
    pub line: i64,
    pub ts: String,
    pub session_id: String,
    pub event_type: String,
    pub raw: String,
}

/// Parse failed_events.jsonl and return all rows.
///
/// Returns `None` if the file does not exist.
pub fn parse_failed_events(content: &str) -> Vec<ReplayRow> {
    let mut rows = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(envelope) => {
                let ts = envelope
                    .get("ts")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(19)
                    .collect::<String>();

                let session_id = envelope
                    .get("p")
                    .and_then(|p| p.get("session_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(8)
                    .collect::<String>();

                let event_type = envelope
                    .get("p")
                    .and_then(|p| p.get("hook_event_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                rows.push(ReplayRow {
                    line: (i + 1) as i64,
                    ts,
                    session_id,
                    event_type,
                    raw: truncate(line, 80),
                });
            }
            Err(_) => {
                rows.push(ReplayRow {
                    line: (i + 1) as i64,
                    ts: String::new(),
                    session_id: String::new(),
                    event_type: "MALFORMED".to_string(),
                    raw: truncate(line, 80),
                });
            }
        }
    }

    rows
}

/// Build the Table from replay rows.
/// Mirrors Python's `cmd_replay` display logic.
pub fn build_table(rows: Vec<ReplayRow>) -> Table {
    let headers = vec![
        "line".to_string(),
        "ts".to_string(),
        "session_id".to_string(),
        "event_type".to_string(),
        "raw".to_string(),
    ];
    let data_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            vec![
                Cell::Int(r.line),
                Cell::Str(r.ts),
                Cell::Str(r.session_id),
                Cell::Str(r.event_type),
                Cell::Str(r.raw),
            ]
        })
        .collect();
    Table::new(headers, data_rows)
}

pub fn replay(args: &ReplayArgs, fmt: &OutputFormat) -> anyhow::Result<()> {
    let _ = args; // no args currently used
    let failed_path = telemetry_dir().join("failed_events.jsonl");

    // Mirrors Python: if not failed_p.exists(): print("No failed_events.jsonl found.")
    if !failed_path.exists() {
        println!("No failed_events.jsonl found.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&failed_path)?;
    let rows = parse_failed_events(&content);

    // Mirrors Python: print(f"failed_events.jsonl: {len(rows)} entries")
    println!("failed_events.jsonl: {} entries", rows.len());

    let table = build_table(rows);
    print!("{}", table.render(fmt));

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::OutputFormat;

    #[test]
    fn parses_valid_envelope() {
        let content = r#"{"v":1,"ts":"2024-01-15T10:00:00Z","p":{"hook_event_name":"SessionStart","session_id":"abc123def456789"}}"#;
        let rows = parse_failed_events(content);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, 1);
        assert_eq!(rows[0].ts, "2024-01-15T10:00:00");
        assert_eq!(rows[0].session_id, "abc123de");
        assert_eq!(rows[0].event_type, "SessionStart");
    }

    #[test]
    fn parses_malformed_json() {
        let content = "this is not json";
        let rows = parse_failed_events(content);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_type, "MALFORMED");
        assert_eq!(rows[0].ts, "");
        assert_eq!(rows[0].session_id, "");
    }

    #[test]
    fn skips_empty_lines() {
        let content = "\n\n{\"ts\":\"2024-01-15T10:00:00Z\",\"p\":{\"hook_event_name\":\"PreToolUse\",\"session_id\":\"s1\"}}\n\n";
        let rows = parse_failed_events(content);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn handles_multiple_lines() {
        let content = concat!(
            "{\"ts\":\"2024-01-15T10:00:00Z\",\"p\":{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s1\"}}\n",
            "not valid json\n",
            "{\"ts\":\"2024-01-15T10:01:00Z\",\"p\":{\"hook_event_name\":\"PreToolUse\",\"session_id\":\"s1\"}}\n",
        );
        let rows = parse_failed_events(content);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].event_type, "SessionStart");
        assert_eq!(rows[1].event_type, "MALFORMED");
        assert_eq!(rows[2].event_type, "PreToolUse");
        // Line numbers are sequential (including blank/malformed)
        assert_eq!(rows[0].line, 1);
        assert_eq!(rows[1].line, 2);
        assert_eq!(rows[2].line, 3);
    }

    #[test]
    fn builds_table_with_expected_headers() {
        let table = build_table(vec![]);
        assert_eq!(
            table.headers,
            vec!["line", "ts", "session_id", "event_type", "raw"]
        );
    }

    #[test]
    fn handles_empty_result() {
        let table = build_table(vec![]);
        let out = table.render(&OutputFormat::Table);
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn truncates_raw_to_80_chars() {
        let long_line = "a".repeat(100);
        let content = long_line.as_str(); // not valid JSON so becomes MALFORMED
        let rows = parse_failed_events(content);
        assert_eq!(rows.len(), 1);
        // raw should be truncated to 80 chars + ellipsis = 81 chars
        assert_eq!(rows[0].raw.chars().count(), 81);
        assert!(rows[0].raw.ends_with('…'));
    }

    #[test]
    fn no_failed_events_for_empty_content() {
        let rows = parse_failed_events("");
        assert!(rows.is_empty());
    }
}
