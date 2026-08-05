<!--
VENDORED COPY — DO NOT EDIT IN THIS REPOSITORY.

Source:  Software-Standards/Rust Code Standards.md
Version: High-Performance Rust Coding Standards v3.1
sha256:  6815e5cda7e89e04 (first 16 hex)
Synced:  2026-08-05

Edits belong upstream. To re-sync, copy the upstream file over this one,
keeping this header, and update the version, hash, and date. Project-specific
decisions and approved exceptions live in rust-standards-conformance.md, not
here.
-->

# High-Performance Rust Coding Standards v3.1

**Hardware sympathy, predictable latency, and evidence-based optimization — Rust 1.95+ / Edition 2024**

**Reviewed through Rust 1.97.1, the current stable release (July 16, 2026). Declared baseline remains Rust 1.95.**

> Performance is a property of the whole system under a defined workload, not a property inferred from syntax.

These standards optimize for sustained throughput, bounded tail latency, efficient memory use, and predictable behavior under load. They deliberately avoid universal nanosecond or percentage claims: allocator behavior, CPU topology, compiler version, input distribution, and concurrency level can reverse an apparent optimization.

The priority order is:

1. Correctness and soundness
2. Security and bounded resource use
3. Latency and throughput objectives
4. Memory footprint and operational stability
5. Maintainability and build cost

A faster implementation that changes required semantics, weakens a threat model, or relies on undefined behavior is not an optimization.

---

## 0. Scope and Normative Language

The terms in this document are normative:

- **MUST**: required unless an approved, documented exception exists.
- **MUST NOT**: prohibited unless an approved, documented exception exists.
- **SHOULD**: the default; deviations require a concrete engineering reason.
- **SHOULD NOT**: normally prohibited; deviations require a concrete engineering reason.
- **MAY**: context-dependent option.
- **HOT PATH**: a path shown by production profiles, load tests, allocation profiles, or a clearly multiplicative execution pattern to materially affect an objective.

Performance-specific MUST rules apply to hot paths and performance-critical binaries. General correctness, safety, resource-bounding, and measurement rules apply everywhere.

### Rule identifiers

Every normative rule carries a permanent identifier of the form `RPS-NNN`. Identifiers exist so that reviews, waivers, suppression comments, lint configuration, agent adapters, and exception records can cite a rule unambiguously.

- Identifiers are permanent. A reworded rule keeps its identifier.
- Identifiers are never reused. A deleted rule's identifier is retired and recorded in the changelog.
- A new rule takes the next unallocated number regardless of where it appears in the document. After the first revision that adds rules, document order and identifier order no longer match, and that is expected.
- Cite the identifier, not the section number, in review comments and exception records.

As of v3.1 the highest allocated identifier is `RPS-307`. The next new rule takes `RPS-308`.

### Exception record

Any exception to a performance MUST rule must record:

- affected function, module, or service;
- target workload and hardware;
- baseline and candidate measurements;
- correctness, security, and memory tradeoffs;
- owner and review date;
- reason the exception is preferable.

---

## 1. Toolchain, Edition, and Compatibility Policy

### Standards

- `RPS-001` **MUST** use Edition 2024 for new crates.
- `RPS-002` **MUST** declare the supported compiler floor with `package.rust-version`.
- `RPS-003` **MUST** use Rust 1.95 or newer for code governed by this standard.
- `RPS-004` **MUST** test the declared MSRV and the pinned production toolchain in CI.
- `RPS-005` **MUST** pin the exact compiler used to produce production binaries; compiler upgrades can change code generation and must be benchmarked on critical workloads.
- `RPS-006` **SHOULD** test current stable in a non-blocking or scheduled job when production is intentionally pinned to an older release.
- `RPS-007` **MUST NOT** use an API stabilized after the declared `rust-version` unless the MSRV is raised.
- `RPS-008` **MUST NOT** require nightly for production code unless the feature is isolated, the operational value is measured, and the toolchain pin/upgrade process is explicit.

```toml
[package]
edition = "2024"
rust-version = "1.95"
```

For a binary workspace, pin the deployment compiler:

```toml
# rust-toolchain.toml
[toolchain]
channel = "1.97.1" # Production pin. 1.97.0 is defective; see "Compiler defect response".
profile = "minimal"
components = ["clippy", "rustfmt"]
```

This revision was reviewed through Rust 1.97.1, released July 16, 2026 and current stable at the time of writing. It retains 1.95 as the baseline so 1.95 features may be used unconditionally; APIs marked 1.96+ or 1.97+ require a corresponding MSRV increase.

A `rust-version` floor and a toolchain pin answer different questions. `rust-version` is the oldest compiler a consumer may use. The pin is the exact compiler that produced the artifact under measurement. Only the pin is meaningful for a performance claim.

Cargo parses TOML v1.1 in manifests as of Rust 1.94. Using TOML v1.1 syntax raises the development MSRV even though the published manifest remains readable by older parsers; do not introduce it into a workspace whose declared `rust-version` predates 1.94 without treating it as an MSRV change.

### Compiler defect response

A miscompilation is not a performance tradeoff. It is a correctness failure that no benchmark will detect, because the benchmark is built by the same defective compiler. Rust 1.97.1 fixed an LLVM miscompilation that had been present since at least Rust 1.87, so any artifact built with 1.87 through 1.97.0 was exposed.

- `RPS-009` **MUST** pin to a release carrying every published correctness point release for its minor version. Do not pin to an `x.y.0` once `x.y.1` exists.
- `RPS-010` **MUST** treat a compiler correctness point release as a rebuild-and-redeploy event for production binaries, not an optional upgrade deferred to the next planned bump.
- `RPS-011` **MUST** record the exact compiler version in artifact metadata and in every performance change record, so an affected build range can be identified after the fact.
- `RPS-012` **SHOULD** monitor upstream release announcements and codegen/miscompilation reports against the pinned version, and assign an owner for that monitoring.
- `RPS-013` **SHOULD** run the test suite in release mode against the beta channel in a scheduled, non-blocking job. Optimization-dependent defects appear in release builds, not debug builds.

### Rust 1.95 features relevant to this standard

- `core::hint::cold_path` for measured cold branches.
- `Atomic*::update` and `Atomic*::try_update` for closure-based read-modify-write operations.
- `Vec::push_mut`, `Vec::insert_mut`, and corresponding deque methods when an inserted value must be mutated immediately.
- `cfg_select!` for compile-time platform selection.
- `if let` guards on match arms.

These are tools, not automatic optimizations. Their use is governed by the measurement and correctness rules below.

---

## 2. Performance Objectives and Measurement Discipline

### Standards

- `RPS-014` **MUST** define the metric being optimized before changing a hot path: throughput, p99 latency, CPU/request, allocations/request, bytes/request, RSS, cache misses, startup time, or another explicit objective.
- `RPS-015` **MUST** establish a baseline before a performance change and repeat the same measurement after the change.
- `RPS-016` **MUST** use representative input distributions, data sizes, concurrency, feature flags, observability settings, and deployment compiler flags.
- `RPS-017` **MUST** include the exact toolchain, target triple, CPU, build flags, benchmark command, dataset version, and relevant environment settings with reported results.
- `RPS-018` **MUST** evaluate steady-state and overload behavior for services. A throughput win that creates an unbounded queue or worse p99/p99.9 latency is a regression unless explicitly intended.
- `RPS-019` **MUST** measure allocations or memory footprint when an optimization changes ownership, buffering, collection capacity, task count, or allocator behavior.
- `RPS-020` **SHOULD** retain a regression benchmark for material improvements.
- `RPS-021` **MUST NOT** approve a hot-path optimization based only on source appearance, a single timing run, or a debug build.

### Microbenchmarks versus system benchmarks

Use microbenchmarks to isolate CPU, allocation, or code-generation effects. Report an estimate with confidence bounds, throughput where meaningful, and input sizes. Criterion-style microbenchmarks do not substitute for service tail-latency measurements.

Use load or system tests to report at least:

- offered load and achieved throughput;
- p50, p95, p99, and p99.9 latency when tail latency matters;
- error and timeout rates;
- CPU utilization and CPU per operation;
- RSS/peak memory and allocation rate;
- queue depth or in-flight work.

### Noise control

- `RPS-022` **SHOULD** run wall-clock performance gates on dedicated or tightly controlled machines.
- `RPS-023` **SHOULD** pin CPU affinity, control CPU frequency behavior, warm caches/JIT-equivalent state where applicable, and run enough samples to expose variance.
- `RPS-024` **SHOULD** use instruction counts or hardware counters to complement wall-clock results.
- `RPS-025` **MUST NOT** gate wall-clock regressions on an uncontrolled shared CI runner.

---

## 3. Defining Hot and Adversarial Paths

Treat a path as hot when any of the following is true:

- it is executed per request, message, packet, row, token, or element at production scale;
- it appears materially in CPU, allocation, lock-contention, or I/O profiles;
- it controls p95/p99/p99.9 latency;
- it creates tasks, threads, syscalls, allocations, or synchronization operations proportionally to untrusted input;
- it processes large contiguous data or is a numeric kernel.

Treat an input path as adversarial even if it is normally cold when an external actor can trigger it repeatedly. Error construction, parsing failures, hash insertion, decompression, logging, and retries must remain resource-bounded under hostile inputs.

