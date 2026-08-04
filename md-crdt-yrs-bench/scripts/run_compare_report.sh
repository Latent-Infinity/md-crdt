#!/usr/bin/env bash
# Multi-run competitive report with unique Criterion baselines and provenance.
#
# Usage (from repository root):
#   md-crdt-yrs-bench/scripts/run_compare_report.sh [output_dir]
#
# Environment:
#   RUN_COMPARE_REPORT_DRY_RUN=1       — provenance only (no cargo bench)
#   RUN_COMPARE_REPORT_ALLOW_DIRTY=1   — allow dirty worktree (not citable)
#   COMPARE_POWER_MODE=...             — optional host power annotation
#
# Never overwrites an existing output directory. Does not commit baselines.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${1:-md-crdt-yrs-bench/report-out/${STAMP}}"
DRY_RUN="${RUN_COMPARE_REPORT_DRY_RUN:-0}"
ALLOW_DIRTY="${RUN_COMPARE_REPORT_ALLOW_DIRTY:-0}"

if [[ -e "$OUT_DIR" ]]; then
  echo "error: output directory already exists (refusing overwrite): $OUT_DIR" >&2
  exit 1
fi

# Avoid `cmd | grep -q` under `pipefail` (SIGPIPE from early grep exit).
if [[ -n "$(git status --porcelain)" ]]; then
  CLEAN_WORKTREE=false
else
  CLEAN_WORKTREE=true
fi

if [[ "$CLEAN_WORKTREE" != true && "$ALLOW_DIRTY" != 1 ]]; then
  echo "error: worktree is not clean; commit/stash changes or set RUN_COMPARE_REPORT_ALLOW_DIRTY=1" >&2
  exit 1
fi
if [[ "$CLEAN_WORKTREE" != true ]]; then
  echo "warning: dirty worktree — results are not citable under methodology S10" >&2
fi
mkdir -p "$OUT_DIR"

GIT_COMMIT="$(git rev-parse HEAD)"
sha256_file() {
  python3 - "$1" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
}
ROOT_LOCK_SHA="$(sha256_file Cargo.lock)"
COMPARE_LOCK_SHA="$(sha256_file md-crdt-yrs-bench/Cargo.lock)"

read_toml_version() {
  local file="$1" key="$2"
  python3 - "$file" "$key" <<'PY'
import re, sys
path, key = sys.argv[1], sys.argv[2]
text = open(path).read()
if key == "package_version":
    m = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
elif key == "yrs":
    m = re.search(r'yrs\s*=\s*\{[^}]*version\s*=\s*"=?([^"]+)"', text)
elif key == "criterion":
    m = re.search(r'criterion\s*=\s*"=?([^"]+)"', text)
else:
    m = None
print(m.group(1) if m else "unknown")
PY
}

YRS_VERSION="$(read_toml_version md-crdt-yrs-bench/Cargo.toml yrs)"
CRITERION_VERSION="$(read_toml_version md-crdt-yrs-bench/Cargo.toml criterion)"
MD_CRDT_VERSION="$(read_toml_version Cargo.toml package_version)"
YRS_CHECKSUM="$(
  python3 - <<'PY'
import re
lock = open("md-crdt-yrs-bench/Cargo.lock").read()
for block in lock.split("[[package]]"):
    if 'name = "yrs"' in block:
        m = re.search(r'checksum = "([0-9a-f]+)"', block)
        if m:
            print(m.group(1))
            break
else:
    print("unknown")
PY
)"

RUSTC_VERBOSE="$(rustc -Vv 2>&1 || true)"
# Do not `exit` early in awk under `pipefail` (SIGPIPE kills the pipeline).
HOST_TRIPLE="$(rustc -vV 2>/dev/null | awk -F': ' '/^host:/{print $2; found=1} END{if(!found) print "unknown"}')"
TARGET_TRIPLE="${CARGO_BUILD_TARGET:-$HOST_TRIPLE}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
if [[ "$(uname -s)" == "Darwin" ]]; then
  CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
else
  CPU_MODEL="$(awk -F': ' '/model name/{print $2; found=1} END{if(!found) print "unknown"}' /proc/cpuinfo 2>/dev/null || echo unknown)"
fi
POWER_MODE="${COMPARE_POWER_MODE:-unknown}"

