# Rust standards conformance — md-crdt

Governing source: the live `Software-Standards/Rust Code Standards.md`,
High-Performance Rust Coding Standards v3.3.1, checked October 31, 2026.
Documentation corrections also use the live Technical Markdown Style Guide v1.1.0
(reviewed October 2, 2026). The vendored Rust v3.3 document remains an unchanged
historical copy. Dated measurements below describe their frozen overlays, not
verification of today's wire V7/session V10 source.

## Current automation policy

At the user's request on October 2, 2026, the GitHub `CI` workflow is disabled
until explicitly re-enabled. Local verification remains required. CI enforcement
of `RPS-004`, `RPS-290` and `RPS-293`, and scheduled fuzz/beta runs, is temporarily
suspended by this project decision; workflow definitions remain available.
Revisit this suspension when the user requests CI again.

Benchmark CI and a dedicated runner are not project requirements. Continue manual
compiler/profile measurements when relevant; runner provisioning is not an open
action item (`RPS-022`, `RPS-025`, `RPS-297`).

## Configuration and enforcement

- Edition 2024, MSRV 1.95, production pin 1.99.0 (`RPS-001`–`RPS-009`).
- Workspace lint inheritance includes performance/suspicious lints, ownership
  checks and async guard checks. The configured pinned Clippy gate denies warnings;
  a gate definition is not evidence of a current successful run (`RPS-289`, `RPS-299`).
- The paused CI workflow defines formatting, pinned Clippy/tests, explicit MSRV tests with `--locked`,
  release-mode tests, and dependency licenses/advisories (`RPS-290`–`RPS-296`).
- The disabled workflow defines scheduled/manual fuzz jobs using cargo-fuzz 0.13.2
  and nightly-2026-10-01, with fixed input/time/RSS limits and failure-artifact upload.
  These definitions are not active scheduled sanitizer coverage. Beta release tests
  are defined as non-blocking; nightly is tooling only.
- `fuzz/Cargo.toml` registers `snapshot_restore`; registration alone is not execution
  evidence. It is not in the disabled workflow's fuzz matrix. The bounded local
  snapshot smoke below is distinct from the named historical smoke campaigns.
- Public external Markdown boundaries validate bytes, lexical nesting and inline
  syntax work before parsing. Storage reads and alignment tables have allocation
  limits. `Parser::parse` remains a trusted-input API; external callers use
  `try_parse` (`RPS-028`, `RPS-049`, `RPS-203`).

## Performance evidence and reproduction

Compiler upgrades require the complete benchmark suite, allocation probes and
binary-size comparison (`RPS-262`, `RPS-307`). The host measurement runner is
`md-mcp/tools/measure_rust_upgrade.py`; it records both repositories' source-file
hashes, dirty-tree identity, lockfiles, exact compilers, activated features,
production profiles, commands, Criterion confidence estimates and raw logs.
It also measures three independent MCP process runs and independent-arrival
load, including scheduling delay, errors, disconnections and uncompleted work
(`RPS-017`, `RPS-018`, `RPS-310`).

Run from md-mcp, without concurrent builds or load:

```sh
python3 tools/measure_rust_upgrade.py --md-crdt ../md-crdt --output /tmp/rust-upgrade-evidence
```

Do not treat a smoke run as timing evidence, or compare a library benchmark
profile directly to a host binary's timings. The compiler comparison retains
identical source and profile settings within each project. Benchmarks are
manual controlled-host measurements; shared CI never gates wall-clock deltas.
Keep the generated report and logs with the release record. Manual measurements
are the selected project workflow; no dedicated benchmark runner is required
(`RPS-022`, `RPS-025`, `RPS-297`).

## Scope and continuing review

Safe Rust remains the product baseline. Benchmark allocator probes have local
SAFETY explanations. Defined fuzz targets exercise externally driven parsing, mutation,
decoding and snapshot restore, but CI is paused and a target's presence is not evidence
of running it under a sanitizer. Miri is not a substitute for system tests.

Duplicate development/proc-macro dependency versions are reviewed rather than
force-unified across incompatible APIs (`RPS-268`). The consumer chooses its
release profile; library manifests cannot impose one on downstream hosts.
Performance reports must identify the actual package, features and profile.

The user-directed CI suspension is recorded above. Any additional waiver requires
measurements, tradeoffs, an owner and review date from the governing standard.


## Library-specific controls

The disabled CI feature-matrix definition lists minimal, storage, filesync, default,
`sequence_incremental` and all-feature configurations; it does not currently execute them.
Feature-dependent tests/doctests are enabled with their APIs; the workspace benchmark
declares `required-features = ["filesync"]`.

The release profile uses opt-level 3, fat LTO, one codegen unit, abort-on-panic,
line-table debug info, unstripped symbols and explicit wrapping overflow.
The benchmark profile retains its declared debug setting. Profile experiments
must be reported separately from the compiler-only comparison.

`lcs_steps` returns a typed work-limit error before allocating a quadratic table.
Ingestion builds a candidate session, so a late work-budget refusal does not
partially change the live session. Regression tests exercise this rollback.
The unmeasured `inline(always)` directives on disabled performance counters were
replaced by ordinary `inline`; no speed claim is made for that change.

## Current wire V7/session V10 contract and evidence

Current source uses wire V7 (`src/codec/wire.rs`) and snapshot V10
(`src/session/snapshot.rs`). This is a clean incompatible cutover: older envelopes
and snapshots report typed version/reinitialize errors rather than receiving missing-field
defaults. Back up and export unpublished state with a compatible binary before
reinitializing from authoritative Markdown; the [README cutover policy](../README.md)
also requires exclusive storage ownership and generation-aware writers.

