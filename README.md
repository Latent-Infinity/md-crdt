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

- `md-crdt`: Primary library crate (modules: `core`, `doc`, `sync`; features: `storage`, `filesync`) and bundled CLI binary (`src/bin/md-crdt.rs`).
- `md-crdt-ffi`: Reserved workspace placeholder. It is not published and does not expose a C ABI or supported language bindings; Rust consumers should use `md-crdt` directly.
- `md-crdt-naive-oracle`: Unpublished reference implementation used for differential testing.

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
# Ok::<(), Box<dyn std::error::Error>>(())
```

`VaultSession` opens documents lazily by vault-relative path. All documents share the vault's
stable peer ID, but their CRDT state, clocks, and snapshots remain independent.

Open, edit, and durably publish one Markdown document with explicit stale-state checks:

```rust,no_run
use md_crdt::filesync::VaultSession;

let mut vault = VaultSession::open("./notes")?;
let opened = vault.open_document("projects/alpha.md")?;
let block_id = vault
    .session_mut("projects/alpha.md")?
    .document()
    .blocks_in_order()[0]
    .id;

vault
    .session_mut("projects/alpha.md")?
    .insert_text(block_id, 5, " focused")?;
let edited = vault.open_document("projects/alpha.md")?;
let outcome = vault.export_markdown(
    "projects/alpha.md",
    &edited.revision,
    edited.disk_fingerprint,
)?;

assert_eq!(outcome.document_id, opened.document_id);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`save_state` and `save_all_state` persist CRDT snapshots only. `export_markdown` publishes one
source-preserving Markdown view with revision and optional disk-fingerprint preconditions.
`export_markdown_transaction` prevalidates several document identities, revisions, and disk
fingerprints before publishing any of them, then uses a durable recovery journal so an interrupted
commit is completed when the vault reopens. `create_markdown`, `rename_markdown`, and
`delete_markdown` provide the corresponding identity-aware file lifecycle operations.

Before accepting disk as authoritative, inspect the metadata-only state without opening a session:

```rust,no_run
use md_crdt::{ExportState, filesync::VaultSession};

let mut vault = VaultSession::open("./notes")?;
if vault.export_state("projects/alpha.md")? == ExportState::Unexported
    && vault.has_unexported_changes("projects/alpha.md")?
{
    // Export, preserve, or ask the user before accepting disk.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ExportState::Exported` guarantees that persisted session state is not ahead of Markdown. `Unexported` is deliberately conservative; confirm it with `VaultSession::has_unexported_changes`. Persisted unexported work survives close and reopen; if disk diverges, reopen fails without overwriting either side. By contrast, `VaultSession::refresh_markdown`/`ingest_markdown` explicitly accept the current Markdown file as authoritative and discard unexported session work, even when the file bytes have not changed. A host that must prevent data loss must check first or provide its own guarded refresh workflow.

Exchange one path with another vault using the same host-provided transport boundary:

```rust,no_run
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
# Ok::<(), Box<dyn std::error::Error>>(())
```

`apply_remote` saves the updated per-file session snapshot. The application still chooses how the
`ChangeMessage` reaches the other vault.

Long-running hosts can bound retained operation history with `checkpoint_history`. Each checkpoint
request names the active peers and the state vector each has acknowledged; peers omitted by the host
are expired for that checkpoint. `encode_changes_since` returns `RebaseRequired` when a caller is
older than the retained delta floor, while `sync_since` returns either a compact delta or a boxed V7
session checkpoint that the lagging peer can install before incremental exchange resumes. Structural
tombstones are retained because operation acknowledgement alone is not sufficient for causal garbage
collection.

Persistence accepts only V7 session snapshots and current V2 dual-slot storage. Older or legacy
state fails with an explicit reinitialize/re-ingest error; re-ingest the authoritative Markdown files
instead of attempting an in-place upgrade.

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

Install `just` (optional but recommended):

```sh
cargo install just
```

Common commands:

- `just fmt`
- `just lint`
- `just test`
- `just differential-test`
- `just fuzz-quick`

The unpublished `md-crdt-yrs-bench/` workspace provides controlled competitive measurements against exact-pinned Yrs. It is excluded from the product workspace and default quality gate:

- `just test-compare` runs the shared adapter contracts.
- `just bench-compare-quick` checks Criterion liveness; never cite its timings.
- `just bench-compare` runs one full comparison.
- `just bench-compare-report` runs the required clean-worktree, alternating-order, three-invocation report path with provenance.
- `just memory-compare` runs the separate per-engine RSS probe; its results are not cross-engine benchmark ratios.

The suite includes the initial public-text and sync workloads plus large-paste and multi-peer fan-in extensions. Serialization/materialization diagnostics, the illustrative Yrs block-map probe, and memory RSS stay explicitly non-competitive. See the self-contained [comparison methodology](md-crdt-yrs-bench/README.md). No benchmark result is a universal CRDT ranking: compare only like tiers and named workloads.

**License**

MIT. See `LICENSE`.
