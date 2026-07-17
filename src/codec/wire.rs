//! Wire DTOs for collaborative document operations.
//!
//! These types are the **only** forms serialized onto the sync wire. Live
//! `Sequence` maps, pending buffers, and runtime `Block` state are never
//! encoded here.

use crate::core::mark::{Anchor, MarkKind, MarkValue};
use crate::core::{OpId, StateVector};
use crate::doc::Frontmatter;
use crate::doc::{BlockId, CodeFenceStyle, ColumnId, ListStyle, RowId, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Current wire protocol version for [`Envelope`].
pub const WIRE_VERSION: u16 = 4;

/// Maximum nested BlockQuote / structure depth accepted on encode and decode.
///
/// Kept modest so well-formed over-deep payloads can be deserialized far enough
/// for this crate's depth check to run (serde's default recursion limit is 128;
/// each quote level costs several enum layers in JSON).
pub const MAX_WIRE_NEST_DEPTH: u32 = 16;

/// Versioned operation envelope carried as `sync::Operation` payload bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u16,
    pub body: OpBody,
}

/// Top-level body tag for an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpBody {
    /// One logical document operation (may expand to multiple nested RGA effects later).
    Doc(DocOp),
}

/// One grapheme unit on an [`DocOp::InsertText`] envelope (N1 + N4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextUnitWire {
    pub id: OpId,
    pub after: Option<OpId>,
    /// Concurrent-insert right neighbor; always stamped on the wire (N4).
    pub right_origin: Option<OpId>,
    pub grapheme: String,
}

/// A text unit transferred between blocks without changing its identity when possible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedTextUnitWire {
    /// Unit id in the source block.
    pub source_id: OpId,
    /// Unit id in the destination block. Usually equal to `source_id`; a merge may
    /// allocate a replacement when the destination retains that id as a tombstone.
    pub id: OpId,
    pub grapheme: String,
}

/// One logical block placement inside an atomic move range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedBlockWire {
    pub block_id: BlockId,
    pub target: OpId,
    pub id: OpId,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
}

/// Text-bearing block metadata needed to materialize the second half of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextBlockKindWire {
    Paragraph,
    Heading { level: u8 },
}

/// Stable wire representation of table column alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnAlignmentWire {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCellWire {
    pub column_id: ColumnId,
    pub value: String,
}

