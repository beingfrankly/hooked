//! Shared test utilities for `hooked` unit tests.
//!
//! This module is compiled only under `#[cfg(test)]` (declared as
//! `#[cfg(test)] mod test_utils` in `lib.rs`).

use std::env;
use std::ffi::OsStr;
use std::sync::Mutex;

// Process-wide lock serializing HOME mutations across all test modules.
//
// `cargo test` runs tests in parallel by default; without this lock, each
// module's old `with_home` helper raced on the global `$HOME` and tests
// that read paths derived from $HOME (notably the schema marker) saw
// each other's fake homes.  T03's strict init_db turned this latent race
// into visible failures; this mutex restores correctness without requiring
// `--test-threads=1`.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with `HOME` temporarily set to `fake_home`, then restore the
/// original value.
///
/// The function accepts anything that converts to an `OsStr` reference —
/// `&str`, `&String`, `&Path`, and `&PathBuf` all work.
///
/// Serialization: the `HOME_LOCK` mutex is held for the full duration of
/// `f()`, so concurrent tests that call this helper are forced to run one
/// at a time.  Tests that do NOT touch `$HOME` are unaffected and still
/// execute in parallel.
///
/// Poison-tolerant: if a previous test panicked while holding the lock,
/// `unwrap_or_else(|e| e.into_inner())` lets subsequent tests proceed
/// rather than deadlocking the entire suite.
///
/// Panic behaviour: if `f` panics, the original `$HOME` is NOT restored
/// (the thread unwinds, the lock is poisoned, and the next caller gets
/// the poisoned-but-still-acquired guard via `into_inner()`).  This
/// matches the behaviour of the five local helpers this replaces, and is
/// acceptable because each test uses a unique `tempdir` for `fake_home`.
pub(crate) fn with_fake_home<S: AsRef<OsStr>, F: FnOnce()>(fake_home: S, f: F) {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let original = env::var_os("HOME");
    // SAFETY: serialized via HOME_LOCK above; no concurrent access to $HOME
    // from other with_fake_home callers is possible while we hold the guard.
    unsafe {
        env::set_var("HOME", fake_home.as_ref());
    }
    f();
    match original {
        Some(v) => unsafe { env::set_var("HOME", v) },
        None => unsafe { env::remove_var("HOME") },
    }
}
