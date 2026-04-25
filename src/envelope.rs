//! V1 envelope types, JSONL parser (gzip + plain), and JSON serialization helpers.
//!
//! ## Wire format
//!
//! Each line in a `.jsonl` (or `.jsonl.gz`) file is a JSON object of the form:
//!
//! ```text
//! {"v": 1, "ts": "ISO8601", "p": { ... payload ... }, "h": "optional_hash"}
//! ```
//!
//! The `p` (payload) object always contains at minimum `hook_event_name` and
//! `session_id`.  The `h` field carries a pre-computed event hash emitted by
//! `capture.sh` (optional).
//!
//! This module mirrors the Python `_parse_jsonl_file` function in
//! `~/.claude/telemetry/ingest.py`.
//!
//! ## JSON serialization helpers
//!
//! Two public helpers are provided, with distinct semantics:
//!
//! - [`canonicalize`] — Rust-internal canonical form.  Sorts object keys
//!   lexicographically at every level (matching Python `sort_keys=True`),
//!   escapes non-ASCII as `\uXXXX` / surrogate pairs (matching Python
//!   `ensure_ascii=True`), and uses minimal separators.  Useful for
//!   deterministic hashing and canonical `raw_payload` storage where we control
//!   both sides.
//!
//! - [`python_json_compact`] — Python-parity form.  Preserves insertion order
//!   of object keys (matching Python 3.7+ dict ordering after `json.loads`),
//!   escapes non-ASCII identically to `canonicalize`, and uses minimal
//!   separators.  Use this whenever bytes must match Python's
//!   `json.dumps(value, separators=(",", ":"))` output — e.g. for DB storage
//!   strings and byte-count columns.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::Context;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A parsed v1 telemetry envelope.
///
/// Wire fields (`v`, `ts`, `p`, `h`) are deserialized from JSON.
/// The `raw_index` and `raw_line` fields are populated by the parser and are
/// skipped during serialization.
///
/// Python source: `_parse_jsonl_file` in `ingest.py`, lines 648-702.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Schema version -- always `1` for the current format.
    pub v: u32,

    /// ISO 8601 timestamp with sub-second precision (verbatim from the wire).
    pub ts: String,

    /// Raw payload object.  Always a JSON object (`Value::Object`).
    ///
    /// Canonical payload fields: `hook_event_name`, `session_id`, `tool_name`,
    /// `tool_use_id`, `tool_input`, `tool_response`, `result_size`,
    /// `duration_ms`, `error`, `is_interrupt`, `prompt`, `prompt_text`,
    /// `prompt_length`, `agent_id`, `agent_type`, `model`, `permission_mode`,
    /// `cwd`, `notification_type`, `stop_hook_active`, `skill_name`,
    /// `skill_type`, `task_id`, `task_subject`, `teammate_name`.
    pub p: Value,

    /// Optional pre-computed event hash written by `capture.sh`.
    /// Maps to Python `envelope.get("h")`.
    #[serde(default)]
    pub h: Option<String>,

    // Internal fields -- not serialized
    /// 0-based line number within the source file (counting every line
    /// including blank ones, mirroring Python's `enumerate(fh)`).
    #[serde(skip)]
    pub raw_index: u64,

    /// Verbatim text of the JSON line before parsing.
    #[serde(skip)]
    pub raw_line: String,
}

/// A line that could not be parsed as a valid [`Envelope`].
#[derive(Debug, Clone)]
pub struct MalformedLine {
    /// 0-based line number (same counting as [`Envelope::raw_index`]).
    pub raw_index: u64,
    /// Verbatim line text.
    pub line: String,
    /// Human-readable error description.
    pub error: String,
}

/// The outcome of parsing a JSONL file.
#[derive(Debug, Default)]
pub struct ParseResult {
    /// Successfully parsed envelopes, in file order.
    pub envelopes: Vec<Envelope>,
    /// Lines that failed to parse (JSON errors, non-object payloads, missing
    /// required fields), in file order.
    pub malformed: Vec<MalformedLine>,
}

