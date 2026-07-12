//! Collaborative session: document + sync log + peer clock.
//!
//! Owns encode-before-apply local commits and pre-decode remote apply.
//! Payload-opaque [`crate::sync::SyncState`] never sees codec types.

pub mod snapshot;

pub use snapshot::{
    DocumentDto, SNAPSHOT_FORMAT_VERSION, SessionSnapshot, SnapshotError, max_counter_for_peer,
};

use crate::codec::{
    BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert, ColumnAlignmentWire, DocOp, Envelope,
    JsonOpCodec, ListItemSkeleton, MovedTextUnitWire, OpBody, OpCodec, TextBlockKindWire,
    TextUnitWire, WIRE_VERSION, insert_block_paragraph_is_empty,
};
use crate::core::mark::MarkSet;
use crate::core::{OpId, PeerId, Sequence, SequenceOp, StateVector};
use crate::doc::{
    Block, BlockId, BlockKind, ColumnAlignment, ColumnDef, Document, ListItem, Table, TextUnit,
    after_for_grapheme_offset, block_id_from_op, grapheme_count, paragraph_visible_ids,
    paragraph_visible_string, units_from_str,
};
use crate::sync::{
    ChangeMessage, IntegrateResult, Operation, SyncState, ValidationError, ValidationLimits,
    validate_changes,
};
use std::collections::BTreeMap;
use thiserror::Error;

/// Errors from session-local commits and remote apply.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("codec: {0}")]
    Codec(String),
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("unknown wire version {0}")]
    UnknownWireVersion(u16),
    #[error("operation id is not max id in envelope")]
    OperationIdMismatch,
    #[error("peer id mismatch in payload")]
    PeerMismatch,
    #[error("paragraph InsertBlock must have empty text; use InsertText for body")]
    NonEmptyParagraphOnInsertBlock,
    #[error("after anchor not found for local insert")]
    MissingAfterAnchor,
    #[error("delete target not found")]
    MissingDeleteTarget,
    #[error("table InsertBlock must not contain rows; use InsertTableRow")]
    NonEmptyTableOnInsertBlock,
    #[error("block not found")]
    BlockNotFound,
    #[error("target is not a paragraph")]
    NotParagraph,
    #[error("target is not a table")]
    NotTable,
    #[error("table row not found")]
    TableRowNotFound,
    #[error("blocks must be adjacent siblings")]
    BlocksNotAdjacent,
    #[error("grapheme offset out of range")]
    InvalidOffset,
}

fn codec_err<E: std::fmt::Display>(e: E) -> SessionError {
    let s = e.to_string();
    // JsonOpCodec rejects unknown versions as CodecError::UnknownVersion(...).
    if let Some(rest) = s.strip_prefix("unknown wire version ") {
        if let Ok(v) = rest.parse::<u16>() {
            return SessionError::UnknownWireVersion(v);
        }
    }
    SessionError::Codec(s)
}

/// Result of applying a remote change message at the session layer.
#[derive(Debug, Clone, Default)]
pub struct SessionApplyResult {
    pub applied: Vec<OpId>,
    pub buffered: Vec<OpId>,
}

/// Document + sync log + local peer clock for collaborative editing.
pub struct CollaborativeDocument<C: OpCodec = JsonOpCodec> {
    peer: PeerId,
    /// Next counter to allocate; MUST start at 1 (sync rejects counter == 0).
    next_counter: u64,
    document: Document,
    sync: SyncState,
    codec: C,
    /// When true, InsertBlock Paragraph skeleton text must be empty.
    unit_mode: bool,
    /// Decoded envelopes for causally buffered ops (avoid re-decode).
    pending_envelopes: BTreeMap<OpId, Envelope>,
}

impl CollaborativeDocument<JsonOpCodec> {
    pub fn new(peer: PeerId) -> Self {
        // Unit-mode is the collaborative default: empty InsertBlock + InsertText body.
        Self::with_codec(peer, JsonOpCodec, true)
    }
}

impl<C: OpCodec> CollaborativeDocument<C> {
    pub fn with_codec(peer: PeerId, codec: C, unit_mode: bool) -> Self {
        Self {
            peer,
            next_counter: 1,
            document: Document::new(),
            sync: SyncState::new(),
            codec,
            unit_mode,
            pending_envelopes: BTreeMap::new(),
        }
    }

    pub fn peer(&self) -> PeerId {
        self.peer
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn state_vector(&self) -> StateVector {
        self.sync.state_vector()
    }

    pub fn unit_mode(&self) -> bool {
        self.unit_mode
    }

    pub fn set_unit_mode(&mut self, unit_mode: bool) {
        self.unit_mode = unit_mode;
    }

    /// Peek next OpId without advancing the clock.
    pub fn peek_next_id(&self) -> OpId {
        OpId {
            counter: self.next_counter,
            peer: self.peer,
        }
    }

    /// Encode ops not yet seen by `since` (for exchange with peers).
    pub fn encode_changes_since(&self, since: &StateVector) -> ChangeMessage {
        self.sync.encode_changes_since(since)
    }

    /// Insert a top-level block after `after` (None = start). Returns the block `elem_id`.
    pub fn insert_block(
        &mut self,
        after: Option<OpId>,
        kind: BlockKind,
    ) -> Result<OpId, SessionError> {
        self.insert_block_in(None, after, kind)
    }

    /// Insert a block into `parent`'s children (top-level when `parent` is `None`).
    pub fn insert_block_in(
        &mut self,
        parent: Option<OpId>,
        after: Option<OpId>,
        kind: BlockKind,
    ) -> Result<OpId, SessionError> {
        let ok = match self.document.container_children(parent) {
            None => return Err(SessionError::MissingAfterAnchor),
            Some(children) => after.is_none_or(|a| children.get_element(&a).is_some()),
        };
        if !ok {
            return Err(SessionError::MissingAfterAnchor);
        }

        let b = self.next_counter;
        let block_elem = OpId {
            peer: self.peer,
            counter: b,
        };
        let block_id = block_id_from_op(block_elem);
        let right_origin = self.document.compute_child_right_origin(parent, after);
        let skeleton = block_kind_to_skeleton(&kind, self.unit_mode)?;
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertBlock {
                parent,
                after,
                id: block_elem,
                right_origin,
                block: BlockSkeleton {
                    block_id,
                    kind: skeleton,
                },
            }),
        };

