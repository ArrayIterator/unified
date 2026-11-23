#!/usr/bin/env bash
# One-command verification suite for the Unified Pentagonal Project.
#
# Stages, in order:
#   1. cargo fmt --all -- --check             (report only; never reformats)
#   2. cargo clippy --workspace --all-targets (warnings denied)
#   3. cargo check, every workspace member except the root `unified` bin
#   4. cargo test, every member crate that has tests
#   5. TypeScript check (npm run check) when node_modules exists
#   6. Root bin: cargo check -p unified + cargo test -p unified
#      (LAST; needs >= 12 GiB RAM -- a single rustc on the bin crate peaks
#      at ~5.7 GiB RSS and is OOM-killed on the 7.7 GiB dev VM)
#
# Every stage runs even if an earlier one failed. Each stage prints a
# PASS/FAIL/SKIP line, a summary is printed at the end, and the script
# exits non-zero when any stage failed.
#
# Usage:
#   bash scripts/check.sh [--features gateway]
#                         [--no-clippy] [--no-test] [--no-root-bin] [--no-ts]
#
# Environment:
#   UNIFIED_OFFLINE=0      allow network access (default 1: --offline
#                          everywhere; CI sets 0 so a cold cache can fetch
#                          dependencies).
#   UNIFIED_SKIP_ROOT_BIN=1
#                          same as --no-root-bin.
#   CARGO_TARGET_DIR       honoured when set; otherwise the script reuses
#                          ~/workspace/unified-target when that directory
#                          exists (the shared, sccache-warmed target dir on
#                          the dev VM).
#
# Requires a Rust toolchain (rust-version 1.85 in Cargo.toml; CI uses stable).
# `--features gateway` also runs the clippy and check steps for the root bin
# with the Pingora-gated gateway feature enabled.
set -euo pipefail

cd "$(dirname "$0")/.."

NO_CLIPPY=false
NO_TEST=false
NO_ROOT_BIN=false
NO_TS=false
WITH_GATEWAY=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --features)
      if [ "${2:-}" = "gateway" ]; then
        WITH_GATEWAY=true
        shift 2
      else
        echo "error: only '--features gateway' is supported" >&2
        exit 2
      fi
      ;;
    --features=gateway)
      WITH_GATEWAY=true
      shift
      ;;
    --no-clippy)
      NO_CLIPPY=true
      shift
      ;;
    --no-test)
      NO_TEST=true
      shift
      ;;
    --no-root-bin)
      NO_ROOT_BIN=true
      shift
      ;;
    --no-ts)
      NO_TS=true
      shift
      ;;
    -h|--help)
      echo "Usage: bash scripts/check.sh [--features gateway] [--no-clippy] [--no-test] [--no-root-bin] [--no-ts]"
      echo "  --features gateway   also check the root bin with the gateway feature"
      echo "  --no-clippy          skip the clippy stage"
      echo "  --no-test            skip the cargo test stage (and root-bin tests)"
      echo "  --no-root-bin        skip the root bin stage (needs >= 12 GiB RAM)"
      echo "  --no-ts              skip the TypeScript check"
      echo "Env: UNIFIED_OFFLINE=0 allows network; UNIFIED_SKIP_ROOT_BIN=1 skips the root bin."
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      echo "Usage: bash scripts/check.sh [--features gateway] [--no-clippy] [--no-test] [--no-root-bin] [--no-ts]" >&2
      exit 2
      ;;
  esac
done

if [ "${UNIFIED_SKIP_ROOT_BIN:-0}" = "1" ]; then
  NO_ROOT_BIN=true
fi

# Lean environment: one rustc at a time and no incremental state, so the
# per-crate checks fit a small VM. The root bin is handled by skipping it
# (stage 6), not by different flags.
if ! command -v cargo >/dev/null 2>&1 && [ -d "$HOME/.cargo/bin" ]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
export CARGO_BUILD_JOBS=1
export CARGO_INCREMENTAL=0
if [ -z "${CARGO_TARGET_DIR:-}" ] && [ -d "$HOME/workspace/unified-target" ]; then
  export CARGO_TARGET_DIR="$HOME/workspace/unified-target"
fi
if command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER=sccache
fi

