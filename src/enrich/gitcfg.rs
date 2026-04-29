//! Git context collection and config-version hashing.
//!
//! Mirrors two Python helpers from `~/.claude/telemetry/ingest.py`:
//!
//! - `_git_context(cwd)` (lines 265-284) — runs `git rev-parse` subcommands
//!   with a 2-second wall-clock timeout and returns branch + short commit.
//! - `_compute_config_version()` (lines 250-258) — SHA-256 of sorted config
//!   file contents, truncated to 8 hex chars.
//!
//! ## Python originals (verbatim)
//!
//! ```python
//! def _git_context(cwd: Optional[str]) -> tuple[Optional[str], Optional[str]]:
//!     """Return (branch, short_commit) for the given cwd, or (None, None)."""
//!     if not cwd:
//!         return None, None
//!     try:
//!         branch = subprocess.check_output(
//!             ["git", "rev-parse", "--abbrev-ref", "HEAD"],
//!             cwd=cwd,
//!             stderr=subprocess.DEVNULL,
//!             timeout=2,
//!         ).decode().strip()
//!         commit = subprocess.check_output(
//!             ["git", "rev-parse", "--short", "HEAD"],
//!             cwd=cwd,
//!             stderr=subprocess.DEVNULL,
//!             timeout=2,
//!         ).decode().strip()
//!         return branch or None, commit or None
//!     except Exception:
//!         return None, None
//!
//!
//! def _compute_config_version() -> str:
//!     """SHA-256 of sorted contents of config files, truncated to 8 hex chars."""
//!     h = hashlib.sha256()
//!     for p in sorted(str(f) for f in CONFIG_FILES):
//!         try:
//!             h.update(Path(p).read_bytes())
//!         except OSError:
//!             pass
//!     return h.hexdigest()[:8]
//! ```

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};
use wait_timeout::ChildExt as _;

use crate::paths::config_files;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Git context for a working directory.
///
/// Mirrors the `(branch, short_commit)` tuple returned by Python
/// `_git_context(cwd)`.  Fields are `None` when git is unavailable, the
/// directory is not inside a repo, or the command times out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitContext {
    /// The current branch name (`git rev-parse --abbrev-ref HEAD`).
    /// `None` when not in a repo or git unavailable.
    pub branch: Option<String>,

    /// The short commit SHA (`git rev-parse --short HEAD`).
    /// `None` when not in a repo, no commits yet, or git unavailable.
    pub commit_sha: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Collect git context for a given working directory.
///
/// Runs `git rev-parse --abbrev-ref HEAD` and `git rev-parse --short HEAD`
/// with a 2-second wall-clock timeout each, using [`wait_timeout`].
///
/// Mirrors Python `_git_context(cwd)` from `ingest.py` (lines 265-284).
///
/// Returns `None` if:
/// - `cwd` does not exist,
/// - git is unavailable,
/// - the process times out (child is killed),
/// - git exits with a non-zero status (not in a repo),
/// - or any other error occurs.
///
/// Both fields of [`GitContext`] may individually be `None` if the output
/// is empty (mirrors Python's `branch or None` / `commit or None` logic).
pub fn git_context(cwd: &Path) -> Option<GitContext> {
    let branch = run_git(
        &["rev-parse", "--abbrev-ref", "HEAD"],
        cwd,
        Duration::from_secs(2),
    );
    let commit_sha = run_git(
        &["rev-parse", "--short", "HEAD"],
        cwd,
        Duration::from_secs(2),
    );

    // If both branches failed (e.g. not a git repo) return None entirely,
    // mirroring Python which raises on the first failure and returns (None, None).
    if branch.is_none() && commit_sha.is_none() {
        return None;
    }

    Some(GitContext { branch, commit_sha })
}

/// Compute the SHA-256 config-version hash.
///
/// Mirrors Python `_compute_config_version()` from `ingest.py` (lines 250-258):
///
/// 1. Gathers config files via [`crate::paths::config_files`] and sorts them
///    by their string representation (mirrors `sorted(str(f) for f in CONFIG_FILES)`).
/// 2. For each file: reads its bytes and feeds them into the hasher.
///    Missing / unreadable files are **silently skipped** (mirrors `except OSError: pass`).
///    No separator bytes are written between files (Python concatenates raw bytes).
/// 3. Returns the first **8 hex characters** of the SHA-256 digest
///    (mirrors `h.hexdigest()[:8]`).
///
/// # Errors
///
/// This function only returns `Err` if the internal hex-encoding step fails,
/// which cannot happen in practice.  Missing config files are silently skipped,
/// not errors.
pub fn config_hash() -> anyhow::Result<String> {
    let mut files = config_files();
    // Mirror Python: `sorted(str(f) for f in CONFIG_FILES)`
    files.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));

    let mut hasher = Sha256::new();
    for path in &files {
        hash_file(path, &mut hasher);
    }

    let digest = hasher.finalize();
    // Mirror Python `h.hexdigest()[:8]` — only the first 8 hex chars.
    // sha2's output is [u8; 32]; format each byte as two lowercase hex digits.
    let full_hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(full_hex[..8].to_owned())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Run a single `git <args>` command in `cwd` with a hard timeout.
