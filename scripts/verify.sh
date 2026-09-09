#!/usr/bin/env bash
#
# Full verification: build, test, lint and load-test every crate in the workspace,
# including `bnetccd` and `bnetcc-storage-sqlite` (both need crates.io, and both are
# regular workspace members as of 2026-09-08 — see docs/HANDOFF.md §2). Writes
# verify.log with the actual compiler output rather than a summary of it.
#
# Run it with:      bash scripts/verify.sh
# Then send back:   verify.log

set -u
cd "$(dirname "$0")/.." || exit 1

LOG="$PWD/verify.log"
: > "$LOG"

say() { printf '\n=== %s ===\n' "$*" | tee -a "$LOG"; }
note() { printf '%s\n' "$*" | tee -a "$LOG"; }

# rustup installs to ~/.cargo/bin, which a non-login shell may not have on PATH.
if ! command -v cargo >/dev/null 2>&1; then
  # shellcheck disable=SC1090
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  note "cargo not found. Install Rust from https://rustup.rs and re-run."
  exit 1
fi

say "environment"
{ cargo --version; rustc --version; uname -sm; } 2>&1 | tee -a "$LOG"

# --- Build, test, lint ---------------------------------------------------------------
say "cargo build --workspace"
cargo build --workspace 2>&1 | tee -a "$LOG"; BUILD=${PIPESTATUS[0]}

say "cargo test --workspace"
cargo test --workspace 2>&1 | tee -a "$LOG"; TEST=${PIPESTATUS[0]}

say "cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings 2>&1 | tee -a "$LOG"; CLIPPY=${PIPESTATUS[0]}

# --- Load test, only if it built ------------------------------------------------------
SMOKE="skipped"
if [ "$BUILD" -eq 0 ]; then
  say "load test (2500 concurrent connections)"
  # macOS defaults to 256 open files, which is far below what this needs. Raising the
  # soft limit does not require sudo; the ceiling is kern.maxfilesperproc.
  ulimit -n 20000 2>/dev/null || ulimit -n 10240 2>/dev/null || true
  note "open file limit: $(ulimit -n)"

  # This harness is thread-per-connection on both ends (its own client plus the
  # server's handler — see the module docs), so N connections costs the host roughly
  # 2N OS threads. Linux hosts default to a very high thread ceiling, but macOS
  # (kern.num_taskthreads) is often just a few thousand per process — and running
  # right up against that ceiling doesn't just refuse new threads, it makes the
  # scheduler sluggish enough that the harness can sit for minutes waiting on
  # messages from connections that were never spawned. Scale the target down to what
  # this host can sustain with headroom, rather than finding out the hard way.
  SMOKE_TARGET=2500
  TASK_THREAD_CAP=$(sysctl -n kern.num_taskthreads 2>/dev/null || true)
  if [ -n "$TASK_THREAD_CAP" ]; then
    SAFE_TARGET=$((TASK_THREAD_CAP * 70 / 100 / 2))
    if [ "$SAFE_TARGET" -lt "$SMOKE_TARGET" ]; then
      note "host thread ceiling (kern.num_taskthreads=$TASK_THREAD_CAP) is below" \
           "what $SMOKE_TARGET connections need at ~2 threads each;" \
           "scaling the load test down to $SAFE_TARGET."
      SMOKE_TARGET=$SAFE_TARGET
    fi
  fi

  if cargo build --release -p bnetcc-smoke >>"$LOG" 2>&1; then
    ./target/release/bnetcc-smoke "$SMOKE_TARGET" 40 2>&1 | tee -a "$LOG"
    SMOKE="ran"
  else
    note "smoke harness failed to build; see above"
    SMOKE="build failed"
  fi
fi

# --- Summary --------------------------------------------------------------------------
say "summary"
{
  echo "build=$BUILD  test=$TEST  clippy=$CLIPPY  loadtest=$SMOKE"
  [ "$BUILD"  -eq 0 ] && echo "BUILD OK"  || echo "BUILD FAILED"
  [ "$TEST"   -eq 0 ] && echo "TESTS OK"  || echo "TESTS FAILED"
  [ "$CLIPPY" -eq 0 ] && echo "CLIPPY OK" || echo "CLIPPY FAILED"
} | tee -a "$LOG"

printf '\nWrote %s\n' "$LOG"
