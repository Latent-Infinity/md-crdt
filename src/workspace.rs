//! Concrete, transport-agnostic workspace contract types.

use crate::core::mark::{MarkKind, MarkValue};
use crate::core::{OpId, Sequence};
use crate::doc::{
    Block, BlockId, BlockKind, ColumnAlignment, ColumnDef, Document, ListItem, RowId,
    block_text_seq, paragraph_visible_ids,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
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
    /// `text_bytes`/`content_digest` (which track live content) to detect edits.
    pub source_bytes: usize,
    pub text_bytes: usize,
    pub content_digest: u64,
}

/// One bounded page of direct children under a document or container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorPage {
    pub items: Vec<BlockDescriptor>,
    pub next_offset: Option<usize>,
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
        block_id: BlockId,
        grapheme_offset: usize,
        text: String,
    },
    DeleteText {
        block_id: BlockId,
        grapheme_offset: usize,
        grapheme_count: usize,
    },
    SetMark {
        block_id: BlockId,
        start: usize,
        end: usize,
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
        block_id: BlockId,
        grapheme_offset: usize,
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

/// Concrete single-document batch applied against one exact revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditBatch {
    pub document_id: DocumentId,
    pub expected_revision: RevisionToken,
    pub operations: Vec<WorkspaceEdit>,
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
    /// Return at most `limit` body-free descriptors for direct children of `parent`.
    ///
    /// `None` addresses the document root. A blockquote or list item exposes block
    /// children; a list exposes list-item descriptors. Returns `None` only when the
    /// requested parent id does not exist.
    pub fn descriptor_page(
        &self,
        parent: Option<BlockId>,
        offset: usize,
        limit: usize,
    ) -> Option<DescriptorPage> {
        if limit == 0 {
            return Some(DescriptorPage {
                items: Vec::new(),
                next_offset: None,
            });
        }
        let children = match parent {
            None => DescriptorChildren::Blocks(self.blocks()),
            Some(parent) => find_descriptor_children(self.blocks(), parent)?,
        };
        let take = limit.saturating_add(1);
        let mut items: Vec<BlockDescriptor> = match children {
            DescriptorChildren::Blocks(blocks) => blocks
                .iter()
                .enumerate()
                .skip(offset)
                .take(take)
                .map(|(order, block)| block_descriptor(self, block, parent, order))
                .collect(),
            DescriptorChildren::Items(items) => items
                .iter()
                .enumerate()
                .skip(offset)
                .take(take)
                .map(|(order, item)| list_item_descriptor(item, parent, order))
                .collect(),
            DescriptorChildren::Empty => Vec::new(),
        };
        let has_more = items.len() > limit;
        if has_more {
            items.pop();
        }
        Some(DescriptorPage {
            next_offset: has_more.then_some(offset.saturating_add(items.len())),
            items,
        })
    }
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
        content_digest: block_digest(block),
    }
}

fn list_item_descriptor(item: &ListItem, parent: Option<BlockId>, order: usize) -> BlockDescriptor {
    let mut digest = StableDigest::new();
    digest.field(b"list-item");
    BlockDescriptor {
        id: item.id,
        parent,
        order: u32::try_from(order).unwrap_or(u32::MAX),
        kind: BlockDescriptorKind::ListItem,
        heading_level: None,
        source_bytes: 0,
        text_bytes: 0,
        content_digest: digest.finish(),
    }
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

fn block_digest(block: &Block) -> u64 {
    let mut digest = StableDigest::new();
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
        BlockKind::BlockQuote { .. } => digest.field(b"block-quote"),
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
        let ids = paragraph_visible_ids(text);
        for span in block.marks.render_spans(&ids, ids.len()) {
            digest.field(&span.start.to_le_bytes());
            digest.field(&span.end.to_le_bytes());
            for interval_id in span.marks {
                let Some(interval) = block.marks.interval(&interval_id) else {
                    continue;
                };
                hash_mark_kind(&mut digest, &interval.kind);
                for (key, value) in &interval.attrs {
                    digest.field(key.as_bytes());
                    hash_mark_value(&mut digest, value.get_ref());
                }
            }
        }
    }
    digest.finish()
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
        || old.content_digest != new.content_digest
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
