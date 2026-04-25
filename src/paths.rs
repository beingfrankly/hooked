//! Path constants for the Claude Code telemetry system.
//!
//! This is the single source of truth for every filesystem path used by
//! the ingest/query pipeline.  All paths are derived from `$HOME` at
//! runtime so they are correct regardless of which user runs the binary.
//!
//! Python originals live in `~/.claude/telemetry/ingest.py` and
//! `~/.claude/telemetry/query.py`.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Private HOME helper
// ---------------------------------------------------------------------------

/// Returns the current user's home directory from the `HOME` environment
/// variable.  Panics early with a clear message if `HOME` is not set, which
/// mirrors the implicit behaviour of Python's `Path.home()`.
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME environment variable must be set")
}

// ---------------------------------------------------------------------------
// Core telemetry paths
// ---------------------------------------------------------------------------

/// Mirrors Python `TELEMETRY_DIR`.
///
/// Base directory for all Claude Code telemetry data: `~/.claude/telemetry`.
pub fn telemetry_dir() -> PathBuf {
    home().join(".claude").join("telemetry")
}

/// Mirrors Python `LOG_DIR`.
///
/// Directory where Claude Code writes daily JSONL log files:
/// `~/.claude/telemetry/logs`.
pub fn log_dir() -> PathBuf {
    telemetry_dir().join("logs")
}

/// Mirrors Python `ARCHIVE_DIR`.
///
/// Destination for gzip-archived JSONL files after ingestion:
/// `~/.claude/telemetry/logs/archive`.
pub fn archive_dir() -> PathBuf {
    log_dir().join("archive")
}

/// Mirrors Python `DB_PATH`.
///
/// Path to the SQLite database: `~/.claude/telemetry/sessions.db`.
pub fn db_path() -> PathBuf {
    telemetry_dir().join("sessions.db")
}

/// Mirrors Python `LOCK_FILE`.
///
/// Exclusive file lock used to prevent concurrent ingestion:
/// `~/.claude/telemetry/.ingest.lock`.
pub fn lock_file() -> PathBuf {
    telemetry_dir().join(".ingest.lock")
}

/// Mirrors Python `LAST_INGEST_FILE`.
///
/// Timestamp file updated after each successful ingestion run:
/// `~/.claude/telemetry/.last_ingest`.
pub fn last_ingest_file() -> PathBuf {
    telemetry_dir().join(".last_ingest")
}

/// Mirrors Python `SCHEMA_MARKER`.
///
/// Marker file whose content equals the current schema version string.
/// Its presence signals that the DB schema has been initialised:
/// `~/.claude/telemetry/.schema_v4`.
pub fn schema_marker() -> PathBuf {
    telemetry_dir().join(".schema_v4")
}

// ---------------------------------------------------------------------------
// Schema version (path-adjacent constant)
// ---------------------------------------------------------------------------

/// Mirrors Python `SCHEMA_VERSION`.
///
/// The string written into [`schema_marker`] and compared on startup.
pub const SCHEMA_VERSION: &str = "v4";

// ---------------------------------------------------------------------------
// Config files tracked for config-version hashing
// ---------------------------------------------------------------------------

/// Mirrors Python `CONFIG_FILES[0]`.
///
/// Claude Code user settings file: `~/.claude/settings.json`.
pub fn claude_settings_file() -> PathBuf {
    home().join(".claude").join("settings.json")
}

/// Mirrors Python `CONFIG_FILES[1]`.
///
/// Claude Code user instructions file: `~/.claude/CLAUDE.md`.
pub fn claude_md_file() -> PathBuf {
    home().join(".claude").join("CLAUDE.md")
}

/// Mirrors Python `CONFIG_FILES` (the full list).
///
/// Returns the ordered list of config files used when computing the
/// `config_version` hash.
pub fn config_files() -> Vec<PathBuf> {
    vec![claude_settings_file(), claude_md_file()]
}

// ---------------------------------------------------------------------------
// Log filename pattern
// ---------------------------------------------------------------------------

/// Prefix used in daily JSONL log filenames (e.g. `hook_logs_2024-01-15.jsonl`).
///
/// Mirrors the glob pattern `hook_logs_*.jsonl` used in Python `ingest.py`.
pub const LOG_FILE_PREFIX: &str = "hook_logs_";

/// Extension of daily JSONL log files (without the leading dot).
///
/// Mirrors the `.jsonl` suffix used in Python `ingest.py`.
pub const LOG_FILE_EXTENSION: &str = "jsonl";