// ---------------------------------------------------------------------------
// JSONL parser
// ---------------------------------------------------------------------------

/// Parse a JSONL file of v1 telemetry envelopes.
///
/// * If `path` ends with `.gz`, the file is decompressed via
///   [`flate2::read::GzDecoder`] before reading.
/// * Lines are read one at a time via [`BufReader`].
/// * Blank lines are skipped (they still consume a `raw_index` count,
///   mirroring Python's `enumerate(fh)` / `if not line: continue`).
/// * Parse failures are collected into [`ParseResult::malformed`]; they do
///   **not** abort the parse.
///
/// Mirrors Python `_parse_jsonl_file` in `ingest.py`.
pub fn parse_jsonl_file(path: &Path) -> anyhow::Result<ParseResult> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;

    if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("gz"))
        .unwrap_or(false)
    {
        parse_reader(BufReader::new(GzDecoder::new(file)), path)
    } else {
        parse_reader(BufReader::new(file), path)
    }
}

fn parse_reader<R: Read>(reader: BufReader<R>, path: &Path) -> anyhow::Result<ParseResult> {
    let mut result = ParseResult::default();

    for (raw_index, line_result) in reader.lines().enumerate() {
        let raw_index = raw_index as u64;
        let line = line_result
            .with_context(|| format!("IO error reading line {raw_index} of {}", path.display()))?;

        // Mirror Python: `line = line.strip(); if not line: continue`
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match parse_envelope_line(trimmed, raw_index) {
            Ok(mut env) => {
                env.raw_index = raw_index;
                env.raw_line = line;
                result.envelopes.push(env);
            }
            Err(err) => {
                result.malformed.push(MalformedLine {
                    raw_index,
                    line,
                    error: err,
                });
            }
        }
    }

    Ok(result)
}

/// Attempt to parse a single trimmed line into an [`Envelope`].
///
/// Returns `Err(description)` on any parse failure; the caller is responsible
/// for recording it as a [`MalformedLine`].
fn parse_envelope_line(line: &str, raw_index: u64) -> Result<Envelope, String> {
    let env: Envelope = serde_json::from_str(line).map_err(|e| format!("JSON parse error: {e}"))?;

    // Mirror Python: validate that payload is a dict.
    if !env.p.is_object() {
        return Err(format!(
            "payload is not a JSON object (raw_index {raw_index})"
        ));
    }

    // Mirror Python: require hook_event_name and session_id.
    let missing_event_type = env
        .p
        .get("hook_event_name")
        .and_then(Value::as_str)
        .map(str::is_empty)
        .unwrap_or(true);
    let missing_session_id = env
        .p
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::is_empty)
        .unwrap_or(true);

    if missing_event_type || missing_session_id {
        return Err(format!(
            "missing hook_event_name or session_id (raw_index {raw_index})"
        ));
    }

    // raw_index and raw_line are populated by the caller after success.
    Ok(env)
}

// ---------------------------------------------------------------------------
// JSON canonicalization
// ---------------------------------------------------------------------------

