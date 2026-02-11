# md-crdt
[![CI](https://github.com/Latent-Infinity/md-crdt/actions/workflows/ci.yml/badge.svg)](https://github.com/Latent-Infinity/md-crdt/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

md-crdt is a Rust library and CLI for offline-first, deterministic collaboration on Markdown. It models Markdown as CRDT-aware blocks and marks, supports frontmatter and tables, and can sync against plain `.md` files without giving up file ownership.

**Highlights**
- Deterministic merge behavior across peers.
- Markdown-aware parsing and serialization (structural or exact).
- Block- and mark-level operations for precise edits.
- File sync that ingests and flushes against a local vault.

**Workspace Layout**
- `md-crdt`: Primary library crate (modules: `core`, `doc`, `sync`; features: `storage`, `filesync`) and bundled CLI binary (`src/bin/md-crdt.rs`).
- `md-crdt-ffi`: FFI workspace crate (currently minimal placeholder surface).
- `md-crdt-naive-oracle`: Unpublished reference implementation used for differential testing.

**Library Quickstart**
Parse Markdown, edit a block, serialize back:

```rust
use md_crdt::{Document, EquivalenceMode, OpId, Parser};

let input = "# Title\n\nHello world.";
let mut doc = Parser::parse(input);

let first_block = doc.blocks_in_order().first().unwrap();
let block_id = first_block.id;

let op_id = OpId { counter: 1, peer: 1 };
doc.insert_text(block_id, 5, " brave", op_id).unwrap();

let output = doc.serialize(EquivalenceMode::Structural);
println!("{output}");
```

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

Sync changes with `md-crdt` sync APIs:

```rust
use md_crdt::{OpId, Operation, StateVector, SyncState};

let mut doc_a = SyncState::new();
let mut doc_b = SyncState::new();

doc_a.apply_op(Operation {
    id: OpId { counter: 1, peer: 1 },
    payload: vec![1, 2, 3],
});

let since = StateVector::new();
let change = doc_a.encode_changes_since(&since);
let _result = doc_b.apply_changes(change);
```

Work with a vault using `md-crdt` filesync APIs:

```rust
use md_crdt::Vault;

let vault = Vault::open(".")?;
vault.init()?;
vault.ingest()?;
vault.flush()?;
```

**CLI Workflows**
Run the CLI from the repo root:

```sh
cargo run --bin md-crdt -- status
```

Initialize a vault and ingest changes:

```sh
cargo run --bin md-crdt -- init
cargo run --bin md-crdt -- ingest
cargo run --bin md-crdt -- flush
```

Sync (ingest + dirty status):

```sh
cargo run --bin md-crdt -- sync
```

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

**License**
MIT. See `LICENSE`.
