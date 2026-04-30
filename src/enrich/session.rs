//! Per-session enrichment: 4-pass computation over a session's envelopes.
//!
//! Mirrors Python `enrich_session_events` and `_compute_event_hash` from
//! `~/.claude/telemetry/ingest.py` (lines 291-421).
//!
//! ## Python originals (verbatim)
//!
//! ### Sort key (line 316)
//! ```python
//! sess_events.sort(key=lambda e: (e.get("timestamp", ""), e.get("_raw_index", 0)))
//! ```
//!
//! ### Pass 1 — sequence_num + field isolation (lines 318-354)
//! ```python
//! for seq, ev in enumerate(sess_events):
//!     ev["sequence_num"] = seq
//!     et = ev.get("event_type", "")
//!     p = ev.get("_payload", {})
//!     ev["source"] = p.get("source") if et == "SessionStart" else None
//!     ev["reason"] = p.get("reason") if et == "SessionEnd" else None
//!     ev["compact_trigger"] = p.get("trigger") if et == "PreCompact" else None
//!     ev["config_source"] = p.get("source") if et == "ConfigChange" else None
//!     if et == "UserPromptSubmit":
//!         pt = ev.get("prompt_text") or ""
//!         ev["is_slash_command"] = 1 if pt.startswith("/") else 0
//!     if et == "TaskCompleted":
//!         ev["task_id"] = p.get("task_id")
//!         ev["task_subject"] = p.get("task_subject")
//!         ev["teammate_name"] = p.get("teammate_name")
//!     if ev.get("skill_name") is None:  # IMPLEMENTED IN T08 — see enrich_session Pass 1
//!         sname, stype = _detect_skill(ev.get("tool_input"))
//!         if sname:
//!             ev["skill_name"] = sname
//!             ev["skill_type"] = stype
//!     if et == "PreToolUse":
//!         tuid = ev.get("tool_use_id")
//!         if tuid:
//!             pre_tool_times[tuid] = ev.get("timestamp", "")
//! ```
//!
//! ### Pass 2 — duration_ms (lines 357-373)
//! ```python
//! for ev in sess_events:
//!     et = ev.get("event_type", "")
//!     if et in ("PostToolUse", "PostToolUseFailure"):
//!         tuid = ev.get("tool_use_id")
//!         if tuid and tuid in pre_tool_times:
//!             try:
//!                 t_pre = datetime.fromisoformat(pre_tool_times[tuid].replace("Z", "+00:00"))
//!                 t_post = datetime.fromisoformat(ev["timestamp"].replace("Z", "+00:00"))
//!                 ev["duration_ms"] = int((t_post - t_pre).total_seconds() * 1000)
//!             except Exception:
//!                 pass
//! ```
//!
//! ### Pass 3 — context budget / context_cumulative_bytes (lines 376-402)
//! ```python
//! cumulative = 0
//! for ev in sess_events:
//!     raw_input = ev.get("tool_input")
//!     raw_output = ev.get("tool_result") or ev.get("tool_response")
//!     if raw_input is not None:
//!         s = raw_input if isinstance(raw_input, str) else json.dumps(raw_input, separators=(",", ":"))
//!         ev["input_bytes"] = len(s.encode("utf-8"))
//!     else:
//!         ev["input_bytes"] = 0
//!     if raw_output is not None:
//!         s = raw_output if isinstance(raw_output, str) else json.dumps(raw_output, separators=(",", ":"))
//!         ev["output_bytes"] = len(s.encode("utf-8"))
//!     else:
//!         ev["output_bytes"] = 0
//!     cumulative += ev["input_bytes"] + ev["output_bytes"] + (ev.get("prompt_length") or 0)
//!     ev["context_cumulative_bytes"] = cumulative
//! ```
//!
//! ### Pass 4 — event_hash (lines 405-421)
//! ```python
//! def _compute_event_hash(event: dict) -> str:
//!     key = "|".join([
//!         event.get("session_id", ""),
//!         event.get("event_type", ""),
//!         event.get("timestamp", ""),
//!         event.get("tool_use_id") or "",
//!     ])
//!     return hashlib.sha256(key.encode("utf-8")).hexdigest()[:16]
//! ```
//!
//! ## `preserve_order` feature
//!
//! `serde_json` is compiled with the `preserve_order` feature, which swaps
//! `serde_json::Map` from `BTreeMap` to `IndexMap`.  This is required by
//! [`crate::envelope::python_json_compact`] so that JSON objects parsed from
//! the wire preserve their key order, matching Python 3.7+ dict ordering.
//!
//! Note: Python's `_compute_event_hash` does NOT serialize any JSON object —
//! it hashes a plain pipe-delimited string of four scalar fields — so Pass 4
//! is unaffected by key ordering.  The feature is needed for Pass 3's byte
//! counting via `python_json_compact`.

use std::collections::HashMap;

use chrono::DateTime;
use sha2::{Digest, Sha256};