Do not use arbitrary source-code thresholds such as “more than 1,000 items” as the only definition. The relevant threshold comes from the service budget and measured workload.

### Complexity and work amplification

- `RPS-026` **MUST** examine algorithmic complexity and total work before applying instruction-level optimization. Avoiding an unintended extra pass, sort, clone, parse, or nested linear lookup usually dominates local syntax changes.
- `RPS-027` **MUST** test hot algorithms at production-scale and adversarial input sizes; small benchmarks can hide quadratic behavior, allocator cliffs, and cache-capacity transitions.
- `RPS-028` **MUST NOT** perform an unbounded amount of CPU, allocation, retry, logging, decompression, or downstream work from one unit of untrusted input.
- `RPS-029` **SHOULD** batch, index, precompute, canonicalize, or change the data model when that removes repeated work across calls.
- `RPS-030` **MUST** include output size and work amplification in parser, query, retry, and fan-out budgets.

### Moving work out of the request path

The cheapest work is work that never runs per request.

- `RPS-031` **SHOULD** evaluate constant work at compile time with `const`, `const fn`, or a const block when the inputs are known and the resulting binary size and compile time remain acceptable.
- `RPS-032` **SHOULD** build lookup tables, parsed configuration, compiled patterns, and derived indexes once at startup or on first use rather than per request.
- `RPS-033` **MUST** account for first-use cost when deferring initialization. `LazyLock`/`OnceLock` move work to the first caller, where it appears as a cold-start or post-deploy latency spike rather than as steady-state cost.
- `RPS-034` **SHOULD** warm caches, connection pools, and lazily initialized state before a process accepts production traffic when first-request latency is part of the objective.
- `RPS-035` **MUST** measure startup, first-request, and post-deploy latency separately from steady state when deployment frequency, autoscaling, or serverless execution make them material.
- `RPS-036` **MUST NOT** trade a large increase in binary size or compile time for a precomputed table without measuring both.

---

## 4. API Shape, Ownership, and Borrowing

Borrowing avoids copying or allocating the referent, but references are not literally free in every ABI or optimization context. For example, `&str` and `&[T]` are pointer-plus-length values. Design ownership around data flow first, then verify hot interfaces.

### Standards

- `RPS-037` **MUST** avoid requiring owned `T`, `String`, or `Vec<T>` solely for read-only access in a hot API. Accept `&T`, `&str`, or `&[T]` unless ownership transfer is part of the contract.
- `RPS-038` **SHOULD** take small `Copy` values by value.
- `RPS-039` **SHOULD** consume owned inputs when the operation can reuse their allocation or transfer ownership to the result.
- `RPS-040` **MUST** avoid cloning large or heap-owning values in loops, per-item transforms, and task-spawn paths unless the copy is required and measured.
- `RPS-041` **SHOULD** use scoped threads when worker lifetimes are bounded and borrowing removes shared ownership.
- `RPS-042` **SHOULD** use `Arc<T>` or `Arc<[T]>` for immutable data that genuinely outlives a lexical scope or is shared by independently owned tasks.
- `RPS-043` **MUST** avoid repeated `Arc::clone`/drop traffic in an inner loop when one clone can be hoisted outside it.
- `RPS-044` **SHOULD** use `Cow<'a, T>` only when borrow-or-own semantics simplify the API and the extra branch/representation is acceptable. `Cow` is not automatically faster than a clear borrowed or owned API.
- `RPS-045` **SHOULD** keep hot internal functions concrete. Generic convenience wrappers such as `impl AsRef<str>` or `impl Into<Cow<...>>` may be placed at public boundaries and delegate to a concrete core to control monomorphization.
- `RPS-046` **MUST NOT** return borrowed collections merely to avoid copies when doing so creates problematic lifetimes, pins a large backing object, or makes the caller materialize the data anyway. Consider iterators, indices, IDs, or owned compact results.

### Prefer a concrete hot core

```rust
pub fn parse(input: impl AsRef<[u8]>) -> Result<Record, ParseError> {
    parse_bytes(input.as_ref())
}

#[inline]
fn parse_bytes(input: &[u8]) -> Result<Record, ParseError> {
    // Hot implementation has one concrete input representation.
    todo!()
}
```

### Scoped borrowing

```rust
fn process_parallel(items: &[Item], max_workers: usize) {
    assert!(max_workers > 0);
    if items.is_empty() {
        return;
    }

    // Spawn at most `max_workers` OS threads. A persistent pool is preferable
    // when this operation is called repeatedly.
    let workers = max_workers.min(items.len());
    let chunk_size = items.len().div_ceil(workers);

    std::thread::scope(|scope| {
        for chunk in items.chunks(chunk_size) {
            scope.spawn(move || process_chunk(chunk));
        }
    });
}
```

Parallelism still requires enough work per chunk; scoped threads remove ownership overhead, not thread-creation cost. Repeated calls should normally use a bounded persistent pool rather than repeatedly creating OS threads.

---

## 5. Allocation, Capacity, and Buffer Reuse

Allocation avoidance is usually more important than small instruction-count changes, but over-allocation increases RSS, cache pressure, and allocator fragmentation. Capacity policy must account for both throughput and high-water memory retention.

### Standards

- `RPS-047` **MUST** avoid per-element heap allocation in measured hot loops when a contiguous, inline, pooled, interned, or arena-backed representation is practical.
- `RPS-048` **SHOULD** preallocate `Vec`, `String`, maps, and buffers when the required size or a reliable upper bound is known.
- `RPS-049` **MUST NOT** preallocate directly from untrusted declared sizes without validating a configured limit and checking arithmetic.
- `RPS-050` **SHOULD** use `try_reserve`/`try_reserve_exact` at fallible boundaries that must reject impossible or excessive allocations cleanly.
- `RPS-051` **SHOULD** reuse buffers with `clear()` when retained capacity is useful.
- `RPS-052` **MUST** cap or discard oversized reusable buffers after exceptional large requests so one outlier does not permanently inflate every worker’s memory footprint.
- `RPS-053` **SHOULD** allow `collect::<Vec<_>>()` to use iterator size hints before replacing it with hand-written allocation logic. Manual preallocation must provide a measured benefit or stronger failure handling.
- `RPS-054` **MUST NOT** rely on a particular `Vec` growth factor or number of reallocations; growth strategy is not a stable contract.
- `RPS-055` **SHOULD** prefer `reserve` when additional growth is likely and `reserve_exact` only when speculative capacity is undesirable.
- `RPS-056` **MUST NOT** call `shrink_to_fit` in a hot path without evidence; it may move data and often trades immediate work for uncertain memory recovery.
- `RPS-057` **SHOULD** use Rust 1.95 `push_mut`/`insert_mut` when it avoids a second lookup or awkward indexing after insertion.
- `RPS-058` **MAY** use `vec![0; n]`, `Box::new_zeroed_slice`, or `Arc::new_zeroed_slice` for large zeroed buffers when the allocator's zeroed-page path is measurably cheaper than reserving and then writing zeros. The fallible `try_reserve` plus `resize` pattern below remains the requirement at untrusted boundaries, where clean rejection outweighs the memset.

### Fallible capacity from external input

```rust
fn read_frame(declared_len: usize, max_frame: usize) -> Result<Vec<u8>, FrameError> {
    if declared_len > max_frame {
        return Err(FrameError::TooLarge { declared_len, max_frame });
    }

    let mut frame = Vec::new();
    frame
        .try_reserve_exact(declared_len)
        .map_err(|_| FrameError::AllocationFailed)?;
    frame.resize(declared_len, 0);
    Ok(frame)
}
```

### Buffer retention policy

```rust
const DEFAULT_CAPACITY: usize = 16 * 1024;
const MAX_RETAINED_CAPACITY: usize = 1024 * 1024;

fn recycle(mut buffer: Vec<u8>) -> Vec<u8> {
    if buffer.capacity() > MAX_RETAINED_CAPACITY {
        Vec::with_capacity(DEFAULT_CAPACITY)
    } else {
        buffer.clear();
        buffer
    }
}
```

### Shared and sliced buffers

- `RPS-059` **SHOULD** use immutable reference-counted byte storage such as `Arc<[u8]>` or a vetted `Bytes`-style type when payloads must be sliced or shared across independently owned tasks and this avoids copies.
- `RPS-060` **MUST** account for backing-allocation retention: a tiny slice can keep a very large input buffer alive. Copy into a compact allocation at queues, caches, or long-lived ownership boundaries when retained memory costs more than the copy.
- `RPS-061` **MUST NOT** add a general object/buffer pool merely to remove allocator calls. Measure pool contention, cache locality, reset cost, memory retention, and overload behavior against allocation plus bounded reuse.

### Inline and fixed-capacity collections

- `RPS-062` **MAY** use `SmallVec`, `ArrayVec`, or an inline custom representation when observed length distribution is strongly concentrated below the inline capacity.
- `RPS-063` **MUST** benchmark inline collections against `Vec` for the actual element type and workload.
- `RPS-064` **MUST** account for larger stack frames, larger containing structs/enums, copying cost, spill behavior, and code size.
- `RPS-065` **MUST NOT** assume stack allocation is inherently faster or safer for large arrays.

### Arenas and region allocation

- `RPS-066` **SHOULD** evaluate bump/arena allocation for request-, query-, or compilation-phase objects that share one lifetime and can be released together.
- `RPS-067` **MUST** enforce an arena memory limit for externally driven workloads.
- `RPS-068` **MUST** verify that destructor elision or bulk release does not skip required resource cleanup.

