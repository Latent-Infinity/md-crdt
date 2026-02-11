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
//! - `storage` - Enables crash-safe persistence with rkyv serialization
//! - `filesync` - Enables vault-based file system synchronization (requires `storage`)
//! - `dhat-heap` - Enables heap profiling with dhat

// Core CRDT algorithms
pub mod core;

// Document model, parser, and serializer
pub mod doc;

// Synchronization protocol
pub mod sync;

// Optional: Persistent storage layer
#[cfg(feature = "storage")]
pub mod storage;

// Optional: File system synchronization
#[cfg(feature = "filesync")]
pub mod filesync;

// Re-export core types
pub use core::{
    Element, LwwRegister, Map, MarkInterval, MarkSet, OpId, PeerId, Sequence, SequenceOp,
    StateVector, TextAnchor,
};

// Re-export mark types
pub use core::mark::{
    Anchor, AnchorBias, MarkInterval as RichMarkInterval, MarkIntervalId, MarkKind,
    MarkSet as RichMarkSet, MarkValue, RemoveMark, Span,
};

// Re-export doc types
pub use doc::{
    Block, BlockId, BlockKind, CellContent, ColumnAlignment, ColumnDef, Document, EditError,
    EditOp, EquivalenceMode, InsertTextRun, Parser, RowId, SerializeConfig, Table, TableRow,
};

// Re-export doc mark operations
pub use doc::mark_ops;

// Re-export sync types
pub use sync::{
    ApplyResult, ChangeMessage, MalformedKind, Operation, SemanticConflict, SyncState,
    ValidationError, ValidationLimits, validate_changes,
};

// Re-export storage types (feature-gated)
#[cfg(feature = "storage")]
pub use storage::{CompactionReport, Storage, StorageError, TombstoneRetention};

// Re-export filesync types (feature-gated)
#[cfg(feature = "filesync")]
pub use filesync::{
    AddedBlock, ArchivedBlockFingerprint, BlockFingerprint, BlockMapping, BlockMatch, Fingerprint,
    IngestResult, LastFlushedState, MatchConfig, MatchType, ParsedBlock, Score, Vault, VaultError,
    match_blocks, parsed_blocks_from_doc,
};
