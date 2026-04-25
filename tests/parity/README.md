# Parity Fixture Corpus

## Purpose

These JSONL fixtures drive the **Phase 2 parity harness** (T2.2): both the Python
reference implementation (`ingest.py`) and the Rust `hooked` binary ingest each
fixture into separate temporary SQLite databases. The resulting databases are then
diffed table-by-table to verify byte-for-byte parity in the `events`, `sessions`,
and `tool_calls` tables.

The corpus is intentionally minimal (5–20 lines per file) but covers every
behavioural edge case identified during the codex review.

## Running the parity harness

### Rust diff unit tests (no Python required)

The diff logic lives in `src/parity.rs` and its tests run entirely inside
`cargo test`:

```
cargo test parity
```

This exercises all diff semantics: missing rows, extra rows, field-level diffs,
chain topology isomorphism, timestamp normalization, and JSON structural equality.

### Full end-to-end harness (requires Python)

The shell script `tests/parity/run_parity.sh` runs both Python and Rust ingest
against every fixture and diffs the resulting databases:

```bash
# First time: build release binaries
cd <repo root>
cargo build --release --bin ingest_one --bin parity

# Then run (script must be executable: chmod +x tests/parity/run_parity.sh)
tests/parity/run_parity.sh

# Or build and run in one step:
tests/parity/run_parity.sh --build

# Verbose mode (prints full diff report even for OK fixtures):
tests/parity/run_parity.sh --verbose
```

Requirements:
- `python3` in PATH
- `~/.claude/telemetry/ingest.py` present
- `cargo` in PATH (for `--build` flag only; otherwise binaries must be pre-built)

Exit codes:
- `0` — all fixtures passed parity
- `1` — one or more fixtures diverged

Diff reports are written to `target/parity-report/<fixture>.diff.txt`.

## Fixtures

| File | What it exercises |
|---|---|
| `happy_path.jsonl` | One complete session: SessionStart → UserPromptSubmit → PreToolUse → PostToolUse → SessionEnd |
| `compaction_chain.jsonl` | PreCompact event followed by a new SessionStart with `source="compact"` on the same session_id |
| `failed_tools.jsonl` | PostToolUseFailure events, including `is_interrupt=true` |
| `slash_commands.jsonl` | UserPromptSubmit events whose prompt starts with `/` — exercises `is_slash_command` flag |
| `skills_mixed_quotes.jsonl` | tool_input paths referencing `.claude/skills/` with no quotes, single quotes, and double quotes |
| `subagent_chains.jsonl` | Multiple SubagentStart events with distinct `agent_id` / `agent_type` values |
| `missing_fields.jsonl` | Envelopes where optional fields (`model`, `cwd`, `tool_response`) are absent |
| `malformed_lines.jsonl` | Mix of valid lines with broken JSON, empty lines, and plain-text lines; parser must skip and continue |
| `gzip_input.jsonl` | Plain-text copy of happy_path used as gzip source; the T2.2 harness gzips this at runtime to produce `gzip_input.jsonl.gz` and verifies the gzip code path |
| `multi_session.jsonl` | Three distinct `session_id`s interleaved in a single file |
| `nested_tool_input.jsonl` | tool_input is a deeply nested object (arrays, sub-objects, multi-level keys) |
| `subsecond_timestamps.jsonl` | Timestamps with microsecond precision; duplicate millisecond timestamps ordered by `_raw_index` |
| `duplicate_event_hash.jsonl` | Two envelopes with identical (session_id, event_type, timestamp, tool_use_id) — second must be dropped by `INSERT OR IGNORE` |
| `float_edge_cases.jsonl` | tool_input containing `-0.0`, `1e100`, `1.23456789e-10`, and mixed int/float arrays |
| `non_ascii_payload.jsonl` | prompt and tool_input containing accented Latin (`é`), CJK (`日本語`), and emoji (`🔥`) |
| `precomputed_event_hash.jsonl` | Envelopes with the `h` field already set (simulating capture.sh output); ingestion must use the pre-supplied hash, not recompute it |

## Gzip fixture note

The worker profile that created these fixtures cannot run shell commands, so
`gzip_input.jsonl.gz` is **not** pre-built. The T2.2 harness must create it at
runtime:

```rust
// In parity.rs setup:
let plain = fs::read("tests/parity/fixtures/gzip_input.jsonl")?;
let gz_path = tmp_dir.path().join("gzip_input.jsonl.gz");
let mut gz = GzEncoder::new(fs::File::create(&gz_path)?, Compression::default());
gz.write_all(&plain)?;
gz.finish()?;
```

## Adding new fixtures

1. Create a `.jsonl` file in this directory.
2. Each line must be a valid JSON envelope: `{"v":1,"ts":"<ISO-8601>","p":{...}}`.
3. Every payload must include `hook_event_name` and `session_id`.
4. Optional: add `"h":"<16-hex-chars>"` to test pre-computed hash passthrough.
5. Document the new fixture with a one-line entry in the table above.
6. The T2.2 harness auto-discovers all `*.jsonl` files in this directory.