        if self.unit_mode && !insert_block_paragraph_is_empty(&envelope) {
            return Err(SessionError::NonEmptyParagraphOnInsertBlock);
        }

        // Operation.id is the max embedded id (N1); a paragraph body expands into text
        // units at b+1..b+G, so the op covers a counter range and its id is b+G.
        let (op_id, _span) = operation_extent(&envelope);
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        // Apply to document before advancing clock / logging (N3).
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation { id: op_id, payload });
        // Advance past the whole reserved range so later ids never collide with the units.
        self.next_counter = op_id.counter + 1;
        Ok(block_elem)
    }

    /// Delete a top-level block element `target`. Returns the delete-op id.
    pub fn delete_block(&mut self, target: OpId) -> Result<OpId, SessionError> {
        self.delete_block_in(None, target)
    }

    /// Delete a block from `parent`'s children (top-level when `parent` is `None`).
    pub fn delete_block_in(
        &mut self,
        parent: Option<OpId>,
        target: OpId,
    ) -> Result<OpId, SessionError> {
        let found = matches!(
            self.document.container_children(parent),
            Some(children) if children.get_element(&target).is_some()
        );
        if !found {
            return Err(SessionError::MissingDeleteTarget);
        }

        let b = self.next_counter;
        let delete_id = OpId {
            peer: self.peer,
            counter: b,
        };
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::DeleteBlock {
                parent,
                target,
                id: delete_id,
            }),
        };
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation {
            id: delete_id,
            payload,
        });
        self.next_counter = b + 1;
        Ok(delete_id)
    }

    /// Insert an empty paragraph skeleton, then `InsertText` for `text` when non-empty.
    ///
    /// Two N3 commits (N6-d). Returns the block `elem_id`. Empty `text` is block-only.
    pub fn insert_paragraph(
        &mut self,
        after: Option<OpId>,
        text: &str,
    ) -> Result<OpId, SessionError> {
        self.insert_paragraph_in(None, after, text)
    }

    /// Insert a paragraph (empty `InsertBlock` + `InsertText` body, N6-d) into `parent`'s
    /// children (top-level when `parent` is `None`).
    pub fn insert_paragraph_in(
        &mut self,
        parent: Option<OpId>,
        after: Option<OpId>,
        text: &str,
    ) -> Result<OpId, SessionError> {
        let empty = BlockKind::paragraph(
            "",
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_elem = self.insert_block_in(parent, after, empty)?;
        if text.is_empty() {
            return Ok(block_elem);
        }
        let block_id = block_id_from_op(block_elem);
        self.insert_text(block_id, 0, text)?;
        Ok(block_elem)
    }

    /// Insert an empty table block. Rows are added with [`Self::insert_table_row`].
    pub fn insert_table(
        &mut self,
        after: Option<OpId>,
        columns: Vec<ColumnDef>,
        header: Vec<String>,
    ) -> Result<OpId, SessionError> {
        let elem_id = self.peek_next_id();
        let table = Table::new(block_id_from_op(elem_id), elem_id, columns, header, elem_id);
        self.insert_block(after, BlockKind::Table { table })
    }

    /// Insert a row into a table's row sequence.
    pub fn insert_table_row(
        &mut self,
        table_id: BlockId,
        after: Option<OpId>,
        cells: Vec<String>,
    ) -> Result<OpId, SessionError> {
        let (table_elem, right_origin) = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            if after.is_some_and(|id| table.rows.get_element(&id).is_none()) {
                return Err(SessionError::TableRowNotFound);
            }
            (block.elem_id, table.rows.compute_right_origin(after))
        };
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertTableRow {
                table_elem,
                table_id,
                after,
                id,
                right_origin,
                cells,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Replace a row's cells using its LWW register.
    pub fn set_table_row_cells(
        &mut self,
        table_id: BlockId,
        row: OpId,
        cells: Vec<String>,
    ) -> Result<OpId, SessionError> {
        let table_elem = self.table_elem_with_row(table_id, row)?;
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::SetTableRowCells {
                table_elem,
                table_id,
                row,
                id,
                cells,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Tombstone a table row.
    pub fn delete_table_row(
        &mut self,
        table_id: BlockId,
        target: OpId,
    ) -> Result<OpId, SessionError> {
        let table_elem = self.table_elem_with_row(table_id, target)?;
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::DeleteTableRow {
                table_elem,
                table_id,
                target,
                id,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    fn table_elem_with_row(&self, table_id: BlockId, row: OpId) -> Result<OpId, SessionError> {
        let block = self
            .document
            .find_block_by_id(table_id)
            .ok_or(SessionError::BlockNotFound)?;
        let BlockKind::Table { table } = &block.kind else {
            return Err(SessionError::NotTable);
        };
        if table.rows.get_element(&row).is_none() {
            return Err(SessionError::TableRowNotFound);
        }
        Ok(block.elem_id)
    }

    fn commit_single_id(&mut self, envelope: Envelope, id: OpId) -> Result<OpId, SessionError> {
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation { id, payload });
        self.next_counter = id.counter + 1;
        Ok(id)
    }

    /// Insert grapheme units into a paragraph. Returns the max unit `OpId` (N1).
    ///
    /// Empty `text` is a no-op that does not advance the clock.
    pub fn insert_text(
        &mut self,
        block_id: BlockId,
        grapheme_offset: usize,
        text: &str,
    ) -> Result<Option<OpId>, SessionError> {
        if text.is_empty() {
            return Ok(None);
        }

        let (block_elem, units) = {
            let block = self
                .document
                .find_block_by_id(block_id)
                .ok_or(SessionError::BlockNotFound)?;
            let block_elem = block.elem_id;
            let Some(body) = crate::doc::block_text_seq(&block.kind) else {
                return Err(SessionError::NotParagraph);
            };
            if grapheme_offset > body.len_visible() {
                return Err(SessionError::InvalidOffset);
            }

            let mut after = after_for_grapheme_offset(body, grapheme_offset);
            let mut counter = self.next_counter;
            let mut units = Vec::new();
            for g in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
                let id = OpId {
                    counter,
                    peer: self.peer,
                };
                counter = counter.saturating_add(1);
                // First unit: right_origin from current paragraph; subsequent chain units
                // insert after a brand-new id so right_origin is None.
                let right_origin = if units.is_empty() {
                    body.compute_right_origin(after)
                } else {
                    None
                };
                units.push(TextUnitWire {
                    id,
                    after,
                    right_origin,
                    grapheme: g.to_string(),
                });
                after = Some(id);
            }
            (block_elem, units)
        };

        if units.is_empty() {
            return Ok(None);
        }

        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertText {
                block_elem,
                block_id,
                units,
            }),
        };
        let (op_id, _span) = operation_extent(&envelope);
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation { id: op_id, payload });
        self.next_counter = op_id.counter + 1;
        Ok(Some(op_id))
    }

    /// Delete a visible grapheme range from a paragraph. Returns the delete-op id.
    ///
    /// `grapheme_count == 0` is a no-op that does not advance the clock.
    pub fn delete_text(
        &mut self,
        block_id: BlockId,
        grapheme_offset: usize,
        grapheme_count: usize,
    ) -> Result<Option<OpId>, SessionError> {
        if grapheme_count == 0 {
            return Ok(None);
        }

        let (block_elem, targets) = {
            let block = self
                .document
                .find_block_by_id(block_id)
                .ok_or(SessionError::BlockNotFound)?;
            let block_elem = block.elem_id;
            let Some(body) = crate::doc::block_text_seq(&block.kind) else {
                return Err(SessionError::NotParagraph);
            };
            let ids = paragraph_visible_ids(body);
            let end = grapheme_offset
                .checked_add(grapheme_count)
                .ok_or(SessionError::InvalidOffset)?;
            if end > ids.len() {
                return Err(SessionError::InvalidOffset);
            }
            let targets = ids[grapheme_offset..end].to_vec();
            (block_elem, targets)
        };

        let delete_id = OpId {
            peer: self.peer,
            counter: self.next_counter,
        };
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::DeleteText {
                block_elem,
                block_id,
                id: delete_id,
                targets,
            }),
        };
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation {
            id: delete_id,
            payload,
        });
        self.next_counter = delete_id.counter + 1;
        Ok(Some(delete_id))
    }

    /// Split a top-level paragraph or heading at a grapheme offset.
    pub fn split_block(
        &mut self,
        block_id: BlockId,
        grapheme_offset: usize,
    ) -> Result<OpId, SessionError> {
        self.split_block_in(None, block_id, grapheme_offset)
    }

    /// Split a paragraph or heading inside `parent`, preserving suffix unit ids.
    pub fn split_block_in(
        &mut self,
        parent: Option<OpId>,
        block_id: BlockId,
        grapheme_offset: usize,
    ) -> Result<OpId, SessionError> {
        let (target, kind, units) = {
            let children = self
                .document
                .container_children(parent)
                .ok_or(SessionError::BlockNotFound)?;
            let block = children
                .iter()
                .find(|block| block.id == block_id)
                .ok_or(SessionError::BlockNotFound)?;
            let (body, kind) = match &block.kind {
                BlockKind::Paragraph { text } => (text, TextBlockKindWire::Paragraph),
                BlockKind::Heading { level, text } => {
                    (text, TextBlockKindWire::Heading { level: *level })
                }
                _ => return Err(SessionError::NotParagraph),
            };
            if grapheme_offset > body.len_visible() {
                return Err(SessionError::InvalidOffset);
            }
            let units = body
                .iter_all()
                .filter_map(|element| {
                    element
                        .value
                        .as_ref()
                        .map(|unit| (element.id, unit.grapheme.clone()))
                })
                .skip(grapheme_offset)
                .map(|(id, grapheme)| MovedTextUnitWire {
                    source_id: id,
                    id,
                    grapheme,
                })
                .collect();
            (block.elem_id, kind, units)
        };

        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::SplitBlock {
                parent,
                target,
                id,
                new_block_id: block_id_from_op(id),
                right_origin: self
                    .document
                    .compute_child_right_origin(parent, Some(target)),
                kind,
                units,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Merge two adjacent top-level paragraphs or headings into the left block.
    pub fn merge_blocks(
        &mut self,
        left_id: BlockId,
        right_id: BlockId,
    ) -> Result<OpId, SessionError> {
        self.merge_blocks_in(None, left_id, right_id)
    }

    /// Merge two adjacent text-bearing siblings inside `parent`.
    pub fn merge_blocks_in(
        &mut self,
        parent: Option<OpId>,
        left_id: BlockId,
        right_id: BlockId,
    ) -> Result<OpId, SessionError> {
        let action_id = self.peek_next_id();
        let (left, right, after, right_origin, source_units, occupied) = {
            let children = self
                .document
                .container_children(parent)
                .ok_or(SessionError::BlockNotFound)?;
            let visible: Vec<_> = children.iter().collect();
            let left_pos = visible
                .iter()
                .position(|block| block.id == left_id)
                .ok_or(SessionError::BlockNotFound)?;
            let right_pos = visible
                .iter()
                .position(|block| block.id == right_id)
                .ok_or(SessionError::BlockNotFound)?;
            if right_pos != left_pos + 1 {
                return Err(SessionError::BlocksNotAdjacent);
            }
            let left_block = visible[left_pos];
            let right_block = visible[right_pos];
            let Some(left_body) = crate::doc::block_text_seq(&left_block.kind) else {
                return Err(SessionError::NotParagraph);
            };
            let Some(right_body) = crate::doc::block_text_seq(&right_block.kind) else {
                return Err(SessionError::NotParagraph);
            };
            let after = paragraph_visible_ids(left_body).last().copied();
            let right_origin = left_body.compute_right_origin(after);
            let source_units = right_body
                .iter_all()
                .filter_map(|element| {
                    element
                        .value
                        .as_ref()
                        .map(|unit| (element.id, unit.grapheme.clone()))
                })
                .collect::<Vec<_>>();
            let occupied = left_body
                .iter_all()
                .map(|element| element.id)
                .collect::<std::collections::BTreeSet<_>>();
            (
                left_block.elem_id,
                right_block.elem_id,
                after,
                right_origin,
                source_units,
                occupied,
            )
        };

        let mut replacement_counter = action_id.counter.saturating_add(1);
        let units = source_units
            .into_iter()
            .map(|(source_id, grapheme)| {
                let id = if occupied.contains(&source_id) {
                    let replacement = OpId {
                        counter: replacement_counter,
                        peer: self.peer,
                    };
                    replacement_counter = replacement_counter.saturating_add(1);
                    replacement
                } else {
                    source_id
                };
                MovedTextUnitWire {
                    source_id,
                    id,
                    grapheme,
                }
            })
            .collect();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::MergeBlocks {
                parent,
                left,
                right,
                id: action_id,
                after,
                right_origin,
                units,
            }),
        };
        let (op_id, _) = operation_extent(&envelope);
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation { id: op_id, payload });
        self.next_counter = op_id.counter.saturating_add(1);
        Ok(op_id)
    }

    /// Apply remote changes: pre-decode all, then integrate with document apply.
    pub fn apply_remote(
        &mut self,
        message: ChangeMessage,
        limits: &ValidationLimits,
    ) -> Result<SessionApplyResult, SessionError> {
        validate_changes(&message, limits, self.sync.pending_count())?;

        let mut prepared: Vec<(Operation, Envelope)> = Vec::with_capacity(message.ops.len());
        for op in message.ops {
            if self.sync.contains(op.id) {
                continue;
            }
            if op.id.counter == 0 {
                return Err(SessionError::Validation(
                    ValidationError::MalformedOperation {
                        op_id: op.id,
                        kind: crate::sync::MalformedKind::ZeroCounter,
                    },
                ));
            }
            let env = self.codec.decode(&op.payload).map_err(codec_err)?;
            if env.version != WIRE_VERSION {
                return Err(SessionError::UnknownWireVersion(env.version));
            }
            // Reject a unit-mode non-empty paragraph before computing the expansion-based
            // max id (in unit-mode a paragraph must not expand at all).
            if self.unit_mode && !insert_block_paragraph_is_empty(&env) {
                return Err(SessionError::NonEmptyParagraphOnInsertBlock);
            }
            check_operation_id_is_max(&op, &env)?;
            check_peer_consistency(&op, &env)?;
            prepared.push((op, env));
        }

        let mut result = SessionApplyResult::default();
        for (op, env) in prepared {
            let id = op.id;
            let (_, span) = operation_extent(&env);
            match self.sync.apply_one(op, span) {
                IntegrateResult::AlreadyPresent => {}
                IntegrateResult::Buffered => {
                    self.pending_envelopes.insert(id, env);
                    result.buffered.push(id);
                }
                IntegrateResult::Applied => {
                    apply_envelope_to_document(&mut self.document, &env);
                    result.applied.push(id);
                    self.drain_promoted(&mut result)?;
                }
            }
        }
        Ok(result)
    }

    fn drain_promoted(&mut self, result: &mut SessionApplyResult) -> Result<(), SessionError> {
        for promoted in self.sync.promote_ready_pending() {
            let env = if let Some(env) = self.pending_envelopes.remove(&promoted.id) {
                env
            } else {
                self.codec.decode(&promoted.payload).map_err(codec_err)?
            };
            apply_envelope_to_document(&mut self.document, &env);
            result.buffered.retain(|x| *x != promoted.id);
            result.applied.push(promoted.id);
        }
        Ok(())
    }

    /// Serialize current session state (document + op log + clock).
    ///
    /// Includes the full applied op log for retransmission; growth without
    /// compaction is accepted for 0.1.
    pub fn save_snapshot(&self) -> Result<SessionSnapshot, SnapshotError> {
        let ops = self.sync.applied_ops();
        let pending: Vec<(OpId, Vec<u8>)> = self
            .sync
            .pending()
            .into_iter()
            .map(|op| (op.id, op.payload))
            .collect();
        Ok(SessionSnapshot {
            format_version: SNAPSHOT_FORMAT_VERSION,
            peer: self.peer,
            next_counter: self.next_counter,
            unit_mode: self.unit_mode,
            document: DocumentDto::from_document(&self.document),
            ops,
            pending,
        })
    }

    /// Switch peer identity after restore (late join without reloading bytes).
    pub fn rebind_peer(&mut self, local_peer: PeerId) {
        self.peer = local_peer;
        let ops = self.sync.applied_ops();
        let mut max = max_counter_for_peer(local_peer, &ops, &self.document);
        for op in self.sync.pending() {
            if op.id.peer == local_peer {
                max = max.max(op.id.counter);
            }
        }
        self.next_counter = max.saturating_add(1).max(1);
        self.pending_envelopes.clear();
    }

    #[cfg(feature = "storage")]
    pub fn write_to_storage(&self, storage: &crate::storage::Storage) -> Result<(), SnapshotError> {
        let snap = self.save_snapshot()?;
        let bytes = snap.to_bytes()?;
        storage.write_snapshot(&bytes, &[], false)?;
        Ok(())
    }
}

