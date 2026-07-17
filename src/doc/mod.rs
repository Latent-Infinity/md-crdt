//! Markdown document model, parser, serializer, and editing API.
//!
//! This module provides a block-based document model for markdown content,
//! with support for collaborative editing operations.

use crate::core::mark::{Anchor, MarkIntervalId, MarkKind, MarkSet, MarkValue};
use crate::core::{OpId, Sequence, SequenceOp, StateVector};
use std::collections::{BTreeMap, HashMap};
use std::ops::{Deref, DerefMut};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

pub mod frontmatter;
mod inline;
pub mod mark_ops;
mod parser;
mod serialize;
mod source;
pub mod text;

pub(crate) use source::DocumentSource;

pub use frontmatter::{Frontmatter, FrontmatterError};
pub use parser::Parser;
use serialize::{
    grapheme_offset_to_byte, is_grapheme_boundary, normalize_structural, serialize_block,
};
pub use text::{
    TextUnit, after_for_grapheme_offset, grapheme_count, insert_graphemes, paragraph_visible_ids,
    paragraph_visible_string, units_from_str, units_from_str_at,
};

pub type BlockId = Uuid;

/// Derive a stable [`BlockId`] (and table [`RowId`]) from the create [`OpId`].
///
/// Layout: high 64 bits = peer, low 64 bits = counter. Same create op always
/// yields the same id; no random UUIDs on collab create paths.
pub fn block_id_from_op(op: OpId) -> BlockId {
    Uuid::from_u128(((op.peer as u128) << 64) | (op.counter as u128))
}

#[derive(Debug)]
pub struct Document {
    pub frontmatter: Option<Frontmatter>,
    pub blocks: IndexedBlocks,
    source: Option<DocumentSource>,
    block_index: RwLock<Option<CachedBlockIndex>>,
}

/// Top-level block sequence that invalidates the document index on mutation.
#[derive(Debug)]
pub struct IndexedBlocks {
    sequence: Sequence<Block>,
    generation: u64,
}

impl IndexedBlocks {
    fn new(sequence: Sequence<Block>) -> Self {
        Self {
            sequence,
            generation: 0,
        }
    }

    fn generation(&self) -> u64 {
        self.generation
    }
}

impl Clone for IndexedBlocks {
    fn clone(&self) -> Self {
        Self::new(self.sequence.clone())
    }
}

impl PartialEq for IndexedBlocks {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence
    }
}

impl Eq for IndexedBlocks {}

impl Deref for IndexedBlocks {
    type Target = Sequence<Block>;

    fn deref(&self) -> &Self::Target {
        &self.sequence
    }
}

