#!/usr/bin/env bash
# Quality gate for the hooked project.
#
# Runs format check, clippy, tests, and a security audit.
# Exits non-zero on any failure, suitable for pre-commit hooks
# and CI invocations.

set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> cargo fmt --check"
cargo fmt --check

echo "==> cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> cargo test -- --test-threads=1"
cargo test -- --test-threads=1

echo "==> cargo audit"
cargo audit

echo "==> all checks passed"
