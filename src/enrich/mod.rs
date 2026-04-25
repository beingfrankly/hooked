//! Enrichment computations applied to parsed telemetry events.
//!
//! Each sub-module mirrors a section of the Python `ingest.py` enrichment
//! pipeline.  All enrichers are pure functions (no I/O) so they can be unit-
//! tested without a database or filesystem.
//!
//! ## Modules
//!
//! - [`skill`] — skill-name / skill-type detection from `tool_input` text,
//!   porting Python `_detect_skill` and `_SKILL_TYPE_MAP` from `ingest.py`.
//! - [`gitcfg`] — git context collection (`git rev-parse`, 2s timeout) and
//!   config-version hashing (SHA-256 of config files, truncated to 8 hex chars),
//!   porting Python `_git_context` and `_compute_config_version` from `ingest.py`.
//! - [`lineage`] — cross-session lineage scoring (non-transitive, 1:1 parent
//!   relation), porting Python `enrich_cross_session` and `_lineage_score`
//!   from `ingest.py`.  Returns the direct parent only; `chain_id` propagation
//!   is the caller's responsibility.

pub mod gitcfg;
pub mod lineage;
pub mod session;
pub mod skill;
