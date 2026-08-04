# md-crdt vs Yrs Competitive Benchmark — Plan State

Companion to [`yrs-bench-compare-plan.md`](./yrs-bench-compare-plan.md).

| Phase | Status | Notes |
| --- | --- | --- |
| **0** Scaffolding | **done** | Nested workspace, root exclude, just recipes, lockfile, deny policy |
| **1** Shared contracts / size matrix | **done** | sizes, traits, 32 manifests, harness batch helpers |
| **2** md-crdt adapter | **done** | `MdCrdtAdapter` TextEngine + SyncEngine; Tier A contracts |
| **3** Yrs adapter | **done** | `YrsAdapter` + shared `adapter_contract` matrix |
| **4** Criterion suite | **done** | `benches/compare.rs`; 64 smoke cases; metadata |
| **5** Docs / reporting | **done** | README, provenance script, CI policy, arch note |
| **6** Optional extensions | **done** | paste, fan-in, memory RSS, diagnostics, illustrative map |

## Phase 0 completion record

**Completed:** 2026-08-04

### Tasks

| ID | Status | Evidence |
| --- | --- | --- |
| 0.1 | done | `md-crdt-yrs-bench/Cargo.toml` has `[package]` + `[workspace]` resolver 2, `publish = false`, edition 2024, rust-version 1.85 |
| 0.2 | done (with decision) | See **MSRV / pin decision** below |
| 0.3 | done | `src/lib.rs` constants + link smoke tests; `README.md` methodology stub |
| 0.4 | done | Root `exclude = ["md-crdt-yrs-bench"]`; `cargo metadata --locked` has no `yrs` |
| 0.5 | done | `just test-compare`, `bench-compare`, `bench-compare-quick` |
| 0.6 | done | Comparison work did not change the root `Cargo.lock`; no yrs in root graph. Later product dependency changes are independent of this workspace. |
| 0.7 | done | Committed comparison `Cargo.lock`; advisories green with explicit ignore; SPDX inventory recorded |
| 0.8 | done | `[profile.bench] debug = true` matches root |
| 0.9 | done | `cargo fmt --check` and `clippy -D warnings` green for comparison crate |

### MSRV / pin decision (0.2)

- Pin retained: `yrs = "=0.27.3"` (latest on crates.io as of 2026-08-04).
- `cargo +1.85.0 check --manifest-path md-crdt-yrs-bench/Cargo.toml --locked` **fails** for:
  - **yrs 0.27.3:** `if let` guards (unstable on 1.85)
  - **path-dep md-crdt:** `let` chains (unstable on 1.85)
- Repository `rust-version = "1.85"` was **not** raised (plan forbids silent MSRV bump).
- Effective compile of product + comparison today requires a toolchain that accepts those language features (host stable used for gates; ≥1.88 in practice). Tracking product MSRV accuracy is out of scope for this comparison crate.
- Comparison crate keeps `rust-version = "1.85"` to match the declared repository field.

### Advisory decision (0.7)

- `RUSTSEC-2026-0215` (unmaintained `smallstr` via `yrs 0.27.3`): **explicitly accepted** for the comparison-only graph in `md-crdt-yrs-bench/deny.toml`. No safe upgrade; not present in root lockfile.
- Command: `cargo deny --manifest-path md-crdt-yrs-bench/Cargo.toml check --config md-crdt-yrs-bench/deny.toml advisories` → ok.

### SPDX inventory snapshot (0.7)

From `cargo deny --manifest-path md-crdt-yrs-bench/Cargo.toml list --format json` (no unknown/unlicensed groups):

| Expression | Approx. crate count in list |
| --- | --- |
| MIT | 73 |
| Apache-2.0 | 64 |
| 0BSD | 1 |
| LGPL-2.1-or-later | 1 (`r-efi` transitive) |
| Unicode-3.0 | 1 |
| Unlicense | 1 |
| Zlib | 1 |

No unlicensed/unknown package blocked completion. License **allowlist** still not enforced (no project-wide `deny.toml` licenses policy).

### Gate results