use crate::envelope::Envelope;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Field-isolation results computed during Pass 1.
///
/// These mirror fields that Python sets conditionally based on `event_type`.
#[derive(Debug, Clone, Default)]
pub struct FieldIsolation {
    /// Set only for `SessionStart` events: `p.get("source")`.
    pub source: Option<String>,
    /// Set only for `SessionEnd` events: `p.get("reason")`.
    pub reason: Option<String>,
    /// Set only for `PreCompact` events: `p.get("trigger")`.
    pub compact_trigger: Option<String>,
    /// Set only for `ConfigChange` events: `p.get("source")`.
    pub config_source: Option<String>,
    /// `1` for `UserPromptSubmit` events whose `prompt_text` starts with `/`.
    pub is_slash_command: i32,
    /// `TaskCompleted` fields.
    pub task_id: Option<String>,
    pub task_subject: Option<String>,
    pub teammate_name: Option<String>,
}

/// An enriched event produced by running the 4 enrichment passes over a
/// session's envelopes.
///
/// Field names mirror the Python event-dict keys inserted by
/// `enrich_session_events` in `ingest.py`.
#[derive(Debug, Clone)]
pub struct EnrichedEvent {
    /// The original parsed envelope.
    pub envelope: Envelope,

    // Pass 1
    /// 0-based position within the session (sorted by timestamp + raw_index).
    pub sequence_num: i64,
    /// Field-isolation results (conditional per event_type).
    pub isolation: FieldIsolation,

    // Pass 2
    /// Milliseconds between the matching `PreToolUse` and this
    /// `PostToolUse`/`PostToolUseFailure` event.  `None` for all other
    /// event types, and for post-tool events where no matching pre-tool
    /// timestamp was found, or when ISO-8601 parsing fails.
    ///
    /// Mirrors Python `ev["duration_ms"]` set in Pass 2.
    pub duration_ms: Option<i64>,

    // Pass 3
    /// Byte length of `tool_input` for this event (0 when absent).
    pub input_bytes: i64,
    /// Byte length of `tool_result`/`tool_response` for this event (0 when absent).
    pub output_bytes: i64,
    /// Running cumulative total of `input_bytes + output_bytes + prompt_length`
    /// up to and including this event.
    ///
    /// Mirrors Python `ev["context_cumulative_bytes"]`.
    pub context_cumulative_bytes: i64,