impl CollaborativeDocument<JsonOpCodec> {
    /// Crash recovery for the **same** peer: restore peer id and `next_counter`.
    pub fn restore_from_snapshot(snap: SessionSnapshot) -> Result<Self, SnapshotError> {
        if snap.format_version != SNAPSHOT_FORMAT_VERSION
            && snap.format_version != snapshot::SNAPSHOT_FORMAT_VERSION_V1
        {
            return Err(SnapshotError::UnsupportedVersion(snap.format_version));
        }
        let doc = snap.document.clone().into_document();
        let mut max = max_counter_for_peer(snap.peer, &snap.ops, &doc);
        for (id, _) in &snap.pending {
            if id.peer == snap.peer {
                max = max.max(id.counter);
            }
        }
        if snap.next_counter <= max {
            return Err(SnapshotError::ClockBehind {
                peer: snap.peer,
                next: snap.next_counter,
                max,
            });
        }

        let mut sync = SyncState::new();
        sync.restore_applied(snap.ops);
        let pending_ops: Vec<(Operation, u64)> = snap
            .pending
            .into_iter()
            .map(|(id, payload)| {
                let span = span_of_payload(&payload);
                (Operation { id, payload }, span)
            })
            .collect();
        sync.restore_pending(pending_ops);

        Ok(Self {
            peer: snap.peer,
            next_counter: snap.next_counter,
            document: doc,
            sync,
            codec: JsonOpCodec,
            unit_mode: snap.unit_mode,
            pending_envelopes: BTreeMap::new(),
        })
    }

