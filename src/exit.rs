//! Exit code conventions used by `hooked` subcommands.
//!
//! Mirrors the Python CLI's convention:
//! - 0: success
//! - 1: generic error
//! - 130: interrupted (SIGINT / Ctrl-C); used by the `tail` subcommand

/// Successful completion.
pub const SUCCESS: i32 = 0;

/// Generic error (invalid arguments, I/O failure, etc.).
pub const ERROR: i32 = 1;

/// Interrupted by SIGINT (Ctrl-C).
///
/// The `tail` subcommand exits with this code when the user presses Ctrl-C,
/// matching the POSIX convention of `128 + signal_number` where SIGINT = 2.
pub const INTERRUPTED: i32 = 130;
