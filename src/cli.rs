//! CLI definition for `hooked` using clap derive.
//!
//! Mirrors the argparse setup in `query.py` — all 30 subcommands with the same
//! positional arguments, flags, defaults, and help text.

use clap::{Args, Parser, Subcommand, ValueEnum};

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "hooked",
    version,
    about = "Telemetry ingest + query for Claude Code",
    long_about = "Query tool for Claude Code session telemetry.\n\nSetup:\n  Add to your ~/.zshrc or ~/.bashrc:\n    alias ccq='hooked'\n\n  Then use: hooked summary, hooked sessions, hooked last, hooked search \"text\", etc."
)]
pub struct Cli {
    /// Output format (default: table for TTY, json for pipe)
    #[arg(long, value_enum, global = true, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    /// Shortcut for --format json
    #[arg(long, global = true, conflicts_with = "format")]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Returns the effective output format, with `--json` taking priority.
    pub fn effective_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Output format
// ---------------------------------------------------------------------------

#[derive(ValueEnum, Clone, Debug, Default, PartialEq)]
pub enum OutputFormat {
    /// Human-readable aligned table
    #[default]
    Table,
    /// JSON array
    Json,
    /// Comma-separated values
    Csv,
    /// GitHub-flavored Markdown table
    Markdown,
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Daily overview table (default: last 7 days)
    Summary(SummaryArgs),
    /// Recent sessions with filters
    Sessions(SessionsArgs),
    /// Full event timeline for a session (by ID prefix)
    Session(SessionArgs),
    /// Full event timeline for the most recent session
    Last(LastArgs),
    /// All sessions in a lineage chain
    Chain(ChainArgs),
    /// Tool usage stats (default: current config version)
    Tools(ToolsArgs),
    /// Subagent performance stats
    Agents(AgentsArgs),
    /// Skill usage by frequency
    Skills(SkillsArgs),
    /// Recent tool failures
    Failures(FailuresArgs),
    /// Pattern analysis of events before Stop
    #[command(name = "before-stop")]
    BeforeStop(BeforeStopArgs),
    /// Compaction events with trigger and context
    Compactions(CompactionsArgs),
    /// FTS5 full-text search across prompts, errors, tool inputs
    Search(SearchArgs),
    /// Compare metrics across config versions
    Configs(ConfigsArgs),
    /// System diagnostics
    Health(HealthArgs),
    /// Live event stream from today's JSONL (500ms poll)
    Tail(TailArgs),
    /// Side-by-side session comparison
    Diff(DiffArgs),
    /// Per-day aggregates with ASCII sparkline
    Trends(TrendsArgs),
    /// Performance outliers from tool_calls
    Slow(SlowArgs),
    /// Estimated token consumption
    Tokens(TokensArgs),
    /// Initialize the v4 SQLite database and schema marker.
    ///
    /// Creates ~/.claude/telemetry/sessions.db and writes the schema marker
    /// if either is absent.  Idempotent: safe to run on an already-initialized
    /// database.
    Init(InitArgs),
    /// Manually trigger ingestion
    Ingest(IngestArgs),
    /// Label the most recent config version
    Label(LabelArgs),
    /// Attach outcome label to a session
    Annotate(AnnotateArgs),
    /// Inspect failed_events.jsonl fallback
    Replay(ReplayArgs),
    /// Delete old SQLite rows and optionally JSONL archives
    Prune(PruneArgs),
    /// Export sessions as JSONL to stdout
    Export(ExportArgs),
    /// Drop all SQLite tables and re-ingest all JSONL (nuclear recovery)
    Rebuild(RebuildArgs),
    /// Safe SQLite snapshot via sqlite3 backup API
    Backup(BackupArgs),
    /// Arbitrary SQL query (read-only by default)
    Sql(SqlArgs),
    /// Append session metrics to Obsidian daily note
    #[command(name = "append-daily")]
    AppendDaily(AppendDailyArgs),
    /// Backfill from existing per-project JSONL logs and old SQLite databases
    #[command(name = "import-legacy")]
    ImportLegacy(ImportLegacyArgs),
    /// Derive observed file/pattern coverage from telemetry, keyed by agent_id
    Coverage(CoverageArgs),
    /// Phase 0 provenance metrics: re-exploration overlap and coverage-edge incidents
    Provenance(ProvenanceArgs),
    /// Non-blocking PreToolUse coverage gate (reads stdin, warns worker once per file)
    Gate(GateArgs),
}

// ---------------------------------------------------------------------------
// S-group: per-session / ad-hoc query commands
// ---------------------------------------------------------------------------

/// Args for `summary`
#[derive(Args, Debug)]
pub struct SummaryArgs {
    /// Number of days to show (default: 7)
    #[arg(long, default_value_t = 7)]
    pub days: u32,
}

/// Args for `sessions`
#[derive(Args, Debug)]
pub struct SessionsArgs {
    /// Filter by working directory (substring match)
    #[arg(long)]
    pub cwd: Option<String>,