Required durable state includes block/list-item placement and deletion topology,
creation-operation incarnations, causal structured-register candidates, independent
frontmatter base/field registers and initializer clocks, and source-local frontmatter
revision provenance. `BlockTopology` owns metadata rather than a second body tree.
Deletion captures the target and visible incarnation-qualified descendants at the
author's causal cut: captured concurrent escapes die, observed completed escapes
survive, and unseen incoming outsiders use historical viable placement fallback.
Late initial births into deleted origins do not survive by escaping; an unrelated
incoming move is not such a birth. A fresh incarnation does not inherit old moves
or let stale aliases refer to it (`src/doc/topology.rs`, `src/session/mod.rs`).

`checkpoint_history` only invokes transport-log checkpointing; it does not prune
the document's full placement/deletion topology, origin metadata or structural
tombstones. Retention is not bounded by live content. Neither `max_retained_ops`
nor the pending byte charge establishes a bound on snapshot size or process RSS.

Generic-codec snapshot saves normalize applied, pending and deferred payloads to
native JSON; restore/rebase constructs a Json session without changing the original
live codec or its payloads (`OpCodec::snapshot_payload`, `save_snapshot`,
`restore_from_snapshot`). Session admission and snapshot validation both enforce
100,000 pending operations and 64 MiB logical charge conjunctively. Caller limits
can tighten but not raise those ceilings, and accounting includes normalized snapshot
bytes and nested pending state (`src/limits.rs`, `src/session/snapshot.rs`).

Frontmatter initializer LWW bases do not overwrite independent explicit per-key
writes/deletions; fields received before initialization remain fields. Opaque bases
can hide, but do not discard, supported remote registers. Source adoption is not
semantic base replication: opaque adopted Exact source may also hide durable remote
registers while local structured edits are refused. Source-revision provenance survives
snapshot/reopen (`src/doc/frontmatter.rs`, `src/doc/source.rs`,
`Document::adopt_source_from`).

### Observed native correctness run

The coordinator's local November 1 run recorded this exact command:

```sh
env CARGO_TARGET_DIR=/tmp/md-crdt-cycle2-rqqa5sde/targets/integrated-correctness-debug CARGO_BUILD_JOBS=3 cargo +1.99.0 test --offline --locked --workspace --no-fail-fast
```

Evidence is local, not committed publication:
`/tmp/md-crdt-cycle2-rqqa5sde/evidence/run-69oiyaeg/` contains `invocation.json`,
`summary.json`, `stdout.log` and `stderr.log`. The command returned 101.
Selected complete target results from those logs are:

| Target | Passed | Failed |
| :--- | ---: | ---: |
| Library unit tests | 155 | 0 |
| `codec_snapshot_council_regressions` | 20 | 0 |
| `logical_container_movement` | 22 | 0 |
| `session_council_regressions` | 24 | 0 |
| `frontmatter_initialization_convergence` | 6 | 0 |
| `filesync_ingest` | 26 | 0 |
| `vault_council_regressions` | 16 | 0 |

The failing targets were `codec_binary_golden`, `document_council_regressions`,
`filesync_export_state` and `workspace_projection_transcript`. This record does not
claim a workspace pass or an aggregate passed-test total. It is evidence for that
invocation, not for subsequent source edits, current all-feature/MSRV/Clippy success,
sanitizer execution, power-loss safety, or any performance improvement.

### Bounded snapshot-restore sanitizer smoke

The wire V7/snapshot V10 correctness overlay, before acceptance of any new
performance candidate, completed **1,000 executions** of `snapshot_restore`
with cargo-fuzz and nightly-2026-10-01. The command used `-runs=1000`,
`-max_len=4096`, `-rss_limit_mb=2048` and `-timeout=30`, with an external corpus
seeded from `fuzz/seeds/snapshot_restore/materialized.md`. The complete invocation
and logs are local evidence in
`/tmp/md-crdt-cycle2-rqqa5sde/evidence/run-tgjk8_5t/`; libFuzzer reported
`Done 1000 runs in 122 second(s)`, exit zero. The resulting executable contains
`__asan_init` and `__asan_report_load1` instrumentation/runtime symbols.

The target exercises parsed materialization, valid and mutated snapshot bytes,
pending/deferred recovery with late dependencies, checkpointing and editable
rebase continuations. This bounded AddressSanitizer smoke is not a long campaign,
proof of memory safety, active CI coverage, or verification of later source edits.

### Measured complete debug ordering reference

The accepted debug-only reference rebuild borrows all retained metadata, sorts
and traverses independently of the incremental index, then compares every stored
ID. Tombstones, assertion invocation points, default dev opt-level 0, overflow
checks and the unchanged 9,999-edit boundary remain intact.

Three fresh processes per variant, pinned to CPU 0, produced these mean
untraced times at workload size 512. No builds, tests or proof workloads overlapped
sampling. O is the original full reference, I removes only its redundant initial
index, and B is the complete borrowed rebuild; I is an attribution precursor,
not an additive shipping component.

| Workload | O (ms) | I (ms) | B (ms) |
| --- | --- | --- | --- |
| Mixed sequence | 10413.1 | 8944.9 | 2712.2 |
| Six-grapheme edits | 5135.2 | 4298.2 | 420.4 |
| Open/replay | 5171.8 | 4348.4 | 494.7 |
| Flush I/O control | 170.0 | 173.5 | 168.4 |

