#!/usr/bin/env bash
#
# Full verification, including the two crates that need crates.io.
#
# The workspace ships with `bnetccd` and `bnetcc-storage-sqlite` excluded, because the
# environment this was scaffolded in had no registry access — so those two have never
# been through a compiler. This script enables them, builds, tests, lints, and writes
# verify.log.
#
# Run it with:      bash scripts/verify.sh
# Then send back:   verify.log
#
# That log is the useful artifact: it carries the actual compiler output rather than a
# summary of it. Written for macOS and Linux with no dependencies beyond awk and cargo.

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

# --- Enable the two excluded crates -------------------------------------------------
# awk rather than sed -i, because BSD sed on macOS and GNU sed disagree about both the
# -i flag and newlines in a replacement.
cp Cargo.toml Cargo.toml.bak
awk '
  /^    "crates\/smoke",$/ {
    print
    print "    \"crates/bnetccd\","
    print "    \"crates/bnetcc-storage-sqlite\","
    next
  }
  /^exclude = \[/ { print "exclude = []"; next }
  { print }
' Cargo.toml.bak > Cargo.toml

if ! grep -q 'crates/bnetccd' Cargo.toml; then
  note "Could not enable the excluded crates automatically; Cargo.toml may have changed."
  note "Add \"crates/bnetccd\" and \"crates/bnetcc-storage-sqlite\" to members by hand."
  mv Cargo.toml.bak Cargo.toml
  exit 1
fi
note "Enabled bnetccd and bnetcc-storage-sqlite in the workspace."

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
  if cargo build --release -p bnetcc-smoke >>"$LOG" 2>&1; then
    ./target/release/bnetcc-smoke 2500 40 2>&1 | tee -a "$LOG"
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

if [ "$BUILD" -ne 0 ]; then
  note ""
  note "Build failed, so the reduced workspace has been restored — the four library"
  note "crates still build and test on their own."
  mv Cargo.toml.bak Cargo.toml
else
  rm -f Cargo.toml.bak
  note ""
  note "Full workspace left enabled: bnetccd and the SQLite backend now build."
fi

printf '\nWrote %s\n' "$LOG"
