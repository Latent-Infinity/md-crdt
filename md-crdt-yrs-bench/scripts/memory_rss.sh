#!/usr/bin/env bash
# Separate-process RSS snapshots for md-crdt vs Yrs (Phase 6.3).
# Never merge both engines in one process.
#
# Usage (repo root):
#   md-crdt-yrs-bench/scripts/memory_rss.sh [n]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
N="${1:-10000}"
OUT="${2:-md-crdt-yrs-bench/report-out/memory-rss-$(date -u +%Y%m%dT%H%M%SZ).txt}"
mkdir -p "$(dirname "$OUT")"

# Build once, then time only the probe process. Timing `cargo run` would mix
# Cargo/rustc memory with the engine document being measured.
cargo build --release --manifest-path md-crdt-yrs-bench/Cargo.toml \
  --example memory_probe --locked
PROBE_BIN="$(
  cargo metadata --manifest-path md-crdt-yrs-bench/Cargo.toml \
    --format-version 1 --no-deps \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"] + "/release/examples/memory_probe")'
)"
if [[ ! -x "$PROBE_BIN" ]]; then
  echo "error: memory probe executable not found: $PROBE_BIN" >&2
  exit 1
fi

run_one() {
  local engine="$1"
  echo "=== engine=$engine n=$N ==="
  if [[ "$(uname -s)" == "Darwin" ]]; then
    /usr/bin/time -l "$PROBE_BIN" "$engine" "$N" 200 2>&1
  else
    /usr/bin/time -v "$PROBE_BIN" "$engine" "$N" 200 2>&1
  fi
  echo
}

{
  echo "# Memory RSS probe (separate processes)"
  echo "# n=$N host=$(uname -a)"
  echo
  run_one md_crdt
  run_one yrs
  echo "# Interpret RSS figures separately per engine; do not treat as Criterion timings."
} | tee "$OUT"

echo "Wrote $OUT"