---

## 6. Collection and Representation Selection

Choose by access pattern, data size, iteration behavior, and memory layout—not only asymptotic complexity.

| Requirement | Default candidate | Important caveat |
|---|---|---|
| Dense sequence / indexed traversal | `Vec<T>` | Best general locality; front removal is O(n) |
| Queue / double-ended operations | `VecDeque<T>` | Ring buffer may be split across two slices |
| Priority queue | `BinaryHeap<T>` | No sorted full iteration |
| Fast key lookup | `HashMap<K, V>` | Hashing, spare capacity, and iteration cost matter |
| Ordered/range lookup | `BTreeMap<K, V>` | More comparisons; often good locality for ordered scans |
| Small read-mostly map | sorted `Vec<(K, V)>` | Updates are O(n), but compact lookup can win for small sets |
| Fixed immutable sequence | `Box<[T]>` / `Arc<[T]>` | No spare capacity or growth |

### Standards

- `RPS-069` **SHOULD** use `Vec` as the starting point for sequences.
- `RPS-070` **SHOULD** use `VecDeque` for frequent front operations rather than `Vec::remove(0)`.
- `RPS-071` **MUST** use `VecDeque::as_slices()` when two-slice processing is acceptable; use `make_contiguous()` only when a single slice is required and account for the rotation cost.
- `RPS-072` **MUST NOT** treat a `VecDeque` as a contiguous allocation or pass it to FFI as one buffer without first obtaining a valid contiguous slice.
- `RPS-073` **SHOULD** benchmark sorted `Vec`, `BTreeMap`, and `HashMap` for small or read-mostly maps rather than assuming hashing wins.
- `RPS-074` **MUST** account for the current `HashMap` implementation’s O(capacity) full iteration. Excessive reservation can slow scans as well as consume memory.
- `RPS-075` **SHOULD** split hot and cold fields when only a subset is accessed in the common path.
- `RPS-076` **SHOULD** use structure-of-arrays only when consumers operate on a subset of fields or vectorize across one field. Array-of-structures may be better when all fields are consumed together.
- `RPS-077` **MUST** verify layout assumptions with `size_of`, `align_of`, and target-specific tests.
- `RPS-078` **MUST NOT** assume default Rust struct or enum layout is stable across compiler versions.
- `RPS-079` **MUST** use `#[repr(C)]` or `#[repr(transparent)]` for ABI contracts; do not apply them as an unmeasured general optimization.
- `RPS-080` **SHOULD NOT** use `#[repr(packed)]` in normal application code; unaligned access and reference rules make it hazardous and often slower.
- `RPS-081` **MAY** use `#[repr(align(N))]` on a hot structure when profiles show line-crossing or false-sharing effects, but account for the size increase in every array, collection, and containing type.

### Compact representations

- `RPS-082` **SHOULD** replace repeated long keys with interned IDs or domain-specific integer keys when lookup dominates and lifetime management is clear.
- `RPS-083` **MAY** use `NonZero*` types to enable niche-optimized `Option` layouts, but must verify the actual type size and conversion cost.
- `RPS-084` **SHOULD** prefer indices/handles over pointer-rich object graphs when stable storage and compact traversal are more important than direct object ownership.

### Trait contracts the standard library optimizes against

The standard library specializes on trait guarantees. An implementation that compiles but is semantically wrong silently disables those optimizations or, increasingly, panics.

- `RPS-085` **MUST** implement `Ord`/`PartialOrd` as a genuine total order and keep `Eq` and `Hash` consistent, so that `a == b` implies equal hashes. Ordered and hashed collections optimize against these guarantees; Rust 1.96 optimized `BTreeMap::append`, and an incorrect `Ord` implementation can now panic there rather than merely producing a strange ordering.
- `RPS-086` **MUST** return a correct `size_hint` from custom iterators, and implement `ExactSizeIterator` only when the length is exact. `collect`, `extend`, and `reserve` preallocate from the hint; a wrong or needlessly conservative hint quietly reintroduces reallocation on every hot pipeline that consumes the iterator.
- `RPS-087` **SHOULD** keep `Clone` cheap and predictable, or document prominently that it is not. Generic code and reviewers both assume `Clone` is not a deep traversal.
- `RPS-088` **MUST** treat these as correctness rules, not style. They are the reason a pipeline preallocates, a comparison short-circuits, or a collection stays sound under optimization.

---

## 7. Iterators, Loops, and Materialization

Iterator adapters are often optimized well, but “zero cost” is a goal, not a guarantee for every chain. Explicit loops remain valid when they improve code generation, make bounds obvious, simplify error handling, or reduce code size.

### Standards

- `RPS-089` **MUST** avoid an intermediate `collect()` that is immediately consumed once by `map`, `filter`, `find`, `any`, `all`, `sum`, `fold`, or another streaming operation.
- `RPS-090` **SHOULD** keep single-pass transformations lazy.
- `RPS-091` **SHOULD** materialize when the data is reused, randomly accessed, sorted, sent across an API boundary, or when contiguous storage measurably improves locality/vectorization.
- `RPS-092` **SHOULD** use short-circuiting terminal operations instead of collecting to answer existence or search questions.
- `RPS-093` **SHOULD** use `try_fold`, `try_for_each`, or an explicit loop for fallible pipelines to avoid partial intermediate collections.
- `RPS-094` **MUST** preserve tails when using `chunks_exact`, `as_chunks`, or manual vector-width processing.
- `RPS-095` **MUST** compare generated code or counters when a loop is performance-critical; stylistic preference does not decide between adapters and `for` loops.
- `RPS-096` **MUST NOT** use unchecked indexing merely because an iterator “looks slower.”

### Avoid unnecessary materialization

```rust
let total: u64 = records
    .iter()
    .filter(|record| record.enabled)
    .map(|record| u64::from(record.cost))
    .sum();
```

### Fixed-size chunks with a correct tail

`as_chunks` is useful for fixed-width kernels, but the remainder is part of the result and must be handled.

```rust
fn xor_in_place(dst: &mut [u8], src: &[u8]) {
    assert_eq!(dst.len(), src.len());

    let (dst_blocks, dst_tail) = dst.as_chunks_mut::<32>();
    let (src_blocks, src_tail) = src.as_chunks::<32>();

    for (dst_block, src_block) in dst_blocks.iter_mut().zip(src_blocks) {
        for i in 0..32 {
            dst_block[i] ^= src_block[i];
        }
    }

    for (dst_byte, src_byte) in dst_tail.iter_mut().zip(src_tail) {
        *dst_byte ^= *src_byte;
    }
}
```

The compiler may vectorize this, but verification is still required.

### Removing bounds checks without `unsafe`

Most redundant bounds checks can be removed by giving the optimizer a proof it can already see. Try these before considering unchecked access:

- Bind and narrow the slice once up front (`let src = &src[..n];`). A later index against that binding has a bound the optimizer already knows.
- Iterate or `zip` parallel slices instead of indexing them; `zip` establishes the shorter length once.
- Assert equal lengths at the top of the function. One `assert_eq!` outside the loop can remove a check inside it.
- Use fixed-size views so the element count lives in the type: `as_array`, `as_chunks`, `array_windows`.

- `RPS-097` **SHOULD** use one of the forms above before restructuring a loop for speed; they cost at most one assertion outside the loop and preserve panic-on-bug behavior.
- `RPS-098` **MUST** verify with generated code or hardware counters that a bounds check was actually removed before claiming the transformation as an optimization.
- `RPS-099` **MUST NOT** reach for `get_unchecked` until the safe forms above have been tried and their generated code inspected.

---

## 8. Strings, Bytes, and Formatting

### Standards

- `RPS-100` **MUST** accept `&str` for read-only UTF-8 text and `&[u8]` for byte-oriented protocols or data.
- `RPS-101` **MUST NOT** introduce UTF-8 validation, Unicode scalar iteration, normalization, or case conversion into a byte-oriented hot path without a semantic requirement.
- `RPS-102` **SHOULD** use `String::with_capacity` when a reliable output size estimate exists.
- `RPS-103` **SHOULD** use `push`, `push_str`, `extend_from_slice`, or `write!` into a reusable buffer for incremental construction.
- `RPS-104` **MUST** avoid `format!` in a hot loop when the result is immediately written and a reusable `fmt::Write`/`io::Write` target is available.
- `RPS-105` **SHOULD** use `Path`/`OsStr` for OS paths rather than round-tripping through UTF-8 strings.
- `RPS-106` **SHOULD** intern or map repeated text keys to compact IDs when profiling shows hashing/comparison or allocation dominates.

`String + &str` consumes and reuses the left-hand buffer, growing it if necessary. Prefer `push_str` in loops because ownership and capacity behavior are clearer, not because `+` necessarily allocates a fresh string on every iteration.

```rust
use std::fmt::Write as _;

fn render(items: &[Item], output: &mut String) {
    output.clear();
    for item in items {
        writeln!(output, "{}:{}", item.id, item.value).expect("writing to String cannot fail");
    }
}
```

---

## 9. Hashing and Map Access

Rust’s standard `HashMap` is seeded for HashDoS resistance. Its current default algorithm is an implementation detail and may change. Alternative hashers are a threat-model decision as well as a performance decision.

### Standards

