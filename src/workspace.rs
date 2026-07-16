//! Concrete, transport-agnostic workspace contract types.

use crate::core::mark::{Anchor, AnchorBias, MarkKind, MarkValue};
use crate::core::{OpId, Sequence};
use crate::doc::{
    Block, BlockId, BlockKind, ColumnAlignment, ColumnDef, Document, ListItem, RowId,
    block_text_seq, paragraph_visible_ids,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use uuid::Uuid;

macro_rules! persistent_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn from_u128(value: u128) -> Self {
                Self(Uuid::from_u128(value))
            }

            pub fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}

persistent_id!(VaultId);
persistent_id!(DocumentId);

/// Opaque precondition token for the observable state of one open document.
///
/// This is a content digest, not a sequence number: it is only ever compared for
/// equality (staleness preconditions). It deliberately does **not** implement
/// `Ord`/`PartialOrd`, since ordering two digests is meaningless — reverting to a
/// prior state yields the same token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionToken([u8; 16]);

impl RevisionToken {
    pub fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for RevisionToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Fingerprint of the Markdown bytes currently observed on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DiskFingerprint(pub u64);

impl fmt::Display for DiskFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

/// Stable handle returned when a document is opened or refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentHandle {
    pub vault_id: VaultId,
    pub document_id: DocumentId,
    pub revision: RevisionToken,
    pub disk_fingerprint: Option<DiskFingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockDescriptorKind {
    Paragraph,
    Heading,
    List,
    ListItem,
    CodeFence,
    BlockQuote,
    RawBlock,
    Table,
}

/// Body-free structural description for bounded workspace inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDescriptor {
    pub id: BlockId,
    pub parent: Option<BlockId>,
    pub order: u32,
    pub kind: BlockDescriptorKind,
    pub heading_level: Option<u8>,
    /// Byte length of this block's region in the last ingested/exported on-disk source.
    /// It reflects the pinned source span, **not** live in-memory edits — after a local
    /// edit `source_bytes` is stale until the next export re-pins the source. Use
    /// `text_bytes`/`node_digest` (which track live content) to detect edits.
    pub source_bytes: usize,
    pub text_bytes: usize,
    /// Semantic digest of this node only. Descendant content and source trivia are excluded.
    pub node_digest: u64,
    pub direct_child_count: u64,
    pub descendant_count: u64,
    /// Reserved for a measured, invalidation-safe subtree cache. The current node-local
    /// strategy deliberately omits it.
    pub subtree_digest: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DescriptorTraversal {
    DirectChildren,
}

/// Opaque continuation for a revision-bound descriptor traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorCursor {
    version: u8,
    document_id: DocumentId,
    revision: RevisionToken,
    parent: Option<BlockId>,
    traversal: DescriptorTraversal,
    last_id: BlockId,
    next_index: u64,
    next_order: u64,
    checksum: [u8; 16],
}

impl DescriptorCursor {
    const VERSION: u8 = 1;
    const WIRE_BYTES: usize = 99;

    fn new(
        document_id: DocumentId,
        revision: RevisionToken,
        parent: Option<BlockId>,
        traversal: DescriptorTraversal,
        last_id: BlockId,
        next_index: usize,
        next_order: usize,
    ) -> Self {
        let mut cursor = Self {
            version: Self::VERSION,
            document_id,
            revision,
            parent,
            traversal,
            last_id,
            next_index: u64::try_from(next_index).unwrap_or(u64::MAX),
            next_order: u64::try_from(next_order).unwrap_or(u64::MAX),
            checksum: [0; 16],
        };
        cursor.checksum = cursor.expected_checksum();
        cursor
    }

    fn expected_checksum(&self) -> [u8; 16] {
        let mut digest = StableDigest128::new();
        digest.bytes(&[self.version]);
        digest.bytes(self.document_id.as_uuid().as_bytes());
        digest.bytes(self.revision.as_bytes());
        match self.parent {
            Some(parent) => {
                digest.bytes(&[1]);
                digest.bytes(parent.as_bytes());
            }
            None => digest.bytes(&[0]),
        }
        digest.bytes(&[match self.traversal {
            DescriptorTraversal::DirectChildren => 0,
        }]);
        digest.bytes(self.last_id.as_bytes());
        digest.bytes(&self.next_index.to_le_bytes());
        digest.bytes(&self.next_order.to_le_bytes());
        digest.finish().to_be_bytes()
    }

    fn validate(
        &self,
        document_id: DocumentId,
        revision: &RevisionToken,
        parent: Option<BlockId>,
        traversal: DescriptorTraversal,
    ) -> Result<(), DescriptorError> {
        if self.version != Self::VERSION || self.checksum != self.expected_checksum() {
            return Err(DescriptorError::CorruptCursor);
        }
        if self.document_id != document_id {
            return Err(DescriptorError::CursorDocumentMismatch {
                expected: document_id,
                actual: self.document_id,
            });
        }
        if &self.revision != revision {
            return Err(DescriptorError::CursorRevisionMismatch {
                expected: revision.clone(),
                actual: self.revision.clone(),
            });
        }
        if self.parent != parent {
            return Err(DescriptorError::CursorParentMismatch {
                expected: parent,
                actual: self.parent,
            });
        }
        if self.traversal != traversal {
            return Err(DescriptorError::CursorTraversalMismatch);
        }
        Ok(())
    }

    fn wire_bytes(&self) -> [u8; Self::WIRE_BYTES] {
        let mut bytes = [0; Self::WIRE_BYTES];
        bytes[0] = self.version;
        bytes[1..17].copy_from_slice(self.document_id.as_uuid().as_bytes());
        bytes[17..33].copy_from_slice(self.revision.as_bytes());
        if let Some(parent) = self.parent {
            bytes[33] = 1;
            bytes[34..50].copy_from_slice(parent.as_bytes());
        }
        bytes[50] = match self.traversal {
            DescriptorTraversal::DirectChildren => 0,
        };
        bytes[51..67].copy_from_slice(self.last_id.as_bytes());
        bytes[67..75].copy_from_slice(&self.next_index.to_le_bytes());
        bytes[75..83].copy_from_slice(&self.next_order.to_le_bytes());
        bytes[83..99].copy_from_slice(&self.checksum);
        bytes
    }

    fn from_wire_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::WIRE_BYTES {
            return None;
        }
        let parent_bytes = fixed_bytes(bytes, 34..50)?;
        Some(Self {
            version: bytes[0],
            document_id: DocumentId::from_uuid(Uuid::from_bytes(fixed_bytes(bytes, 1..17)?)),
            revision: RevisionToken(fixed_bytes(bytes, 17..33)?),
            parent: (bytes[33] == 1).then(|| Uuid::from_bytes(parent_bytes)),
            traversal: DescriptorTraversal::DirectChildren,
            last_id: Uuid::from_bytes(fixed_bytes(bytes, 51..67)?),
            next_index: u64::from_le_bytes(fixed_bytes(bytes, 67..75)?),
            next_order: u64::from_le_bytes(fixed_bytes(bytes, 75..83)?),
            checksum: fixed_bytes(bytes, 83..99)?,
        })
    }
}

fn fixed_bytes<const N: usize>(bytes: &[u8], range: Range<usize>) -> Option<[u8; N]> {
    bytes.get(range)?.try_into().ok()
}

impl Serialize for DescriptorCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let bytes = self.wire_bytes();
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write;
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for DescriptorCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() != Self::WIRE_BYTES * 2 || !encoded.is_ascii() {
            return Err(serde::de::Error::custom(
                "invalid descriptor cursor encoding",
            ));
        }
        let mut bytes = Vec::with_capacity(Self::WIRE_BYTES);
        for pair in encoded.as_bytes().chunks_exact(2) {
            let text = std::str::from_utf8(pair)
                .map_err(|_| serde::de::Error::custom("invalid descriptor cursor encoding"))?;
            bytes.push(
                u8::from_str_radix(text, 16)
                    .map_err(|_| serde::de::Error::custom("invalid descriptor cursor encoding"))?,
            );
        }
        Self::from_wire_bytes(&bytes)
            .ok_or_else(|| serde::de::Error::custom("invalid descriptor cursor encoding"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    #[error("descriptor page limit must be greater than zero")]
    InvalidLimit,
    #[error("descriptor cursor is corrupt or has an unsupported encoding")]
    CorruptCursor,
    #[error("descriptor cursor document mismatch: expected {expected}, actual {actual}")]
    CursorDocumentMismatch {
        expected: DocumentId,
        actual: DocumentId,
    },
    #[error("descriptor cursor revision mismatch: expected {expected}, actual {actual}")]
    CursorRevisionMismatch {
        expected: RevisionToken,
        actual: RevisionToken,
    },
    #[error("descriptor cursor parent mismatch")]
    CursorParentMismatch {
        expected: Option<BlockId>,
        actual: Option<BlockId>,
    },
    #[error("descriptor cursor traversal mismatch")]
    CursorTraversalMismatch,
    #[error("descriptor cursor anchor no longer resolves: {block_id}")]
    CursorAnchorNotFound { block_id: BlockId },
    #[error("descriptor parent not found: {parent}")]
    ParentNotFound { parent: BlockId },
}

/// One bounded, revision-consistent page of direct children under a document or container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorPage {
    pub document_id: DocumentId,
    pub revision: RevisionToken,
    pub parent: Option<BlockId>,
    pub traversal: DescriptorTraversal,
    pub items: Vec<BlockDescriptor>,
    pub next_cursor: Option<DescriptorCursor>,
}

