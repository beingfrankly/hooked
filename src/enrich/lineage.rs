//! Cross-session lineage scoring: identify the most likely parent session.
//!
//! Mirrors Python `enrich_cross_session` and `_lineage_score` from
//! `~/.claude/telemetry/ingest.py` (lines 428–586).
//!
//! ## Algorithm summary
//!
//! For each new session (ordered by `started_at`), every candidate session that
//! ended within the preceding 30-minute window is scored using a weighted
//! multi-signal function.  The candidate with the highest score that meets the
//! acceptance threshold (`>= 6`) becomes the direct parent.
//!
//! ## Scoring signals (mirrors Python `_lineage_score`)
//!
//! | Signal                              | Points |
//! |-------------------------------------|--------|
//! | Time gap < 30 s                     | +5     |
//! | Time gap < 2 min (120 s)            | +4     |
//! | Time gap < 5 min (300 s)            | +3     |
//! | Time gap < 15 min (900 s)           | +2     |
//! | Time gap < 30 min (1800 s)          | +1     |
//! | Time gap >= 30 min                  | +0     |
//! | Same `cwd` (both non-empty, equal)  | +3     |
//! | Same `git_branch` (both non-empty)  | +2     |
//! | Same `model` (both non-empty)       | +1     |
//! | `end_reason` ≠ `"logout"`           | +2     |
//!
//! **Acceptance threshold:** score >= 6.
//!
//! **Tie-break rule:** when two candidates score equally, the more recent one
//! wins.  This mirrors Python's `if score > best_score` (strict greater-than)
//! over candidates iterated from most-recent to oldest — the first candidate
//! at a given score is kept, which is the most-recently-ended session.
//!
//! ## Non-transitive chaining
//!
//! [`find_parent_session`] returns only the **direct** parent.  `chain_id`
//! assignment is the caller's responsibility — the caller should inherit the
//! parent's existing `chain_id` (from the DB) or create a new one and
//! back-fill it onto the parent.  This function does **not** walk the chain
//! backwards to find a root.
//!
//! In other words: if A→B and B→C, this function called for C will return B
//! (not A).  The `chain_id` for C is set by the caller using B's `chain_id`.
//!
//! ## 1:1 parent relation
//!
//! Each session has **at most one** parent.  A parent may have multiple
//! children (the relation is not bidirectional).
//!
//! ## Look-back boundary
//!
//! The 30-minute window is measured from `prev.last_event_ts` (the candidate's
//! last event) to `new_session.first_event_ts` (the new session's first event).
//! Both are required to be present; if either is absent the time-gap score is 0
//! and the candidate can only qualify via the non-time signals.
//!
//! // Caller responsibility: chain_id propagation

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Minimal per-session data required to compute lineage scores.
///
/// Mirrors the summary dict built by Python's `enrich_cross_session`:
///
/// ```python
/// {
///     "session_id":  sid,
///     "started_at":  ...,   # first_event_ts in Rust
///     "ended_at":    ...,   # last_event_ts  in Rust
///     "end_reason":  ...,
///     "source":      ...,
///     "cwd":         ...,
///     "git_branch":  ...,
///     "model":       ...,
/// }
/// ```
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// Unique session identifier.
    pub session_id: String,

    /// Timestamp of the first event in the session (used as `started_at`).
    pub first_event_ts: Option<DateTime<Utc>>,

    /// Timestamp of the last event in the session (used as `ended_at`).
    pub last_event_ts: Option<DateTime<Utc>>,

    /// Reason the previous session ended (from `SessionEnd.reason`).
    /// `None` means the session did not have a clean `SessionEnd` — Python
    /// treats this as "not logout", so the `+2` end_reason bonus still applies.
    pub end_reason: Option<String>,

    /// Session source (`"startup"` | `"resume"` | `"compact"` | …).
    /// Only `"startup"` and `"resume"` sessions participate in lineage.
    pub source: Option<String>,

    /// Working directory at session start (`p["cwd"]`).
    pub cwd: Option<String>,

    /// Git branch at session start (enriched by [`crate::enrich::gitcfg`]).
    pub git_branch: Option<String>,

    /// Model name used in the session.
    pub model: Option<String>,
}

/// Result of a successful parent match.
#[derive(Debug, Clone, PartialEq)]
pub struct ParentMatch {
    /// `session_id` of the best parent candidate.
    pub parent_session_id: String,

