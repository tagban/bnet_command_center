#!/usr/bin/env bash
# Full verification, including the two crates that need crates.io.
#
# The workspace ships with `bnetccd` and `bnetcc-storage-sqlite` excluded, because the
# environment this was scaffolded in had no registry access. This script enables them,
# runs everything, and writes verify.log — which is the file to send back when something
# fails, since it carries the full compiler output rather than a screenshot of it.
set -uo pipefail
cd "$(dirname "$0")/.."

LOG=verify.log
: > "$LOG"
say() { echo "=== $* ===" | tee -a "$LOG"; }

cp Cargo.toml Cargo.toml.workspace-backup
python3 - <<'PY'
s = open('Cargo.toml').read()
s = s.replace('    "crates/smoke",\n]', '    "crates/smoke",\n    "crates/bnetccd",\n    "crates/bnetcc-storage-sqlite",\n]')
s = s.replace('exclude = ["crates/bnetccd", "crates/bnetcc-storage-sqlite"]', 'exclude = []')
open('Cargo.toml','w').write(s)
PY

say "cargo --version"; cargo --version 2>&1 | tee -a "$LOG"
say "cargo build --workspace";      cargo build --workspace 2>&1 | tee -a "$LOG"; BUILD=${PIPESTATUS[0]}
say "cargo test --workspace";       cargo test  --workspace 2>&1 | tee -a "$LOG"; TEST=${PIPESTATUS[0]}
say "cargo clippy --all-targets";   cargo clippy --all-targets -- -D warnings 2>&1 | tee -a "$LOG"; CLIPPY=${PIPESTATUS[0]}

say "summary"
{
  echo "build=$BUILD test=$TEST clippy=$CLIPPY"
  [ "$BUILD" -eq 0 ] && echo "BUILD OK" || echo "BUILD FAILED"
  [ "$TEST"  -eq 0 ] && echo "TESTS OK" || echo "TESTS FAILED"
  [ "$CLIPPY" -eq 0 ] && echo "CLIPPY OK" || echo "CLIPPY FAILED"
} | tee -a "$LOG"

if [ "$BUILD" -ne 0 ]; then
  echo "Restoring the excluded-workspace Cargo.toml so the library crates still build." | tee -a "$LOG"
  mv Cargo.toml.workspace-backup Cargo.toml
else
  rm -f Cargo.toml.workspace-backup
  echo "Left the full workspace enabled — bnetccd and the SQLite backend now build." | tee -a "$LOG"
fi

echo
echo "Wrote $LOG"
