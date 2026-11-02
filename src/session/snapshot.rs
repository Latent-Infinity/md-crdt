//! Session snapshot schema and Document ↔ DTO conversion.
//!
//! Snapshots persist document materialization plus the opaque op log for
//! crash recovery, checkpoint rebase, and late join.

use crate::core::causal::CausalCandidate;
use crate::core::mark::MarkSet;
use crate::core::{Element, LwwRegister, OpId, PeerId, Sequence, SequenceOp};
use crate::doc::{
    Block, BlockId, BlockKind, BlockKindValue, CellAddress, CellContent, CodeFenceStyle,
    ColumnAlignment, ColumnId, Document, DocumentSource, Frontmatter, ListItemPlacement, ListStyle,
    PendingColumnAlignment, PendingListItemMove, PendingTableMove, Placement, RowId, Table,
    TableCell, TableColumn, TableRow, TaskState, TextUnit,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufWriter, Write};
use thiserror::Error;

/// Snapshot schema version (not wire `Envelope` version).
///
/// v10: durable placement/deletion incarnations, independent frontmatter bases,
/// and source-local frontmatter revision provenance.
pub const SNAPSHOT_FORMAT_VERSION: u16 = 10;

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
    #[error("invalid snapshot state: {0}")]
    InvalidState(&'static str),
    #[error(transparent)]
    Validation(#[from] crate::sync::ValidationError),
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
    /// Applied operations waiting for document dependencies or an observed frontier.
    pub deferred: Vec<(OpId, Vec<u8>)>,
}

/// Serializable document: ordered sequence elements (incl. tombstones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDto {
    pub frontmatter: Option<Frontmatter>,
    pub blocks: SequenceDto<BlockDto>,
    pub block_topology: crate::doc::BlockTopology,
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
    pub kind_conflicts: Vec<CausalCandidate<BlockKindValue>>,
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
    pub task_conflicts: Vec<CausalCandidate<Option<TaskState>>>,
    pub placement_observed: crate::core::StateVector,
    pub placement_conflicts: Vec<CausalCandidate<ListItemPlacement>>,
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
    pub alignment_conflicts: Vec<CausalCandidate<ColumnAlignment>>,
    pub placement_observed: crate::core::StateVector,
    pub placement_conflicts: Vec<CausalCandidate<Placement>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRowDto {
    pub id: BlockId,
    pub elem_id: OpId,
    pub deleted: LwwDto<bool>,
    pub placement_observed: crate::core::StateVector,
    pub placement_conflicts: Vec<CausalCandidate<Placement>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCellDto {
    pub row_id: RowId,
    pub column_id: ColumnId,
    pub value: LwwDto<CellContent>,
    pub observed: crate::core::StateVector,
    pub conflicts: Vec<CausalCandidate<CellContent>>,
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
            block_topology: doc.topology_state(),
            source: doc.source_state(),
        }
    }

    pub fn into_document(self) -> Document {
        let mut doc = Document::new();
        doc.frontmatter = self.frontmatter;
        *doc.blocks_mut() = sequence_from_dto(self.blocks, block_from_dto);
        doc.blocks.block_topology = self.block_topology;
        doc.initialize_placement_roots();
        doc.set_source_state(self.source);
        doc
    }

    fn validate_placement_projection(&self) -> Result<(), SnapshotError> {
        use crate::doc::{BlockPlacement, ItemPlacement, LogicalContainer};
        let metadata = &self.block_topology;
        let selected = metadata.project();
        let mut blocks = std::collections::BTreeSet::new();
        let mut items = std::collections::BTreeSet::new();
        let mut pending = vec![(&self.blocks, None)];
        while let Some((sequence, parent)) = pending.pop() {
            let parent_origin = parent
                .and_then(|parent| metadata.current_target(parent))
                .map(|target| target.origin);
            for element in &sequence.elements {
                let Some(block) = &element.value else {
                    continue;
                };
                let actual = BlockPlacement {
                    parent,
                    parent_origin,
                    id: element.id,
                    after: element.after,
                    right_origin: element.right_origin,
                };
                if block.elem_id != element.id
                    || !blocks.insert(block.id)
                    || metadata.deleted.contains(&block.id)
                    || selected.blocks.get(&block.id) != Some(&actual)
                {
                    return Err(SnapshotError::InvalidState(
                        "document body disagrees with block placement metadata",
                    ));
                }
                match &block.kind {
                    BlockKindDto::BlockQuote { children } => {
                        pending.push((children, Some(LogicalContainer::Block(block.id))))
                    }
                    BlockKindDto::List {
                        items: sequence, ..
                    } => {
                        let list_origin = metadata.initial[&block.id].id;
                        for element in &sequence.elements {
                            let Some(item) = &element.value else {
                                continue;
                            };
                            let actual = ItemPlacement {
                                list_id: block.id,
                                list_origin,
                                id: element.id,
                                after: element.after,
                                right_origin: element.right_origin,
                            };
                            if item.elem_id != element.id
                                || !items.insert(item.id)
                                || metadata.deleted_items.contains(&item.id)
                                || selected.items.get(&item.id) != Some(&actual)
                            {
                                return Err(SnapshotError::InvalidState(
                                    "document body disagrees with item placement metadata",
                                ));
                            }
                            pending.push((&item.children, Some(LogicalContainer::Item(item.id))));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Shared deterministic admission charge for every document-level pending
    /// layer. It is serialized bytes plus entry storage, not an RSS estimate.
    pub(super) fn pending_resources(&self) -> (usize, usize) {
        let mut resources = pending_slice_resources(&self.blocks.pending);
        let mut blocks: Vec<_> = sequence_values(&self.blocks).collect();
        while let Some(block) = blocks.pop() {
            match &block.kind {
                BlockKindDto::Paragraph { units } | BlockKindDto::Heading { units, .. } => {
                    add_resources(&mut resources, pending_slice_resources(&units.pending));
                }
                BlockKindDto::BlockQuote { children } => {
                    add_resources(&mut resources, pending_slice_resources(&children.pending));
                    blocks.extend(sequence_values(children));
                }
                BlockKindDto::List {
                    items,
                    pending_moves,
                    ..
                } => {
                    add_resources(&mut resources, pending_slice_resources(&items.pending));
                    add_resources(&mut resources, pending_slice_resources(pending_moves));
                    for item in sequence_values(items) {
                        add_resources(
                            &mut resources,
                            pending_slice_resources(&item.children.pending),
                        );
                        blocks.extend(sequence_values(&item.children));
                    }
                }
                BlockKindDto::Table { table } => {
                    add_resources(
                        &mut resources,
                        pending_slice_resources(&table.columns.pending),
                    );
                    add_resources(&mut resources, pending_slice_resources(&table.rows.pending));
                    add_resources(
                        &mut resources,
                        pending_slice_resources(&table.pending_row_moves),
                    );
                    add_resources(
                        &mut resources,
                        pending_slice_resources(&table.pending_column_moves),
                    );
                    add_resources(
                        &mut resources,
                        pending_slice_resources(&table.pending_column_alignments),
                    );
                }
                BlockKindDto::CodeFence { .. } | BlockKindDto::RawBlock { .. } => {}
            }
        }
        resources
    }

    fn validate_pending(&self) -> Result<(), SnapshotError> {
        validate_sequence_pending(&self.blocks)?;
        let mut blocks: Vec<_> = sequence_values(&self.blocks).collect();
        while let Some(block) = blocks.pop() {
            match &block.kind {
                BlockKindDto::Paragraph { units } | BlockKindDto::Heading { units, .. } => {
                    validate_sequence_pending(units)?;
                }
                BlockKindDto::BlockQuote { children } => {
                    validate_sequence_pending(children)?;
                    blocks.extend(sequence_values(children));
                }
                BlockKindDto::List {
                    items,
                    pending_moves,
                    ..
                } => {
                    validate_sequence_pending(items)?;
                    for movement in pending_moves {
                        validate_pending_move(
                            movement.id,
                            movement.target,
                            movement.after,
                            movement.right_origin,
                        )?;
                    }
                    for item in sequence_values(items) {
                        validate_sequence_pending(&item.children)?;
                        blocks.extend(sequence_values(&item.children));
                    }
                }
                BlockKindDto::Table { table } => {
                    validate_sequence_pending(&table.columns)?;
                    validate_sequence_pending(&table.rows)?;
                    for movement in table
                        .pending_row_moves
                        .iter()
                        .chain(&table.pending_column_moves)
                    {
                        validate_pending_move(
                            movement.id,
                            movement.target,
                            movement.after,
                            movement.right_origin,
                        )?;
                    }
                    if table
                        .pending_column_alignments
                        .iter()
                        .any(|alignment| alignment.id.counter == 0)
                    {
                        return Err(SnapshotError::InvalidState("zero pending alignment id"));
                    }
                }
                BlockKindDto::CodeFence { .. } | BlockKindDto::RawBlock { .. } => {}
            }
        }
        Ok(())
    }
}

/// Owned validation proof. No publicly mutable DTO escapes between validation,
/// causal-queue normalization and installation of the decoded envelopes.
pub(super) struct ValidatedSnapshot {
    snapshot: SessionSnapshot,
    buffered: BTreeMap<OpId, super::PendingEnvelope>,
}

impl ValidatedSnapshot {
    pub(super) fn from_bytes(bytes: &[u8]) -> Result<Self, SnapshotError> {
        Self::new(SessionSnapshot::decode(bytes)?)
    }

    pub(super) fn new(mut snapshot: SessionSnapshot) -> Result<Self, SnapshotError> {
        let mut buffered = BTreeMap::new();
        snapshot.validate_with(|id, envelope, decoded_bytes| {
            buffered.insert(
                id,
                super::PendingEnvelope {
                    envelope,
                    decoded_bytes,
                    snapshot_raw_bytes: 0,
                },
            );
        })?;
        let mut normalized = false;
        super::take_document_causal_pending(&mut snapshot.document, |envelope| {
            use crate::codec::OpCodec as _;
            let id = super::operation_extent(&envelope).0;
            let payload = crate::codec::JsonOpCodec
                .encode(&envelope)
                .map_err(|error| SnapshotError::Serde(error.to_string()))?;
            snapshot.deferred.push((id, payload));
            normalized = true;
            Ok(())
        })?;
        if normalized {
            // Normalization changes pending representation and raw identity
            // obligations. Never reuse the pre-normalization proof or charges.
            buffered.clear();
            snapshot.validate_with(|id, envelope, decoded_bytes| {
                buffered.insert(
                    id,
                    super::PendingEnvelope {
                        envelope,
                        decoded_bytes,
                        snapshot_raw_bytes: 0,
                    },
                );
            })?;
        }
        Ok(Self { snapshot, buffered })
    }

    pub(super) fn into_parts(self) -> (SessionSnapshot, BTreeMap<OpId, super::PendingEnvelope>) {
        (self.snapshot, self.buffered)
    }
}

impl SessionSnapshot {
    pub fn to_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        encode_snapshot_json(self, MAX_SNAPSHOT_DECOMPRESSED_BYTES)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let snap = Self::decode(bytes)?;
        snap.validate()?;
        Ok(snap)
    }

    fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
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
            if bytes.len() > MAX_SNAPSHOT_DECOMPRESSED_BYTES {
                return Err(SnapshotError::SnapshotTooLarge {
                    size: bytes.len() as u64,
                    max: MAX_SNAPSHOT_DECOMPRESSED_BYTES,
                });
            }
            bytes
        };

        // Check the schema before requiring its fields: old snapshots must fail
        // with the typed cutover error, not a missing-candidate serde error.
        #[derive(Deserialize)]
        struct Version {
            format_version: u16,
        }
        let version: Version =
            serde_json::from_slice(payload).map_err(|e| SnapshotError::Serde(e.to_string()))?;
        if version.format_version != SNAPSHOT_FORMAT_VERSION {
            return Err(SnapshotError::ReinitializeRequired {
                found: version.format_version,
                expected: SNAPSHOT_FORMAT_VERSION,
            });
        }
        serde_json::from_slice(payload).map_err(|e| SnapshotError::Serde(e.to_string()))
    }

    /// Validate persisted metadata before source ranges or buffered operations
    /// are installed into a live session.
    pub(crate) fn validate(&self) -> Result<(), SnapshotError> {
        self.validate_with(|_, _, _| {})
    }

    fn validate_with(
        &self,
        mut buffered: impl FnMut(OpId, crate::codec::Envelope, usize),
    ) -> Result<(), SnapshotError> {
        if self.format_version != SNAPSHOT_FORMAT_VERSION {
            return Err(SnapshotError::ReinitializeRequired {
                found: self.format_version,
                expected: SNAPSHOT_FORMAT_VERSION,
            });
        }
        if let Some(source) = &self.document.source {
            source.validate().map_err(SnapshotError::InvalidState)?;
        }
        self.document
            .block_topology
            .validate(&self.state_vector)
            .map_err(SnapshotError::InvalidState)?;
        self.document.validate_placement_projection()?;
        for (peer, floor) in self.delta_floor.iter() {
            if floor > self.state_vector.get(peer).unwrap_or(0) {
                return Err(SnapshotError::InvalidState("delta floor exceeds frontier"));
            }
        }
        let mut applied = BTreeMap::new();
        for (id, payload) in &self.ops {
            if id.counter == 0 || id.counter > self.state_vector.get(id.peer).unwrap_or(0) {
                return Err(SnapshotError::InvalidState(
                    "applied id is outside frontier",
                ));
            }
            if applied.insert(*id, payload.as_slice()).is_some() {
                return Err(SnapshotError::InvalidState("duplicate applied operation"));
            }
        }
        let max_pending = crate::limits::MAX_DURABLE_PENDING_OPERATIONS;
        let max_bytes = crate::limits::MAX_DURABLE_RETAINED_BYTES;
        let (document_pending, document_bytes) = self.document.pending_resources();
        self.document.validate_pending()?;
        if self
            .pending
            .len()
            .saturating_add(self.deferred.len())
            .saturating_add(document_pending)
            > max_pending
        {
            return Err(SnapshotError::InvalidState(
                "buffered operation count exceeds limit",
            ));
        }
        let mut buffered_ids = BTreeSet::new();
        let mut buffered_spans: BTreeMap<PeerId, BTreeMap<u64, u64>> = BTreeMap::new();
        let mut retained_bytes = document_bytes;
        if retained_bytes > max_bytes {
            return Err(SnapshotError::InvalidState(
                "document pending bytes exceed limit",
            ));
        }
        for (is_deferred, operations) in [(false, &self.pending), (true, &self.deferred)] {
            for (id, payload) in operations {
                if id.counter == 0 || !buffered_ids.insert(*id) {
                    return Err(SnapshotError::InvalidState(
                        "invalid or duplicate buffered id",
                    ));
                }
                // Deferred payloads are canonical envelope encodings; the raw
                // retained copy is the applied log entry, if not checkpointed.
                let raw_bytes = if is_deferred {
                    applied.get(id).map_or(0, |raw| raw.len())
                } else {
                    payload.len()
                };
                retained_bytes = retained_bytes.saturating_add(raw_bytes);
                if retained_bytes > max_bytes {
                    return Err(SnapshotError::InvalidState(
                        "buffered operation bytes exceed limit",
                    ));
                }
                use crate::codec::OpCodec as _;
                let envelope = crate::codec::JsonOpCodec
                    .decode(payload)
                    .map_err(|error| SnapshotError::Serde(error.to_string()))?;
                if let crate::codec::OpBody::Doc(crate::codec::DocOp::InsertBlock {
                    block, ..
                }) = &envelope.body
                {
                    super::validate_block_skeleton(&block.kind).map_err(|_| {
                        SnapshotError::InvalidState("invalid buffered block skeleton")
                    })?;
                }
                super::wire::validate_operation_envelope(*id, &envelope).map_err(|_| {
                    SnapshotError::InvalidState("invalid buffered operation envelope")
                })?;
                if self.unit_mode && !crate::codec::insert_block_paragraph_is_empty(&envelope) {
                    return Err(SnapshotError::InvalidState(
                        "nonempty buffered unit-mode block",
                    ));
                }
                let decoded_bytes = super::wire::envelope_retained_bytes(&envelope);
                retained_bytes = retained_bytes.saturating_add(decoded_bytes);
                if retained_bytes > max_bytes {
                    return Err(SnapshotError::InvalidState(
                        "buffered operation bytes exceed limit",
                    ));
                }
                let (_, span) = super::wire::operation_extent(&envelope);
                let start = id
                    .counter
                    .checked_sub(span.saturating_sub(1))
                    .filter(|start| *start != 0)
                    .ok_or(SnapshotError::InvalidState("invalid buffered counter span"))?;
                let spans = buffered_spans.entry(id.peer).or_default();
                if spans
                    .range(..=id.counter)
                    .next_back()
                    .is_some_and(|(_, end)| *end >= start)
                {
                    return Err(SnapshotError::InvalidState(
                        "overlapping buffered counter spans",
                    ));
                }
                spans.insert(start, id.counter);
                let frontier = self.state_vector.get(id.peer).unwrap_or(0);
                if is_deferred {
                    if id.counter > frontier {
                        return Err(SnapshotError::InvalidState("deferred id exceeds frontier"));
                    }
                    match applied.get(id) {
                        Some(applied_payload) if *applied_payload == payload.as_slice() => {}
                        Some(applied_payload)
                            if crate::codec::JsonOpCodec
                                .decode(applied_payload)
                                .is_ok_and(|applied_envelope| applied_envelope == envelope) => {}
                        None if id.counter <= self.delta_floor.get(id.peer).unwrap_or(0) => {}
                        _ => {
                            return Err(SnapshotError::InvalidState(
                                "deferred payload is not applied",
                            ));
                        }
                    }
                } else {
                    if start <= frontier || applied.contains_key(id) {
                        return Err(SnapshotError::InvalidState(
                            "pending span overlaps frontier",
                        ));
                    }
                }
                buffered(*id, envelope, decoded_bytes);
            }
        }
        Ok(())
    }
}

fn sequence_values<T>(sequence: &SequenceDto<T>) -> impl Iterator<Item = &T> {
    sequence
        .elements
        .iter()
        .filter_map(|element| element.value.as_ref())
        .chain(
            sequence
                .pending
                .iter()
                .filter_map(|operation| match operation {
                    SequenceOpDto::Insert { value, .. } => Some(value),
                    SequenceOpDto::Delete { .. } => None,
                }),
        )
}

fn add_resources(total: &mut (usize, usize), extra: (usize, usize)) {
    total.0 = total.0.saturating_add(extra.0);
    total.1 = total.1.saturating_add(extra.1);
}

fn pending_slice_resources<T: Serialize>(pending: &[T]) -> (usize, usize) {
    if pending.is_empty() {
        return (0, 0);
    }
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    let bytes = if serde_json::to_writer(&mut counter, pending).is_ok() {
        counter
            .0
            .saturating_add(pending.len().saturating_mul(size_of::<T>()))
    } else {
        usize::MAX
    };
    (pending.len(), bytes)
}

fn validate_sequence_pending<T>(sequence: &SequenceDto<T>) -> Result<(), SnapshotError> {
    if sequence.pending.is_empty() {
        return Ok(());
    }
    let mut ids: BTreeSet<_> = sequence.elements.iter().map(|element| element.id).collect();
    for operation in &sequence.pending {
        match operation {
            SequenceOpDto::Insert {
                id,
                after,
                right_origin,
                ..
            } => {
                if id.counter == 0
                    || *after == Some(*id)
                    || *right_origin == Some(*id)
                    || !ids.insert(*id)
                {
                    return Err(SnapshotError::InvalidState(
                        "invalid pending sequence insert",
                    ));
                }
            }
            SequenceOpDto::Delete { id, target } => {
                if id.counter == 0 || target.counter == 0 {
                    return Err(SnapshotError::InvalidState(
                        "invalid pending sequence delete",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_pending_move(
    id: OpId,
    target: OpId,
    after: Option<OpId>,
    right_origin: Option<OpId>,
) -> Result<(), SnapshotError> {
    if id.counter == 0
        || target.counter == 0
        || id == target
        || after == Some(id)
        || right_origin == Some(id)
    {
        return Err(SnapshotError::InvalidState("invalid pending placement"));
    }
    Ok(())
}

/// Only compressed output is retained. Oversized JSON stops at the first write
/// crossing the budget instead of allocating its complete representation.
fn encode_snapshot_json<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, SnapshotError> {
    let mut writer = BoundedSnapshotWriter {
        output: BufWriter::with_capacity(16 * 1024, CompressedSnapshotWriter::new()),
        len: 0,
        max,
        exceeded: None,
    };
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        return Err(match writer.exceeded {
            Some(size) => SnapshotError::SnapshotTooLarge { size, max },
            None => SnapshotError::Serde(error.to_string()),
        });
    }
    let json_len = writer.len as u64;
    let output = writer
        .output
        .into_inner()
        .map_err(|error| SnapshotError::Serde(error.to_string()))?;
    let mut bytes = output
        .finish()
        .map_err(|error| SnapshotError::Serde(error.to_string()))?;
    bytes[SNAPSHOT_MAGIC.len()..SNAPSHOT_HEADER_LEN].copy_from_slice(&json_len.to_le_bytes());
    Ok(bytes)
}

struct BoundedSnapshotWriter {
    output: BufWriter<CompressedSnapshotWriter>,
    len: usize,
    max: usize,
    exceeded: Option<u64>,
}

impl Write for BoundedSnapshotWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.len.saturating_add(bytes.len());
        if next > self.max {
            self.exceeded = Some(next as u64);
            return Err(io::Error::other("snapshot JSON exceeds byte budget"));
        }
        self.output.write_all(bytes)?;
        self.len = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

struct CompressedSnapshotWriter {
    compressor: Box<miniz_oxide::deflate::core::CompressorOxide>,
    bytes: Vec<u8>,
}

impl CompressedSnapshotWriter {
    fn new() -> Self {
        let mut compressor = Box::<miniz_oxide::deflate::core::CompressorOxide>::default();
        compressor.set_format_and_level(miniz_oxide::DataFormat::Zlib, SNAPSHOT_DEFLATE_LEVEL);
        let mut bytes = Vec::with_capacity(SNAPSHOT_HEADER_LEN);
        bytes.extend_from_slice(SNAPSHOT_MAGIC);
        bytes.extend_from_slice(&0u64.to_le_bytes());
        Self { compressor, bytes }
    }

    fn compress(&mut self, mut input: &[u8], finish: bool) -> io::Result<()> {
        use miniz_oxide::{MZFlush, MZStatus};
        let flush = if finish {
            MZFlush::Finish
        } else {
            MZFlush::None
        };
        let mut output = [0u8; 8192];
        loop {
            let result = miniz_oxide::deflate::stream::deflate(
                &mut self.compressor,
                input,
                &mut output,
                flush,
            );
            self.bytes
                .try_reserve(result.bytes_written)
                .map_err(|_| io::Error::other("cannot allocate compressed snapshot"))?;
            self.bytes
                .extend_from_slice(&output[..result.bytes_written]);
            input = &input[result.bytes_consumed..];
            match result.status {
                Ok(MZStatus::StreamEnd) if finish => return Ok(()),
                Ok(_) if !finish && input.is_empty() => return Ok(()),
                Ok(_) if result.bytes_consumed != 0 || result.bytes_written != 0 => {}
                _ => return Err(io::Error::other("snapshot compression failed")),
            }
        }
    }

    fn finish(mut self) -> io::Result<Vec<u8>> {
        self.compress(&[], true)?;
        Ok(self.bytes)
    }
}

impl Write for CompressedSnapshotWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !bytes.is_empty() {
            self.compress(bytes, false)?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Max counter for `peer` across applied ops and all materialized/pending document ids.
pub fn max_counter_for_peer(peer: PeerId, ops: &[(OpId, Vec<u8>)], doc: &Document) -> u64 {
    let mut max = 0u64;
    if let Some(frontmatter) = &doc.frontmatter {
        max = frontmatter.max_counter_for_peer(peer);
    }
    for (id, _) in ops {
        if id.peer == peer {
            max = max.max(id.counter);
        }
    }
    walk_block_seq_max_peer(peer, doc.blocks(), &mut max);
    for movement in &doc.blocks.block_topology.moves {
        record_counter(peer, movement.id, &mut max);
        for block in &movement.blocks {
            record_counter(peer, block.id, &mut max);
        }
    }
    for placement in doc.blocks.block_topology.initial.values() {
        record_counter(peer, placement.id, &mut max);
    }
    for placement in doc.blocks.block_topology.initial_items.values() {
        record_counter(peer, placement.id, &mut max);
    }
    for movement in &doc.blocks.block_topology.item_moves {
        record_counter(peer, movement.id, &mut max);
    }
    for (origin, _) in &doc.blocks.block_topology.block_origins {
        record_counter(peer, *origin, &mut max);
    }
    for (origin, _) in &doc.blocks.block_topology.item_origins {
        record_counter(peer, *origin, &mut max);
    }
    for deletion in &doc.blocks.block_topology.deletions {
        record_counter(peer, deletion.id, &mut max);
    }
    max
}

fn record_counter(peer: PeerId, id: OpId, max: &mut u64) {
    if id.peer == peer {
        *max = (*max).max(id.counter);
    }
}

fn walk_sequence_max_peer<T: Clone>(
    peer: PeerId,
    sequence: &Sequence<T>,
    max: &mut u64,
    mut visit: impl FnMut(&T, &mut u64),
) {
    for element in sequence.iter_all() {
        record_counter(peer, element.id, max);
        if let Some(value) = &element.value {
            visit(value, max);
        }
    }
    for operation in sequence.pending_ops_iter() {
        match operation {
            SequenceOp::Insert { id, value, .. } => {
                record_counter(peer, *id, max);
                visit(value, max);
            }
            SequenceOp::Delete { id, .. } => record_counter(peer, *id, max),
        }
    }
}

fn walk_block_seq_max_peer(peer: PeerId, seq: &Sequence<Block>, max: &mut u64) {
    walk_sequence_max_peer(peer, seq, max, |block, max| {
        record_counter(peer, block.elem_id, max);
        record_counter(peer, block.kind_op, max);
        for candidate in &block.kind_conflicts {
            record_counter(peer, candidate.op_id, max);
        }
        walk_marks_max_peer(peer, &block.marks, max);
        walk_kind_max_peer(peer, &block.kind, max);
    });
}

fn walk_kind_max_peer(peer: PeerId, kind: &BlockKind, max: &mut u64) {
    match kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
            walk_sequence_max_peer(peer, text, max, |_, _| {});
        }
        BlockKind::BlockQuote { children } => walk_block_seq_max_peer(peer, children, max),
        BlockKind::List {
            items,
            pending_moves,
            ..
        } => {
            walk_sequence_max_peer(peer, items, max, |item, max| {
                record_counter(peer, item.elem_id, max);
                record_counter(peer, item.task_op, max);
                for candidate in &item.task_conflicts {
                    record_counter(peer, candidate.op_id, max);
                }
                for candidate in &item.placement_conflicts {
                    record_counter(peer, candidate.op_id, max);
                }
                walk_block_seq_max_peer(peer, &item.children, max);
            });
            for movement in pending_moves {
                record_counter(peer, movement.id, max);
            }
        }
        BlockKind::Table { table } => {
            record_counter(peer, table.elem_id, max);
            walk_sequence_max_peer(peer, &table.columns, max, |column, max| {
                for id in [
                    column.elem_id,
                    column.deleted.op_id(),
                    column.alignment.op_id(),
                ] {
                    record_counter(peer, id, max);
                }
                for candidate in &column.alignment_conflicts {
                    record_counter(peer, candidate.op_id, max);
                }
                for candidate in &column.placement_conflicts {
                    record_counter(peer, candidate.op_id, max);
                }
            });
            walk_sequence_max_peer(peer, &table.rows, max, |row, max| {
                record_counter(peer, row.elem_id, max);
                record_counter(peer, row.deleted.op_id(), max);
                for candidate in &row.placement_conflicts {
                    record_counter(peer, candidate.op_id, max);
                }
            });
            for cell in table.cells.values() {
                record_counter(peer, cell.op_id, max);
                for candidate in &cell.conflicts {
                    record_counter(peer, candidate.op_id, max);
                }
            }
            for movement in table
                .pending_row_moves
                .iter()
                .chain(&table.pending_column_moves)
            {
                record_counter(peer, movement.id, max);
            }
            for alignment in &table.pending_column_alignments {
                record_counter(peer, alignment.id, max);
            }
        }
        BlockKind::CodeFence { .. } | BlockKind::RawBlock { .. } => {}
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
            .pending_ops_iter()
            .map(|operation| match operation {
                SequenceOp::Insert {
                    after,
                    id,
                    value,
                    right_origin,
                } => SequenceOpDto::Insert {
                    after: *after,
                    id: *id,
                    value: map(value),
                    right_origin: *right_origin,
                },
                SequenceOp::Delete { target, id } => SequenceOpDto::Delete {
                    target: *target,
                    id: *id,
                },
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
        kind_conflicts: block.kind_conflicts.clone(),
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
        kind_conflicts: dto.kind_conflicts,
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
        task_conflicts: item.task_conflicts.clone(),
        placement_observed: item.placement_observed.clone(),
        placement_conflicts: item.placement_conflicts.clone(),
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
        task_conflicts: dto.task_conflicts,
        placement_observed: dto.placement_observed,
        placement_conflicts: dto.placement_conflicts,
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
                conflicts: value.conflicts.clone(),
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
                    TableCell {
                        value: cell.value.value,
                        op_id: cell.value.op_id,
                        observed: cell.observed,
                        conflicts: cell.conflicts,
                    },
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
        alignment_conflicts: column.alignment_conflicts.clone(),
        placement_observed: column.placement_observed.clone(),
        placement_conflicts: column.placement_conflicts.clone(),
    }
}

fn column_from_dto(dto: TableColumnDto) -> TableColumn {
    TableColumn {
        id: dto.id,
        elem_id: dto.elem_id,
        deleted: LwwRegister::new(dto.deleted.value, dto.deleted.op_id),
        alignment: LwwRegister::new(dto.alignment.value, dto.alignment.op_id),
        alignment_observed: dto.alignment_observed,
        alignment_conflicts: dto.alignment_conflicts,
        placement_observed: dto.placement_observed,
        placement_conflicts: dto.placement_conflicts,
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
        placement_conflicts: row.placement_conflicts.clone(),
    }
}

fn row_from_dto(dto: TableRowDto) -> TableRow {
    TableRow {
        id: dto.id,
        elem_id: dto.elem_id,
        deleted: LwwRegister::new(dto.deleted.value, dto.deleted.op_id),
        placement_observed: dto.placement_observed,
        placement_conflicts: dto.placement_conflicts,
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
    use crate::session::snapshot::{
        SNAPSHOT_DEFLATE_LEVEL, SNAPSHOT_HEADER_LEN, SessionSnapshot, SnapshotError,
        encode_snapshot_json,
    };

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

    #[test]
    fn streaming_budget_accepts_exact_json_length_and_rejects_the_next_byte() {
        let snapshot = CollaborativeDocument::new(1).save_snapshot().unwrap();
        let json = serde_json::to_vec(&snapshot).unwrap();
        let bytes = encode_snapshot_json(&snapshot, json.len()).unwrap();
        let decoded =
            miniz_oxide::inflate::decompress_to_vec_zlib(&bytes[SNAPSHOT_HEADER_LEN..]).unwrap();
        assert_eq!(decoded, json);
        assert!(matches!(
            encode_snapshot_json(&snapshot, json.len() - 1),
            Err(SnapshotError::SnapshotTooLarge { max, .. }) if max == json.len() - 1
        ));
    }

    #[test]
    fn streaming_budget_stops_serialization_without_visiting_the_full_value() {
        use serde::ser::SerializeSeq as _;
        use std::cell::Cell;

        struct Repeated<'a>(&'a Cell<usize>);
        impl serde::Serialize for Repeated<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(Some(1_000_000))?;
                for _ in 0..1_000_000 {
                    self.0.set(self.0.get() + 1);
                    sequence.serialize_element("a bounded JSON chunk")?;
                }
                sequence.end()
            }
        }
        let visited = Cell::new(0);
        assert!(matches!(
            encode_snapshot_json(&Repeated(&visited), 4096),
            Err(SnapshotError::SnapshotTooLarge { max: 4096, .. })
        ));
        assert!(visited.get() < 200);
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