| Gate | Result |
| --- | --- |
| `cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked` | 3 passed |
| `cargo fmt --manifest-path md-crdt-yrs-bench/Cargo.toml --all -- --check` | ok |
| `cargo clippy --manifest-path md-crdt-yrs-bench/Cargo.toml --all-targets --locked -- -D warnings` | ok (md-crdt path-dep emits dead_code warnings when `default-features = false`; comparison crate itself clean) |
| Root `just check` | ok |
| Root graph contains yrs | false |

## Phase 1 completion record

**Completed:** 2026-08-04

### Tasks

| ID | Status | Evidence |
| --- | --- | --- |
| 1.1 | done | `sizes.rs`: `TEXT_LENS`/`APPEND_LENS`/`DELTA_LAGS`, fillers, `fill_text` / `middle_index` |
| 1.2 | done | `adapter.rs`: `TextEngine` + `SyncEngine`; fake-engine generic runner tests |
| 1.3 | done | `scenario.rs`: all v1 workloads → 32 unique manifests with tiers, schedules, exact or concurrent expectations |
| 1.4 | done | `harness.rs`: `run_batched_iteration` / traced spy; setup-before-measure; seed clone in setup only |

### Design notes

- `BatchPolicy::LargeInput` is harness-owned (Criterion mapping lands with benches); default for all v1 manifests.
- Lag edits use repeated `KEYSTROKE_PAYLOAD` (`"z"`) so histories stay ASCII and single-byte.
- `two_peer_round_trip` uses `SequentialExpectation::ConcurrentMarkers` (no cross-engine order claim).
- Adapters remain stubs until Phase 2/3.

### Gate results

| Gate | Result |
| --- | --- |
| `cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked` | **25 passed** |
| `cargo fmt --manifest-path md-crdt-yrs-bench/Cargo.toml --all -- --check` | ok |
| `cargo clippy --manifest-path md-crdt-yrs-bench/Cargo.toml --all-targets --locked -- -D warnings` | ok |

## Phase 2 completion record

**Completed:** 2026-08-04

### Tasks

| ID | Status | Evidence |
| --- | --- | --- |
| 2.1 | done | Middle insert exact string for n∈{8,1k,10k} |
| 2.2 | done | Two restores independent; seed unchanged after sibling mutations |
| 2.3 | done | `ChangeMessage` export; JSON wire encode/decode; two-peer exact sync + delta |
| 2.4 | done | All Tier A manifests (`text_*`) exact post-conditions |

### Design notes

- Seed = `SessionSnapshot` + paragraph `BlockId` (no handle clone).
- `visible_string` uses `block_text_seq` + `paragraph_visible_string` (body only).
- `export_decoded_since` → `encode_changes_since` (not wire).
- Wire: `serde_json::{to_vec,from_slice}` on `ChangeMessage` (`md_crdt_serde_json_v1`).
- `apply_decoded` uses `ValidationLimits::default()` and refreshes `block_id` for empty receivers.

### Gate results

| Gate | Result |
| --- | --- |
| `cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked` | **33 passed** |
| fmt + clippy `-D warnings` on compare crate | ok |

## Phase 3 completion record

**Completed:** 2026-08-04

### Tasks

| ID | Status | Evidence |
| --- | --- | --- |
| 3.1 | done | `YrsAdapter` TextEngine; exact middle insert; frozen Options |
| 3.2 | done | Seed = peer + lib0 v1 update; independent restores |
| 3.3 | done | `encode_state_as_update_v1` / `Update::decode_v1` / `apply_update`; wire + two-peer tests |
| 3.4 | done | `tests/adapter_contract.rs` — same helpers × `MdCrdtAdapter` and `YrsAdapter` |

### Design notes

- `ComparisonAdapter: TextEngine + SyncEngine` with `empty(peer)` for receivers.
- Yrs seed rebuilds a **new** `Doc` with fixed `OffsetKind::Bytes`, `skip_gc = false`, root `"text"`.
- Tier B prep for Yrs: encode then `decode_v1` in setup (`export_decoded_since`).
- One Yrs write transaction per `insert_at` / `delete_at` (keystroke = M transactions).

### Gate results