    // Pass 4
    /// 16-character lowercase hex SHA-256 digest of
    /// `"session_id|event_type|timestamp|tool_use_id"`.
    ///
    /// Mirrors Python `_compute_event_hash`.
    pub event_hash: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Enrich a single session's envelopes with 4 computed passes.
///
/// The envelopes do not need to be pre-sorted — this function sorts them by
/// `(ts, raw_index)` first (matching Python line 316).  They should all share
/// the same `session_id`; if they do not, results are still deterministic but
/// the cumulative context bytes will span the mixed sessions.
///
/// Returns an empty `Vec` when given an empty input.
pub fn enrich_session(envelopes: Vec<Envelope>) -> Vec<EnrichedEvent> {
    if envelopes.is_empty() {
        return Vec::new();
    }

    // Sort by (timestamp string, raw_index) — mirrors Python line 316.
    // Python compares ISO-8601 strings lexicographically, which is equivalent
    // to chronological order for well-formed timestamps with the same timezone.
    let mut sorted = envelopes;
    sorted.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.raw_index.cmp(&b.raw_index)));

    // Allocate working vector with enough capacity.
    let mut events: Vec<EnrichedEvent> = Vec::with_capacity(sorted.len());

    // Pre-populate with defaults — passes fill in their fields below.
    for (seq, envelope) in sorted.iter().enumerate() {
        events.push(EnrichedEvent {
            envelope: envelope.clone(),
            sequence_num: seq as i64,
            isolation: FieldIsolation::default(),
            duration_ms: None,
            input_bytes: 0,
            output_bytes: 0,
            context_cumulative_bytes: 0,
            event_hash: String::new(),
        });
    }

    // --- Pass 1: sequence_num + field isolation + flags ---
    //
    // Also collect PreToolUse timestamps for Pass 2.
    let mut pre_tool_times: HashMap<String, String> = HashMap::new();

    for (i, envelope) in sorted.iter().enumerate() {
        let et = p_str(envelope, "hook_event_name");
        let payload = &envelope.p;

        // Field isolation — scoped so the `iso` mutable borrow is released
        // before the skill-detection block accesses `events[i].envelope.p`.
        {
            let iso = &mut events[i].isolation;

            iso.source = if et == Some("SessionStart") {
                p_opt_str(payload, "source")
            } else {
                None
            };

            iso.reason = if et == Some("SessionEnd") {
                p_opt_str(payload, "reason")
            } else {
                None
            };

            iso.compact_trigger = if et == Some("PreCompact") {
                p_opt_str(payload, "trigger")
            } else {
                None
            };

            iso.config_source = if et == Some("ConfigChange") {
                p_opt_str(payload, "source")
            } else {
                None
            };

            // is_slash_command
            if et == Some("UserPromptSubmit") {
                let pt = p_str(envelope, "prompt").or_else(|| p_str(envelope, "prompt_text"));
                iso.is_slash_command = if pt.map(|s| s.starts_with('/')).unwrap_or(false) {
                    1
                } else {
                    0
                };
            }

            // TaskCompleted fields
            if et == Some("TaskCompleted") {
                iso.task_id = p_opt_str(payload, "task_id");
                iso.task_subject = p_opt_str(payload, "task_subject");
                iso.teammate_name = p_opt_str(payload, "teammate_name");
            }
        }

        // T08: skill detection — wire `enrich::skill::detect_skill` into the
        // enrichment pipeline.  Mirrors Python (~/.claude/telemetry/ingest.py
        // lines 343-348):
        //
        //   if ev.get("skill_name") is None:
        //       sname, stype = _detect_skill(ev.get("tool_input"))
        //       if sname:
        //           ev["skill_name"] = sname
        //           ev["skill_type"] = stype
        //
        // Skip detection if a skill_name is already present in the payload
        // (e.g., a hook injected it upstream).  Convert non-string tool_input
        // to compact JSON to match Python's json.dumps(..., separators=(",", ":")).
        {
            let already_has = events[i]
                .envelope
                .p
                .as_object()
                .and_then(|m| m.get("skill_name"))
                .map(|v| !v.is_null())
                .unwrap_or(false);

            if !already_has {
                let tool_input_str: String =
                    match events[i].envelope.p.as_object().and_then(|m| m.get("tool_input")) {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(other) => crate::envelope::python_json_compact(other),
                        None => String::new(),
                    };

                if let Some(detected) = crate::enrich::skill::detect_skill(&tool_input_str) {
                    if let serde_json::Value::Object(ref mut map) = events[i].envelope.p {
                        map.insert(
                            "skill_name".to_string(),
                            serde_json::Value::String(detected.name),
                        );
                        if let Some(st) = detected.skill_type {
                            map.insert(
                                "skill_type".to_string(),
                                serde_json::Value::String(st),
                            );
                        }
                    }
                }
            }
        }

        // Track PreToolUse timestamps for Pass 2
        if et == Some("PreToolUse")
            && let Some(tuid) = p_opt_str(payload, "tool_use_id")
        {
            pre_tool_times.insert(tuid, envelope.ts.clone());
        }
    }

    // --- Pass 2: duration_ms ---
    //
    // For PostToolUse and PostToolUseFailure events, look up the timestamp of
    // the matching PreToolUse by tool_use_id and compute the elapsed time.
    for (i, envelope) in sorted.iter().enumerate() {
        let et = p_str(envelope, "hook_event_name");
        if (et == Some("PostToolUse") || et == Some("PostToolUseFailure"))
            && let Some(tuid) = p_opt_str(&envelope.p, "tool_use_id")
            && let Some(pre_ts) = pre_tool_times.get(&tuid)
            && let Some(ms) = diff_ms(pre_ts, &envelope.ts)
        {
            events[i].duration_ms = Some(ms);
        }
    }

    // --- Pass 3: context budget (running cumulative sum) ---
    let mut cumulative: i64 = 0;
    for (i, envelope) in sorted.iter().enumerate() {
        let payload = &envelope.p;

        // tool_input byte count
        let input_bytes = payload
            .get("tool_input")
            .map(byte_len_of_value)
            .unwrap_or(0);

        // tool_result / tool_response (PostToolUse uses tool_response in payload)
        let output_bytes = payload
            .get("tool_result")
            .or_else(|| payload.get("tool_response"))
            .map(byte_len_of_value)
            .unwrap_or(0);

        // prompt_length
        let prompt_bytes = payload
            .get("prompt_length")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        cumulative += input_bytes + output_bytes + prompt_bytes;

        events[i].input_bytes = input_bytes;
        events[i].output_bytes = output_bytes;
        events[i].context_cumulative_bytes = cumulative;
    }

    // --- Pass 4: event_hash ---
    for (i, envelope) in sorted.iter().enumerate() {
        // Only compute if not pre-computed by capture.sh (mirrors Python `if ev.get("event_hash") is None`).
        if envelope.h.is_none() {
            events[i].event_hash = compute_event_hash(envelope);
        } else {
            events[i].event_hash = envelope.h.clone().unwrap_or_default();
        }
    }

    events
}

// ---------------------------------------------------------------------------
// Hash computation — mirrors Python `_compute_event_hash`
// ---------------------------------------------------------------------------