Flush has no full-oracle calls and no improvement is claimed. Independent A/A
processes showed at most 0.53% drift outside flush I/O; traced runs retained exactly
2,048 mixed-sequence and 512 edit/replay oracle calls at size 512. An independent
same-source rebuild produced a byte-identical untraced baseline executable.

Local source and raw-result identities are recorded in
`local://md-crdt-cycle2-debug-OIB-analysis-c84e.json` and
`local://md-crdt-cycle2-accepted-debug-reference-c84e.json`. Native ordering tests
include exhaustive fixture permutations, deep/wide layouts and rejection of
wrong order, unrooted components and duplicate IDs.

The unchanged all-features debug boundary passed in
`/tmp/md-crdt-cycle2-rqqa5sde/evidence/run-2cmraaea/`. That run overlapped other
candidate builds: it proves completion and correctness, not isolated boundary
latency. The integrated root example also passed (`artifact://522`).
These are local invocation records, not committed measurement publication,
complete current MSRV/feature-matrix verification, or general performance claims.

### Measured paired Vault snapshot and replay load

Vault lazy loading shares one validated snapshot selection with generation-aware
operation replay. Healthy loads read a snapshot payload once rather than three
times; corrupt-newest fallback reads two payloads rather than four. These counts
come from syscall attribution, not inferred timing. Retired replay remains
generation-fenced, independent APIs revalidate separately, and no proof survives
the externally serialized call.

The wire V7/snapshot V10 study used Rust 1.99.0 release opt-level 3, the default
CLI/filesync/storage features, CPU 0 affinity, 30 samples, one-second warmup and
three-second measurement windows. Sampling did not overlap builds, tests or
proof workloads. Four original-control, three helper-only and three paired-load
processes covered all 80 cases, retaining all 800 estimates. An independent
same-source control rebuild produced a byte-identical executable.

| Targeted workload | Median latency reduction | Range across 16 cases |
| --- | --- | --- |
| Vault open and lazy load | 16.0% | 4.6–21.3% |
| Lazy load with already-open Vault | 15.8% | 5.9–21.8% |

Every paired process was faster than every original control in all 32 targeted
cases. Turning pairing off while retaining the helper extraction removed the
gain. This accepts the named Vault-load optimization, not a universal speedup.

Adverse standalone observations remain: mixed-process medians were 0.9–1.3%
slower, with 1 MiB fallback means 11.6–23.6% slower and pronounced process modes.
Single-fixture checks were near parity for snapshot/segment reads; a sequential
read A/A control varied 86%. Fixed libc mmap/heap-policy diagnostics with the same
binaries reduced fallback differences to roughly 0–1%, while slowing both variants
absolutely. This supports allocation-history dependence, not proof that every
default mixed workflow is regression-free. No allocator tuning is shipped or
recommended. Criterion within-process confidence intervals do not represent
between-process uncertainty; the process means, ranges and empirical bootstrap
intervals are retained, with small-sample and single-host limits.

The integrated source passed 57 explicit native recovery/publication records,
43 storage unit regressions, 10 durability and three Vault regressions, and all
11 public-consumer records including valid-fence export/reopen. Complete local
source/binary identities, component results, adverse observations and exercised
commands are indexed by
`local://md-crdt-cycle2-accepted-storage-pairing-c84e.json`.

### Measured restore proof and borrowed snapshot-save preparation

Restore reuses a private owned validation/decoded-envelope proof. Public mutable
DTOs always validate freshly; normalization clears the proof and fully revalidates
the changed DTO. Topology, incarnation, deletion, identity, receipt-frontier,
codec and durable count/byte guards remain enabled. Snapshot construction borrows
applied transport payloads until canonicalization, then owns only the final JSON.

The 37-case matched release study retained 481 estimates: four baseline processes
and three each for full, restore-only and save-only variants. It used the same
CPU-0/sample/window protocol as the storage study above, with no overlapping
builds, tests or proof runs. DTO timings use Criterion `iter_batched` with
`SmallInput`, excluding input cloning and output destruction; they describe that
controlled batch workload, not isolated end-user latency.

| DTO fixture: units / held | Baseline restore (µs) | Full candidate (µs) | Baseline tracked peak (KiB) | Candidate tracked peak (KiB) |
| --- | --- | --- | --- | --- |
| 100 / 16 pending | 138.5 | 79.8 | 175.5 | 175.5 |
| 10,000 / 128 pending | 1936.5 | 1472.4 | 4473.3 | 4473.3 |
| 100 / 16 deferred | 142.6 | 84.0 | 175.0 | 245.9 |
| 10,000 / 128 deferred | 1991.6 | 1541.6 | 4478.5 | 5038.1 |

The native memory probe tracks a freshly cloned owned DTO plus restore, without
resetting live accounting when the input is consumed. It does not measure
whole-process heap, RSS or physical realloc overlap. All seven fixtures retained
the same bytes after restore; pending cases had no peak increase. The disclosed
deferred peak is a transient tradeoff, not a memory improvement. Fourteen native
memory records and 56 Criterion test-mode cases passed across the matched pair.

Reproduce the owned-input memory observation separately from statistical sampling:

```sh
MD_CRDT_BENCH_GROUPS=cycle2_restore MD_CRDT_RESTORE_INPUT_MEMORY=1 \
  cargo bench --locked --bench council_performance -- --test '^cycle2_restore/'
```

The probe starts tracking before cloning the owned DTO and keeps that input's live
accounting through restore. Benchmark fixture validation and these allocation
records are outside Criterion's timed loops.