/// Bounded identities affected by one workspace transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSummary {
    pub created: Vec<BlockId>,
    pub deleted: Vec<BlockId>,
    pub moved: Vec<BlockId>,
    pub updated: Vec<BlockId>,
    pub affected_parents: Vec<BlockId>,
    pub affected_sections: Vec<BlockId>,
    pub operation_count: usize,
    pub revision: RevisionToken,
}

/// Result value and compact delta for one non-atomic local edit closure.
///
/// The value may itself be a `Result`; the summary always describes the state
/// left by the closure. Atomic preconditioned batches use a separate API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEditOutcome<T> {
    pub value: T,
    pub changes: ChangeSummary,
}

/// Remote integration result plus the bounded document delta it produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteApplyOutcome {
    pub applied: Vec<OpId>,
    pub buffered: Vec<OpId>,
    pub changes: ChangeSummary,
}

/// Stable position within one text-bearing block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextPosition {
    Start,
    End,
    Unit(Anchor),
}

/// Stable text position tied to one logical block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPoint {
    pub block_id: BlockId,
    pub position: TextPosition,
}

/// Stable half-open text range. Both endpoints must address the same block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start: TextPoint,
    pub end: TextPoint,
}

/// Compact field selector for owned block projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectionFields(u16);

impl ProjectionFields {
    pub const NONE: Self = Self(0);
    pub const KIND: Self = Self(1 << 0);
    pub const TEXT: Self = Self(1 << 1);
    pub const MARKS: Self = Self(1 << 2);
    pub const STRUCTURE: Self = Self(1 << 3);
    pub const CONTENT_DIGEST: Self = Self(1 << 4);
    pub const TEXT_POINTS: Self = Self(1 << 5);
    pub const EXACT_MARKDOWN: Self = Self(1 << 6);
    pub const ALL: Self = Self(
        Self::KIND.0
            | Self::TEXT.0
            | Self::MARKS.0
            | Self::STRUCTURE.0
            | Self::CONTENT_DIGEST.0
            | Self::TEXT_POINTS.0
            | Self::EXACT_MARKDOWN.0,
    );
    pub const MINIMAL: Self = Self(Self::KIND.0 | Self::CONTENT_DIGEST.0);
    pub const SEMANTIC: Self = Self(
        Self::KIND.0
            | Self::TEXT.0
            | Self::MARKS.0
            | Self::STRUCTURE.0
            | Self::CONTENT_DIGEST.0
            | Self::TEXT_POINTS.0,
    );
    pub const EXACT: Self = Self(Self::EXACT_MARKDOWN.0);

    pub const fn contains(self, field: Self) -> bool {
        self.0 & field.0 == field.0
    }

    const fn is_valid(self) -> bool {
        self.0 & !Self::ALL.0 == 0
    }
}

impl std::ops::BitOr for ProjectionFields {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Stateless continuation bound to one exact projection request shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectionContinuation {
    request_digest: [u8; 16],
    offset: u64,
}

/// Bounded, ordered projection request for selected logical block identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionRequest {
    pub document_id: DocumentId,
    pub base_revision: RevisionToken,
    pub block_ids: Vec<BlockId>,
    pub fields: ProjectionFields,
    pub max_items: usize,
    pub max_bytes: usize,
    pub continuation: Option<ProjectionContinuation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockProjectionKind {
    Paragraph,
    Heading { level: u8 },
    List { ordered: bool },
    ListItem,
    CodeFence { info: Option<String> },
    BlockQuote,
    RawBlock,
    Table,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedTableRow {
    pub id: RowId,
    pub cells: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockProjectionStructure {
    Children {
        block_ids: Vec<BlockId>,
    },
    ListItems {
        item_ids: Vec<BlockId>,
    },
    Table {
        columns: Vec<ColumnDef>,
        header: Vec<String>,
        rows: Vec<ProjectedTableRow>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedMark {
    pub range: TextRange,
    pub kind: MarkKind,
    pub attrs: BTreeMap<String, MarkValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactMarkdownProjection {
    pub owner_block_id: BlockId,
    pub markdown: String,
}

/// Owned semantic view of one selected block or list-item identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockProjection {
    pub id: BlockId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<BlockProjectionKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marks: Option<Vec<ProjectedMark>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structure: Option<BlockProjectionStructure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_ranges: Option<Vec<TextRange>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact: Option<ExactMarkdownProjection>,
}

/// One hard-bounded serialized page of selected block projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionPage {
    pub document_id: DocumentId,
    pub revision: RevisionToken,
    pub items: Vec<BlockProjection>,
    pub omitted_ids: Vec<BlockId>,
    pub continuation: Option<ProjectionContinuation>,
    pub bytes_used: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    #[error("projection limits must both be greater than zero")]
    InvalidLimits,
    #[error("projection field mask contains unknown fields")]
    UnknownFields,
    #[error("duplicate projection block id: {block_id}")]
    DuplicateBlockId { block_id: BlockId },
    #[error("projection continuation does not match the request")]
    InvalidContinuation,
    #[error("projection item {block_id} requires {required_bytes} bytes, limit is {max_bytes}")]
    ItemTooLarge {
        block_id: BlockId,
        required_bytes: usize,
        max_bytes: usize,
    },
    #[error("projection page requires {required_bytes} bytes, limit is {max_bytes}")]
    PageTooLarge {
        block_id: Option<BlockId>,
        required_bytes: usize,
        max_bytes: usize,
    },
    #[error("projection serialization failed")]
    Serialization,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceTargetError {
    #[error("block not found: {block_id}")]
    BlockNotFound { block_id: BlockId },
    #[error("block is not text-bearing: {block_id}")]
    NotText { block_id: BlockId },
    #[error("text offset {offset} is outside block {block_id}")]
    InvalidOffset { block_id: BlockId, offset: usize },
    #[error("text range endpoints must address the same block in forward order")]
    InvalidRange,
    #[error("anchor {anchor:?} belongs to block {actual_block_id}, not {block_id}")]
    WrongBlock {
        block_id: BlockId,
        actual_block_id: BlockId,
        anchor: OpId,
    },
    #[error("anchor {anchor:?} was deleted from block {block_id}")]
    DeletedAnchor { block_id: BlockId, anchor: OpId },
    #[error("anchor {anchor:?} is unknown in block {block_id}")]
    UnknownAnchor { block_id: BlockId, anchor: OpId },
    #[error("anchor {anchor:?} has multiple candidate blocks when resolving from {block_id}")]
    AmbiguousAnchor {
        block_id: BlockId,
        anchor: OpId,
        candidate_blocks: Vec<BlockId>,
    },
    #[error("workspace target precondition no longer matches")]
    PreconditionMismatch,
}

/// One scoped optimistic-concurrency assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetPrecondition {
    Text {
        range: TextRange,
        content_digest: u64,
    },
    Block {
        block_id: BlockId,
        content_digest: u64,
    },
    Placement {
        block_id: Option<BlockId>,
        parent: Option<BlockId>,
        after: Option<BlockId>,
        structural_digest: u64,
    },
}

/// Stable, identity-targeted edit operations accepted by atomic workspace batches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceEdit {
    InsertParagraph {
        parent: Option<BlockId>,
        after: Option<BlockId>,
        text: String,
    },
    InsertHeading {
        parent: Option<BlockId>,
        after: Option<BlockId>,
        level: u8,
        text: String,
    },
    DeleteBlock {
        block_id: BlockId,
    },
    InsertText {
        at: TextPoint,
        text: String,
    },
    DeleteText {
        range: TextRange,
    },
    SetMark {
        range: TextRange,
        kind: MarkKind,
        attrs: BTreeMap<String, MarkValue>,
    },
    RemoveMark {
        block_id: BlockId,
        interval_id: OpId,
    },
    SetFrontmatterField {
        key: String,
        value: Option<String>,
    },
    MoveBlock {
        block_id: BlockId,
        parent: Option<BlockId>,
        after: Option<BlockId>,
    },
    MoveSection {
        heading_id: BlockId,
        after: Option<BlockId>,
    },
    SplitBlock {
        at: TextPoint,
    },
    MergeBlocks {
        left_id: BlockId,
        right_id: BlockId,
    },
    InsertTable {
        parent: Option<BlockId>,
        after: Option<BlockId>,
        columns: Vec<ColumnDef>,
        header: Vec<String>,
    },
    InsertTableRow {
        table_id: BlockId,
        after: Option<RowId>,
        cells: Vec<String>,
    },
    SetTableRowCells {
        table_id: BlockId,
        row_id: RowId,
        cells: Vec<String>,
    },
    DeleteTableRow {
        table_id: BlockId,
        row_id: RowId,
    },
    SetTableMetadata {
        table_id: BlockId,
        columns: Vec<ColumnDef>,
        header: Vec<String>,
    },
    MoveTableRow {
        table_id: BlockId,
        row_id: RowId,
        after: Option<RowId>,
    },
}

/// One ordered workspace operation and its scoped concurrency assertions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMutation {
    pub edit: WorkspaceEdit,
    pub preconditions: Vec<TargetPrecondition>,
}

impl WorkspaceMutation {
    pub fn strict(edit: WorkspaceEdit) -> Self {
        Self {
            edit,
            preconditions: Vec::new(),
        }
    }

    pub fn scoped(edit: WorkspaceEdit, preconditions: Vec<TargetPrecondition>) -> Self {
        Self {
            edit,
            preconditions,
        }
    }
}

/// Concrete single-document batch applied against a base revision and scoped targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditBatch {
    pub document_id: DocumentId,
    pub base_revision: RevisionToken,
    pub operations: Vec<WorkspaceMutation>,
}

/// Opaque binding of an exact revision to an exact batch operation sequence.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PreviewToken([u8; 16]);

impl PreviewToken {
    pub(crate) fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }
}