/// Builds a daily JSONL log filename for the given date string (e.g. `"2024-01-15"`).
///
/// Returns `hook_logs_<date>.jsonl`, matching the naming convention in
/// Python `ingest.py` line ~1102.
pub fn log_file_for_date(date: &str) -> String {
    format!("{LOG_FILE_PREFIX}{date}.{LOG_FILE_EXTENSION}")
}

/// Returns the path to a daily JSONL log file inside [`log_dir`].
pub fn log_file_path(date: &str) -> PathBuf {
    log_dir().join(log_file_for_date(date))
}

// ---------------------------------------------------------------------------
// Obsidian vault (query.py only)
// ---------------------------------------------------------------------------

/// Mirrors Python `VAULT_PATH` (defined in `query.py`).
///
/// Root of the Obsidian Second Brain vault: `~/Sync/Obsidian/Second Brain`.
pub fn vault_path() -> PathBuf {
    home().join("Sync").join("Obsidian").join("Second Brain")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Helper: run a closure with `HOME` temporarily set to `fake_home`,
    /// then restore the original value.  Not thread-safe — tests in this
    /// module must not be run in parallel (use `cargo test -- --test-threads=1`
    /// if you add more env-mutating tests).
    fn with_fake_home<F: FnOnce()>(fake_home: &str, f: F) {
        let original = env::var_os("HOME");
        // SAFETY: single-threaded test context.
        unsafe {
            env::set_var("HOME", fake_home);
        }
        f();
        match original {
            Some(v) => unsafe { env::set_var("HOME", v) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    fn telemetry_dir_ends_with_expected_segments() {
        with_fake_home("/tmp/fake-home", || {
            let p = telemetry_dir();
            assert!(
                p.ends_with(".claude/telemetry"),
                "expected …/.claude/telemetry, got {p:?}"
            );
            assert!(p.starts_with("/tmp/fake-home"));
        });
    }

    #[test]
    fn log_dir_is_inside_telemetry_dir() {
        with_fake_home("/tmp/fake-home", || {
            assert!(log_dir().starts_with(telemetry_dir()));
            assert!(log_dir().ends_with("logs"));
        });
    }

    #[test]
    fn archive_dir_is_inside_log_dir() {
        with_fake_home("/tmp/fake-home", || {
            assert!(archive_dir().starts_with(log_dir()));
            assert!(archive_dir().ends_with("archive"));
        });
    }

    #[test]
    fn db_path_filename_is_sessions_db() {
        with_fake_home("/tmp/fake-home", || {
            assert_eq!(db_path().file_name().unwrap(), "sessions.db");
        });
    }

    #[test]
    fn lock_file_name() {
        with_fake_home("/tmp/fake-home", || {
            assert_eq!(lock_file().file_name().unwrap(), ".ingest.lock");
        });
    }

    #[test]
    fn last_ingest_file_name() {
        with_fake_home("/tmp/fake-home", || {
            assert_eq!(last_ingest_file().file_name().unwrap(), ".last_ingest");
        });
    }

    #[test]
    fn schema_marker_name() {
        with_fake_home("/tmp/fake-home", || {
            assert_eq!(schema_marker().file_name().unwrap(), ".schema_v4");
        });
    }

    #[test]
    fn schema_version_matches_marker_filename_suffix() {
        // The marker filename is `.schema_<version>` — keep them in sync.
        let expected_name = format!(".schema_{SCHEMA_VERSION}");
        with_fake_home("/tmp/fake-home", || {
            assert_eq!(
                schema_marker().file_name().unwrap().to_string_lossy(),
                expected_name
            );
        });
    }

    #[test]
    fn log_file_for_date_format() {
        assert_eq!(
            log_file_for_date("2024-01-15"),
            "hook_logs_2024-01-15.jsonl"
        );
    }

    #[test]
    fn log_file_path_inside_log_dir() {
        with_fake_home("/tmp/fake-home", || {
            let p = log_file_path("2024-01-15");
            assert!(p.starts_with(log_dir()));
            assert_eq!(p.file_name().unwrap(), "hook_logs_2024-01-15.jsonl");
        });
    }

    #[test]
    fn config_files_has_two_entries() {
        with_fake_home("/tmp/fake-home", || {
            let files = config_files();
            assert_eq!(files.len(), 2);
            assert!(files[0].ends_with(".claude/settings.json"));
            assert!(files[1].ends_with(".claude/CLAUDE.md"));
        });
    }

    #[test]
    fn vault_path_ends_with_expected_segments() {
        with_fake_home("/tmp/fake-home", || {
            let p = vault_path();
            assert!(p.starts_with("/tmp/fake-home"));
            assert!(p.ends_with("Sync/Obsidian/Second Brain"));
        });
    }
}
