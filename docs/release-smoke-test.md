# Release smoke test

Run after `cargo build --release` to verify the binary works against the real
`~/.claude/telemetry/sessions.db` without mutating it.

## Prerequisites

- `cargo build --release` succeeded
- The binary lives at `target/release/hooked`
- Real DB at `~/.claude/telemetry/sessions.db` (will be read, NOT modified)

## Procedure

For each command below: confirm it produces output (or an empty result) without
panicking and without modifying the DB.

```bash
./target/release/hooked health
./target/release/hooked summary
./target/release/hooked sessions --limit 5
./target/release/hooked tools
./target/release/hooked agents
./target/release/hooked skills
./target/release/hooked failures
./target/release/hooked --json summary
./target/release/hooked --format csv sessions --limit 3
./target/release/hooked --format markdown summary
```

If `health` reports anomalies (missing schema marker, lock held by another process,
archive count of 0 when archives should exist), STOP and investigate before
continuing.

## Expected

- All commands return exit 0
- No crashes or panics
- Output is either valid data or "(no results)" if the DB is empty

## After smoke

Proceed to T5.2 (full-DB parity test against `sessions.db`).