impl fmt::Display for PreviewToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchPreview {
    pub document_id: DocumentId,
    pub revision: RevisionToken,
    pub token: PreviewToken,
    pub changes: ChangeSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentEditBatch {
    pub path: std::path::PathBuf,
    pub batch: EditBatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiBatchReceipt {
    pub receipts: Vec<BatchReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentExportRequest {
    pub path: std::path::PathBuf,
    pub document_id: DocumentId,
    pub expected_revision: RevisionToken,
    pub expected_disk_fingerprint: Option<DiskFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiExportOutcome {
    pub documents: Vec<ExportOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedDocument {
    pub document_id: DocumentId,
    pub path: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub transactions_recovered: usize,
    pub files_recovered: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReceipt {
    pub document_id: DocumentId,
    pub previous_revision: RevisionToken,
    pub revision: RevisionToken,
    pub changes: ChangeSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOutcome {
    pub document_id: DocumentId,
    pub revision: RevisionToken,
    pub disk_fingerprint: Option<DiskFingerprint>,
    pub bytes_written: usize,
    pub changed: bool,
    pub changes: ChangeSummary,
}

enum DescriptorChildren<'a> {
    Blocks(&'a Sequence<Block>),
    Items(&'a Sequence<ListItem>),
    Empty,
}

impl Document {
    pub fn text_point(
        &self,
        block_id: BlockId,
        grapheme_offset: usize,
    ) -> Result<TextPoint, WorkspaceTargetError> {
        let text = self.text_sequence(block_id)?;
        let ids = paragraph_visible_ids(text);
        if grapheme_offset > ids.len() {
            return Err(WorkspaceTargetError::InvalidOffset {
                block_id,
                offset: grapheme_offset,
            });
        }
        let position = if grapheme_offset == 0 {
            TextPosition::Start
        } else if grapheme_offset == ids.len() {
            TextPosition::End
        } else {
            TextPosition::Unit(Anchor {
                elem_id: ids[grapheme_offset],
                bias: AnchorBias::Before,
            })
        };
        Ok(TextPoint { block_id, position })
    }

    pub fn text_range(
        &self,
        block_id: BlockId,
        range: Range<usize>,
    ) -> Result<TextRange, WorkspaceTargetError> {
        let text = self.text_sequence(block_id)?;
        text_range_for_sequence(block_id, text, range)
    }

    pub fn resolve_text_point(&self, point: &TextPoint) -> Result<usize, WorkspaceTargetError> {
        let text = self.text_sequence(point.block_id)?;
        match point.position {
            TextPosition::Start => Ok(0),
            TextPosition::End => Ok(text.len_visible()),
            TextPosition::Unit(anchor) => {
                let Some(element) = text.get_element(&anchor.elem_id) else {
                    let candidate_blocks = self.text_unit_owners(anchor.elem_id);
                    if candidate_blocks.len() > 1 {
                        return Err(WorkspaceTargetError::AmbiguousAnchor {
                            block_id: point.block_id,
                            anchor: anchor.elem_id,
                            candidate_blocks,
                        });
                    }
                    if let Some(actual_block_id) = candidate_blocks.first().copied() {
                        return Err(WorkspaceTargetError::WrongBlock {
                            block_id: point.block_id,
                            actual_block_id,
                            anchor: anchor.elem_id,
                        });
                    }
                    return Err(WorkspaceTargetError::UnknownAnchor {
                        block_id: point.block_id,
                        anchor: anchor.elem_id,
                    });
                };
                if element.value.is_none() {
                    return Err(WorkspaceTargetError::DeletedAnchor {
                        block_id: point.block_id,
                        anchor: anchor.elem_id,
                    });
                }
                let ids = paragraph_visible_ids(text);
                let index = ids.iter().position(|id| *id == anchor.elem_id).ok_or(
                    WorkspaceTargetError::DeletedAnchor {
                        block_id: point.block_id,
                        anchor: anchor.elem_id,
                    },
                )?;
                Ok(match anchor.bias {
                    AnchorBias::Before => index,
                    AnchorBias::After => index + 1,
                })
            }
        }
    }

    pub fn resolve_text_range(
        &self,
        range: &TextRange,
    ) -> Result<Range<usize>, WorkspaceTargetError> {
        if range.start.block_id != range.end.block_id {
            return Err(WorkspaceTargetError::InvalidRange);
        }
        let start = self.resolve_text_point(&range.start)?;
        let end = self.resolve_text_point(&range.end)?;
        if start > end {
            return Err(WorkspaceTargetError::InvalidRange);
        }
        Ok(start..end)
    }

    pub fn preconditions_for_edit(
        &self,
        edit: &WorkspaceEdit,
    ) -> Result<Vec<TargetPrecondition>, WorkspaceTargetError> {
        let mut preconditions = Vec::new();
        match edit {
            WorkspaceEdit::InsertParagraph { parent, after, .. }
            | WorkspaceEdit::InsertHeading { parent, after, .. }
            | WorkspaceEdit::InsertTable { parent, after, .. } => {
                preconditions.push(self.placement_precondition(None, *parent, *after)?);
            }
            WorkspaceEdit::DeleteBlock { block_id } => {
                preconditions.push(self.block_precondition(*block_id)?);
                preconditions.push(self.current_placement_precondition(*block_id)?);
            }
            WorkspaceEdit::InsertText { at, .. } | WorkspaceEdit::SplitBlock { at } => {
                self.resolve_text_point(at)?;
                preconditions.push(self.block_precondition(at.block_id)?);
            }
            WorkspaceEdit::DeleteText { range } | WorkspaceEdit::SetMark { range, .. } => {
                preconditions.push(self.text_precondition(*range)?);
            }
            WorkspaceEdit::RemoveMark { block_id, .. } => {
                preconditions.push(self.block_precondition(*block_id)?);
            }
            WorkspaceEdit::SetFrontmatterField { .. } => {}
            WorkspaceEdit::MoveBlock {
                block_id,
                parent,
                after,
            } => {
                preconditions.push(self.block_precondition(*block_id)?);
                preconditions.push(self.current_placement_precondition(*block_id)?);
                preconditions.push(self.placement_precondition(None, *parent, *after)?);
            }
            WorkspaceEdit::MoveSection { heading_id, after } => {
                preconditions.push(self.block_precondition(*heading_id)?);
                let source = self.current_placement_precondition(*heading_id)?;
                let TargetPrecondition::Placement { parent, .. } = &source else {
                    return Err(WorkspaceTargetError::PreconditionMismatch);
                };
                let parent = *parent;
                preconditions.push(source);
                preconditions.push(self.placement_precondition(None, parent, *after)?);
            }
            WorkspaceEdit::MergeBlocks { left_id, right_id } => {
                preconditions.push(self.block_precondition(*left_id)?);
                preconditions.push(self.block_precondition(*right_id)?);
                preconditions.push(self.current_placement_precondition(*left_id)?);
                preconditions.push(self.current_placement_precondition(*right_id)?);
            }
            WorkspaceEdit::InsertTableRow { table_id, .. }
            | WorkspaceEdit::SetTableRowCells { table_id, .. }
            | WorkspaceEdit::DeleteTableRow { table_id, .. }
            | WorkspaceEdit::SetTableMetadata { table_id, .. }
            | WorkspaceEdit::MoveTableRow { table_id, .. } => {
                preconditions.push(self.block_precondition(*table_id)?);
            }
        }
        Ok(preconditions)
    }

    pub(crate) fn projection_page(
        &self,
        request: &ProjectionRequest,
        revision: RevisionToken,
    ) -> Result<ProjectionPage, ProjectionError> {
        if request.max_items == 0 || request.max_bytes == 0 {
            return Err(ProjectionError::InvalidLimits);
        }
        if !request.fields.is_valid() {
            return Err(ProjectionError::UnknownFields);
        }
        let mut unique = BTreeSet::new();
        for block_id in &request.block_ids {
            if !unique.insert(*block_id) {
                return Err(ProjectionError::DuplicateBlockId {
                    block_id: *block_id,
                });
            }
        }
        let request_digest = projection_request_digest(request)?;
        let offset = match &request.continuation {
            Some(continuation) if continuation.request_digest == request_digest => {
                usize::try_from(continuation.offset)
                    .ok()
                    .filter(|offset| *offset <= request.block_ids.len())
                    .ok_or(ProjectionError::InvalidContinuation)?
            }
            Some(_) => return Err(ProjectionError::InvalidContinuation),
            None => 0,
        };
        let page_targets: BTreeSet<_> = request.block_ids[offset..]
            .iter()
            .take(request.max_items)
            .copied()
            .collect();
        let mut selected_nodes = BTreeMap::new();
        collect_projection_nodes(self.blocks(), &page_targets, &mut selected_nodes);
        let mut page = ProjectionPage {
            document_id: request.document_id,
            revision,
            items: Vec::with_capacity(request.max_items.min(request.block_ids.len() - offset)),
            omitted_ids: Vec::new(),
            continuation: projection_continuation(request_digest, offset, request.block_ids.len()),
            bytes_used: 0,
        };
        stabilize_projection_bytes(&mut page)?;
        if page.bytes_used > request.max_bytes {
            return Err(ProjectionError::PageTooLarge {
                block_id: request.block_ids.get(offset).copied(),
                required_bytes: page.bytes_used,
                max_bytes: request.max_bytes,
            });
        }

        let mut index = offset;
        let mut processed = 0usize;
        while index < request.block_ids.len() && processed < request.max_items {
            let block_id = request.block_ids[index];
            let projection = selected_nodes
                .get(&block_id)
                .copied()
                .map(|node| self.project_node(node, request.fields))
                .transpose()?;
            let found = projection.is_some();
            match projection {
                Some(projection) => page.items.push(projection),
                None => page.omitted_ids.push(block_id),
            }
            index += 1;
            page.continuation =
                projection_continuation(request_digest, index, request.block_ids.len());
            stabilize_projection_bytes(&mut page)?;
            if page.bytes_used > request.max_bytes {
                if found {
                    page.items.pop();
                } else {
                    page.omitted_ids.pop();
                }
                index -= 1;
                page.continuation =
                    projection_continuation(request_digest, index, request.block_ids.len());
                let required_bytes = page.bytes_used;
                stabilize_projection_bytes(&mut page)?;
                if processed == 0 {
                    return Err(ProjectionError::ItemTooLarge {
                        block_id,
                        required_bytes,
                        max_bytes: request.max_bytes,
                    });
                }
                break;
            }
            processed += 1;
        }
        page.continuation = projection_continuation(request_digest, index, request.block_ids.len());
        stabilize_projection_bytes(&mut page)?;
        Ok(page)
    }

    fn project_node(
        &self,
        node: ProjectionNode<'_>,
        fields: ProjectionFields,
    ) -> Result<BlockProjection, ProjectionError> {
        let id = node.id();
        let kind = fields
            .contains(ProjectionFields::KIND)
            .then(|| projection_kind(node));
        let text = fields
            .contains(ProjectionFields::TEXT)
            .then(|| projection_visible_text(node));
        let marks = fields
            .contains(ProjectionFields::MARKS)
            .then(|| {
                let mut marks = Vec::new();
                collect_projection_marks(node, &mut marks)?;
                Ok(marks)
            })
            .transpose()?;
        let structure = fields
            .contains(ProjectionFields::STRUCTURE)
            .then(|| projection_structure(node))
            .flatten();
        let content_digest = fields
            .contains(ProjectionFields::CONTENT_DIGEST)
            .then(|| projection_node_digest(node));
        let text_ranges = fields
            .contains(ProjectionFields::TEXT_POINTS)
            .then(|| {
                let mut ranges = Vec::new();
                collect_projection_text_ranges(node, &mut ranges)?;
                Ok(ranges)
            })
            .transpose()?;
        let exact = fields
            .contains(ProjectionFields::EXACT_MARKDOWN)
            .then(|| {
                self.projection_exact_region(id)
                    .map(|(owner_block_id, markdown)| ExactMarkdownProjection {
                        owner_block_id,
                        markdown,
                    })
            })
            .flatten();
        Ok(BlockProjection {
            id,
            kind,
            text,
            marks,
            structure,
            content_digest,
            text_ranges,
            exact,
        })
    }

    fn text_sequence(
        &self,
        block_id: BlockId,
    ) -> Result<&Sequence<crate::doc::TextUnit>, WorkspaceTargetError> {
        let block = self
            .find_block_by_id(block_id)
            .ok_or(WorkspaceTargetError::BlockNotFound { block_id })?;
        block_text_seq(&block.kind).ok_or(WorkspaceTargetError::NotText { block_id })
    }

    fn text_unit_owners(&self, elem_id: OpId) -> Vec<BlockId> {
        capture_outline(self)
            .entries
            .keys()
            .filter_map(|block_id| {
                let block = self.find_block_by_id(*block_id)?;
                block_text_seq(&block.kind)
                    .and_then(|text| text.get_element(&elem_id))
                    .map(|_| *block_id)
            })
            .collect()
    }

    fn block_precondition(
        &self,
        block_id: BlockId,
    ) -> Result<TargetPrecondition, WorkspaceTargetError> {
        let block = self
            .find_block_by_id(block_id)
            .ok_or(WorkspaceTargetError::BlockNotFound { block_id })?;
        Ok(TargetPrecondition::Block {
            block_id,
            content_digest: block_digest(block),
        })
    }

    fn text_precondition(
        &self,
        range: TextRange,
    ) -> Result<TargetPrecondition, WorkspaceTargetError> {
        Ok(TargetPrecondition::Text {
            range,
            content_digest: text_range_digest(self, &range)?,
        })
    }

    fn current_placement_precondition(
        &self,
        block_id: BlockId,
    ) -> Result<TargetPrecondition, WorkspaceTargetError> {
        let outline = capture_outline(self);
        let entry = outline
            .entries
            .get(&block_id)
            .ok_or(WorkspaceTargetError::BlockNotFound { block_id })?;
        let parent = entry.descriptor.parent;
        let after = outline
            .entries
            .values()
            .filter(|candidate| {
                candidate.descriptor.parent == parent
                    && candidate.descriptor.order < entry.descriptor.order
            })
            .max_by_key(|candidate| candidate.descriptor.order)
            .map(|candidate| candidate.descriptor.id);
        self.placement_precondition(Some(block_id), parent, after)
    }

    fn placement_precondition(
        &self,
        block_id: Option<BlockId>,
        parent: Option<BlockId>,
        after: Option<BlockId>,
    ) -> Result<TargetPrecondition, WorkspaceTargetError> {
        let outline = capture_outline(self);
        if let Some(parent_id) = parent {
            match find_descriptor_children(self.blocks(), parent_id) {
                Some(DescriptorChildren::Blocks(_)) => {}
                Some(DescriptorChildren::Items(_) | DescriptorChildren::Empty) => {
                    return Err(WorkspaceTargetError::PreconditionMismatch);
                }
                None => {
                    return Err(WorkspaceTargetError::BlockNotFound {
                        block_id: parent_id,
                    });
                }
            }
        }
        if let Some(after_id) = after {
            let after_entry = outline
                .entries
                .get(&after_id)
                .ok_or(WorkspaceTargetError::BlockNotFound { block_id: after_id })?;
            if after_entry.descriptor.parent != parent {
                return Err(WorkspaceTargetError::PreconditionMismatch);
            }
        }
        Ok(TargetPrecondition::Placement {
            block_id,
            parent,
            after,
            structural_digest: placement_digest(&outline, parent),
        })
    }

    /// Return at most `limit` body-free descriptors for direct children of `parent`.
    ///
    /// `None` addresses the document root. A blockquote or list item exposes block
    /// children; a list exposes list-item descriptors. Continuations are bound to
    /// this document identity, revision, parent, and traversal order.
    pub(crate) fn descriptor_page(
        &self,
        document_id: DocumentId,
        revision: RevisionToken,
        parent: Option<BlockId>,
        cursor: Option<&DescriptorCursor>,
        limit: usize,
    ) -> Result<DescriptorPage, DescriptorError> {
        if limit == 0 {
            return Err(DescriptorError::InvalidLimit);
        }
        let children = match parent {
            None => DescriptorChildren::Blocks(self.blocks()),
            Some(parent) => find_descriptor_children(self.blocks(), parent)
                .ok_or(DescriptorError::ParentNotFound { parent })?,
        };
        let traversal = DescriptorTraversal::DirectChildren;
        let (start_physical, start_order) = match cursor {
            Some(cursor) => {
                cursor.validate(document_id, &revision, parent, traversal)?;
                (
                    usize::try_from(cursor.next_index)
                        .map_err(|_| DescriptorError::CorruptCursor)?,
                    usize::try_from(cursor.next_order)
                        .map_err(|_| DescriptorError::CorruptCursor)?,
                )
            }
            None => (0, 0),
        };
        if let Some(cursor) = cursor {
            let prior = start_physical
                .checked_sub(1)
                .and_then(|index| match children {
                    DescriptorChildren::Blocks(blocks) => {
                        blocks.visible_at_physical(index).map(|block| block.id)
                    }
                    DescriptorChildren::Items(items) => {
                        items.visible_at_physical(index).map(|item| item.id)
                    }
                    DescriptorChildren::Empty => None,
                });
            if prior != Some(cursor.last_id) {
                return Err(DescriptorError::CursorAnchorNotFound {
                    block_id: cursor.last_id,
                });
            }
        }
        let take = limit.saturating_add(1);
        let mut indexed_items: Vec<(usize, BlockDescriptor)> = match children {
            DescriptorChildren::Blocks(blocks) => blocks
                .iter_visible_from_physical(start_physical)
                .enumerate()
                .take(take)
                .map(|(local_order, (physical, block))| {
                    (
                        physical,
                        block_descriptor(
                            self,
                            block,
                            parent,
                            start_order.saturating_add(local_order),
                        ),
                    )
                })
                .collect(),
            DescriptorChildren::Items(items) => items
                .iter_visible_from_physical(start_physical)
                .enumerate()
                .take(take)
                .map(|(local_order, (physical, item))| {
                    (
                        physical,
                        list_item_descriptor(item, parent, start_order.saturating_add(local_order)),
                    )
                })
                .collect(),
            DescriptorChildren::Empty => Vec::new(),
        };
        let has_more = indexed_items.len() > limit;
        if has_more {
            indexed_items.pop();
        }
        let returned = indexed_items.len();
        let next_cursor = has_more.then(|| {
            let (physical, descriptor) = indexed_items
                .last()
                .expect("a continuation follows a non-empty page");
            DescriptorCursor::new(
                document_id,
                revision.clone(),
                parent,
                traversal,
                descriptor.id,
                physical.saturating_add(1),
                start_order.saturating_add(returned),
            )
        });
        let items = indexed_items
            .into_iter()
            .map(|(_, descriptor)| descriptor)
            .collect();
        Ok(DescriptorPage {
            document_id,
            revision,
            parent,
            traversal,
            items,
            next_cursor,
        })
    }
}

#[derive(Clone, Copy)]
enum ProjectionNode<'a> {
    Block(&'a Block),
    ListItem(&'a ListItem),
}

impl ProjectionNode<'_> {
    fn id(self) -> BlockId {
        match self {
            Self::Block(block) => block.id,
            Self::ListItem(item) => item.id,
        }
    }
}

fn collect_projection_nodes<'a>(
    blocks: &'a Sequence<Block>,
    targets: &BTreeSet<BlockId>,
    selected: &mut BTreeMap<BlockId, ProjectionNode<'a>>,
) {
    if selected.len() == targets.len() {
        return;
    }
    for block in blocks.iter() {
        if targets.contains(&block.id) {
            selected.insert(block.id, ProjectionNode::Block(block));
        }
        if selected.len() == targets.len() {
            return;
        }
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                collect_projection_nodes(children, targets, selected);
                if selected.len() == targets.len() {
                    return;
                }
            }
            BlockKind::List { items, .. } => {
                for item in items.iter() {
                    if targets.contains(&item.id) {
                        selected.insert(item.id, ProjectionNode::ListItem(item));
                    }
                    if selected.len() == targets.len() {
                        return;
                    }
                    collect_projection_nodes(&item.children, targets, selected);
                    if selected.len() == targets.len() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

fn projection_kind(node: ProjectionNode<'_>) -> BlockProjectionKind {
    match node {
        ProjectionNode::ListItem(_) => BlockProjectionKind::ListItem,
        ProjectionNode::Block(block) => match &block.kind {
            BlockKind::Paragraph { .. } => BlockProjectionKind::Paragraph,
            BlockKind::Heading { level, .. } => BlockProjectionKind::Heading { level: *level },
            BlockKind::List { ordered, .. } => BlockProjectionKind::List { ordered: *ordered },
            BlockKind::CodeFence { info, .. } => {
                BlockProjectionKind::CodeFence { info: info.clone() }
            }
            BlockKind::BlockQuote { .. } => BlockProjectionKind::BlockQuote,
            BlockKind::RawBlock { .. } => BlockProjectionKind::RawBlock,
            BlockKind::Table { .. } => BlockProjectionKind::Table,
        },
    }
}

fn projection_visible_text(node: ProjectionNode<'_>) -> String {
    match node {
        ProjectionNode::ListItem(item) => projection_blocks_text(&item.children),
        ProjectionNode::Block(block) => match &block.kind {
            BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
                crate::doc::paragraph_visible_string(text)
            }
            BlockKind::List { items, .. } => items
                .iter()
                .map(|item| projection_blocks_text(&item.children))
                .collect::<Vec<_>>()
                .join("\n"),
            BlockKind::CodeFence { text, .. } => text.clone(),
            BlockKind::BlockQuote { children } => projection_blocks_text(children),
            BlockKind::RawBlock { raw } => raw.clone(),
            BlockKind::Table { table } => {
                table.header.get_ref().join("\t")
                    + &table
                        .rows
                        .iter()
                        .filter(|row| !*row.deleted.get_ref())
                        .map(|row| format!("\n{}", row.cells.get_ref().join("\t")))
                        .collect::<String>()
            }
        },
    }
}

fn projection_blocks_text(blocks: &Sequence<Block>) -> String {
    blocks
        .iter()
        .map(|block| projection_visible_text(ProjectionNode::Block(block)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn projection_structure(node: ProjectionNode<'_>) -> Option<BlockProjectionStructure> {
    match node {
        ProjectionNode::ListItem(item) => Some(BlockProjectionStructure::Children {
            block_ids: item.children.iter().map(|block| block.id).collect(),
        }),
        ProjectionNode::Block(block) => match &block.kind {
            BlockKind::BlockQuote { children } => Some(BlockProjectionStructure::Children {
                block_ids: children.iter().map(|child| child.id).collect(),
            }),
            BlockKind::List { items, .. } => Some(BlockProjectionStructure::ListItems {
                item_ids: items.iter().map(|item| item.id).collect(),
            }),
            BlockKind::Table { table } => Some(BlockProjectionStructure::Table {
                columns: table.columns.get_ref().clone(),
                header: table.header.get_ref().clone(),
                rows: table
                    .rows
                    .iter()
                    .filter(|row| !*row.deleted.get_ref())
                    .map(|row| ProjectedTableRow {
                        id: row.id,
                        cells: row.cells.get_ref().clone(),
                    })
                    .collect(),
            }),
            _ => None,
        },
    }
}

fn text_range_for_sequence(
    block_id: BlockId,
    text: &Sequence<crate::doc::TextUnit>,
    range: Range<usize>,
) -> Result<TextRange, WorkspaceTargetError> {
    let ids = paragraph_visible_ids(text);
    if range.start > range.end || range.end > ids.len() {
        return Err(WorkspaceTargetError::InvalidRange);
    }
    let point = |offset: usize, end: bool| TextPoint {
        block_id,
        position: if offset == 0 {
            TextPosition::Start
        } else if offset == ids.len() {
            TextPosition::End
        } else {
            TextPosition::Unit(Anchor {
                elem_id: ids[if end { offset - 1 } else { offset }],
                bias: if end {
                    AnchorBias::After
                } else {
                    AnchorBias::Before
                },
            })
        },
    };
    Ok(TextRange {
        start: point(range.start, false),
        end: point(range.end, true),
    })
}

fn collect_projection_marks(
    node: ProjectionNode<'_>,
    output: &mut Vec<ProjectedMark>,
) -> Result<(), ProjectionError> {
    match node {
        ProjectionNode::ListItem(item) => {
            for child in item.children.iter() {
                collect_projection_marks(ProjectionNode::Block(child), output)?;
            }
        }
        ProjectionNode::Block(block) => {
            if let Some(text) = block_text_seq(&block.kind) {
                let ids = paragraph_visible_ids(text);
                for (interval, start, end) in block.marks.resolved_intervals(&ids) {
                    let range = text_range_for_sequence(block.id, text, start..end)
                        .map_err(|_| ProjectionError::Serialization)?;
                    output.push(ProjectedMark {
                        range,
                        kind: interval.kind.clone(),
                        attrs: interval
                            .attrs
                            .iter()
                            .map(|(key, value)| (key.clone(), value.get_ref().clone()))
                            .collect(),
                    });
                }
            }
            match &block.kind {
                BlockKind::BlockQuote { children } => {
                    for child in children.iter() {
                        collect_projection_marks(ProjectionNode::Block(child), output)?;
                    }
                }
                BlockKind::List { items, .. } => {
                    for item in items.iter() {
                        collect_projection_marks(ProjectionNode::ListItem(item), output)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn collect_projection_text_ranges(
    node: ProjectionNode<'_>,
    output: &mut Vec<TextRange>,
) -> Result<(), ProjectionError> {
    match node {
        ProjectionNode::ListItem(item) => {
            for child in item.children.iter() {
                collect_projection_text_ranges(ProjectionNode::Block(child), output)?;
            }
        }
        ProjectionNode::Block(block) => {
            if let Some(text) = block_text_seq(&block.kind) {
                output.push(
                    text_range_for_sequence(block.id, text, 0..text.len_visible())
                        .map_err(|_| ProjectionError::Serialization)?,
                );
            }
            match &block.kind {
                BlockKind::BlockQuote { children } => {
                    for child in children.iter() {
                        collect_projection_text_ranges(ProjectionNode::Block(child), output)?;
                    }
                }
                BlockKind::List { items, .. } => {
                    for item in items.iter() {
                        collect_projection_text_ranges(ProjectionNode::ListItem(item), output)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn projection_node_digest(node: ProjectionNode<'_>) -> u64 {
    match node {
        ProjectionNode::Block(block) => block_digest(block),
        ProjectionNode::ListItem(item) => list_item_digest(item),
    }
}

fn list_item_digest(item: &ListItem) -> u64 {
    let mut digest = StableDigest::new();
    digest.field(b"list-item-subtree");
    digest.field(&list_item_node_digest(item).to_le_bytes());
    for child in item.children.iter() {
        digest.field(&block_digest(child).to_le_bytes());
    }
    digest.finish()
}

fn projection_request_digest(request: &ProjectionRequest) -> Result<[u8; 16], ProjectionError> {
    let encoded = serde_json::to_vec(&(
        request.document_id,
        &request.base_revision,
        &request.block_ids,
        request.fields,
        request.max_items,
        request.max_bytes,
    ))
    .map_err(|_| ProjectionError::Serialization)?;
    Ok(stable_hash_128(&encoded).to_be_bytes())
}

struct StableDigest128(u128);

impl StableDigest128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u128::from(*byte)).wrapping_mul(Self::PRIME);
        }
    }

    fn finish(self) -> u128 {
        self.0
    }
}

pub(crate) fn stable_hash_128(bytes: &[u8]) -> u128 {
    let mut digest = StableDigest128::new();
    digest.bytes(bytes);
    digest.finish()
}

fn projection_continuation(
    request_digest: [u8; 16],
    offset: usize,
    total: usize,
) -> Option<ProjectionContinuation> {
    (offset < total).then(|| ProjectionContinuation {
        request_digest,
        offset: u64::try_from(offset).unwrap_or(u64::MAX),
    })
}

fn stabilize_projection_bytes(page: &mut ProjectionPage) -> Result<(), ProjectionError> {
    for _ in 0..usize::BITS {
        let bytes = serde_json::to_vec(page)
            .map_err(|_| ProjectionError::Serialization)?
            .len();
        if bytes == page.bytes_used {
            return Ok(());
        }
        page.bytes_used = bytes;
    }
    Err(ProjectionError::Serialization)
}

fn find_descriptor_children(
    blocks: &Sequence<Block>,
    parent: BlockId,
) -> Option<DescriptorChildren<'_>> {
    for block in blocks.iter() {
        if block.id == parent {
            return Some(match &block.kind {
                BlockKind::BlockQuote { children } => DescriptorChildren::Blocks(children),
                BlockKind::List { items, .. } => DescriptorChildren::Items(items),
                _ => DescriptorChildren::Empty,
            });
        }
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                if let Some(found) = find_descriptor_children(children, parent) {
                    return Some(found);
                }
            }
            BlockKind::List { items, .. } => {
                for item in items.iter() {
                    if item.id == parent {
                        return Some(DescriptorChildren::Blocks(&item.children));
                    }
                    if let Some(found) = find_descriptor_children(&item.children, parent) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn block_descriptor(
    document: &Document,
    block: &Block,
    parent: Option<BlockId>,
    order: usize,
) -> BlockDescriptor {
    let (direct_child_count, descendant_count) = block_hierarchy_counts(block);
    let (kind, heading_level) = match &block.kind {
        BlockKind::Paragraph { .. } => (BlockDescriptorKind::Paragraph, None),
        BlockKind::Heading { level, .. } => (BlockDescriptorKind::Heading, Some(*level)),
        BlockKind::List { .. } => (BlockDescriptorKind::List, None),
        BlockKind::CodeFence { .. } => (BlockDescriptorKind::CodeFence, None),
        BlockKind::BlockQuote { .. } => (BlockDescriptorKind::BlockQuote, None),
        BlockKind::RawBlock { .. } => (BlockDescriptorKind::RawBlock, None),
        BlockKind::Table { .. } => (BlockDescriptorKind::Table, None),
    };
    BlockDescriptor {
        id: block.id,
        parent,
        order: u32::try_from(order).unwrap_or(u32::MAX),
        kind,
        heading_level,
        source_bytes: document.source_region_bytes(block.id).unwrap_or(0),
        text_bytes: block_text_bytes(block),
        node_digest: block_node_digest(block),
        direct_child_count,
        descendant_count,
        subtree_digest: None,
    }
}

fn list_item_descriptor(item: &ListItem, parent: Option<BlockId>, order: usize) -> BlockDescriptor {
    let (direct_child_count, descendant_count) = list_item_hierarchy_counts(item);
    BlockDescriptor {
        id: item.id,
        parent,
        order: u32::try_from(order).unwrap_or(u32::MAX),
        kind: BlockDescriptorKind::ListItem,
        heading_level: None,
        source_bytes: 0,
        text_bytes: 0,
        node_digest: list_item_node_digest(item),
        direct_child_count,
        descendant_count,
        subtree_digest: None,
    }
}

fn block_hierarchy_counts(block: &Block) -> (u64, u64) {
    match &block.kind {
        BlockKind::BlockQuote { children } => {
            let direct = u64::try_from(children.len_visible()).unwrap_or(u64::MAX);
            let descendants = children.iter().fold(direct, |count, child| {
                count.saturating_add(block_hierarchy_counts(child).1)
            });
            (direct, descendants)
        }
        BlockKind::List { items, .. } => {
            let direct = u64::try_from(items.len_visible()).unwrap_or(u64::MAX);
            let descendants = items.iter().fold(direct, |count, item| {
                count.saturating_add(list_item_hierarchy_counts(item).1)
            });
            (direct, descendants)
        }
        _ => (0, 0),
    }
}

fn list_item_hierarchy_counts(item: &ListItem) -> (u64, u64) {
    let direct = u64::try_from(item.children.len_visible()).unwrap_or(u64::MAX);
    let descendants = item.children.iter().fold(direct, |count, child| {
        count.saturating_add(block_hierarchy_counts(child).1)
    });
    (direct, descendants)
}

fn block_text_bytes(block: &Block) -> usize {
    match &block.kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
            text.iter().map(|unit| unit.grapheme.len()).sum()
        }
        BlockKind::CodeFence { text, .. } => text.len(),
        BlockKind::RawBlock { raw } => raw.len(),
        BlockKind::Table { table } => table
            .header
            .get_ref()
            .iter()
            .chain(
                table
                    .rows
                    .iter()
                    .filter(|row| !*row.deleted.get_ref())
                    .flat_map(|row| row.cells.get_ref().iter()),
            )
            .map(String::len)
            .sum(),
        BlockKind::List { .. } | BlockKind::BlockQuote { .. } => 0,
    }
}

struct StableDigest(u64);

impl StableDigest {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(Self::PRIME);
        }
    }

    fn field(&mut self, bytes: &[u8]) {
        self.bytes(&(bytes.len() as u64).to_le_bytes());
        self.bytes(bytes);
    }

    fn finish(self) -> u64 {
        self.0
    }
}

fn block_node_digest(block: &Block) -> u64 {
    let mut digest = StableDigest::new();
    hash_block_node(&mut digest, block);
    digest.finish()
}

fn list_item_node_digest(_item: &ListItem) -> u64 {
    let mut digest = StableDigest::new();
    digest.field(b"list-item");
    digest.finish()
}

fn block_digest(block: &Block) -> u64 {
    let mut digest = StableDigest::new();
    digest.field(b"block-subtree");
    digest.field(&block_node_digest(block).to_le_bytes());
    match &block.kind {
        BlockKind::BlockQuote { children } => {
            for child in children.iter() {
                digest.field(&block_digest(child).to_le_bytes());
            }
        }
        BlockKind::List { items, .. } => {
            for item in items.iter() {
                digest.field(&list_item_digest(item).to_le_bytes());
            }
        }
        _ => {}
    }
    digest.finish()
}

fn hash_block_node(digest: &mut StableDigest, block: &Block) {
    match &block.kind {
        BlockKind::Paragraph { .. } => digest.field(b"paragraph"),
        BlockKind::Heading { level, .. } => {
            digest.field(b"heading");
            digest.field(&[*level]);
        }
        BlockKind::List { ordered, .. } => {
            digest.field(b"list");
            digest.field(&[u8::from(*ordered)]);
        }
        BlockKind::CodeFence { info, text } => {
            digest.field(b"code-fence");
            digest.field(info.as_deref().unwrap_or_default().as_bytes());
            digest.field(text.as_bytes());
        }
        BlockKind::BlockQuote { .. } => {
            digest.field(b"block-quote");
        }
        BlockKind::RawBlock { raw } => {
            digest.field(b"raw");
            digest.field(raw.as_bytes());
        }
        BlockKind::Table { table } => {
            digest.field(b"table");
            for column in table.columns.get_ref() {
                digest.field(alignment_tag(&column.alignment));
            }
            for cell in table.header.get_ref() {
                digest.field(cell.as_bytes());
            }
            for row in table.rows.iter().filter(|row| !*row.deleted.get_ref()) {
                digest.field(b"row");
                for cell in row.cells.get_ref() {
                    digest.field(cell.as_bytes());
                }
            }
        }
    }
    if let Some(text) = block_text_seq(&block.kind) {
        for unit in text.iter() {
            digest.field(unit.grapheme.as_bytes());
        }
        if block.marks.iter_active_intervals().next().is_some() {
            let ids = paragraph_visible_ids(text);
            hash_semantic_marks(digest, block, &ids, 0..ids.len());
        }
    }
}

fn text_range_digest(document: &Document, range: &TextRange) -> Result<u64, WorkspaceTargetError> {
    let resolved = document.resolve_text_range(range)?;
    let block_id = range.start.block_id;
    let block = document
        .find_block_by_id(block_id)
        .ok_or(WorkspaceTargetError::BlockNotFound { block_id })?;
    let text = block_text_seq(&block.kind).ok_or(WorkspaceTargetError::NotText { block_id })?;
    let mut digest = StableDigest::new();
    digest.field(block_id.as_bytes());
    for unit in text.iter().skip(resolved.start).take(resolved.len()) {
        digest.field(unit.grapheme.as_bytes());
    }
    if block.marks.iter_active_intervals().next().is_some() {
        let ids = paragraph_visible_ids(text);
        hash_semantic_marks(&mut digest, block, &ids, resolved);
    }
    Ok(digest.finish())
}

type SemanticMark = (MarkKind, Vec<(String, MarkValue)>);

fn hash_semantic_marks(
    digest: &mut StableDigest,
    block: &Block,
    ids: &[OpId],
    range: Range<usize>,
) {
    let mut runs: Vec<(usize, usize, BTreeSet<SemanticMark>)> = Vec::new();
    for span in block.marks.render_spans(ids, ids.len()) {
        let start = span.start.max(range.start);
        let end = span.end.min(range.end);
        if start >= end {
            continue;
        }
        let marks: BTreeSet<_> = span
            .marks
            .into_iter()
            .filter_map(|interval_id| block.marks.interval(&interval_id))
            .map(|interval| {
                let attrs = interval
                    .attrs
                    .iter()
                    .filter(|(key, _)| key.as_str() != "delimiter")
                    .map(|(key, value)| (key.clone(), value.get_ref().clone()))
                    .collect();
                (interval.kind.clone(), attrs)
            })
            .collect();
        if marks.is_empty() {
            continue;
        }
        if let Some((_, prior_end, prior_marks)) = runs.last_mut()
            && *prior_end == start
            && *prior_marks == marks
        {
            *prior_end = end;
        } else {
            runs.push((start, end, marks));
        }
    }
    for (start, end, marks) in runs {
        digest.field(&(start - range.start).to_le_bytes());
        digest.field(&(end - range.start).to_le_bytes());
        for (kind, attrs) in marks {
            hash_mark_kind(digest, &kind);
            for (key, value) in attrs {
                digest.field(key.as_bytes());
                hash_mark_value(digest, &value);
            }
        }
    }
}

fn alignment_tag(alignment: &ColumnAlignment) -> &'static [u8] {
    match alignment {
        ColumnAlignment::Left => b"left",
        ColumnAlignment::Center => b"center",
        ColumnAlignment::Right => b"right",
    }
}

fn hash_mark_kind(digest: &mut StableDigest, kind: &MarkKind) {
    match kind {
        MarkKind::Bold => digest.field(b"bold"),
        MarkKind::Italic => digest.field(b"italic"),
        MarkKind::Code => digest.field(b"code"),
        MarkKind::Link => digest.field(b"link"),
        MarkKind::Custom(name) => {
            digest.field(b"custom");
            digest.field(name.as_bytes());
        }
    }
}

fn hash_mark_value(digest: &mut StableDigest, value: &MarkValue) {
    match value {
        MarkValue::String(value) => {
            digest.field(b"string");
            digest.field(value.as_bytes());
        }
        MarkValue::Bool(value) => {
            digest.field(b"bool");
            digest.field(&[u8::from(*value)]);
        }
    }
}

#[derive(Clone)]
pub(crate) struct DocumentOutline {
    entries: BTreeMap<BlockId, OutlineEntry>,
}

#[derive(Clone)]
struct OutlineEntry {
    descriptor: BlockDescriptor,
    section: Option<BlockId>,
}

pub(crate) fn capture_outline(document: &Document) -> DocumentOutline {
    let mut entries = BTreeMap::new();
    collect_block_outline(document, document.blocks(), None, None, &mut entries);
    DocumentOutline { entries }
}

fn placement_digest(outline: &DocumentOutline, parent: Option<BlockId>) -> u64 {
    let mut children: Vec<_> = outline
        .entries
        .values()
        .filter(|entry| entry.descriptor.parent == parent)
        .map(|entry| (entry.descriptor.order, entry.descriptor.id))
        .collect();
    children.sort_by_key(|(order, _)| *order);
    let mut digest = StableDigest::new();
    match parent {
        Some(parent) => digest.field(parent.as_bytes()),
        None => digest.field(b"document-root"),
    }
    for (_, id) in children {
        digest.field(id.as_bytes());
    }
    digest.finish()
}

fn collect_block_outline(
    document: &Document,
    blocks: &Sequence<Block>,
    parent: Option<BlockId>,
    inherited_section: Option<BlockId>,
    entries: &mut BTreeMap<BlockId, OutlineEntry>,
) {
    let mut section = inherited_section;
    for (order, block) in blocks.iter().enumerate() {
        if matches!(block.kind, BlockKind::Heading { .. }) {
            section = Some(block.id);
        }
        entries.insert(
            block.id,
            OutlineEntry {
                descriptor: block_descriptor(document, block, parent, order),
                section,
            },
        );
        match &block.kind {
            BlockKind::BlockQuote { children } => {
                collect_block_outline(document, children, Some(block.id), section, entries);
            }
            BlockKind::List { items, .. } => {
                for (item_order, item) in items.iter().enumerate() {
                    entries.insert(
                        item.id,
                        OutlineEntry {
                            descriptor: list_item_descriptor(item, Some(block.id), item_order),
                            section,
                        },
                    );
                    collect_block_outline(
                        document,
                        &item.children,
                        Some(item.id),
                        section,
                        entries,
                    );
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn summarize_outline_change(
    before: &DocumentOutline,
    after: &DocumentOutline,
    operation_count: usize,
    revision: RevisionToken,
) -> ChangeSummary {
    let before_ids: BTreeSet<_> = before.entries.keys().copied().collect();
    let after_ids: BTreeSet<_> = after.entries.keys().copied().collect();
    let created: Vec<_> = after_ids.difference(&before_ids).copied().collect();
    let deleted: Vec<_> = before_ids.difference(&after_ids).copied().collect();
    let mut moved: BTreeSet<BlockId> = before_ids
        .intersection(&after_ids)
        .filter(|id| before.entries[*id].descriptor.parent != after.entries[*id].descriptor.parent)
        .copied()
        .collect();
    moved.extend(reordered_ids(before, after));
    let mut updated = Vec::new();
    for id in before_ids.intersection(&after_ids) {
        let old = &before.entries[id].descriptor;
        let new = &after.entries[id].descriptor;
        if descriptor_content_changed(old, new) {
            updated.push(*id);
        }
    }

    let changed: BTreeSet<_> = created
        .iter()
        .chain(&deleted)
        .chain(moved.iter())
        .chain(&updated)
        .copied()
        .collect();
    let mut affected_parents = BTreeSet::new();
    let mut affected_sections = BTreeSet::new();
    for id in changed {
        if let Some(entry) = before.entries.get(&id) {
            affected_parents.extend(entry.descriptor.parent);
            affected_sections.extend(entry.section);
        }
        if let Some(entry) = after.entries.get(&id) {
            affected_parents.extend(entry.descriptor.parent);
            affected_sections.extend(entry.section);
        }
    }

    ChangeSummary {
        created,
        deleted,
        moved: moved.into_iter().collect(),
        updated,
        affected_parents: affected_parents.into_iter().collect(),
        affected_sections: affected_sections.into_iter().collect(),
        operation_count,
        revision,
    }
}

pub(crate) fn replace_moved_ids(
    summary: &mut ChangeSummary,
    before: &DocumentOutline,
    after: &DocumentOutline,
    moved: BTreeSet<BlockId>,
) {
    summary.moved = moved.into_iter().collect();
    let changed: BTreeSet<_> = summary
        .created
        .iter()
        .chain(&summary.deleted)
        .chain(&summary.moved)
        .chain(&summary.updated)
        .copied()
        .collect();
    let mut affected_parents = BTreeSet::new();
    let mut affected_sections = BTreeSet::new();
    for id in changed {
        if let Some(entry) = before.entries.get(&id) {
            affected_parents.extend(entry.descriptor.parent);
            affected_sections.extend(entry.section);
        }
        if let Some(entry) = after.entries.get(&id) {
            affected_parents.extend(entry.descriptor.parent);
            affected_sections.extend(entry.section);
        }
    }
    summary.affected_parents = affected_parents.into_iter().collect();
    summary.affected_sections = affected_sections.into_iter().collect();
}

fn descriptor_content_changed(old: &BlockDescriptor, new: &BlockDescriptor) -> bool {
    old.kind != new.kind
        || old.heading_level != new.heading_level
        || old.source_bytes != new.source_bytes
        || old.text_bytes != new.text_bytes
        || old.node_digest != new.node_digest
        || old.direct_child_count != new.direct_child_count
        || old.descendant_count != new.descendant_count
        || old.subtree_digest != new.subtree_digest
}

fn reordered_ids(before: &DocumentOutline, after: &DocumentOutline) -> BTreeSet<BlockId> {
    let parents: BTreeSet<_> = before
        .entries
        .values()
        .chain(after.entries.values())
        .map(|entry| entry.descriptor.parent)
        .collect();
    let mut moved = BTreeSet::new();
    for parent in parents {
        let mut old: Vec<_> = before
            .entries
            .values()
            .filter(|entry| entry.descriptor.parent == parent)
            .map(|entry| (entry.descriptor.order, entry.descriptor.id))
            .collect();
        let mut new: Vec<_> = after
            .entries
            .values()
            .filter(|entry| entry.descriptor.parent == parent)
            .map(|entry| (entry.descriptor.order, entry.descriptor.id))
            .collect();
        old.sort_unstable();
        new.sort_unstable();
        let old_positions: BTreeMap<_, _> = old
            .iter()
            .enumerate()
            .map(|(position, (_, id))| (*id, position))
            .collect();
        let common: Vec<_> = new
            .into_iter()
            .filter_map(|(_, id)| old_positions.get(&id).map(|position| (id, *position)))
            .collect();
        let stationary = longest_increasing_ids(&common);
        moved.extend(
            common
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| !stationary.contains(id)),
        );
    }
    moved
}

fn longest_increasing_ids(items: &[(BlockId, usize)]) -> BTreeSet<BlockId> {
    let mut tails: Vec<usize> = Vec::new();
    let mut tail_indices: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; items.len()];
    for (index, (_, value)) in items.iter().enumerate() {
        let slot = tails.partition_point(|tail| tail < value);
        if slot == tails.len() {
            tails.push(*value);
            tail_indices.push(index);
        } else {
            tails[slot] = *value;
            tail_indices[slot] = index;
        }
        if slot > 0 {
            previous[index] = Some(tail_indices[slot - 1]);
        }
    }
    let mut stationary = BTreeSet::new();
    let mut cursor = tail_indices.last().copied();
    while let Some(index) = cursor {
        stationary.insert(items[index].0);
        cursor = previous[index];
    }
    stationary
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use crate::doc::Parser;

    fn selected_projection_size(blocks: usize) -> usize {
        let markdown = (0..blocks)
            .map(|index| format!("item-{index:05}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let document = Parser::parse(&markdown);
        let id = document.blocks_in_order()[0].id;
        let request = ProjectionRequest {
            document_id: DocumentId::from_u128(1),
            base_revision: RevisionToken::from_u128(2),
            block_ids: vec![id],
            fields: ProjectionFields::SEMANTIC,
            max_items: 1,
            max_bytes: 1_024,
            continuation: None,
        };
        document
            .projection_page(&request, request.base_revision.clone())
            .unwrap()
            .bytes_used
    }

    #[test]
    fn selected_projection_size_does_not_scale_with_document() {
        let small = selected_projection_size(100);
        let large = selected_projection_size(10_000);
        assert_eq!(small, large);
        assert!(large < 1_024);
    }

    #[test]
    fn exact_projection_without_source_uses_the_structural_root() {
        for markdown in ["plain", "> quoted", "- listed"] {
            let mut document = Parser::parse(markdown);
            let root = document.blocks_in_order()[0];
            let selected_id = match &root.kind {
                BlockKind::BlockQuote { children } => children.iter().next().unwrap().id,
                BlockKind::List { items, .. } => items.iter().next().unwrap().id,
                _ => root.id,
            };
            let owner_id = root.id;
            document.set_source_state(None);
            let request = ProjectionRequest {
                document_id: DocumentId::from_u128(1),
                base_revision: RevisionToken::from_u128(2),
                block_ids: vec![selected_id],
                fields: ProjectionFields::EXACT,
                max_items: 1,
                max_bytes: 1_024,
                continuation: None,
            };

            let page = document
                .projection_page(&request, request.base_revision.clone())
                .unwrap();
            let exact = page.items[0].exact.as_ref().unwrap();
            assert_eq!(exact.owner_block_id, owner_id);
            assert_eq!(exact.markdown, markdown);
        }
    }
}
