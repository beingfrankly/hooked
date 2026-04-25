//! Gzip-based file archiving and advisory file locking for the ingest pipeline.
//!
//! Mirrors two behaviours from Python's `ingest.py`:
//!
//! 1. **`_archive_file`** (line 1130–1136): gzip-compress a JSONL file into
//!    `archive_dir()/<basename>.gz`, then delete the original.
//! 2. **File locking** (lines 1078–1094): exclusive advisory lock on
//!    `lock_file()` with a non-blocking try followed by a 5-second retry loop.
//!
//! # Python verbatim — `_archive_file`
//!
//! ```python
//! def _archive_file(path: Path) -> None:
//!     """Gzip a JSONL file into the archive directory, then delete the original."""
//!     ARCHIVE_DIR.mkdir(parents=True, exist_ok=True)
//!     dest = ARCHIVE_DIR / (path.name + ".gz")
//!     with open(path, "rb") as f_in, gzip.open(dest, "wb") as f_out:
//!         f_out.write(f_in.read())
//!     path.unlink()
//! ```
//!
//! # Python verbatim — lock section inside `ingest_all_unprocessed`
//!
//! ```python
//! lock_fd = open(lock_path, "w")
//! try:
//!     # Non-blocking first; fall back to 5s wait
//!     try:
//!         fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
//!     except BlockingIOError:
//!         import time
//!         deadline = time.monotonic() + 5.0
//!         while time.monotonic() < deadline:
//!             try:
//!                 fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
//!                 break
//!             except BlockingIOError:
//!                 time.sleep(0.1)
//!         else:
//!             print("[ingest] Another ingestion is running; skipping.", file=sys.stderr)
//!             return 0
//! finally:
//!     fcntl.flock(lock_fd, fcntl.LOCK_UN)
//!     lock_fd.close()
//! ```
//!
//! # File-lock implementation
//!
//! Uses `std::fs::File::lock()` and `std::fs::File::try_lock()`, which were
//! stabilised in Rust 1.89.0.  The pinned toolchain is 1.95.0 so this is
//! available.  The `fslock` crate is present in `Cargo.toml` as a fallback
//! but is not used.
//!
//! # Compression level
//!
//! Python's `gzip.open(dest, "wb")` uses `compresslevel=9` (Python's default).
//! We therefore use [`flate2::Compression::new(9)`].

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::paths;

// ---------------------------------------------------------------------------
// IngestLock
// ---------------------------------------------------------------------------

/// An exclusive advisory file lock on [`paths::lock_file()`].
///
/// Mirrors the `fcntl.flock(LOCK_EX | LOCK_NB)` pattern used in Python's
/// `ingest_all_unprocessed`.  The lock is released automatically when this
/// struct is dropped (the OS releases the lock when the `File` is closed).
///
/// # Lock strategy
///
/// Uses `std::fs::File::try_lock()` (non-blocking exclusive), stabilised in
/// Rust 1.89.0.  The [`IngestLock::acquire`] method retries for up to 5
/// seconds with 100 ms sleeps, exactly mirroring the Python retry loop.
pub struct IngestLock {
    _file: File,
}

impl IngestLock {
    /// Acquire an exclusive advisory lock, blocking for up to **5 seconds**.
    ///
    /// Mirrors Python:
    /// ```python
    /// try:
    ///     fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    /// except BlockingIOError:
    ///     deadline = time.monotonic() + 5.0
    ///     while time.monotonic() < deadline:
    ///         try:
    ///             fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    ///             break
    ///         except BlockingIOError:
    ///             time.sleep(0.1)
    ///     else:
    ///         print("[ingest] Another ingestion is running; skipping.")
    ///         return 0
    /// ```
    ///
    /// Returns `Ok(None)` if the lock could not be acquired within 5 seconds
    /// (another process holds it).  Returns `Err` only on I/O errors.
    pub fn acquire() -> anyhow::Result<Option<Self>> {
        let path = paths::lock_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create lock dir {parent:?}"))?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open lock file {path:?}"))?;

        // First attempt: non-blocking.
        match file.try_lock() {
            Ok(()) => return Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(anyhow::anyhow!("try_lock {path:?}: {e}")),
        }

        // Retry for up to 5 seconds with 100 ms sleeps — mirrors Python's
        // `deadline = time.monotonic() + 5.0` loop.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { _file: file })),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => {
                    return Err(anyhow::anyhow!("try_lock {path:?}: {e}"));
                }
            }
        }

        // Timed out — another process holds the lock.
        Ok(None)
    }

    /// Try to acquire the lock exactly once, **non-blocking**.
    ///
    /// Returns `Ok(Some(lock))` if acquired, `Ok(None)` if another process
    /// holds it.  Returns `Err` only on I/O errors.
    pub fn try_acquire() -> anyhow::Result<Option<Self>> {
        let path = paths::lock_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create lock dir {parent:?}"))?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open lock file {path:?}"))?;

        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(anyhow::anyhow!("try_lock {path:?}: {e}")),
        }
    }
}