    /// Filter by git branch name
    #[arg(long)]
    pub branch: Option<String>,

    /// Filter by annotation label
    #[arg(long)]
    pub label: Option<String>,

    /// Number of days to look back (default: 30)
    #[arg(long, default_value_t = 30)]
    pub days: u32,

    /// Show chain_id column
    #[arg(long)]
    pub chain: bool,
}

/// Args for `session`
#[derive(Args, Debug)]
pub struct SessionArgs {
    /// Session ID prefix (first 8 chars or more)
    pub id_prefix: String,
}

/// Args for `last`
#[derive(Args, Debug)]
pub struct LastArgs {}

/// Args for `chain`
#[derive(Args, Debug)]
pub struct ChainArgs {
    /// Session ID prefix to identify chain
    pub session_prefix: String,
}

/// Args for `tools`
#[derive(Args, Debug)]
pub struct ToolsArgs {
    /// Show stats across all config versions
    #[arg(long)]
    pub all_time: bool,
}

/// Args for `agents`
#[derive(Args, Debug)]
pub struct AgentsArgs {}

/// Args for `skills`
#[derive(Args, Debug)]
pub struct SkillsArgs {
    /// Optional session ID prefix to filter
    pub session_id: Option<String>,
}

/// Args for `failures`
#[derive(Args, Debug)]
pub struct FailuresArgs {
    /// Number of days to show (default: 7)
    #[arg(long, default_value_t = 7)]
    pub days: u32,
}

/// Args for `before-stop`
#[derive(Args, Debug)]
pub struct BeforeStopArgs {}

/// Args for `compactions`
#[derive(Args, Debug)]
pub struct CompactionsArgs {}

/// Args for `search`
#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Search query (FTS5 syntax supported)
    pub query: String,
}

/// Args for `configs`
#[derive(Args, Debug)]
pub struct ConfigsArgs {}

// ---------------------------------------------------------------------------
// S-group cont.: health, tail
// ---------------------------------------------------------------------------

/// Args for `health`
#[derive(Args, Debug)]
pub struct HealthArgs {
    /// Include chain statistics
    #[arg(long)]
    pub chain_stats: bool,
}

/// Args for `tail`
#[derive(Args, Debug)]
pub struct TailArgs {
    /// Filter by event type or tool name (substring match)
    #[arg(long, value_name = "EVENT|TOOL")]
    pub filter: Option<String>,
}

// ---------------------------------------------------------------------------
// M-group: multi-session / analytical commands
// ---------------------------------------------------------------------------

/// Args for `diff`
#[derive(Args, Debug)]
pub struct DiffArgs {
    /// First session ID prefix
    pub session_a: String,
    /// Second session ID prefix
    pub session_b: String,
}