INV_FILE="$(mktemp)"
trap 'rm -f "$INV_FILE"' EXIT
echo '[]' >"$INV_FILE"
ORDERS=(md_first yrs_first md_first)
OVERALL_STATUS=0

for i in 0 1 2; do
  ORDER="${ORDERS[$i]}"
  BASELINE="compare_${i}_${ORDER}_${STAMP}"
  STARTED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  export MD_CRDT_COMPARE_ENGINE_ORDER="$ORDER"
  CMD="cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked -- --save-baseline ${BASELINE}"
  STATUS="ok"
  if [[ "$DRY_RUN" == 1 ]]; then
    STATUS="dry_run"
    echo "[dry-run] would run: $CMD"
  else
    echo "Running invocation $i order=$ORDER baseline=$BASELINE"
    set +e
    cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked -- --save-baseline "$BASELINE"
    rc=$?
    set -e
    if [[ $rc -ne 0 ]]; then
      STATUS="failed"
      OVERALL_STATUS=1
    fi
  fi
  FINISHED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  python3 - "$INV_FILE" "$i" "$ORDER" "$BASELINE" "$CMD" "$STARTED" "$FINISHED" "$STATUS" <<'PY'
import json, sys
path, index, order, baseline, cmd, started, finished, status = sys.argv[1:]
data = json.loads(open(path).read())
data.append({
    "index": int(index),
    "engine_order": order,
    "baseline_name": baseline,
    "command": cmd,
    "started_utc": started,
    "finished_utc": finished,
    "status": status,
})
open(path, "w").write(json.dumps(data))
PY
  if [[ "$STATUS" == "failed" ]]; then
    echo "error: invocation $i failed; writing partial provenance" >&2
    break
  fi
done

export OUT_DIR GIT_COMMIT ROOT_LOCK_SHA COMPARE_LOCK_SHA YRS_VERSION YRS_CHECKSUM
export MD_CRDT_VERSION CRITERION_VERSION HOST_TRIPLE TARGET_TRIPLE OS CPU_MODEL POWER_MODE
export STAMP CLEAN_WORKTREE INV_FILE RUSTC_VERBOSE

python3 - <<'PY'
import json, os
from pathlib import Path