| Gate | Result |
| --- | --- |
| unit tests | **37 passed** |
| `tests/adapter_contract.rs` | **14 passed** (7×2 engines) |
| fmt + clippy `-D warnings` | ok |

## Phase 4 completion record

**Completed:** 2026-08-04

### Tasks

| ID | Status | Evidence |
| --- | --- | --- |
| 4.1 | done | `[[bench]] name = "compare" harness = false` |
| 4.2 | done | All v1 manifests × both engines; `cargo bench … -- --test` Success |
| 4.3 | done | `BenchmarkId::new("{tier}/{workload}/{engine}", parameter_id)` |
| 4.4 | done | `iter_batched_ref` + `BatchSize::LargeInput`; setup outside timer |
| 4.5 | done | `CaseMetadata` completeness tests + `case_metadata` stderr lines |
| 4.6 | done | README full vs quick; CONTRIBUTING pointer |

### Design notes

- Shared `runners` module used by unit tests and Criterion (DRY).
- Integrate path uses `Option<DecodedUpdate>` + `take` so Yrs non-Clone `Update` works with `iter_batched_ref`.
- Tier C samples wire bytes once for throughput + metadata completeness.
- Product `just bench` unchanged (compare crate excluded from root workspace).

### Gate results

| Gate | Result |
| --- | --- |
| unit + integration tests | **43 + 14 passed** |
| `cargo bench --no-run --locked` | ok |
| `cargo bench --locked -- --test` | all cases Success |
| fmt + clippy `-D warnings` | (run at close-out) |

## Phase 5 completion record

**Completed:** 2026-08-04

### Tasks

| ID | Status | Evidence |
| --- | --- | --- |
| 5.1 | done | Expanded `md-crdt-yrs-bench/README.md` (rules, pins, tiers, out-of-scope) |
| 5.2 | done | “How to interpret results” section (no universal winner; concurrent disclaimer) |
| 5.3 | done | `scripts/run_compare_report.sh`; `PROVENANCE_REQUIRED_KEYS`; fixture tests |
| 5.4 | done | On-demand multi-run path; `report-out/` gitignored; baselines not committed |
| 5.5 | done | CI off-by-default recorded in README |
| 5.6 | done | Note under architecture-evolution Non-Goals item 5 |

### Design notes

- Engine order via `MD_CRDT_COMPARE_ENGINE_ORDER` for alternating multi-run reports.
- Provenance schema enforces ≥3 invocations, unique baselines, alternating order, clean worktree for citability.
- Dry-run mode for schema/script verification without long Criterion runs.

### Gate results

| Gate | Result |
| --- | --- |
| unit + integration tests | **46 + 14 passed** |
| dry-run report script | provenance schema ok |
| fmt + clippy `-D warnings` | ok |

## Phase 6 completion record

**Completed:** 2026-08-04

### Tasks

| ID | Status | Evidence |
| --- | --- | --- |
| 6.1 | done | `multi_peer_fan_in` n=1000 peers∈{4,8}; wire fan-in apply |
| 6.2 | done | `text_paste_middle` n=1000 r∈{256,1024,4096} |
| 6.3 | done | `examples/memory_probe` keeps the live document through sampling; `scripts/memory_rss.sh` builds once and times the probe executable directly / `just memory-compare` |
| 6.4 | done | Criterion group `illustrative_yrs_block_map` (labeled non-competitive) |
| 6.5 | done | `diagnostic_md_crdt_markdown_serialize` + `diagnostic_yrs_text_get_string` |

### Benchmark run (indicative)

- Command: `cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked --bench compare -- --sample-size 25 --measurement-time 1 --warm-up-time 0.3`
- Host: aarch64-apple-darwin (Apple M4 Max)
- Artifacts (gitignored): `md-crdt-yrs-bench/report-out/bench-pairs.md`, `bench-summary-means.txt`, `memory-rss-*.txt`
- **Not S10-citable** (single moderate run; dirty worktree not required for local exploration)

## Plan status

**Phases 0–6 complete.** Competitive suite includes v1 + stretch workloads; diagnostics and memory probes are separate and not ratioed across engines.
