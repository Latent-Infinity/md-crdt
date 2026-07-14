# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

#### Concrete workspace lifecycle (`md-crdt::workspace` / `filesync`)
- Persistent, content-independent `VaultId` and `DocumentId` identities plus opaque `RevisionToken` and `DiskFingerprint` preconditions
- Direct Rust workspace contracts for document handles, block descriptors, change summaries, edit batches, receipts, and export outcomes without a public generic engine trait
- Path-scoped open, refresh, ingest, and durable Markdown export through `VaultSession`
- Crash-safe single-file publication using temp write, file sync, atomic rename, and directory sync on Unix
- Body-free hierarchical descriptor pages with semantic digests and bounded change summaries for local, remote, ingest, and export operations
- Preconditioned semantic edit batches with isolated preview, exact preview tokens, no-id-burn rejection, compact receipts, and all-or-nothing multi-document installation
- Identity-aware Markdown create, rename, and delete operations
- Recoverable multi-file export using synced pending/backup files and durable transaction intents completed automatically on vault open

#### Lossless source-backed documents (`md-crdt::doc`)
- Per-root-block source regions with owned leading trivia and dirty-region exact serialization
- Byte-identical no-op serialization for original line endings, blank lines, marker style, opaque blocks, and final-newline state
- Snapshot format v3 persistence for lossless source state while retaining v1/v2 reads
- Scoped edits re-render only their owning region; unsupported raw blocks remain byte-preserved and reject structured text mutation

#### Semantic inline Markdown and frontmatter (`md-crdt::doc` / `codec` / `session`)
- Supported bold, italic, code, and link syntax now parses to semantic grapheme text plus unit-anchored causal intervals
- Dirty structural serialization renders mark/link delimiter attributes; wire exchange and snapshots retain mark history
- Lossless frontmatter bases preserve comments, order, and quoting while per-key LWW operations collaborate on supported top-level fields
- Unsupported or malformed YAML remains opaque and rejects structured mutation
- Public byte/grapheme range helpers resolve exact text ranges to stable unit anchors

#### Identity-preserving moves and replacement (`md-crdt::session` / `filesync`)
- Atomic block and heading-section moves preserve block, descendant, text-unit, row, and mark/link identities under fresh placement ids
- Concurrent moves converge by placement id; logical-id delete wins move/delete races; cycle and anchor rejection do not burn clock ids
- External grapheme replacement projects marks through retained Unicode semantic ranges with outside-boundary/inside-expanding affinity

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
- Nested collaborative editing: `DocOp::InsertBlock`/`DeleteBlock` carry an optional `parent` container id; session `insert_block_in`/`insert_paragraph_in` and recursive text apply support editing blocks inside blockquotes (arbitrary depth)
- Vault ingest preserves blockquote nesting (no longer flattened); table-bearing files ingest structurally instead of being skipped

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
- `session_mut` / `save_state` / `save_all_state` / `close`; snapshot persistence is explicitly separate from Markdown export
- Path-scoped `state_vector` / `encode_changes_since` / `apply_remote`; remote application persists the affected session snapshot
- Two-vault coverage for concurrent external text edits, bidirectional delta exchange, convergence, and reopen persistence

#### Vault structure ingest (`md-crdt::filesync`)
- `VaultSession::ingest_all` / `ingest_markdown`: content-hash gate → parse → `match_blocks` → structure ops
- Removed blocks → `delete_block`; added paragraphs → N6-d `insert_paragraph`
- `IngestReport { files_noop, files_changed, ops_emitted }`; CLI `ingest`/`sync` use `VaultSession`
- Matched-block text diffs deferred (no grapheme LCS yet)

#### Nested re-ingest matching (`md-crdt::filesync`)
- Recursive structure sync for blockquotes (no skip on re-ingest)
- Quote containers match content-agnostically; children reconciled with structure ops
- Stricter re-ingest match floor so position alone cannot force zero-content matches

