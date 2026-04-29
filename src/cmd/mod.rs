//! Subcommand handlers.
//!
//! Each handler is a free function that takes its parsed args and returns `anyhow::Result<()>`.
//! Phase 3 lands stubs only; Phase 4 (T4.1–T4.3) implements them.
//!
//! Implemented modules (T4.1a batch):
//!   tools, agents, skills, configs, slow, tokens, before_stop, compactions
//!
//! Implemented modules (T4.1b batch):
//!   session, chain, search, backup, export, replay
//!
//! Implemented modules (T4.2 batch):
//!   summary, sessions, last, failures, diff, trends
//!   util (shared formatting helpers)
//!
//! Implemented modules (T4.3 batch):
//!   ingest, label, annotate, prune, health, append_daily

// ---------------------------------------------------------------------------
// Shared formatting helpers
// ---------------------------------------------------------------------------

pub mod util;

// ---------------------------------------------------------------------------
// Implemented subcommand modules (T4.1a)
// ---------------------------------------------------------------------------

pub mod agents;
pub mod before_stop;
pub mod compactions;
pub mod configs;
pub mod skills;
pub mod slow;
pub mod tokens;
pub mod tools;

// Re-exports so callers can use `cmd::tools(args, fmt)` directly.
pub use agents::agents;
pub use before_stop::before_stop;
pub use compactions::compactions;
pub use configs::configs;
pub use skills::skills;
pub use slow::slow;
pub use tokens::tokens;
pub use tools::tools;

// ---------------------------------------------------------------------------
// Implemented subcommand modules (T4.1b)
// ---------------------------------------------------------------------------

pub mod backup;
pub mod chain;
pub mod export;
pub mod replay;
pub mod search;
pub mod session;

pub use backup::backup;
pub use chain::chain;
pub use export::export;
pub use replay::replay;
pub use search::search;
pub use session::session;

// ---------------------------------------------------------------------------
// Implemented subcommand modules (T4.2 — M-group)
// ---------------------------------------------------------------------------

pub mod diff;
pub mod failures;
pub mod last;
pub mod sessions;
pub mod summary;
pub mod trends;

pub use diff::diff;
pub use failures::failures;
pub use last::last;
pub use sessions::sessions;
pub use summary::summary;
pub use trends::trends;

// ---------------------------------------------------------------------------
// L-group: implemented subcommand modules (T4.3)
// ---------------------------------------------------------------------------

pub mod annotate;
pub mod append_daily;
pub mod health;
pub mod init;
pub mod ingest;
pub mod label;
pub mod prune;

pub use annotate::annotate;
pub use append_daily::append_daily;
pub use health::health;
pub use init::init;
pub use ingest::ingest;
pub use label::label;
pub use prune::prune;

// ---------------------------------------------------------------------------
// Implemented subcommand modules (T4.3b)
// ---------------------------------------------------------------------------

pub mod sql;
pub mod tail;

pub use sql::sql;
pub use tail::tail;

// ---------------------------------------------------------------------------
// Implemented subcommand modules (T4.3c)
// ---------------------------------------------------------------------------

pub mod rebuild;

pub use rebuild::rebuild;

// ---------------------------------------------------------------------------
// Implemented subcommand modules (T4.3d)
// ---------------------------------------------------------------------------

pub mod import_legacy;

pub use import_legacy::import_legacy;