/// Produces the same bytes as Python
/// `json.dumps(value, sort_keys=True, separators=(",", ":"))`.
///
/// Python's `json.dumps` uses `ensure_ascii=True` by default, which means
/// every non-ASCII Unicode codepoint is escaped as `\uXXXX` (BMP) or a UTF-16
/// surrogate pair `\uXXXX\uXXXX` (codepoints > U+FFFF).  `serde_json::to_string`
/// emits raw UTF-8 and does **not** escape non-ASCII.  To match Python
/// byte-for-byte we implement a custom recursive serializer.
///
/// Object keys are sorted lexicographically (matching Python's `sort_keys=True`
/// which uses Python's default `str` comparison -- Unicode codepoint order --
/// identical to Rust's `str` ordering).
///
/// Used for event-hash computation and canonical `raw_payload` storage.
pub fn canonicalize(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

/// Produces the same bytes as Python's
/// `json.dumps(value, separators=(",", ":"))` with the default `ensure_ascii=True`.
///
/// Differences from [`canonicalize`]:
/// - Preserves insertion order of object keys (requires `serde_json`'s
///   `preserve_order` feature, which swaps `serde_json::Map` from `BTreeMap`
///   to `IndexMap`).  This matches Python 3.7+ dict ordering: after
///   `json.loads` the keys follow wire order.
/// - Does **not** sort keys.
///
/// Non-ASCII escaping is identical to [`canonicalize`]: `\uXXXX` for BMP
/// codepoints U+0080..U+FFFF, surrogate pairs for codepoints > U+FFFF.
///
/// Use this for any output that must match Python ingest bytes — DB storage
/// strings, byte-count columns (`input_bytes`, `output_bytes`), and
/// `tool_input_json` / `tool_result_json` writes.
pub fn python_json_compact(value: &Value) -> String {
    let mut out = String::new();
    write_value_insertion_order(value, &mut out);
    out
}

fn write_value_insertion_order(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            out.push_str(&n.to_string());
        }
        Value::String(s) => {
            write_string(s, out);
        }
        Value::Array(arr) => {
            out.push('[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value_insertion_order(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            // Iterate in insertion order — this is the key difference from
            // `write_value` which sorts explicitly.  With the `preserve_order`
            // feature, `serde_json::Map` is backed by `IndexMap` and iterates
            // in insertion order.
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_value_insertion_order(v, out);
            }
            out.push('}');
        }
    }
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            // serde_json::Number formatting matches Python json.dumps for
            // integers and most floats.  Edge cases (very large floats,
            // -0.0) are noted in the module docs.
            out.push_str(&n.to_string());
        }
        Value::String(s) => {
            write_string(s, out);
        }
        Value::Array(arr) => {
            out.push('[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            // Sort keys to match Python's sort_keys=True.
            let mut pairs: Vec<(&String, &Value)> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_value(v, out);
            }
            out.push('}');
        }
    }
}