/// Args for `trends`
#[derive(Args, Debug)]
pub struct TrendsArgs {
    /// Metric to trend (default: tool_calls)
    #[arg(long, value_enum, default_value_t = TrendsMetric::ToolCalls)]
    pub metric: TrendsMetric,

    /// Days to show (default: 14)
    #[arg(long, default_value_t = 14)]
    pub window: u32,
}

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum TrendsMetric {
    Sessions,
    #[default]
    ToolCalls,
    Failures,
    Prompts,
    Duration,
}

/// Args for `slow`
#[derive(Args, Debug)]
pub struct SlowArgs {
    /// Duration threshold in ms (default: 5000)
    #[arg(long, default_value_t = 5000)]
    pub threshold: u64,

    /// Filter by specific tool name
    #[arg(long)]
    pub tool: Option<String>,
}

/// Args for `tokens`
#[derive(Args, Debug)]
pub struct TokensArgs {
    /// Optional session ID prefix
    pub session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// L-group: write / admin / ingestion commands
// ---------------------------------------------------------------------------

/// Args for `init`
#[derive(Args, Debug, Default)]
pub struct InitArgs {}

/// Args for `ingest`
#[derive(Args, Debug)]
pub struct IngestArgs {
    /// Specific JSONL files to ingest (default: all unprocessed)
    #[arg(value_name = "FILE")]
    pub files: Vec<String>,

    /// Also ingest today's file
    #[arg(long)]
    pub include_today: bool,
}

/// Args for `label`
#[derive(Args, Debug)]
pub struct LabelArgs {
    /// Human-readable description for this config version
    pub description: String,
}

/// Args for `annotate`
#[derive(Args, Debug)]
pub struct AnnotateArgs {
    /// Session ID prefix
    pub session_prefix: String,

    /// Label to attach (e.g., 'success', 'failed', 'interesting')
    pub label: String,

    /// Optional notes
    pub notes: Option<String>,
}

/// Args for `replay`
#[derive(Args, Debug)]
pub struct ReplayArgs {}

/// Args for `prune`
#[derive(Args, Debug)]
pub struct PruneArgs {
    /// Delete data older than this many days
    pub days: u32,

    /// Also delete JSONL archives (irreversible)
    #[arg(long)]
    pub archive: bool,

    /// Skip confirmation prompts
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// Args for `export`
#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Start date (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub from: Option<String>,

    /// End date (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub to: Option<String>,
}

/// Args for `rebuild`
#[derive(Args, Debug)]
pub struct RebuildArgs {
    /// Only re-ingest files on or after this date (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// Skip confirmation prompt
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// Args for `backup`
#[derive(Args, Debug)]
pub struct BackupArgs {
    /// Destination path for backup file
    pub path: String,
}

/// Args for `sql`
#[derive(Args, Debug)]
pub struct SqlArgs {
    /// SQL query string
    pub query: String,

    /// Allow write operations (INSERT, UPDATE, DELETE, etc.)
    #[arg(long)]
    pub write: bool,
}

/// Args for `append-daily`
#[derive(Args, Debug)]
pub struct AppendDailyArgs {
    /// Vault path (default: ~/Sync/Obsidian/Second Brain)
    #[arg(long, value_name = "PATH")]
    pub vault: Option<String>,
}

/// Args for `import-legacy`
#[derive(Args, Debug)]
pub struct ImportLegacyArgs {}

/// Args for `coverage`
#[derive(Args, Debug)]
pub struct CoverageArgs {
    /// Agent ID to derive coverage for
    pub agent_id: String,

    /// Write coverage JSON to ~/.claude/telemetry/coverage/<agent_id>.json
    #[arg(long)]
    pub write: bool,
}

/// Args for `provenance`
#[derive(Args, Debug)]
pub struct ProvenanceArgs {
    /// Number of trailing calendar days of logs to include (default: 7)
    #[arg(long, default_value_t = 7)]
    pub days: u32,
}

/// Args for `gate`
///
/// No CLI arguments — the hook payload is read from stdin.
#[derive(Args, Debug)]
pub struct GateArgs {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // --- global flag tests ---

