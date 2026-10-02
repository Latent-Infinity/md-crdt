# Rust standards conformance — md-crdt

Governing source: `Software-Standards/Rust Code Standards.md`, High-Performance
Rust Coding Standards v3.3 (September 29, 2026). Vendored standards are copied
verbatim; project decisions are recorded here. Reviewed October 2, 2026.

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
  checks and async guard checks. The new compiler's Clippy runs with warnings
  denied; no renamed or unknown lint is suppressed (`RPS-289`, `RPS-299`).
- The paused CI workflow defines formatting, pinned Clippy/tests, explicit MSRV tests with `--locked`,
  release-mode tests, and dependency licenses/advisories (`RPS-290`–`RPS-296`).
- The paused scheduled/manual fuzz jobs use cargo-fuzz 0.13.2 and nightly-2026-10-01 with
  AddressSanitizer, fixed input/time/RSS limits, and preserved failure artifacts.
  Beta release tests are explicitly non-blocking. Nightly is tooling only.
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
SAFETY explanations; the sanitizer fuzz gate targets externally driven parsing,
mutation and decoding. Miri is not a substitute for those system tests.

Duplicate development/proc-macro dependency versions are reviewed rather than
force-unified across incompatible APIs (`RPS-268`). The consumer chooses its
release profile; library manifests cannot impose one on downstream hosts.
Performance reports must identify the actual package, features and profile.

The user-directed CI suspension is recorded above. Any additional waiver requires
measurements, tradeoffs, an owner and review date from the governing standard.


## Library-specific controls

The paused CI feature matrix covers minimal, storage, filesync, default, sequence_incremental and all-feature
configurations. Feature-dependent tests/doctests are enabled with their APIs;
the workspace benchmark declares `required-features = ["filesync"]`.

The release profile uses opt-level 3, fat LTO, one codegen unit, abort-on-panic,
line-table debug info, unstripped symbols and explicit wrapping overflow.
The benchmark profile retains its declared debug setting. Profile experiments
must be reported separately from the compiler-only comparison.

`lcs_steps` returns a typed work-limit error before allocating a quadratic table.
Ingestion builds a candidate session, so a late work-budget refusal does not
partially change the live session. Regression tests exercise this rollback.
The unmeasured `inline(always)` directives on disabled performance counters were
replaced by ordinary `inline`; no speed claim is made for that change.

## Recorded audit closure

The [Rust 1.99.0 measurement record](../../md-mcp/docs/performance/rust-1.99.0.md) preserves compiler, allocation,
binary-size, overload and release-profile results, including unfavorable results.
It documents the manual-host choice for `RPS-297` and the separate, uncited Yrs
research scope. Dedicated CI hardware is not claimed to exist.

| Rule group | Evidence / disposition |
| --- | --- |
| Toolchain and MSRV | Pinned 1.99.0; Local tests cover 1.95 and production; CI paused; no MSRV increase needed |
| Input work and memory | Bounded external Markdown, frames, artifact reads and diff tables; refusal/rollback tests |
| Async and cancellation | Host admission before blocking work; permit retained through cancellation; transport request limit |
| Feature and quality gates | Explicit feature matrix, Clippy, docs, optimized tests, license/advisory policy |
| Fuzz and sanitizer coverage | Scheduled ASan fuzz jobs and local smoke runs of all targets |
| Performance provenance | Committed measurements and reproduction commands; no uncontrolled CI timing gate |
| Dependency coherence | Exact Git revision checked outside the local Cargo path override |
| Unsafe optimizations, custom allocators, SIMD/PGO | Not introduced; no performance claim requires these techniques |