Adding restore proof to the save-only control reduced held DTO times 22.8–42.6%
and held native-byte restore times 2.4–8.0%. Save-only also shifted several
unchanged decode/non-held timings, so raw baseline-to-full differences in those
paths are not all attributed to proof reuse. Small empty DTO/decode controls had
1–3% adverse observations; no universal CPU or memory win is claimed.

| Largest borrowed-prefix save metric | Baseline | Full candidate |
| --- | --- | --- |
| Controlled save time (µs) | 308.5 | 180.1 |
| Requested allocation traffic (bytes) | 5538319 | 3339799 |
| Peak-live-byte delta | 4296019 | 3339799 |
| Allocation calls | 1013 | 1010 |

Native-JSON allocation counters were unchanged in all three fixtures; JSON save
latency is not claimed improved. An initial fallible-iterator collection added
40 bytes of capacity in native JSON cases and was rejected in favor of an
explicit exact-size output vector. Historical adverse results remain retained.

Integrated all-features runs passed 20 codec/snapshot, 22 movement and 21 session
snapshot regressions, including fresh mutable-DTO validation, normalization,
late-dependency replay and typed identity rejection. Actual public-consumer
programs passed 11 continuation and four contract records. Source and binary
identities, independent component results, memory scope and complete commands are
indexed by `local://md-crdt-cycle2-accepted-snapshot-proof-save-c84e.json`.
These isolated-stage checks are supplemented by the final integrated checks
below; neither substitutes independent review.

### Measured exact diff and nested mutable access

The final performance pass used independently rebuilt A-A controls, four baseline
processes and three processes per candidate/component. Repeated statistics cover
16 predefined diff cases per kernel/refresh scope and 25 structural cases;
full native correctness/allocation coverage separately covers 165 cases per diff
scope and 304 structural cases. Statistical sampling did not overlap builds,
tests or native proofs. These are selected workloads on one host, not universal
latency guarantees.

The accepted diff preserves the original complete traceback, including repeated
grapheme ties. Native proofs compare 30,770 finite pairs and 16 newly admitted long
cases against the original recurrence. All original successful inputs remain
supported in the measured matrix: kernel success increases from 99 to 159 cases,
and refresh success from 105 to 159, out of 165 each. Six dissimilar cases remain
bounded refusals. A refusal becoming success is a new admission, not a speedup.

| Comparable kernel case | Original (µs) | Accepted diff (µs) |
| --- | --- | --- |
| Unique start insertion, 16 graphemes | 1.489 | 0.100 |
| Repeated middle deletion, 16 graphemes | 1.045 | 0.091 |
| Unicode middle substitution, 128 graphemes | 35.839 | 9.990 |
| Unique end substitution, 1,000 graphemes | 3670.411 | 3681.985 |
| Dissimilar, 1,000 graphemes | 2237.240 | 2355.347 |

The 1,000-grapheme dissimilar kernel is 5.3% slower. A 50,000-grapheme dissimilar
input now spends about 1.31 ms attempting bounded alignment instead of an immediate
shape refusal. Refresh A-A spreads reached 26.7% in selected cases; no general
refresh speedup is established. The optimized path shares one cumulative
4,000,000-work-unit and 16,000,000-requested-byte budget across output and all
attempted bands. Original dense admissions retain their existing cell bound.

Only indexed mutable container access (`P4b`) was accepted from the structural
components. It preserves incarnation resolution, conservative invalidation,
public nested replacement behavior and full structural history.

| History-preserving mutation case | Original (ms) | P4b only (ms) |
| --- | --- | --- |
| Cold quote, 64 children, 256 moves | 21.434 | 16.540 |
| Warm item, 64 children, 256 moves | 26.634 | 25.532 |

Other construction/mutation results include overlapping and adverse process
ranges. Existing indexes remain expensive for one cold lookup: the selected large
list cold lookup was about 14.58 ms versus 0.31 ms in the scoped linear control.
Warm repeated lookup and mutation favor indexing; this study does not establish
an adaptive crossover policy or a global no-index replacement.

The redundant-anchor change (`P4a`) had no established repeatable material gain.
Borrowed physical lookup paths (`P5`) removed an allocation and improved warm
physical lookup, but the P4b+P5 combination slowed large list content mutation
9.9% and quote content mutation 6.2%; P5 was not shipped. Frontmatter A/B
candidates improved adopted/same-value rendering but slowed edited CRLF cases
12.6–18.1%; neither rendering component was shipped. Allocation savings alone
were not acceptance evidence.

The complete process estimates, component states, source/binary identities,
adverse results and decisions are indexed by
`local://md-crdt-cycle2-phase2-performance-decisions-c84e.json`.

### Persistence and publication evidence limits

Persistence/publication code uses file sync and Unix directory sync around replacement;
the non-Unix directory-sync helpers are no-ops (`src/storage/mod.rs`,
`src/filesync/session.rs`). Local fault-injection and reopen/recovery regressions
exercise selected interruption boundaries and preservation of acknowledged state.
They do not establish actual power-loss behavior, device cache guarantees, every
filesystem's ordering semantics, or cross-platform durability equivalence.

Performance decisions above retain rejected candidates and the limits of the
isolated measurements. Integrated feature/MSRV/quality checks and the fresh
council are separate gates; a candidate's native proof is not their completion.

### Final November 2 integrated local verification

- Rust 1.99.0 default workspace: 857 tests passed across 89 result summaries.
  All features: 863 passed, with three explicitly manual DHAT cases ignored.
  The unchanged `six_grapheme_batch_below_payload_limit_flushes_and_reopens`
  boundary case passed with full incremental-debug verification.