impl Drop for IngestLock {
    fn drop(&mut self) {
        // `std::fs::File::lock` releases automatically when the file is
        // dropped (OS closes the fd).  We call `unlock()` explicitly for
        // clarity and to mirror `fcntl.flock(lock_fd, fcntl.LOCK_UN)`.
        let _ = self._file.unlock();
    }
}

// ---------------------------------------------------------------------------
// archive_jsonl
// ---------------------------------------------------------------------------

/// Gzip-compress `src` and move it to `archive_dir()/<basename>.gz`.
///
/// Mirrors Python's `_archive_file`:
///
/// ```python
/// def _archive_file(path: Path) -> None:
///     ARCHIVE_DIR.mkdir(parents=True, exist_ok=True)
///     dest = ARCHIVE_DIR / (path.name + ".gz")
///     with open(path, "rb") as f_in, gzip.open(dest, "wb") as f_out:
///         f_out.write(f_in.read())
///     path.unlink()
/// ```
///
/// Behaviour:
/// - Always re-compresses the input, even if `src` is already `.gz`
///   (Python does not special-case this).
/// - Destination is `archive_dir()/<src.filename>.gz`.
/// - Overwrites the destination if it already exists (Python does not check
///   for conflicts — `gzip.open` truncates and rewrites).
/// - Deletes `src` on successful write.
/// - Does **not** preserve mtime (Python does not preserve mtime either).
/// - Compression level **9** — matches Python's `gzip.open` default.
///
/// Returns the path of the created `.gz` file on success.
pub fn archive_jsonl(src: &Path) -> anyhow::Result<PathBuf> {
    let arch_dir = paths::archive_dir();
    std::fs::create_dir_all(&arch_dir)
        .with_context(|| format!("create archive dir {arch_dir:?}"))?;

    // Destination filename: <basename>.gz
    let src_name = src
        .file_name()
        .with_context(|| format!("src has no filename: {src:?}"))?;
    let mut dest_name = src_name.to_owned();
    dest_name.push(".gz");
    let dest = arch_dir.join(&dest_name);

    // Read source bytes.
    let src_bytes = std::fs::read(src).with_context(|| format!("read source file {src:?}"))?;

    // Gzip-compress into destination.  Python's `gzip.open(dest, "wb")`
    // defaults to compresslevel=9.
    let dest_file = File::create(&dest).with_context(|| format!("create archive file {dest:?}"))?;
    let mut encoder = GzEncoder::new(dest_file, Compression::new(9));
    encoder
        .write_all(&src_bytes)
        .with_context(|| format!("write compressed bytes to {dest:?}"))?;
    encoder
        .finish()
        .with_context(|| format!("finalise gzip stream for {dest:?}"))?;

    // Delete the original — mirrors `path.unlink()`.
    std::fs::remove_file(src).with_context(|| format!("remove source file {src:?}"))?;

    crate::info!("ingest", "archived {} → {}", src.display(), dest.display());

    Ok(dest)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use flate2::read::GzDecoder;
    use tempfile::TempDir;

    use super::*;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Run a closure with `HOME` set to `home_dir`, restoring afterwards.
    /// Must be called in a single-threaded test context.
    fn with_home<F: FnOnce()>(home_dir: &Path, f: F) {
        let original = std::env::var_os("HOME");
        // SAFETY: only used in single-threaded test context (--test-threads=1).
        unsafe {
            std::env::set_var("HOME", home_dir);
        }
        f();
        match original {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    // -----------------------------------------------------------------------
    // archive_plain_jsonl
    // -----------------------------------------------------------------------

    /// Write a plain `test.jsonl`, archive it, assert `.gz` exists in archive
    /// dir, contents decompress to the original, original file deleted.
    #[test]
    fn archive_plain_jsonl() {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().to_path_buf();

        // Create the source JSONL in a logs dir.
        let logs_dir = home.join(".claude/telemetry/logs");
        std::fs::create_dir_all(&logs_dir).expect("create logs dir");
        let src = logs_dir.join("hook_logs_2024-01-01.jsonl");
        let original_content = b"{\"v\":1,\"ts\":\"2024-01-01T00:00:00Z\",\"p\":{}}\n";
        std::fs::write(&src, original_content).expect("write src");

        with_home(&home, || {
            let dst = archive_jsonl(&src).expect("archive_jsonl");

            // Archive file should exist.
            assert!(dst.exists(), "archive file must exist: {dst:?}");
            assert_eq!(
                dst.file_name().unwrap().to_string_lossy(),
                "hook_logs_2024-01-01.jsonl.gz"
            );

            // Archive must be inside archive_dir().
            assert!(
                dst.starts_with(paths::archive_dir()),
                "archive must be inside archive_dir"
            );

            // Decompress and verify contents.
            let gz_bytes = std::fs::read(&dst).expect("read gz");
            let mut decoder = GzDecoder::new(gz_bytes.as_slice());
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed).expect("decompress");
            assert_eq!(decompressed, original_content, "decompressed != original");

            // Original must have been deleted.
            assert!(!src.exists(), "source file must be deleted after archive");
        });
    }

    // -----------------------------------------------------------------------
    // archive_preserves_content
    // -----------------------------------------------------------------------

    /// Round-trip: original bytes → gzip → decompress → must equal original.
    #[test]
    fn archive_preserves_content() {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().to_path_buf();

        let logs_dir = home.join(".claude/telemetry/logs");
        std::fs::create_dir_all(&logs_dir).expect("create logs dir");

        let original: Vec<u8> = (0u8..=255u8)
            .flat_map(|b| std::iter::repeat_n(b, 4))
            .collect();

        let src = logs_dir.join("hook_logs_2024-06-15.jsonl");
        std::fs::write(&src, &original).expect("write src");

        with_home(&home, || {
            let dst = archive_jsonl(&src).expect("archive_jsonl");

            let gz_bytes = std::fs::read(&dst).expect("read gz");
            let mut decoder = GzDecoder::new(gz_bytes.as_slice());
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed).expect("decompress");
            assert_eq!(
                decompressed, original,
                "round-trip must preserve every byte"
            );
        });
    }

    // -----------------------------------------------------------------------
    // archive_idempotent_or_fails
    // -----------------------------------------------------------------------

    /// Call archive_jsonl twice on the same source path (recreated between
    /// calls).  Python does not guard against overwriting an existing archive:
    /// `gzip.open(dest, "wb")` truncates and overwrites.  Rust mirrors that.
    #[test]
    fn archive_idempotent_or_fails() {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().to_path_buf();

        let logs_dir = home.join(".claude/telemetry/logs");
        std::fs::create_dir_all(&logs_dir).expect("create logs dir");

        with_home(&home, || {
            // First call.
            let src = logs_dir.join("hook_logs_2024-07-01.jsonl");
            std::fs::write(&src, b"first content\n").expect("write first");
            let dst1 = archive_jsonl(&src).expect("first archive");
            assert!(dst1.exists());

            // Recreate the source with different content and archive again.
            // Python would overwrite the destination; Rust should too.
            std::fs::write(&src, b"second content\n").expect("write second");
            let dst2 = archive_jsonl(&src).expect("second archive");
            assert_eq!(dst1, dst2, "destination path must be the same");

            // The destination now holds the second content.
            let gz_bytes = std::fs::read(&dst2).expect("read gz");
            let mut decoder = GzDecoder::new(gz_bytes.as_slice());
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed).expect("decompress");
            assert_eq!(decompressed, b"second content\n");

            // Source is deleted again.
            assert!(!src.exists());
        });
    }

    // -----------------------------------------------------------------------
    // lock_smoke
    // -----------------------------------------------------------------------

    /// Plain acquire / drop cycle completes without error.
    #[test]
    fn lock_smoke() {
        let tmp = TempDir::new().expect("tempdir");
        with_home(tmp.path(), || {
            let lock = IngestLock::acquire().expect("acquire");
            assert!(lock.is_some(), "must acquire on first call");
            drop(lock);
        });
    }

    // -----------------------------------------------------------------------
    // lock_exclusive_blocks_second_try_acquire
    // -----------------------------------------------------------------------

    /// Hold a lock, then call `try_acquire` from the same process.
    ///
    /// On Linux, `flock(2)` locks are per open-file-description, so a second
    /// `open` + `try_lock` from the same process on the same path may or may
    /// not be blocked depending on the OS.  On macOS (which uses `flock`
    /// semantics for `OFD`-style locks via `fcntl`), each `open` creates a
    /// new file description.  We use separate `File` handles opened by two
    /// `try_acquire` calls, which is the realistic scenario.
    ///
    /// This test verifies that the *second* `try_acquire` (a fresh `File`
    /// open) returns `None` while the first lock is held.
    ///
    /// Rust 1.89's `File::try_lock` uses OFD (open-file-description) locks on
    /// Linux (`fcntl F_OFD_SETLK`) and `flock(2)` on macOS.  Both are
    /// per-file-description, so two separate `File::open` calls on the same
    /// path yield independent lock holders even within the same process.
    #[test]
    fn lock_exclusive_blocks_second_try_acquire() {
        let tmp = TempDir::new().expect("tempdir");
        with_home(tmp.path(), || {
            // First acquire — should succeed.
            let first = IngestLock::try_acquire().expect("first try_acquire");
            assert!(first.is_some(), "first try_acquire must succeed");

            // Second acquire on the same lock file — must return None because
            // the first lock is still held (different file description).
            let second = IngestLock::try_acquire().expect("second try_acquire no I/O error");
            assert!(
                second.is_none(),
                "second try_acquire must return None while first lock is held"
            );

            drop(first);
        });
    }

    // -----------------------------------------------------------------------
    // lock_released_on_drop
    // -----------------------------------------------------------------------

    /// Acquire a lock, drop it, then acquire again — must succeed.
    #[test]
    fn lock_released_on_drop() {
        let tmp = TempDir::new().expect("tempdir");
        with_home(tmp.path(), || {
            {
                let lock = IngestLock::try_acquire().expect("first acquire");
                assert!(lock.is_some(), "first acquire must succeed");
                // lock is dropped here
            }

            let second = IngestLock::try_acquire().expect("second acquire after drop");
            assert!(
                second.is_some(),
                "must be acquirable after first lock is dropped"
            );
        });
    }
}