/// Write a JSON string with Python's `ensure_ascii=True` escaping.
///
/// Python escapes non-ASCII codepoints as `\uXXXX` (BMP, U+0080..U+FFFF) or
/// as a UTF-16 surrogate pair `\uXXXX\uXXXX` (supplementary, > U+FFFF).
/// Control characters below U+0020 use the standard JSON escapes
/// (`\n`, `\t`, `\r`, `\b`, `\f`) or `\uXXXX` for the rest.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Other ASCII control characters: \u00XX
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) <= 0x7e => {
                // Printable ASCII -- emit as-is.
                out.push(c);
            }
            c => {
                // Non-ASCII: escape with \uXXXX or surrogate pair.
                let cp = c as u32;
                if cp <= 0xffff {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    // Encode as UTF-16 surrogate pair, mirroring Python's
                    // ensure_ascii=True behavior for codepoints > U+FFFF.
                    let cp_adj = cp - 0x10000;
                    let high = 0xd800u32 + (cp_adj >> 10);
                    let low = 0xdc00u32 + (cp_adj & 0x3ff);
                    out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                }
            }
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a minimal valid v1 envelope line.
    fn make_line(session_id: &str, event_type: &str, ts: &str) -> String {
        format!(
            r#"{{"v":1,"ts":"{ts}","p":{{"hook_event_name":"{event_type}","session_id":"{session_id}"}}}}"#
        )
    }

    /// Write lines to a temp file and return it.
    fn write_temp_jsonl(lines: &[&str]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        for line in lines {
            writeln!(f, "{line}").expect("write line");
        }
        f
    }

    // -----------------------------------------------------------------------
    // parse_happy_path
    // -----------------------------------------------------------------------

    #[test]
    fn parse_happy_path() {
        let l0 = make_line("s1", "SessionStart", "2024-01-01T00:00:00.000Z");
        let l1 = make_line("s1", "PreToolUse", "2024-01-01T00:00:01.000Z");
        let l2 = make_line("s1", "PostToolUse", "2024-01-01T00:00:02.000Z");
        let f = write_temp_jsonl(&[&l0, &l1, &l2]);

        let result = parse_jsonl_file(f.path()).expect("parse");
        assert_eq!(result.malformed.len(), 0, "no malformed lines expected");
        assert_eq!(result.envelopes.len(), 3, "3 envelopes expected");

        assert_eq!(result.envelopes[0].raw_index, 0);
        assert_eq!(result.envelopes[1].raw_index, 1);
        assert_eq!(result.envelopes[2].raw_index, 2);
    }

    // -----------------------------------------------------------------------
    // parse_with_malformed
    // -----------------------------------------------------------------------

    #[test]
    fn parse_with_malformed() {
        let good0 = make_line("s1", "SessionStart", "2024-01-01T00:00:00.000Z");
        let bad1 = "{bad json";
        let good2 = make_line("s1", "PostToolUse", "2024-01-01T00:00:02.000Z");
        let f = write_temp_jsonl(&[&good0, bad1, &good2]);

        let result = parse_jsonl_file(f.path()).expect("parse");
        assert_eq!(result.envelopes.len(), 2, "2 good envelopes");
        assert_eq!(result.malformed.len(), 1, "1 malformed line");

        // Good lines keep their original raw_index (0 and 2).
        assert_eq!(result.envelopes[0].raw_index, 0);
        assert_eq!(result.envelopes[1].raw_index, 2);

        // Malformed line is at index 1.
        assert_eq!(result.malformed[0].raw_index, 1);
        assert!(
            result.malformed[0].error.contains("JSON parse error"),
            "expected JSON parse error, got: {}",
            result.malformed[0].error
        );
    }

    // -----------------------------------------------------------------------
    // parse_gzip
    // -----------------------------------------------------------------------

    #[test]
    fn parse_gzip() {
        let l0 = make_line("sg", "SessionStart", "2024-02-01T00:00:00.000Z");
        let l1 = make_line("sg", "PreToolUse", "2024-02-01T00:00:01.000Z");

        // Write a gzipped temp file (must end in .gz for the parser to detect it).
        let gz_file = {
            let mut tf = tempfile::Builder::new()
                .suffix(".jsonl.gz")
                .tempfile()
                .expect("gz tempfile");
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            writeln!(enc, "{l0}").expect("write l0");
            writeln!(enc, "{l1}").expect("write l1");
            let compressed = enc.finish().expect("finish gz");
            tf.write_all(&compressed).expect("write gz bytes");
            tf
        };

        let result = parse_jsonl_file(gz_file.path()).expect("parse gz");
        assert_eq!(result.envelopes.len(), 2, "2 envelopes from gzip");
        assert_eq!(result.malformed.len(), 0, "no malformed lines");
        assert_eq!(result.envelopes[0].raw_index, 0);
        assert_eq!(result.envelopes[1].raw_index, 1);
    }

    // -----------------------------------------------------------------------
    // parse_empty
    // -----------------------------------------------------------------------

    #[test]
    fn parse_empty() {
        let f = write_temp_jsonl(&[]);
        let result = parse_jsonl_file(f.path()).expect("parse empty");
        assert_eq!(result.envelopes.len(), 0);
        assert_eq!(result.malformed.len(), 0);
    }

    // -----------------------------------------------------------------------
    // parse_blank_lines -- blank lines increment raw_index but are skipped
    // -----------------------------------------------------------------------

    #[test]
    fn parse_blank_lines_increment_raw_index() {
        let l0 = make_line("sb", "SessionStart", "2024-03-01T00:00:00.000Z");
        let l2 = make_line("sb", "PostToolUse", "2024-03-01T00:00:01.000Z");
        // line 0: good, line 1: blank, line 2: good
        let f = write_temp_jsonl(&[&l0, "", &l2]);

        let result = parse_jsonl_file(f.path()).expect("parse");
        assert_eq!(result.envelopes.len(), 2);
        // Blank line at index 1 is skipped; second envelope is at raw_index 2.
        assert_eq!(result.envelopes[0].raw_index, 0);
        assert_eq!(result.envelopes[1].raw_index, 2);
    }

    // -----------------------------------------------------------------------
    // raw_line is preserved verbatim
    // -----------------------------------------------------------------------

    #[test]
    fn parse_raw_line_preserved() {
        let l0 = make_line("sr", "SessionStart", "2024-04-01T00:00:00.000Z");
        let f = write_temp_jsonl(&[&l0]);
        let result = parse_jsonl_file(f.path()).expect("parse");
        // raw_line should equal the line as written (without the trailing newline
        // stripped by BufRead::lines).
        assert_eq!(result.envelopes[0].raw_line, l0);
    }

    // -----------------------------------------------------------------------
    // canonicalize_nested
    // -----------------------------------------------------------------------

    #[test]
    fn canonicalize_nested() {
        // Keys are deliberately out of lexicographic order to verify sorting.
        let v = json!({
            "z": 1,
            "a": [3, 2, 1],
            "m": {"y": true, "b": null}
        });
        let got = canonicalize(&v);
        // Expected: keys sorted at every level, minimal separators.
        // Top-level order: a, m, z
        // Inner object order: b, y
        let expected = r#"{"a":[3,2,1],"m":{"b":null,"y":true},"z":1}"#;
        assert_eq!(got, expected);
    }

    // -----------------------------------------------------------------------
    // canonicalize_unicode -- verify ensure_ascii=True escaping
    // -----------------------------------------------------------------------

    #[test]
    fn canonicalize_unicode() {
        // "cafe\u{00e9}" = "cafe" + e-acute (U+00E9).
        // Python ensure_ascii=True (default) escapes it as é.
        let v = json!({"key": "caf\u{00e9}"});
        let got = canonicalize(&v);
        // Python: json.dumps({"key": "cafeé"}, sort_keys=True, separators=(",",":"))
        // => {"key":"café"}
        assert_eq!(got, "{\"key\":\"caf\\u00e9\"}");
    }

    // -----------------------------------------------------------------------
    // canonicalize_surrogate_pair -- codepoint > U+FFFF
    // -----------------------------------------------------------------------

    #[test]
    fn canonicalize_surrogate_pair() {
        // U+1F600 (GRINNING FACE) requires a UTF-16 surrogate pair:
        // high = 0xD83D, low = 0xDE00
        let v = json!({"emoji": "\u{1F600}"});
        let got = canonicalize(&v);
        // Python: json.dumps({"emoji": "\U0001f600"}, sort_keys=True, separators=(",",":"))
        // => {"emoji":"😀"}
        assert_eq!(got, "{\"emoji\":\"\\ud83d\\ude00\"}");
    }

    // -----------------------------------------------------------------------
    // canonicalize_numbers
    // -----------------------------------------------------------------------

    #[test]
    fn canonicalize_numbers() {
        // Integer -- no decimal point (matches Python).
        assert_eq!(canonicalize(&json!(42)), "42");
        // Negative integer.
        assert_eq!(canonicalize(&json!(-1)), "-1");
        // Float with fractional part.
        assert_eq!(canonicalize(&json!(3.25)), "3.25");
        // Zero.
        assert_eq!(canonicalize(&json!(0)), "0");
    }

    // -----------------------------------------------------------------------
    // canonicalize_primitives
    // -----------------------------------------------------------------------

    #[test]
    fn canonicalize_primitives() {
        assert_eq!(canonicalize(&json!(null)), "null");
        assert_eq!(canonicalize(&json!(true)), "true");
        assert_eq!(canonicalize(&json!(false)), "false");
        assert_eq!(canonicalize(&json!("hello")), "\"hello\"");
    }

    // -----------------------------------------------------------------------
    // canonicalize_special_string_escapes
    // -----------------------------------------------------------------------

    #[test]
    fn canonicalize_special_string_escapes() {
        // Newline, tab, carriage return, backslash, double-quote.
        // Input string literal contains: LF TAB CR backslash double-quote
        let v = json!("\n\t\r\\\"");
        let got = canonicalize(&v);
        // Expected JSON encoding of those five characters: "\n\t\r\\\""
        assert_eq!(got, "\"\\n\\t\\r\\\\\\\"\"");

        // ASCII control char U+0001 encodes as  in JSON.
        let v2 = json!("\x01");
        assert_eq!(canonicalize(&v2), "\"\\u0001\"");
    }

    // -----------------------------------------------------------------------
    // python_json_compact_preserves_insertion_order
    // -----------------------------------------------------------------------

    #[test]
    fn python_json_compact_preserves_insertion_order() {
        // Construct an object with keys in "z", "a", "m" order.
        // With preserve_order enabled, serde_json::Map retains insertion order.
        let mut map = serde_json::Map::new();
        map.insert("z".to_owned(), json!(1));
        map.insert("a".to_owned(), json!(2));
        map.insert("m".to_owned(), json!(3));
        let v = Value::Object(map);

        let got = python_json_compact(&v);
        // Insertion order must be preserved: z, a, m.
        assert_eq!(got, r#"{"z":1,"a":2,"m":3}"#);

        // Verify that canonicalize still sorts: a, m, z.
        let canonical = canonicalize(&v);
        assert_eq!(canonical, r#"{"a":2,"m":3,"z":1}"#);
    }

    // -----------------------------------------------------------------------
    // python_json_compact_escapes_non_ascii
    // -----------------------------------------------------------------------

    #[test]
    fn python_json_compact_escapes_non_ascii() {
        // "é" is U+00E9 — should escape as é.
        let v = json!("\u{00e9}");
        let got = python_json_compact(&v);
        assert_eq!(got, "\"\\u00e9\"");
    }

    // -----------------------------------------------------------------------
    // python_json_compact_matches_reference
    // -----------------------------------------------------------------------

    #[test]
    fn python_json_compact_matches_reference() {
        // Reference computed via:
        //   python3 -c "import json; print(json.dumps({'x': 'café', 'nums': [1, 2]}, separators=(',', ':')))"
        // Python ensure_ascii=True (default) escapes é (U+00E9) as é, so
        // the output is: {"x":"café","nums":[1,2]}
        // (20 characters, all ASCII after escaping)
        let mut map = serde_json::Map::new();
        map.insert("x".to_owned(), json!("caf\u{00e9}"));
        map.insert("nums".to_owned(), json!([1, 2]));
        let v = Value::Object(map);

        let got = python_json_compact(&v);
        // Keys in insertion order (x before nums), non-ASCII escaped.
        assert_eq!(got, "{\"x\":\"caf\\u00e9\",\"nums\":[1,2]}");
    }

    // -----------------------------------------------------------------------
    // parse_missing_required_fields_collected_as_malformed
    // -----------------------------------------------------------------------

    #[test]
    fn parse_missing_required_fields_collected_as_malformed() {
        // Valid JSON but missing session_id in payload.
        let bad = r#"{"v":1,"ts":"2024-01-01T00:00:00Z","p":{"hook_event_name":"SessionStart"}}"#;
        let f = write_temp_jsonl(&[bad]);
        let result = parse_jsonl_file(f.path()).expect("parse");
        assert_eq!(result.envelopes.len(), 0);
        assert_eq!(result.malformed.len(), 1);
        assert!(
            result.malformed[0].error.contains("missing"),
            "expected 'missing' in error, got: {}",
            result.malformed[0].error
        );
    }
}