/// Serializable document operations for the collaborative wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocOp {
    /// Block-level RGA insert with skeleton only (no nested live Sequence).
    InsertBlock {
        /// Container block elem_id to insert into; `None` = top-level document.
        #[serde(default)]
        parent: Option<OpId>,
        after: Option<OpId>,
        id: OpId,
        right_origin: Option<OpId>,
        block: BlockSkeleton,
    },
    /// Block-level RGA delete.
    DeleteBlock {
        /// Container block elem_id the target lives in; `None` = top-level document.
        #[serde(default)]
        parent: Option<OpId>,
        target: OpId,
        /// Delete-op id; also `Operation.id` when this is the sole effect.
        id: OpId,
    },
    /// Delete a logical block regardless of its winning concurrent placement.
    DeleteBlockById {
        parent: Option<OpId>,
        target: OpId,
        block_id: BlockId,
        id: OpId,
    },
    /// Nested RGA inserts into a paragraph body (one element per grapheme).
    InsertText {
        block_elem: OpId,
        block_id: BlockId,
        /// Contiguous paste: units listed in left-to-right insert order.
        units: Vec<TextUnitWire>,
    },
    /// Tombstone unit element ids inside a paragraph body.
    DeleteText {
        block_elem: OpId,
        block_id: BlockId,
        /// Delete-op id; equals `Operation.id` (one fresh counter).
        id: OpId,
        /// Existing unit element ids to tombstone.
        targets: Vec<OpId>,
    },
    /// Create/update one causal mark interval over stable text-unit anchors.
    SetMark {
        block_elem: OpId,
        block_id: BlockId,
        id: OpId,
        kind: MarkKind,
        start: Anchor,
        end: Anchor,
        attrs: BTreeMap<String, MarkValue>,
    },
    /// Causally remove one complete mark interval.
    RemoveMark {
        block_elem: OpId,
        block_id: BlockId,
        interval_id: OpId,
        id: OpId,
        observed: StateVector,
    },
    /// LWW update/delete of one supported top-level frontmatter key.
    SetFrontmatterField {
        id: OpId,
        key: String,
        value: Option<String>,
    },
    /// Establish the lossless frontmatter base on first ingest.
    InitializeFrontmatter { id: OpId, frontmatter: Frontmatter },
    /// Atomically move one block or a contiguous heading section.
    MoveBlocks {
        to_parent: Option<OpId>,
        id: OpId,
        blocks: Vec<MovedBlockWire>,
    },
    /// Split one text-bearing block, transferring the visible suffix to a new sibling.
    SplitBlock {
        #[serde(default)]
        parent: Option<OpId>,
        target: OpId,
        id: OpId,
        new_block_id: BlockId,
        right_origin: Option<OpId>,
        kind: TextBlockKindWire,
        units: Vec<MovedTextUnitWire>,
    },
    /// Append one text-bearing sibling to another and tombstone the right block.
    MergeBlocks {
        #[serde(default)]
        parent: Option<OpId>,
        left: OpId,
        right: OpId,
        /// Fresh operation id used for the structural delete.
        id: OpId,
        /// Stable insertion anchor in the left block's text sequence.
        after: Option<OpId>,
        right_origin: Option<OpId>,
        units: Vec<MovedTextUnitWire>,
    },
    /// Insert one row into a table's row RGA.
    InsertTableRow {
        table_elem: OpId,
        table_id: BlockId,
        after: Option<OpId>,
        id: OpId,
        right_origin: Option<OpId>,
        cells: Vec<TableCellWire>,
    },
    /// Insert one stable column and its header cell.
    InsertTableColumn {
        table_elem: OpId,
        table_id: BlockId,
        after: Option<OpId>,
        id: OpId,
        right_origin: Option<OpId>,
        alignment: ColumnAlignmentWire,
        header: String,
    },
    /// Update one cell through its independent LWW register.
    SetTableCell {
        table_elem: OpId,
        table_id: BlockId,
        row_id: RowId,
        column_id: ColumnId,
        id: OpId,
        value: String,
        observed: StateVector,
    },
    /// Tombstone one table row.
    DeleteTableRow {
        table_elem: OpId,
        table_id: BlockId,
        target: OpId,
        id: OpId,
    },
    /// Delete a logical row regardless of its winning concurrent placement.
    DeleteTableRowById {
        table_elem: OpId,
        table_id: BlockId,
        target: OpId,
        row_id: BlockId,
        id: OpId,
    },
    /// Delete one logical column regardless of its winning placement.
    DeleteTableColumnById {
        table_elem: OpId,
        table_id: BlockId,
        target: OpId,
        column_id: ColumnId,
        id: OpId,
    },
    /// Update one stable column's alignment.
    SetTableColumnAlignment {
        table_elem: OpId,
        table_id: BlockId,
        column_id: ColumnId,
        id: OpId,
        alignment: ColumnAlignmentWire,
        observed: StateVector,
    },
    /// Move one logical row under a fresh placement id.
    MoveTableRow {
        table_elem: OpId,
        table_id: BlockId,
        row_id: BlockId,
        target: OpId,
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
        observed: StateVector,
    },
    /// Move one logical column under a fresh placement id.
    MoveTableColumn {
        table_elem: OpId,
        table_id: BlockId,
        column_id: ColumnId,
        target: OpId,
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
        observed: StateVector,
    },
    InsertListItem {
        list_elem: OpId,
        list_id: BlockId,
        after: Option<OpId>,
        id: OpId,
        right_origin: Option<OpId>,
        task: Option<TaskState>,
    },
    DeleteListItemById {
        list_elem: OpId,
        list_id: BlockId,
        target: OpId,
        item_id: BlockId,
        id: OpId,
    },
    MoveListItem {
        from_list_elem: OpId,
        to_list_elem: OpId,
        list_id: BlockId,
        item_id: BlockId,
        target: OpId,
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
        observed: StateVector,
    },
    SetListStyle {
        block_elem: OpId,
        block_id: BlockId,
        id: OpId,
        style: ListStyle,
        observed: StateVector,
    },
    SetListItemTask {
        item_id: BlockId,
        id: OpId,
        task: Option<TaskState>,
        observed: StateVector,
    },
    SetCodeFence {
        block_elem: OpId,
        block_id: BlockId,
        id: OpId,
        style: CodeFenceStyle,
        info: Option<String>,
        text: String,
        observed: StateVector,
    },
    ConvertTextBlock {
        block_elem: OpId,
        block_id: BlockId,
        id: OpId,
        kind: TextBlockKindWire,
        observed: StateVector,
    },
    ReplaceRawBlock {
        block_elem: OpId,
        block_id: BlockId,
        id: OpId,
        raw: String,
        observed: StateVector,
    },
}

