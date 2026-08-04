# md-crdt vs Yrs Competitive Benchmark Plan

**Status:** implemented; Phases 0–6 complete
**Date:** 2026-08-04
**Scope:** unpublished, standalone benchmark workspace that compares controlled shared workloads between `md-crdt` and [Yrs](https://docs.rs/yrs) (Rust Y-CRDT) without adding Yrs to the published library or the root workspace quality path.

### Decisions frozen by this review

- The comparison lives in `md-crdt-yrs-bench/` as a nested standalone workspace and is listed in the root workspace's `exclude`, not `members`. This is the only design that keeps the repository's existing `cargo ... --workspace` checks and `just bench` from resolving and building Yrs. Root `members` stay as they are today; `exclude` is additive.
- The implemented dependency pins are exact: `yrs = "=0.27.3"` and `criterion = "=0.5.1"`. The required Rust 1.85 spike failed for both Yrs (`if let` guards) and current md-crdt (let-chains); the repository MSRV was not raised. Current comparison gates therefore use a toolchain that accepts those features (1.88 or newer in practice), with the discrepancy recorded in the plan state.
- `md-crdt` is consumed with `default-features = false`; the comparison exercises the core collaborative document and sync APIs, not file or storage layers. Remote apply uses `ValidationLimits::default()` only; changing limits is a new scenario.
- Results are split into public text-operation, decoded-integration, and declared wire-pipeline tiers. Cross-tier ratios are forbidden.
- Yrs uses root name `"text"`, client ids `1` and `2`, `OffsetKind::Bytes`, and
  `skip_gc = false`. md-crdt uses peer ids `1` and `2`. Changing any of these is a new
  scenario, not a benchmark-tuning option.
- Baseline data is generated on demand. A cited result comes from a clean worktree and records the
  md-crdt repository commit (which also identifies the in-tree comparison harness), both lockfile
  hashes, the Yrs package version and registry checksum, features, toolchain, target, host, command,
  configuration, and raw Criterion estimates.
- Nested `target/` and Criterion output under the comparison crate are already covered by the root
  gitignore entry `target` (no gitignore change required). Do not commit generated baselines.

Reference basis verified on 2026-08-04:

- [Yrs 0.27.3 crate documentation](https://docs.rs/yrs/0.27.3/yrs/) and
  [`Doc`](https://docs.rs/yrs/0.27.3/yrs/doc/struct.Doc.html) establish that shared types are
  document-scoped and all mutations occur within transactions.
- [`Text`](https://docs.rs/yrs/0.27.3/yrs/types/text/trait.Text.html) documents byte versus UTF-16
  offset behavior.
- Criterion 0.5.1
  [`iter_batched_ref`](https://docs.rs/criterion/0.5.1/criterion/struct.Bencher.html#method.iter_batched_ref)
  is the normative setup-exclusion mechanism for destructive-input probes.

---

## 1. Goals

1. Produce side-by-side Criterion measurements for explicitly specified collaborative workloads on **md-crdt** and **Yrs**, with the compared layer named in every benchmark.
2. Keep competitor dependencies **out of the published `md-crdt` crate** (no `yrs` in root package deps, features, or main `benches/`).
3. Make results **reproducible, labeled, and interpretable** (same sizes, causal histories, API-call and transaction schedules, and setup-outside-timer rules).
4. Follow **TDD** for shared harness logic; keep Criterion probes thin.
5. Apply **SOLID / DRY / KISS**: one scenario contract, two adapters, no dual CRDT engine in production.

## 2. Non-goals

| Non-goal | Reason |
| --- | --- |
| Replace md-crdt’s RGA with Yrs | Already rejected in architecture evolution (“external CRDT abandons in-tree RGA”) |
| Wire interop / Yjs protocol compatibility | Separate product effort |
| Compare markdown-only features 1:1 with Yrs | No equivalent native mapping without biased modeling |
| Fold competitive benches into default `just check` / CI gate | Optional cost; product benches remain the regression gate |
| Publish the comparison crate | `publish = false`, like `md-crdt-naive-oracle` |
| Claim a universal CRDT winner | The libraries expose different document models and wire protocols; conclusions apply only to the named workload and layer |
| Compare md-crdt Markdown serialization with Yrs plain-text materialization | Yrs has no native Markdown serializer, so this would measure different work |

## 3. Success criteria

| # | Criterion | Verification |
| --- | --- | --- |
| S1 | Standalone comparison workspace exists; only its manifest/lockfile contain Yrs, while the root package, workspace members, and root lockfile do not | both manifests, both lockfiles, and root `cargo metadata --locked` review |
| S2 | Shared scenario contract unit-tested before any Criterion wiring | `cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked` |
| S3 | Controlled workloads run for both engines under one Criterion group naming scheme and never mix result tiers | `cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked -- --test` (or full run) |
| S4 | Fixture restoration, wire decoding for decoded-integration probes, and destructive-input setup are outside timed sections | `iter_batched_ref` review + harness tests that count setup vs measured steps |
| S5 | Product `benches/performance.rs` unchanged in role (internal ablation/regression only) | Diff review |
| S6 | `just bench` stays product-only; `just bench-compare` runs the competitive suite | justfile |
| S7 | Documented methodology + pinned Yrs version + size matrix | crate README |
| S8 | Root `just check`, `just bench`, and `cargo test --workspace` do not resolve or build Yrs | root exclusion + root `cargo metadata --locked` package review + command logs |
| S9 | Every scenario freezes logical content, peer ids, operation history, API-call count, and Yrs transaction boundaries | scenario manifest + contract tests |
| S10 | Reported numbers include raw estimates, confidence intervals, wire payload sizes, named codecs, and full provenance; quick-smoke numbers are never cited | report schema + README review |

## 4. Design principles

### 4.1 SOLID

| Principle | Application |
| --- | --- |
| **S**ingle responsibility | Scenarios define *what* to measure; adapters define *how* each library does it; Criterion only drives timing |
| **O**pen/closed | New workloads = new scenario + adapter methods; no edits to unrelated product benches |
| **L**iskov | Both adapters satisfy the same trait contracts with exact sequential text and per-engine replica convergence, not merely equal lengths |
| **I**nterface segregation | Narrow traits: `TextEngine`, `SyncEngine` — not one god trait for every future feature |
| **D**ependency inversion | Criterion benches depend on scenario contracts and generic adapter runners, not concrete Yrs/md-crdt types inline |

### 4.2 DRY

- One size matrix, peer ids, causal history, API-call count, and transaction schedule shared by both engines.
- One setup/teardown helper pattern (restore an immutable seed outside the timer).
- One reporting id scheme: `{tier}/{workload}/{engine}/{parameter}`.
- No copy-paste of fixture sizes between unit tests and benches — constants live once.

### 4.3 KISS

- Only controlled, mappable CRDT workloads in v1; every claim names the abstraction layer.
- Yrs model: plain `Doc` + `Text` (and encode/apply update for sync). No XmlFragment markdown tree unless a later phase explicitly needs it.
- md-crdt model: public `CollaborativeDocument` APIs. Low-level `Sequence` probes remain product-only because Yrs `Text` is document-scoped and transactional.
- No allocation-counting global allocator in the compare crate unless a later phase needs it (product benches already own that complexity).

## 5. Placement and package shape

```text
md-crdt-yrs-bench/                 # publish = false; standalone workspace (resolver = "2")
  Cargo.lock                       # committed exact comparison graph
  Cargo.toml
  README.md                        # methodology, how to run, claim boundaries
  scripts/                         # provenance / multi-run reporting (Phase 5)
  src/
    lib.rs                         # re-exports
    sizes.rs                       # shared N = 1_000 / 10_000, peer counts
    scenario.rs                    # workload definitions (pure data + steps)
    adapter.rs                     # traits
    md_crdt_adapter.rs
    yrs_adapter.rs
    harness.rs                     # setup-outside-timer helpers (testable)
  tests/
    adapter_contract.rs            # TDD: both adapters obey scenario contracts
    equivalence.rs                 # TDD: exact sequential text, histories, convergence, isolation
  benches/
    compare.rs                     # Criterion only; thin wrappers
```

**Root workspace change (minimal but required):**

```toml
[workspace]
members = [
    "md-crdt-ci",
    "md-crdt-ffi",
    "md-crdt-naive-oracle",
]
exclude = ["md-crdt-yrs-bench"]
resolver = "2"
```

The comparison manifest declares its own `[workspace]` table with `resolver = "2"`, matching the
root workspace. Do not add it to root
`members` or try to solve isolation with `default-members`: this repository's `just check`
uses `--workspace`, and root `just bench` would also discover a member benchmark. The standalone
workspace owns its own lockfile and is invoked only through `--manifest-path` or the dedicated
just recipe. Path dependency `md-crdt = { path = ".." }` is resolved as a normal path crate by the
nested workspace (not as a nested-workspace member).

**Dependencies (compare crate only):**

- `md-crdt = { path = "..", default-features = false }`
- `yrs = { version = "=0.27.3", default-features = false }`, subject to the Rust 1.85 compile gate
- `serde = { version = "=1.0.228", features = ["derive"] }` and
  `serde_json = "=1.0.149"` as direct dependencies for the benchmark-defined md-crdt JSON codec
- `criterion = "=0.5.1"` as a dev-dependency, matching the product benchmark harness
- no production coupling back into `md-crdt`

The standalone workspace commits its own lockfile and repeats the root benchmark profile setting:

```toml
[profile.bench]
debug = true
```

Do not add unrelated optimization overrides: both engines must compile under the same profile, and
the profile must remain aligned with `benches/performance.rs` when results are discussed together.

**Just recipes:**

```just
# Full competitive suite (cite only after three full invocations + provenance; see §6.3)
bench-compare:
    cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked

# Liveness only — Criterion --test; never cite timings from this recipe
bench-compare-quick:
    cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked -- --test

# Harness / contract tests for the nested workspace
test-compare:
    cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked

# Three full invocations with alternating order and provenance (citable path)
bench-compare-report:
    md-crdt-yrs-bench/scripts/run_compare_report.sh

# Separate-process RSS probe; interpret each engine independently
memory-compare:
    md-crdt-yrs-bench/scripts/memory_rss.sh
```

**Product surfaces left alone:**

- `benches/performance.rs` — internal regression / ablation only
- root package `dev-dependencies` — do **not** add `yrs` here
- root `Cargo.lock` — must remain free of Yrs and its comparison-only transitive graph

## 6. Comparison and claim model

### 6.1 In scope (v1) — controlled result tiers

No benchmark id may omit its tier. Ratios are valid only between the two engine ids in the same
row, with the same scenario manifest.

#### Tier A: public text operations (layer-inclusive)

These compare the public collaborative-document path each library asks a Rust caller to use. They
are useful product measurements, but not claims that the underlying sequence algorithms perform
identical work: md-crdt maintains a Markdown block and grapheme model, while Yrs maintains a root
`Text` inside a transactional `Doc`.

| Workload id | Timed work | Shared schedule | Parameter(s) |
| --- | --- | --- | --- |
| `text_insert_middle` | One public call inserting `"y"` at N/2 | one md-crdt session call; one Yrs write transaction | N ∈ {1k, 10k} |
| `text_append_run` | One public call appending one M-byte ASCII string | one call and one Yrs write transaction | N ∈ {1k, 10k}, M ∈ {32, 256} |
| `text_append_keystrokes` | M one-byte appends | M calls; Yrs opens and commits one transaction per call | N ∈ {1k, 10k}, M ∈ {32, 256} |
| `text_delete_middle` | One public call deleting one unit at N/2 | one md-crdt session call; one Yrs write transaction | N ∈ {1k, 10k} |

#### Tier B: decoded update integration

`integrate_decoded_update` starts from an engine-native decoded update prepared outside the timer:
`ChangeMessage` for md-crdt and `yrs::Update` for Yrs. Only integration and commit are timed. The
target is restored outside the timer for every iteration. This isolates integration from each
protocol's decoding cost. It has two manifest variants: `history=full,n=N` applies a full update to
an empty target, while `history=delta,n=N,k=K` applies K lagging edits to a target restored at the
captured base state. The source and target state vectors must be asserted before timing.

**Preparation asymmetry (setup only, not timed):** md-crdt may obtain `ChangeMessage` directly via
`CollaborativeDocument::encode_changes_since` (in-memory selection; no JSON). Yrs exposes updates as
lib0 bytes first, so setup encodes then `Update::decode_v1` before the timer. Both still time only
`apply_decoded` / integrate+commit. Do not move either engine's preparation into the timed section
to "balance" setup.

#### Tier C: declared wire pipeline

These are end-to-end protocol measurements, not pure CRDT-algorithm comparisons. The comparison
crate defines `md_crdt_serde_json_v1` as `serde_json::to_vec(ChangeMessage)` /
`serde_json::from_slice`; this is a benchmark transport, not a claim that md-crdt has standardized
JSON as its product wire format. Yrs uses its `yrs_lib0_v1` pipeline through
`encode_state_as_update_v1` / `Update::decode_v1`. Every result names its codec and reports payload
bytes beside time. Ratios describe these complete pipelines only; they do not establish intrinsic
protocol efficiency.

| Workload id | Timed work | Parameter(s) |
| --- | --- | --- |
| `wire_encode_full` | Select and serialize everything unknown to an empty state vector | fixed history with N visible units |
| `wire_encode_delta` | Select and serialize K one-character edits after a captured base vector | N ∈ {1k, 10k}, K ∈ {1, 100} |
| `wire_decode_apply` | Decode prebuilt codec bytes and apply them to a restored target | `history=full,n=N` and `history=delta,n=N,k=K` |
| `two_peer_round_trip` | Encode, decode, and apply both peers' one-edit concurrent deltas | fixed two-peer schedule |

Tier B uses the same N/K histories as Tier C but consumes decoded updates. For
`two_peer_round_trip`, the manifest fixes this schedule: seed peer 1, synchronize the base to peer
2, capture both pre-concurrency state vectors, and have peer 1 insert `"a"` and peer 2 insert `"b"`
at N/2 in one API call/transaction each, all outside the timer. The timed routine encodes each
peer's delta against the other peer's captured vector, decodes both byte buffers, and applies the
remote update to each peer. Post-timing checks require per-engine convergence and one occurrence of
each unique marker.

### 6.2 Explicitly out of scope for competitive claims (v1)

Product-only benches stay in `benches/performance.rs` and are **not** claimed as Yrs wins/losses:

- `workspace_hierarchy`, `workspace_projection`, `workspace_edit_replay`
- `table_cell_edit`, `structured_workspace_edit`
- `checkpoint_history`, block-index ablations, markdown parser locality
- `sequence_incremental` feature comparisons (md-crdt-internal)

Also excluded from competitive claims:

- `visible_export`: md-crdt Markdown structural serialization and Yrs `Text::get_string` do not
  produce the same representation or perform the same work.
- direct `Sequence` versus Yrs `Text`: the former is a low-level in-tree primitive and the latter
  is document-scoped and transactional.

If useful later, a **Phase 6 “modeled Yrs”** appendix may implement a Yrs Map/Xml sketch of blocks — labeled **illustrative only**, never as an equivalent-engine comparison.

### 6.3 Methodology rules (must land in README + tests)

1. **Scenario manifest:** every case records visible N, edit payload, peer ids, base-history construction,
   edit count, API-call count, Yrs transaction count, lag vector, tier, codec, and Criterion batch
   policy.
2. **Same logical content:** both engines start with one text container holding N ASCII `x` bytes and
   use the same unique edit payloads. md-crdt's required paragraph operation is recorded as model
   overhead, not hidden. **`visible_string` is the text-container body only** (md-crdt: ordered
   visible graphemes of the single seeded paragraph; Yrs: root `Text` string). It is **not**
   Markdown structural serialization and must not include document chrome beyond that body.
3. **Frozen history:** seed N with one bulk-insert API call and one Yrs transaction. Build delta lag
   K with K one-byte API calls and K Yrs transactions. Never compare states that merely have equal
   final text but were produced by different histories.
4. **Fixed identities and options:** use peer/client ids `1` and `2`; use Yrs root name `"text"`,
   `OffsetKind::Bytes`, `skip_gc = false`, no default features, and lib0 v1 encoding. Record these
   values in every report.
5. **Explicit transaction boundaries:** each Yrs adapter method owns the transaction named by the
   scenario. The keystroke case commits M transactions; the run case commits one. Do not hold an
   undocumented transaction across benchmark calls.
6. **Setup outside timer:** use Criterion `iter_batched_ref` with `BatchSize::LargeInput` for
   destructive cases so seed/target restoration and Tier-B update decoding are excluded. Read-only
   encode cases may reuse an immutable source but still use a batched routine so returned byte
   buffers are dropped outside the timer. Do not hand-roll `Instant` loops in the comparison crate.
   A memory-driven batch-policy change must apply to both engine ids in that scenario and be recorded
   as a distinct run configuration.
7. **Drop and output policy:** `iter_batched_ref` keeps destructive input drop outside the measured
   routine; pass observable results through `black_box`. Apply the same output/drop treatment to both
   adapters.
8. **Correctness before timing:** sequential scenarios assert exact visible strings, not only lengths.
   Concurrent scenarios assert convergence within each engine and exactly one occurrence of each
   peer's unique marker; cross-engine concurrent ordering may differ and is not compared.
9. **Decoded versus wire separation:** Tier B excludes decoding for both engines. Tier C includes
   the declared serialization/decoding pipeline for both. `CollaborativeDocument::encode_changes_since` alone is
   not a wire-encode result because it returns an in-memory `ChangeMessage` whose operation payloads
   were encoded earlier.
10. **Throughput labels:** use `Elements` for logical operations and `Bytes` only for actual
    byte buffers. Report operation count, transaction count, and payload length as separate metadata.
11. **No mixed configuration:** both engine cases for one benchmark run use the same build profile,
    target, CPU affinity/power conditions, warm-up, sample size, and measurement duration.
12. **No smoke-test claims:** `--test` and short-duration runs prove liveness only. A cited result uses
    at least three independent full invocations with engine registration order alternated by the
    reporting script, reports Criterion estimates/confidence intervals and between-run spread, and
    does not declare a winner when host variance is comparable to the observed gap.
13. **Full provenance:** require a clean worktree and record the md-crdt repository commit, both
    lockfile hashes, Yrs version and registry checksum, dependency versions, features, `rustc -Vv`,
    target triple, OS, CPU model, power mode, command, batch policy, and timestamp.
14. **Raw evidence retained:** give each full invocation a unique Criterion `--save-baseline` name
    and retain its machine-readable estimates plus the provenance sidecar before aggregation.
    Generated `target/criterion` output is not committed by default.

### 6.4 Char / grapheme caveat

- md-crdt is grapheme-oriented in public text APIs.
- Yrs 0.27.3 supports byte or UTF-16 offsets and defaults to bytes; the adapter fixes
  `OffsetKind::Bytes` explicitly.
- **v1 rule:** use ASCII-only fixtures (`x`, `y`) so grapheme, Unicode scalar, UTF-16 unit, and byte offsets coincide.
- Non-ASCII comparison is a later optional matrix, not a v1 blocker.

## 7. Architecture sketch

```text
                    ┌─────────────────────────┐
                    │  ScenarioManifest       │
                    │  (tier, history, ids)   │
                    └───────────┬─────────────┘
                                │
              ┌─────────────────┴─────────────────┐
              ▼                                   ▼
     ┌────────────────┐                 ┌────────────────┐
     │ MdCrdtAdapter  │                 │ YrsAdapter     │
     │ : TextEngine   │                 │ : TextEngine   │
     │ : SyncEngine   │                 │ : SyncEngine   │
     └────────┬───────┘                 └────────┬───────┘
              │                                   │
              └─────────────────┬─────────────────┘
                                ▼
                     ┌────────────────────────────┐
                     │  harness (batched setup /  │
                     │  measure for Criterion)    │
                     └─────────────┬──────────────┘
                                   ▼
                     ┌────────────────────┐
                     │ benches/compare.rs │  Criterion groups
                     └────────────────────┘
```

### 7.1 Core traits (KISS, narrow)

```rust
/// Minimal public text document used by controlled scenarios.
pub trait TextEngine: Sized {
    type Seed: Clone;

    fn seed(peer: u64, text: &str) -> Self::Seed;
    fn restore(seed: &Self::Seed) -> Self;
    fn insert_at(&mut self, index: usize, text: &str);
    fn delete_at(&mut self, index: usize, len: usize);
    fn visible_len(&self) -> usize;
    /// Text-container body only (see methodology rule 2); not Markdown serialize.
    fn visible_string(&self) -> String;
}

/// Native state-vector, decoded-integration, and wire operations.
pub trait SyncEngine {
    type StateVector;
    type DecodedUpdate;

    fn state_vector(&self) -> Self::StateVector;
    /// Tier B prep: engine-native decoded update (no timed decode).
    /// md-crdt: `ChangeMessage` from `encode_changes_since`.
    /// Yrs: `Update` after setup-only lib0 decode of an encoded state/diff.
    fn export_decoded_since(&self, sv: &Self::StateVector) -> Self::DecodedUpdate;
    /// Tier C: declared codec bytes (`md_crdt_serde_json_v1` or `yrs_lib0_v1`).
    fn encode_wire_since(&self, sv: &Self::StateVector) -> Vec<u8>;
    fn decode_wire(bytes: &[u8]) -> Self::DecodedUpdate;
    fn apply_decoded(&mut self, update: Self::DecodedUpdate);
}
```

Do not add `Clone` to an engine merely for iteration isolation: a handle clone may alias the same
document store and make samples mutate each other. `Seed` is the explicit independent-rebuild
contract. For md-crdt it is a `SessionSnapshot` (already `Clone`) plus the paragraph `BlockId`
obtained from `insert_paragraph` / `block_id_from_op`; for Yrs it is a full lib0 v1 update plus the
fixed root name and options. Exact fallible return types may adjust during the compile spike
(public session APIs return `Result`), but the **contract tests and tier boundaries** own the
semantics. Benches may `expect` only after contracts prove the ASCII scenarios are infallible.

### 7.2 Scenario as data + runner (DRY)

```rust
pub struct SizeMatrix {
    pub text_lens: &'static [usize], // [1_000, 10_000]
    pub append_lens: &'static [usize], // [32, 256]
    pub delta_lags: &'static [usize], // [1, 100]
}

pub enum Workload {
    TextInsertMiddle { n: usize },
    TextAppendRun { n: usize, m: usize },
    TextAppendKeystrokes { n: usize, m: usize },
    // ...
}
```

Each workload expands to an immutable `ScenarioManifest` containing its tier and complete history /
transaction schedule. Unit tests run workloads through adapters for **correctness of the harness**
(not CRDT differential equality with an oracle). Competitive benches call the same runners with
Criterion.

## 8. TDD workflow (per phase)

For every harness/adapter increment:

1. **Red:** write a failing unit/integration test that states the contract (for example, a middle
   insert must equal `"x".repeat(N / 2) + "y" + &"x".repeat(N - N / 2)`, not merely have length
   N+1).
2. **Green:** implement the smallest adapter/scenario code that passes.
3. **Refactor:** extract shared constants, remove duplication, keep Criterion files thin.
4. Only after contracts are green: wire Criterion group(s), use Criterion's `--test` mode for a
   liveness smoke, then run the full suite when reporting.

Criterion is **not** the unit test. Harness bugs must fail
`cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml` without running full benches.

## 9. Phases and tasks

### Phase 0 — Decisions and scaffolding — **done** (2026-08-04)

**Objective:** package exists, workspace wires cleanly, no product pollution.

| ID | Task | TDD / gate | Done when |
| --- | --- | --- | --- |
| 0.1 | Create standalone `md-crdt-yrs-bench` workspace with `publish = false`, its own lockfile, edition 2024, Rust 1.85, and workspace resolver 2 | Review | Manifest contains both `[package]` and `[workspace]`; resolver matches root |
| 0.2 | Install MSRV if needed; compile-spike exact `yrs = "=0.27.3"` on Rust 1.85; stop and record a newer/older compatible pin if incompatible (do not raise repo MSRV silently) | `rustup install 1.85.0` as needed; `cargo +1.85.0 check --manifest-path md-crdt-yrs-bench/Cargo.toml --locked` after lock exists | Exact compatible pin committed; repository MSRV unchanged |
| 0.3 | Add crate skeleton + empty `lib.rs` + `README.md` methodology stub | `cargo check --manifest-path md-crdt-yrs-bench/Cargo.toml` | Compiles |
| 0.4 | Add root `workspace.exclude = ["md-crdt-yrs-bench"]` **alongside existing `members`**; never add it to `members` | Root `cargo test --workspace` and `just bench` do not build Yrs; `cargo metadata --locked` shows no yrs | Isolation is structural, not convention-based |
| 0.5 | Add `just test-compare`, `just bench-compare`, and `just bench-compare-quick` (`--test` liveness only) | Manual | Documented in crate README |
| 0.6 | Confirm root package dependencies and root lockfile are unchanged by comparison dependencies | manifest/lock diff + root `cargo metadata --locked` | No `yrs` in the root dependency graph |
| 0.7 | Commit the comparison lockfile; check advisories and inventory licenses | Global flag form: `cargo deny --manifest-path md-crdt-yrs-bench/Cargo.toml check advisories` and `cargo deny --manifest-path md-crdt-yrs-bench/Cargo.toml list --format json` | No advisory is ignored silently; SPDX inventory is recorded and any unlicensed/unknown package blocks completion pending explicit review |
| 0.8 | Match the comparison `[profile.bench]` to the root product benchmark profile | manifest assertion/review | Both engines and product comparisons use the documented profile |
| 0.9 | Format/lint gate for nested crate (fmt + clippy on comparison manifest) | Commands in §11 | Green on skeleton |

The repository currently has no root `deny.toml`; therefore `cargo deny check licenses` is not a valid
green gate until an explicit project license allowlist is approved. The inventory in 0.7 is
deliberate and must not be represented as an enforced allowlist. `cargo deny`'s `--manifest-path`
is a **global** option (before the subcommand), not a `check` flag. The comparison crate may carry
its own `deny.toml` for **explicit** advisory acceptances (see plan state).

**Exit criteria:** standalone crate checks; root package dependency metadata and lockfile unchanged;
root exclusion and just recipe present.

**Completion notes:** all 0.1–0.9 done. Pin `yrs = "=0.27.3"` retained; Rust 1.85 spike fails for both
yrs (`if let` guards) and path-dep md-crdt (let-chains)—repository MSRV field not raised. Advisory
`RUSTSEC-2026-0215` (`smallstr` via yrs) explicitly accepted in `md-crdt-yrs-bench/deny.toml`.
Details: [`yrs-bench-compare-plan-state.md`](./yrs-bench-compare-plan-state.md).

---

### Phase 1 — Shared contracts and size matrix (TDD first) — **done** (2026-08-04)

**Objective:** pure shared definitions with tests; no Criterion yet required.

| ID | Task | TDD | Done when |
| --- | --- | --- | --- |
| 1.1 | `sizes.rs`: text lengths `[1_000, 10_000]`, append lengths `[32, 256]`, delta lags `[1, 100]`, fixed peer ids, ASCII fillers | Unit tests for constants / helpers | Single source of truth |
| 1.2 | Define `TextEngine` + `SyncEngine` traits in `adapter.rs` | Compile-only generic-runner smoke | Traits documented; no object-safety requirement |
| 1.3 | `scenario.rs`: workload enums + `ScenarioManifest` histories, calls, transactions, tier, codec, exact expected text | Unit tests for every manifest and exact sequential outcome | No implicit schedule |
| 1.4 | `harness.rs`: setup and measured closures consumed by batched Criterion routines with `BatchSize::LargeInput` | Call-order/count spy + destructive-input isolation test | Restore/decode setup cannot enter measured closure; outputs drop afterward |

**Exit criteria:** comparison-manifest tests are green for pure modules; adapters may still be stubs.

**Completion notes:** 32 frozen v1 manifests; exact sequential expectations; concurrent markers for
`two_peer_round_trip`; harness `run_batched_iteration` + call-order spy. Real adapters still Phase 2/3.
Details: [`yrs-bench-compare-plan-state.md`](./yrs-bench-compare-plan-state.md).

---

### Phase 2 — md-crdt adapter (TDD) — **done** (2026-08-04)

**Objective:** md-crdt side satisfies contracts using public APIs.

| ID | Task | TDD | Done when |
| --- | --- | --- | --- |
| 2.1 | Implement `MdCrdtAdapter` for `TextEngine` via `CollaborativeDocument` | Tests: create with N×`x`, insert middle `y`, assert the exact expected string | Public API only |
| 2.2 | Session-seed restore path (`SessionSnapshot` + paragraph `BlockId`) for per-iteration isolation | Test: two restores are independent; mutating one leaves seed and sibling unchanged | No handle-clone shortcut |
| 2.3 | Implement `export_decoded_since` (`ChangeMessage`), JSON wire encode/decode, and `apply_decoded` via `apply_remote` + `ValidationLimits::default()` | Two-peer exact-content, convergence, decode/apply, and payload round-trip tests | `ChangeMessage` selection is not mislabeled as byte encoding |
| 2.4 | Delete + append paths (`delete_text`, append via `insert_text` at end) | Contract tests for each workload’s post-condition | All text workloads green on md-crdt |

**API choice notes:**

- Use `CollaborativeDocument` for session-realistic keystroke/sync paths.
- Seed construction: `insert_paragraph(None, &"x".repeat(N))`, store `block_id_from_op` of the
  returned elem id, then `save_snapshot()` for the immutable seed bytes/state.
- `visible_string`: concatenate visible graphemes of that paragraph only (not full-document
  Markdown serialize).
- Do not reach into `Sequence` in v1. Adding a low-level cross-engine tier requires a separate plan
  amendment with APIs that perform equivalent work; it cannot be introduced as a fallback for a
  missing public equivalent.

**Exit criteria:** all Phase 1 contracts pass against `MdCrdtAdapter`.

**Completion notes:** `md_crdt_adapter.rs` implements both traits; `empty(peer)` receivers refresh
`block_id` after apply; wire codec is `serde_json` over `ChangeMessage`. All Tier A manifests pass
exact post-conditions. Details: [`yrs-bench-compare-plan-state.md`](./yrs-bench-compare-plan-state.md).

---

### Phase 3 — Yrs adapter (TDD) — **done** (2026-08-04)

**Objective:** Yrs side satisfies the **same** contracts.

| ID | Task | TDD | Done when |
| --- | --- | --- | --- |
| 3.1 | Implement `YrsAdapter` `TextEngine` with `yrs::Doc` + `Text` | Same contract tests as 2.1 parameterized over engines | ASCII middle insert works |
| 3.2 | Restore Yrs from a full lib0 v1 seed update into a new fixed-option `Doc`; do not use `Doc`/handle clone | Isolation test analogous to 2.2 | Per-iteration independence |
| 3.3 | Implement `SyncEngine` with `encode_state_as_update_v1`, `Update::decode_v1`, and `apply_update` | Same exact-content and convergence matrix as 2.3 | Tier B and C boundaries preserved |
| 3.4 | Parameterize `tests/adapter_contract.rs` over **both** engines | One test table × two engines | DRY contract matrix green |

**Exit criteria:** comparison-manifest tests are fully green for both engines on all v1 workloads.

**Completion notes:** `yrs_adapter.rs` freezes `OffsetKind::Bytes`, `skip_gc = false`, root `"text"`.
Seed = peer + full lib0 v1 update into a new `Doc`. `ComparisonAdapter` shared by both engines.
`tests/adapter_contract.rs` runs the full contract matrix twice. Details:
[`yrs-bench-compare-plan-state.md`](./yrs-bench-compare-plan-state.md).

---

### Phase 4 — Criterion competitive suite — **done** (2026-08-04)

**Objective:** thin Criterion surface; reportable side-by-side groups.

| ID | Task | TDD / gate | Done when |
| --- | --- | --- | --- |
| 4.1 | Add `[[bench]] name = "compare" harness = false` | `cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --no-run` | Builds |
| 4.2 | Wire groups for each tier × workload × engine × complete parameter id | Smoke: `--test`; short sample is liveness-only | Completes without panic |
| 4.3 | Naming: `BenchmarkId::new("{tier}/{workload}/{engine}", manifest_id)` | Review | Criterion HTML makes cross-tier comparison difficult to do accidentally |
| 4.4 | Use batched Criterion routines via harness helpers; apply identical batch size, `black_box`, and output/drop policy | Code review against Phase 1.4 | No restore, Tier-B decode, input drop, or output drop inside timed region |
| 4.5 | Emit wire payload bytes, codec name, logical operations, API calls, and transaction counts per case into report metadata | Contract test for metadata completeness | Timing never appears without work-size context |
| 4.6 | Document how to run full vs quick compare in crate README + root CONTRIBUTING pointer (one line) | Doc review | Discoverable |

**Exit criteria:** competitive bench target runs; product `just bench` unchanged.

**Completion notes:** `benches/compare.rs` + `runners` + `report::CaseMetadata`;
`iter_batched_ref` + `BatchSize::LargeInput`; smoke `--test` green for all 64 cases
(32 manifests × 2 engines). Details: [`yrs-bench-compare-plan-state.md`](./yrs-bench-compare-plan-state.md).

---

### Phase 5 — Documentation, reporting, hardening — **done** (2026-08-04)

**Objective:** review-ready results story and maintenance path.

| ID | Task | Done when |
| --- | --- | --- |
| 5.1 | Complete crate README: scenario rules, ASCII caveat, claim boundaries, out-of-scope product features, and version pins | Self-contained for external readers |
| 5.2 | Add “How to interpret results” (tier boundaries, model overhead, protocol differences, noise, confidence intervals) | No universal-winner language; explicit concurrent-order disclaimer |
| 5.3 | Add a deterministic script under `md-crdt-yrs-bench/scripts/` to require a clean worktree, run three full invocations with alternating engine order and unique `--save-baseline` names, then emit Markdown plus machine-readable provenance | Output fixture test; every S10 field required and no run overwrites another |
| 5.4 | Capture at least three full invocations on a labeled machine when numbers are cited (not CI-gated) | Raw artifact bundle retained externally or attached; baseline files not committed by default |
| 5.5 | Record the frozen CI policy: **off by default**; optional nightly/manual workflow later | Written decision in README |
| 5.6 | Brief note in `docs/architecture-evolution.md` or plan-state: “competitive Yrs bench crate exists; product engine remains in-tree” | Cross-link only |

**Exit criteria:** reviewers can run and interpret without reading implementation chat history.

**Completion notes:** Full README; `scripts/run_compare_report.sh` + `just bench-compare-report`;
`report::validate_provenance_document` + fixture tests; `MD_CRDT_COMPARE_ENGINE_ORDER`;
`report-out/` gitignored; architecture-evolution cross-link. Details:
[`yrs-bench-compare-plan-state.md`](./yrs-bench-compare-plan-state.md).

---

### Phase 6 (optional stretch) — Extended workloads — **done** (2026-08-04)

Implemented after Phases 0–5 were accepted.

| ID | Workload | Notes |
| --- | --- | --- |
| 6.1 | Multi-peer fan-in (P peers, interleaved inserts) | Stress sync more than single keystroke |
| 6.2 | Large paste (insert run of length R at middle) | Controlled single-call/single-transaction comparison |
| 6.3 | Memory / RSS snapshots via external tools | Not Criterion; separate process per engine because allocators and global state cannot share one measurement |
| 6.4 | Illustrative Yrs “block map” model | **Not** competitive claims; labeled synthetic |
| 6.5 | Output materialization diagnostics | md-crdt Markdown serialization and Yrs plain text shown separately, never ratioed |

**Completion notes:** `text_paste_middle` + `multi_peer_fan_in` in competitive suite;
`examples/memory_probe` + `scripts/memory_rss.sh`; Criterion groups
`diagnostic_*` and `illustrative_yrs_block_map` (never ratioed). Details in plan state.

## 10. Mapping to existing product benches

Use product benches as **inspiration for sizes and patterns**, not as a place to inject Yrs.

| Product probe (`benches/performance.rs`) | Competitive analogue | Notes |
| --- | --- | --- |
| `session_insert_text` | `text_insert_middle` | Closest controlled public-API comparison; model overhead remains visible |
| `nested_text_insert` / `sequence_insert_middle` | none | md-crdt low-level diagnostics only; Yrs Text includes document/transaction machinery |
| `state_vector` | none in v1 | State vectors are untimed scenario inputs; add a peer-count matrix later only with a distinct question |
| `encode_changes_since` | Tier C `wire_encode_full` / `wire_encode_delta` | Comparison adds actual md-crdt JSON container serialization; payload models differ and bytes are mandatory |
| `document_serialize` | none in v1 | Structural Markdown versus plain text is not ratioed |
| workspace / table / structured groups | none | Product-only |

## 11. Verification gates

### Per phase

```bash
# Harness / contracts
cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked

# Compile competitive benches without full timing
cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --no-run --locked

# Quick competitive smoke; never cite these numbers
cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked -- --test

# Comparison lint/format gate
cargo fmt --manifest-path md-crdt-yrs-bench/Cargo.toml --all -- --check
cargo clippy --manifest-path md-crdt-yrs-bench/Cargo.toml --all-targets -- -D warnings

# Product path still clean
just check
just bench   # still root md-crdt only because the comparison is excluded
```

### Definition of done (whole effort)

1. Phases 0–6 complete.
2. Both adapters green on contract tests.
3. Criterion compare target runs for all v1 workloads and every id contains its tier.
4. No `yrs` dependency or transitive package in the root workspace graph/lockfile.
5. Exact-content, isolation, convergence, scenario-manifest, and report-schema tests pass.
6. README methodology and interpretation guidance are self-contained.
7. Root product benches and CI quality gate behavior are unchanged.
8. Any cited result satisfies S10; no performance threshold or preferred winner is required for completion.

## 12. Risk register

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Inequivalent text models (grapheme vs UTF units) | Misleading “winner” | ASCII-only v1; document caveat |
| md-crdt session overhead vs bare Yrs Text | Session path looks slower for reasons outside CRDT core | Keep Tier A explicitly layer-inclusive; make no underlying-algorithm claim; defer any low-level tier to a separately approved design |
| Encode payload contents differ (JSON ops vs Yrs binary) | Byte size not comparable as pure “efficiency” | Report times and bytes separately; avoid claiming wire superiority without protocol context |
| Seed restoration cost leaks into timer | Inflated or noisy results | Harness tests + code review of timed sections |
| Handle cloning aliases the same Yrs store | Cross-iteration contamination and falsely cheap setup | No `Clone` engine bound; rebuild independent docs from immutable seed bytes |
| Same final text, different causal histories | Sync and memory results measure different work | Frozen scenario manifest with operation and transaction counts |
| md-crdt message selection compared with Yrs binary encoding | Invalid encode-time ratio | Serialize md-crdt `ChangeMessage` in Tier C; keep decoded integration separate in Tier B |
| Output/drop asymmetry | One engine pays destructor or allocation cost the other avoids | Shared `iter_batched_ref` and `black_box` policy reviewed per workload |
| Workspace CI time / dependency bloat | Slower clones/builds | Standalone nested workspace + root `exclude`; comparison absent from `just check` |
| Scenario drift from product benches | Harder narrative | Shared size constants; doc table in §10 |
| Yrs API churn or MSRV increase | Broken adapter or accidental policy change | Exact pin + committed comparison lockfile; the failed Rust 1.85 spike is recorded and current gates use 1.88+; upgrades are explicit |
| Host drift / thermal throttling | Spurious winner | Three full invocations, provenance, confidence intervals, stable power/CPU conditions |

## 13. Implementation order (summary)

```text
Phase 0  Scaffold standalone workspace + root exclusion + exact/MSRV-checked pins  [done]
    ↓
Phase 1  Traits, sizes, pure scenario expectations, harness (TDD)  [done]
    ↓
Phase 2  MdCrdtAdapter (TDD contracts green)  [done]
    ↓
Phase 3  YrsAdapter + parameterized contract matrix (TDD)  [done]
    ↓
Phase 4  Criterion compare.rs (thin; smoke then full)  [done]
    ↓
Phase 5  Docs, reporting guidance, CI policy  [done]
    ↓
Phase 6  Optional extensions  [done]
```

## 14. Review checklist (resolved)

- [x] Crate isolation: `md-crdt-yrs-bench` is a standalone nested workspace excluded from root.
- [x] V1 workloads: public text operations, decoded integration, and declared wire-pipeline tiers only.
- [x] Offset rule: ASCII fixtures with explicit Yrs `OffsetKind::Bytes`.
- [x] Identity/configuration rule: root `"text"`, ids `1`/`2`, and Yrs `skip_gc = false`.
- [x] Claim boundary: public document paths are layer-inclusive; wire protocols are labeled and
  cross-tier ratios are forbidden.
- [x] CI policy: off by default; root quality and product benchmark commands remain unchanged.
- [x] Phase 6 extensions are implemented and remain segregated where comparisons would be invalid.
- [x] Baseline numbers are generated on demand; cited results retain raw evidence and provenance.
- [x] Comparison features: `md-crdt` default features disabled; Yrs default features disabled.
- [x] `visible_string` is text-container body only (not Markdown serialize).
- [x] Tier B prep asymmetry (md-crdt native `ChangeMessage` vs Yrs setup decode) is documented and untimed.
- [x] `SyncEngine` includes `export_decoded_since` for Tier B prep.
- [x] `cargo deny --manifest-path ...` uses the global option form; the local `deny.toml` records the explicit advisory acceptance, while licenses remain inventory-only until a project license allowlist is approved.
- [x] Root public APIs needed for the md-crdt adapter exist in-tree.

## 15. Change-control decisions

The following require explicit review before implementation diverges from this plan:

1. adding a root-workspace member or any Yrs dependency to the root package/lockfile;
2. changing the exact Yrs or Criterion pin, the repository MSRV, Yrs offset/GC configuration, or
   md-crdt feature set;
3. adding a benchmark that lacks a tier or complete `ScenarioManifest`;
4. timing fixture restoration in one engine but not the other;
5. publishing a ratio across tiers or between unlike output representations;
6. committing generated baseline data rather than attaching a provenance-complete artifact.

## 16. Implementation status

| Gate | Status |
| --- | --- |
| Isolation design (`exclude` + nested workspace + own lockfile) | Frozen |
| Claim model (tiers A/B/C, no cross-tier ratios) | Frozen |
| v1 workload set and size matrix | Frozen |
| Phase 6 competitive extensions and non-ratioed probes | Verified in tree 2026-08-04 |
| TDD order (contracts before Criterion) | Frozen |
| Exact pins + MSRV compile spike as first build step | Frozen (Phase 0.2) |
| Public APIs exist for md-crdt path (`CollaborativeDocument`, `insert_text` / `delete_text`, snapshot restore, `encode_changes_since` / `apply_remote`, `ChangeMessage: Serialize`) | Verified in tree 2026-08-04 |
| Criterion setup exclusion mechanism (`iter_batched_ref`) | Documented against 0.5.1 |
| Root quality path remains free of Yrs | Required by S1/S8; enforced by exclude |
| Remaining open product questions | None blocking; change-control in §15 |

**Verdict:** Phases 0–6 are implemented and verified. The Rust 1.85 compile spike failed for both the exact Yrs pin and current md-crdt language features; the declared repository MSRV was not changed. Competitive large-paste and fan-in workloads are complete, while diagnostic, illustrative, and memory probes remain explicitly non-ratioed. Detailed evidence and current commands live in [`yrs-bench-compare-plan-state.md`](./yrs-bench-compare-plan-state.md).

---

## Appendix A — Suggested Criterion group layout

```text
compare/
  public_text/text_insert_middle/md_crdt/n=1000
  public_text/text_insert_middle/yrs/n=1000
  public_text/text_append_run/md_crdt/n=1000,m=32
  public_text/text_append_keystrokes/yrs/n=1000,m=32
  decoded_integration/integrate_decoded_update/...
  wire_pipeline/wire_encode_full/...
  wire_pipeline/wire_encode_delta/...
  wire_pipeline/wire_decode_apply/...
  wire_pipeline/two_peer_round_trip/...
```

## Appendix B — Relation to naive-oracle

| Crate | Purpose | Competitor / reference | Gate |
| --- | --- | --- | --- |
| `md-crdt-naive-oracle` | Correctness differential for Sequence | In-tree naive model | `just differential-test` |
| `md-crdt-yrs-bench` | Controlled cross-engine performance comparison | External `yrs` | `just bench-compare` (explicit, standalone) |
| `benches/performance.rs` | Internal regression / ablation | None | `just bench` |

These remain separate: **correctness ≠ competitive performance**.

## Appendix C — Historical implementation slices

The implementation was designed around these independently green slices:

1. **PR-A:** Phase 0 + Phase 1 (scaffold + pure contracts)
2. **PR-B:** Phase 2 + Phase 3 (both adapters + contract tests)
3. **PR-C:** Phase 4 + Phase 5 (Criterion + docs)

Each PR must leave `just check` green and must not add `yrs` to the published package.
