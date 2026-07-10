//! Session snapshot schema and Document ↔ DTO conversion.
//!
//! Snapshots persist document materialization plus the opaque op log for
//! crash recovery and late join. Full op log growth is accepted for 0.1
//! (compaction is a later concern).

use crate::core::{Element, LwwRegister, OpId, PeerId, Sequence};
use crate::doc::{
    Block, BlockId, BlockKind, CellContent, ColumnAlignment, ColumnDef, Document, Table, TableRow,
    TextUnit, units_from_str,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Snapshot schema version (not wire `Envelope` version).
///
/// v1: paragraph body as plain string.  
/// v2: paragraph body as ordered grapheme units (with OpIds).
pub const SNAPSHOT_FORMAT_VERSION: u16 = 2;

/// Prior format that stored paragraph text as a plain string.
pub const SNAPSHOT_FORMAT_VERSION_V1: u16 = 1;

/// Errors loading or decoding session snapshots.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("serde: {0}")]
    Serde(String),
    #[error("unsupported snapshot format version {0}")]
    UnsupportedVersion(u16),
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
    pub document: DocumentDto,
    /// Applied ops `(OpId, payload bytes)` for retransmission / audit.
    pub ops: Vec<(OpId, Vec<u8>)>,
    /// Causally buffered ops not yet in the applied log.
    pub pending: Vec<(OpId, Vec<u8>)>,
}