/// Serializable block creation payload — no Sequence maps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSkeleton {
    pub block_id: BlockId,
    pub kind: BlockKindSkeleton,
}

/// Wire form of block kinds. Paragraph `text` is the body in string-mode;
/// unit-mode sessions require empty paragraph text (session-enforced).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKindSkeleton {
    Paragraph {
        text: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    List {
        style: ListStyle,
        items: Vec<ListItemSkeleton>,
    },
    CodeFence {
        style: CodeFenceStyle,
        info: Option<String>,
        text: String,
    },
    BlockQuote {
        children: Vec<BlockSkeletonInsert>,
    },
    RawBlock {
        raw: String,
    },
    Table,
}

/// Wire form of a list item (children are nested structure inserts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItemSkeleton {
    pub after: Option<OpId>,
    pub id: OpId,
    pub right_origin: Option<OpId>,
    pub block_id: crate::doc::BlockId,
    pub task: Option<TaskState>,
    pub task_op: OpId,
    pub children: Vec<BlockSkeletonInsert>,
}

/// Nested block insert used inside blockquotes on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSkeletonInsert {
    pub after: Option<OpId>,
    pub id: OpId,
    pub right_origin: Option<OpId>,
    pub block: BlockSkeleton,
}

/// Returns `true` when the envelope does not violate the unit-mode empty-paragraph
/// rule: only `InsertBlock` with non-empty `Paragraph` text returns `false`.
///
/// Callers in unit-mode sessions use this to reject; the codec never rejects on
/// this predicate during decode of historical/string-mode payloads.
pub fn insert_block_paragraph_is_empty(envelope: &Envelope) -> bool {
    fn kind_is_empty(kind: &BlockKindSkeleton) -> bool {
        match kind {
            BlockKindSkeleton::Paragraph { text } | BlockKindSkeleton::Heading { text, .. } => {
                text.is_empty()
            }
            BlockKindSkeleton::BlockQuote { children } => children
                .iter()
                .all(|child| kind_is_empty(&child.block.kind)),
            BlockKindSkeleton::List { items, .. } => items.iter().all(|item| {
                item.children
                    .iter()
                    .all(|child| kind_is_empty(&child.block.kind))
            }),
            BlockKindSkeleton::CodeFence { .. }
            | BlockKindSkeleton::RawBlock { .. }
            | BlockKindSkeleton::Table => true,
        }
    }
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock { block, .. }) => kind_is_empty(&block.kind),
        OpBody::Doc(
            DocOp::DeleteBlock { .. }
            | DocOp::DeleteBlockById { .. }
            | DocOp::InsertText { .. }
            | DocOp::DeleteText { .. }
            | DocOp::SetMark { .. }
            | DocOp::RemoveMark { .. }
            | DocOp::SetFrontmatterField { .. }
            | DocOp::InitializeFrontmatter { .. }
            | DocOp::MoveBlocks { .. }
            | DocOp::SplitBlock { .. }
            | DocOp::MergeBlocks { .. }
            | DocOp::InsertTableRow { .. }
            | DocOp::InsertTableColumn { .. }
            | DocOp::SetTableCell { .. }
            | DocOp::DeleteTableRow { .. }
            | DocOp::DeleteTableRowById { .. }
            | DocOp::DeleteTableColumnById { .. }
            | DocOp::SetTableColumnAlignment { .. }
            | DocOp::MoveTableRow { .. }
            | DocOp::MoveTableColumn { .. }
            | DocOp::InsertListItem { .. }
            | DocOp::DeleteListItemById { .. }
            | DocOp::MoveListItem { .. }
            | DocOp::SetListStyle { .. }
            | DocOp::SetListItemTask { .. }
            | DocOp::SetCodeFence { .. }
            | DocOp::ConvertTextBlock { .. }
            | DocOp::ReplaceRawBlock { .. },
        ) => true,
    }
}