/// Compute the event hash exactly as Python does:
///
/// ```python
/// def _compute_event_hash(event: dict) -> str:
///     key = "|".join([
///         event.get("session_id", ""),
///         event.get("event_type", ""),
///         event.get("timestamp", ""),
///         event.get("tool_use_id") or "",
///     ])
///     return hashlib.sha256(key.encode("utf-8")).hexdigest()[:16]
/// ```
///
/// Note: `event_type` maps to the payload field `hook_event_name`.
/// `timestamp` is the envelope's `ts` field.
pub fn compute_event_hash(envelope: &Envelope) -> String {
    let session_id = p_str(envelope, "session_id").unwrap_or_default();
    let event_type = p_str(envelope, "hook_event_name").unwrap_or_default();
    let timestamp = envelope.ts.as_str();
    let tool_use_id = p_opt_str(&envelope.p, "tool_use_id").unwrap_or_default();

    let key = format!("{session_id}|{event_type}|{timestamp}|{tool_use_id}");

    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();

    // First 16 hex characters (8 bytes → 16 hex chars).
    // Mirrors Python `hexdigest()[:16]`.
    let full_hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    full_hex[..16].to_owned()
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Extract a `&str` from a payload field, returning `None` if absent or not a string.
fn p_opt_str(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Extract an `Option<&str>` from an envelope's payload field.
fn p_str<'a>(envelope: &'a Envelope, key: &str) -> Option<&'a str> {
    envelope.p.get(key).and_then(serde_json::Value::as_str)
}

/// Compute the UTF-8 byte length of a JSON value, mirroring Python:
///
/// ```python
/// s = raw if isinstance(raw, str) else json.dumps(raw, separators=(",", ":"))
/// len(s.encode("utf-8"))
/// ```
///
/// If the value is a JSON string, we measure the raw string content bytes
/// (matching Python's `isinstance(raw, str)` branch — the string value is used
/// as-is, not re-serialized).
///
/// For any other JSON type, we use [`crate::envelope::python_json_compact`]
/// which produces bytes identical to Python's
/// `json.dumps(raw, separators=(",", ":"))`:
/// - Preserves insertion order (matching Python 3.7+ dict order from `json.loads`).
/// - Escapes non-ASCII as `\uXXXX` / surrogate pairs (`ensure_ascii=True`).
///
/// Because the output is all-ASCII after escaping, `len()` on the Rust `String`
/// gives the same count as `len(s.encode("utf-8"))` in Python.
fn byte_len_of_value(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::String(s) => s.len() as i64,
        other => {
            // Use python_json_compact to match Python's json.dumps byte output:
            // insertion-order keys + ensure_ascii escaping.
            crate::envelope::python_json_compact(other).len() as i64
        }
    }
}

/// Compute `(t_post - t_pre).total_seconds() * 1000` truncated to `i64`.
///
/// Mirrors Python:
/// ```python
/// t_pre = datetime.fromisoformat(pre_tool_times[tuid].replace("Z", "+00:00"))
/// t_post = datetime.fromisoformat(ev["timestamp"].replace("Z", "+00:00"))
/// ev["duration_ms"] = int((t_post - t_pre).total_seconds() * 1000)
/// ```
///
/// Returns `None` if either timestamp fails to parse.
fn diff_ms(pre_ts: &str, post_ts: &str) -> Option<i64> {
    let pre = parse_iso8601(pre_ts)?;
    let post = parse_iso8601(post_ts)?;
    let delta = post.signed_duration_since(pre);
    Some(delta.num_milliseconds())
}

