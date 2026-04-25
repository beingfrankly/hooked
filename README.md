# hooked

Rust port of the Claude Code telemetry ingest + query CLI (`ccq` replacement).

## Status

- Stable for read-only queries against the live `sessions.db`.
- Ingest path verified at byte-for-byte parity with the Python implementation via `tests/parity/run_parity.sh`.
- All 30 subcommands are implemented and match the Python `ccq` interface.

## Requirements

- Rust 1.89 or newer (the project pins `1.95.0` via `rust-toolchain.toml`)
- macOS or Linux (CI tested on both)

## Install

```bash
git clone <repo-url>
cd hooked
cargo install --path .
```

This installs a `hooked` binary into `~/.cargo/bin`. Make sure that directory is on `PATH`.

## Usage

```bash
hooked --help
hooked summary
hooked sessions --limit 10
hooked session <session-id>
hooked tail
```

All 30 subcommands match the Python `ccq` interface.

### Aliasing as `ccq` (optional)

Add to your shell profile:

```bash
alias ccq=hooked
```

This lets existing scripts that call `ccq` keep working unchanged.

## launchd integration

To replace the Python `nightly-ingest.sh` with the Rust binary, install the following plist at
`~/Library/LaunchAgents/com.user.hooked.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.user.hooked</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/<your-user>/.cargo/bin/hooked</string>
    <string>ingest</string>
  </array>
  <key>StartInterval</key>
  <integer>3600</integer>
  <key>StandardOutPath</key>
  <string>/tmp/hooked.out.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/hooked.err.log</string>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
```

Replace `<your-user>` with your macOS username.  `StartInterval` of `3600` runs ingest once per hour,
matching the cadence of the Python job.

To install:

```bash
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/com.user.hooked.plist
```

To check status:

```bash
launchctl list | grep hooked
```

## Subcommand index

| Subcommand | Description |
|---|---|
| `summary` | Daily overview table (default: last 7 days) |
| `sessions` | Recent sessions with filters |
| `session` | Full event timeline for a session (by ID prefix) |
| `last` | Full event timeline for the most recent session |
| `chain` | All sessions in a lineage chain |
| `tools` | Tool usage stats (default: current config version) |
| `agents` | Subagent performance stats |
| `skills` | Skill usage by frequency |
| `failures` | Recent tool failures |
| `before-stop` | Pattern analysis of events before Stop |
| `compactions` | Compaction events with trigger and context |
| `search` | FTS5 full-text search across prompts, errors, tool inputs |
| `configs` | Compare metrics across config versions |
| `health` | System diagnostics |
| `tail` | Live event stream from today's JSONL (500ms poll) |
| `diff` | Side-by-side session comparison |
| `trends` | Per-day aggregates with ASCII sparkline |
| `slow` | Performance outliers from tool_calls |
| `tokens` | Estimated token consumption |
| `ingest` | Manually trigger ingestion |
| `label` | Label the most recent config version |
| `annotate` | Attach outcome label to a session |
| `replay` | Inspect failed_events.jsonl fallback |
| `prune` | Delete old SQLite rows and optionally JSONL archives |
| `export` | Export sessions as JSONL to stdout |
| `rebuild` | Drop all SQLite tables and re-ingest all JSONL (nuclear recovery) |
| `backup` | Safe SQLite snapshot via sqlite3 backup API |
| `sql` | Arbitrary SQL query (read-only by default) |
| `append-daily` | Append session metrics to Obsidian daily note |
| `import-legacy` | Backfill from existing per-project JSONL logs and old SQLite databases |

## Configuration

- Telemetry directory: `~/.claude/telemetry/`
- DB path: `~/.claude/telemetry/sessions.db`
- JSONL log dir: `~/.claude/telemetry/logs/`
- Archive dir: `~/.claude/telemetry/logs/archive/`
- Schema marker: `~/.claude/telemetry/.schema_v4`

These paths are not configurable; they mirror the Python implementation exactly.

## Output formats

`--format` flag accepts: `table` (default), `json`, `csv`, `markdown`.
Shortcut: `--json` is equivalent to `--format json`.

Both flags are global and work with any subcommand that produces tabular output.

## Architecture

The crate is structured as a library (`src/lib.rs`) with a thin `src/main.rs` entry point.
The library exposes modules for the CLI definition (`cli`), all subcommand implementations
(`cmd`), the database handle (`dbh`), path constants (`paths`), the rendering layer
(`render`), schema management (`schema`), and the ingest pipeline (`envelope`, `enrich`,
`ingest`).

The ingest pipeline mirrors the Python `ingest.py` three-stage approach: (1) `envelope`
parses each JSONL line into a typed `Envelope` struct, handling both plain and gzip-compressed
files; (2) `enrich` derives computed columns (session metadata, tool names, byte counts, hashes)
that are stored alongside the raw payload; (3) `ingest` drives the pipeline over a set of
JSONL files, deduplicates via the event hash, and writes rows into the SQLite schema defined
by `schema`.

Parity tests (`tests/parity/`) run both the Python reference implementation and the Rust
binary over shared JSONL fixtures and diff the resulting SQLite databases.  The parity shell
script exits non-zero on any divergence, giving a byte-level guarantee that the two
implementations agree.

## Development

The standard quality gate combines formatting, linting, tests, and a security audit:

```bash
./scripts/check.sh
```

This is the same set of checks that pre-commit runs on push. Pre-commit runs the fast subset (fmt + clippy) on every commit.

Or run each step individually:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test -- --test-threads=1
cargo audit
```

Tests use `--test-threads=1` because some tests mutate `HOME` for fixture isolation.

`cargo audit` scans `Cargo.lock` against the RustSec advisory database. Install once with:

```bash
cargo install --locked cargo-audit
```

Run before each release to confirm no new vulnerabilities have been disclosed in transitive dependencies.

## First-time setup

After cloning:

```bash
# Install Rust toolchain (rustup reads rust-toolchain.toml).
rustup show

# Install Python (asdf reads .tool-versions).
asdf install

# Install pre-commit hooks.
pip install pre-commit
pre-commit install
pre-commit install --hook-type pre-push

# (Optional, one-time) install the security audit tool.
cargo install --locked cargo-audit
```

Pre-commit will run `cargo fmt` + `cargo clippy` on every commit, and `cargo test` + `cargo audit` on every push. To skip hooks on a one-off (use sparingly): `git commit --no-verify`.

## Parity testing

The `tests/parity/run_parity.sh` script ingests every fixture under `tests/parity/fixtures/`
with both the Python `ingest.py` and the Rust binary, then diffs the resulting SQLite DBs.
It exits non-zero on any divergence.

```bash
./tests/parity/run_parity.sh
```

Requires Python 3 and the existing `~/.claude/telemetry/ingest.py`.

## License

TBD.