- `RPS-107` **MUST** use a HashDoS-resistant keyed hasher for untrusted or attacker-influenced keys unless a security review approves a different bounded design.
- `RPS-108` **MAY** use a faster non-default hasher for trusted internal keys when a representative benchmark shows material benefit.
- `RPS-109` **MUST** document key provenance and collision/DoS assumptions next to a non-default hasher type alias or constructor.
- `RPS-110` **MUST** benchmark the complete map operation, not only the standalone hash function.
- `RPS-111` **SHOULD** improve key representation before swapping hashers: use integer IDs, borrowed lookup, interning, pre-parsed keys, or shorter canonical keys where appropriate.
- `RPS-112` **SHOULD** use `entry` APIs to avoid a lookup followed by a second insertion lookup.
- `RPS-113` **SHOULD** use borrowed lookup (`String` keys queried by `&str`, for example) to avoid temporary allocation.
- `RPS-114` **SHOULD** reserve a reliable number of entries with `with_capacity`; the collection already accounts for its internal load policy.
- `RPS-115` **MUST NOT** use fixed universal nanosecond or speedup assumptions when selecting a hasher.

```rust
use std::collections::HashMap;

fn count<'a>(keys: impl IntoIterator<Item = &'a str>) -> HashMap<&'a str, u64> {
    let mut counts = HashMap::new();
    for key in keys {
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}
```

---

## 10. Error Handling, Panics, and Cold Paths

`Result` and `Option` are efficient representations in many cases, but their size and code-generation cost depend on the contained types. Large error variants can inflate every success return value even when errors are rare.

### Standards

- `RPS-116` **MUST** use typed errors in core/library layers.
- `RPS-117` **SHOULD** keep frequently returned error enums compact; box or otherwise indirect large diagnostic payloads when that reduces the common-path representation and the error path is genuinely rare.
- `RPS-118` **MUST** avoid eager string formatting on a success-dominated path. Preserve structured context and format at a boundary.
- `RPS-119` **SHOULD** use `ok_or_else`, `map_err`, or explicit branches when error construction is nontrivial and should be lazy.
- `RPS-120` **MAY** use `thiserror` for maintainability; the crate is not a performance requirement.
- `RPS-121` **MUST NOT** use `Result<T, String>` as the default library error model.
- `RPS-122` **MUST NOT** use `unwrap` for recoverable library errors.
- `RPS-123` **MAY** use `expect` for a proven invariant with a message that explains the invariant, not merely the symptom.
- `RPS-124` **MUST** ensure error paths triggered by external input are bounded and do not cause log amplification or large repeated allocations.
- `RPS-125` **MAY** call `core::hint::cold_path()` in a measured, genuinely unlikely branch.
- `RPS-126` **MUST NOT** assume `cold_path` or `#[cold]` is beneficial; an incorrectly marked branch can reduce performance.

```rust
fn parse_header(input: &[u8]) -> Result<Header, ParseError> {
    if input.len() < Header::ENCODED_LEN {
        core::hint::cold_path();
        return Err(ParseError::Truncated {
            actual: input.len(),
            required: Header::ENCODED_LEN,
        });
    }

    Header::decode(input)
}
```

### Panic strategy

- `RPS-127` **SHOULD** consider `panic = "abort"` for final binaries that do not rely on unwinding, `catch_unwind`, unwind-based cleanup across boundaries, or an embedding contract that requires unwinding.
- `RPS-128` **MUST NOT** impose `panic = "abort"` as a universal library rule.
- `RPS-129` **MUST** prevent unwinding across FFI boundaries unless the ABI and both sides explicitly support it.

---

## 11. Logging, Tracing, and Metrics

Observability is part of the production workload. Disabled events, enabled formatting, span creation, field recording, export queues, and cardinality can all affect latency.

### Standards

- `RPS-130` **MUST** use structured fields rather than preformatted message blobs for production events.
- `RPS-131` **MUST** avoid `format!` solely to feed a log macro.
- `RPS-132` **SHOULD** compile out verbose levels in release binaries when operational requirements permit, using the logging/tracing crate’s compile-time level controls.
- `RPS-133` **MUST** guard expensive field construction when the logging API does not guarantee it is skipped for disabled events.
- `RPS-134` **MUST** sample, aggregate, or rate-limit events emitted inside high-frequency paths.
- `RPS-135` **MUST** bound metric label cardinality; request IDs, user IDs, raw URLs, and unbounded error strings are not metric labels.
- `RPS-136` **SHOULD** aggregate counters locally and flush in batches rather than perform contended global updates for every item.
- `RPS-137` **SHOULD** avoid `#[instrument]` on tiny hot functions by default. When used, skip large or expensive arguments and benchmark with the production subscriber configuration.
- `RPS-138` **MUST** account for the cost of reading the clock. `Instant::now` is a vDSO- or syscall-level operation whose cost varies by platform and clock source; two reads per item in a high-frequency loop can be a measurable share of the operation being timed.
- `RPS-139` **SHOULD** time a batch, sample a fraction of operations, or use a coarse clock when per-item timing costs more than the resulting information is worth.
- `RPS-140` **MUST** count histogram and counter updates, including any lock or contended atomic in the recording path, as hot-path work rather than as free instrumentation.
- `RPS-141` **MUST** include production-equivalent observability in service load tests.

```rust
if tracing::enabled!(tracing::Level::DEBUG) {
    let summary = build_expensive_summary(&state);
    tracing::debug!(request_id = %request_id, ?summary, "state snapshot");
}
```

---

## 12. Static Dispatch, Dynamic Dispatch, and Inlining

Static dispatch can enable inlining, constant propagation, and vectorization. It can also create monomorphization bloat and instruction-cache pressure. Dynamic dispatch adds an indirect call and blocks some cross-call optimization, but can reduce code size and is often appropriate outside an inner loop.

### Standards

- `RPS-142` **SHOULD** use static dispatch inside measured hot loops when specialization enables useful optimization.
- `RPS-143` **SHOULD** hoist runtime polymorphism outside an element loop: select a strategy once, then run a concrete kernel.
- `RPS-144` **MAY** use enum dispatch for a small closed set of strategies.
- `RPS-145` **SHOULD** use dynamic dispatch at cold boundaries, plugin interfaces, heterogeneous storage, or where code-size measurements favor it.
- `RPS-146` **MUST** measure binary/text size when introducing broad generic APIs or many monomorphized combinations.
- `RPS-147` **SHOULD** use `#[inline]` selectively on small cross-crate hot functions when LTO or existing code generation does not inline them.
- `RPS-148` **MUST** reserve `#[inline(always)]` for a demonstrated code-generation problem and check code size as well as speed.
- `RPS-149` **MAY** use `#[inline(never)]`/`#[cold]` to isolate large cold paths after measurement.
- `RPS-150` **MUST NOT** annotate functions mechanically based on line count.

---

## 13. Concurrency, Locks, Atomics, and Parallelism

Parallelism is profitable only when useful work exceeds scheduling, synchronization, cache-coherence, and memory-bandwidth costs.

### Standards

- `RPS-151` **MUST** bound thread, task, queue, and in-flight operation counts.
- `RPS-152` **MUST** choose parallel granularity from measurement; do not spawn one thread or task per small item.
- `RPS-153` **SHOULD** use local aggregation plus merge/reduce instead of a shared increment or lock on every item.
- `RPS-154` **SHOULD** use scoped threads for bounded lifetimes and Rayon for suitable data-parallel workloads, but neither is mandatory when a dedicated pool, thread-per-core design, or sequential loop performs better.
- `RPS-155` **MUST** prevent pool oversubscription when Rayon, async blocking pools, native libraries, and application threads coexist.
- `RPS-156` **SHOULD** prefer single ownership and bounded message passing when it naturally matches the data flow.
- `RPS-157` **MUST NOT** assume channels are faster than locks. Measure queueing, copying, allocation, wakeups, and contention for the workload.
- `RPS-158` **MUST NOT** select `RwLock` from an arbitrary read/write ratio. Benchmark contention, hold times, fairness, and platform behavior.
- `RPS-159` **SHOULD** shorten write-lock hold time before changing lock type. `RwLockWriteGuard::downgrade` gives up exclusivity without a release-and-reacquire window when the rest of the critical section only reads.
- `RPS-160` **MUST** avoid holding a blocking mutex across `.await`.
- `RPS-161` **SHOULD** use an async mutex only when the guard must remain held across `.await`; a short non-awaiting critical section may be better served by a standard or parking-lot mutex.

### Atomic ordering

- `RPS-162` **MUST** choose the weakest ordering that is proven correct, and document the synchronization relationship for non-`Relaxed` operations.
- `RPS-163` **MAY** use `Relaxed` for independent statistics that do not publish or protect other data.
- `RPS-164` **MUST NOT** use `Relaxed` as a default for state transitions or publication.
- `RPS-165` **MUST** keep closures passed to `Atomic*::update`/`try_update` free of externally visible side effects because contention may cause the closure to run multiple times.
- `RPS-166` **SHOULD** use Loom or an equivalent model test for nontrivial lock-free protocols.

```rust
use std::sync::atomic::{AtomicU64, Ordering};

static REQUESTS: AtomicU64 = AtomicU64::new(0);

fn record_request() {
    // This counter does not publish or guard any other data.
    REQUESTS.fetch_add(1, Ordering::Relaxed);
}
```