stamp = os.environ["STAMP"]
timestamp = f"{stamp[0:4]}-{stamp[4:6]}-{stamp[6:8]}T{stamp[9:11]}:{stamp[11:13]}:{stamp[13:15]}Z"
invocations = json.loads(Path(os.environ["INV_FILE"]).read_text())
provenance = {
    "schema_version": 1,
    "clean_worktree": os.environ["CLEAN_WORKTREE"] == "true",
    "git_commit": os.environ["GIT_COMMIT"],
    "root_lockfile_sha256": os.environ["ROOT_LOCK_SHA"],
    "compare_lockfile_sha256": os.environ["COMPARE_LOCK_SHA"],
    "yrs_version": os.environ["YRS_VERSION"],
    "yrs_checksum": os.environ["YRS_CHECKSUM"],
    "md_crdt_version": os.environ["MD_CRDT_VERSION"],
    "criterion_version": os.environ["CRITERION_VERSION"],
    "md_crdt_features": "default-features=false",
    "yrs_features": "default-features=false",
    "yrs_text_root": "text",
    "yrs_offset_kind": "Bytes",
    "yrs_skip_gc": False,
    "md_crdt_peer_ids": [1, 2],
    "yrs_client_ids": [1, 2],
    "md_crdt_wire_codec": "md_crdt_serde_json_v1",
    "yrs_wire_codec": "yrs_lib0_v1",
    "rustc_verbose": os.environ.get("RUSTC_VERBOSE", ""),
    "host_triple": os.environ.get("HOST_TRIPLE", "unknown"),
    "target_triple": os.environ.get("TARGET_TRIPLE", "unknown"),
    "os": os.environ.get("OS", "unknown"),
    "cpu_model": os.environ.get("CPU_MODEL", "unknown"),
    "power_mode": os.environ.get("POWER_MODE", "unknown"),
    "batch_policy": "LargeInput",
    "timestamp_utc": timestamp,
    "output_dir": os.environ["OUT_DIR"],
    "invocations": invocations,
}
out = Path(os.environ["OUT_DIR"])
(out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")

cs = provenance["yrs_checksum"]
cs_short = (cs[:16] + "…") if len(cs) > 16 else cs
lines = [
    "# md-crdt vs Yrs competitive report",
    "",
    f"- Generated (UTC): `{provenance['timestamp_utc']}`",
    f"- Git commit: `{provenance['git_commit']}`",
    f"- Clean worktree: **{provenance['clean_worktree']}**",
    f"- Yrs: `{provenance['yrs_version']}` checksum `{cs_short}`",
    f"- md-crdt: `{provenance['md_crdt_version']}`",
    f"- Criterion: `{provenance['criterion_version']}`",
    f"- Peers / Yrs clients: `{provenance['md_crdt_peer_ids']}` / `{provenance['yrs_client_ids']}`",
    f"- Yrs model: root `{provenance['yrs_text_root']}`, offset `{provenance['yrs_offset_kind']}`, skip_gc `{provenance['yrs_skip_gc']}`",
    f"- Wire codecs: `{provenance['md_crdt_wire_codec']}` / `{provenance['yrs_wire_codec']}`",
    f"- Host: `{provenance['host_triple']}` / `{provenance['os']}`",
    f"- CPU: {provenance['cpu_model']}",
    f"- Power mode: {provenance['power_mode']}",
    f"- Batch policy: {provenance['batch_policy']}",
    "",
    "## Invocations",
    "",
    "| # | Engine order | Baseline | Status |",
    "| --- | --- | --- | --- |",
]
for inv in invocations:
    lines.append(
        f"| {inv['index']} | {inv['engine_order']} | `{inv['baseline_name']}` | {inv['status']} |"
    )
lines += [
    "",
    "## How to interpret",
    "",
    "See `md-crdt-yrs-bench/README.md` (How to interpret results).",
    "Do **not** ratio across tiers. Do **not** cite dry-run or Criterion `--test` smoke numbers.",
    "Raw Criterion estimates live under `md-crdt-yrs-bench/target/criterion` for each `--save-baseline` name.",
    "This report directory is **not** committed by default.",
    "",
]
(out / "report.md").write_text("\n".join(lines))
print(f"Wrote {out / 'provenance.json'}")
print(f"Wrote {out / 'report.md'}")

# Schema checks (mirrors report::validate_provenance_document)
required = [
    "schema_version", "clean_worktree", "git_commit", "root_lockfile_sha256",
    "compare_lockfile_sha256", "yrs_version", "yrs_checksum", "md_crdt_version",
    "criterion_version", "md_crdt_features", "yrs_features", "rustc_verbose",
    "yrs_text_root", "yrs_offset_kind", "yrs_skip_gc", "md_crdt_peer_ids",
    "yrs_client_ids", "md_crdt_wire_codec", "yrs_wire_codec",
    "host_triple", "target_triple", "os", "cpu_model", "power_mode",
    "batch_policy", "timestamp_utc", "output_dir", "invocations",
]
inv_required = [
    "index", "engine_order", "baseline_name", "command",
    "started_utc", "finished_utc", "status",
]
for key in required:
    if key not in provenance:
        raise SystemExit(f"missing provenance key: {key}")
expected_config = {
    "yrs_text_root": "text",
    "yrs_offset_kind": "Bytes",
    "yrs_skip_gc": False,
    "md_crdt_peer_ids": [1, 2],
    "yrs_client_ids": [1, 2],
    "md_crdt_wire_codec": "md_crdt_serde_json_v1",
    "yrs_wire_codec": "yrs_lib0_v1",
}
for key, expected in expected_config.items():
    if provenance[key] != expected:
        raise SystemExit(f"{key} must be {expected!r}")
if len(invocations) < 3:
    raise SystemExit("need at least 3 invocations")
bases = set()
for i, inv in enumerate(invocations):
    for key in inv_required:
        if key not in inv:
            raise SystemExit(f"invocations[{i}] missing {key}")
    if inv["baseline_name"] in bases:
        raise SystemExit("duplicate baseline_name")
    bases.add(inv["baseline_name"])
o0, o1, o2 = (invocations[j]["engine_order"] for j in range(3))
if o0 == o1 or o1 == o2:
    raise SystemExit("engine_order must alternate across first three runs")
print("provenance schema ok")
PY

echo "Report complete: $OUT_DIR"
exit "$OVERALL_STATUS"
