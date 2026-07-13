//! Collaborative session: document + sync log + peer clock.
//!
//! Owns encode-before-apply local commits and pre-decode remote apply.
//! Payload-opaque [`crate::sync::SyncState`] never sees codec types.

pub mod snapshot;
mod wire;

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
use wire::*;

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
        self.sync.add_local_op(Operation {
            id: op_id,
            payload: payload.into(),
        });
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
            payload: payload.into(),
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
        self.sync.add_local_op(Operation {
            id,
            payload: payload.into(),
        });
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
        self.sync.add_local_op(Operation {
            id: op_id,
            payload: payload.into(),
        });
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
            payload: payload.into(),
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
        self.sync.add_local_op(Operation {
            id: op_id,
            payload: payload.into(),
        });
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
            .map(|op| (op.id, op.payload.to_vec()))
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
                (
                    Operation {
                        id,
                        payload: payload.into(),
                    },
                    span,
                )
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
                (
                    Operation {
                        id,
                        payload: payload.into(),
                    },
                    span,
                )
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