/// Serializable document: ordered sequence elements (incl. tombstones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDto {
    pub frontmatter: Option<String>,
    pub blocks: Vec<ElementDto<BlockDto>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementDto<T> {
    pub id: OpId,
    pub value: Option<T>,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub kind: BlockKindDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextUnitDto {
    pub grapheme: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKindDto {
    /// v2: `units` preferred. v1 snapshots may only set `legacy_text`.
    Paragraph {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        units: Option<Vec<ElementDto<TextUnitDto>>>,
        #[serde(default, rename = "text", skip_serializing_if = "Option::is_none")]
        legacy_text: Option<String>,
    },
    CodeFence {
        info: Option<String>,
        text: String,
    },
    BlockQuote {
        children: Vec<ElementDto<BlockDto>>,
    },
    RawBlock {
        raw: String,
    },
    Table {
        table: TableDto,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub deleted: LwwDto<bool>,
    pub columns: LwwDto<Vec<ColumnDefDto>>,
    pub header: LwwDto<Vec<CellContent>>,
    pub rows: Vec<ElementDto<TableRowDto>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRowDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub deleted: LwwDto<bool>,
    pub cells: LwwDto<Vec<CellContent>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwDto<T> {
    pub value: T,
    pub op_id: OpId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnDefDto {
    Left,
    Center,
    Right,
}

impl From<&ColumnDef> for ColumnDefDto {
    fn from(c: &ColumnDef) -> Self {
        match c.alignment {
            ColumnAlignment::Left => ColumnDefDto::Left,
            ColumnAlignment::Center => ColumnDefDto::Center,
            ColumnAlignment::Right => ColumnDefDto::Right,
        }
    }
}

impl From<ColumnDefDto> for ColumnDef {
    fn from(c: ColumnDefDto) -> Self {
        ColumnDef {
            alignment: match c {
                ColumnDefDto::Left => ColumnAlignment::Left,
                ColumnDefDto::Center => ColumnAlignment::Center,
                ColumnDefDto::Right => ColumnAlignment::Right,
            },
        }
    }
}

impl DocumentDto {
    pub fn from_document(doc: &Document) -> Self {
        Self {
            frontmatter: doc.frontmatter.clone(),
            blocks: sequence_to_elements(&doc.blocks, block_to_dto),
        }
    }

    pub fn into_document(self) -> Document {
        let mut doc = Document::new();
        doc.frontmatter = self.frontmatter;
        doc.blocks = sequence_from_elements(self.blocks, block_from_dto);
        doc
    }
}

impl SessionSnapshot {
    pub fn to_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        serde_json::to_vec(self).map_err(|e| SnapshotError::Serde(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let snap: Self =
            serde_json::from_slice(bytes).map_err(|e| SnapshotError::Serde(e.to_string()))?;
        if snap.format_version != SNAPSHOT_FORMAT_VERSION
            && snap.format_version != SNAPSHOT_FORMAT_VERSION_V1
        {
            return Err(SnapshotError::UnsupportedVersion(snap.format_version));
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
    walk_block_seq_max_peer(peer, &doc.blocks, &mut max);
    max
}

fn walk_block_seq_max_peer(peer: PeerId, seq: &Sequence<Block>, max: &mut u64) {
    for elem in seq.iter_all() {
        if elem.id.peer == peer {
            *max = (*max).max(elem.id.counter);
        }
        if let Some(block) = elem.value.as_ref() {
            walk_kind_max_peer(peer, &block.kind, max);
        }
    }
}

fn walk_kind_max_peer(peer: PeerId, kind: &BlockKind, max: &mut u64) {
    match kind {
        BlockKind::BlockQuote { children } => walk_block_seq_max_peer(peer, children, max),
        BlockKind::Table { table } => {
            if table.elem_id.peer == peer {
                *max = (*max).max(table.elem_id.counter);
            }
            for elem in table.rows.iter_all() {
                if elem.id.peer == peer {
                    *max = (*max).max(elem.id.counter);
                }
            }
        }
        _ => {}
    }
}

fn sequence_to_elements<T, U, F>(seq: &Sequence<T>, map: F) -> Vec<ElementDto<U>>
where
    T: Clone,
    F: Fn(&T) -> U,
{
    seq.iter_all()
        .map(|elem| ElementDto {
            id: elem.id,
            value: elem.value.as_ref().map(&map),
            after: elem.after,
            right_origin: elem.right_origin,
        })
        .collect()
}

fn sequence_from_elements<T, U, F>(elements: Vec<ElementDto<U>>, map: F) -> Sequence<T>
where
    T: Clone,
    F: Fn(U) -> T,
{
    let elems: Vec<Element<T>> = elements
        .into_iter()
        .map(|e| Element {
            id: e.id,
            value: e.value.map(&map),
            after: e.after,
            right_origin: e.right_origin,
        })
        .collect();
    Sequence::from_elements(elems)
}

fn block_to_dto(block: &Block) -> BlockDto {
    BlockDto {
        id: block.id,
        elem_id: block.elem_id,
        kind: kind_to_dto(&block.kind),
    }
}

fn block_from_dto(dto: BlockDto) -> Block {
    Block {
        id: dto.id,
        elem_id: dto.elem_id,
        kind: kind_from_dto(dto.kind),
        marks: crate::core::MarkSet::new(),
    }
}

fn kind_to_dto(kind: &BlockKind) -> BlockKindDto {
    match kind {
        BlockKind::Paragraph { text } => BlockKindDto::Paragraph {
            units: Some(sequence_to_elements(text, |u| TextUnitDto {
                grapheme: u.grapheme.clone(),
            })),
            legacy_text: None,
        },
        BlockKind::CodeFence { info, text } => BlockKindDto::CodeFence {
            info: info.clone(),
            text: text.clone(),
        },
        BlockKind::RawBlock { raw } => BlockKindDto::RawBlock { raw: raw.clone() },
        BlockKind::BlockQuote { children } => BlockKindDto::BlockQuote {
            children: sequence_to_elements(children, block_to_dto),
        },
        BlockKind::Table { table } => BlockKindDto::Table {
            table: table_to_dto(table),
        },
    }
}

fn kind_from_dto(kind: BlockKindDto) -> BlockKind {
    match kind {
        BlockKindDto::Paragraph { units, legacy_text } => {
            let seq = if let Some(units) = units {
                sequence_from_elements(units, |u| TextUnit {
                    grapheme: u.grapheme,
                })
            } else if let Some(s) = legacy_text {
                let mut c = 1u64;
                units_from_str(&s, &mut c, 0)
            } else {
                Sequence::new()
            };
            BlockKind::Paragraph { text: seq }
        }
        BlockKindDto::CodeFence { info, text } => BlockKind::CodeFence { info, text },
        BlockKindDto::RawBlock { raw } => BlockKind::RawBlock { raw },
        BlockKindDto::BlockQuote { children } => BlockKind::BlockQuote {
            children: sequence_from_elements(children, block_from_dto),
        },
        BlockKindDto::Table { table } => BlockKind::Table {
            table: table_from_dto(table),
        },
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
        columns: LwwDto {
            value: table
                .columns
                .get_ref()
                .iter()
                .map(ColumnDefDto::from)
                .collect(),
            op_id: table.columns.op_id(),
        },
        header: LwwDto {
            value: table.header.get(),
            op_id: table.header.op_id(),
        },
        rows: sequence_to_elements(&table.rows, row_to_dto),
    }
}

fn table_from_dto(dto: TableDto) -> Table {
    Table {
        id: dto.id,
        elem_id: dto.elem_id,
        deleted: LwwRegister::new(dto.deleted.value, dto.deleted.op_id),
        columns: LwwRegister::new(
            dto.columns.value.into_iter().map(ColumnDef::from).collect(),
            dto.columns.op_id,
        ),
        header: LwwRegister::new(dto.header.value, dto.header.op_id),
        rows: sequence_from_elements(dto.rows, row_from_dto),
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
        cells: LwwDto {
            value: row.cells.get(),
            op_id: row.cells.op_id(),
        },
    }
}

fn row_from_dto(dto: TableRowDto) -> TableRow {
    TableRow {
        id: dto.id,
        elem_id: dto.elem_id,
        deleted: LwwRegister::new(dto.deleted.value, dto.deleted.op_id),
        cells: LwwRegister::new(dto.cells.value, dto.cells.op_id),
    }
}
