//! Shared formatting helpers for query subcommands.
//!
//! These mirror the Python helpers `_fmt_duration`, `_fmt_bytes`, and
//! `_truncate` from `query.py`.  All helpers are `pub(crate)` so that
//! any `cmd::*` module can use them without exposing them outside the crate.

/// Format duration in milliseconds as a human-readable string.
/// Mirrors Python `_fmt_duration`.
pub(crate) fn fmt_duration(ms: Option<i64>) -> String {
    match ms {
        None => String::new(),
        Some(ms) if ms < 1000 => format!("{}ms", ms),
        Some(ms) if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

/// Format bytes as a human-readable string.
/// Mirrors Python `_fmt_bytes`.
#[allow(dead_code)]
pub(crate) fn fmt_bytes(b: Option<i64>) -> String {
    match b {
        None => String::new(),
        Some(b) if b < 1024 => format!("{}B", b),
        Some(b) if b < 1024 * 1024 => format!("{:.1}K", b as f64 / 1024.0),
        Some(b) => format!("{:.1}M", b as f64 / (1024.0 * 1024.0)),
    }
}

/// Truncate a string to `n` codepoints, appending `…` if truncated.
/// Mirrors Python `_truncate`.
pub(crate) fn truncate(s: Option<&str>, n: usize) -> String {
    match s {
        None => String::new(),
        Some(s) if s.chars().count() > n => {
            let truncated: String = s.chars().take(n).collect();
            format!("{}…", truncated)
        }
        Some(s) => s.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_duration_formats_correctly() {
        assert_eq!(fmt_duration(None), "");
        assert_eq!(fmt_duration(Some(500)), "500ms");
        assert_eq!(fmt_duration(Some(1500)), "1.5s");
        assert_eq!(fmt_duration(Some(65_000)), "1m5s");
        assert_eq!(fmt_duration(Some(0)), "0ms");
    }

    #[test]
    fn fmt_bytes_formats_correctly() {
        assert_eq!(fmt_bytes(None), "");
        assert_eq!(fmt_bytes(Some(500)), "500B");
        assert_eq!(fmt_bytes(Some(2048)), "2.0K");
        assert_eq!(fmt_bytes(Some(2 * 1024 * 1024)), "2.0M");
    }

    #[test]
    fn truncate_works() {
        assert_eq!(truncate(None, 10), "");
        assert_eq!(truncate(Some("hello"), 10), "hello");
        assert_eq!(truncate(Some("hello world"), 5), "hello…");
    }
}