### False sharing and sharding

- `RPS-167` **SHOULD** shard contended counters/state by worker or core and aggregate less frequently.
- `RPS-168` **SHOULD** use a platform-aware padding abstraction such as `CachePadded<T>` when profiles show false sharing.
- `RPS-169` **MUST NOT** hard-code 64-byte padding as universally correct across targets.
- `RPS-170` **MUST** account for the memory and cache-footprint cost of padding.
- `RPS-171` **SHOULD** pad shards, not every field blindly; fields updated together benefit from sharing a line.

### NUMA

For multi-socket or large-core-count systems:

- `RPS-172` **SHOULD** measure remote memory access and cross-node traffic.
- `RPS-173` **SHOULD** use first-touch/local allocation and worker affinity when NUMA effects are material.
- `RPS-174` **MUST** avoid global hot state that forces all nodes to contend on one cache line or allocation arena.

---

## 14. Async and Executor-Aware Design

Async improves utilization for waiting-heavy work; it does not make CPU work faster. Task creation, wakeups, allocation, cancellation, and executor fairness remain real costs.

### Standards

- `RPS-175` **MUST** use async primarily for I/O-bound concurrency or integration with an async ecosystem.
- `RPS-176` **SHOULD** directly `.await` trivial operations rather than spawn a task solely for indirection.
- `RPS-177` **MUST** apply admission control or an in-flight limit before spawning, queueing, or issuing work proportional to externally driven input.
- `RPS-178` **SHOULD** prefer a lazily polled bounded stream or worker pool over spawning one task per item.
- `RPS-179` **MUST** account for ordering: `buffer_unordered` may reorder results; use an order-preserving combinator when required.
- `RPS-180` **MUST** move blocking I/O and long CPU work off executor worker threads.
- `RPS-181` **MUST** bound how long a single future runs between yield points. A future that polls for a long stretch without awaiting starves every other task on that worker; the symptom is tail latency across unrelated endpoints, not a slow endpoint.
- `RPS-182` **SHOULD** split long synchronous stretches inside async code into chunks with an explicit yield, or move them to a blocking or CPU pool, whichever measurement favors.
- `RPS-183` **MUST** also bound `spawn_blocking` or use a dedicated CPU/Rayon pool for sustained CPU work; the blocking pool is not a substitute for backpressure.
- `RPS-184` **MUST** define cancellation behavior. Dropping a timeout/select loser must not corrupt protocol state, leak permits, or leave partial side effects without recovery.
- `RPS-185` **MUST** avoid async locks, channels, and tasks that permit unbounded queued memory.
- `RPS-186` **MUST** avoid retaining large locals, buffers, or guards across `.await` when they are no longer needed. Future size and retained state multiply by the number of in-flight tasks.
- `RPS-187` **SHOULD** inspect future/task size for very high-concurrency paths and isolate unusually large cold states when that improves memory use without excessive boxing or indirection.
- `RPS-188` **SHOULD** batch messages and writes to amortize wakeups and synchronization.

### Bounded concurrency without one spawned task per item

```rust
use futures::{stream, StreamExt};

async fn fetch_all(
    urls: Vec<Url>,
    concurrency: usize,
) -> Vec<Result<Response, FetchError>> {
    assert!(concurrency > 0);

    stream::iter(urls)
        .map(|url| async move { fetch(url).await })
        .buffer_unordered(concurrency)
        .collect()
        .await
}
```

Use `.buffered(concurrency)` when output order must match input order.

### Async traits and closures

- `RPS-189` **SHOULD** use native `async fn` in traits for statically dispatched traits.
- `RPS-190` **MUST** decide and document whether returned futures must be `Send` before exposing a public trait. When callers must spawn them on a multithreaded executor, prefer an explicit `fn -> impl Future + Send` signature until native async-trait bounds can express the required contract cleanly.
- `RPS-191` **SHOULD** isolate boxed futures or `#[async_trait]` behind a dynamic-dispatch boundary when `dyn Trait` is required; do not make every internal call pay boxing by default.
- `RPS-192` **SHOULD** use native async closures and `AsyncFn*` bounds where borrowing from closure captures simplifies a higher-order async API.
- `RPS-193` **SHOULD** use `std::pin::pin!` for stack pinning when the future need not be heap-owned or moved after pinning.

---

## 15. I/O, Parsing, and Serialization

### Standards

- `RPS-194` **MUST** batch small reads/writes where semantics and the latency budget permit, reducing syscall and protocol overhead without introducing unacceptable queueing delay.
- `RPS-195` **SHOULD** use `BufReader`/`BufWriter` for chatty sequential I/O and benchmark buffer size for high-throughput paths.
- `RPS-196` **MUST NOT** add a buffering layer blindly to one-shot whole-file reads; `std::fs::read`, direct `read_to_end`, memory mapping, or specialized APIs may avoid an extra copy.
- `RPS-197` **SHOULD** use vectored I/O when multiple buffers already exist and the platform/API can consume them together.
- `RPS-198` **MUST** handle partial reads/writes correctly; use `read_exact`, `write_all`, or explicit state machines where their semantics fit.
- `RPS-199` **SHOULD** stream large serialization directly to a buffered writer instead of constructing a second full in-memory representation.
- `RPS-200` **SHOULD** borrow deserialized fields from a retained input buffer when the format and parser support it.
- `RPS-201` **MUST** account for input-buffer retention when storing borrowed fields or zero-copy slices beyond parsing; a small record may otherwise retain an entire large payload.
- `RPS-202` **SHOULD** use `Cow<'a, str>` when some strings can be borrowed but escaped/normalized values require ownership.
- `RPS-203` **MUST** cap frame sizes, nesting depth, collection lengths, decompressed size, and parser work for untrusted input.
- `RPS-204` **MUST** benchmark SIMD-accelerated parsers on real payloads and target CPUs before adopting them.
- `RPS-205` **MAY** use memory mapping for large random-access or shared files after evaluating page faults, truncation races, lifetime safety, and platform behavior.

### Stream serialization

```rust
use std::{
    fs::File,
    io::{BufWriter, Write as _},
};

fn write_dataset(path: &std::path::Path, data: &Dataset) -> Result<(), Error> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, data)?;
    writer.flush()?; // Surface any error from the final buffered write.
    Ok(())
}
```

### Borrow when possible, own when required

```rust
use std::borrow::Cow;
use serde::Deserialize;

#[derive(Deserialize)]
struct Event<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
}
```

---

## 16. Data Locality, Branches, and Cache Behavior

### Standards

- `RPS-206` **MUST** optimize memory access before instruction cleverness when profiles show cache or memory stalls.
- `RPS-207` **SHOULD** traverse contiguous memory in the order it is laid out.
- `RPS-208` **SHOULD** separate frequently accessed fields from large cold metadata.
- `RPS-209` **SHOULD** batch operations by type or state when doing so improves locality without unacceptable latency.
- `RPS-210` **MUST NOT** replace a predictable branch with branchless work automatically. Branchless code can execute more instructions, perform unnecessary loads, or inhibit other optimizations.
- `RPS-211` **MAY** use `std::hint::select_unpredictable` only for a demonstrated hard-to-predict condition and after benchmarking. It is not a constant-time cryptography primitive.
- `RPS-212` **MAY** use prefetching only after hardware-counter evidence shows latency misses that cannot be hidden by normal access order; bad prefetching wastes bandwidth and cache.
- `RPS-213` **MUST** measure working-set size and cache misses when changing AoS/SoA, padding, indirection, or object size.

---

## 17. SIMD, CPU Features, and Numeric Semantics

As of Rust 1.97.1, `std::simd` remains unstable. Stable options are compiler auto-vectorization, architecture intrinsics in `std::arch`, or a vetted portable-SIMD crate.

### Standards

- `RPS-214` **SHOULD** first express independent operations over contiguous slices with clear aliasing and no loop-carried dependency.
- `RPS-215` **MUST** verify vectorization with assembly, LLVM optimization remarks, or hardware counters.
- `RPS-216` **MUST** handle scalar tails correctly.
- `RPS-217` **MUST** document and test floating-point tolerance when vectorization, reassociation, fused multiply-add, or reduction order can change results.
- `RPS-218` **MUST NOT** assume a floating-point reduction will auto-vectorize; strict evaluation order and IEEE semantics can prevent reassociation.
- `RPS-219` **SHOULD** use `mul_add` only when fused semantics are acceptable and validated.
- `RPS-220` **MUST** make integer overflow semantics explicit in critical arithmetic: checked, saturating, wrapping, overflowing, or strict behavior as required.
- `RPS-221` **SHOULD** avoid integer division and remainder by a runtime value in an inner loop. A compile-time divisor is strength-reduced; a runtime divisor is not, and division latency dominates most other integer arithmetic.
- `RPS-222` **MAY** use a `NonZero` divisor type so the compiler can drop the divide-by-zero check, or precompute a reciprocal multiply when the rounding semantics are validated against the exact result.
- `RPS-223` **SHOULD** use `div_ceil`, `is_multiple_of`, `midpoint`, and the carrying/borrowing integer operations rather than hand-written equivalents. They are correct at the boundaries and lower well.
- `RPS-224` **MUST** use runtime feature detection for portable binaries before calling a `#[target_feature]` implementation.
- `RPS-225` **MUST** ensure every `unsafe` call into a target-feature function is dominated by the corresponding feature check.
- `RPS-226` **SHOULD** use function multiversioning when one artifact must support a baseline fleet and exploit newer CPUs.
- `RPS-227` **MAY** compile a fleet-specific artifact with an explicit deployment CPU target.
- `RPS-228` **MUST NOT** publish a generally portable artifact built with `target-cpu=native` on an arbitrary build host.

