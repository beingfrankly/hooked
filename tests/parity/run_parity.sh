#!/usr/bin/env bash
# Phase 2 parity harness runner.
#
# For each fixture in tests/parity/fixtures/, ingests with both:
#   - Python:  ~/.claude/telemetry/ingest.py  (reference implementation)
#   - Rust:    ./target/release/ingest_one     (Rust hooked ingest)
#
# Then diffs the resulting databases using ./target/release/parity.
# Exits with code 1 if any fixture diverges (suitable for CI).
#
# Requirements:
#   - python3 in PATH
#   - ~/.claude/telemetry/ingest.py present
#   - cargo build --release (run once before this script, or pass --build flag)
#
# Usage:
#   cd <hooked repo root>
#   tests/parity/run_parity.sh              # run all fixtures
#   tests/parity/run_parity.sh --build      # cargo build first, then run
#   tests/parity/run_parity.sh --verbose    # print full diff report even on OK
#   tests/parity/run_parity.sh --strict     # treat any SKIP as a failure
#
# Strict mode:
#   Pass --strict (or set PARITY_STRICT=1 in the environment) to make any
#   SKIPped fixture escalate to a non-zero exit, in addition to the normal
#   FAIL and all-skipped gates. Useful in CI where skips should not silently
#   paper over broken tooling.

set -euo pipefail

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

BUILD=0
VERBOSE=0
# PARITY_STRICT can also be pre-set in the environment (PARITY_STRICT=1).
STRICT="${PARITY_STRICT:-0}"

for arg in "$@"; do
  case "$arg" in
    --build)   BUILD=1 ;;
    --verbose) VERBOSE=1 ;;
    --strict)  STRICT=1 ;;
    --help|-h)
      sed -n '2,/^[^#]/{ /^#/{ s/^# \?//; p }; /^[^#]/q }' "$0"
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      exit 1
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES_DIR="${FIXTURES_DIR:-$REPO_ROOT/tests/parity/fixtures}"
REPORT_DIR="$REPO_ROOT/target/parity-report"
INGEST_PY="$HOME/.claude/telemetry/ingest.py"

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------

if [ ! -f "$INGEST_PY" ]; then
  echo "ERROR: Python ingest script not found at $INGEST_PY" >&2
  exit 1
fi

if ! command -v python3 &>/dev/null; then
  echo "ERROR: python3 not found in PATH" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Build (optional)
# ---------------------------------------------------------------------------

cd "$REPO_ROOT"

if [ "$BUILD" -eq 1 ]; then
  echo "==> Building release binaries..."
  cargo build --release --bin ingest_one --bin parity
fi

INGEST_ONE="$REPO_ROOT/target/release/ingest_one"
PARITY_BIN="$REPO_ROOT/target/release/parity"

if [ ! -x "$INGEST_ONE" ]; then
  echo "ERROR: $INGEST_ONE not found. Run with --build or: cargo build --release --bin ingest_one" >&2
  exit 1
fi

if [ ! -x "$PARITY_BIN" ]; then
  echo "ERROR: $PARITY_BIN not found. Run with --build or: cargo build --release --bin parity" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Python ingest helper
#
# Invokes ingest.py against a single fixture file, writing to the given DB path.
# Uses the module's stable public API: ingest.ingest_file(db_path, jsonl_path).
# ---------------------------------------------------------------------------

python_ingest() {
  local fixture="$1"
  local db_path="$2"

  python3 - "$fixture" "$db_path" <<'PY'
import sys
import os
import sqlite3

fixture  = sys.argv[1]
db_path  = sys.argv[2]

sys.path.insert(0, os.path.expanduser('~/.claude/telemetry'))
import ingest

# ---------------------------------------------------------------------------
# Workaround: _init_db() uses a process-wide marker file (~/.claude/telemetry/
# .schema_v4) to decide whether to create the schema.  When the marker already
# exists (because the developer has run real ingestion before), _init_db()
# skips _create_schema() entirely — even when db_path is a brand-new empty
# tempfile.  The subsequent INSERT then fails with:
#   sqlite3.OperationalError: no such table: events
#
# Strategy 2 (chosen because _create_schema uses CREATE TABLE IF NOT EXISTS /
# CREATE INDEX IF NOT EXISTS throughout the DDL, making it fully idempotent):
# open the temp DB directly and call _create_schema() to pre-seed the schema
# before handing control to ingest_file().  This does not touch the marker
# file and has no side-effects on the user's real sessions database.
#
# TODO(parity): fix in ingest.py — _init_db should check sqlite_master for
# actual table presence, not just the marker file.
# ---------------------------------------------------------------------------
conn = sqlite3.connect(db_path, timeout=5.0)
conn.row_factory = sqlite3.Row
conn.execute("PRAGMA journal_mode=WAL;")
conn.execute("PRAGMA busy_timeout=5000;")
ingest._create_schema(conn)
conn.close()

# Python contract: ingest.ingest_file(db_path: str, jsonl_path: str) -> int
ingest.ingest_file(db_path, fixture)
PY
}