    /// Late join: load compact state as `local_peer` (does not adopt snapshot peer).
    pub fn import_state(
        document: DocumentDto,
        ops: Vec<(OpId, Vec<u8>)>,
        pending: Vec<(OpId, Vec<u8>)>,
        local_peer: PeerId,
        unit_mode: bool,
    ) -> Self {
        let doc = document.into_document();
        let mut max = max_counter_for_peer(local_peer, &ops, &doc);
        for (id, _) in &pending {
            if id.peer == local_peer {
                max = max.max(id.counter);
            }
        }
        let next_counter = max.saturating_add(1).max(1);

        let mut sync = SyncState::new();
        sync.restore_applied(ops);
        let pending_ops: Vec<(Operation, u64)> = pending
            .into_iter()
            .map(|(id, payload)| {
                let span = span_of_payload(&payload);
                (Operation { id, payload }, span)
            })
            .collect();
        sync.restore_pending(pending_ops);

        Self {
            peer: local_peer,
            next_counter,
            document: doc,
            sync,
            codec: JsonOpCodec,
            unit_mode,
            pending_envelopes: BTreeMap::new(),
        }
    }

    #[cfg(feature = "storage")]
    pub fn read_from_storage(storage: &crate::storage::Storage) -> Result<Self, SnapshotError> {
        let (bytes, _, _) = storage.read_snapshot()?;
        let snap = SessionSnapshot::from_bytes(&bytes)?;
        Self::restore_from_snapshot(snap)
    }
}

