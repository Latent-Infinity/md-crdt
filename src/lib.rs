//! md-crdt: Conflict-free replicated data types for collaborative markdown editing.
//!
//! This crate provides a complete solution for real-time collaborative markdown
//! editing with offline-first capabilities. It includes:
//!
//! - **Core CRDT algorithms** - RGA sequences, LWW registers, and mark intervals
//! - **Document model** - Markdown parser and serializer with block-level editing
//! - **Sync protocol** - Change validation and state vector synchronization
//! - **Storage layer** - Crash-safe persistence with checkpointing (optional)
//! - **File sync** - Vault-based file system synchronization (optional)
//!
//! # Quick Start
//!
//! ```rust
//! use md_crdt::{Document, Parser, EquivalenceMode};
//!
//! // Parse a markdown document
//! let doc = Parser::parse("# Hello\n\nWorld");
//!
//! // Serialize back to markdown
//! let output = doc.serialize(EquivalenceMode::Structural);
//! ```
//!
//! # Features
//!
//! - `storage` - Enables checksummed, generation-based persistence with rkyv serialization
//! - `filesync` - Enables vault-based file system synchronization (requires `storage`)
//! - `dhat-heap` - Enables heap profiling with dhat

/// Compiles the README's Rust examples as doctests so they cannot silently rot.
///
/// This item exists only during `cargo test --doc` (zero cost for normal builds)
/// and keeps the full README off the crate's rustdoc landing page while still
/// compiling every ```rust``` block it contains.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

// Core CRDT algorithms
pub mod core;

// Document model, parser, and serializer
pub mod doc;

// Synchronization protocol
pub mod sync;

// Versioned wire codec for collaborative ops
pub mod codec;

// Collaborative session (document + sync + peer clock)
pub mod session;

// Concrete workspace contract types
pub mod workspace;

// Optional: Persistent storage layer
#[cfg(feature = "storage")]
pub mod storage;

// Optional: File system synchronization
#[cfg(feature = "filesync")]
pub mod filesync;

// Re-export core types
pub use core::{Element, LwwRegister, Map, OpId, PeerId, Sequence, SequenceOp, StateVector};

// Re-export unified mark types (rich causal MarkSet is the single public API)
pub use core::mark::{
    Anchor, AnchorBias, MarkInterval, MarkIntervalId, MarkKind, MarkSet, MarkValue, RemoveMark,
    Span,
};

/// Deprecated alias — use [`MarkSet`].
#[deprecated(note = "use MarkSet")]
pub type RichMarkSet = MarkSet;

/// Deprecated alias — use [`MarkInterval`].
#[deprecated(note = "use MarkInterval")]
pub type RichMarkInterval = MarkInterval;

// Re-export doc types
pub use doc::{
    Block, BlockId, BlockKind, CellContent, ColumnAlignment, ColumnDef, Document, EditError,
    EditOp, EquivalenceMode, InsertTextRun, ListItem, Parser, RowId, SerializeConfig, Table,
    TableRow, block_id_from_op, block_text_seq, block_text_seq_mut,
};

// Re-export doc mark operations
pub use doc::mark_ops;

// Re-export sync types
pub use sync::{
    ApplyResult, ChangeMessage, MalformedKind, Operation, SemanticConflict, SyncState,
    ValidationError, ValidationLimits, validate_changes,
};

// Re-export codec types
pub use codec::{
    BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert, CodecError, DocOp, Envelope,
    JsonOpCodec, MAX_WIRE_NEST_DEPTH, OpBody, OpCodec, TextUnitWire, WIRE_VERSION,
    insert_block_paragraph_is_empty,
};

// Re-export session types
pub use session::{
    CollaborativeDocument, DocumentDto, SNAPSHOT_FORMAT_VERSION, SessionApplyResult, SessionError,
    SessionSnapshot, SnapshotError,
};

pub use workspace::{
    BatchReceipt, BlockDescriptor, BlockDescriptorKind, ChangeSummary, DiskFingerprint,
    DocumentHandle, DocumentId, EditBatch, ExportOutcome, RevisionToken, VaultId,
};

// Re-export sync integrate types used with session
pub use sync::IntegrateResult;

// Re-export storage types (feature-gated)
#[cfg(feature = "storage")]
pub use storage::{CompactionReport, Storage, StorageError, TombstoneRetention};

// Re-export filesync types (feature-gated)
#[cfg(feature = "filesync")]
pub use filesync::{
    AddedBlock, ArchivedBlockFingerprint, BlockFingerprint, BlockMapping, BlockMatch, Fingerprint,
    IngestReport, IngestResult, LastFlushedState, MatchConfig, MatchType, ParsedBlock, Score,
    Vault, VaultError, VaultSession, fingerprint_document, match_blocks, parsed_blocks_from_doc,
};