OFFLINE=()
if [ "${UNIFIED_OFFLINE:-1}" = "1" ]; then
  OFFLINE=(--offline)
fi

# Every workspace member except the root `unified` bin.
CHECK_CRATES=(
  unified-core unified-scripting unified-ts
  unified-db-core unified-db-commerce unified-db-education unified-db-people
  unified-db-operations unified-db-platform unified-db-infra
  external-whatsapp external-esignature external-fleet external-iot external-bi
  example-hello-module unified-calculator
)

# Members with #[cfg(test)] code (verified 2026-11-23). The seven
# unified-db-* crates and example-hello-module have no tests.
TEST_CRATES=(
  unified-core unified-scripting unified-ts
  external-whatsapp external-esignature external-fleet external-iot external-bi
  unified-calculator
)

declare -a RESULTS=()

run_stage() {
  local name="$1"
  shift
  printf '\n=== %s ===\n' "$name"
  if "$@"; then
    printf 'PASS: %s\n' "$name"
    RESULTS+=("PASS  $name")
  else
    printf 'FAIL: %s\n' "$name"
    RESULTS+=("FAIL  $name")
  fi
}

skip_stage() {
  printf '\n=== %s ===\n' "$1"
  printf 'SKIP: %s\n' "$1"
  RESULTS+=("SKIP  $1")
}

clippy_stage() {
  cargo clippy --workspace "${OFFLINE[@]}" --all-targets -- -D warnings
  if [ "$WITH_GATEWAY" = "true" ]; then
    cargo clippy -p unified "${OFFLINE[@]}" --all-targets --features gateway -- -D warnings
  fi
}

# Needs >= 12 GiB RAM: a single rustc on this crate peaks at ~5.7 GiB RSS.
root_bin_stage() {
  local gateway_args=()
  if [ "$WITH_GATEWAY" = "true" ]; then
    gateway_args=(--features gateway)
  fi
  cargo check "${OFFLINE[@]}" -p unified "${gateway_args[@]}"
  if [ "$NO_TEST" = "false" ]; then
    cargo test "${OFFLINE[@]}" -p unified "${gateway_args[@]}"
  fi
}

# When network access is allowed, fetch the whole lockfile first so a cold
# cache cannot fail the --offline runs below (CI sets UNIFIED_OFFLINE=0).
if [ "${UNIFIED_OFFLINE:-1}" = "0" ]; then
  run_stage "cargo fetch" cargo fetch
fi

run_stage "cargo fmt --all -- --check" cargo fmt --all -- --check

if [ "$NO_CLIPPY" = "true" ]; then
  skip_stage "cargo clippy (--no-clippy)"
else
  run_stage "cargo clippy --workspace --all-targets" clippy_stage
fi

check_args=()
for crate in "${CHECK_CRATES[@]}"; do
  check_args+=(-p "$crate")
done
run_stage "cargo check (workspace crates, no root bin)" cargo check "${OFFLINE[@]}" "${check_args[@]}"

if [ "$NO_TEST" = "true" ]; then
  skip_stage "cargo test (member crates) (--no-test)"
else
  test_args=()
  for crate in "${TEST_CRATES[@]}"; do
    test_args+=(-p "$crate")
  done
  run_stage "cargo test (member crates with tests)" cargo test "${OFFLINE[@]}" "${test_args[@]}"
fi

if [ "$NO_TS" = "true" ]; then
  skip_stage "TypeScript check (--no-ts)"
elif [ -d node_modules ] && [ -f package.json ]; then
  run_stage "TypeScript check (npm run check)" npm run check
else
  skip_stage "TypeScript check (no node_modules; run npm install first)"
fi

if [ "$NO_ROOT_BIN" = "true" ]; then
  skip_stage "root bin check + test (--no-root-bin; needs >= 12 GiB RAM)"
else
  run_stage "root bin check + test (needs >= 12 GiB RAM)" root_bin_stage
fi

printf '\n=== Summary ===\n'
failures=0
for result in "${RESULTS[@]}"; do
  printf '%s\n' "$result"
  case "$result" in
    FAIL*) failures=$((failures + 1)) ;;
  esac
done
if [ "$failures" -gt 0 ]; then
  printf '\n%d stage(s) failed.\n' "$failures"
  exit 1
fi
printf '\nAll checks passed.\n'
