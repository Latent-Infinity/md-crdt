//! Session snapshot schema and Document ↔ DTO conversion.
//!
//! Snapshots persist document materialization plus the opaque op log for
//! crash recovery, checkpoint rebase, and late join.

use crate::core::mark::MarkSet;
use crate::core::{Element, LwwRegister, OpId, PeerId, Sequence, SequenceOp};
use crate::doc::{
    Block, BlockId, BlockKind, CellAddress, CellContent, CodeFenceStyle, ColumnAlignment, ColumnId,
    Document, DocumentSource, Frontmatter, ListStyle, PendingColumnAlignment, PendingListItemMove,
    PendingTableMove, RowId, Table, TableCell, TableColumn, TableRow, TaskState, TextUnit,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Snapshot schema version (not wire `Envelope` version).
///
/// v7: snapshots use a bounded compressed envelope around the versioned JSON payload.
pub const SNAPSHOT_FORMAT_VERSION: u16 = 7;

const SNAPSHOT_MAGIC: &[u8; 5] = b"MDSN\x01";
const SNAPSHOT_HEADER_LEN: usize = SNAPSHOT_MAGIC.len() + size_of::<u64>();
const MAX_SNAPSHOT_DECOMPRESSED_BYTES: usize = 512 * 1024 * 1024;

/// Deflate level used for persisted snapshots.
///
/// A level 1/3/6/9 ablation on representative 123 KiB vault snapshots selected
/// level 3 as the balance between full-snapshot tail latency and persisted size.
/// Readers remain level-agnostic because the envelope uses ordinary zlib.
const SNAPSHOT_DEFLATE_LEVEL: u8 = 3;

/// Errors loading or decoding session snapshots.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("serde: {0}")]
    Serde(String),
    #[error("snapshot encoding is corrupt: {0}")]
    CorruptEncoding(&'static str),
    #[error("snapshot expands to {size} bytes, exceeding the {max}-byte safety limit")]
    SnapshotTooLarge { size: u64, max: usize },
    #[error(
        "snapshot format version {found} is unsupported; expected {expected}; reinitialize and re-ingest from Markdown"
    )]
    ReinitializeRequired { found: u16, expected: u16 },
    #[error("next_counter {next} is behind max observed counter {max} for peer {peer}")]
    ClockBehind { peer: PeerId, next: u64, max: u64 },
    #[cfg(feature = "storage")]
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
}

/// Durable session state for recovery and late join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub format_version: u16,
    pub peer: PeerId,
    pub next_counter: u64,
    pub unit_mode: bool,
    /// Applied frontier, including counters whose operation payloads were checkpointed away.
    pub state_vector: crate::core::StateVector,
    pub checkpoint_epoch: u64,
    pub delta_floor: crate::core::StateVector,
    pub document: DocumentDto,
    /// Applied ops `(OpId, payload bytes)` for retransmission / audit.
    pub ops: Vec<(OpId, Vec<u8>)>,
    /// Causally buffered ops not yet in the applied log.
    pub pending: Vec<(OpId, Vec<u8>)>,
    /// Applied operations waiting for an observed cross-peer frontier.
    pub deferred: Vec<(OpId, Vec<u8>)>,
}