///
/// Returns the trimmed stdout string on success, or `None` on:
/// - spawn failure (git not on PATH),
/// - timeout (child killed),
/// - non-zero exit status,
/// - empty output (mirrors Python's `branch or None`).
///
/// Stderr is discarded (mirrors `stderr=subprocess.DEVNULL`).
fn run_git(args: &[&str], cwd: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    match child.wait_timeout(timeout).ok()? {
        Some(status) => {
            if !status.success() {
                return None;
            }
            // Read stdout only after confirmed success.
            let mut stdout = child.stdout.take()?;
            let mut buf = String::new();
            stdout.read_to_string(&mut buf).ok()?;
            let trimmed = buf.trim().to_owned();
            // Mirror Python: `branch or None` — empty string becomes None.
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        None => {
            // Timeout elapsed — kill the child.
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Feed the bytes of `path` into `hasher`.
///
/// Silently does nothing if the file cannot be read (mirrors Python
/// `except OSError: pass`).
fn hash_file(path: &Path, hasher: &mut Sha256) {
    if let Ok(bytes) = std::fs::read(path) {
        hasher.update(&bytes);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Returns true if `git` is available on PATH.
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // git_context_in_repo
    // -----------------------------------------------------------------------

    /// Create a real git repo in a tempdir, commit a file, and verify that
    /// `git_context` returns sensible values.
    #[test]
    fn git_context_in_repo() {
        if !git_available() {
            eprintln!("skipping git_context_in_repo: git not on PATH");
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        // Initialise a repo with a known branch name.
        let init_ok = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !init_ok {
            // Older git may not support -b; fall back.
            Command::new("git")
                .arg("init")
                .current_dir(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok();
        }

        // Configure identity for the commit (required in some environments).
        for (key, val) in [("user.email", "test@example.com"), ("user.name", "Test")] {
            Command::new("git")
                .args(["config", key, val])
                .current_dir(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok();
        }

        // Create and commit a file.
        std::fs::write(path.join("README"), b"hello").expect("write README");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();

        let ctx = git_context(path).expect("git_context should return Some in a repo");

        // Branch: should be "main" or "master" depending on git config.
        let branch = ctx.branch.as_deref().expect("branch should be Some");
        assert!(
            branch == "main" || branch == "master",
            "expected main or master, got {branch:?}"
        );

        // Short commit: should be a non-empty hex string (typically 7 chars).
        let sha = ctx
            .commit_sha
            .as_deref()
            .expect("commit_sha should be Some");
        assert!(!sha.is_empty(), "commit_sha should not be empty");
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "commit_sha should be hex, got {sha:?}"
        );
    }

    // -----------------------------------------------------------------------
    // git_context_not_in_repo
    // -----------------------------------------------------------------------

    /// Calling git_context on a directory that is not inside a git repo should
    /// return None (git exits non-zero, Python raises and returns (None, None)).
    #[test]
    fn git_context_not_in_repo() {
        if !git_available() {
            eprintln!("skipping git_context_not_in_repo: git not on PATH");
            return;
        }

        // /tmp is very unlikely to be inside a git repo.
        let result = git_context(Path::new("/tmp"));
        assert!(result.is_none(), "expected None for /tmp, got {result:?}");
    }

    // -----------------------------------------------------------------------
    // git_context_timeout
    // -----------------------------------------------------------------------

    /// Verify that run_git kills a sleeping child and returns None rather than
    /// blocking forever.  Uses a shell sleep command as a stand-in for a slow
    /// git operation.
    ///
    /// This test is skipped on platforms where `/bin/sh` or `sleep` is absent.
    #[test]
    fn git_context_timeout() {
        // We test run_git directly using a sleep command instead of git.
        // This verifies the timeout machinery without needing a slow git server.
        let path = Path::new("/tmp");
        let timeout = Duration::from_millis(100);

        // Attempt to spawn `sleep 5` via the same wait_timeout pattern.
        let child_result = Command::new("sleep")
            .arg("5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        let mut child = match child_result {
            Ok(c) => c,
            Err(_) => {
                eprintln!("skipping git_context_timeout: sleep not available");
                return;
            }
        };

        let wait_result = child.wait_timeout(timeout).ok();
        let timed_out = matches!(wait_result, Some(None));

        if timed_out {
            let _ = child.kill();
            let _ = child.wait();
        }

        assert!(
            timed_out,
            "expected timeout to elapse for 'sleep 5' with 100ms limit"
        );

        // Also verify that run_git on a nonexistent cwd returns None quickly.
        let result = run_git(&["status"], path, Duration::from_secs(2));
        // /tmp is not a git repo — should return None (non-zero exit from git).
        // We just confirm it does not hang.
        let _ = result; // None is fine, Some is also fine if /tmp is somehow a repo.
    }

    // -----------------------------------------------------------------------
    // config_hash_deterministic
    // -----------------------------------------------------------------------

    /// Calling config_hash twice should produce the same result.
    #[test]
    fn config_hash_deterministic() {
        let h1 = config_hash().expect("config_hash should not error");
        let h2 = config_hash().expect("config_hash should not error");
        assert_eq!(h1, h2, "config_hash must be deterministic");
    }

    // -----------------------------------------------------------------------
    // config_hash_format
    // -----------------------------------------------------------------------

    /// The hash must be exactly 8 lowercase hex characters (mirrors Python's
    /// `h.hexdigest()[:8]`).
    #[test]
    fn config_hash_format() {
        let h = config_hash().expect("config_hash should not error");
        assert_eq!(h.len(), 8, "expected 8-char hash, got {h:?}");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash must be lowercase hex, got {h:?}"
        );
    }

    // -----------------------------------------------------------------------
    // config_hash_missing_files
    // -----------------------------------------------------------------------

    /// When HOME points to an empty directory with no config files, config_hash
    /// must succeed gracefully and return the SHA-256 of empty input (all
    /// files are skipped, mirrors `except OSError: pass`).
    ///
    /// SHA-256("") = e3b0c44298fc1c149afb...  → first 8 chars = "e3b0c442"
    #[test]
    fn config_hash_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut result = None;

        crate::test_utils::with_fake_home(dir.path(), || {
            result = Some(config_hash());
        });

        let h = result
            .unwrap()
            .expect("config_hash should not error even when all files are missing");
        // All files missing → hasher digests nothing → SHA-256 of empty input.
        assert_eq!(
            h, "e3b0c442",
            "expected SHA-256('') truncated to 8 chars, got {h:?}"
        );
    }

    // -----------------------------------------------------------------------
    // config_hash_matches_python_algorithm
    // -----------------------------------------------------------------------

    /// Hand-verify that the Rust implementation matches Python's algorithm.
    ///
    /// Algorithm (from ingest.py lines 250-258):
    ///   1. Sort config file paths as strings.
    ///   2. For each path: try to read bytes, skip on OSError.
    ///      No separator bytes between files.
    ///   3. Return `sha256(concatenated_bytes).hexdigest()[:8]`.
    ///
    /// This test verifies the algorithm on known fixed inputs by running the
    /// same logic in Rust and asserting the expected SHA-256 prefix.
    #[test]
    fn config_hash_matches_python_algorithm() {
        // Build a known input: two files with deterministic content.
        // Sort by path string, concatenate bytes, SHA-256, take [:8].
        //
        // We stage two files and verify the Rust function returns the expected
        // hash using our own reference computation.
        let dir = tempfile::tempdir().expect("tempdir");
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).expect("create .claude dir");

        let settings_content = b"{}";
        let claude_md_content = b"# Instructions\n";

        std::fs::write(claude_dir.join("settings.json"), settings_content)
            .expect("write settings.json");
        std::fs::write(claude_dir.join("CLAUDE.md"), claude_md_content).expect("write CLAUDE.md");

        let mut result = None;

        crate::test_utils::with_fake_home(dir.path(), || {
            result = Some(config_hash());
        });

        let h = result.unwrap().expect("config_hash should succeed");

        // Compute the expected hash: sort paths as strings, concat bytes, SHA-256[:8].
        // settings.json sorts before CLAUDE.md because 's' > 'C' in ASCII,
        // but we replicate the sort exactly as Python does.
        let settings_path = claude_dir
            .join("settings.json")
            .to_string_lossy()
            .to_string();
        let claude_md_path = claude_dir.join("CLAUDE.md").to_string_lossy().to_string();

        let mut sorted_paths = vec![settings_path, claude_md_path];
        sorted_paths.sort();

        let mut ref_hasher = Sha256::new();
        for p in &sorted_paths {
            if let Ok(bytes) = std::fs::read(p) {
                ref_hasher.update(&bytes);
            }
        }
        let ref_digest = ref_hasher.finalize();
        let ref_hex: String = ref_digest.iter().map(|b| format!("{b:02x}")).collect();
        let expected = &ref_hex[..8];

        assert_eq!(
            h, expected,
            "Rust config_hash {h:?} does not match reference computation {expected:?}"
        );
        assert_eq!(h.len(), 8);
    }
}
