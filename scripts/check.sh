#!/usr/bin/env bash
# scripts/check.sh — quality gate before commit.
#
# Runs:
#   - cargo fmt --check
#   - cargo clippy --all -- -D warnings
#   - cargo test --workspace --no-fail-fast
#   - cargo audit (if installed)
#   - cargo deny check (if installed)
#
# Exit non-zero on any failure. Designed to be safe to run in a
# pre-push hook.

set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

fail=0

step() { echo; echo "==> $*"; }

step "cargo fmt --check"
if ! cargo fmt --all -- --check; then
  echo "    FAILED — run 'cargo fmt --all' and re-commit"
  fail=1
fi

step "cargo clippy --workspace -- -D warnings"
if ! cargo clippy --workspace --all-targets -- -D warnings; then
  echo "    FAILED — fix lints"
  fail=1
fi

step "cargo test --workspace --no-fail-fast"
if ! cargo test --workspace --no-fail-fast; then
  echo "    FAILED — fix tests"
  fail=1
fi

if command -v cargo-audit >/dev/null 2>&1; then
  step "cargo audit"
  if ! cargo audit; then
    echo "    FAILED — review advisories"
    fail=1
  fi
else
  echo "==> SKIP cargo audit (not installed). Install: cargo install cargo-audit"
fi

if command -v cargo-deny >/dev/null 2>&1; then
  step "cargo deny check"
  if ! cargo deny check; then
    echo "    FAILED — review deny warnings"
    fail=1
  fi
else
  echo "==> SKIP cargo deny (not installed). Install: cargo install cargo-deny"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "==> OK — quality gate passed"
else
  echo "==> FAIL — quality gate did NOT pass"
fi
exit "$fail"