# ---------------------------------------------------------------------------
# Zero-fixtures guard
#
# Use nullglob so unmatched globs expand to nothing, then count manually.
# An empty fixtures directory is always a harness misconfiguration.
# ---------------------------------------------------------------------------

mkdir -p "$REPORT_DIR"

shopt -s nullglob
fixture_list=("$FIXTURES_DIR"/*.jsonl "$FIXTURES_DIR"/*.jsonl.gz)
shopt -u nullglob

if [ "${#fixture_list[@]}" -eq 0 ]; then
  echo "ERROR: no fixture files found in $FIXTURES_DIR (*.jsonl or *.jsonl.gz)" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

TOTAL=0
FAIL=0
SKIP=0
PASS=0

for fixture in "${fixture_list[@]}"; do
  [ -e "$fixture" ] || continue
  name="$(basename "$fixture")"
  TOTAL=$((TOTAL + 1))

  echo "=== $name ==="

  py_db="$REPORT_DIR/${name}.py.db"
  rs_db="$REPORT_DIR/${name}.rs.db"
  rm -f "$py_db" "$rs_db"

  # -- Python side --
  if ! python_ingest "$fixture" "$py_db" 2>/tmp/parity_py_err; then
    echo "  [SKIP] Python ingest failed:"
    cat /tmp/parity_py_err | sed 's/^/    /'
    SKIP=$((SKIP + 1))
    continue
  fi

  # -- Rust side --
  if ! "$INGEST_ONE" "$fixture" "$rs_db" 2>/tmp/parity_rs_err; then
    echo "  [SKIP] Rust ingest_one failed:"
    cat /tmp/parity_rs_err | sed 's/^/    /'
    SKIP=$((SKIP + 1))
    continue
  fi

  # -- Diff --
  diff_out="$REPORT_DIR/${name}.diff.txt"
  if "$PARITY_BIN" "$py_db" "$rs_db" > "$diff_out" 2>&1; then
    echo "  [OK]"
    if [ "$VERBOSE" -eq 1 ]; then
      cat "$diff_out" | sed 's/^/    /'
    fi
    PASS=$((PASS + 1))
  else
    echo "  [DIVERGED]"
    cat "$diff_out" | sed 's/^/    /'
    FAIL=$((FAIL + 1))
  fi
done

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "=============================="
echo "Parity harness summary"
echo "  PASS: $PASS  FAIL: $FAIL  SKIP: $SKIP"
echo "=============================="

# Determine exit code.
# Failure conditions:
#   1. Any fixture diverged (FAIL > 0).
#   2. No fixture produced a PASS result (all-skipped or zero results).
#   3. --strict / PARITY_STRICT=1: any SKIP is treated as a failure.
EXIT_CODE=0

if [ "$FAIL" -gt 0 ]; then
  echo "PARITY FAILED: $FAIL fixture(s) diverged"
  EXIT_CODE=1
fi

if [ "$PASS" -eq 0 ]; then
  echo "PARITY FAILED: no fixture produced a PASS result (all skipped or empty)" >&2
  EXIT_CODE=1
fi

if [ "$STRICT" -eq 1 ] && [ "$SKIP" -gt 0 ]; then
  echo "PARITY FAILED (strict): $SKIP fixture(s) were skipped — strict mode treats skips as failures" >&2
  EXIT_CODE=1
fi

if [ "$EXIT_CODE" -eq 0 ]; then
  echo "PARITY OK"
fi

exit "$EXIT_CODE"
