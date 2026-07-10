# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

#### Wire Codec (`md-crdt::codec`)
- Versioned `Envelope` / `DocOp` DTOs for collaborative ops (`InsertBlock`, `DeleteBlock`)
- `JsonOpCodec` encode/decode with nest-depth limits and unknown-version rejection
- `insert_block_paragraph_is_empty` helper for unit-mode session validation (not an unconditional decode ban)
- Wire kinds: Paragraph, CodeFence, BlockQuote (nested), RawBlock — no live `Sequence` serialization

#### Document identity (`md-crdt::doc`)
- `block_id_from_op` — deterministic `BlockId` / row id from create `OpId` (peer×counter packing)
- `Block::new` and table `insert_row` no longer use random UUIDs
- Parser-produced block ids are stable for the same markdown input
- Vault `match_blocks` unmatched adds use content/position-derived ids (not `Uuid::new_v4`)

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