- After semantically equivalent lint cleanup, 60 focused document, movement,
  index and codec/snapshot regressions passed. The actual debug workload ran at
  size 128 with debug assertions enabled; no elapsed smoke time is a performance claim.
- MSRV 1.95 checked every workspace target with all features; minimal and
  storage-only library runs passed 89 and 132 tests. Filesync without CLI checked.
  The excluded comparison workspace passed 65 opt-in `sub_probes` tests and
  an all-target MSRV check. Its real attribution executable reported nonzero
  apply/decode/validation activity; these are not competitive Yrs ratios.
- Pinned all-target/all-feature Clippy passed with warnings denied. Formatting
  checks passed for the main, fuzz and excluded comparison workspaces.
  Offline `cargo deny --locked check` passed advisories, bans, licenses and sources;
  advisory freshness is limited to the available local database.
- Fresh native executables exercised 57 storage recovery/publication records,
  all 165 kernel and 165 refresh diff cases, the complete tie proofs, 16 structural
  history fixtures in both cold/warm regimes, seven owned-input memory records
  and 28 restore test-mode cases. Native smoke elapsed time is not statistical evidence.
- Real public-consumer programs passed 11 continuation and four contract records.
  Fresh CLI processes initialized, ingested, synced an external Unicode edit,
  reopened without duplicate work and preserved mode `0640`. `status` reports
  tracking, not external content differences; `sync` detected the edit and exited 2,
  followed by clean exit 0 on the next process.
- The integrated ASan snapshot run reached `INITED=469`, `DONE=1000`; after
  lint-only changes, the final replay reached `INITED=DONE=585`. The latter did
  not perform new mutation iterations despite requesting `-runs=100`.
  Defined `__asan_init`, load and store report symbols were verified in the
  statically linked runtime. Subsequent fuzz formatting changed layout only.
  These bounded runs are neither a long fuzz campaign nor memory-safety proof.

Complete commands, captures, failed-then-corrected gates and verification limits
are retained in `local://md-crdt-cycle2-final-integrated-verification-c84e.json`.
They do not establish complete standards conformance, independent council
coverage, active CI, power-loss durability or publication of local evidence.

## Historical host audit closure

The [Rust 1.99.0 measurement record](../../md-mcp/docs/performance/rust-1.99.0.md)
is a separate host record for compiler, allocation, binary-size, overload and
release-profile results, including unfavorable results. Its publication status
is not established by this library's local invocations. It records the manual-host
choice for `RPS-297` and the separate, uncited Yrs research scope; dedicated CI
hardware is not claimed to exist. The dispositions below are historical and do
not certify the current correction overlay.

| Rule group | Evidence / disposition |
| --- | --- |
| Toolchain and MSRV | Pinned 1.99.0; dated host checks below; current overlay MSRV checks are listed above; CI paused |
| Input work and memory | Bounded external Markdown, frames, artifact reads and diff tables; refusal/rollback tests |
| Async and cancellation | Host admission before blocking work; permit retained through cancellation; transport request limit |
| Feature and quality gates | Explicit matrix and gate definitions; dated executed checks below are overlay-specific |
| Fuzz and sanitizer coverage | CI paused; only the specifically named historical targets ran in this host audit; current snapshot evidence is listed above |
| Performance provenance | Separate host measurement record and local overlay artifacts; local invocation is not committed publication or a current performance acceptance |
| Dependency coherence | Historical host audit reports an exact Git revision check outside the local Cargo path override; not a current overlay verification |
| Unsafe optimizations, custom allocators, SIMD/PGO | No alternate production allocator, SIMD, or PGO accepted; isolated benchmark probes use a `System`-forwarding `GlobalAlloc` wrapper with documented unsafe forwarding |

## Initial October 31 local correction evidence

This historical record describes the initial correction overlay, before the subsequent
wire V6/session V9 causal-register/admission corrections and today's V7/V10 contract.
It distinguishes behavioral verification from performance evidence, not complete standards
conformance or the independent council's coverage report. CI remains paused.
The current contract/evidence section supersedes current-policy claims, not these measurements.

Local verification exercised:

- `cargo test --workspace --locked` and the default workspace on MSRV 1.95.
- Minimal, storage-only, filesync-without-CLI, `sequence_incremental`, and
  all-feature workspace tests; the nested benchmark workspace with `sub_probes`.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
- Release-mode core, session, document, codec/snapshot, mark, and vault regressions.
- `cargo deny --locked check`: advisories, bans, licenses, and sources passed.
- ASan fuzz smoke with cargo-fuzz 0.13.2 and nightly-2026-10-01: parser,
  semantic `apply_changes`, and `decode_binary_wire`. These bounded smoke runs
  are not long-running fuzz campaigns, Miri evidence, or proof of memory safety.
- A separate public-API executable: Unicode frontmatter, editing after reopen,
  late three-peer dependencies, a 120,000-character durable paste, heading
  reconciliation, and refusal of an escaping recovery journal.
- Fresh CLI processes: initialize, ingest, external edit, sync, reopen/sync
  without duplicate operations, and preservation of a pre-existing `0640` mode.
  Status exit 1 for untracked input and sync exit 2 for emitted operations are
  successful observations of the CLI's documented status contract.
- Competitive-report diagnostic mode recorded an explicit `RUSTFLAGS` override,
  profiles, inherited Cargo configuration, and resolved feature graph. Its three
  invocations were `dry_run`; validation reported `citable=false`. No new
  md-crdt/Yrs competitive timing claim is made.