#### Text LCS ingest (`md-crdt::filesync`)
- Grapheme LCS over visible paragraph units → `InsertText` / `DeleteText`
- Preserves `BlockId` and LCS-equal unit OpIds across external paragraph edits
- Position-pairs unmatched paragraphs before falling back to block insert/delete

#### Structured headings and lists (`md-crdt::doc`)
- `Heading { level, text: Sequence<TextUnit> }` for ATX and setext headings
- `List { ordered, items }` with nested list-item child blocks
- Canonical heading/list serialization with stable multiline and tab-indented list normalization
- Heading/list wire DTO and session snapshot round trips

#### Structured tables (`md-crdt::doc` / `codec` / `session`)
- GFM table parsing into the existing `Table` model, including left/center/right alignment
- Canonical table serialization and stable parse/serialize round trips
- Table block metadata on `InsertBlock`; rows use independent insert/update/delete wire operations
- Collaborative table APIs with concurrent row-insert convergence and LWW row-cell updates
- Parsed table ingest emits metadata and row insert/update/delete/reorder operations while preserving matched table/row and unrelated prose identities

#### Collaborative block split and merge (`md-crdt::codec` / `session`)
- Wire `DocOp::SplitBlock` / `MergeBlocks` operations for paragraph and heading siblings
- Top-level and nested session APIs with adjacency and grapheme-offset validation
- Split preserves suffix text-unit IDs; merge preserves source IDs unless the destination already retains them as tombstones, then allocates collision-free replacements
- Stable merge insertion anchors, mark-history transfer, snapshot recovery, and multi-peer convergence coverage

#### Indexed document and sync frontiers (`md-crdt::doc` / `sync`)
- Generation-invalidated `BlockId`/element path index for constant-time-average block lookup, including nested blockquotes and list items
- Cached per-peer `StateVector` frontiers updated by local, remote, promoted, and restored operations
- Immutable operation payloads now use `Arc<[u8]>`, so delta/outbox/pending encoding shares bytes instead of cloning buffers; serialized wire and snapshot shapes remain unchanged
- Criterion baselines for block lookup, state-vector generation, and delta encoding in `benches/performance.rs`

#### Optional incremental sequence ordering (`md-crdt::core`)
- Default-off `sequence_incremental` feature for sibling-local insertion without a full sequence rebuild
- Debug dual-path validation against the full rebuild after each completed apply, including released pending batches
- Differential coverage for varied right origins and Criterion probes for top-level, nested-text, and serialization paths
- Public `CollaborativeDocument::insert_text` benchmark at 1k/10k graphemes; `just bench` runs both default and incremental ordering strategies
- `Document` remains `Send + Sync`; public top-level sequence mutation invalidates or self-repairs the index
- Responsibility-based module layout for document parsing/serialization, session wire handling, and sync validation; public API paths remain unchanged

#### FFI workspace packaging (`md-crdt-ffi`)
- Explicitly retained as an unpublished, API-empty workspace placeholder; removed the template `add` function and documented that no C ABI or supported language bindings exist

#### CLI and multi-document workflows
- Global `--vault <PATH>` option targets any vault root from any working directory while retaining `.` as the default
- CLI help now describes per-file collaborative ingest and distinguishes fingerprint tracking from session persistence
- README examples now use `CollaborativeDocument` for peer exchange and `VaultSession` for independent multi-file sessions with one shared vault peer
- Documented that CLI `sync` performs local ingest only; host applications provide transport, and `flush` records status fingerprints rather than exporting session snapshots

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
- V1/V2 dual-read superblocks with monotonically increasing generations
- Alternating metadata and payload slots retain the previous committed snapshot for recovery
- V2 superblock CRC32 trailer plus payload length/checksum verification
- Atomic temp-file publication with file sync and Unix directory sync
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
- Unpublished workspace placeholder; no C ABI or supported language bindings

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