/// Counter span an op payload covers, for restoring pending ops. Falls back to 1 if the
/// payload cannot be decoded (trusted local disk, N5).
fn span_of_payload(payload: &[u8]) -> u64 {
    JsonOpCodec
        .decode(payload)
        .map(|env| operation_extent(&env).1)
        .unwrap_or(1)
}

fn check_operation_id_is_max(op: &Operation, env: &Envelope) -> Result<(), SessionError> {
    let (max, _span) = operation_extent(env);
    if op.id != max {
        return Err(SessionError::OperationIdMismatch);
    }
    Ok(())
}

/// The `(max embedded OpId, counter span)` for an operation, accounting for paragraph
/// unit expansion on apply. `span` is the number of contiguous counters the operation
/// allocates, so it covers `[max.counter - span + 1, max.counter]`. For a well-formed
/// op all embedded ids share the op's peer; foreign-peer ids are rejected separately by
/// [`check_peer_consistency`].
fn operation_extent(env: &Envelope) -> (OpId, u64) {
    match &env.body {
        OpBody::Doc(DocOp::InsertBlock { id, block, .. }) => {
            let hi = max_counter_in_kind(&block.kind, *id);
            let span = hi.saturating_sub(id.counter).saturating_add(1);
            (
                OpId {
                    counter: hi,
                    peer: id.peer,
                },
                span,
            )
        }
        OpBody::Doc(DocOp::DeleteBlock { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::InsertText { units, .. }) => {
            // Empty InsertText should not appear on the wire; treat as span 1 with peer-0 id 0
            // only for defensive extent — producers never emit empty InsertText.
            let Some(first) = units.first() else {
                return (
                    OpId {
                        counter: 0,
                        peer: 0,
                    },
                    1,
                );
            };
            let mut hi = first.id.counter;
            let peer = first.id.peer;
            let mut lo = first.id.counter;
            for u in units {
                hi = hi.max(u.id.counter);
                lo = lo.min(u.id.counter);
            }
            let span = hi.saturating_sub(lo).saturating_add(1);
            (OpId { counter: hi, peer }, span)
        }
        OpBody::Doc(DocOp::DeleteText { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::SplitBlock { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::MergeBlocks { id, units, .. }) => {
            let hi = units
                .iter()
                .filter(|unit| unit.id != unit.source_id && unit.id.peer == id.peer)
                .map(|unit| unit.id.counter)
                .fold(id.counter, u64::max);
            (
                OpId {
                    counter: hi,
                    peer: id.peer,
                },
                hi.saturating_sub(id.counter).saturating_add(1),
            )
        }
        OpBody::Doc(
            DocOp::InsertTableRow { id, .. }
            | DocOp::SetTableRowCells { id, .. }
            | DocOp::DeleteTableRow { id, .. },
        ) => (*id, 1),
    }
}

/// Highest counter that `kind_from_skeleton` assigns when expanding `kind` under
/// `parent`. A paragraph seeds units at `parent.counter + 1 ..= parent.counter + G`.
fn max_counter_in_kind(kind: &BlockKindSkeleton, parent: OpId) -> u64 {
    match kind {
        BlockKindSkeleton::Paragraph { text } | BlockKindSkeleton::Heading { text, .. } => {
            parent.counter.saturating_add(grapheme_count(text) as u64)
        }
        BlockKindSkeleton::BlockQuote { children } => {
            let mut hi = parent.counter;
            for child in children {
                hi = hi
                    .max(child.id.counter)
                    .max(max_counter_in_kind(&child.block.kind, child.id));
            }
            hi
        }
        BlockKindSkeleton::List { items, .. } => {
            let mut hi = parent.counter;
            for item in items {
                hi = hi.max(item.id.counter);
                for child in &item.children {
                    hi = hi
                        .max(child.id.counter)
                        .max(max_counter_in_kind(&child.block.kind, child.id));
                }
            }
            hi
        }
        BlockKindSkeleton::CodeFence { .. }
        | BlockKindSkeleton::RawBlock { .. }
        | BlockKindSkeleton::Table { .. } => parent.counter,
    }
}

fn check_peer_consistency(op: &Operation, env: &Envelope) -> Result<(), SessionError> {
    let peer = op.id.peer;
    match &env.body {
        OpBody::Doc(DocOp::InsertBlock { id, block, .. }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
            check_kind_peers(peer, &block.kind)?;
        }
        OpBody::Doc(DocOp::DeleteBlock { id, .. }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
        OpBody::Doc(DocOp::InsertText { units, .. }) => {
            for u in units {
                if u.id.peer != peer {
                    return Err(SessionError::PeerMismatch);
                }
            }
        }
        OpBody::Doc(DocOp::DeleteText { id, .. }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
        OpBody::Doc(DocOp::SplitBlock { .. }) => {}
        OpBody::Doc(DocOp::MergeBlocks { units, .. }) => {
            if units
                .iter()
                .any(|unit| unit.id != unit.source_id && unit.id.peer != peer)
            {
                return Err(SessionError::PeerMismatch);
            }
        }
        OpBody::Doc(
            DocOp::InsertTableRow { id, .. }
            | DocOp::SetTableRowCells { id, .. }
            | DocOp::DeleteTableRow { id, .. },
        ) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
    }
    Ok(())
}

fn check_kind_peers(peer: PeerId, kind: &BlockKindSkeleton) -> Result<(), SessionError> {
    match kind {
        BlockKindSkeleton::BlockQuote { children } => {
            for child in children {
                if child.id.peer != peer {
                    return Err(SessionError::PeerMismatch);
                }
                check_kind_peers(peer, &child.block.kind)?;
            }
        }
        BlockKindSkeleton::List { items, .. } => {
            for item in items {
                if item.id.peer != peer {
                    return Err(SessionError::PeerMismatch);
                }
                for child in &item.children {
                    if child.id.peer != peer {
                        return Err(SessionError::PeerMismatch);
                    }
                    check_kind_peers(peer, &child.block.kind)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn block_kind_to_skeleton(
    kind: &BlockKind,
    unit_mode: bool,
) -> Result<BlockKindSkeleton, SessionError> {
    match kind {
        BlockKind::Paragraph { text } => Ok(BlockKindSkeleton::Paragraph {
            text: if unit_mode {
                String::new()
            } else {
                paragraph_visible_string(text)
            },
        }),
        BlockKind::Heading { level, text } => Ok(BlockKindSkeleton::Heading {
            level: *level,
            text: if unit_mode {
                String::new()
            } else {
                paragraph_visible_string(text)
            },
        }),
        BlockKind::List { ordered, items } => {
            let mut wire_items = Vec::new();
            for elem in items.iter_all() {
                if let Some(item) = elem.value.as_ref() {
                    let mut children = Vec::new();
                    for ce in item.children.iter_all() {
                        if let Some(child) = ce.value.as_ref() {
                            children.push(BlockSkeletonInsert {
                                after: ce.after,
                                id: ce.id,
                                right_origin: ce.right_origin,
                                block: BlockSkeleton {
                                    block_id: child.id,
                                    kind: block_kind_to_skeleton(&child.kind, unit_mode)?,
                                },
                            });
                        }
                    }
                    wire_items.push(ListItemSkeleton {
                        after: elem.after,
                        id: elem.id,
                        right_origin: elem.right_origin,
                        block_id: item.id,
                        children,
                    });
                }
            }
            Ok(BlockKindSkeleton::List {
                ordered: *ordered,
                items: wire_items,
            })
        }
        BlockKind::CodeFence { info, text } => Ok(BlockKindSkeleton::CodeFence {
            info: info.clone(),
            text: text.clone(),
        }),
        BlockKind::RawBlock { raw } => Ok(BlockKindSkeleton::RawBlock { raw: raw.clone() }),
        BlockKind::BlockQuote { children } => {
            let mut wire_children = Vec::new();
            for elem in children.iter_all() {
                if let Some(child) = elem.value.as_ref() {
                    wire_children.push(BlockSkeletonInsert {
                        after: elem.after,
                        id: elem.id,
                        right_origin: elem.right_origin,
                        block: BlockSkeleton {
                            block_id: child.id,
                            kind: block_kind_to_skeleton(&child.kind, unit_mode)?,
                        },
                    });
                }
            }
            Ok(BlockKindSkeleton::BlockQuote {
                children: wire_children,
            })
        }
        BlockKind::Table { table } => {
            if table.rows.iter().next().is_some() {
                return Err(SessionError::NonEmptyTableOnInsertBlock);
            }
            Ok(BlockKindSkeleton::Table {
                columns: table
                    .columns
                    .get()
                    .into_iter()
                    .map(|column| alignment_to_wire(&column.alignment))
                    .collect(),
                header: table.header.get(),
            })
        }
    }
}

fn apply_envelope_to_document(document: &mut Document, envelope: &Envelope) {
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock {
            parent,
            after,
            id,
            right_origin,
            block,
        }) => {
            let value = block_from_skeleton(block, *id);
            document.insert_block_at(*parent, *after, *id, value, *right_origin);
        }
        OpBody::Doc(DocOp::DeleteBlock { parent, target, id }) => {
            document.delete_block_at(*parent, *target, *id);
        }
        OpBody::Doc(DocOp::InsertText {
            block_elem, units, ..
        }) => {
            // block_elem may be nested inside a blockquote; search the whole tree.
            let _ = document.with_block_mut(*block_elem, |block| {
                let Some(body) = crate::doc::block_text_seq_mut(&mut block.kind) else {
                    return;
                };
                for u in units {
                    body.apply(SequenceOp::Insert {
                        after: u.after,
                        id: u.id,
                        value: TextUnit {
                            grapheme: u.grapheme.clone(),
                        },
                        right_origin: u.right_origin,
                    });
                }
            });
        }
        OpBody::Doc(DocOp::DeleteText {
            block_elem,
            id,
            targets,
            ..
        }) => {
            let _ = document.with_block_mut(*block_elem, |block| {
                let Some(body) = crate::doc::block_text_seq_mut(&mut block.kind) else {
                    return;
                };
                for target in targets {
                    body.apply(SequenceOp::Delete {
                        target: *target,
                        id: *id,
                    });
                }
            });
        }
        OpBody::Doc(DocOp::SplitBlock {
            parent,
            target,
            id,
            new_block_id,
            right_origin,
            kind,
            units,
        }) => {
            let marks = document.with_block_mut(*target, |block| {
                let marks = block.marks.clone();
                if let Some(body) = crate::doc::block_text_seq_mut(&mut block.kind) {
                    for unit in units {
                        body.apply(SequenceOp::Delete {
                            target: unit.source_id,
                            id: *id,
                        });
                    }
                }
                marks
            });
            if let Some(marks) = marks {
                let body = Sequence::from_ordered(
                    units
                        .iter()
                        .map(|unit| {
                            (
                                unit.id,
                                TextUnit {
                                    grapheme: unit.grapheme.clone(),
                                },
                            )
                        })
                        .collect(),
                );
                let block_kind = match kind {
                    TextBlockKindWire::Paragraph => BlockKind::Paragraph { text: body },
                    TextBlockKindWire::Heading { level } => BlockKind::Heading {
                        level: *level,
                        text: body,
                    },
                };
                let block = Block {
                    id: *new_block_id,
                    elem_id: *id,
                    kind: block_kind,
                    marks,
                };
                document.insert_block_at(*parent, Some(*target), *id, block, *right_origin);
            }
        }
        OpBody::Doc(DocOp::MergeBlocks {
            parent,
            left,
            right,
            id,
            after,
            right_origin,
            units,
        }) => {
            let right_marks = document.find_block(*right).map(|block| block.marks.clone());
            let _ = document.with_block_mut(*left, |block| {
                if let Some(body) = crate::doc::block_text_seq_mut(&mut block.kind) {
                    let mut anchor = *after;
                    for (index, unit) in units.iter().enumerate() {
                        body.apply(SequenceOp::Insert {
                            after: anchor,
                            id: unit.id,
                            value: TextUnit {
                                grapheme: unit.grapheme.clone(),
                            },
                            right_origin: if index == 0 { *right_origin } else { None },
                        });
                        anchor = Some(unit.id);
                    }
                }
                if let Some(marks) = &right_marks {
                    block.marks.merge_from(marks);
                }
            });
            document.delete_block_at(*parent, *right, *id);
        }
        OpBody::Doc(DocOp::InsertTableRow {
            table_elem,
            after,
            id,
            right_origin,
            cells,
            ..
        }) => {
            let _ = document.with_block_mut(*table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    let row = crate::doc::TableRow {
                        id: block_id_from_op(*id),
                        elem_id: *id,
                        deleted: crate::core::LwwRegister::new(false, *id),
                        cells: crate::core::LwwRegister::new(cells.clone(), *id),
                    };
                    table.rows.apply(SequenceOp::Insert {
                        after: *after,
                        id: *id,
                        value: row,
                        right_origin: *right_origin,
                    });
                }
            });
        }
        OpBody::Doc(DocOp::SetTableRowCells {
            table_elem,
            row,
            id,
            cells,
            ..
        }) => {
            let _ = document.with_block_mut(*table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    table.set_row_cells(*row, cells.clone(), *id);
                }
            });
        }
        OpBody::Doc(DocOp::DeleteTableRow {
            table_elem,
            target,
            id,
            ..
        }) => {
            let _ = document.with_block_mut(*table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    table.remove_row(*target, *id);
                }
            });
        }
    }
}

fn block_from_skeleton(skel: &BlockSkeleton, elem_id: OpId) -> Block {
    Block {
        id: skel.block_id,
        elem_id,
        kind: kind_from_skeleton(&skel.kind, elem_id),
        marks: MarkSet::new(),
    }
}

fn kind_from_skeleton(kind: &BlockKindSkeleton, parent_elem: OpId) -> BlockKind {
    match kind {
        BlockKindSkeleton::Paragraph { text } => {
            // Deterministic unit ids after the block elem (same on every peer).
            let mut counter = parent_elem.counter.saturating_add(1);
            BlockKind::Paragraph {
                text: units_from_str(text, &mut counter, parent_elem.peer),
            }
        }
        BlockKindSkeleton::Heading { level, text } => {
            let mut counter = parent_elem.counter.saturating_add(1);
            BlockKind::Heading {
                level: *level,
                text: units_from_str(text, &mut counter, parent_elem.peer),
            }
        }
        BlockKindSkeleton::List { ordered, items } => {
            let mut seq = Sequence::new();
            for item in items {
                let mut child_seq = Sequence::new();
                for child in &item.children {
                    let block = Block {
                        id: child.block.block_id,
                        elem_id: child.id,
                        kind: kind_from_skeleton(&child.block.kind, child.id),
                        marks: MarkSet::new(),
                    };
                    child_seq.apply(SequenceOp::Insert {
                        after: child.after,
                        id: child.id,
                        value: block,
                        right_origin: child.right_origin,
                    });
                }
                let list_item = ListItem {
                    id: item.block_id,
                    elem_id: item.id,
                    children: child_seq,
                };
                seq.apply(SequenceOp::Insert {
                    after: item.after,
                    id: item.id,
                    value: list_item,
                    right_origin: item.right_origin,
                });
            }
            BlockKind::List {
                ordered: *ordered,
                items: seq,
            }
        }
        BlockKindSkeleton::CodeFence { info, text } => BlockKind::CodeFence {
            info: info.clone(),
            text: text.clone(),
        },
        BlockKindSkeleton::RawBlock { raw } => BlockKind::RawBlock { raw: raw.clone() },
        BlockKindSkeleton::BlockQuote { children } => {
            let mut seq = Sequence::new();
            for child in children {
                let block = Block {
                    id: child.block.block_id,
                    elem_id: child.id,
                    kind: kind_from_skeleton(&child.block.kind, child.id),
                    marks: MarkSet::new(),
                };
                seq.apply(SequenceOp::Insert {
                    after: child.after,
                    id: child.id,
                    value: block,
                    right_origin: child.right_origin,
                });
            }
            BlockKind::BlockQuote { children: seq }
        }
        BlockKindSkeleton::Table { columns, header } => BlockKind::Table {
            table: Table::new(
                block_id_from_op(parent_elem),
                parent_elem,
                columns
                    .iter()
                    .map(|alignment| ColumnDef {
                        alignment: alignment_from_wire(*alignment),
                    })
                    .collect(),
                header.clone(),
                parent_elem,
            ),
        },
    }
}

fn alignment_to_wire(alignment: &ColumnAlignment) -> ColumnAlignmentWire {
    match alignment {
        ColumnAlignment::Left => ColumnAlignmentWire::Left,
        ColumnAlignment::Center => ColumnAlignmentWire::Center,
        ColumnAlignment::Right => ColumnAlignmentWire::Right,
    }
}

fn alignment_from_wire(alignment: ColumnAlignmentWire) -> ColumnAlignment {
    match alignment {
        ColumnAlignmentWire::Left => ColumnAlignment::Left,
        ColumnAlignmentWire::Center => ColumnAlignment::Center,
        ColumnAlignmentWire::Right => ColumnAlignment::Right,
    }
}