/// Serializable document: ordered sequence elements (incl. tombstones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDto {
    pub frontmatter: Option<Frontmatter>,
    pub blocks: SequenceDto<BlockDto>,
    pub(crate) source: Option<DocumentSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementDto<T> {
    pub id: OpId,
    pub value: Option<T>,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceDto<T> {
    pub elements: Vec<ElementDto<T>>,
    pub pending: Vec<SequenceOpDto<T>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceOpDto<T> {
    Insert {
        after: Option<OpId>,
        id: OpId,
        value: T,
        right_origin: Option<OpId>,
    },
    Delete {
        target: OpId,
        id: OpId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub kind_op: OpId,
    pub kind_observed: crate::core::StateVector,
    pub kind: BlockKindDto,
    pub marks: MarkSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextUnitDto {
    pub grapheme: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKindDto {
    Paragraph {
        units: SequenceDto<TextUnitDto>,
    },
    Heading {
        level: u8,
        units: SequenceDto<TextUnitDto>,
    },
    List {
        style: ListStyle,
        items: SequenceDto<ListItemDto>,
        pending_moves: Vec<PendingListItemMove>,
    },
    CodeFence {
        style: CodeFenceStyle,
        info: Option<String>,
        text: String,
    },
    BlockQuote {
        children: SequenceDto<BlockDto>,
    },
    RawBlock {
        raw: String,
    },
    Table {
        table: TableDto,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItemDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub task: Option<TaskState>,
    pub task_op: OpId,
    pub task_observed: crate::core::StateVector,
    pub placement_observed: crate::core::StateVector,
    pub children: SequenceDto<BlockDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub deleted: LwwDto<bool>,
    pub columns: SequenceDto<TableColumnDto>,
    pub rows: SequenceDto<TableRowDto>,
    pub cells: Vec<TableCellDto>,
    pub pending_row_moves: Vec<PendingTableMove>,
    pub pending_column_moves: Vec<PendingTableMove>,
    pub pending_column_alignments: Vec<PendingColumnAlignment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableColumnDto {
    pub id: ColumnId,
    pub elem_id: OpId,
    pub deleted: LwwDto<bool>,
    pub alignment: LwwDto<ColumnAlignment>,
    pub alignment_observed: crate::core::StateVector,
    pub placement_observed: crate::core::StateVector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRowDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub deleted: LwwDto<bool>,
    pub placement_observed: crate::core::StateVector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCellDto {
    pub row_id: RowId,
    pub column_id: ColumnId,
    pub value: LwwDto<CellContent>,
    pub observed: crate::core::StateVector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwDto<T> {
    pub value: T,
    pub op_id: OpId,
}

impl DocumentDto {
    pub fn from_document(doc: &Document) -> Self {
        Self {
            frontmatter: doc.frontmatter.clone(),
            blocks: sequence_to_dto(doc.blocks(), block_to_dto),
            source: doc.source_state(),
        }
    }

    pub fn into_document(self) -> Document {
        let mut doc = Document::new();
        doc.frontmatter = self.frontmatter;
        *doc.blocks_mut() = sequence_from_dto(self.blocks, block_from_dto);
        doc.set_source_state(self.source);
        doc
    }
}

impl SessionSnapshot {
    pub fn to_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        let json = serde_json::to_vec(self).map_err(|e| SnapshotError::Serde(e.to_string()))?;
        if json.len() > MAX_SNAPSHOT_DECOMPRESSED_BYTES {
            return Err(SnapshotError::SnapshotTooLarge {
                size: json.len() as u64,
                max: MAX_SNAPSHOT_DECOMPRESSED_BYTES,
            });
        }

        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&json, SNAPSHOT_DEFLATE_LEVEL);
        let mut bytes = Vec::with_capacity(SNAPSHOT_HEADER_LEN + compressed.len());
        bytes.extend_from_slice(SNAPSHOT_MAGIC);
        bytes.extend_from_slice(&(json.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&compressed);
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let json;
        let payload = if bytes.starts_with(SNAPSHOT_MAGIC) {
            if bytes.len() < SNAPSHOT_HEADER_LEN {
                return Err(SnapshotError::CorruptEncoding("truncated header"));
            }
            let declared_len = u64::from_le_bytes(
                bytes[SNAPSHOT_MAGIC.len()..SNAPSHOT_HEADER_LEN]
                    .try_into()
                    .expect("snapshot header length was checked"),
            );
            if declared_len > MAX_SNAPSHOT_DECOMPRESSED_BYTES as u64 {
                return Err(SnapshotError::SnapshotTooLarge {
                    size: declared_len,
                    max: MAX_SNAPSHOT_DECOMPRESSED_BYTES,
                });
            }
            json = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
                &bytes[SNAPSHOT_HEADER_LEN..],
                MAX_SNAPSHOT_DECOMPRESSED_BYTES,
            )
            .map_err(|_| SnapshotError::CorruptEncoding("invalid compressed payload"))?;
            if json.len() as u64 != declared_len {
                return Err(SnapshotError::CorruptEncoding(
                    "decompressed length does not match header",
                ));
            }
            json.as_slice()
        } else {
            bytes
        };

        let snap: Self =
            serde_json::from_slice(payload).map_err(|e| SnapshotError::Serde(e.to_string()))?;
        if snap.format_version != SNAPSHOT_FORMAT_VERSION {
            return Err(SnapshotError::ReinitializeRequired {
                found: snap.format_version,
                expected: SNAPSHOT_FORMAT_VERSION,
            });
        }
        Ok(snap)
    }
}

/// Max counter for `peer` across applied ops and document element ids.
pub fn max_counter_for_peer(peer: PeerId, ops: &[(OpId, Vec<u8>)], doc: &Document) -> u64 {
    let mut max = 0u64;
    for (id, _) in ops {
        if id.peer == peer {
            max = max.max(id.counter);
        }
    }
    walk_block_seq_max_peer(peer, doc.blocks(), &mut max);
    max
}

fn walk_block_seq_max_peer(peer: PeerId, seq: &Sequence<Block>, max: &mut u64) {
    for elem in seq.iter_all() {
        if elem.id.peer == peer {
            *max = (*max).max(elem.id.counter);
        }
        if let Some(block) = elem.value.as_ref() {
            if block.kind_op.peer == peer {
                *max = (*max).max(block.kind_op.counter);
            }
            walk_marks_max_peer(peer, &block.marks, max);
            walk_kind_max_peer(peer, &block.kind, max);
        }
    }
}

fn walk_kind_max_peer(peer: PeerId, kind: &BlockKind, max: &mut u64) {
    match kind {
        BlockKind::BlockQuote { children } => walk_block_seq_max_peer(peer, children, max),
        BlockKind::List {
            items,
            pending_moves,
            ..
        } => {
            for element in items.iter_all() {
                if element.id.peer == peer {
                    *max = (*max).max(element.id.counter);
                }
                if let Some(item) = element.value.as_ref() {
                    if item.task_op.peer == peer {
                        *max = (*max).max(item.task_op.counter);
                    }
                    walk_block_seq_max_peer(peer, &item.children, max);
                }
            }
            for movement in pending_moves {
                if movement.id.peer == peer {
                    *max = (*max).max(movement.id.counter);
                }
            }
        }
        BlockKind::Table { table } => {
            if table.elem_id.peer == peer {
                *max = (*max).max(table.elem_id.counter);
            }
            for elem in table.columns.iter_all() {
                if elem.id.peer == peer {
                    *max = (*max).max(elem.id.counter);
                }
                if let Some(column) = elem.value.as_ref() {
                    for id in [column.deleted.op_id(), column.alignment.op_id()] {
                        if id.peer == peer {
                            *max = (*max).max(id.counter);
                        }
                    }
                }
            }
            for elem in table.rows.iter_all() {
                if elem.id.peer == peer {
                    *max = (*max).max(elem.id.counter);
                }
                if let Some(row) = elem.value.as_ref() {
                    let id = row.deleted.op_id();
                    if id.peer == peer {
                        *max = (*max).max(id.counter);
                    }
                }
            }
            for cell in table.cells.values() {
                let id = cell.op_id;
                if id.peer == peer {
                    *max = (*max).max(id.counter);
                }
            }
            for movement in table
                .pending_row_moves
                .iter()
                .chain(&table.pending_column_moves)
            {
                if movement.id.peer == peer {
                    *max = (*max).max(movement.id.counter);
                }
            }
            for alignment in &table.pending_column_alignments {
                if alignment.id.peer == peer {
                    *max = (*max).max(alignment.id.counter);
                }
            }
        }
        _ => {}
    }
}

fn walk_marks_max_peer(peer: PeerId, marks: &MarkSet, max: &mut u64) {
    for interval in marks.iter_all_intervals() {
        for id in [interval.id, interval.op_id] {
            if id.peer == peer {
                *max = (*max).max(id.counter);
            }
        }
        for register in interval.attrs.values() {
            let id = register.op_id();
            if id.peer == peer {
                *max = (*max).max(id.counter);
            }
        }
    }
    for (_, remove) in marks.iter_removes() {
        if remove.op_id.peer == peer {
            *max = (*max).max(remove.op_id.counter);
        }
    }
}

fn sequence_to_dto<T, U, F>(seq: &Sequence<T>, map: F) -> SequenceDto<U>
where
    T: Clone,
    F: Fn(&T) -> U + Copy,
{
    SequenceDto {
        elements: seq
            .iter_all()
            .map(|elem| ElementDto {
                id: elem.id,
                value: elem.value.as_ref().map(map),
                after: elem.after,
                right_origin: elem.right_origin,
            })
            .collect(),
        pending: seq
            .pending_ops()
            .into_iter()
            .map(|operation| match operation {
                SequenceOp::Insert {
                    after,
                    id,
                    value,
                    right_origin,
                } => SequenceOpDto::Insert {
                    after,
                    id,
                    value: map(&value),
                    right_origin,
                },
                SequenceOp::Delete { target, id } => SequenceOpDto::Delete { target, id },
            })
            .collect(),
    }
}

fn sequence_from_dto<T, U, F>(dto: SequenceDto<U>, map: F) -> Sequence<T>
where
    T: Clone,
    F: Fn(U) -> T + Copy,
{
    let elements = dto
        .elements
        .into_iter()
        .map(|e| Element {
            id: e.id,
            value: e.value.map(map),
            after: e.after,
            right_origin: e.right_origin,
        })
        .collect();
    let pending = dto
        .pending
        .into_iter()
        .map(|operation| match operation {
            SequenceOpDto::Insert {
                after,
                id,
                value,
                right_origin,
            } => SequenceOp::Insert {
                after,
                id,
                value: map(value),
                right_origin,
            },
            SequenceOpDto::Delete { target, id } => SequenceOp::Delete { target, id },
        })
        .collect();
    Sequence::from_elements_and_pending(elements, pending)
}

fn block_to_dto(block: &Block) -> BlockDto {
    BlockDto {
        id: block.id,
        elem_id: block.elem_id,
        kind_op: block.kind_op,
        kind_observed: block.kind_observed.clone(),
        kind: kind_to_dto(&block.kind),
        marks: block.marks.clone(),
    }
}

fn block_from_dto(dto: BlockDto) -> Block {
    Block {
        id: dto.id,
        elem_id: dto.elem_id,
        kind_op: dto.kind_op,
        kind_observed: dto.kind_observed,
        kind: kind_from_dto(dto.kind),
        marks: dto.marks,
    }
}

fn kind_to_dto(kind: &BlockKind) -> BlockKindDto {
    match kind {
        BlockKind::Paragraph { text } => BlockKindDto::Paragraph {
            units: sequence_to_dto(text, |u| TextUnitDto {
                grapheme: u.grapheme.clone(),
            }),
        },
        BlockKind::Heading { level, text } => BlockKindDto::Heading {
            level: *level,
            units: sequence_to_dto(text, |u| TextUnitDto {
                grapheme: u.grapheme.clone(),
            }),
        },
        BlockKind::List {
            style,
            items,
            pending_moves,
        } => BlockKindDto::List {
            style: *style,
            items: sequence_to_dto(items, list_item_to_dto),
            pending_moves: pending_moves.clone(),
        },
        BlockKind::CodeFence { style, info, text } => BlockKindDto::CodeFence {
            style: *style,
            info: info.clone(),
            text: text.clone(),
        },
        BlockKind::RawBlock { raw } => BlockKindDto::RawBlock { raw: raw.clone() },
        BlockKind::BlockQuote { children } => BlockKindDto::BlockQuote {
            children: sequence_to_dto(children, block_to_dto),
        },
        BlockKind::Table { table } => BlockKindDto::Table {
            table: table_to_dto(table),
        },
    }
}

fn list_item_to_dto(item: &crate::doc::ListItem) -> ListItemDto {
    ListItemDto {
        id: item.id,
        elem_id: item.elem_id,
        task: item.task,
        task_op: item.task_op,
        task_observed: item.task_observed.clone(),
        placement_observed: item.placement_observed.clone(),
        children: sequence_to_dto(&item.children, block_to_dto),
    }
}

fn kind_from_dto(kind: BlockKindDto) -> BlockKind {
    match kind {
        BlockKindDto::Paragraph { units } => BlockKind::Paragraph {
            text: sequence_from_dto(units, |u| TextUnit {
                grapheme: u.grapheme,
            }),
        },
        BlockKindDto::Heading { level, units } => BlockKind::Heading {
            level,
            text: sequence_from_dto(units, |u| TextUnit {
                grapheme: u.grapheme,
            }),
        },
        BlockKindDto::List {
            style,
            items,
            pending_moves,
        } => BlockKind::List {
            style,
            items: sequence_from_dto(items, list_item_from_dto),
            pending_moves,
        },
        BlockKindDto::CodeFence { style, info, text } => BlockKind::CodeFence { style, info, text },
        BlockKindDto::RawBlock { raw } => BlockKind::RawBlock { raw },
        BlockKindDto::BlockQuote { children } => BlockKind::BlockQuote {
            children: sequence_from_dto(children, block_from_dto),
        },
        BlockKindDto::Table { table } => BlockKind::Table {
            table: Box::new(table_from_dto(table)),
        },
    }
}

fn list_item_from_dto(dto: ListItemDto) -> crate::doc::ListItem {
    crate::doc::ListItem {
        id: dto.id,
        elem_id: dto.elem_id,
        task: dto.task,
        task_op: dto.task_op,
        task_observed: dto.task_observed,
        placement_observed: dto.placement_observed,
        children: sequence_from_dto(dto.children, block_from_dto),
    }
}

fn table_to_dto(table: &Table) -> TableDto {
    TableDto {
        id: table.id,
        elem_id: table.elem_id,
        deleted: LwwDto {
            value: table.deleted.get(),
            op_id: table.deleted.op_id(),
        },
        columns: sequence_to_dto(&table.columns, column_to_dto),
        rows: sequence_to_dto(&table.rows, row_to_dto),
        cells: table
            .cells
            .iter()
            .map(|(address, value)| TableCellDto {
                row_id: address.row_id,
                column_id: address.column_id,
                value: LwwDto {
                    value: value.value.clone(),
                    op_id: value.op_id,
                },
                observed: value.observed.clone(),
            })
            .collect(),
        pending_row_moves: table.pending_row_moves.clone(),
        pending_column_moves: table.pending_column_moves.clone(),
        pending_column_alignments: table.pending_column_alignments.clone(),
    }
}

fn table_from_dto(dto: TableDto) -> Table {
    Table {
        id: dto.id,
        elem_id: dto.elem_id,
        deleted: LwwRegister::new(dto.deleted.value, dto.deleted.op_id),
        columns: sequence_from_dto(dto.columns, column_from_dto),
        rows: sequence_from_dto(dto.rows, row_from_dto),
        cells: dto
            .cells
            .into_iter()
            .map(|cell| {
                (
                    CellAddress {
                        row_id: cell.row_id,
                        column_id: cell.column_id,
                    },
                    TableCell::new(cell.value.value, cell.value.op_id, cell.observed),
                )
            })
            .collect(),
        pending_row_moves: dto.pending_row_moves,
        pending_column_moves: dto.pending_column_moves,
        pending_column_alignments: dto.pending_column_alignments,
    }
}

fn column_to_dto(column: &TableColumn) -> TableColumnDto {
    TableColumnDto {
        id: column.id,
        elem_id: column.elem_id,
        deleted: LwwDto {
            value: column.deleted.get(),
            op_id: column.deleted.op_id(),
        },
        alignment: LwwDto {
            value: column.alignment.get(),
            op_id: column.alignment.op_id(),
        },
        alignment_observed: column.alignment_observed.clone(),
        placement_observed: column.placement_observed.clone(),
    }
}

fn column_from_dto(dto: TableColumnDto) -> TableColumn {
    TableColumn {
        id: dto.id,
        elem_id: dto.elem_id,
        deleted: LwwRegister::new(dto.deleted.value, dto.deleted.op_id),
        alignment: LwwRegister::new(dto.alignment.value, dto.alignment.op_id),
        alignment_observed: dto.alignment_observed,
        placement_observed: dto.placement_observed,
    }
}

fn row_to_dto(row: &TableRow) -> TableRowDto {
    TableRowDto {
        id: row.id,
        elem_id: row.elem_id,
        deleted: LwwDto {
            value: row.deleted.get(),
            op_id: row.deleted.op_id(),
        },
        placement_observed: row.placement_observed.clone(),
    }
}

fn row_from_dto(dto: TableRowDto) -> TableRow {
    TableRow {
        id: dto.id,
        elem_id: dto.elem_id,
        deleted: LwwRegister::new(dto.deleted.value, dto.deleted.op_id),
        placement_observed: dto.placement_observed,
    }
}

fn count_units_in_blocks(blocks: &SequenceDto<BlockDto>) -> usize {
    blocks
        .elements
        .iter()
        .filter_map(|elem| elem.value.as_ref())
        .map(|block| match &block.kind {
            BlockKindDto::Paragraph { units } | BlockKindDto::Heading { units, .. } => {
                units.elements.len()
            }
            BlockKindDto::List { items, .. } => items
                .elements
                .iter()
                .filter_map(|item| item.value.as_ref())
                .map(|item| count_units_in_blocks(&item.children))
                .sum(),
            BlockKindDto::BlockQuote { children } => count_units_in_blocks(children),
            BlockKindDto::CodeFence { text, .. } => text.chars().count().max(1),
            BlockKindDto::RawBlock { .. } | BlockKindDto::Table { .. } => 0,
        })
        .sum()
}

/// Byte-level breakdown of a session snapshot (feeds size attribution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSizeBreakdown {
    pub source_chars: usize,
    pub unit_count: usize,
    pub op_count: usize,
    pub json_total: usize,
    pub document_json: usize,
    pub ops_json: usize,
    pub ops_payload_bytes: usize,
    pub compressed_envelope: usize,
    pub header_overhead: usize,
}

impl SessionSnapshot {
    /// Attribute serialized snapshot cost without changing on-disk format.
    #[must_use]
    pub fn size_breakdown(&self, source_chars: usize) -> SnapshotSizeBreakdown {
        let json_total = serde_json::to_vec(self).map(|v| v.len()).unwrap_or(0);
        let document_json = serde_json::to_vec(&self.document)
            .map(|v| v.len())
            .unwrap_or(0);
        let ops_json = serde_json::to_vec(&self.ops).map(|v| v.len()).unwrap_or(0);
        let ops_payload_bytes: usize = self.ops.iter().map(|(_, p)| p.len()).sum();
        let unit_count = count_units_in_blocks(&self.document.blocks);
        let compressed_envelope = self.to_bytes().map(|b| b.len()).unwrap_or(0);
        SnapshotSizeBreakdown {
            source_chars,
            unit_count,
            op_count: self.ops.len(),
            json_total,
            document_json,
            ops_json,
            ops_payload_bytes,
            compressed_envelope,
            header_overhead: SNAPSHOT_HEADER_LEN,
        }
    }
}

#[cfg(test)]
mod compression_tests {
    use crate::session::CollaborativeDocument;
    use crate::session::snapshot::{SNAPSHOT_DEFLATE_LEVEL, SessionSnapshot};

    /// The deflate level is chosen by ablation, so it must stay a tuning knob
    /// and never a correctness one: whatever it is set to, a snapshot has to
    /// survive the round trip byte for byte.
    #[test]
    fn a_snapshot_round_trips_at_the_configured_deflate_level() {
        let mut session = CollaborativeDocument::new(1);
        session
            .insert_paragraph(None, &"lorem ipsum ".repeat(200))
            .expect("seed paragraph");
        let original = session.save_snapshot().expect("snapshot");

        let bytes = original.to_bytes().expect("encode");
        let restored = SessionSnapshot::from_bytes(&bytes).expect("decode");

        assert_eq!(
            original, restored,
            "level {SNAPSHOT_DEFLATE_LEVEL} must be lossless"
        );
    }
}

#[cfg(test)]
mod size_attribution_tests {
    use crate::session::CollaborativeDocument;

    #[test]
    fn snapshot_size_is_dominated_by_document_and_ops_json() {
        let source = "x".repeat(2_000);
        let mut session = CollaborativeDocument::new(1);
        session
            .insert_paragraph(None, &source)
            .expect("seed paragraph");
        let snap = session.save_snapshot().expect("snapshot");
        let b = snap.size_breakdown(source.len());

        // ~one unit per source grapheme in a single paragraph.
        assert_eq!(b.unit_count, source.len());
        assert!(b.json_total > source.len().saturating_mul(10));
        // Document materialization and retained op log are both first-class costs.
        assert!(b.document_json > source.len());
        assert!(b.ops_json > 0);
        assert!(b.ops_payload_bytes > 0);
        // Compressed envelope is smaller than raw JSON but still multiplies source.
        assert!(b.compressed_envelope < b.json_total);
        assert!(b.compressed_envelope > source.len());
        // Document + ops account for most of the JSON blob.
        let accounted = b.document_json + b.ops_json;
        assert!(accounted as f64 / b.json_total as f64 > 0.85);

        // Bytes per source char for plan-state reporting (indicative, debug build).
        let ratio = b.compressed_envelope as f64 / source.len() as f64;
        assert!(
            ratio > 2.0,
            "expected snapshot expansion; got ratio {ratio:.2}"
        );
        eprintln!(
            "snapshot_size_attribution source={n} units={} ops={} json={} doc_json={} ops_json={} payload={} compressed={} ratio={ratio:.2}",
            b.unit_count,
            b.op_count,
            b.json_total,
            b.document_json,
            b.ops_json,
            b.ops_payload_bytes,
            b.compressed_envelope,
            n = source.len(),
        );
    }
}