- The release-mode `sub_probes ... apply` executable printed nonzero
  `apply_integrate` counts and time for both workload sizes. The dependency-aware
  drain retains this consumer-visible phase instrumentation; exact-span wiring
  and table-wording assertions are not treated as behavioral regression tests.

At this stage the generation-aware persistence change retained session JSON V8.
The subsequent historical causal-register correction used V9; today's format is V10.
See README for the current exclusive-writer and backup/export-before-reinitialization
policy. Logical retained-byte admission is not a measured RSS cap.

### Measurement protocol

`benches/council_performance.rs` contains reproducible Criterion kernels and
allocator probes. The before implementation is committed
`45ab2c463811862b1733280a0d464500000a14b5`; the after implementation is the
isolated local correction overlay. Both used rustc 1.99.0 on this Linux
aarch64/Cortex-X925 host, default features, opt-level 3, fat LTO, one codegen
unit, and the declared bench debug setting. There were no explicit Rust flag
or Cargo profile environment overrides in these measurements.

```sh
cargo bench --locked --bench council_performance -- \
  --sample-size 20 --measurement-time 2 --warm-up-time 1 --nresamples 10000
```

Measurements were sequential, without concurrent builds during sampling.
The main series did not pin CPU affinity or frequency and is not a production
tail-latency qualification. Allocator probes report requested bytes and maximum
live-byte delta, not RSS; their identical allocator wrapper also participates
in the timed harness. Confidence estimates, source/lock/harness hashes, raw
logs, and experiment identities are retained in the local council evidence.
The baseline and candidate harness sources differ only in rustfmt layout.
Small deltas should not be treated as portable wins.

### Observed medians and decisions

Main kernel medians, in microseconds; a negative change is faster:

| Workload | Before µs | After µs | Change |
| --- | ---: | ---: | ---: |
| Exact render, 5,000 blocks | 5539.896 | 443.982 | -92.0% |
| Table parse, 100×20 | 259.972 | 260.565 | +0.2% |
| Table render, 100×20 | 290.039 | 91.275 | -68.5% |
| Inline parse, 256 repeated spans | 42022.530 | 677.521 | -98.4% |
| Direct insert, 128 graphemes | 1559.430 | 66.695 | -95.7% |
| Concurrent subtree insert, 5,000 elements | 191034.289 | 173.359 | -99.9% |
| Owned insert, 64 KiB payload | 3.311 | 0.084 | -97.5% |
| LCS, 1,000×1,000 | 1993.373 | 1904.913 | -4.4% |
| Snapshot encode, 5,000 characters | 5379.351 | 6648.229 | +23.6% |
| Mark spans, 10,000 units / 64 intervals | 4232.902 | 65.503 | -98.5% |
| Mark serialization, 64 intervals | 23.244 | 17.896 | -23.0% |

Selected consumer-path medians from the existing `performance` harness:

| Workspace projection | Before µs | After µs | Change |
| --- | ---: | ---: | ---: |
| owned_semantic, 10,000 blocks / 32 selected | 753.037 | 104.684 | -86.1% |
| owned_minimal, 10,000 blocks / 32 selected | 114.156 | 42.192 | -63.0% |
| selected_exact_slice_page, 10,000 blocks / 32 selected | 104.978 | 9.158 | -91.3% |

The full before run selected all `workspace_projection` cases; the after run
selected these same three cases plus sequence-middle insertion, session insertion,
and table-cell updates. It used 10 samples, one-second measurement/warmup, and
1,000 resamples. Unchanged or unfavorable cases are retained, not discarded:

| Workload / experiment | Before µs | After µs |
| --- | ---: | ---: |
| Sequence middle insert, 1,000 elements | 7.099 | 7.448 |
| Sequence middle insert, 10,000 elements | 104.946 | 107.414 |
| Per-cell table update, 100×20 | 0.086 | 0.100 |
| Row-vector table update, 100×20 | 0.355 | 0.373 |
| Dense inline render, unpinned | 789.841 | 860.855 |
| Dense inline render, paired CPU-0 control | 1750.294 | 1794.132 |

These observations do not establish a universal regression or a portable win.
The additional inline control pinned both revisions to CPU 0 and used 30 samples,
three-second measurement, one-second warmup, and 10,000 resamples. CPU placement
materially changes absolute timings on this heterogeneous host. The proposed
sparse inline renderer and its suffix-buffer variant failed the improvement gate
and were removed; the original dense rendering algorithm remains. The faster
boundary-correct inline parser and core mark-span sweep are separate changes.
No inline-rendering speedup is claimed.

### Leave-one-change-out ablations

Each control starts from the same candidate source and restores only the named
original implementation. The toolchain, harness, features, and profile stay the
same; medians are microseconds. This distinguishes an observed algorithm effect
from a bundled before/after change. The LCS delta is small and needs stronger host
repetition before making a general throughput claim.

| Reverted component in control | Candidate µs | Control µs |
| --- | ---: | ---: |
| Restore repeated ancestry scans | 173.359 | 201795.581 |
| Restore linear source-ordinal searches | 443.982 | 5692.147 |
| Restore separate LCS row allocations | 1904.913 | 2010.870 |

### Allocation evidence and explicit tradeoffs

| Operation | Before peak live-byte delta | After peak live-byte delta |
| --- | ---: | ---: |
| Snapshot encode, 5,000 characters | 5,626,383 | 450,606 |
| Table render, 100×20 | 484,107 | 54,792 |
| Inline parse, 256 repeated spans | 1,435,800 | 1,479,584 |