/// Validate structural limits on an envelope (nest depth). Does not enforce unit-mode rules.
pub(crate) fn validate_envelope_structure(envelope: &Envelope) -> Result<(), super::CodecError> {
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock { block, .. }) => {
            check_kind_depth(&block.kind, 0)?;
        }
        OpBody::Doc(
            DocOp::DeleteBlock { .. }
            | DocOp::DeleteBlockById { .. }
            | DocOp::InsertText { .. }
            | DocOp::DeleteText { .. }
            | DocOp::SetMark { .. }
            | DocOp::RemoveMark { .. }
            | DocOp::SetFrontmatterField { .. }
            | DocOp::InitializeFrontmatter { .. }
            | DocOp::MoveBlocks { .. }
            | DocOp::SplitBlock { .. }
            | DocOp::MergeBlocks { .. }
            | DocOp::InsertTableRow { .. }
            | DocOp::InsertTableColumn { .. }
            | DocOp::SetTableCell { .. }
            | DocOp::DeleteTableRow { .. }
            | DocOp::DeleteTableRowById { .. }
            | DocOp::DeleteTableColumnById { .. }
            | DocOp::SetTableColumnAlignment { .. }
            | DocOp::MoveTableRow { .. }
            | DocOp::MoveTableColumn { .. }
            | DocOp::InsertListItem { .. }
            | DocOp::DeleteListItemById { .. }
            | DocOp::MoveListItem { .. }
            | DocOp::SetListStyle { .. }
            | DocOp::SetListItemTask { .. }
            | DocOp::SetCodeFence { .. }
            | DocOp::ConvertTextBlock { .. }
            | DocOp::ReplaceRawBlock { .. },
        ) => {}
    }
    Ok(())
}

fn check_kind_depth(kind: &BlockKindSkeleton, depth: u32) -> Result<(), super::CodecError> {
    if depth > MAX_WIRE_NEST_DEPTH {
        return Err(super::CodecError::NestDepthExceeded);
    }
    match kind {
        BlockKindSkeleton::BlockQuote { children } => {
            for child in children {
                // Child block sits one level deeper than the quote itself.
                check_kind_depth(&child.block.kind, depth + 1)?;
            }
            Ok(())
        }
        BlockKindSkeleton::List { items, .. } => {
            for item in items {
                for child in &item.children {
                    check_kind_depth(&child.block.kind, depth + 1)?;
                }
            }
            Ok(())
        }
        BlockKindSkeleton::Paragraph { .. }
        | BlockKindSkeleton::Heading { .. }
        | BlockKindSkeleton::CodeFence { .. }
        | BlockKindSkeleton::RawBlock { .. }
        | BlockKindSkeleton::Table => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_paragraph_predicate() {
        let empty = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertBlock {
                parent: None,
                after: None,
                id: OpId {
                    counter: 1,
                    peer: 1,
                },
                right_origin: None,
                block: BlockSkeleton {
                    block_id: BlockId::nil(),
                    kind: BlockKindSkeleton::Paragraph {
                        text: String::new(),
                    },
                },
            }),
        };
        assert!(insert_block_paragraph_is_empty(&empty));
    }
}
