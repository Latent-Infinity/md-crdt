# md-crdt-yrs-bench

Unpublished nested workspace for **controlled** competitive benchmarks between
[`md-crdt`](https://github.com/latenty-infinity/md-crdt) and
[Yrs](https://docs.rs/yrs) (Rust Y-CRDT).

This package is **not** part of the root Cargo workspace. The root manifest lists
it under `workspace.exclude` so `just check`, `cargo test --workspace`, and
`just bench` never resolve or build Yrs.

The methodology, scenario contract, reporting rules, and success criteria are
self-contained below.

**Product engine remains in-tree md-crdt.** This crate measures; it does not
replace the RGA or add Yrs as a runtime dependency of the published library.

## Isolation

| Graph | Contents |
| --- | --- |
| Root workspace + `Cargo.lock` | Product crates only; no `yrs` |
| This directory + `Cargo.lock` | Comparison graph including `yrs` |

Invoke only via `--manifest-path` or the just recipes below.

## Product-shaped config (md-crdt features)

The compare dependency is:

```toml
md-crdt = { path = "..", default-features = false }
```

| Feature | Compare default | Notes |
| --- | --- | --- |
| `storage` / `filesync` | **off** | Competitive path is in-memory session only; no vault/persistence (Band E excluded) |
| `sequence_incremental` | **off** | Matches product default. Ablation: build with `--features` only on the product side (`just bench` runs both). Compare stays on the control (full rebuild) so ratios remain a stable soak baseline |
| `perf_trace` | **off** for Criterion | Enable via compare feature `sub_probes` for attribution only |

**Decision (P0):** keep `sequence_incremental` default-off in the product and in compare. Competitive numbers report the control path. When measuring the treatment, run product benches with `--features sequence_incremental` and label results; do not silently flip compare defaults.

## Pins (frozen)

| Dependency | Pin |
| --- | --- |
| `yrs` | `=0.27.3` (`default-features = false`) |
| `criterion` | `=0.5.1` |
| `md-crdt` | path `..`, `default-features = false` |
| Rust | declared repository MSRV `1.95` (effective host toolchain must build both crates) |

Yrs configuration fixed by methodology: root name `"text"`, client ids `1`/`2`,
`OffsetKind::Bytes`, `skip_gc = false`, lib0 v1 encoding. md-crdt peer ids `1`/`2`.
`ValidationLimits::default()` for remote apply. Changing any of these is a new
scenario, not a tuning knob.

## Scenario rules (v1)

1. **ASCII only** (`x` seed, `y` middle insert, `z` keystrokes/lag, `a`/`b` concurrent markers)
   so grapheme, Unicode scalar, UTF-16 unit, and byte offsets coincide. Non-ASCII is out of scope.
2. **Frozen histories:** bulk-seed N with one insert/transaction; lag K with K one-byte calls/transactions.
3. **Text-container body only** for correctness (`visible_string`); not Markdown structural serialize.
4. **Setup outside the timer:** Criterion `iter_batched_ref` + `BatchSize::LargeInput`.
5. **Tier labels required** on every id. Cross-tier ratios are forbidden.
6. **Contracts before timing:** exact sequential strings; concurrent runs assert per-engine convergence
   and one occurrence of each marker (cross-engine concurrent order is **not** compared).

### Size matrix

| Parameter | Values |
| --- | --- |
| Text length N | 1_000, 10_000 |
| Append / keystroke M | 32, 256 |
| Delta lag K | 1, 100 |

### Workloads by tier

| Tier | Workloads |
| --- | --- |
| **A – public text** (layer-inclusive) | `text_insert_middle`, `text_append_run`, `text_append_keystrokes`, `text_delete_middle` |
| **B – decoded integration** | `integrate_decoded_update` (`history=full` / `history=delta`) |
| **C – wire pipeline** | `wire_encode_full`, `wire_encode_delta`, `wire_decode_apply`, `two_peer_round_trip`, `multi_peer_fan_in` (stretch) |
| **A stretch** | `text_paste_middle` (single-call paste of R bytes) |

Stretch sizes: paste R ∈ {256, 1024, 4096}; fan-in peers ∈ {4, 8} at N=1000.

**Non-competitive (never ratio):** Criterion groups `diagnostic_md_crdt_markdown_serialize`,
`diagnostic_yrs_text_get_string`, `illustrative_yrs_block_map`. Memory: `just memory-compare`.
The memory recipe builds once, then records each engine's live document RSS in a separate probe
process; Cargo and compiler memory are outside the measurement.

**Codecs (Tier C only):**

| Engine | Codec label | Meaning |
| --- | --- | --- |
| md-crdt | `md_crdt_serde_json_v1` | `serde_json` of `ChangeMessage` (benchmark transport, not a product wire mandate) |
| Yrs | `yrs_lib0_v1` | `encode_state_as_update_v1` / `Update::decode_v1` |

### Out of scope for competitive claims

Product benches stay in root `benches/performance.rs` and must not be framed as Yrs wins/losses:

- workspace hierarchy / projection / edit replay
- table cell edit, structured workspace edit
- checkpoint history, sequence_incremental ablations
- Markdown serialize vs Yrs `get_string`
- low-level `Sequence` vs Yrs `Text`

md-crdt public text paths include **paragraph/block model overhead**; Tier A is layer-inclusive,
not a pure sequence-algorithm shootout.

## Commands

From the repository root:

```bash
# Contract / unit tests (no Criterion timings)
just test-compare

# Full competitive suite (do not cite without three full runs + provenance)
just bench-compare

# Liveness only — Criterion --test; never cite numbers from this recipe
just bench-compare-quick

# Three full invocations + provenance sidecar (citable path)
just bench-compare-report
# dry-run (schema only; not citable timings):
RUN_COMPARE_REPORT_DRY_RUN=1 just bench-compare-report
```

Equivalents:

```bash
cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked
cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked
cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked -- --test
md-crdt-yrs-bench/scripts/run_compare_report.sh [output_dir]
```

Criterion ids: `{tier}/{workload}/{engine}` with parameter `n=…` / `history=…`.
Each case prints a `case_metadata` line (ops, codec, wire bytes) before timing.

`MD_CRDT_COMPARE_ENGINE_ORDER=md_first|yrs_first` controls registration order for multi-run reports.

Format / lint / advisories:

```bash
cargo fmt --manifest-path md-crdt-yrs-bench/Cargo.toml --all -- --check
cargo clippy --manifest-path md-crdt-yrs-bench/Cargo.toml --all-targets --locked -- -D warnings
cargo deny --manifest-path md-crdt-yrs-bench/Cargo.toml check --config md-crdt-yrs-bench/deny.toml advisories
```

## Sub-probes and flamegraphs (md-crdt only)

Optional feature `sub_probes` enables product `perf_trace` and builds exclusive-span
attribution helpers. **Never ratio these against Yrs.**

```bash
# Print span tables for insert_middle n∈{1k,10k} and apply_remote k=100
just sub-probes
# equivalent:
cargo run --manifest-path md-crdt-yrs-bench/Cargo.toml --example sub_probes --features sub_probes --release --locked

# Select only one probe family (used by the flamegraph recipes)
cargo run --manifest-path md-crdt-yrs-bench/Cargo.toml --example sub_probes --features sub_probes --release --locked -- insert

# Tests for sub-probe wiring
cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --features sub_probes --locked

# Flamegraph (requires cargo-flamegraph; host-dependent)
just flamegraph-compare insert
# or with samply / Instruments:
samply record cargo run --manifest-path md-crdt-yrs-bench/Cargo.toml \
  --example sub_probes --features sub_probes --release -- insert
```

Workloads covered: middle `insert_text` on a bulk-seeded paragraph; `apply_remote` of
full history with lag keystrokes (k=100). Spans: `block_lookup`, `unit_expand`,
`envelope_encode`, `sequence_apply`, `sync_log_append`, `apply_validate` /
`apply_decode` / `apply_integrate`.

## How to interpret results

1. **Same tier only.** Ratio `public_text` vs `wire_pipeline` (or B vs C) is invalid.
2. **Layer-inclusive Tier A.** md-crdt times include collaborative session + paragraph model work;
   Yrs times include transactional `Doc` + root `Text`. Neither is “CRDT core only.”
3. **Tier B** isolates decoded integration (decode cost in setup). **Tier C** measures declared
   end-to-end codecs; wire byte sizes are protocol-specific, not pure efficiency medals.
4. **Concurrent order** may differ across engines; only per-engine convergence and marker presence
   are asserted. Do not claim a “correct” global string order across libraries.
5. **Noise.** Cite only after ≥3 full invocations with alternating engine order, unique Criterion
   baselines, and a provenance sidecar. Report estimates, confidence intervals, and between-run
   spread. Do not declare a winner when host variance is comparable to the gap.
6. **Never cite** `just bench-compare-quick` / Criterion `--test`, dry-runs, dirty worktrees, or
   incomplete provenance.
7. **No universal winner language.** Conclusions apply only to the named workload, tier, sizes,
   pins, and host.

## CI policy (frozen)

| Path | Policy |
| --- | --- |
| Root `just check` / product CI | **Does not** build or run this crate |
| Competitive benches | **Off by default** |
| Optional future | Manual or nightly workflow only, if explicitly added later |

## Provenance for cited results (S10)

Use `just bench-compare-report` (or the script). It requires a clean worktree by default, runs
**three** full suite invocations with alternating `md_first` / `yrs_first` order, unique
`--save-baseline` names (no overwrite), and writes:

| File | Contents |
| --- | --- |
| `provenance.json` | Schema-validated sidecar (commit, lockfiles, dependencies, frozen Yrs/peer/codec settings, toolchain, host, invocations, …) |
| `report.md` | Human summary pointing at baselines and interpretation rules |

Raw Criterion data remains under `md-crdt-yrs-bench/target/criterion/` and is **not** committed.
Report directories under `report-out/` are generated on demand and **not** committed by default.

Schema keys are defined in `src/report.rs` (`PROVENANCE_REQUIRED_KEYS`) and checked by unit tests
against `tests/fixtures/provenance_valid.json`.

## Layout

```text
src/           contracts, adapters, runners, report schema
benches/       Criterion compare target
tests/         adapter contracts + fixtures
scripts/       multi-run provenance report + separate-process RSS probe
```