/// Parse an ISO-8601 timestamp string, accepting both `Z` and `+00:00` suffixes.
fn parse_iso8601(ts: &str) -> Option<DateTime<chrono::FixedOffset>> {
    // Replace trailing Z with +00:00 to match Python's `.replace("Z", "+00:00")`.
    let normalized = if let Some(stripped) = ts.strip_suffix('Z') {
        format!("{}+00:00", stripped)
    } else {
        ts.to_owned()
    };
    DateTime::parse_from_rfc3339(&normalized).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // -----------------------------------------------------------------------
    // Helper builders
    // -----------------------------------------------------------------------

    /// Build a minimal valid Envelope with the given session_id, event_type,
    /// timestamp, and raw_index.
    fn make_envelope(session_id: &str, event_type: &str, ts: &str, raw_index: u64) -> Envelope {
        Envelope {
            v: 1,
            ts: ts.to_owned(),
            p: json!({
                "hook_event_name": event_type,
                "session_id": session_id,
            }),
            h: None,
            raw_index,
            raw_line: String::new(),
        }
    }

    /// Build an Envelope with an optional `tool_use_id` payload field.
    fn make_tool_envelope(
        session_id: &str,
        event_type: &str,
        ts: &str,
        raw_index: u64,
        tool_use_id: Option<&str>,
    ) -> Envelope {
        let mut payload = serde_json::Map::new();
        payload.insert("hook_event_name".to_owned(), json!(event_type));
        payload.insert("session_id".to_owned(), json!(session_id));
        if let Some(tuid) = tool_use_id {
            payload.insert("tool_use_id".to_owned(), json!(tuid));
        }
        Envelope {
            v: 1,
            ts: ts.to_owned(),
            p: serde_json::Value::Object(payload),
            h: None,
            raw_index,
            raw_line: String::new(),
        }
    }

    // -----------------------------------------------------------------------
    // empty_session
    // -----------------------------------------------------------------------

    #[test]
    fn empty_session() {
        let result = enrich_session(vec![]);
        assert!(result.is_empty(), "empty input → empty output");
    }

    // -----------------------------------------------------------------------
    // single_event_session
    // -----------------------------------------------------------------------

    #[test]
    fn single_event_session() {
        let ev = make_envelope("s1", "SessionStart", "2024-01-01T00:00:00.000Z", 0);
        let result = enrich_session(vec![ev]);
        assert_eq!(result.len(), 1);
        // Single event gets sequence_num 0.
        assert_eq!(result[0].sequence_num, 0);
        // Duration: None (no PreToolUse pairing).
        assert!(result[0].duration_ms.is_none());
    }

    // -----------------------------------------------------------------------
    // sort_by_timestamp_then_raw_index
    // -----------------------------------------------------------------------

    /// Two events with identical timestamps — raw_index breaks the tie.
    #[test]
    fn sort_by_timestamp_then_raw_index() {
        let ts = "2024-01-01T00:00:00.000Z";
        // Deliberately pass them in reversed raw_index order.
        let ev_b = make_envelope("s1", "PreToolUse", ts, 10);
        let ev_a = make_envelope("s1", "SessionStart", ts, 3);

        let result = enrich_session(vec![ev_b, ev_a]);
        assert_eq!(result.len(), 2);

        // raw_index 3 sorts before raw_index 10.
        assert_eq!(
            result[0].envelope.raw_index, 3,
            "lower raw_index must come first"
        );
        assert_eq!(result[1].envelope.raw_index, 10);

        assert_eq!(result[0].sequence_num, 0);
        assert_eq!(result[1].sequence_num, 1);
    }

    // -----------------------------------------------------------------------
    // sequence_num_monotonic
    // -----------------------------------------------------------------------

    #[test]
    fn sequence_num_monotonic() {
        let evs: Vec<Envelope> = (0..5u64)
            .map(|i| {
                make_envelope(
                    "s1",
                    "PreToolUse",
                    &format!("2024-01-01T00:00:0{i}.000Z"),
                    i,
                )
            })
            .collect();

        let result = enrich_session(evs);
        assert_eq!(result.len(), 5);
        for (expected_seq, enriched) in result.iter().enumerate() {
            assert_eq!(
                enriched.sequence_num, expected_seq as i64,
                "sequence_num mismatch at position {expected_seq}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // duration_ms_calculated
    // -----------------------------------------------------------------------

    /// 3 events: PreToolUse, PostToolUse (paired), SessionEnd (no duration).
    /// Duration should be computed for PostToolUse only.
    #[test]
    fn duration_ms_calculated() {
        let tuid = "tool-abc";
        let pre = make_tool_envelope(
            "s1",
            "PreToolUse",
            "2024-01-01T00:00:00.000Z",
            0,
            Some(tuid),
        );
        let post = make_tool_envelope(
            "s1",
            "PostToolUse",
            "2024-01-01T00:00:00.100Z",
            1,
            Some(tuid),
        );
        let end = make_envelope("s1", "SessionEnd", "2024-01-01T00:00:00.250Z", 2);

        let result = enrich_session(vec![pre, post, end]);
        assert_eq!(result.len(), 3);

        // PreToolUse — no duration
        assert!(
            result[0].duration_ms.is_none(),
            "PreToolUse should have no duration"
        );

        // PostToolUse — 100ms
        assert_eq!(
            result[1].duration_ms,
            Some(100),
            "PostToolUse duration should be 100ms"
        );

        // SessionEnd — no duration
        assert!(
            result[2].duration_ms.is_none(),
            "SessionEnd should have no duration"
        );
    }

    // -----------------------------------------------------------------------
    // duration_ms_two_pairs
    // -----------------------------------------------------------------------

    /// Verify two independent PreToolUse/PostToolUse pairs compute correct durations.
    #[test]
    fn duration_ms_two_pairs() {
        let tuid_a = "tool-a";
        let tuid_b = "tool-b";

        let pre_a = make_tool_envelope(
            "s1",
            "PreToolUse",
            "2024-01-01T00:00:00.000Z",
            0,
            Some(tuid_a),
        );
        let post_a = make_tool_envelope(
            "s1",
            "PostToolUse",
            "2024-01-01T00:00:00.250Z",
            1,
            Some(tuid_a),
        );
        let pre_b = make_tool_envelope(
            "s1",
            "PreToolUse",
            "2024-01-01T00:00:01.000Z",
            2,
            Some(tuid_b),
        );
        let post_b = make_tool_envelope(
            "s1",
            "PostToolUse",
            "2024-01-01T00:00:02.500Z",
            3,
            Some(tuid_b),
        );

        let result = enrich_session(vec![pre_a, post_a, pre_b, post_b]);
        assert_eq!(result[1].duration_ms, Some(250), "pair A: 250ms");
        assert_eq!(result[3].duration_ms, Some(1500), "pair B: 1500ms");
    }

    // -----------------------------------------------------------------------
    // context_bytes_accumulated
    // -----------------------------------------------------------------------

    /// Verify the running cumulative formula:
    /// `context_cumulative_bytes += input_bytes + output_bytes + prompt_length`
    #[test]
    fn context_bytes_accumulated() {
        // Build 3 envelopes with known input/output sizes.
        let mut payload0 = serde_json::Map::new();
        payload0.insert("hook_event_name".to_owned(), json!("PreToolUse"));
        payload0.insert("session_id".to_owned(), json!("s1"));
        payload0.insert("tool_input".to_owned(), json!("hello")); // 5 bytes

        let mut payload1 = serde_json::Map::new();
        payload1.insert("hook_event_name".to_owned(), json!("PostToolUse"));
        payload1.insert("session_id".to_owned(), json!("s1"));
        payload1.insert("tool_response".to_owned(), json!("world!")); // 6 bytes

        let mut payload2 = serde_json::Map::new();
        payload2.insert("hook_event_name".to_owned(), json!("UserPromptSubmit"));
        payload2.insert("session_id".to_owned(), json!("s1"));
        payload2.insert("prompt_length".to_owned(), json!(10_i64));

        let ev0 = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: serde_json::Value::Object(payload0),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };
        let ev1 = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:01.000Z".to_owned(),
            p: serde_json::Value::Object(payload1),
            h: None,
            raw_index: 1,
            raw_line: String::new(),
        };
        let ev2 = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:02.000Z".to_owned(),
            p: serde_json::Value::Object(payload2),
            h: None,
            raw_index: 2,
            raw_line: String::new(),
        };

        let result = enrich_session(vec![ev0, ev1, ev2]);
        assert_eq!(result.len(), 3);

        // ev0: input_bytes=5, output_bytes=0, prompt=0 → cumulative=5
        assert_eq!(result[0].input_bytes, 5);
        assert_eq!(result[0].output_bytes, 0);
        assert_eq!(result[0].context_cumulative_bytes, 5);

        // ev1: input_bytes=0, output_bytes=6, prompt=0 → cumulative=11
        assert_eq!(result[1].input_bytes, 0);
        assert_eq!(result[1].output_bytes, 6);
        assert_eq!(result[1].context_cumulative_bytes, 11);

        // ev2: input_bytes=0, output_bytes=0, prompt=10 → cumulative=21
        assert_eq!(result[2].input_bytes, 0);
        assert_eq!(result[2].output_bytes, 0);
        assert_eq!(result[2].context_cumulative_bytes, 21);
    }

    // -----------------------------------------------------------------------
    // event_hash_stable
    // -----------------------------------------------------------------------

    /// Same input twice → identical hash.
    #[test]
    fn event_hash_stable() {
        let ev = make_envelope("sess-1", "SessionStart", "2024-01-01T00:00:00.000Z", 0);
        let r1 = enrich_session(vec![ev.clone()]);
        let r2 = enrich_session(vec![ev]);
        assert_eq!(
            r1[0].event_hash, r2[0].event_hash,
            "same input must produce identical event_hash"
        );
    }

    // -----------------------------------------------------------------------
    // event_hash_matches_python_algorithm
    // -----------------------------------------------------------------------

    /// Manually compute the hash using the Python algorithm and assert it matches
    /// the Rust output.  This does NOT run Python; it reproduces the algorithm inline.
    ///
    /// Python algorithm (verbatim from ingest.py lines 414-421):
    /// ```python
    /// def _compute_event_hash(event: dict) -> str:
    ///     key = "|".join([
    ///         event.get("session_id", ""),
    ///         event.get("event_type", ""),
    ///         event.get("timestamp", ""),
    ///         event.get("tool_use_id") or "",
    ///     ])
    ///     return hashlib.sha256(key.encode("utf-8")).hexdigest()[:16]
    /// ```
    ///
    /// For our test event:
    ///   session_id  = "test-session-42"
    ///   event_type  = "PreToolUse"
    ///   timestamp   = "2024-06-15T12:30:45.123Z"
    ///   tool_use_id = "tool-xyz-999"
    ///
    /// The key string is:
    ///   "test-session-42|PreToolUse|2024-06-15T12:30:45.123Z|tool-xyz-999"
    #[test]
    fn event_hash_matches_python_algorithm() {
        let session_id = "test-session-42";
        let event_type = "PreToolUse";
        let timestamp = "2024-06-15T12:30:45.123Z";
        let tool_use_id = "tool-xyz-999";

        // Reference computation — identical to Python's algorithm:
        let key = format!("{session_id}|{event_type}|{timestamp}|{tool_use_id}");
        let mut ref_hasher = Sha256::new();
        ref_hasher.update(key.as_bytes());
        let ref_digest = ref_hasher.finalize();
        let ref_hex: String = ref_digest.iter().map(|b| format!("{b:02x}")).collect();
        let expected_hash = &ref_hex[..16];

        // Build an envelope and run through the enricher.
        let mut payload = serde_json::Map::new();
        payload.insert("hook_event_name".to_owned(), json!(event_type));
        payload.insert("session_id".to_owned(), json!(session_id));
        payload.insert("tool_use_id".to_owned(), json!(tool_use_id));

        let ev = Envelope {
            v: 1,
            ts: timestamp.to_owned(),
            p: serde_json::Value::Object(payload),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };

        let result = enrich_session(vec![ev]);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].event_hash, expected_hash,
            "Rust hash must match Python algorithm for key={key:?}"
        );
    }

    // -----------------------------------------------------------------------
    // event_hash_no_tool_use_id
    // -----------------------------------------------------------------------

    /// When tool_use_id is absent, Python uses `or ""` → empty string.
    /// Rust should produce the same result.
    #[test]
    fn event_hash_no_tool_use_id() {
        let session_id = "sess-noid";
        let event_type = "SessionStart";
        let timestamp = "2024-01-01T00:00:00Z";

        // Reference: tool_use_id is "" (Python's `event.get("tool_use_id") or ""`)
        let key = format!("{session_id}|{event_type}|{timestamp}|");
        let mut ref_hasher = Sha256::new();
        ref_hasher.update(key.as_bytes());
        let ref_digest = ref_hasher.finalize();
        let ref_hex: String = ref_digest.iter().map(|b| format!("{b:02x}")).collect();
        let expected = &ref_hex[..16];

        let ev = make_envelope(session_id, event_type, timestamp, 0);
        let result = enrich_session(vec![ev]);
        assert_eq!(result[0].event_hash, expected);
    }

    // -----------------------------------------------------------------------
    // event_hash_length
    // -----------------------------------------------------------------------

    /// event_hash must always be exactly 16 lowercase hex characters.
    #[test]
    fn event_hash_length() {
        let ev = make_envelope("s1", "SessionStart", "2024-01-01T00:00:00.000Z", 0);
        let result = enrich_session(vec![ev]);
        let hash = &result[0].event_hash;
        assert_eq!(
            hash.len(),
            16,
            "event_hash must be 16 hex chars, got {hash:?}"
        );
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "event_hash must be lowercase hex, got {hash:?}"
        );
    }

    // -----------------------------------------------------------------------
    // precomputed_hash_preserved
    // -----------------------------------------------------------------------

    /// When the envelope's `h` field is already set (pre-computed by capture.sh),
    /// the enricher must NOT overwrite it — mirroring Python's
    /// `if ev.get("event_hash") is None`.
    #[test]
    fn precomputed_hash_preserved() {
        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": "SessionStart",
                "session_id": "s1",
            }),
            h: Some("precomputed1234".to_owned()),
            raw_index: 0,
            raw_line: String::new(),
        };
        let result = enrich_session(vec![ev]);
        assert_eq!(
            result[0].event_hash, "precomputed1234",
            "pre-computed hash must be preserved"
        );
    }

    // -----------------------------------------------------------------------
    // field_isolation_session_start
    // -----------------------------------------------------------------------

    #[test]
    fn field_isolation_session_start() {
        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": "SessionStart",
                "session_id": "s1",
                "source": "startup",
            }),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };
        let result = enrich_session(vec![ev]);
        assert_eq!(result[0].isolation.source.as_deref(), Some("startup"));
        assert!(result[0].isolation.reason.is_none());
        assert!(result[0].isolation.compact_trigger.is_none());
    }

    // -----------------------------------------------------------------------
    // field_isolation_session_end
    // -----------------------------------------------------------------------

    #[test]
    fn field_isolation_session_end() {
        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:01.000Z".to_owned(),
            p: json!({
                "hook_event_name": "SessionEnd",
                "session_id": "s1",
                "reason": "logout",
            }),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };
        let result = enrich_session(vec![ev]);
        assert_eq!(result[0].isolation.reason.as_deref(), Some("logout"));
        assert!(result[0].isolation.source.is_none());
    }

    // -----------------------------------------------------------------------
    // field_isolation_slash_command
    // -----------------------------------------------------------------------

    #[test]
    fn field_isolation_slash_command() {
        let slash_ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "s1",
                "prompt": "/help me",
            }),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };
        let plain_ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:01.000Z".to_owned(),
            p: json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "s1",
                "prompt": "plain text",
            }),
            h: None,
            raw_index: 1,
            raw_line: String::new(),
        };
        let result = enrich_session(vec![slash_ev, plain_ev]);
        assert_eq!(result[0].isolation.is_slash_command, 1, "slash command");
        assert_eq!(result[1].isolation.is_slash_command, 0, "plain text");
    }

    // -----------------------------------------------------------------------
    // context_bytes_non_ascii_matches_python
    // -----------------------------------------------------------------------

    /// Verify that byte counting for non-ASCII payloads matches Python's
    /// `len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))`.
    ///
    /// Reference Python calculation:
    ///   json.dumps({"x": "é"}, separators=(",", ":"))
    ///   → '{"x":"\\u00e9"}'    (ensure_ascii=True escapes é as \\u00e9)
    ///   → 14 characters: { " x " : " \ u 0 0 e 9 " }
    ///   → 14 UTF-8 bytes (all ASCII after escaping)
    ///
    /// The envelope has `tool_input` = `{"x": "é"}` (as a JSON object, not a
    /// string), so the non-string branch of `byte_len_of_value` is exercised.
    #[test]
    fn context_bytes_non_ascii_matches_python() {
        let mut payload = serde_json::Map::new();
        payload.insert("hook_event_name".to_owned(), json!("PreToolUse"));
        payload.insert("session_id".to_owned(), json!("s1"));
        // tool_input is an object (not a string) so it goes through python_json_compact.
        let mut tool_input = serde_json::Map::new();
        tool_input.insert("x".to_owned(), json!("\u{00e9}"));
        payload.insert(
            "tool_input".to_owned(),
            serde_json::Value::Object(tool_input),
        );

        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: serde_json::Value::Object(payload),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };

        let result = enrich_session(vec![ev]);
        assert_eq!(result.len(), 1);

        // python3 -c "import json; s = json.dumps({'x': 'é'}, separators=(',',':')); print(s, len(s.encode('utf-8')))"
        // → {"x":"é"} 14
        // Breakdown: { "x" : "é" } = 1+3+1+8+1 = 14 bytes
        assert_eq!(
            result[0].input_bytes, 14,
            "input_bytes for {{\"x\":\"\\u00e9\"}} must be 14 (all-ASCII after ensure_ascii escaping)"
        );
    }

    // -----------------------------------------------------------------------
    // skill_detection_string_tool_input
    // -----------------------------------------------------------------------

    /// When `tool_input` is a JSON string containing a `.claude/skills/` path,
    /// `enrich_session` must populate `skill_name` and `skill_type` in the
    /// enriched event's envelope payload.
    ///
    /// Mirrors Python ingest.py lines 343-348.
    #[test]
    fn skill_detection_string_tool_input() {
        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_input": "read_file \".claude/skills/agents/foo-skill.md\"",
            }),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };

        let result = enrich_session(vec![ev]);
        assert_eq!(result.len(), 1);

        let p = &result[0].envelope.p;
        assert_eq!(
            p.get("skill_name").and_then(serde_json::Value::as_str),
            Some("foo-skill"),
            "skill_name must be detected from string tool_input"
        );
        assert_eq!(
            p.get("skill_type").and_then(serde_json::Value::as_str),
            Some("agent_definition"),
            "skill_type must be detected from agents/ path"
        );
    }

    // -----------------------------------------------------------------------
    // skill_detection_object_tool_input
    // -----------------------------------------------------------------------

    /// When `tool_input` is a JSON object (not a string), `enrich_session` must
    /// compact-serialize it and then scan for a `.claude/skills/` path.
    ///
    /// Mirrors Python: `json.dumps(tool_input_raw, separators=(",", ":"))`.
    #[test]
    fn skill_detection_object_tool_input() {
        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_input": {
                    "path": ".claude/skills/system/base-instructions.md",
                    "action": "read",
                },
            }),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };

        let result = enrich_session(vec![ev]);
        assert_eq!(result.len(), 1);

        let p = &result[0].envelope.p;
        assert_eq!(
            p.get("skill_name").and_then(serde_json::Value::as_str),
            Some("base-instructions"),
            "skill_name must be detected from compact-serialized object tool_input"
        );
        assert_eq!(
            p.get("skill_type").and_then(serde_json::Value::as_str),
            Some("system_skill"),
            "skill_type must be detected from system/ path"
        );
    }

    // -----------------------------------------------------------------------
    // skill_detection_skipped_when_already_set
    // -----------------------------------------------------------------------

    /// When `skill_name` is already present in the payload (e.g. injected by a
    /// hook upstream), `enrich_session` must NOT overwrite it.
    ///
    /// Mirrors Python: `if ev.get("skill_name") is None:`.
    #[test]
    fn skill_detection_skipped_when_already_set() {
        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_input": "read_file \".claude/skills/agents/other-skill.md\"",
                "skill_name": "upstream-skill",
                "skill_type": "project_skill",
            }),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };

        let result = enrich_session(vec![ev]);
        assert_eq!(result.len(), 1);

        let p = &result[0].envelope.p;
        assert_eq!(
            p.get("skill_name").and_then(serde_json::Value::as_str),
            Some("upstream-skill"),
            "pre-existing skill_name must not be overwritten"
        );
        assert_eq!(
            p.get("skill_type").and_then(serde_json::Value::as_str),
            Some("project_skill"),
            "pre-existing skill_type must not be overwritten"
        );
    }

    // -----------------------------------------------------------------------
    // skill_detection_no_match_leaves_fields_absent
    // -----------------------------------------------------------------------

    /// When `tool_input` has no `.claude/skills/` path, neither `skill_name`
    /// nor `skill_type` should be inserted into the payload.
    #[test]
    fn skill_detection_no_match_leaves_fields_absent() {
        let ev = Envelope {
            v: 1,
            ts: "2024-01-01T00:00:00.000Z".to_owned(),
            p: json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_input": "read_file \"/some/other/path.txt\"",
            }),
            h: None,
            raw_index: 0,
            raw_line: String::new(),
        };

        let result = enrich_session(vec![ev]);
        assert_eq!(result.len(), 1);

        let p = &result[0].envelope.p;
        assert!(
            p.get("skill_name").is_none(),
            "skill_name must not be inserted when no skill path is found"
        );
        assert!(
            p.get("skill_type").is_none(),
            "skill_type must not be inserted when no skill path is found"
        );
    }
}