### CPU target policy

Choose one of these explicitly:

1. **Portable baseline**: compile for the oldest supported CPU and use runtime-dispatched optimized kernels.
2. **Fleet baseline**: compile for a documented minimum microarchitecture common to the deployment fleet.
3. **Per-host build**: use `target-cpu=native` only when the artifact is built and consumed on the same compatible host class.

---

## 18. Unsafe Performance Optimizations

Unsafe code is permitted when it unlocks a measured, material improvement that safe code cannot obtain, but it carries a permanent proof and maintenance cost.

### Standards

- `RPS-229` **MUST** retain or provide a correct safe reference implementation or an independently checkable specification.
- `RPS-230` **MUST** demonstrate the performance benefit with the production compiler and representative workload before merging unsafe optimization code.
- `RPS-231` **MUST** keep the unsafe region minimal and expose a safe API wherever possible.
- `RPS-232` **MUST** place a `// SAFETY:` comment immediately at each unsafe operation explaining every required invariant and why it holds at that point.
- `RPS-233` **MUST** set `unsafe_op_in_unsafe_fn = "deny"` so unsafe functions still require explicit unsafe blocks.
- `RPS-234` **MUST** test unsafe code with Miri where supported and use sanitizers/fuzzing for pointer, FFI, parser, or concurrency-sensitive code.
- `RPS-235` **MUST** test zero lengths, alignment edges, maximum sizes, overlapping buffers, panics, and target-specific paths.
- `RPS-236` **MUST NOT** use `get_unchecked`, unchecked UTF-8, manual initialization, or raw-pointer arithmetic solely on the assumption that bounds or validation checks are expensive.
- `RPS-237` **MUST** inspect generated code first; the optimizer often removes provably redundant checks from safe slice/iterator code.
- `RPS-238` **MUST** review unsafe code again when compiler, target, allocator, or data-layout assumptions change.
- `RPS-239` **MUST NOT** create references that violate aliasing, alignment, initialization, or lifetime rules even if the machine instructions appear to work.

### Prefer stable safe primitives over hand-rolled unsafe

Several patterns that once required raw pointers are expressible with checked or narrowly scoped standard APIs at the 1.95 baseline. Reach for these before writing new unsafe:

- Bulk slice initialization: `<[MaybeUninit<T>]>::write_copy_of_slice`, `write_clone_of_slice`, `assume_init_ref`, `assume_init_mut`, `assume_init_drop`.
- Zeroed allocation: `Box::new_zeroed`, `Box::new_zeroed_slice`, `Arc::new_zeroed_slice`, `Rc::new_zeroed_slice`.
- Ownership transfer across FFI: `Vec::into_raw_parts` and `String::into_raw_parts` instead of a manual `as_mut_ptr` plus `mem::forget` sequence.
- Layout arithmetic: `Layout::repeat`, `Layout::repeat_packed`, `Layout::extend_packed`, and `Layout::dangling_ptr` instead of hand-computed byte sizes.
- Fixed-size views: `<[T]>::as_array`, `as_chunks`, `array_windows`.

- `RPS-240` **SHOULD** replace existing hand-rolled unsafe with one of these forms when behavior is equivalent, and re-benchmark rather than assuming parity.
- `RPS-241` **MUST** apply the same `// SAFETY:` and testing requirements to the remaining unsafe operations these APIs still require, such as `assume_init`.

### FFI and accelerator boundaries

- `RPS-242` **MUST** define ownership, lifetime, alignment, aliasing, thread-safety, error, and panic behavior at every FFI boundary.
- `RPS-243` **MUST** use a stable external ABI and explicit representation (`extern "C"`, `#[repr(C)]`, or `#[repr(transparent)]` as appropriate) across independently compiled components; Rust's native ABI is not a stable component boundary.
- `RPS-244` **MUST** batch work across FFI/GPU/accelerator boundaries when per-element calls or transfers materially affect the objective.
- `RPS-245` **SHOULD** use borrowed slices, caller-provided output buffers, or ownership-transfer handles to avoid redundant copies when the lifetime contract is sound.
- `RPS-246` **MUST** validate lengths and checked byte-size arithmetic before constructing slices or layouts from foreign values.
- `RPS-247` **MUST NOT** allow a Rust panic to cross a non-unwind FFI boundary.
- `RPS-248` **MUST** benchmark conversion, marshaling, synchronization, and transfer costs, not only the foreign kernel.

### `MaybeUninit` and spare capacity

- `RPS-249` **MAY** use `MaybeUninit`, `Vec::spare_capacity_mut`, or uninitialized boxed slices for bulk initialization when initialization cost or extra copies are measured bottlenecks.
- `RPS-250` **MUST** track exactly which elements are initialized across every early return and panic path.
- `RPS-251` **MUST** set length or call `assume_init` only after all required elements are initialized.

---

## 19. Build, Link, and Deployment Optimization

Cargo’s own documentation notes that `opt-level = 3` can be slower than `2` for some programs. LTO and codegen-unit choices are workload- and build-pipeline-dependent.

### Starting production profile

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
incremental = false
debug = 1
# overflow-checks = true   # Enable deliberately and benchmark; see Section 17.

[profile.profiling]
inherits = "release"
debug = 2
strip = "none"
```

This is a starting point, not a universal optimum.

### Standards

- `RPS-252` **MUST** benchmark production binaries with the actual production profile.
- `RPS-253` **SHOULD** compare at least `opt-level = 2` versus `3`, thin versus fat LTO, and practical codegen-unit settings for important binaries.
- `RPS-254` **SHOULD** keep usable symbols or separate debug information for production profiling and crash diagnosis.
- `RPS-255` **MUST NOT** strip the only copy of symbols needed to attribute regressions.
- `RPS-256` **SHOULD** evaluate `panic = "abort"` under the constraints in Section 10.
- `RPS-257` **SHOULD** use PGO for stable, performance-critical binaries when representative training traffic is available.
- `RPS-258` **MUST** train PGO on representative behavior, including important uncommon paths; stale or skewed profiles can regress performance.
- `RPS-259` **SHOULD** evaluate post-link layout optimization only after LTO/PGO and only with a reproducible symbolized workflow.
- `RPS-260` **MUST** use the deployment CPU policy from Section 17.
- `RPS-261` **MUST** preserve build IDs and enough toolchain metadata to reproduce a shipped artifact.
- `RPS-262` **MUST** benchmark compiler upgrades. A newer compiler is not assumed to be faster for every workload.
- `RPS-263` **MUST** decide `overflow-checks` for release builds explicitly rather than inheriting it by accident, and benchmark the choice. It is a decision about failure mode first and speed second.
- `RPS-264` **MUST NOT** rely on `debug_assertions` being enabled or disabled without checking the profile actually in use; the setting differs between profiles and can differ between workspace and dependency builds.
- `RPS-265` **SHOULD** verify which linker is in use. Recent toolchains default to `lld` on some Linux targets, which changes link time and can change section and symbol behavior that build or symbol tooling depends on.
- `RPS-266` **MUST** account for `panic = "abort"` no longer implying the absence of unwind tables. Recent toolchains emit them by default on Linux so backtraces work; `-C force-unwind-tables=no` restores the smaller binary at the cost of diagnosability.

Rust 1.97 switched stable Rust to v0 symbol mangling by default. Verify that profilers, crash tooling, and symbol pipelines demangle the shipped binaries correctly.

### Build and iteration cost

Build cost is a stated objective in the priority order and is measurable like any other.

- `RPS-267` **SHOULD** measure clean build, incremental build, and link time when changing profiles, features, dependencies, or generic surface area.
- `RPS-268` **SHOULD** audit duplicate and unnecessary dependencies. Duplicate semver-incompatible copies of a crate inflate build time, binary size, and instruction-cache pressure simultaneously.
- `RPS-269` **MUST** account for feature unification across a workspace: a feature enabled by one member is enabled for every member sharing that dependency, which can silently pull heavier code paths into a hot binary.
- `RPS-270` **SHOULD** keep the profile used for local iteration distinct from the profile used for production measurement, so that nobody benchmarks a configuration nobody ships.

---

## 20. Allocator and Memory-System Policy

Alternative allocators can improve one workload and regress another through different fragmentation, caching, page retention, and concurrency behavior.

### Standards

- `RPS-271` **MUST** profile allocator time or allocation rate before replacing the global allocator for performance.
- `RPS-272` **MUST** compare throughput, p99 latency, RSS/peak RSS, fragmentation/retained memory, and startup/termination behavior.
- `RPS-273` **MUST** run allocator tests at realistic thread counts and object-size distributions.
- `RPS-274` **MUST** account for the fact that a global allocator affects dependencies and all application allocations.
- `RPS-275` **SHOULD** evaluate jemalloc, mimalloc, the platform allocator, and domain-specific arenas without declaring a universal winner.
- `RPS-276` **MUST** verify target support, licensing, observability, crash behavior, and deployment packaging.
- `RPS-277` **MUST** retain an easy rollback path.

### Large pages and locking memory

Huge pages, locked memory, and custom page allocators are deployment-level optimizations:

- `RPS-278` **MAY** use them for measured TLB/page-fault bottlenecks.
- `RPS-279` **MUST** account for memory waste, allocation latency, container limits, privilege requirements, and failure behavior.
- `RPS-280` **MUST NOT** make them a library default.

---

## 21. Benchmarking and Profiling Tooling

Use multiple tools because no single benchmark explains time, allocation, cache behavior, or concurrency.

| Question | Preferred tools |
|---|---|
| Which code consumes CPU? | `perf`, `cargo flamegraph`, platform profilers |
| Did a local function get faster? | Criterion or Divan microbenchmark |
| Did instruction/cache behavior change? | `iai-callgrind`, `perf stat`, Cachegrind |
| Did allocation behavior change? | heaptrack, Bytehound, DHAT, custom counting allocator |
| What assembly was generated? | `cargo-show-asm`, `rustc --emit=asm`, LLVM remarks |
| Is unsafe code sound on supported paths? | Miri, sanitizers, fuzzing |
| Is concurrency logic correct? | Loom/model tests, stress tests, ThreadSanitizer where applicable |
| Did service latency improve? | production-like load generator plus HDR-style histograms |

### Standards

- `RPS-281` **MUST** use `std::hint::black_box` in microbenchmarks where optimizer elision is possible.
- `RPS-282` **MUST** benchmark realistic data distributions, not only all-zero or perfectly predictable synthetic input.
- `RPS-283` **SHOULD** separate setup/allocation from the operation under test unless setup is part of the production path.
- `RPS-284` **SHOULD** report throughput for byte/item-oriented work.
- `RPS-285` **SHOULD** include small, median, large, and adversarial sizes.
- `RPS-286` **MUST** keep correctness assertions outside the timed inner loop when possible, while still validating benchmark outputs.
- `RPS-287` **MUST NOT** describe instruction-count tools as complete latency predictors; cache, frequency, contention, and I/O still matter.
- `RPS-288` **MUST NOT** run production benchmarks with debug assertions or an accidental non-release dependency profile.

### Criterion example

```rust
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

