//! Stderr logging helpers.
//!
//! Matches Python's `[component] LEVEL: message` format used across
//! `ingest.py` and `query.py`.
//!
//! # Examples
//!
//! ```
//! use hooked::{info, warn_, error_};
//!
//! info!("ingest", "processed {} events", 3);
//! // → [ingest] INFO: processed 3 events
//!
//! warn_!("query", "no results for session {}", "abc123");
//! // → [query] WARNING: no results for session abc123
//!
//! error_!("ingest", "failed to open {}: {}", "foo.db", "permission denied");
//! // → [ingest] ERROR: failed to open foo.db: permission denied
//! ```

use std::fmt::Arguments;
use std::io::Write;

/// Severity level for a log message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        }
    }
}

/// Format a log line without performing any I/O.
///
/// Used internally by [`log`] and directly in unit tests to verify the
/// output format without capturing stderr.
pub fn format_line(component: &str, level: Level, msg: &str) -> String {
    format!("[{component}] {}: {msg}", level.as_str())
}

/// Write a single log line to stderr.
///
/// Prefer the [`info!`], [`warn_!`], and [`error_!`] macros over calling
/// this function directly.
pub fn log(component: &str, level: Level, args: Arguments<'_>) {
    let msg = args.to_string();
    let line = format_line(component, level, &msg);
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
}

/// Log at [`Level::Info`].
///
/// ```
/// hooked::info!("ingest", "processed {n} events", n = 3);
/// ```
/// Writes `[ingest] INFO: processed 3 events` to stderr.
#[macro_export]
macro_rules! info {
    ($component:expr, $($arg:tt)*) => {
        $crate::logging::log(
            $component,
            $crate::logging::Level::Info,
            format_args!($($arg)*),
        )
    };
}

/// Log at [`Level::Warning`].
///
/// Named `warn_` to avoid shadowing the `warn!` macro from the `log` crate
/// and Rust's built-in `deprecated` warning mechanism.
#[macro_export]
macro_rules! warn_ {
    ($component:expr, $($arg:tt)*) => {
        $crate::logging::log(
            $component,
            $crate::logging::Level::Warning,
            format_args!($($arg)*),
        )
    };
}

/// Log at [`Level::Error`].
///
/// Named `error_` to avoid shadowing the `error!` macro from the `log` crate.
#[macro_export]
macro_rules! error_ {
    ($component:expr, $($arg:tt)*) => {
        $crate::logging::log(
            $component,
            $crate::logging::Level::Error,
            format_args!($($arg)*),
        )
    };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_line_info() {
        let line = format_line("ingest", Level::Info, "processed 3 events");
        assert_eq!(line, "[ingest] INFO: processed 3 events");
    }

    #[test]
    fn format_line_warning() {
        let line = format_line("query", Level::Warning, "no results");
        assert_eq!(line, "[query] WARNING: no results");
    }

    #[test]
    fn format_line_error() {
        let line = format_line("ingest", Level::Error, "failed to open foo.db");
        assert_eq!(line, "[ingest] ERROR: failed to open foo.db");
    }

    #[test]
    fn format_line_auto_ingest_pattern() {
        // Mirrors Python: print(f"[auto-ingest] WARNING: {exc}", file=sys.stderr)
        let line = format_line("auto-ingest", Level::Warning, "something went wrong");
        assert_eq!(line, "[auto-ingest] WARNING: something went wrong");
    }
}