impl DerefMut for IndexedBlocks {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.generation = self.generation.wrapping_add(1);
        &mut self.sequence
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockContainerPath {
    BlockQuote(OpId),
    ListItem { list: OpId, item: OpId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockPath {
    containers: Vec<BlockContainerPath>,
    elem_id: OpId,
}

#[derive(Debug, Default)]
struct BlockIndex {
    by_block_id: HashMap<BlockId, BlockPath>,
    by_elem_id: HashMap<OpId, BlockPath>,
}

#[derive(Debug)]
struct CachedBlockIndex {
    generation: u64,
    index: BlockIndex,
}

impl Clone for Document {
    fn clone(&self) -> Self {
        Self {
            frontmatter: self.frontmatter.clone(),
            blocks: self.blocks.clone(),
            source: self.source.clone(),
            block_index: RwLock::new(None),
        }
    }
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        self.frontmatter == other.frontmatter
            && self.blocks == other.blocks
            && self.source == other.source
    }
}

impl Eq for Document {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub elem_id: OpId,
    /// Winning operation for kind-specific metadata/body replacement.
    pub kind_op: OpId,
    /// Causal frontier observed by the winning kind-specific replacement.
    pub kind_observed: StateVector,
    pub kind: BlockKind,
    pub marks: MarkSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ListDelimiter {
    Period,
    Parenthesis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BulletMarker {
    Dash,
    Plus,
    Asterisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TaskState {
    Unchecked,
    Checked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ListStyle {
    pub ordered: bool,
    pub start: u32,
    pub delimiter: ListDelimiter,
    pub bullet: BulletMarker,
    pub loose: bool,
}

impl Default for ListStyle {
    fn default() -> Self {
        Self {
            ordered: false,
            start: 1,
            delimiter: ListDelimiter::Period,
            bullet: BulletMarker::Dash,
            loose: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FenceMarker {
    Backtick,
    Tilde,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CodeFenceStyle {
    pub marker: FenceMarker,
    pub length: u8,
}

impl Default for CodeFenceStyle {
    fn default() -> Self {
        Self {
            marker: FenceMarker::Backtick,
            length: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// Paragraph body as a CRDT sequence of grapheme clusters.
    Paragraph {
        text: Sequence<TextUnit>,
    },
    /// ATX or setext heading; body is grapheme units like a paragraph.
    Heading {
        /// 1–6
        level: u8,
        text: Sequence<TextUnit>,
    },
    /// Ordered or unordered list of items (items may nest further lists).
    List {
        style: ListStyle,
        items: Sequence<ListItem>,
        pending_moves: Vec<PendingListItemMove>,
    },
    CodeFence {
        style: CodeFenceStyle,
        info: Option<String>,
        text: String,
    },
    BlockQuote {
        children: Sequence<Block>,
    },
    RawBlock {
        raw: String,
    },
    Table {
        table: Box<Table>,
    },
}

/// One list item; children are typically paragraphs and nested lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub id: BlockId,
    pub elem_id: OpId,
    pub task: Option<TaskState>,
    pub task_op: OpId,
    pub task_observed: StateVector,
    pub placement_observed: StateVector,
    pub children: Sequence<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingListItemMove {
    pub from_list_elem: OpId,
    pub to_list_elem: OpId,
    pub list_id: BlockId,
    pub item_id: BlockId,
    pub target: OpId,
    pub id: OpId,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
    pub observed: StateVector,
}

impl BlockKind {
    /// Paragraph from a string; unit OpIds start at `start` (peer + counter chain).
    pub fn paragraph(s: &str, start: OpId) -> Self {
        BlockKind::Paragraph {
            text: units_from_str_at(s, start),
        }
    }

    /// Heading from a string; unit OpIds start at `start`.
    pub fn heading(level: u8, s: &str, start: OpId) -> Self {
        BlockKind::Heading {
            level: level.clamp(1, 6),
            text: units_from_str_at(s, start),
        }
    }
}

/// Mutable access to a block's grapheme sequence when it is paragraph or heading text.
pub fn block_text_seq_mut(kind: &mut BlockKind) -> Option<&mut Sequence<TextUnit>> {
    match kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => Some(text),
        _ => None,
    }
}

/// Shared visible-string helper for paragraph/heading bodies.
pub fn block_text_seq(kind: &BlockKind) -> Option<&Sequence<TextUnit>> {
    match kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => Some(text),
        _ => None,
    }
}

fn projection_root(blocks: &Sequence<Block>, target: BlockId) -> Option<BlockId> {
    blocks
        .iter()
        .find(|block| projection_block_contains(block, target))
        .map(|block| block.id)
}

fn projection_block_contains(block: &Block, target: BlockId) -> bool {
    if block.id == target {
        return true;
    }
    match &block.kind {
        BlockKind::BlockQuote { children } => children
            .iter()
            .any(|child| projection_block_contains(child, target)),
        BlockKind::List { items, .. } => items.iter().any(|item| {
            item.id == target
                || item
                    .children
                    .iter()
                    .any(|child| projection_block_contains(child, target))
        }),
        _ => false,
    }
}

fn index_block_sequence(
    sequence: &Sequence<Block>,
    containers: &[BlockContainerPath],
    index: &mut BlockIndex,
) {
    for element in sequence.iter_all() {
        let Some(block) = element.value.as_ref() else {
            continue;
        };
        let path = BlockPath {
            containers: containers.to_vec(),
            elem_id: element.id,
        };
        index
            .by_block_id
            .entry(block.id)
            .or_insert_with(|| path.clone());
        index.by_elem_id.entry(element.id).or_insert(path);

        match &block.kind {
            BlockKind::BlockQuote { children } => {
                let mut nested = containers.to_vec();
                nested.push(BlockContainerPath::BlockQuote(element.id));
                index_block_sequence(children, &nested, index);
            }
            BlockKind::List { items, .. } => {
                for item_element in items.iter_all() {
                    let Some(item) = item_element.value.as_ref() else {
                        continue;
                    };
                    let mut nested = containers.to_vec();
                    nested.push(BlockContainerPath::ListItem {
                        list: element.id,
                        item: item_element.id,
                    });
                    index_block_sequence(&item.children, &nested, index);
                }
            }
            _ => {}
        }
    }
}

fn block_at_path<'a>(sequence: &'a Sequence<Block>, path: &BlockPath) -> Option<&'a Block> {
    let mut current = sequence;
    for container in &path.containers {
        match *container {
            BlockContainerPath::BlockQuote(id) => {
                let block = current.get_element(&id)?.value.as_ref()?;
                let BlockKind::BlockQuote { children } = &block.kind else {
                    return None;
                };
                current = children;
            }
            BlockContainerPath::ListItem { list, item } => {
                let block = current.get_element(&list)?.value.as_ref()?;
                let BlockKind::List { items, .. } = &block.kind else {
                    return None;
                };
                current = &items.get_element(&item)?.value.as_ref()?.children;
            }
        }
    }
    current.get_element(&path.elem_id)?.value.as_ref()
}

fn block_at_path_mut<'a>(
    sequence: &'a mut Sequence<Block>,
    containers: &[BlockContainerPath],
    elem_id: OpId,
) -> Option<&'a mut Block> {
    let Some((container, rest)) = containers.split_first() else {
        return sequence.value_mut(elem_id);
    };
    match *container {
        BlockContainerPath::BlockQuote(id) => {
            let block = sequence.value_mut(id)?;
            let BlockKind::BlockQuote { children } = &mut block.kind else {
                return None;
            };
            block_at_path_mut(children, rest, elem_id)
        }
        BlockContainerPath::ListItem { list, item } => {
            let block = sequence.value_mut(list)?;
            let BlockKind::List { items, .. } = &mut block.kind else {
                return None;
            };
            let item = items.value_mut(item)?;
            block_at_path_mut(&mut item.children, rest, elem_id)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquivalenceMode {
    Exact,
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializeConfig {
    pub equivalence: EquivalenceMode,
    pub prefer_raw_source: bool,
}

impl SerializeConfig {
    pub fn exact() -> Self {
        Self {
            equivalence: EquivalenceMode::Exact,
            prefer_raw_source: true,
        }
    }

    pub fn structural() -> Self {
        Self {
            equivalence: EquivalenceMode::Structural,
            prefer_raw_source: false,
        }
    }
}

impl Default for SerializeConfig {
    fn default() -> Self {
        Self::exact()
    }
}

pub type RowId = Uuid;
pub type ColumnId = Uuid;
pub type CellContent = String;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColumnAlignment {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ColumnDef {
    pub alignment: ColumnAlignment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub id: ColumnId,
    pub elem_id: OpId,
    pub deleted: crate::core::LwwRegister<bool>,
    pub alignment: crate::core::LwwRegister<ColumnAlignment>,
    pub alignment_observed: StateVector,
    pub placement_observed: StateVector,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct CellAddress {
    pub row_id: RowId,
    pub column_id: ColumnId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    pub value: CellContent,
    pub op_id: OpId,
    pub observed: StateVector,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingTableMove {
    pub logical_id: Uuid,
    pub target: OpId,
    pub id: OpId,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
    pub observed: StateVector,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingColumnAlignment {
    pub column_id: ColumnId,
    pub alignment: ColumnAlignment,
    pub id: OpId,
    pub observed: StateVector,
}

impl TableCell {
    pub fn new(value: CellContent, op_id: OpId, observed: StateVector) -> Self {
        Self {
            value,
            op_id,
            observed,
        }
    }

    pub fn set(&mut self, value: CellContent, op_id: OpId, observed: StateVector) {
        let incoming_observed_current =
            observed.get(self.op_id.peer).unwrap_or(0) >= self.op_id.counter;
        let current_observed_incoming = self.observed.get(op_id.peer).unwrap_or(0) >= op_id.counter;
        if incoming_observed_current || (!current_observed_incoming && op_id >= self.op_id) {
            self.value = value;
            self.op_id = op_id;
            self.observed = observed;
        }
    }
}

fn causal_write_wins(
    current_id: OpId,
    current_observed: &StateVector,
    incoming_id: OpId,
    incoming_observed: &StateVector,
) -> bool {
    let incoming_observed_current =
        incoming_observed.get(current_id.peer).unwrap_or(0) >= current_id.counter;
    let current_observed_incoming =
        current_observed.get(incoming_id.peer).unwrap_or(0) >= incoming_id.counter;
    incoming_observed_current || (!current_observed_incoming && incoming_id >= current_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub id: BlockId,
    pub elem_id: OpId,
    pub deleted: crate::core::LwwRegister<bool>,
    pub columns: Sequence<TableColumn>,
    pub rows: Sequence<TableRow>,
    pub cells: BTreeMap<CellAddress, TableCell>,
    pub pending_row_moves: Vec<PendingTableMove>,
    pub pending_column_moves: Vec<PendingTableMove>,
    pub pending_column_alignments: Vec<PendingColumnAlignment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub id: RowId,
    pub elem_id: OpId,
    pub deleted: crate::core::LwwRegister<bool>,
    pub placement_observed: StateVector,
}

impl Table {
    pub fn new(id: BlockId, elem_id: OpId, op_id: OpId) -> Self {
        Self {
            id,
            elem_id,
            deleted: crate::core::LwwRegister::new(false, op_id),
            columns: Sequence::new(),
            rows: Sequence::new(),
            cells: BTreeMap::new(),
            pending_row_moves: Vec::new(),
            pending_column_moves: Vec::new(),
            pending_column_alignments: Vec::new(),
        }
    }

    pub fn header_row_id(&self) -> RowId {
        self.id
    }

    pub fn insert_column(
        &mut self,
        after: Option<OpId>,
        alignment: ColumnAlignment,
        header: CellContent,
        op_id: OpId,
    ) {
        let column_id = block_id_from_op(op_id);
        let column = TableColumn {
            id: column_id,
            elem_id: op_id,
            deleted: crate::core::LwwRegister::new(false, op_id),
            alignment: crate::core::LwwRegister::new(alignment, op_id),
            alignment_observed: StateVector::new(),
            placement_observed: StateVector::new(),
        };
        self.columns.insert(after, column, op_id);
        self.set_cell(self.header_row_id(), column_id, header, op_id);
        self.resolve_pending_column_ops(column_id);
    }

    pub fn insert_row(
        &mut self,
        after: Option<OpId>,
        cells: Vec<(ColumnId, CellContent)>,
        op_id: OpId,
    ) {
        let row_id = block_id_from_op(op_id);
        let row = TableRow {
            id: row_id,
            elem_id: op_id,
            deleted: crate::core::LwwRegister::new(false, op_id),
            placement_observed: StateVector::new(),
        };
        self.rows.insert(after, row, op_id);
        for (column_id, value) in cells {
            self.set_cell(row_id, column_id, value, op_id);
        }
        self.resolve_pending_row_moves(row_id);
    }

    pub fn remove_row(&mut self, target: OpId, op_id: OpId) {
        // Clone only the necessary row, not twice
        if let Some(existing) = self.rows.get_element(&target)
            && let Some(row) = existing.value.as_ref()
        {
            let mut updated = row.clone();
            updated.deleted.set(true, op_id);
            self.rows.update_value(target, updated);
        }
        self.rows.delete(target, op_id);
    }

    pub fn set_cell(
        &mut self,
        row_id: RowId,
        column_id: ColumnId,
        value: CellContent,
        op_id: OpId,
    ) {
        self.set_cell_observed(row_id, column_id, value, op_id, StateVector::new());
    }

    pub fn set_cell_observed(
        &mut self,
        row_id: RowId,
        column_id: ColumnId,
        value: CellContent,
        op_id: OpId,
        observed: StateVector,
    ) {
        let address = CellAddress { row_id, column_id };
        self.cells
            .entry(address)
            .and_modify(|cell| cell.set(value.clone(), op_id, observed.clone()))
            .or_insert_with(|| TableCell::new(value, op_id, observed));
    }

    pub fn cell_value(&self, row_id: RowId, column_id: ColumnId) -> Option<&str> {
        self.row_is_live(row_id)
            .then_some(())
            .and_then(|_| self.column_by_id(column_id))
            .and_then(|_| self.cells.get(&CellAddress { row_id, column_id }))
            .map(|cell| cell.value.as_str())
    }

    pub fn row_cells(&self, row_id: RowId) -> Vec<CellContent> {
        self.columns_in_order()
            .into_iter()
            .map(|column| {
                self.cell_value(row_id, column.id)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    pub fn columns_in_order(&self) -> Vec<TableColumn> {
        self.columns.iter().cloned().collect()
    }

    pub fn column_by_id(&self, column_id: ColumnId) -> Option<&TableColumn> {
        self.columns.iter().find(|column| column.id == column_id)
    }

    pub fn set_column_alignment(
        &mut self,
        column_id: ColumnId,
        alignment: ColumnAlignment,
        op_id: OpId,
    ) {
        self.set_column_alignment_observed(column_id, alignment, op_id, StateVector::new());
    }

    pub fn set_column_alignment_observed(
        &mut self,
        column_id: ColumnId,
        alignment: ColumnAlignment,
        op_id: OpId,
        observed: StateVector,
    ) {
        let Some(mut updated) = self.column_by_id(column_id).cloned() else {
            self.pending_column_alignments.push(PendingColumnAlignment {
                column_id,
                alignment,
                id: op_id,
                observed,
            });
            return;
        };
        let current_id = updated.alignment.op_id();
        let incoming_observed_current =
            observed.get(current_id.peer).unwrap_or(0) >= current_id.counter;
        let current_observed_incoming =
            updated.alignment_observed.get(op_id.peer).unwrap_or(0) >= op_id.counter;
        if incoming_observed_current || (!current_observed_incoming && op_id >= current_id) {
            updated.alignment = crate::core::LwwRegister::new(alignment, op_id);
            updated.alignment_observed = observed;
            self.columns.update_value(updated.elem_id, updated);
        }
    }

    fn row_is_live(&self, row_id: RowId) -> bool {
        row_id == self.header_row_id() || self.row_by_id(row_id).is_some()
    }

    pub fn row_by_id(&self, row_id: RowId) -> Option<&TableRow> {
        self.rows.iter().find(|row| row.id == row_id)
    }

    pub(crate) fn move_row(
        &mut self,
        row_id: RowId,
        target: OpId,
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
        observed: StateVector,
    ) -> bool {
        let Some(mut row) = self.row_by_id(row_id).cloned() else {
            if self.rows.get_element(&target).is_none() {
                self.pending_row_moves.push(PendingTableMove {
                    logical_id: row_id,
                    target,
                    id,
                    after,
                    right_origin,
                    observed,
                });
            } else if self.rows.get_element(&id).is_none() {
                let tombstone = TableRow {
                    id: row_id,
                    elem_id: id,
                    deleted: crate::core::LwwRegister::new(true, id),
                    placement_observed: observed,
                };
                self.rows.apply(SequenceOp::Insert {
                    after,
                    id,
                    value: tombstone,
                    right_origin,
                });
                self.rows.delete(id, id);
            }
            return false;
        };
        let already_moved = block_id_from_op(row.elem_id) != row.id;
        let incoming_observed_current =
            observed.get(row.elem_id.peer).unwrap_or(0) >= row.elem_id.counter;
        let current_observed_incoming =
            row.placement_observed.get(id.peer).unwrap_or(0) >= id.counter;
        if already_moved
            && !incoming_observed_current
            && (current_observed_incoming || row.elem_id >= id)
        {
            if self.rows.get_element(&id).is_none() {
                row.elem_id = id;
                self.rows.apply(SequenceOp::Insert {
                    after,
                    id,
                    value: row,
                    right_origin,
                });
                self.rows.delete(id, id);
            }
            return false;
        }
        self.rows.delete(row.elem_id, id);
        row.elem_id = id;
        row.placement_observed = observed;
        self.rows.apply(SequenceOp::Insert {
            after,
            id,
            value: row,
            right_origin,
        });
        true
    }

    pub fn rows_in_order(&self) -> Vec<TableRow> {
        self.rows.iter().cloned().collect()
    }

    pub fn remove_column(&mut self, column_id: ColumnId, op_id: OpId) {
        if let Some(column) = self.column_by_id(column_id).cloned() {
            self.columns.delete(column.elem_id, op_id);
        }
    }

    pub(crate) fn move_column(
        &mut self,
        column_id: ColumnId,
        target: OpId,
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
        observed: StateVector,
    ) -> bool {
        let Some(mut column) = self.column_by_id(column_id).cloned() else {
            if self.columns.get_element(&target).is_none() {
                self.pending_column_moves.push(PendingTableMove {
                    logical_id: column_id,
                    target,
                    id,
                    after,
                    right_origin,
                    observed,
                });
            } else if self.columns.get_element(&id).is_none() {
                let tombstone = TableColumn {
                    id: column_id,
                    elem_id: id,
                    deleted: crate::core::LwwRegister::new(true, id),
                    alignment: crate::core::LwwRegister::new(ColumnAlignment::Left, id),
                    alignment_observed: StateVector::new(),
                    placement_observed: observed,
                };
                self.columns.apply(SequenceOp::Insert {
                    after,
                    id,
                    value: tombstone,
                    right_origin,
                });
                self.columns.delete(id, id);
            }
            return false;
        };
        let already_moved = block_id_from_op(column.elem_id) != column.id;
        let incoming_observed_current =
            observed.get(column.elem_id.peer).unwrap_or(0) >= column.elem_id.counter;
        let current_observed_incoming =
            column.placement_observed.get(id.peer).unwrap_or(0) >= id.counter;
        if already_moved
            && !incoming_observed_current
            && (current_observed_incoming || column.elem_id >= id)
        {
            if self.columns.get_element(&id).is_none() {
                column.elem_id = id;
                self.columns.apply(SequenceOp::Insert {
                    after,
                    id,
                    value: column,
                    right_origin,
                });
                self.columns.delete(id, id);
            }
            return false;
        }
        self.columns.delete(column.elem_id, id);
        column.elem_id = id;
        column.placement_observed = observed;
        self.columns.apply(SequenceOp::Insert {
            after,
            id,
            value: column,
            right_origin,
        });
        true
    }

    pub(crate) fn resolve_pending_row_moves(&mut self, row_id: RowId) {
        let mut pending = Vec::new();
        std::mem::swap(&mut pending, &mut self.pending_row_moves);
        pending.sort_by_key(|movement| movement.id);
        for movement in pending {
            if movement.logical_id == row_id {
                self.move_row(
                    row_id,
                    movement.target,
                    movement.id,
                    movement.after,
                    movement.right_origin,
                    movement.observed,
                );
            } else {
                self.pending_row_moves.push(movement);
            }
        }
    }

    pub(crate) fn resolve_pending_column_ops(&mut self, column_id: ColumnId) {
        let mut movements = Vec::new();
        std::mem::swap(&mut movements, &mut self.pending_column_moves);
        movements.sort_by_key(|movement| movement.id);
        for movement in movements {
            if movement.logical_id == column_id {
                self.move_column(
                    column_id,
                    movement.target,
                    movement.id,
                    movement.after,
                    movement.right_origin,
                    movement.observed,
                );
            } else {
                self.pending_column_moves.push(movement);
            }
        }
        let mut alignments = Vec::new();
        std::mem::swap(&mut alignments, &mut self.pending_column_alignments);
        alignments.sort_by_key(|alignment| alignment.id);
        for alignment in alignments {
            if alignment.column_id == column_id {
                self.set_column_alignment_observed(
                    column_id,
                    alignment.alignment,
                    alignment.id,
                    alignment.observed,
                );
            } else {
                self.pending_column_alignments.push(alignment);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertTextRun {
    pub block_id: BlockId,
    pub grapheme_offset: usize,
    pub byte_offset: usize,
    pub text: String,
    pub op_id: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    InsertText(InsertTextRun),
    /// Apply a rich mark interval on a block (anchors are text-unit OpIds).
    SetMark {
        block_id: BlockId,
        interval_id: MarkIntervalId,
        kind: MarkKind,
        start: Anchor,
        end: Anchor,
        attrs: BTreeMap<String, MarkValue>,
        op_id: OpId,
    },
    /// Causal remove of a mark interval (optional range split emits follow-up SetMarks).
    RemoveMark {
        block_id: BlockId,
        interval_id: MarkIntervalId,
        observed: StateVector,
        op_id: OpId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("block not found")]
    BlockNotFound,
    #[error("invalid offset")]
    InvalidOffset,
    #[error("invalid grapheme boundary")]
    InvalidGraphemeBoundary,
}

impl Document {
    pub fn new() -> Self {
        Self {
            frontmatter: None,
            blocks: IndexedBlocks::new(Sequence::new()),
            source: None,
            block_index: RwLock::new(None),
        }
    }

    /// Read-only access to the top-level block sequence.
    pub fn blocks(&self) -> &Sequence<Block> {
        &self.blocks
    }

    /// Mutable access to the top-level block sequence, invalidating the BlockId index.
    pub fn blocks_mut(&mut self) -> &mut Sequence<Block> {
        self.source = None;
        &mut self.blocks
    }

    fn block_index_read(&self) -> RwLockReadGuard<'_, Option<CachedBlockIndex>> {
        self.block_index
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn block_index_write(&self) -> RwLockWriteGuard<'_, Option<CachedBlockIndex>> {
        self.block_index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn rebuild_block_index(&self) {
        let generation = self.blocks.generation();
        let mut index = BlockIndex::default();
        index_block_sequence(&self.blocks, &[], &mut index);
        *self.block_index_write() = Some(CachedBlockIndex { generation, index });
    }

    fn ensure_block_index(&self) {
        let generation = self.blocks.generation();
        if self
            .block_index_read()
            .as_ref()
            .is_some_and(|cached| cached.generation == generation)
        {
            return;
        }
        self.rebuild_block_index();
    }

    /// Resolve a stable block id to its sequence element id in O(1) average time.
    pub fn block_elem_id(&self, block_id: BlockId) -> Option<OpId> {
        self.find_block_by_id(block_id).map(|block| block.elem_id)
    }

    pub fn set_raw_source(&mut self, source: String) {
        let parsed = Parser::parse(&source);
        self.adopt_source_from(&parsed);
    }

    pub fn clear_raw_source(&mut self) {
        self.source = None;
    }

    pub(crate) fn source_state(&self) -> Option<DocumentSource> {
        self.source.clone()
    }

    pub(crate) fn has_source_state(&self) -> bool {
        self.source.is_some()
    }

    pub(crate) fn source_region_bytes(&self, block_id: BlockId) -> Option<usize> {
        self.source.as_ref()?.region_body_bytes(block_id)
    }

    pub(crate) fn projection_exact_region(&self, block_id: BlockId) -> Option<(BlockId, String)> {
        let root_id = self
            .source
            .as_ref()
            .and_then(|source| source.root_for_block(block_id))
            .or_else(|| projection_root(self.blocks(), block_id))?;
        let root = self.blocks().iter().find(|block| block.id == root_id)?;
        let markdown = self
            .source
            .as_ref()
            .and_then(|source| source.render_root_region(root_id, root))
            .unwrap_or_else(|| serialize_block(root));
        Some((root_id, markdown))
    }

    pub(crate) fn set_source_state(&mut self, mut source: Option<DocumentSource>) {
        if let Some(source) = &mut source {
            source.reindex(&self.blocks);
        }
        self.source = source;
    }

    pub(crate) fn adopt_source_from(&mut self, parsed: &Document) -> bool {
        let Some(source) = parsed.source.as_ref() else {
            self.source = None;
            return false;
        };
        let current = self.blocks_in_order();
        let parsed_blocks = parsed.blocks_in_order();
        if current.len() != parsed_blocks.len()
            || current.iter().zip(parsed_blocks).any(|(left, right)| {
                std::mem::discriminant(&left.kind) != std::mem::discriminant(&right.kind)
            })
        {
            self.source = None;
            return false;
        }
        self.source = source.adopt_for(&current);
        self.source.is_some()
    }

    fn mark_source_block_dirty(&mut self, block_id: BlockId) {
        if let Some(source) = &mut self.source {
            source.mark_block_dirty(block_id);
        }
    }

    fn mark_source_elem_dirty(&mut self, elem_id: OpId) {
        if let Some(source) = &mut self.source {
            source.mark_elem_dirty(elem_id);
        }
    }

    pub fn blocks_in_order(&self) -> Vec<&Block> {
        self.blocks.iter_asc().collect()
    }

    /// Find a block by `elem_id` anywhere in the tree (nested in blockquotes or list items).
    pub fn find_block(&self, elem_id: OpId) -> Option<&Block> {
        self.ensure_block_index();
        let path = self
            .block_index_read()
            .as_ref()?
            .index
            .by_elem_id
            .get(&elem_id)
            .cloned();
        if let Some(path) = path
            && let Some(block) = block_at_path(&self.blocks, &path)
            && block.elem_id == elem_id
        {
            return Some(block);
        }
        self.rebuild_block_index();
        let path = self
            .block_index_read()
            .as_ref()?
            .index
            .by_elem_id
            .get(&elem_id)
            .cloned()?;
        block_at_path(&self.blocks, &path).filter(|block| block.elem_id == elem_id)
    }

    /// Find a list item by its `elem_id` anywhere in the tree.
    pub fn find_list_item(&self, elem_id: OpId) -> Option<&ListItem> {
        fn walk(seq: &Sequence<Block>, id: OpId) -> Option<&ListItem> {
            for e in seq.iter_all() {
                if let Some(b) = e.value.as_ref() {
                    if let BlockKind::List { items, .. } = &b.kind {
                        for ie in items.iter_all() {
                            if let Some(item) = ie.value.as_ref() {
                                if item.elem_id == id {
                                    return Some(item);
                                }
                                if let Some(f) = walk(&item.children, id) {
                                    return Some(f);
                                }
                            }
                        }
                    } else if let BlockKind::BlockQuote { children } = &b.kind
                        && let Some(f) = walk(children, id)
                    {
                        return Some(f);
                    }
                }
            }
            None
        }
        walk(&self.blocks, elem_id)
    }

    /// Find a list item by stable logical identity anywhere in the tree.
    pub fn find_list_item_by_id(&self, item_id: BlockId) -> Option<&ListItem> {
        fn walk(sequence: &Sequence<Block>, item_id: BlockId) -> Option<&ListItem> {
            for block in sequence.iter() {
                match &block.kind {
                    BlockKind::List { items, .. } => {
                        for item in items.iter() {
                            if item.id == item_id {
                                return Some(item);
                            }
                            if let Some(found) = walk(&item.children, item_id) {
                                return Some(found);
                            }
                        }
                    }
                    BlockKind::BlockQuote { children } => {
                        if let Some(found) = walk(children, item_id) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&self.blocks, item_id)
    }

    /// Mutate a block by `elem_id` anywhere in the tree. Returns `None` if not found.
    pub fn with_block_mut<R>(
        &mut self,
        elem_id: OpId,
        f: impl FnOnce(&mut Block) -> R,
    ) -> Option<R> {
        self.find_block(elem_id)?;
        self.mark_source_elem_dirty(elem_id);
        let path = self
            .block_index_read()
            .as_ref()?
            .index
            .by_elem_id
            .get(&elem_id)
            .cloned()?;
        block_at_path_mut(&mut self.blocks, &path.containers, path.elem_id).map(f)
    }

    /// Apply `f` to the children of the container with `container_elem` (a blockquote block
    /// or a list item), searching the whole tree. `None` if no such container exists.
    fn with_container_children_mut<R>(
        &mut self,
        container_elem: OpId,
        f: impl FnOnce(&mut Sequence<Block>) -> R,
    ) -> Option<R> {
        fn walk<R, F: FnOnce(&mut Sequence<Block>) -> R>(
            seq: &mut Sequence<Block>,
            target: OpId,
            f: &mut Option<F>,
        ) -> Option<R> {
            for bid in seq.ids() {
                if seq.value_mut(bid).is_some_and(|b| {
                    b.elem_id == target && matches!(b.kind, BlockKind::BlockQuote { .. })
                }) {
                    let func = f.take()?;
                    return seq.value_mut(bid).and_then(|b| match &mut b.kind {
                        BlockKind::BlockQuote { children } => Some(func(children)),
                        _ => None,
                    });
                }
                let has_item = seq.value_mut(bid).is_some_and(|b| match &b.kind {
                    BlockKind::List { items, .. } => items.iter().any(|it| it.elem_id == target),
                    _ => false,
                });
                if has_item {
                    let func = f.take()?;
                    if let Some(b) = seq.value_mut(bid)
                        && let BlockKind::List { items, .. } = &mut b.kind
                    {
                        for iid in items.ids() {
                            if items.value_mut(iid).is_some_and(|it| it.elem_id == target) {
                                return items.value_mut(iid).map(|it| func(&mut it.children));
                            }
                        }
                    }
                    return None;
                }
            }
            for bid in seq.ids() {
                if let Some(b) = seq.value_mut(bid) {
                    match &mut b.kind {
                        BlockKind::BlockQuote { children } => {
                            if let Some(r) = walk(children, target, f) {
                                return Some(r);
                            }
                        }
                        BlockKind::List { items, .. } => {
                            for iid in items.ids() {
                                if let Some(item) = items.value_mut(iid)
                                    && let Some(r) = walk(&mut item.children, target, f)
                                {
                                    return Some(r);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            None
        }
        let mut f = Some(f);
        walk(&mut self.blocks, container_elem, &mut f)
    }

    /// Insert a block into `parent`'s children (top-level when `parent` is `None`).
    /// Returns `false` if `parent` is not a container (blockquote or list item) in the tree.
    pub fn insert_block_at(
        &mut self,
        parent: Option<OpId>,
        after: Option<OpId>,
        id: OpId,
        value: Block,
        right_origin: Option<OpId>,
    ) -> bool {
        if let Some(parent) = parent {
            self.mark_source_elem_dirty(parent);
        }
        match parent {
            None => {
                self.blocks.apply(SequenceOp::Insert {
                    after,
                    id,
                    value,
                    right_origin,
                });
                true
            }
            Some(p) => self
                .with_container_children_mut(p, |children| {
                    children.apply(SequenceOp::Insert {
                        after,
                        id,
                        value,
                        right_origin,
                    });
                })
                .is_some(),
        }
    }

    /// Delete a block from `parent`'s children (top-level when `parent` is `None`).
    pub fn delete_block_at(&mut self, parent: Option<OpId>, target: OpId, id: OpId) -> bool {
        if let Some(parent) = parent {
            self.mark_source_elem_dirty(parent);
        }
        match parent {
            None => {
                self.blocks.apply(SequenceOp::Delete { target, id });
                true
            }
            Some(p) => self
                .with_container_children_mut(p, |children| {
                    children.apply(SequenceOp::Delete { target, id });
                })
                .is_some(),
        }
    }

    /// The children sequence of a container (top-level when `parent` is `None`); `None`
    /// if `parent` is not a container (blockquote or list item) in the tree.
    pub fn container_children(&self, parent: Option<OpId>) -> Option<&Sequence<Block>> {
        match parent {
            None => Some(&self.blocks),
            Some(p) => {
                if let Some(Block {
                    kind: BlockKind::BlockQuote { children },
                    ..
                }) = self.find_block(p)
                {
                    Some(children)
                } else {
                    self.find_list_item(p).map(|it| &it.children)
                }
            }
        }
    }

    /// Compute the RGA `right_origin` for an insert into `parent`'s children.
    pub fn compute_child_right_origin(
        &self,
        parent: Option<OpId>,
        after: Option<OpId>,
    ) -> Option<OpId> {
        self.container_children(parent)
            .and_then(|c| c.compute_right_origin(after))
    }

    /// Visible item sequence for one logical list block.
    pub fn list_items(&self, list_id: BlockId) -> Option<&Sequence<ListItem>> {
        let block = self.find_block_by_id(list_id)?;
        match &block.kind {
            BlockKind::List { items, .. } => Some(items),
            _ => None,
        }
    }

    /// Logical list block containing an item.
    pub fn list_containing_item(&self, item_id: BlockId) -> Option<(BlockId, OpId)> {
        self.list_item_placement(item_id)
            .map(|(list_id, list_elem, _)| (list_id, list_elem))
    }

    /// Current list and sequence placement for one logical list item.
    pub fn list_item_placement(&self, item_id: BlockId) -> Option<(BlockId, OpId, OpId)> {
        fn walk(sequence: &Sequence<Block>, item_id: BlockId) -> Option<(BlockId, OpId, OpId)> {
            for block in sequence.iter() {
                match &block.kind {
                    BlockKind::List { items, .. } => {
                        if let Some(placement) = items.iter_all().find_map(|element| {
                            element
                                .value
                                .as_ref()
                                .filter(|item| item.id == item_id)
                                .map(|_| element.id)
                        }) {
                            return Some((block.id, block.elem_id, placement));
                        }
                        for item in items.iter() {
                            if let Some(found) = walk(&item.children, item_id) {
                                return Some(found);
                            }
                        }
                    }
                    BlockKind::BlockQuote { children } => {
                        if let Some(found) = walk(children, item_id) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&self.blocks, item_id)
    }

    pub fn compute_list_item_right_origin(
        &self,
        list_id: BlockId,
        after: Option<OpId>,
    ) -> Option<OpId> {
        self.list_items(list_id)?.compute_right_origin(after)
    }

    pub(crate) fn insert_list_item_at(
        &mut self,
        list_elem: OpId,
        after: Option<OpId>,
        id: OpId,
        right_origin: Option<OpId>,
        task: Option<TaskState>,
    ) -> bool {
        let item_id = block_id_from_op(id);
        let inserted = self
            .with_block_mut(list_elem, |block| {
                let BlockKind::List { items, .. } = &mut block.kind else {
                    return false;
                };
                items.apply(SequenceOp::Insert {
                    after,
                    id,
                    value: ListItem {
                        id: item_id,
                        elem_id: id,
                        task,
                        task_op: id,
                        task_observed: StateVector::new(),
                        placement_observed: StateVector::new(),
                        children: Sequence::new(),
                    },
                    right_origin,
                });
                true
            })
            .unwrap_or(false);
        if inserted {
            self.resolve_pending_list_item_moves(item_id);
        }
        inserted
    }

    pub(crate) fn delete_list_item_at(
        &mut self,
        list_elem: OpId,
        item_id: BlockId,
        target: OpId,
        id: OpId,
    ) -> bool {
        let (list_elem, target) = self
            .list_item_placement(item_id)
            .map(|(_, current_list, placement)| (current_list, placement))
            .unwrap_or((list_elem, target));
        self.with_block_mut(list_elem, |block| {
            let BlockKind::List { items, .. } = &mut block.kind else {
                return false;
            };
            if let Some(element) = items.get_element(&target)
                && element
                    .value
                    .as_ref()
                    .is_some_and(|item| item.id != item_id)
            {
                return false;
            }
            items.apply(SequenceOp::Delete { target, id });
            true
        })
        .unwrap_or(false)
    }

    pub(crate) fn move_list_item_at(&mut self, movement: &PendingListItemMove) -> bool {
        let Some(item) = self.find_list_item_by_id(movement.item_id).cloned() else {
            let deleted_target = self
                .find_block(movement.from_list_elem)
                .and_then(|block| match &block.kind {
                    BlockKind::List { items, .. } => items.get_element(&movement.target),
                    _ => None,
                })
                .is_some_and(|element| element.value.is_none());
            if !deleted_target {
                return self.with_block_mut(movement.from_list_elem, |block| {
                    let BlockKind::List { pending_moves, .. } = &mut block.kind else {
                        return false;
                    };
                    if !pending_moves
                        .iter()
                        .any(|pending| pending.id == movement.id)
                    {
                        pending_moves.push(movement.clone());
                    }
                    true
                }) == Some(true);
            }
            return self.with_block_mut(movement.to_list_elem, |block| {
                let BlockKind::List { items, .. } = &mut block.kind else {
                    return false;
                };
                items.apply(SequenceOp::Insert {
                    after: movement.after,
                    id: movement.id,
                    value: ListItem {
                        id: movement.item_id,
                        elem_id: movement.id,
                        task: None,
                        task_op: movement.id,
                        task_observed: StateVector::new(),
                        placement_observed: movement.observed.clone(),
                        children: Sequence::new(),
                    },
                    right_origin: movement.right_origin,
                });
                items.apply(SequenceOp::Delete {
                    target: movement.id,
                    id: movement.id,
                });
                true
            }) == Some(true);
        };
        let current_placement = self
            .list_item_placement(movement.item_id)
            .map_or(movement.target, |(_, _, placement)| placement);
        let already_moved = block_id_from_op(current_placement) != movement.item_id;
        if already_moved
            && !causal_write_wins(
                current_placement,
                &item.placement_observed,
                movement.id,
                &movement.observed,
            )
        {
            if self
                .find_block(movement.to_list_elem)
                .and_then(|block| match &block.kind {
                    BlockKind::List { items, .. } => items.get_element(&movement.id),
                    _ => None,
                })
                .is_none()
            {
                let mut tombstone = item;
                tombstone.placement_observed = movement.observed.clone();
                let _ = self.with_block_mut(movement.to_list_elem, |block| {
                    let BlockKind::List { items, .. } = &mut block.kind else {
                        return;
                    };
                    items.apply(SequenceOp::Insert {
                        after: movement.after,
                        id: movement.id,
                        value: tombstone,
                        right_origin: movement.right_origin,
                    });
                    items.apply(SequenceOp::Delete {
                        target: movement.id,
                        id: movement.id,
                    });
                });
            }
            return false;
        }
        let current = current_placement;
        let current_list_elem = self
            .list_containing_item(movement.item_id)
            .map_or(movement.from_list_elem, |(_, elem)| elem);
        let deleted = self.with_block_mut(current_list_elem, |block| {
            let BlockKind::List { items, .. } = &mut block.kind else {
                return false;
            };
            if items.get_element(&current).is_none() {
                return false;
            }
            items.apply(SequenceOp::Delete {
                target: current,
                id: movement.id,
            });
            true
        });
        if deleted != Some(true) {
            return false;
        }
        let mut item = item;
        item.placement_observed = movement.observed.clone();
        self.with_block_mut(movement.to_list_elem, |block| {
            let BlockKind::List { items, .. } = &mut block.kind else {
                return false;
            };
            items.apply(SequenceOp::Insert {
                after: movement.after,
                id: movement.id,
                value: item,
                right_origin: movement.right_origin,
            });
            true
        }) == Some(true)
    }

    fn resolve_pending_list_item_moves(&mut self, item_id: BlockId) {
        let Some((_, list_elem)) = self.list_containing_item(item_id) else {
            return;
        };
        let mut pending = self
            .with_block_mut(list_elem, |block| {
                let BlockKind::List { pending_moves, .. } = &mut block.kind else {
                    return Vec::new();
                };
                let mut retained = Vec::new();
                let mut matching = Vec::new();
                for movement in std::mem::take(pending_moves) {
                    if movement.item_id == item_id {
                        matching.push(movement);
                    } else {
                        retained.push(movement);
                    }
                }
                *pending_moves = retained;
                matching
            })
            .unwrap_or_default();
        pending.sort_by_key(|movement| movement.id);
        for movement in pending {
            self.move_list_item_at(&movement);
        }
    }

    pub(crate) fn set_list_style(
        &mut self,
        block_elem: OpId,
        style: ListStyle,
        id: OpId,
        observed: StateVector,
    ) -> bool {
        self.with_block_mut(block_elem, |block| {
            let BlockKind::List { style: current, .. } = &mut block.kind else {
                return false;
            };
            if !causal_write_wins(block.kind_op, &block.kind_observed, id, &observed) {
                return false;
            }
            *current = style;
            block.kind_op = id;
            block.kind_observed = observed;
            true
        })
        .unwrap_or(false)
    }

    pub(crate) fn set_list_item_task(
        &mut self,
        item_id: BlockId,
        task: Option<TaskState>,
        id: OpId,
        observed: StateVector,
    ) -> bool {
        let Some((_, list_elem, placement)) = self.list_item_placement(item_id) else {
            return false;
        };
        self.with_block_mut(list_elem, |block| {
            let BlockKind::List { items, .. } = &mut block.kind else {
                return false;
            };
            let Some(item) = items.value_mut(placement).filter(|item| item.id == item_id) else {
                return false;
            };
            if !causal_write_wins(item.task_op, &item.task_observed, id, &observed) {
                return false;
            }
            item.task = task;
            item.task_op = id;
            item.task_observed = observed;
            true
        })
        .unwrap_or(false)
    }

    pub(crate) fn set_code_fence(
        &mut self,
        block_elem: OpId,
        style: CodeFenceStyle,
        info: Option<String>,
        text: String,
        id: OpId,
        observed: StateVector,
    ) -> bool {
        self.with_block_mut(block_elem, |block| {
            if !causal_write_wins(block.kind_op, &block.kind_observed, id, &observed)
                || !matches!(block.kind, BlockKind::CodeFence { .. })
            {
                return false;
            }
            block.kind = BlockKind::CodeFence { style, info, text };
            block.kind_op = id;
            block.kind_observed = observed;
            true
        })
        .unwrap_or(false)
    }

    pub(crate) fn convert_text_block(
        &mut self,
        block_elem: OpId,
        kind: crate::codec::TextBlockKindWire,
        id: OpId,
        observed: StateVector,
    ) -> bool {
        self.with_block_mut(block_elem, |block| {
            if !causal_write_wins(block.kind_op, &block.kind_observed, id, &observed) {
                return false;
            }
            let text = match &mut block.kind {
                BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
                    std::mem::replace(text, Sequence::new())
                }
                _ => return false,
            };
            block.kind = match kind {
                crate::codec::TextBlockKindWire::Paragraph => BlockKind::Paragraph { text },
                crate::codec::TextBlockKindWire::Heading { level } => {
                    BlockKind::Heading { level, text }
                }
            };
            block.kind_op = id;
            block.kind_observed = observed;
            true
        })
        .unwrap_or(false)
    }

    pub(crate) fn replace_raw_block(
        &mut self,
        block_elem: OpId,
        raw: String,
        id: OpId,
        observed: StateVector,
    ) -> bool {
        self.with_block_mut(block_elem, |block| {
            if !causal_write_wins(block.kind_op, &block.kind_observed, id, &observed)
                || !matches!(block.kind, BlockKind::RawBlock { .. })
            {
                return false;
            }
            block.kind = BlockKind::RawBlock { raw };
            block.kind_op = id;
            block.kind_observed = observed;
            true
        })
        .unwrap_or(false)
    }

    /// Current container placement for a logical block (`None` is top-level).
    pub fn block_parent(&self, block_id: BlockId) -> Option<Option<OpId>> {
        self.ensure_block_index();
        let path = self
            .block_index_read()
            .as_ref()?
            .index
            .by_block_id
            .get(&block_id)?
            .clone();
        Some(path.containers.last().map(|container| match container {
            BlockContainerPath::BlockQuote(id) => *id,
            BlockContainerPath::ListItem { item, .. } => *item,
        }))
    }

    /// Whether `candidate` is the block itself or one of its descendant containers.
    pub fn block_contains_container(&self, block_id: BlockId, candidate: OpId) -> bool {
        fn contains(block: &Block, candidate: OpId) -> bool {
            if block.elem_id == candidate {
                return true;
            }
            match &block.kind {
                BlockKind::BlockQuote { children } => {
                    children.iter().any(|child| contains(child, candidate))
                }
                BlockKind::List { items, .. } => items.iter().any(|item| {
                    item.elem_id == candidate
                        || item.children.iter().any(|child| contains(child, candidate))
                }),
                _ => false,
            }
        }
        self.find_block_by_id(block_id)
            .is_some_and(|block| contains(block, candidate))
    }

    /// Apply an atomic identity-preserving placement change. Returns false when a newer
    /// concurrent move already won or any prerequisite is missing/invalid.
    pub(crate) fn move_blocks_at(
        &mut self,
        to_parent: Option<OpId>,
        moves: &[crate::codec::MovedBlockWire],
        move_id: OpId,
    ) -> bool {
        if moves.is_empty() || self.container_children(to_parent).is_none() {
            return false;
        }
        let mut prepared = Vec::with_capacity(moves.len());
        let mut loses = false;
        for moved in moves {
            let Some(block) = self.find_block_by_id(moved.block_id).cloned() else {
                prepared.push((
                    Block {
                        id: moved.block_id,
                        elem_id: moved.id,
                        kind_op: moved.id,
                        kind_observed: StateVector::new(),
                        kind: BlockKind::RawBlock { raw: String::new() },
                        marks: MarkSet::new(),
                    },
                    None,
                ));
                loses = true;
                continue;
            };
            if to_parent.is_some_and(|parent| self.block_contains_container(block.id, parent)) {
                return false;
            }
            let already_moved = block_id_from_op(block.elem_id) != block.id;
            if already_moved && block.elem_id >= moved.id {
                loses = true;
            }
            let Some(parent) = self.block_parent(block.id) else {
                return false;
            };
            prepared.push((block, Some(parent)));
        }
        if loses {
            // Materialize then tombstone losing concurrent placements so every replica has
            // the same RGA history, not merely the same visible order.
            for ((mut block, _), moved) in prepared.into_iter().zip(moves) {
                if self
                    .container_children(to_parent)
                    .is_some_and(|children| children.get_element(&moved.id).is_some())
                {
                    continue;
                }
                block.elem_id = moved.id;
                self.insert_block_at(to_parent, moved.after, moved.id, block, moved.right_origin);
                self.delete_block_at(to_parent, moved.id, move_id);
            }
            return false;
        }
        for (block, parent) in &prepared {
            self.delete_block_at(
                parent.expect("winning moves have a source"),
                block.elem_id,
                move_id,
            );
        }
        for ((mut block, _), moved) in prepared.into_iter().zip(moves) {
            block.elem_id = moved.id;
            self.insert_block_at(to_parent, moved.after, moved.id, block, moved.right_origin);
        }
        true
    }

    /// Find a block by its stable `BlockId` anywhere in the tree.
    pub fn find_block_by_id(&self, block_id: BlockId) -> Option<&Block> {
        self.ensure_block_index();
        let path = self
            .block_index_read()
            .as_ref()?
            .index
            .by_block_id
            .get(&block_id)
            .cloned();
        if let Some(path) = path
            && let Some(block) = block_at_path(&self.blocks, &path)
            && block.id == block_id
        {
            return Some(block);
        }
        self.rebuild_block_index();
        let path = self
            .block_index_read()
            .as_ref()?
            .index
            .by_block_id
            .get(&block_id)
            .cloned()?;
        block_at_path(&self.blocks, &path).filter(|block| block.id == block_id)
    }

    pub fn insert_text(
        &mut self,
        block_id: BlockId,
        grapheme_offset: usize,
        text: &str,
        op_id: OpId,
    ) -> Result<Vec<EditOp>, EditError> {
        let elem_id = self
            .block_elem_id(block_id)
            .ok_or(EditError::BlockNotFound)?;

        // Get element via O(1) lookup and clone block for modification
        let Some(existing) = self.blocks.get_element(&elem_id) else {
            return Err(EditError::BlockNotFound);
        };
        let Some(block) = existing.value.as_ref() else {
            return Err(EditError::BlockNotFound);
        };

        let mut updated = block.clone();
        let Some(body) = block_text_seq_mut(&mut updated.kind) else {
            return Err(EditError::InvalidOffset);
        };

        let visible = paragraph_visible_string(body);
        let byte_offset =
            grapheme_offset_to_byte(&visible, grapheme_offset).ok_or(EditError::InvalidOffset)?;
        insert_graphemes(body, grapheme_offset, text, op_id).ok_or(EditError::InvalidOffset)?;

        self.blocks.update_value(elem_id, updated);
        self.mark_source_block_dirty(block_id);

        Ok(vec![EditOp::InsertText(InsertTextRun {
            block_id,
            grapheme_offset,
            byte_offset,
            text: text.to_string(),
            op_id,
        })])
    }

    pub fn raw_apply_op(
        &mut self,
        op: EditOp,
        validate_grapheme_boundaries: bool,
    ) -> Result<(), EditError> {
        match op {
            EditOp::InsertText(run) => {
                // Find block's elem_id by block_id
                let elem_id = self
                    .blocks
                    .iter_asc()
                    .find(|block| block.id == run.block_id)
                    .map(|block| block.elem_id)
                    .ok_or(EditError::BlockNotFound)?;

                // Get element via O(1) lookup and clone block for modification
                let Some(existing) = self.blocks.get_element(&elem_id) else {
                    return Err(EditError::BlockNotFound);
                };
                let Some(block) = existing.value.as_ref() else {
                    return Err(EditError::BlockNotFound);
                };

                let mut updated = block.clone();
                let Some(body) = block_text_seq_mut(&mut updated.kind) else {
                    return Err(EditError::InvalidOffset);
                };

                let visible = paragraph_visible_string(body);
                let byte_offset = run.byte_offset;
                if byte_offset > visible.len() {
                    return Err(EditError::InvalidOffset);
                }
                if !visible.is_char_boundary(byte_offset) {
                    return Err(EditError::InvalidOffset);
                }
                if validate_grapheme_boundaries && !is_grapheme_boundary(&visible, byte_offset) {
                    return Err(EditError::InvalidGraphemeBoundary);
                }
                // Prefer grapheme_offset on the run; fall back to byte→grapheme map.
                let g_off = run.grapheme_offset;
                insert_graphemes(body, g_off, &run.text, run.op_id)
                    .ok_or(EditError::InvalidOffset)?;

                self.blocks.update_value(elem_id, updated);
                self.mark_source_block_dirty(run.block_id);
                Ok(())
            }
            EditOp::SetMark {
                block_id,
                interval_id,
                kind,
                start,
                end,
                attrs,
                op_id,
            } => {
                let elem_id = self
                    .block_elem_id(block_id)
                    .ok_or(EditError::BlockNotFound)?;
                let Some(existing) = self.blocks.get_element(&elem_id) else {
                    return Err(EditError::BlockNotFound);
                };
                let Some(block) = existing.value.as_ref() else {
                    return Err(EditError::BlockNotFound);
                };
                let mut updated = block.clone();
                updated
                    .marks
                    .set_mark(interval_id, kind, start, end, attrs, op_id);
                self.blocks.update_value(elem_id, updated);
                self.mark_source_block_dirty(block_id);
                Ok(())
            }
            EditOp::RemoveMark {
                block_id,
                interval_id,
                observed,
                op_id,
            } => {
                let elem_id = self
                    .block_elem_id(block_id)
                    .ok_or(EditError::BlockNotFound)?;
                let Some(existing) = self.blocks.get_element(&elem_id) else {
                    return Err(EditError::BlockNotFound);
                };
                let Some(block) = existing.value.as_ref() else {
                    return Err(EditError::BlockNotFound);
                };
                let mut updated = block.clone();
                updated.marks.remove_mark(interval_id, observed, op_id);
                self.blocks.update_value(elem_id, updated);
                self.mark_source_block_dirty(block_id);
                Ok(())
            }
        }
    }

    /// Set a mark on a block's text units (anchors are unit OpIds).
    #[allow(clippy::too_many_arguments)] // mirrors MarkSet::set_mark fields
    pub fn set_mark(
        &mut self,
        block_id: BlockId,
        interval_id: MarkIntervalId,
        kind: MarkKind,
        start: Anchor,
        end: Anchor,
        attrs: BTreeMap<String, MarkValue>,
        op_id: OpId,
    ) -> Result<Vec<EditOp>, EditError> {
        let op = EditOp::SetMark {
            block_id,
            interval_id,
            kind,
            start,
            end,
            attrs,
            op_id,
        };
        self.raw_apply_op(op.clone(), false)?;
        Ok(vec![op])
    }

    /// Remove (or range-split) a mark. Range split uses [`mark_ops::lower_remove_mark_range`].
    pub fn remove_mark(
        &mut self,
        block_id: BlockId,
        interval_id: MarkIntervalId,
        remove_id: OpId,
        observed: StateVector,
        remove_start: Anchor,
        remove_end: Anchor,
    ) -> Result<Vec<EditOp>, EditError> {
        let elem_id = self
            .block_elem_id(block_id)
            .ok_or(EditError::BlockNotFound)?;

        let Some(existing) = self.blocks.get_element(&elem_id) else {
            return Err(EditError::BlockNotFound);
        };
        let Some(block) = existing.value.as_ref() else {
            return Err(EditError::BlockNotFound);
        };

        if block.marks.interval(&interval_id).is_none() {
            return Err(EditError::InvalidOffset);
        }

        // Anchors are ordered by visible position, so pass the text body's element order.
        let element_order = block_text_seq(&block.kind)
            .map(paragraph_visible_ids)
            .unwrap_or_default();
        let (new_intervals, _removed) = mark_ops::lower_remove_mark_range(
            &block.marks,
            interval_id,
            remove_start,
            remove_end,
            OpId {
                counter: remove_id.counter.saturating_add(1),
                peer: remove_id.peer,
            },
            &element_order,
        );

        let mut updated = block.clone();
        let mut ops = Vec::new();
        updated
            .marks
            .remove_mark(interval_id, observed.clone(), remove_id);
        ops.push(EditOp::RemoveMark {
            block_id,
            interval_id,
            observed,
            op_id: remove_id,
        });

        for interval in new_intervals {
            let attrs: BTreeMap<String, MarkValue> = interval
                .attrs
                .iter()
                .map(|(k, reg)| (k.clone(), reg.get()))
                .collect();
            updated.marks.set_mark(
                interval.id,
                interval.kind.clone(),
                interval.start,
                interval.end,
                attrs.clone(),
                interval.op_id,
            );
            ops.push(EditOp::SetMark {
                block_id,
                interval_id: interval.id,
                kind: interval.kind,
                start: interval.start,
                end: interval.end,
                attrs,
                op_id: interval.op_id,
            });
        }

        self.blocks.update_value(elem_id, updated);
        self.mark_source_block_dirty(block_id);
        Ok(ops)
    }

    /// Render mark spans over a paragraph block using visible text-unit order.
    pub fn render_paragraph_spans(
        &self,
        block_id: BlockId,
    ) -> Result<Vec<crate::core::mark::Span>, EditError> {
        let block = self
            .find_block_by_id(block_id)
            .ok_or(EditError::BlockNotFound)?;
        let Some(text) = block_text_seq(&block.kind) else {
            return Err(EditError::InvalidOffset);
        };
        let order = paragraph_visible_ids(text);
        Ok(block.marks.render_spans(&order, order.len()))
    }

    /// Convert a non-empty half-open grapheme range to stable unit anchors.
    pub fn grapheme_range_to_anchors(
        &self,
        block_id: BlockId,
        range: std::ops::Range<usize>,
    ) -> Result<(Anchor, Anchor), EditError> {
        let block = self
            .find_block_by_id(block_id)
            .ok_or(EditError::BlockNotFound)?;
        let text = block_text_seq(&block.kind).ok_or(EditError::InvalidOffset)?;
        let ids = paragraph_visible_ids(text);
        if range.start >= range.end || range.end > ids.len() {
            return Err(EditError::InvalidOffset);
        }
        Ok((
            Anchor {
                elem_id: ids[range.start],
                bias: crate::core::mark::AnchorBias::Before,
            },
            Anchor {
                elem_id: ids[range.end - 1],
                bias: crate::core::mark::AnchorBias::After,
            },
        ))
    }

    /// Convert a UTF-8 byte range whose endpoints are grapheme boundaries to anchors.
    pub fn byte_range_to_anchors(
        &self,
        block_id: BlockId,
        range: std::ops::Range<usize>,
    ) -> Result<(Anchor, Anchor), EditError> {
        let block = self
            .find_block_by_id(block_id)
            .ok_or(EditError::BlockNotFound)?;
        let text = block_text_seq(&block.kind).ok_or(EditError::InvalidOffset)?;
        let visible = paragraph_visible_string(text);
        if range.start >= range.end || range.end > visible.len() {
            return Err(EditError::InvalidOffset);
        }
        if !is_grapheme_boundary(&visible, range.start)
            || !is_grapheme_boundary(&visible, range.end)
        {
            return Err(EditError::InvalidGraphemeBoundary);
        }
        let start = visible[..range.start].graphemes(true).count();
        let end = visible[..range.end].graphemes(true).count();
        self.grapheme_range_to_anchors(block_id, start..end)
    }

    pub fn frontmatter_field(&self, key: &str) -> Option<&str> {
        self.frontmatter.as_ref()?.get(key)
    }

    pub fn set_frontmatter_field(
        &mut self,
        key: String,
        value: Option<String>,
        op_id: OpId,
    ) -> Result<(), FrontmatterError> {
        self.frontmatter
            .get_or_insert_with(Frontmatter::empty)
            .set(key, value, op_id)
    }

    pub fn serialize(&self, mode: EquivalenceMode) -> String {
        let config = SerializeConfig {
            equivalence: mode,
            prefer_raw_source: true,
        };
        self.serialize_with_config(&config)
    }

    pub fn serialize_with_config(&self, config: &SerializeConfig) -> String {
        if let EquivalenceMode::Exact = config.equivalence
            && config.prefer_raw_source
            && let Some(source) = &self.source
        {
            let replacement = self
                .frontmatter
                .as_ref()
                .filter(|frontmatter| frontmatter.is_dirty())
                .map(Frontmatter::render);
            return source.render_with_frontmatter(&self.blocks, replacement.as_deref());
        }

        let mut output = String::new();
        if let Some(frontmatter) = &self.frontmatter {
            output.push_str("---\n");
            output.push_str(&frontmatter.render());
            output.push_str("\n---\n\n");
        }

        let blocks = self.blocks_in_order();
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 {
                output.push_str("\n\n");
            }
            output.push_str(&serialize_block(block));
        }

        match config.equivalence {
            EquivalenceMode::Exact => output,
            EquivalenceMode::Structural => normalize_structural(&output),
        }
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Block {
    pub fn new(kind: BlockKind, insert_id: OpId) -> Self {
        Self {
            id: block_id_from_op(insert_id),
            elem_id: insert_id,
            kind_op: insert_id,
            kind_observed: StateVector::new(),
            kind,
            marks: MarkSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mark::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
    use std::collections::BTreeMap;

    #[test]
    fn default_serialization_and_table_metadata_updates_are_observable() {
        assert_eq!(SerializeConfig::default(), SerializeConfig::exact());

        let created = OpId {
            counter: 1,
            peer: 1,
        };
        let updated = OpId {
            counter: 2,
            peer: 1,
        };
        let mut table = Table::new(block_id_from_op(created), created, created);
        table.insert_column(None, ColumnAlignment::Center, "title".into(), updated);

        assert_eq!(
            table.columns_in_order()[0].alignment.get(),
            ColumnAlignment::Center
        );
        assert_eq!(table.row_cells(table.header_row_id()), vec!["title"]);
    }

    #[test]
    fn test_block_ids_and_elem_ids() {
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let other = OpId {
            counter: 2,
            peer: 1,
        };
        let block_a = Block::new(BlockKind::paragraph("A", id), id);
        let block_b = Block::new(BlockKind::paragraph("B", id), id);
        let block_c = Block::new(BlockKind::paragraph("C", other), other);
        // Same create OpId → same BlockId (deterministic).
        assert_eq!(block_a.id, block_b.id);
        assert_eq!(block_a.id, block_id_from_op(id));
        assert_ne!(block_a.id, block_c.id);
        assert_eq!(block_a.elem_id, id);
        assert_eq!(block_c.elem_id, other);
    }

    #[test]
    fn test_blockquote_hierarchy() {
        let child = Block::new(
            BlockKind::paragraph(
                "Nested",
                OpId {
                    counter: 1,
                    peer: 0,
                },
            ),
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let mut children = Sequence::new();
        children.apply_op((child.elem_id, child.clone()));
        let quote = Block::new(
            BlockKind::BlockQuote { children },
            OpId {
                counter: 2,
                peer: 0,
            },
        );
        if let BlockKind::BlockQuote { children } = &quote.kind {
            let collected: Vec<_> = children.iter().collect();
            assert_eq!(collected.len(), 1);
            assert_eq!(collected[0].kind, child.kind);
        } else {
            panic!("Expected blockquote block");
        }
    }

    #[test]
    fn test_exact_equivalence_roundtrip() {
        let input = "---\ntitle: Test\n---\n\nHello\n\n```rust\nfn main() {}\n```\n\n:::custom\nraw block\n";
        let doc = Parser::parse(input);
        let output = doc.serialize(EquivalenceMode::Exact);
        assert_eq!(output, input);
    }

    #[test]
    fn test_structural_equivalence() {
        let a = "Hello\n\nWorld\n";
        let b = "Hello\n\n\nWorld";
        let doc_a = Parser::parse(a);
        let doc_b = Parser::parse(b);
        let norm_a = doc_a.serialize(EquivalenceMode::Structural);
        let norm_b = doc_b.serialize(EquivalenceMode::Structural);
        assert_eq!(norm_a, norm_b);
    }

    #[test]
    fn test_raw_block_preservation() {
        let input = ":::custom\nraw line\n\nNext";
        let doc = Parser::parse(input);
        let output = doc.serialize(EquivalenceMode::Structural);
        assert!(output.contains(":::custom\nraw line"));
    }

    #[test]
    fn test_insert_text_grapheme_offsets() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::paragraph(
                "a🇺🇸b",
                OpId {
                    counter: 1,
                    peer: 0,
                },
            ),
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        let ops = doc
            .insert_text(
                block_id,
                1,
                "X",
                OpId {
                    counter: 100, // must not collide with parse unit ids (1..)
                    peer: 0,
                },
            )
            .unwrap();
        match &ops[0] {
            EditOp::InsertText(run) => {
                assert_eq!(run.grapheme_offset, 1);
                assert_eq!(run.text, "X");
                assert!(run.byte_offset > 0);
            }
            _ => panic!("Expected insert op"),
        }

        let updated = doc.blocks.iter().next().unwrap();
        if let BlockKind::Paragraph { text } = &updated.kind {
            assert_eq!(paragraph_visible_string(text), "aX🇺🇸b");
        } else {
            panic!("Expected paragraph block");
        }
    }

    #[test]
    fn test_remove_mark_split() {
        let mut doc = Document::new();
        let mut block = Block::new(
            BlockKind::paragraph(
                "Hello",
                OpId {
                    counter: 1,
                    peer: 0,
                },
            ),
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        // Units for "Hello" are counters 1..5 peer 0.
        let mark_id = OpId {
            counter: 10,
            peer: 0,
        };
        block.marks.set_mark(
            mark_id,
            MarkKind::Bold,
            Anchor {
                elem_id: OpId {
                    counter: 1,
                    peer: 0,
                },
                bias: AnchorBias::Before,
            },
            Anchor {
                elem_id: OpId {
                    counter: 5,
                    peer: 0,
                },
                bias: AnchorBias::After,
            },
            BTreeMap::new(),
            mark_id,
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        let ops = doc
            .remove_mark(
                block_id,
                mark_id,
                OpId {
                    counter: 20,
                    peer: 0,
                },
                StateVector::new(),
                Anchor {
                    elem_id: OpId {
                        counter: 2,
                        peer: 0,
                    },
                    bias: AnchorBias::Before,
                },
                Anchor {
                    elem_id: OpId {
                        counter: 4,
                        peer: 0,
                    },
                    bias: AnchorBias::After,
                },
            )
            .unwrap();

        let set_ops = ops
            .iter()
            .filter(|op| matches!(op, EditOp::SetMark { .. }))
            .count();
        assert_eq!(set_ops, 2, "Expected split into two SetMark ops");
        assert!(matches!(ops[0], EditOp::RemoveMark { .. }));
    }

    #[test]
    fn test_render_spans_over_paragraph_units() {
        let mut doc = Document::new();
        let mut block = Block::new(
            BlockKind::paragraph(
                "abc",
                OpId {
                    counter: 1,
                    peer: 0,
                },
            ),
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let mark_id = OpId {
            counter: 50,
            peer: 0,
        };
        // Bold on middle unit "b" (counter 2).
        block.marks.set_mark(
            mark_id,
            MarkKind::Bold,
            Anchor {
                elem_id: OpId {
                    counter: 2,
                    peer: 0,
                },
                bias: AnchorBias::Before,
            },
            Anchor {
                elem_id: OpId {
                    counter: 2,
                    peer: 0,
                },
                bias: AnchorBias::After,
            },
            BTreeMap::new(),
            mark_id,
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        let spans = doc.render_paragraph_spans(block_id).unwrap();
        assert_eq!(spans.len(), 3);
        assert!(spans[0].marks.is_empty());
        assert_eq!(spans[1].marks, vec![mark_id]);
        assert!(spans[2].marks.is_empty());
    }

    #[test]
    fn test_raw_apply_op_grapheme_validation() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::paragraph(
                "a🇺🇸b",
                OpId {
                    counter: 1,
                    peer: 0,
                },
            ),
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        let mut chars = "a🇺🇸b".char_indices();
        chars.next();
        let _first_flag = chars.next().unwrap().0;
        let bad_offset = chars.next().unwrap().0;
        let op = EditOp::InsertText(InsertTextRun {
            block_id,
            grapheme_offset: 0,
            byte_offset: bad_offset,
            text: "X".into(),
            op_id: OpId {
                counter: 2,
                peer: 0,
            },
        });

        assert_eq!(
            doc.raw_apply_op(op.clone(), true),
            Err(EditError::InvalidGraphemeBoundary)
        );
        assert!(doc.raw_apply_op(op, false).is_ok());
    }

    #[test]
    fn test_insert_text_run_mark_expansion() {
        let mut set = MarkSet::new();
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let start = Anchor {
            elem_id: OpId {
                counter: 1,
                peer: 1,
            },
            bias: AnchorBias::Before,
        };
        let end = Anchor {
            elem_id: OpId {
                counter: 2,
                peer: 1,
            },
            bias: AnchorBias::After,
        };
        let mut attrs = BTreeMap::new();
        attrs.insert("k".to_string(), MarkValue::String("v".into()));
        set.set_mark(id, MarkKind::Bold, start, end, attrs, id);

        let order = vec![
            OpId {
                counter: 1,
                peer: 1,
            },
            OpId {
                counter: 2,
                peer: 1,
            },
        ];
        let marks = mark_ops::expand_marks_for_insert(
            &set,
            &order,
            2,
            Anchor {
                elem_id: OpId {
                    counter: 1,
                    peer: 1,
                },
                bias: AnchorBias::After,
            },
            true,
        );
        assert_eq!(marks, vec![id]);
    }

    #[test]
    fn test_insert_text_run_no_expand() {
        let set = MarkSet::new();
        let order = vec![OpId {
            counter: 1,
            peer: 1,
        }];
        let marks = mark_ops::expand_marks_for_insert(
            &set,
            &order,
            1,
            Anchor {
                elem_id: OpId {
                    counter: 1,
                    peer: 1,
                },
                bias: AnchorBias::Before,
            },
            false,
        );
        assert!(marks.is_empty());
    }

    #[test]
    fn test_remove_mark_range_splits_interval() {
        let mut set = MarkSet::new();
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let start = Anchor {
            elem_id: OpId {
                counter: 1,
                peer: 1,
            },
            bias: AnchorBias::Before,
        };
        let end = Anchor {
            elem_id: OpId {
                counter: 3,
                peer: 1,
            },
            bias: AnchorBias::After,
        };
        set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

        let (new_intervals, removed) = mark_ops::lower_remove_mark_range(
            &set,
            id,
            Anchor {
                elem_id: OpId {
                    counter: 2,
                    peer: 1,
                },
                bias: AnchorBias::Before,
            },
            Anchor {
                elem_id: OpId {
                    counter: 2,
                    peer: 1,
                },
                bias: AnchorBias::After,
            },
            OpId {
                counter: 10,
                peer: 1,
            },
            &[
                OpId {
                    counter: 1,
                    peer: 1,
                },
                OpId {
                    counter: 2,
                    peer: 1,
                },
                OpId {
                    counter: 3,
                    peer: 1,
                },
            ],
        );
        assert_eq!(removed, vec![id]);
        assert_eq!(new_intervals.len(), 2);
    }

    #[test]
    fn test_remove_mark_range_full() {
        let mut set = MarkSet::new();
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let start = Anchor {
            elem_id: OpId {
                counter: 1,
                peer: 1,
            },
            bias: AnchorBias::Before,
        };
        let end = Anchor {
            elem_id: OpId {
                counter: 2,
                peer: 1,
            },
            bias: AnchorBias::After,
        };
        set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

        let (new_intervals, removed) = mark_ops::lower_remove_mark_range(
            &set,
            id,
            start,
            end,
            OpId {
                counter: 10,
                peer: 1,
            },
            &[
                OpId {
                    counter: 1,
                    peer: 1,
                },
                OpId {
                    counter: 2,
                    peer: 1,
                },
            ],
        );
        assert_eq!(removed, vec![id]);
        assert!(new_intervals.is_empty());
    }

    #[test]
    fn remove_mark_range_orders_by_visible_position_not_opid() {
        // Visible order [B, A] but OpId order [A, B] (B has the higher counter) —
        // exactly what RGA can produce for concurrently-inserted units. The range
        // split must use visible position, not raw OpId order.
        let a = OpId {
            counter: 3,
            peer: 1,
        };
        let b = OpId {
            counter: 5,
            peer: 2,
        };
        let element_order = [b, a]; // B is first visually, then A
        let mut set = MarkSet::new();
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        // Mark spans the whole paragraph: B(before) .. A(after).
        let start = Anchor {
            elem_id: b,
            bias: AnchorBias::Before,
        };
        let end = Anchor {
            elem_id: a,
            bias: AnchorBias::After,
        };
        set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

        // Remove the mark from the first *visible* element (B).
        let (new_intervals, removed) = mark_ops::lower_remove_mark_range(
            &set,
            id,
            Anchor {
                elem_id: b,
                bias: AnchorBias::Before,
            },
            Anchor {
                elem_id: b,
                bias: AnchorBias::After,
            },
            OpId {
                counter: 10,
                peer: 1,
            },
            &element_order,
        );
        assert_eq!(removed, vec![id]);
        // Keep a right remnant over A (positions 1..2); no left remnant (B is first).
        // Raw-OpId ordering would compare B{5,2} > A{3,1} and wrongly drop this remnant.
        assert_eq!(new_intervals.len(), 1, "must keep the right remnant over A");
        assert_eq!(
            new_intervals[0].start,
            Anchor {
                elem_id: b,
                bias: AnchorBias::After
            }
        );
        assert_eq!(new_intervals[0].end, end);
    }
}
