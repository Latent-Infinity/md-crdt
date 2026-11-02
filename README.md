# md-crdt
[![CI](https://github.com/Latent-Infinity/md-crdt/actions/workflows/ci.yml/badge.svg)](https://github.com/Latent-Infinity/md-crdt/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

md-crdt is a Rust library and CLI for offline-first, deterministic collaboration on Markdown. It models Markdown as CRDT-aware blocks and marks, supports frontmatter and tables, and can sync against plain `.md` files without giving up file ownership.

**Highlights**

- Deterministic merge behavior across peers.
- Markdown-aware parsing and serialization (canonical structural or source-preserving exact).
- Block- and mark-level operations for precise edits.
- File sync that ingests and flushes against a local vault.
- Pure, fence-aware outline, section, search, and link queries through `ReadDocument`.
- Metadata-only export-state checks for hosts that must preserve unpublished session work.

**Workspace Layout**

- `md-crdt`: Primary library crate (modules: `core`, `doc`, `sync`; default features: `storage`, `filesync`, `cli`; default-off `sequence_incremental` and diagnostic `perf_trace`) and bundled CLI binary (`src/bin/md-crdt.rs`).
- `md-crdt-ffi`: Reserved workspace placeholder. It is not published and does not expose a C ABI or supported language bindings; Rust consumers should use `md-crdt` directly.
- `md-crdt-naive-oracle`: Unpublished reference implementation used for differential testing.

**Recommended configuration for agents / MCP hosts**

| Knob | Recommendation | Why |
| --- | --- | --- |
| Cargo features | Defaults include `storage`, `filesync`, and `cli`. Library-only consumers can disable defaults and select `storage`/`filesync` without pulling in CLI parsing. Optional: `sequence_incremental` for ablation; `perf_trace` for diagnostics | Default CLI behavior is unchanged |
| Text inserts | Prefer `insert_text` / `insert_text_batch` with the full known run, not one public call per keystroke | Coalesces a known run into one envelope |
| Vault edits | Use `with_local_edit` when each call must be durable; use `apply_local_edit` + `flush_document` to batch edits | Makes the persistence boundary explicit |
| History growth | Call `checkpoint_history` with a modest `max_retained_ops` and accurate active-peer leases on long sessions | Prunes transport payload history only; structural history remains and omitted peers must rebase |
| Wire codec | JSON envelopes default; opt into `md_crdt_bin_v1` only when both peers agree | Binary is compact; not the snapshot format |
| Text representation | Per-grapheme units (stable OpIds) | Run-length packing is deferred |

**Library Quickstart**

Parse Markdown, edit a block, serialize back:

```rust
use md_crdt::{EquivalenceMode, OpId, Parser};

let input = "# Title\n\nHello world.";
let mut doc = Parser::parse(input);

let block_id = doc.blocks_in_order().first().unwrap().id;

let op_id = OpId { counter: 1, peer: 1 };
doc.insert_text(block_id, 5, " brave", op_id).unwrap();

let output = doc.serialize(EquivalenceMode::Structural);
println!("{output}");
```

For read-only work, parse without creating a collaborative session or writing `.mdcrdt` state:

```rust
use md_crdt::ReadDocument;

let markdown = "# Guide\n\nIntro\n\n## Deploy\n\nRun it.\n";
let document = ReadDocument::parse(markdown);
let deploy = document
    .outline()
    .into_iter()
    .find(|heading| heading.title == "Deploy")
    .expect("heading exists");

let section = document.section(deploy.ordinal);
assert_eq!(section.len(), 2);
assert_eq!(document.search("run", 10).len(), 1);
```

`ReadDocument::outline` is fence-aware; `section` addresses a heading by its zero-based top-level block ordinal; `search` returns at most one bounded hit per block; and `links` returns outbound links. Ordinals are valid only for the exact parsed text and must not be persisted or used as `VaultSession` block identities. Parsing currently expands Markdown substantially in memory, so query one document at a time and drop it.

Apply edit operations directly:

```rust
use md_crdt::{EditOp, InsertTextRun, OpId, Parser};

let mut doc = Parser::parse("Hello");
let block_id = doc.blocks_in_order()[0].id;

let op = EditOp::InsertText(InsertTextRun {
    block_id,
    grapheme_offset: 5,
    byte_offset: 5,
    text: " world".into(),
    op_id: OpId { counter: 1, peer: 42 },
});

doc.raw_apply_op(op, false).unwrap();
```

Exchange collaborative document changes between peers:

```rust
use md_crdt::{CollaborativeDocument, EquivalenceMode, ValidationLimits};

let mut alice = CollaborativeDocument::new(1);
let mut bob = CollaborativeDocument::new(2);

alice.insert_paragraph(None, "Hello from Alice")?;
let changes = alice.encode_changes_since(&bob.state_vector())?;
bob.apply_remote(changes, &ValidationLimits::default())?;

assert_eq!(
    bob.document().serialize(EquivalenceMode::Structural),
    "Hello from Alice"
);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Manage independent collaborative sessions for multiple files in one vault:

```rust,no_run
# #[cfg(feature = "filesync")]
# {
use md_crdt::filesync::VaultSession;

let mut vault = VaultSession::open("./notes")?;
let report = vault.ingest_all()?;
println!("ingested {} changed files", report.files_changed);

let shared_peer = vault.peer();
for path in ["projects/alpha.md", "journal/2026-07-13.md"] {
    let document = vault.session_mut(path)?;
    assert_eq!(document.peer(), shared_peer);
}

// Each path has an independent document and operation log. save_all_state writes
// the open snapshots beneath .mdcrdt/sessions/; it does not publish Markdown.
vault.save_all_state()?;
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

`VaultSession` opens documents lazily by vault-relative path. All documents share the vault's
stable peer ID, but their CRDT state, clocks, and snapshots remain independent.

Open, edit, and durably publish one Markdown document with explicit stale-state checks:

```rust,no_run
# #[cfg(feature = "filesync")]
# {
use md_crdt::filesync::VaultSession;

let mut vault = VaultSession::open("./notes")?;
let opened = vault.open_document("projects/alpha.md")?;
let block_id = vault
    .session_mut("projects/alpha.md")?
    .document()
    .blocks_in_order()[0]
    .id;

let edit = vault.with_local_edit("projects/alpha.md", |document| {
    document.insert_text(block_id, 5, " focused")
})?;
edit.value?;
let edited = vault.open_document("projects/alpha.md")?;
let outcome = vault.export_markdown(
    "projects/alpha.md",
    &edited.revision,
    edited.disk_fingerprint,
)?;

assert_eq!(outcome.document_id, opened.document_id);
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

`with_local_edit` preserves its original durability contract: the edit is persisted before the
call returns. For explicit batching, `apply_local_edit` changes only the in-memory session; call
`flush_document` (or its `save_state` alias) to make all accumulated edits durable.
When several already-known text runs belong at one position, `CollaborativeDocument::insert_text_batch`
concatenates them into one grapheme-safe wire envelope; use ordinary `insert_text` when edits must
remain separate or may be interleaved with other operations.
`compact_document` forces a full snapshot. Normal flushes append operation segments only when
the reader's complete replay-admission contract is provable, including dependency progress and
retained-resource budgets; otherwise they fall back to a full snapshot. Reopen replays segments
before reconciling the local operation clock.
`save_all_state` flushes every open session. None of these methods publishes Markdown.
`export_markdown` publishes one source-preserving Markdown view with revision and optional
disk-fingerprint preconditions.
`export_markdown_transaction` prevalidates several document identities, revisions, and disk
fingerprints before publishing any of them, then uses a durable recovery journal so an interrupted
commit is completed when the vault reopens. `create_markdown`, `rename_markdown`, and
`delete_markdown` provide the corresponding identity-aware file lifecycle operations.

Storage mutations require externally serialized ownership across handles and processes.
V2 snapshots now carry checksummed operation-coverage sidecars and an `operations.manifest`;
retired generations remain available while either snapshot slot needs them for fallback.
After adopting this generation layout, do not reopen or write the store with an older binary
that does not understand the manifest. The outer storage/compression framing is unchanged;
the session JSON format is now V10.
Close and reopen a vault after external modification of its storage artifacts.

Recovery validates the entire journal set and its generated pending/backup paths before
applying any transaction. This is local safety hardening, not a change to the trusted-peer /
trusted-local-disk threat model. Existing file permissions are preserved during publication.
Persistence and publication use file sync and, on Unix, directory sync around replacement.
Recovery and fault-injection tests exercise those code paths; they do not establish behavior
under actual power loss or guarantee a filesystem/device's persistence semantics. Directory
sync is not implemented on non-Unix platforms.

`match_blocks` and `Vault::match_blocks` return `Result<BlockMapping, VaultError>` so candidate
work or edge-budget refusal cannot be mistaken for a successful match. `VaultError::Session`
and `Snapshot` retain their typed causes; recoverable transactions retain a boxed `VaultError`.

Before accepting disk as authoritative, inspect the metadata-only state without opening a session:

```rust,no_run
# #[cfg(feature = "filesync")]
# {
use md_crdt::{ExportState, filesync::VaultSession};

let mut vault = VaultSession::open("./notes")?;
if vault.export_state("projects/alpha.md")? == ExportState::Unexported
    && vault.has_unexported_changes("projects/alpha.md")?
{
    // Export, preserve, or ask the user before accepting disk.
}
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ExportState::Exported` guarantees that persisted session state is not ahead of Markdown. `Unexported` is deliberately conservative; confirm it with `VaultSession::has_unexported_changes`. Persisted unexported work survives close and reopen; if disk diverges, reopen fails without overwriting either side. By contrast, `VaultSession::refresh_markdown`/`ingest_markdown` explicitly accept the current Markdown file as authoritative and discard unexported session work, even when the file bytes have not changed. A host that must prevent data loss must check first or provide its own guarded refresh workflow.

Exchange one path with another vault using the same host-provided transport boundary:

```rust,no_run
# #[cfg(feature = "filesync")]
# {
# use md_crdt::{ValidationLimits, filesync::VaultSession};
# let mut local = VaultSession::open("./notes")?;
# let mut remote = VaultSession::open("./notes-copy")?;
let remote_vector = remote.state_vector("projects/alpha.md")?;
let changes = local.encode_changes_since("projects/alpha.md", &remote_vector)?;
remote.apply_remote(
    "projects/alpha.md",
    changes,
    &ValidationLimits::default(),
)?;
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

`apply_remote` durably flushes the updated per-file session before returning. The application still
chooses how the `ChangeMessage` reaches the other vault.

JSON operation envelopes remain the default transport representation. Peers that explicitly agree
on the versioned `md_crdt_bin_v1` format can use `encode_json_change_message_as_bin_v1` and
`decode_bin_v1_change_message_to_json_payloads` for a compact binary transport; the frame is not a
snapshot format and does not change the default session codec.

Long-running hosts can prune retained transport operation payloads with `checkpoint_history`.
Each checkpoint request names the active peers and the state vector each has acknowledged;
peers omitted by the host are expired for that checkpoint. `encode_changes_since` returns
`RebaseRequired` when a caller is older than the retained delta floor, while `sync_since`
returns either a delta or a boxed V10 session checkpoint that the lagging peer can install
before incremental exchange resumes. Checkpointing does not garbage-collect the document's
full placement/deletion topology, creation-incarnation metadata, or structural tombstones.
Those durable histories remain necessary for late operations and placement fallback;
snapshot size and process RSS are not bounded by live content or `max_retained_ops`.

**Agent / MCP workloads:** prefer `insert_text_batch` (or one multi-grapheme `insert_text`) over
per-keystroke public calls; flush vault sessions with `apply_local_edit` + batched
`flush_document` when durability can wait. Use `checkpoint_history` to control retransmission
history, not as a structural-history or RSS cap. Text remains one RGA unit per grapheme by
design (stable OpIds for marks and anchors); run-length packing is not enabled by default.

Persistence accepts only V10 session snapshots and V2 dual-slot storage with generation-aware
operation retirement; session peers use wire V7. The required state includes full block/list-item
placement and deletion topology, creation incarnations, causally maximal structured-register
candidates, an independent frontmatter initializer base and field registers, and source-local
frontmatter revision provenance. Public structured document/DTO literals must supply the required
topology and conflict-history fields; list-item placement candidates name the destination's stable
`BlockId`, not its current sequence element. Older peer/session formats fail closed with typed
version/reinitialize errors; this is a clean incompatible cutover, with no legacy field defaults
or in-place migration. Before upgrading, back up the vault and export any unexported session
changes using a compatible older binary. Reinitialize and re-ingest authoritative Markdown only
after preserving that state; do not discard a session whose latest edits are absent from disk.

Snapshots are native JSON-backed state even when the live session uses `BinaryOpCodec` or
another `OpCodec`. Saving normalizes applied, pending, and deferred operation payloads to JSON;
restore and checkpoint rebase create a `CollaborativeDocument<JsonOpCodec>`. Saving does not
change the original session's codec or retained transport payloads. Configure live transport
agreement separately from snapshot persistence.

**Structural deletion policy:** deletion captures the target and the incarnation-qualified
descendants visible at the author's causal cut. Captured descendants die even if a concurrent
move tries to escape the subtree. An escape completed and observed before deletion survives
because it is no longer in the captured subtree. Unseen incoming outsiders are not absorbed
into the deletion: unavailable destinations fall back to historical viable placements, including
a real surviving list for list items. A late initial birth into a deleted creation origin is
a no-op, even if it concurrently tries to escape; this is not blanket deletion of every incoming
move. Logical UUIDs are opaque, while creation-operation origins distinguish incarnations:
recreating a deleted identity does not revive its old moves or let old aliases address the new
incarnation. Bodies remain owned by one document tree; topology history is metadata, not a
hidden content archive or a synthetic fallback list.

**Frontmatter concurrency:** concurrent initializers select an LWW base independently of
per-key explicit writes and deletions. A winning initializer does not erase those field
registers; even writes arriving before initialization survive checkpoint/restore. Opaque YAML
rejects local structured mutation, but supported remote field writes remain durable even when
an opaque base hides them. Adopting disk source is not semantic base replication: an opaque
adopted Exact view can likewise hide durable remote registers and blocks local structured
edits while that source provenance still applies. Exact source visibility is therefore not
proof that no remote field write exists; source-local provenance is persisted across reopen.

**CLI Workflows**
Target a vault from any working directory (the default is `--vault .`):

```sh
cargo run --bin md-crdt -- --vault ./notes status
```

Initialize a vault, then ingest every `.md` file into its per-file collaborative session:

```sh
cargo run --bin md-crdt -- --vault ./notes init
cargo run --bin md-crdt -- --vault ./notes ingest
```

`sync` performs the same local ingest and returns exit code 2 when it emits operations, which is
useful for automation. The host application is responsible for transporting encoded changes to
other peers.

```sh
cargo run --bin md-crdt -- --vault ./notes sync
```

`flush` records the current Markdown fingerprints used by `status`; it does not export session
snapshots or send changes over a network.

**Development**

The workspace uses the exact Rust 1.99.0 toolchain in `rust-toolchain.toml`;
its declared consumer MSRV is 1.95.

Install `just` (optional but recommended):

```sh
cargo install just
```

Common commands:

- `just fmt`
- `just lint`
- `just test`
- `just feature-check` exercises the declared MSRV, minimal, storage, filesync, and incremental configurations locally; CI remains paused.
- `just differential-test`
- `just fuzz-quick`

The optional incremental-ordering path retains a complete independent ordering
check at every debug verification point, including tombstones. Its reference
rebuild borrows metadata rather than constructing temporary sequences and indexes.
Run the assertion-preserving attribution workload in the default dev profile:

```sh
cargo run --example debug_ordering_workload --features sequence_incremental,perf_trace -- default-dev 128 512
```

This diagnostic includes missing anchors, right origins, deletion, durable edits
and replay. It does not replace or cap the full 9,999-edit regression.

Vault lazy loads share snapshot selection with generation-aware operation replay.
The proof is scoped to one externally serialized storage call, including when a
corrupt newest snapshot requires retired operations; it is not a cross-call cache.
Run the native recovery, editing, publication and reopen probe with:

```sh
cargo bench --bench council_performance -- --open-correctness
```

Qualified local measurements, component ablations and adverse allocator-sensitive
regimes are recorded in [the conformance record](docs/rust-standards-conformance.md).

Snapshot restore keeps its validation proof private and owned: public mutable DTOs
validate freshly, and normalization requires full revalidation. Generic-codec saves
borrow applied payloads until canonicalization instead of cloning a discarded
transport log. The conformance record includes the measured deferred-restore
transient-memory tradeoff; this is not a claim that every regime is faster or smaller.

The unpublished `md-crdt-yrs-bench/` workspace provides controlled competitive measurements against exact-pinned Yrs. It is excluded from the product workspace and default quality gate:

- `just test-compare` runs the shared adapter contracts.
- `just bench-compare-quick` checks Criterion liveness; never cite its timings.
- `just bench-compare` runs one full comparison.
- `just bench-compare-report` runs the required clean-worktree, alternating-order, three-invocation report path with provenance.
- `just memory-compare` runs the separate per-engine RSS probe; its results are not cross-engine benchmark ratios.

The suite includes the initial public-text and sync workloads plus large-paste and multi-peer fan-in extensions. Serialization/materialization diagnostics, the illustrative Yrs block-map probe, and memory RSS stay explicitly non-competitive. See the self-contained [comparison methodology](md-crdt-yrs-bench/README.md). No benchmark result is a universal CRDT ranking: compare only like tiers and named workloads.

**License**

MIT. See `LICENSE`.


### External input budgets

Use `Parser::try_parse` / `ReadDocument::try_parse` for external source; `parse`
is the trusted, prevalidated API. Vault reads and creation validate 4 MiB source,
64 leading quote markers and 256 leading whitespace bytes per nonempty line.
Lexical nesting limits also apply inside fences. Ingestion's `lcs_steps` returns
a typed work/allocation refusal. Original dense alignments retain their
4,000,000-cell admission; additional long local edits use exact suffix trimming
and certified banded alignment with one cumulative 4,000,000-work-unit and
16,000,000-requested-byte budget, including output and all attempted bands.
Repeated text retains the original backward equal-first/insert-on-tie identity
mapping. Large dissimilar edits can still be refused; attempting a bounded
alignment costs more than the former immediate shape refusal.
Lexical table accounting caps width at 1,024 columns, consecutive table-like rows at 16,384
(including header/delimiter), and aggregate rectangular expansion at 262,144 cells. These
conservative checks also count escaped pipes and fenced source before semantic parsing.
A refused ingest leaves the existing session intact; it does not overwrite disk.

External source validation also caps nested square brackets at 64 and inline
syntax delimiter bytes (`*`, `_`, `~`, brackets and backticks) at 16,384 between blank source lines, including literals. The sum of
paragraph byte lengths times delimiter counts is capped at 64 Mi work units. Storage artifact reads are capped at 512 MiB and
transaction-journal reads at 4 MiB before decoding. Identity metadata is capped at 128 bytes.

`CollaborativeDocument` admission and snapshot restore enforce both hard durable ceilings:
100,000 aggregate pending operations and 64 MiB of retained logical raw/decoded charge.
`ValidationLimits::max_pending_buffer` and `max_retained_bytes` may tighten those limits,
but raising them cannot override either ceiling. Both constraints must hold; large envelopes
can exhaust the byte budget before reaching the count limit. Accounting includes nested
sequence pending state and the normalized JSON snapshot representation; it is not a cap on
full structural history, allocator bytes, or RSS. `max_dependency_work` defaults to 1,000,000
work units. Work exhaustion retains accepted dependencies; a subsequent apply call can resume
draining. Hosts must apply backpressure rather than discard operations whose prerequisites
have not arrived.
