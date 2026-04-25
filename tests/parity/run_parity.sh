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

set -euo pipefail

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

BUILD=0
VERBOSE=0

for arg in "$@"; do
  case "$arg" in
    --build)   BUILD=1 ;;
    --verbose) VERBOSE=1 ;;
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
FIXTURES_DIR="$REPO_ROOT/tests/parity/fixtures"
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
# We call the module's public API directly rather than the CLI to avoid needing
# to know the exact CLI flags (which may change between versions).
# ---------------------------------------------------------------------------

python_ingest() {
  local fixture="$1"
  local db_path="$2"

  python3 - "$fixture" "$db_path" <<'PY'
import sys
import os

fixture  = sys.argv[1]
db_path  = sys.argv[2]

sys.path.insert(0, os.path.expanduser('~/.claude/telemetry'))
import ingest

# Initialise schema.
conn = ingest._init_db(db_path)

# Find the ingest-single-file callable (name changed between versions).
ingest_fn = (
    getattr(ingest, 'ingest_file', None)
    or getattr(ingest, '_ingest_file', None)
)
if ingest_fn is None:
    print("ERROR: cannot find ingest_file/_ingest_file in ingest.py", file=sys.stderr)
    sys.exit(2)

# Call with (conn, path) or (path, conn) — try both signatures.
try:
    ingest_fn(conn, fixture)
except TypeError:
    ingest_fn(fixture, conn)

conn.commit()
conn.close()
PY
}

# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

mkdir -p "$REPORT_DIR"

TOTAL=0
FAIL=0
SKIP=0
PASS=0

# Iterate over plain JSONL and pre-gzipped fixtures.
shopt -s nullglob
for fixture in "$FIXTURES_DIR"/*.jsonl "$FIXTURES_DIR"/*.jsonl.gz; do
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
shopt -u nullglob

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "=============================="
echo "Parity harness summary"
echo "  Total  : $TOTAL"
echo "  Passed : $PASS"
echo "  Failed : $FAIL"
echo "  Skipped: $SKIP"
echo "=============================="

if [ "$FAIL" -gt 0 ]; then
  echo "PARITY FAILED: $FAIL fixture(s) diverged"
  exit 1
fi

echo "PARITY OK"
exit 0
