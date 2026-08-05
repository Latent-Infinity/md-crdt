# Rust standards conformance — md-crdt

Governing standard: [`rust-code-standards.md`](./rust-code-standards.md),
High-Performance Rust Coding Standards **v3.1**, vendored verbatim. That file is
a copy; it is not edited here. This file records what *this* repository decided,
where it deviates, and what is not yet met. Cite rule identifiers (`RPS-NNN`),
not section numbers.

Last reviewed: 2026-08-05.

## Declared configuration

| Setting | Value | Rule |
| --- | --- | --- |
| Edition | 2024 | `RPS-001` |
| `rust-version` (MSRV) | 1.95 | `RPS-002`, `RPS-003` |
| Toolchain pin | 1.97.1 (`rust-toolchain.toml`) | `RPS-005`, `RPS-009` |
| Lint baseline | workspace `[lints]`, inherited by every member; excluded workspaces (`fuzz/`, `md-crdt-yrs-bench/`) carry their own copy since they inherit nothing | §22 |
| Clippy thresholds | `clippy.toml` | §22 |
| Feature configurations verified | `--no-default-features`, `storage`, `filesync`, `--all-features`, default — all build warning-free | `RPS-291` |
| `unsafe` | none in the library or the FFI crate | §18 |

`rust-version` and the pin answer different questions: the MSRV is the oldest
compiler a consumer may use, the pin is the exact compiler that built the
artifact under measurement. Only the pin is meaningful for a performance claim.

**The pin is a correctness decision, not a preference.** Rust 1.97.1 carries the
fix for an LLVM miscompilation present from 1.87 through 1.97.0. Any artifact
built by a compiler in that range is exposed, and no benchmark detects it,
because the benchmark is built by the same compiler. This repository declared
1.85 and built on unpinned `stable` before 2026-08-05, so published 0.3.0
artifacts are in the affected range.

**MSRV raise is consumer-visible.** This crate publishes to crates.io, so 1.85
to 1.95 would break a consumer pinned below 1.95. Taken deliberately: `RPS-003`
requires 1.95, and the crate currently has no known downstream consumers.

## Verification

| Gate | Command | Rule |
| --- | --- | --- |
| Format | `just fmt` | `RPS-290` |
| Lints | `just lint` | `RPS-290` |
| Tests (pin) | `just test` | `RPS-293` |
| Differential / oracle | `just differential-test` | `RPS-295` |
| Aggregate | `just check` | — |
| Benchmarks | `just bench` (`benches/performance.rs`) | `RPS-020` |

CI (`.github/workflows/ci.yml`) runs the blocking gate on the **pinned** 1.97.1
compiler rather than on `stable`, because lint sets and diagnostics change
across releases and a blocking gate on a moving compiler is not reproducible
(`RPS-289`). A separate job checks the declared 1.95 MSRV (`RPS-004`), and the
scheduled job exercises current `stable` non-blocking (`RPS-006`).

## Open gaps

Recorded rather than hidden.

| Gap | Rule | Status |
| --- | --- | --- |
| Benchmarks run on shared GitHub runners, with no dedicated host or tracked history | `RPS-022`, `RPS-025`, `RPS-297` | **Open.** No wall-clock regression is gated, which is the correct conservative choice; the cost is that regressions are found by hand |
| Competitive results (`md-crdt-yrs-bench`) have no committed multi-run report | `RPS-017`, `RPS-023` | **Open.** The committed benchmark methodology forbids citing quick, dry-run, dirty-tree, or single-run results; no workload has a committed citable baseline yet |
| No root dependency-policy configuration | `RPS-296` | **Open.** `cargo deny check advisories` is green, but there is no root `deny.toml`; license policy therefore is not enforced and a default `cargo deny check licenses` rejects the dependency graph rather than representing an approved allowlist |
| Fuzz targets exist but run in no gate | `RPS-295` | **Open.** `fuzz/` has corpora and artifacts; nothing invokes it on push or schedule. It now declares Edition 2024, MSRV 1.95 and the lint baseline, so it is at least governed once wired up |
| No feature matrix in the gate | `RPS-291` | **Partly closed.** The 32 `dead_code` warnings a reduced-feature build used to surface are fixed: the projection and descriptor machinery is implementation of the `filesync`-backed workspace API and is now gated behind that feature, so it no longer compiles into builds that cannot reach it. Verified clean across `--no-default-features`, `storage`, `filesync`, `--all-features` and default. **Still open:** nothing in CI walks that matrix, so a future regression would go unnoticed until someone builds a reduced configuration by hand. `cargo hack --feature-powerset` in the scheduled job would close it |
| No Miri or sanitizer job | `RPS-295` | **Partly mitigated.** The library and FFI crate contain no `unsafe`, so the classes Miri catches are largely unreachable; the differential/oracle suite covers semantics |
| Release-mode tests not run separately | `RPS-294` | **Open.** Relevant here: snapshot and codec behaviour is layout- and optimization-adjacent |
| The scheduled job is named `nightly-differential-test` but installs `stable` | — | **Cosmetic.** Left as-is; renaming a job can break required-check configuration. Its behaviour is correct under `RPS-006` |

## Exceptions

None. No performance MUST rule is currently waived. An exception requires the
record described in the standard's §0: affected module, workload and hardware,
baseline and candidate measurements, tradeoffs, owner, review date, and why the
exception is preferable.

## Re-sync procedure

1. Copy the upstream standard over `rust-code-standards.md`, keeping its header.
2. Update the header's version, hash, and date.
3. Re-read the changelog for retired or added identifiers and update this file's
   gap and exception tables.
4. Re-evaluate the lint set (`RPS-299`): lints get added, renamed, and moved
   between groups, and a silently dropped lint is an unenforced rule.