Snapshot streaming is retained for bounded serialization and lower peak
allocation, despite its higher measured encoding CPU cost. Inline parsing trades
a small peak-allocation increase for the measured reduction in repeated prefix
scans and boundary-correct grapheme mapping. Table parsing at the measured size
has no material throughput improvement; its dimension/cell limits are safety
controls, not a speed claim. Do not infer a bounded process RSS from these probes.

Minimal-consumer normal dependency graphs, counted including the root package:

| Configuration | Original packages | Corrected packages |
| --- | ---: | ---: |
| `--no-default-features`, normal edges only | 38 | 22 |

This verifies the CLI/UUID dependency cutover. It is not a measured cold-build
speedup or binary-size reduction. Reproduce with
`cargo tree --locked --no-default-features --edges normal --prefix none`.

Local artifacts for this invocation are `council-fix-performance-baseline-a91c.json`,
`council-fix-performance-selected-a91c.json`, and
`council-fix-performance-consumer-a91c.json` under the council session's `local://`
namespace. The selected record preserves full confidence estimates and the
rejected refinement; the consumer record preserves the pinned follow-up. Capture
and retain fresh source hashes, raw logs, flags, profiles, and host metadata with
any release measurement rather than citing these small kernels as an SLA.

## Historical November 1 wire V6/session V9 correction evidence (b72e)

This record covers a frozen local correction overlay on committed
`45ab2c463811862b1733280a0d464500000a14b5`, not another committed release.
Its format was wire V6/session V9, not the current V7/V10 format. The b72e
measurements and checks remain evidence for that historical overlay only.
CI remains paused; they do not establish current-overlay verification or exhaustive
standards conformance. Preserve unpublished work before the current incompatible cutover.

### Behavioral and compatibility verification

| Frozen integrated-source configuration | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Rust 1.95.0 default workspace | 794 | 0 | 0 |
| Rust 1.99.0 minimal, no default features | 548 | 0 | 0 |
| Rust 1.99.0 storage-only | 589 | 0 | 0 |
| Rust 1.99.0 filesync without CLI | 782 | 0 | 0 |
| Rust 1.99.0 all-feature release workspace | 795 | 0 | 3 |

Counts include reported doctests. The three ignored all-feature tests are
existing manual DHAT runs; they were not executed. The all-feature debug
workspace command timed out after 1,800 seconds in the 9,999-edit vault
regression. Its `sequence_incremental` debug path rebuilds an ordering oracle
after each ready insertion. Keep that incomplete run separate from the
successful complete release run; no large test or debug assertion was removed.

The root all-target/all-feature Clippy gate, including the additional `just lint`
warnings, passed. The nested `sub_probes` suite passed 65 tests; its Clippy gate
passed after deleting an incidental default-codec assertion rather than pinning
its wording or replacing it with another default assertion. Root, nested, and
fuzz formatting checks passed. `cargo deny --locked check` passed advisories,
bans, licenses, and sources, with unmatched-license-allowance and duplicate-`syn`
warnings retained in the evidence.

Actual public-API executables exercised source-preserving move/edit/export;
all structured-register kinds under delivery permutations and checkpoint/restore;
formatting through split/merge; a 300-paragraph, 94,688-byte one-word refresh
preserving identities; cap-one nested dependency delivery in both orders,
same-message delivery, and restoration; direct/deferred integration counts;
and 9,999 six-grapheme edits (9,966,801 payload bytes) through flush/reopen.
A standalone executable derived from consumer regressions also exercised
deleting imported-pending containers, checkpointing while dependencies were
held, dependency-plus-follower delivery at capacity, atomic refusals, and
eventual zero pending debt. This reuses those scenarios, not independent test coverage.
Fresh CLI processes exercised init/ingest/flush, external edit/sync, clean
reopen without duplicate operations, and preservation of mode `0640`.

Bounded AddressSanitizer smoke runs used cargo-fuzz 0.13.2 and
nightly-2026-10-01: 10,000 executions each of parser, semantic apply, and binary
wire decoding. Binary seeds were generated and round-tripped with the real V6
encoder. These runs preceded the final restore-normalization change and are
not snapshot-restore fuzz coverage, a long campaign, or proof of memory safety.
Nightly dependency future-incompatibility warnings for `bytecheck`/`rkyv`
remain disclosed. No instruction-count or RSS-bound claim is made.

### Matched measurement protocol

The before source is the preceding frozen local overlay, not bare commit
`45ab2c4`. The after source includes the causal-state/admission repairs.
Both use identical saved harness bytes from `council_performance.rs` and
`performance.rs`; ordinary repository rustfmt layout is not substituted into
one side only. Allocation wrappers and fixture assertions are shared.

Measurements use Rust 1.99.0, CPU-0 affinity on the same Linux aarch64 host,
the unchanged optimized bench profile, 30 samples, three-second measurement,
one-second warmup, and 10,000 resamples. Builds and checks finish before
sampling. Affinity is not CPU isolation or frequency control. Requested
allocation bytes and peak-live-byte deltas are not process RSS.

Six leave-one-component-out controls retain the other corrections while
restoring DTO-based pending counting, repeated held-envelope sizing, the old
matcher, separate-call snapshot validation, scan-based item lookups, or
duplicated item-index paths. The storage control's combined-call adapter exists
only in the experimental export. Lookup bypass retains candidate index
construction when another lookup needs it; it is not full index removal.
Cold construction and warm lookup are measured separately.

The initial independent before/A-A rebuild produced identical executable bytes,
but storage wall-time medians still varied by up to 20.62%; that noise
characterization used the earlier harness revision and is not an additional
matched before/after pair. Small storage latency deltas cannot establish a win.
Hardware instruction counters were unavailable (`perf_event_paranoid=4`);
no host settings were changed to bypass that restriction.

