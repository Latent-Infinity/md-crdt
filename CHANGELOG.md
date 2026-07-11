# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

#### Wire Codec (`md-crdt::codec`)
- Versioned `Envelope` / `DocOp` DTOs for collaborative ops (`InsertBlock`, `DeleteBlock`, `InsertText`, `DeleteText`)
- `JsonOpCodec` encode/decode with nest-depth limits and unknown-version rejection
- `insert_block_paragraph_is_empty` helper for unit-mode session validation (not an unconditional decode ban)
- Wire kinds: Paragraph, CodeFence, BlockQuote (nested), RawBlock — no live `Sequence` serialization

#### Document identity (`md-crdt::doc`)
- `block_id_from_op` — deterministic `BlockId` / row id from create `OpId` (peer×counter packing)
- `Block::new` and table `insert_row` no longer use random UUIDs
- Parser-produced block ids are stable for the same markdown input
- Vault `match_blocks` unmatched adds use content/position-derived ids (not `Uuid::new_v4`)

#### Collaborative session (`md-crdt::session`)
- `CollaborativeDocument` with local `insert_block` / `delete_block` (encode-before-apply)
- `apply_remote` pre-decodes envelopes, integrates via `SyncState::apply_one`, applies document effects
- `SyncState::{contains, get, apply_one, promote_ready_pending, IntegrateResult}` for interleaved log/document apply
- Span-aware causal readiness: an operation may cover a contiguous counter range (a block plus its expanded text units), with `Operation.id` as the max embedded id; prevents OpId collisions between blocks and paragraph units
- Public `Sequence::compute_right_origin` for wire N4 stamps
- Multi-peer concurrent block insert convergence tests (`tests/session_collab.rs`)
- `SessionSnapshot` / `DocumentDto` save-restore, `import_state`, `rebind_peer`
- Storage helpers `write_to_storage` / `read_from_storage` (feature `storage`)
- `Sequence::from_elements` for full snapshot restore including tombstones

#### Paragraph text units (`md-crdt::doc::text`)
- `TextUnit` + `BlockKind::Paragraph { text: Sequence<TextUnit> }` (one grapheme per element)
- Parse/serialize/insert_text operate on unit sequences; wire `Paragraph { text: String }` unchanged
- Snapshot format v2 stores unit lists; v1 `text` string still loads via peer-0 synthetic unit ids

#### Text CRDT wire ops (`md-crdt::codec` / `session`)
- Wire `DocOp::InsertText` / `DeleteText` with `TextUnitWire` (explicit unit ids + N4 `right_origin`)
- Session `insert_text` / `delete_text` / `insert_paragraph` (N6-d: empty InsertBlock + InsertText)
- `CollaborativeDocument::new` defaults to `unit_mode = true`; string-mode via `with_codec(..., false)`
- Nested paragraph apply via `Sequence::with_value_mut` (no full block clone per unit)
- Concurrent multi-peer same-paragraph insert/delete convergence tests

#### Mark unification (`md-crdt::core::mark`)
- Single public `MarkSet` is the rich causal remove-wins CRDT (`MarkKind`, `Anchor`, spans)
- Removed generic `MarkSet<K,V>` / `TextAnchor` / LWW-only mark path from `core`
- `Block.marks` and `EditOp::{SetMark,RemoveMark}` use the unified type
- `Document::set_mark` / `remove_mark` (range split via `mark_ops`) and `render_paragraph_spans`
- Temporary deprecated aliases: `RichMarkSet`, `RichMarkInterval`

#### Vault multi-doc session (`md-crdt::filesync`)
- `VaultSession`: lazy `Path` → `CollaborativeDocument` map with shared vault peer
- `.mdcrdt/peer_id` load-or-create (non-zero `u64`)
- Per-file session snapshots under `.mdcrdt/sessions/` (separate from fingerprint `state/`)
- `session_mut` / `save` / `save_all` / `flush_all` / `close`

## [0.1.0] - 2025-02-04

### Added

#### Core CRDT Algorithms (`md-crdt::core`)
- Sequence CRDT with RGA (Replicated Growable Array) algorithm
- LWW (Last-Writer-Wins) Register for conflict resolution
- Map CRDT with LWW semantics for key-value pairs
- MarkSet CRDT for rich text formatting with:
  - Anchor-based mark intervals
  - Causal add-wins semantics
  - LWW attribute updates
- StateVector for version tracking and causality
- OpId-based operation ordering with lexicographic tie-breaking

#### Markdown Document Model (`md-crdt::doc`)
- Full CommonMark-compatible markdown parser
- Block types: Paragraph, CodeFence, BlockQuote, RawBlock, Table
- Inline formatting preservation (bold, italic, code, links)
- YAML frontmatter support
- Unicode and grapheme cluster handling
- Round-trip serialization (parse -> edit -> serialize)

#### Synchronization Protocol (`md-crdt::sync`)
- Delta-based change encoding
- Causal ordering with out-of-order buffering
- Validation with configurable resource limits
- Semantic conflict detection and auto-resolution

#### Persistent Storage (`md-crdt` `storage` feature)
- Crash-safe dual-superblock design
- CRC32 checksums for integrity verification
- Incremental operation segments
- Compaction with tombstone retention policies

#### File System Sync (`md-crdt` `filesync` feature)
- Vault-based markdown file management
- Block-level fingerprinting for change detection
- Fuzzy content matching for moved/edited blocks
- Copy detection across files

#### Command-Line Interface (`md-crdt` bundled binary)
- `init` - Initialize a vault
- `status` - Show file tracking status (with JSON output)
- `ingest` - Import external file changes
- `flush` - Write CRDT state to files
- `sync` - Combined ingest and flush

#### FFI Bindings (`md-crdt-ffi`)
- Placeholder for C-compatible foreign function interface

#### Testing & Quality
- Property-based tests with proptest
- Differential testing against naive oracle implementation
- Fuzz testing targets (parser, apply_changes, decode_changes, merge_convergence)
- CommonMark spec compliance tests
- External fixture tests (markdown-it, Comrak, GFM)

#### Developer Experience
- Pre-commit hooks for code quality
- Justfile with common development commands
- Comprehensive CONTRIBUTING.md guide
- MIT license

[0.1.0]: https://github.com/latenty-infinity/md-crdt/releases/tag/v0.1.0