    #[test]
    fn json_flag_overrides_format() {
        let cli = Cli::try_parse_from(["hooked", "--json", "summary"]).unwrap();
        assert!(matches!(cli.effective_format(), OutputFormat::Json));
    }

    #[test]
    fn format_csv_works() {
        let cli = Cli::try_parse_from(["hooked", "--format", "csv", "summary"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Csv));
    }

    #[test]
    fn format_markdown_works() {
        let cli = Cli::try_parse_from(["hooked", "--format", "markdown", "summary"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Markdown));
    }

    #[test]
    fn format_json_works() {
        let cli = Cli::try_parse_from(["hooked", "--format", "json", "summary"]).unwrap();
        assert!(matches!(cli.effective_format(), OutputFormat::Json));
    }

    #[test]
    fn default_format_is_table() {
        let cli = Cli::try_parse_from(["hooked", "summary"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Table));
    }

    // --- S-group ---

    #[test]
    fn parses_summary_default() {
        let cli = Cli::try_parse_from(["hooked", "summary"]).unwrap();
        match cli.command {
            Command::Summary(a) => assert_eq!(a.days, 7),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_summary_with_days() {
        let cli = Cli::try_parse_from(["hooked", "summary", "--days", "30"]).unwrap();
        match cli.command {
            Command::Summary(a) => assert_eq!(a.days, 30),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_sessions_default() {
        let cli = Cli::try_parse_from(["hooked", "sessions"]).unwrap();
        match cli.command {
            Command::Sessions(a) => {
                assert_eq!(a.days, 30);
                assert!(a.cwd.is_none());
                assert!(!a.chain);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_sessions_with_filters() {
        let cli = Cli::try_parse_from([
            "hooked",
            "sessions",
            "--cwd",
            "/home/user",
            "--branch",
            "main",
            "--label",
            "success",
            "--chain",
        ])
        .unwrap();
        match cli.command {
            Command::Sessions(a) => {
                assert_eq!(a.cwd.as_deref(), Some("/home/user"));
                assert_eq!(a.branch.as_deref(), Some("main"));
                assert_eq!(a.label.as_deref(), Some("success"));
                assert!(a.chain);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_session_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "session", "abc-123"]).unwrap();
        match cli.command {
            Command::Session(a) => assert_eq!(a.id_prefix, "abc-123"),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_last_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "last"]).unwrap();
        assert!(matches!(cli.command, Command::Last(_)));
    }

    #[test]
    fn parses_chain_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "chain", "abc-123"]).unwrap();
        match cli.command {
            Command::Chain(a) => assert_eq!(a.session_prefix, "abc-123"),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_tools_default() {
        let cli = Cli::try_parse_from(["hooked", "tools"]).unwrap();
        match cli.command {
            Command::Tools(a) => assert!(!a.all_time),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_tools_all_time() {
        let cli = Cli::try_parse_from(["hooked", "tools", "--all-time"]).unwrap();
        match cli.command {
            Command::Tools(a) => assert!(a.all_time),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_agents_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "agents"]).unwrap();
        assert!(matches!(cli.command, Command::Agents(_)));
    }

    #[test]
    fn parses_skills_no_session() {
        let cli = Cli::try_parse_from(["hooked", "skills"]).unwrap();
        match cli.command {
            Command::Skills(a) => assert!(a.session_id.is_none()),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_skills_with_session() {
        let cli = Cli::try_parse_from(["hooked", "skills", "abc123"]).unwrap();
        match cli.command {
            Command::Skills(a) => assert_eq!(a.session_id.as_deref(), Some("abc123")),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_failures_default() {
        let cli = Cli::try_parse_from(["hooked", "failures"]).unwrap();
        match cli.command {
            Command::Failures(a) => assert_eq!(a.days, 7),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_before_stop_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "before-stop"]).unwrap();
        assert!(matches!(cli.command, Command::BeforeStop(_)));
    }

    #[test]
    fn parses_compactions_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "compactions"]).unwrap();
        assert!(matches!(cli.command, Command::Compactions(_)));
    }

    #[test]
    fn parses_search_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "search", "my query"]).unwrap();
        match cli.command {
            Command::Search(a) => assert_eq!(a.query, "my query"),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_configs_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "configs"]).unwrap();
        assert!(matches!(cli.command, Command::Configs(_)));
    }

    #[test]
    fn parses_health_default() {
        let cli = Cli::try_parse_from(["hooked", "health"]).unwrap();
        match cli.command {
            Command::Health(a) => assert!(!a.chain_stats),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_health_chain_stats() {
        let cli = Cli::try_parse_from(["hooked", "health", "--chain-stats"]).unwrap();
        match cli.command {
            Command::Health(a) => assert!(a.chain_stats),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_tail_default() {
        let cli = Cli::try_parse_from(["hooked", "tail"]).unwrap();
        match cli.command {
            Command::Tail(a) => assert!(a.filter.is_none()),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_tail_with_filter() {
        let cli = Cli::try_parse_from(["hooked", "tail", "--filter", "Read"]).unwrap();
        match cli.command {
            Command::Tail(a) => assert_eq!(a.filter.as_deref(), Some("Read")),
            _ => panic!("wrong subcommand"),
        }
    }

    // --- M-group ---

    #[test]
    fn parses_diff_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "diff", "abc", "def"]).unwrap();
        match cli.command {
            Command::Diff(a) => {
                assert_eq!(a.session_a, "abc");
                assert_eq!(a.session_b, "def");
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_trends_default() {
        let cli = Cli::try_parse_from(["hooked", "trends"]).unwrap();
        match cli.command {
            Command::Trends(a) => {
                assert!(matches!(a.metric, TrendsMetric::ToolCalls));
                assert_eq!(a.window, 14);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_trends_with_options() {
        let cli =
            Cli::try_parse_from(["hooked", "trends", "--metric", "failures", "--window", "7"])
                .unwrap();
        match cli.command {
            Command::Trends(a) => {
                assert!(matches!(a.metric, TrendsMetric::Failures));
                assert_eq!(a.window, 7);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_slow_default() {
        let cli = Cli::try_parse_from(["hooked", "slow"]).unwrap();
        match cli.command {
            Command::Slow(a) => {
                assert_eq!(a.threshold, 5000);
                assert!(a.tool.is_none());
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_slow_with_options() {
        let cli = Cli::try_parse_from(["hooked", "slow", "--threshold", "2000", "--tool", "Read"])
            .unwrap();
        match cli.command {
            Command::Slow(a) => {
                assert_eq!(a.threshold, 2000);
                assert_eq!(a.tool.as_deref(), Some("Read"));
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_tokens_no_session() {
        let cli = Cli::try_parse_from(["hooked", "tokens"]).unwrap();
        match cli.command {
            Command::Tokens(a) => assert!(a.session_id.is_none()),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_tokens_with_session() {
        let cli = Cli::try_parse_from(["hooked", "tokens", "abc123"]).unwrap();
        match cli.command {
            Command::Tokens(a) => assert_eq!(a.session_id.as_deref(), Some("abc123")),
            _ => panic!("wrong subcommand"),
        }
    }

    // --- L-group ---

    #[test]
    fn parses_ingest_no_files() {
        let cli = Cli::try_parse_from(["hooked", "ingest"]).unwrap();
        match cli.command {
            Command::Ingest(a) => {
                assert!(a.files.is_empty());
                assert!(!a.include_today);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_ingest_with_files_and_flag() {
        let cli =
            Cli::try_parse_from(["hooked", "ingest", "a.jsonl", "b.jsonl", "--include-today"])
                .unwrap();
        match cli.command {
            Command::Ingest(a) => {
                assert_eq!(a.files, vec!["a.jsonl", "b.jsonl"]);
                assert!(a.include_today);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_label_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "label", "my experiment"]).unwrap();
        match cli.command {
            Command::Label(a) => assert_eq!(a.description, "my experiment"),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_annotate_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "annotate", "abc123", "success", "worked great"])
            .unwrap();
        match cli.command {
            Command::Annotate(a) => {
                assert_eq!(a.session_prefix, "abc123");
                assert_eq!(a.label, "success");
                assert_eq!(a.notes.as_deref(), Some("worked great"));
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_annotate_no_notes() {
        let cli = Cli::try_parse_from(["hooked", "annotate", "abc123", "failed"]).unwrap();
        match cli.command {
            Command::Annotate(a) => {
                assert_eq!(a.session_prefix, "abc123");
                assert_eq!(a.label, "failed");
                assert!(a.notes.is_none());
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_replay_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "replay"]).unwrap();
        assert!(matches!(cli.command, Command::Replay(_)));
    }

    #[test]
    fn parses_prune_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "prune", "90"]).unwrap();
        match cli.command {
            Command::Prune(a) => {
                assert_eq!(a.days, 90);
                assert!(!a.archive);
                assert!(!a.yes);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_prune_with_flags() {
        let cli = Cli::try_parse_from(["hooked", "prune", "30", "--archive", "--yes"]).unwrap();
        match cli.command {
            Command::Prune(a) => {
                assert_eq!(a.days, 30);
                assert!(a.archive);
                assert!(a.yes);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_export_default() {
        let cli = Cli::try_parse_from(["hooked", "export"]).unwrap();
        match cli.command {
            Command::Export(a) => {
                assert!(a.from.is_none());
                assert!(a.to.is_none());
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_export_with_dates() {
        let cli = Cli::try_parse_from([
            "hooked",
            "export",
            "--from",
            "2024-01-01",
            "--to",
            "2024-12-31",
        ])
        .unwrap();
        match cli.command {
            Command::Export(a) => {
                assert_eq!(a.from.as_deref(), Some("2024-01-01"));
                assert_eq!(a.to.as_deref(), Some("2024-12-31"));
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_rebuild_default() {
        let cli = Cli::try_parse_from(["hooked", "rebuild"]).unwrap();
        match cli.command {
            Command::Rebuild(a) => {
                assert!(a.since.is_none());
                assert!(!a.yes);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_backup_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "backup", "/tmp/backup.db"]).unwrap();
        match cli.command {
            Command::Backup(a) => assert_eq!(a.path, "/tmp/backup.db"),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_sql_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "sql", "SELECT COUNT(*) FROM sessions"]).unwrap();
        match cli.command {
            Command::Sql(a) => {
                assert_eq!(a.query, "SELECT COUNT(*) FROM sessions");
                assert!(!a.write);
            }
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_sql_write_flag() {
        let cli = Cli::try_parse_from(["hooked", "sql", "DELETE FROM events", "--write"]).unwrap();
        match cli.command {
            Command::Sql(a) => assert!(a.write),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_append_daily_default() {
        let cli = Cli::try_parse_from(["hooked", "append-daily"]).unwrap();
        match cli.command {
            Command::AppendDaily(a) => assert!(a.vault.is_none()),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_append_daily_with_vault() {
        let cli =
            Cli::try_parse_from(["hooked", "append-daily", "--vault", "/path/to/vault"]).unwrap();
        match cli.command {
            Command::AppendDaily(a) => assert_eq!(a.vault.as_deref(), Some("/path/to/vault")),
            _ => panic!("wrong subcommand"),
        }
    }

    #[test]
    fn parses_import_legacy_subcommand() {
        let cli = Cli::try_parse_from(["hooked", "import-legacy"]).unwrap();
        assert!(matches!(cli.command, Command::ImportLegacy(_)));
    }
}