### Measured outcomes and adverse regimes

Values below are arithmetic means from Criterion estimates, not its default
displayed regression slope. Empty-delivery rows time eight calls per iteration.
These are named kernels, not application SLAs or a universal speedup.

| Kernel | Before mean µs | Candidate mean µs |
| --- | ---: | ---: |
| Empty delivery with document pending state, 100k text units, eight calls | 45026.824 | 0.897 |
| Empty delivery with 16 held 4-KiB envelopes, eight calls | 259.684 | 7.158 |
| One-word matching refresh, 128 common-token paragraphs | 11994.049 | 90.244 |
| Warm logical-item lookup, 10k-item list | 66.898 | 0.135 |

Restored held-envelope state showed the same targeted benefit: its eight-call
mean was 7.144 µs; the repeated-sizing control was 260.636 µs. Accounting is
rebuilt on restore and uses actual retained raw bytes after checkpoint pruning.

| Leave-one-component-out kernel | Candidate mean µs | Reverted control mean µs |
| --- | ---: | ---: |
| Document pending accounting, 100k units, eight empty calls | 0.897 | 38536.583 |
| Held-envelope accounting, 16 live envelopes, eight empty calls | 7.158 | 261.424 |
| Common-token matching, 128 paragraphs | 90.244 | 11922.853 |
| Compaction, 8-MiB payload per slot | 66623.220 | 72770.852 |
| Warm logical-item lookup, 10k-item list | 0.135 | 132.087 |
| Warm logical-item lookup, compact versus duplicated-path maps | 0.135 | 0.106 |

The compaction latency difference is not a robust win given the observed
between-run storage variation. Its retained benefit is fewer payload reads and
lower allocation traffic. Dependency-delivery latency is likewise not claimed
as improved by borrowed accounting; the dominant sequence work and run variation
outweigh that small component in this kernel.

Allocation probes below cover one operation, not the eight-call timing batch.
They report total requested bytes, not retained heap, process RSS, or a peak cap.

| Allocation probe | Before/control requested bytes | Candidate requested bytes |
| --- | ---: | ---: |
| One empty delivery with document pending state, 100k units | 8900816 | 0 |
| Common-token matching, 128 paragraphs | 1660016 | 49240 |
| Compaction, 8-MiB payload per slot | 268441239 | 134222383 |
| First block lookup/index construction, 10k-item list | 6135344 | 9346384 |
| First index construction, duplicated-path component control | 10919056 | 9346384 |

Compaction peak-live-byte delta remained approximately 16 MiB, despite halving
total requested bytes. Compact item-index values reduce allocation traffic
versus duplicated paths but add a lookup indirection. The compact representation
is a memory tradeoff, not a warm-lookup speed claim.

| Cold/small-operation cost | Before mean µs | Candidate mean µs |
| --- | ---: | ---: |
| First logical-item lookup, 10k-item list | 197.783 | 4858.393 |
| Warm logical-item lookup, one-item list | 0.006 | 0.096 |
| Remote message containing one ready operation | 4.176 | 5.323 |

Cold lookup includes first index construction; fixture parsing and dropping are
outside the timer. Warm lookup excludes construction. The cache benefits
repeated large-list queries, not one-off or tiny-list queries, and increases
cold index allocation versus the original scan implementation. Complete
admission also costs more for a single ready operation; the independently
rebuilt repeat measured 5.051 µs. These regressions are retained and disclosed
rather than marketed as performance improvements.

Payload-return counts were measured with `strace` only between explicit
compaction markers, excluding setup and verification. They are bytes returned
by read syscalls, not proof of physical disk traffic or a tracing latency result.

| Logical payload per snapshot slot | Reverted-control payload bytes read | Candidate payload bytes read |
| --- | ---: | ---: |
| 1 MiB | 8388608 | 4194304 |
| 8 MiB | 67108864 | 33554432 |

The candidate independently rebuilt to identical executable bytes. Fourteen
representative cases were remeasured with the same harness, not the entire
baseline/candidate/control collection. Pending/held/matching means
were stable, but dependency-delivery mean moved 13.46% and cold block-index
construction moved 19.13%. Do not use within-run confidence intervals as a
substitute for between-run noise checks.

Five initial control samples were rejected: a shared Cargo target reused a
fresh artifact from a different same-name/version source export. Distinct
source hashes alone did not prove distinct executables. The corrected controls
cleaned only private package artifacts before each build, required
`fresh=false` compiler artifacts, and verified distinct executable hashes.
Discarded samples remain in the provenance record, not in the tables above.

Build both benchmarks before sampling, then run the named groups without
concurrent builds or checks:

```sh
cargo bench --locked --bench council_performance --bench performance --no-run
taskset -c 0 cargo bench --locked --bench council_performance -- \
  remediation_ --sample-size 30 --measurement-time 3 --warm-up-time 1 --nresamples 10000
taskset -c 0 cargo bench --locked --bench performance -- \
  list_item_index --sample-size 30 --measurement-time 3 --warm-up-time 1 --nresamples 10000
```

Save and restore identical harness bytes when comparing revisions. Local records
`council-remediation-performance-valid-b72e.json`,
`council-remediation-performance-summary-b72e.json`,
`council-remediation-control-inputs-b72e.json`, and
`council-remediation-storage-io-b72e.json` preserve means, confidence estimates,
raw samples, allocator results, source identities, and bounded log references.