    /// Final integer score (>= 6).
    pub score: i32,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Maximum allowed gap (seconds) between a candidate's last event and the new
/// session's first event.  Exactly 1800 s (30 minutes) — mirrors Python:
/// ```python
/// if gap_s > 1800:  # 30 minutes — no need to look further back
///     break
/// ```
pub const LOOK_BACK_SECS: i64 = 1800;

/// Minimum score required to accept a parent candidate.
///
/// Mirrors Python (line 519):
/// ```python
/// if best_score >= 6 and best_prev is not None:
/// ```
pub const SCORE_THRESHOLD: i32 = 6;

/// Compute the most likely parent session for `new_session` from `candidates`.
///
/// `candidates` should be sorted **oldest-to-newest** by `first_event_ts`
/// (ascending started_at), matching the order Python's `summaries` list
/// produces after `summaries.sort(key=lambda s: s["started_at"])`.  The
/// function scans from the most-recent candidate backwards (highest index
/// first), stopping as soon as it encounters a candidate whose last event
/// is more than 30 minutes before `new_session.first_event_ts`.
///
/// Returns `None` if:
/// - `candidates` is empty, or
/// - no candidate scores at or above [`SCORE_THRESHOLD`], or
/// - `new_session.source` is not `"startup"` or `"resume"`.
///
/// ## Non-transitive chaining
/// The returned parent is the **direct** parent only.  Chain-id propagation
/// is the caller's responsibility (see module-level documentation).
///
/// // Caller responsibility: chain_id propagation
pub fn find_parent_session(
    new_session: &SessionSummary,
    candidates: &[SessionSummary],
) -> Option<ParentMatch> {
    // Only startup/resume sessions participate (mirrors Python line 488-489).
    match new_session.source.as_deref() {
        Some("startup") | Some("resume") => {}
        _ => return None,
    }

    let mut best_score: i32 = 0;
    let mut best_prev: Option<&SessionSummary> = None;

    // Scan from most-recent candidate backwards — mirrors Python:
    // `for j in range(i - 1, -1, -1):`
    for prev in candidates.iter().rev() {
        // Only startup/resume sessions are eligible parents (mirrors Python line 495-497).
        match prev.source.as_deref() {
            Some("startup") | Some("resume") => {}
            _ => continue,
        }

        let score = lineage_score(prev, new_session);
        // Strict greater-than: ties keep the first (most-recent) candidate found.
        if score > best_score {
            best_score = score;
            best_prev = Some(prev);
        }

        // Break early if this candidate ended more than 30 minutes before the
        // new session started.  Mirrors Python lines 504-516:
        // ```python
        // gap_s = (t_curr_start - t_prev_end).total_seconds()
        // if gap_s > 1800:
        //     break
        // ```
        if let (Some(prev_end), Some(new_start)) = (prev.last_event_ts, new_session.first_event_ts)
        {
            let gap_s = (new_start - prev_end).num_seconds();
            if gap_s > LOOK_BACK_SECS {
                break;
            }
        }
    }

    if best_score >= SCORE_THRESHOLD {
        best_prev.map(|p| ParentMatch {
            parent_session_id: p.session_id.clone(),
            score: best_score,
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Scoring — mirrors Python `_lineage_score`
// ---------------------------------------------------------------------------

/// Compute the lineage score between a preceding session (`prev`) and a
/// candidate successor (`current`).
///
/// Python source (verbatim, lines 544–586):
///
/// ```python
/// def _lineage_score(prev: dict, current: dict) -> int:
///     score = 0
///
///     # Time gap score
///     if prev["ended_at"] and current["started_at"]:
///         try:
///             t_end = datetime.fromisoformat(prev["ended_at"].replace("Z", "+00:00"))
///             t_start = datetime.fromisoformat(current["started_at"].replace("Z", "+00:00"))
///             gap_s = (t_start - t_end).total_seconds()
///             if gap_s < 30:
///                 score += 5
///             elif gap_s < 120:
///                 score += 4
///             elif gap_s < 300:
///                 score += 3
///             elif gap_s < 900:
///                 score += 2
///             elif gap_s < 1800:
///                 score += 1
///         except Exception:
///             pass
///
///     # Same cwd
///     if prev["cwd"] and current["cwd"] and prev["cwd"] == current["cwd"]:
///         score += 3
///
///     # Same git branch
///     if (
///         prev["git_branch"]
///         and current["git_branch"]
///         and prev["git_branch"] == current["git_branch"]
///     ):
///         score += 2
///
///     # Same model
///     if prev["model"] and current["model"] and prev["model"] == current["model"]:
///         score += 1
///
///     # Previous end_reason not "logout"
///     if prev["end_reason"] != "logout":
///         score += 2
///
///     return score
/// ```
///
/// In Rust the timestamps are already parsed `DateTime<Utc>` values; no
/// string manipulation is required.
pub fn lineage_score(prev: &SessionSummary, current: &SessionSummary) -> i32 {
    let mut score: i32 = 0;

    // --- Time gap score ---
    // Uses prev.last_event_ts (ended_at) and current.first_event_ts (started_at).
    if let (Some(t_end), Some(t_start)) = (prev.last_event_ts, current.first_event_ts) {
        let gap_s = (t_start - t_end).num_seconds();
        if gap_s < 30 {
            score += 5;
        } else if gap_s < 120 {
            score += 4;
        } else if gap_s < 300 {
            score += 3;
        } else if gap_s < 900 {
            score += 2;
        } else if gap_s < 1800 {
            score += 1;
        }
        // gap_s >= 1800 → +0 (no contribution)
    }

    // --- Same cwd ---
    // Python: `if prev["cwd"] and current["cwd"] and prev["cwd"] == current["cwd"]`
    if let (Some(p), Some(c)) = (prev.cwd.as_deref(), current.cwd.as_deref())
        && !p.is_empty()
        && !c.is_empty()
        && p == c
    {
        score += 3;
    }

    // --- Same git_branch ---
    if let (Some(p), Some(c)) = (prev.git_branch.as_deref(), current.git_branch.as_deref())
        && !p.is_empty()
        && !c.is_empty()
        && p == c
    {
        score += 2;
    }

    // --- Same model ---
    if let (Some(p), Some(c)) = (prev.model.as_deref(), current.model.as_deref())
        && !p.is_empty()
        && !c.is_empty()
        && p == c
    {
        score += 1;
    }

    // --- Previous end_reason not "logout" ---
    // Python: `if prev["end_reason"] != "logout":` — this is true when
    // end_reason is None *or* any string other than "logout".
    if prev.end_reason.as_deref() != Some("logout") {
        score += 2;
    }

    score
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn utc(year: i32, month: u32, day: u32, h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, h, m, s)
            .single()
            .expect("valid datetime")
    }

    /// Build a minimal SessionSummary that participates in lineage scoring.
    fn session(
        id: &str,
        ended_minutes_ago: Option<f64>,
        started_now: bool,
        cwd: Option<&str>,
        git_branch: Option<&str>,
        model: Option<&str>,
        end_reason: Option<&str>,
    ) -> SessionSummary {
        let now = utc(2024, 6, 1, 12, 0, 0);
        let last_event_ts =
            ended_minutes_ago.map(|mins| now - chrono::Duration::seconds((mins * 60.0) as i64));
        let first_event_ts = if started_now { Some(now) } else { None };

        SessionSummary {
            session_id: id.to_owned(),
            first_event_ts,
            last_event_ts,
            end_reason: end_reason.map(str::to_owned),
            source: Some("startup".to_owned()),
            cwd: cwd.map(str::to_owned),
            git_branch: git_branch.map(str::to_owned),
            model: model.map(str::to_owned),
        }
    }

    // -----------------------------------------------------------------------
    // within_30min_cwd_match
    //
    // Candidate ended 5 min ago (300 s, bucket < 900 s → time = +2), same cwd.
    // time(+2) + cwd(+3) + end_reason(None≠logout, +2) → total = 7 ≥ 6 ✓
    // -----------------------------------------------------------------------
    #[test]
    fn within_30min_cwd_match() {
        let prev = session(
            "prev-1",
            Some(5.0),
            false,
            Some("/home/user/project"),
            None,
            None,
            None, // end_reason=None → +2
        );
        let new = session(
            "new-1",
            None,
            true,
            Some("/home/user/project"),
            None,
            None,
            None,
        );

        let result = find_parent_session(&new, &[prev]);
        assert!(
            result.is_some(),
            "should accept parent when cwd matches within 30 min"
        );
        let m = result.unwrap();
        assert_eq!(m.parent_session_id, "prev-1");
        assert!(m.score >= SCORE_THRESHOLD, "score {} < threshold", m.score);
    }

    // -----------------------------------------------------------------------
    // outside_30min_window
    //
    // Mirrors Python's `break` behavior precisely:
    //
    //   The loop iterates from most-recent to oldest.  When it encounters a
    //   candidate whose `gap_s > 1800`, it SCORES that candidate first, then
    //   breaks (stops looking at even older sessions).  The candidate itself is
    //   not excluded — it simply contributes 0 to the time score.
    //
    //   For rejection, the remaining signals must be insufficient to reach the
    //   threshold on their own.  Here: end_reason(+2) only → total = 2 < 6.
    // -----------------------------------------------------------------------
    #[test]
    fn outside_30min_window() {
        let prev = session(
            "prev-old",
            Some(31.0),
            false,
            None, // no cwd
            None, // no branch
            None, // no model
            None, // end_reason=None (+2 bonus, but not enough alone)
        );
        let new = session("new-1", None, true, None, None, None, None);

        // gap = 31 min → time = 0; end_reason = +2 → total = 2 < 6 → None
        let result = find_parent_session(&new, &[prev]);
        assert!(
            result.is_none(),
            "31-min gap with no other signals: score 2 < threshold 6 → None"
        );
    }

    // -----------------------------------------------------------------------
    // outside_30min_window_score_only
    //
    // Verify the time signal contributes 0 for a 31-min gap.
    // The candidate IS scored (break fires after scoring in Python).
    // With cwd match: time=0 + cwd=+3 + end_reason=+2 → 5 < 6.
    // -----------------------------------------------------------------------
    #[test]
    fn outside_30min_window_score_only() {
        let now = utc(2024, 6, 1, 12, 0, 0);
        let prev = SessionSummary {
            session_id: "p".to_owned(),
            first_event_ts: None,
            last_event_ts: Some(now - chrono::Duration::seconds(31 * 60)), // 31 min ago
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/x".to_owned()),
            git_branch: None,
            model: None,
        };
        let current = SessionSummary {
            session_id: "c".to_owned(),
            first_event_ts: Some(now),
            last_event_ts: None,
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/x".to_owned()),
            git_branch: None,
            model: None,
        };
        // gap >= 1800s → time = 0; cwd = +3; end_reason(None) = +2 → score = 5
        let score = lineage_score(&prev, &current);
        assert_eq!(score, 5, "31-min gap: time=0 + cwd=3 + end_reason=2 = 5");
    }

    // -----------------------------------------------------------------------
    // outside_30min_window_all_signals
    //
    // Confirm that a 31-min-ago candidate CAN still be accepted when its
    // non-time signals are strong enough.  This matches Python: the break
    // fires AFTER the candidate is scored.
    //
    // cwd(+3) + branch(+2) + model(+1) + end_reason(+2) = 8 ≥ 6 → parent accepted.
    // -----------------------------------------------------------------------
    #[test]
    fn outside_30min_window_all_signals() {
        let prev = session(
            "prev-strong",
            Some(31.0),
            false,
            Some("/project"),
            Some("main"),
            Some("claude-3-opus"),
            None,
        );
        let new = session(
            "new-strong",
            None,
            true,
            Some("/project"),
            Some("main"),
            Some("claude-3-opus"),
            None,
        );

        let result = find_parent_session(&new, &[prev]);
        assert!(
            result.is_some(),
            "31-min gap but cwd+branch+model+end_reason = 8 ≥ 6 → parent accepted"
        );
        assert_eq!(result.unwrap().score, 8);
    }

    // -----------------------------------------------------------------------
    // no_signals_match
    //
    // Candidate is within window but no cwd/branch/model match.
    // Gap = 5 min = 300 s → bucket < 900 s → time = +2; end_reason None → +2;
    // total = 4 < 6.
    // -----------------------------------------------------------------------
    #[test]
    fn no_signals_match() {
        let prev = session(
            "prev-2",
            Some(5.0),
            false,
            Some("/other/project"),
            Some("feature-x"),
            Some("claude-2"),
            None,
        );
        let new = session(
            "new-2",
            None,
            true,
            Some("/different/cwd"),
            Some("main"),
            Some("claude-3"),
            None,
        );

        let result = find_parent_session(&new, &[prev]);
        assert!(
            result.is_none(),
            "should reject parent when no signals match (score < threshold)"
        );
    }

    // -----------------------------------------------------------------------
    // multiple_candidates_highest_wins
    //
    // Two valid candidates; higher-scoring one is selected.
    // -----------------------------------------------------------------------
    #[test]
    fn multiple_candidates_highest_wins() {
        let now = utc(2024, 6, 1, 12, 0, 0);

        // Candidate A: ended 20 min ago (1200 s → bucket <1800 s → time=+1),
        // different cwd → time=+1, end_reason=+2 → score = 3
        let candidate_a = SessionSummary {
            session_id: "cand-a".to_owned(),
            first_event_ts: None,
            last_event_ts: Some(now - chrono::Duration::seconds(20 * 60)),
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/project/other".to_owned()),
            git_branch: None,
            model: None,
        };

        // Candidate B: ended 5 min ago (300 s → bucket <900 s → time=+2),
        // same cwd → time=+2, cwd=+3, end_reason=+2 → score = 7
        let candidate_b = SessionSummary {
            session_id: "cand-b".to_owned(),
            first_event_ts: None,
            last_event_ts: Some(now - chrono::Duration::seconds(5 * 60)),
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/project/main".to_owned()),
            git_branch: None,
            model: None,
        };

        let new = SessionSummary {
            session_id: "new-3".to_owned(),
            first_event_ts: Some(now),
            last_event_ts: None,
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/project/main".to_owned()),
            git_branch: None,
            model: None,
        };

        // candidates must be sorted oldest-to-newest
        let result = find_parent_session(&new, &[candidate_a, candidate_b]);
        assert!(result.is_some(), "should find a parent");
        let m = result.unwrap();
        assert_eq!(
            m.parent_session_id, "cand-b",
            "higher-scoring candidate (cand-b, score=7) must win over cand-a (score=3)"
        );
    }

    // -----------------------------------------------------------------------
    // tie_break_deterministic
    //
    // Two candidates with identical scores: the most-recent one wins.
    // Python scans from most-recent to oldest using strict `>`, so the first
    // candidate seen (most recent) is kept when scores tie.
    // -----------------------------------------------------------------------
    #[test]
    fn tie_break_deterministic() {
        let now = utc(2024, 6, 1, 12, 0, 0);

        // Both ended 5 min ago (same gap bucket → same time score), same cwd.
        // Score: time=+1, cwd=+3, end_reason=+2 → 6 each.
        let older = SessionSummary {
            session_id: "older-sess".to_owned(),
            first_event_ts: None,
            // slightly older (5 min + 10s)
            last_event_ts: Some(now - chrono::Duration::seconds(5 * 60 + 10)),
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/shared/cwd".to_owned()),
            git_branch: None,
            model: None,
        };
        let newer = SessionSummary {
            session_id: "newer-sess".to_owned(),
            first_event_ts: None,
            // slightly newer (5 min exactly)
            last_event_ts: Some(now - chrono::Duration::seconds(5 * 60)),
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/shared/cwd".to_owned()),
            git_branch: None,
            model: None,
        };

        let new = SessionSummary {
            session_id: "new-tie".to_owned(),
            first_event_ts: Some(now),
            last_event_ts: None,
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/shared/cwd".to_owned()),
            git_branch: None,
            model: None,
        };

        // Sorted oldest-to-newest: [older, newer]
        // Scan reversed: newer is visited first → becomes best_prev.
        // older has same score → strict `>` is false → does NOT replace.
        let result = find_parent_session(&new, &[older, newer]);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().parent_session_id,
            "newer-sess",
            "tie-break: most-recent candidate (newer-sess) must win"
        );
    }

    // -----------------------------------------------------------------------
    // empty_candidate_list
    // -----------------------------------------------------------------------
    #[test]
    fn empty_candidate_list() {
        let new = session("new-empty", None, true, Some("/cwd"), None, None, None);
        let result = find_parent_session(&new, &[]);
        assert!(result.is_none(), "empty candidate list → None");
    }

    // -----------------------------------------------------------------------
    // exact_python_formula
    //
    // Construct a known candidate with precomputed expected score and assert
    // Rust produces the same integer value.
    //
    // Inputs:
    //   gap = 45 s  → bucket < 120 s → time = +4
    //   cwd match   → +3
    //   branch match → +2
    //   model match  → +1
    //   end_reason = "normal" (≠ logout) → +2
    //   Expected total = 4 + 3 + 2 + 1 + 2 = 12
    // -----------------------------------------------------------------------
    #[test]
    fn exact_python_formula() {
        let now = utc(2024, 6, 1, 12, 0, 0);

        let prev = SessionSummary {
            session_id: "known-prev".to_owned(),
            first_event_ts: None,
            last_event_ts: Some(now - chrono::Duration::seconds(45)),
            end_reason: Some("normal".to_owned()),
            source: Some("startup".to_owned()),
            cwd: Some("/workspace/hooked".to_owned()),
            git_branch: Some("main".to_owned()),
            model: Some("claude-3-opus".to_owned()),
        };

        let current = SessionSummary {
            session_id: "known-curr".to_owned(),
            first_event_ts: Some(now),
            last_event_ts: None,
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/workspace/hooked".to_owned()),
            git_branch: Some("main".to_owned()),
            model: Some("claude-3-opus".to_owned()),
        };

        let score = lineage_score(&prev, &current);
        assert_eq!(
            score, 12,
            "exact formula: time(45s→<120s=+4) + cwd(+3) + branch(+2) + model(+1) + end_reason(+2) = 12"
        );

        // Also verify find_parent_session picks it up
        let result = find_parent_session(&current, &[prev]);
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.score, 12);
        assert_eq!(m.parent_session_id, "known-prev");
    }

    // -----------------------------------------------------------------------
    // source_filter
    //
    // Sessions with source != "startup" | "resume" should not link.
    // -----------------------------------------------------------------------
    #[test]
    fn source_filter_new_session() {
        let prev = session("prev-s", Some(1.0), false, Some("/cwd"), None, None, None);
        let mut new = session("new-s", None, true, Some("/cwd"), None, None, None);
        new.source = Some("compact".to_owned());

        let result = find_parent_session(&new, &[prev]);
        assert!(
            result.is_none(),
            "compact source new session should not receive a parent"
        );
    }

    #[test]
    fn source_filter_candidate() {
        let now = utc(2024, 6, 1, 12, 0, 0);
        let mut prev = session("prev-c", Some(1.0), false, Some("/cwd"), None, None, None);
        prev.source = Some("compact".to_owned());

        let new = SessionSummary {
            session_id: "new-c".to_owned(),
            first_event_ts: Some(now),
            last_event_ts: None,
            end_reason: None,
            source: Some("startup".to_owned()),
            cwd: Some("/cwd".to_owned()),
            git_branch: None,
            model: None,
        };

        let result = find_parent_session(&new, &[prev]);
        assert!(
            result.is_none(),
            "compact source candidate should not be an eligible parent"
        );
    }

    // -----------------------------------------------------------------------
    // end_reason_logout_penalty
    //
    // end_reason = "logout" → the +2 bonus is NOT applied.
    // Gap 5 min → +1; logout → 0; total = 1 < threshold.
    // -----------------------------------------------------------------------
    #[test]
    fn end_reason_logout_no_bonus() {
        let prev = session(
            "prev-logout",
            Some(5.0),
            false,
            None, // no cwd match
            None,
            None,
            Some("logout"),
        );
        let new = session("new-x", None, true, None, None, None, None);

        let score = lineage_score(&prev, &new);
        // time(5min=300s→<900s=+2) + no_cwd + no_branch + no_model + logout(no bonus) = 2
        assert_eq!(
            score, 2,
            "logout end_reason: no +2 bonus; 5-min gap → +2 time"
        );
    }

    // -----------------------------------------------------------------------
    // resume_source_accepted
    //
    // "resume" source is also eligible (mirrors Python: startup or resume).
    // -----------------------------------------------------------------------
    #[test]
    fn resume_source_accepted() {
        let now = utc(2024, 6, 1, 12, 0, 0);
        let prev = SessionSummary {
            session_id: "prev-resume".to_owned(),
            first_event_ts: None,
            last_event_ts: Some(now - chrono::Duration::seconds(10)),
            end_reason: None,
            source: Some("resume".to_owned()),
            cwd: Some("/cwd/r".to_owned()),
            git_branch: Some("dev".to_owned()),
            model: None,
        };
        let new = SessionSummary {
            session_id: "new-resume".to_owned(),
            first_event_ts: Some(now),
            last_event_ts: None,
            end_reason: None,
            source: Some("resume".to_owned()),
            cwd: Some("/cwd/r".to_owned()),
            git_branch: Some("dev".to_owned()),
            model: None,
        };

        let result = find_parent_session(&new, &[prev]);
        // gap<30s=+5, cwd=+3, branch=+2, end_reason=+2 → 12 ≥ 6
        assert!(result.is_some(), "resume source sessions should link");
    }
}