fn bench_process(c: &mut Criterion) {
    let mut group = c.benchmark_group("process");

    for size in [64usize, 4 * 1024, 1024 * 1024] {
        let input = vec![1u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &input, |b, input| {
            b.iter(|| black_box(process(black_box(input))));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_process);
criterion_main!(benches);
```

### Allocation regression tests

Allocation-count tests must run in an isolated process or serialized harness. A global counting allocator observes unrelated test-harness and parallel-test allocations; do not assert exact counts in an uncontrolled test process.

---

## 22. Lints, CI, and Automated Enforcement

### Workspace lint baseline

```toml
# Workspace Cargo.toml
[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"
unused_must_use = "deny"

[workspace.lints.clippy]
perf = { level = "warn", priority = -1 }
suspicious = { level = "warn", priority = -1 }
needless_collect = "warn"
redundant_clone = "warn"
large_enum_variant = "warn"
large_stack_arrays = "warn"
await_holding_lock = "deny"          # Section 14: no blocking guard across `.await`.
await_holding_refcell_ref = "deny"
large_futures = "warn"               # Section 14: future size multiplies by in-flight tasks.
unused_async = "warn"
rc_buffer = "warn"                   # Prefer `Arc<[T]>` over `Arc<Vec<T>>`.

# Each member crate
[lints]
workspace = true
```

### Standards

- `RPS-289` **MUST** pin the compiler used for blocking Clippy jobs; lint sets and diagnostics change across releases.
- `RPS-290` **MUST** run `cargo fmt --check` and Clippy on every supported target/feature combination that matters.
- `RPS-291` **MUST NOT** assume `--all-features` represents a valid product configuration when features are mutually exclusive. Use an explicit matrix or `cargo hack`.
- `RPS-292` **SHOULD** avoid enabling the entire Clippy `nursery` group as a deny-level organizational standard; select useful lints deliberately.
- `RPS-293` **MUST** run tests on the MSRV and production compiler.
- `RPS-294` **SHOULD** run release-mode tests for code whose behavior depends on optimization, overflow settings, layout, or FFI.
- `RPS-295` **SHOULD** run Miri/sanitizers/fuzzers in scheduled or targeted jobs.
- `RPS-296` **SHOULD** use dependency, license, and advisory checks such as `cargo-deny`/RustSec tooling.
- `RPS-297` **SHOULD** track benchmark history on dedicated runners and alert on statistically or operationally material regressions.
- `RPS-298` **SHOULD** map every rule that a lint can enforce to that lint, and treat the unmapped remainder as the review checklist's actual job. A rule with an available machine check should not be enforced by human attention.
- `RPS-299` **MUST** re-evaluate the lint set on each compiler upgrade. Lints are added, renamed, and moved between groups, and a silently dropped lint is an unenforced rule.

Rust 1.97+ can deny build warnings without injecting `RUSTFLAGS`:

```bash
CARGO_BUILD_WARNINGS=deny cargo check --workspace
```

For the Rust 1.95 MSRV job, continue using the project’s compatible warning policy. Do not add 1.97-only configuration while claiming a 1.95 Cargo MSRV unless older Cargo behavior has been tested.

---

## 23. Edition 2024 and Modern Idioms

These features primarily improve correctness and clarity; they are not performance wins by themselves.

### Standards

- `RPS-300` **SHOULD** use `let` chains where they reduce nested control flow and preserve clear drop timing.
- `RPS-301` **SHOULD** use `let-else` for early exits.
- `RPS-302` **SHOULD** use `std::sync::LazyLock`/`OnceLock` instead of adding an external dependency for equivalent thread-safe behavior.
- `RPS-303` **SHOULD** use `std::pin::pin!` instead of `Box::pin` when heap ownership is unnecessary.
- `RPS-304` **MUST** understand Edition 2024 temporary-scope changes when locks, guards, or borrows are created in `if let` and tail expressions.
- `RPS-305` **MUST** mark extern blocks and unsafe attributes as required by Edition 2024.
- `RPS-306` **MUST** account for 2024 RPIT lifetime capture; use precise capture syntax where a broader capture would unnecessarily extend a borrow.

### Performance-relevant stable features at or below the 1.95 baseline

These may be used unconditionally under the declared MSRV.

| Release | Items worth knowing |
|---|---|
| 1.85 | Edition 2024; async closures |
| 1.87 | `Vec::with_capacity` guarantees it allocates the requested capacity; `<[T]>::split_off*`; `is_multiple_of`; `unbounded_shl`/`unbounded_shr` |
| 1.88 | `let` chains; `as_chunks`/`as_rchunks`; `hint::select_unpredictable`; `HashMap::extract_if`; naked functions |
| 1.90 | `lld` used by default on `x86_64-unknown-linux-gnu` |
| 1.91 | `strict_*` integer operations; `carrying_mul_add`; `core::iter::chain`; `BTreeMap::extract_if`; `AtomicPtr::fetch_*`; `build.build-dir` |
| 1.92 | `RwLockWriteGuard::downgrade`; `Box`/`Rc`/`Arc::new_zeroed[_slice]`; `btree_map::Entry::insert_entry`; unwind tables emitted by default under `panic=abort` on Linux |
| 1.93 | `Vec`/`String::into_raw_parts`; `<[MaybeUninit<T>]>::write_copy_of_slice` and `assume_init_*`; `<[T]>::as_array`; `VecDeque::pop_front_if`/`pop_back_if`; `-Cjump-tables=bool` |
| 1.94 | `<[T]>::array_windows`; `<[T]>::element_offset`; `Peekable::next_if_map`; fp16 intrinsics; const `mul_add`; Cargo TOML v1.1 |
| 1.95 | `core::hint::cold_path`; `Atomic*::update`/`try_update`; `cfg_select!`; `push_mut`/`insert_mut`; `if let` guards; `Layout::repeat`/`extend_packed`/`dangling_ptr` |

### Later stable releases

- Rust 1.96: `core::range` additions, `assert_matches!`/`debug_assert_matches!`, `NonZero` range iteration.
- Rust 1.97: Cargo warning policy, v0 symbol mangling by default, and new integer bit-position/width APIs.
- Rust 1.97.1: LLVM miscompilation fix. Required for production pins; see Section 1.

Using a later API requires raising `rust-version` to that release or newer.

### Known upgrade hazards

Compiler upgrades change more than diagnostics. These recent, concrete examples are why Section 19 requires re-benchmarking instead of assuming a newer compiler is faster.

- **1.93** removed an unsound internal `Copy` specialization in the standard library. Some standard APIs now call `Clone::clone` where they previously performed a bitwise copy, which can regress copy-heavy code.
- **1.96** optimized `BTreeMap::append`, which can now panic for element types with an incorrect `Ord` implementation that previously went unnoticed.
- **1.92** began emitting unwind tables by default under `panic = "abort"` on Linux so that backtraces work, changing binary size.
- **1.90** made `lld` the default linker on `x86_64-unknown-linux-gnu`.
- The bundled LLVM moved repeatedly across this range. Auto-vectorization, inlining, and layout decisions can differ between adjacent releases for identical source.

- `RPS-307` **MUST** re-run the benchmark suite, allocation profile, and binary-size check on every compiler upgrade, including point releases.

---

## 24. Code Review Checklist

Apply this checklist to every PR that changes a hot or adversarial path.

### Evidence

- [ ] Objective and workload are explicit.
- [ ] Baseline and candidate use the same compiler, flags, hardware, data, and observability.
- [ ] Throughput/tail latency, CPU, allocations, and memory are reported as relevant.
- [ ] Overload/backpressure behavior is tested.
- [ ] A regression benchmark remains in-tree when practical.
- [ ] Startup and first-request cost are measured when initialization was deferred or added.

### Data and allocation

- [ ] Clones, `Arc` operations, allocations, and formatting inside loops are justified.
- [ ] Capacity estimates are reliable, bounded, and safe for untrusted input.
- [ ] Reused buffers have a high-water retention policy.
- [ ] Borrowed/shared slices do not retain disproportionately large backing buffers.
- [ ] Collection choice matches lookup, update, scan, and ordering patterns.
- [ ] Struct size, enum size, indirection, and hot/cold layout are considered.
- [ ] `Ord`, `Eq`/`Hash`, and `size_hint` contracts are correct for types used in optimized collections and pipelines.

### CPU and code generation

- [ ] Iterator/materialization choices were verified rather than assumed.
- [ ] Chunked/SIMD paths handle tails and all target feature checks.
- [ ] Floating-point and overflow semantics are explicit.
- [ ] Dynamic dispatch is outside inner loops where static specialization matters.
- [ ] Inlining/monomorphization changes include code-size consideration.
- [ ] Bounds checks were removed by safe means and verified, not assumed away.
- [ ] Division or remainder by a runtime value in an inner loop is avoided or justified.

### Concurrency and async

- [ ] Threads, tasks, queues, and in-flight work are bounded.
- [ ] Work granularity exceeds scheduling/synchronization overhead.
- [ ] Atomic orderings and happens-before relationships are documented.
- [ ] No blocking mutex is held across `.await`.
- [ ] Large values and guards are not retained across `.await` without need.
- [ ] Cancellation and timeout behavior is safe.
- [ ] No single future polls for a long stretch without yielding.
- [ ] False sharing, oversubscription, and NUMA effects are considered where relevant.

### I/O and observability

- [ ] Small I/O is batched and partial I/O is handled.
- [ ] Serialization/parsing avoids unnecessary full-size intermediates.
- [ ] Input sizes, nesting, decompression, and parser work are capped.
- [ ] Logging is structured, sampled/rate-limited, and tested with production settings.
- [ ] Metric labels have bounded cardinality.
- [ ] Clock reads and metric updates in hot paths are counted as work.

### Unsafe and deployment

- [ ] Unsafe optimization has a measured benefit and safe reference/specification.
- [ ] Every unsafe operation has a complete `// SAFETY:` justification.
- [ ] FFI ownership, representation, panic, length, and batching contracts are explicit.
- [ ] Miri/sanitizer/fuzz/target coverage is appropriate.
- [ ] Production profile, CPU target, allocator, and compiler pin match the benchmark.
- [ ] The compiler pin includes the latest correctness point release for its minor version.
- [ ] Symbols/build IDs remain available for production diagnosis.

---

## 25. Performance Change Record Template

```markdown
### Performance change

**Objective:**
**Hot path:**
**Workload/data version:**
**Compiler/target/CPU:**
**Build profile and flags:**

| Metric | Baseline | Candidate | Delta |
|---|---:|---:|---:|
| Throughput | | | |
| p50 / p95 / p99 / p99.9 | | | |
| CPU/op or instructions | | | |
| Allocations/op and bytes/op | | | |
| RSS / peak RSS | | | |
| Binary text size | | | |

**Correctness/precision impact:**
**Security/resource-bound impact:**
**Operational tradeoffs:**
**Benchmark command and raw result location:**
**Regression coverage added:**
```

---

## 26. Reference Configuration

### CI sketch

```yaml
jobs:
  msrv:
    steps:
      - run: rustup toolchain install 1.95.0 --profile minimal
      - run: cargo +1.95.0 test --workspace

  # Optimization-dependent defects appear in release builds. Non-blocking.
  beta-release-tests:
    continue-on-error: true
    steps:
      - run: rustup toolchain install beta --profile minimal
      - run: cargo +beta test --workspace --release

  quality:
    steps:
      - run: cargo fmt --all -- --check
      - run: CARGO_BUILD_WARNINGS=deny cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace

  release-tests:
    steps:
      - run: cargo test --workspace --release

  # Run performance gates on a dedicated runner, not a shared generic VM.
  benchmark:
    steps:
      - run: cargo bench --workspace
```

Adapt the feature and target matrix to real products; do not mechanically use incompatible feature combinations.

### Production profile experiment matrix

At minimum, benchmark plausible combinations around:

```text
opt-level:      2, 3
LTO:            off, thin, fat
codegen units:  workspace default, 1
panic:          unwind, abort (eligible binaries only)
CPU target:     portable baseline, fleet baseline
allocator:      system, selected alternatives (allocation-bound workloads only)
PGO:            off, representative profile
```

Keep only settings that improve the actual objective without unacceptable build, memory, portability, or operational cost.

---

## 27. References

Primary and authoritative references should be checked when changing toolchain policy:

- Rust 1.95 release: <https://blog.rust-lang.org/2026/04/16/Rust-1.95.0/>
- Rust 1.96 release: <https://blog.rust-lang.org/2026/05/28/Rust-1.96.0/>
- Rust 1.97 release: <https://blog.rust-lang.org/2026/07/09/Rust-1.97.0/>
- Rust 1.97.1 release: <https://blog.rust-lang.org/2026/07/16/Rust-1.97.1/>
- Rust release notes: <https://doc.rust-lang.org/stable/releases.html>
- Rust 2024 Edition Guide: <https://doc.rust-lang.org/edition-guide/rust-2024/>
- Cargo profiles: <https://doc.rust-lang.org/cargo/reference/profiles.html>
- rustc code-generation options: <https://doc.rust-lang.org/rustc/codegen-options/>
- rustc profile-guided optimization: <https://doc.rust-lang.org/rustc/profile-guided-optimization.html>
- Cargo build-performance guide: <https://doc.rust-lang.org/stable/cargo/guide/build-performance.html>
- Clippy lint index: <https://rust-lang.github.io/rust-clippy/stable/index.html>
- Rust Performance Book: <https://nnethercote.github.io/perf-book/>
- Rustonomicon: <https://doc.rust-lang.org/nomicon/>
- Miri: <https://github.com/rust-lang/miri>

---

## Changelog

### v3.1 — August 5, 2026

- Reviewed through Rust 1.97.1, the current stable release, and moved the example production pin off the defective 1.97.0.
- Added a compiler defect response policy: correctness point releases are rebuild-and-redeploy events, and the exact compiler version must appear in artifact metadata.
- Assigned permanent `RPS-NNN` identifiers to every normative rule and defined the allocation policy.
- Added standards for moving work to compile time or startup, and for measuring startup and first-request latency separately from steady state.
- Added trait-contract standards covering `Ord`, `Eq`/`Hash`, and `size_hint`, including the optimizations and panics that now depend on them.
- Added safe bounds-check elimination guidance that must be exhausted before any unchecked indexing.
- Added integer division, remainder, and `NonZero` divisor guidance.
- Added executor starvation and yield-budget standards for async code.
- Added clock-read and metric-update cost to the observability standards.
- Added `overflow-checks`, `debug_assertions`, default-linker, and unwind-table policy to build configuration, plus a build and iteration cost subsection.
- Added a catalogue of performance-relevant stable APIs by release and a list of known upgrade hazards from 1.90 through 1.97.
- Mapped several existing rules to enforcing Clippy lints, including `await_holding_lock` and `large_futures`.
- Noted that Cargo TOML v1.1 syntax raises the development MSRV, and added a non-blocking beta release-mode CI job.

### v3.0 — July 10, 2026

- Reviewed through Rust 1.97.0 while retaining Rust 1.95 as the declared baseline.
- Replaced universal timing and percentage claims with workload-specific measurement requirements.
- Corrected the `VecDeque` contiguous/FFI guidance and removed the nonexistent `as_ptr` example.
- Corrected the claim that repeated `String + &str` necessarily creates a new allocation.
- Replaced mandatory `SmallVec`, Rayon, alternative allocator, full LTO, and branchless-code rules with measured decision criteria.
- Corrected bounded-async guidance so concurrency limits do not still create one task per input.
- Added tail-correct chunking guidance; removed a dot-product example that dropped remainder elements.
- Added atomic-ordering, cancellation-safety, NUMA, high-water and sliced-buffer retention, async future-size, FFI, adversarial input, floating-point semantics, and unsafe-optimization standards.
- Separated microbenchmark statistics from service p95/p99 latency measurement.
- Added CPU portability/function-multiversion policy and clarified that `std::simd` remains unstable in Rust 1.97.
- Updated build guidance to benchmark `opt-level`, LTO, codegen units, PGO, panic strategy, allocator, and CPU target rather than declaring one universal profile.
- Added Rust 1.97 Cargo warning-policy and symbol-mangling operational notes.

### v2.x

Previous version covered ownership, collections, iterators, strings, errors, logging, dispatch, concurrency, async, hashing, I/O, build configuration, benchmarking, hardware sympathy, and Edition 2024 idioms. Version 3.0 preserves that scope while making the rules more accurate, enforceable, and workload-driven.