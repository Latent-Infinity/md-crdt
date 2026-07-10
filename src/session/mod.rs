//! Collaborative session: document + sync log + peer clock.
//!
//! Owns encode-before-apply local commits and pre-decode remote apply.
//! Payload-opaque [`crate::sync::SyncState`] never sees codec types.

use crate::codec::{
    BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert, DocOp, Envelope, JsonOpCodec, OpBody,
    OpCodec, WIRE_VERSION, insert_block_paragraph_is_empty,
};
use crate::core::MarkSet;
use crate::core::{OpId, PeerId, Sequence, SequenceOp, StateVector};
use crate::doc::{Block, BlockKind, Document, block_id_from_op};
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
    #[error("unsupported block kind on the collaborative wire: {0}")]
    UnsupportedBlockKind(&'static str),
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
        Self::with_codec(peer, JsonOpCodec, false)
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

    /// Insert a block after `after` (None = start). Returns the block `elem_id`.
    pub fn insert_block(
        &mut self,
        after: Option<OpId>,
        kind: BlockKind,
    ) -> Result<OpId, SessionError> {
        if let Some(anchor) = after {
            if self.document.blocks.get_element(&anchor).is_none() {
                return Err(SessionError::MissingAfterAnchor);
            }
        }

        let b = self.next_counter;
        let block_elem = OpId {
            peer: self.peer,
            counter: b,
        };
        let block_id = block_id_from_op(block_elem);
        let right_origin = self.document.blocks.compute_right_origin(after);
        let skeleton = block_kind_to_skeleton(&kind, self.unit_mode)?;
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertBlock {
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

        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        // Apply to document before advancing clock / logging (N3).
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation {
            id: block_elem,
            payload,
        });
        self.next_counter = b + 1;
        Ok(block_elem)
    }

    /// Delete block element `target`. Returns the delete-op id.
    pub fn delete_block(&mut self, target: OpId) -> Result<OpId, SessionError> {
        if self.document.blocks.get_element(&target).is_none() {
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
            check_operation_id_is_max(&op, &env)?;
            check_peer_consistency(&op, &env)?;
            if self.unit_mode && !insert_block_paragraph_is_empty(&env) {
                return Err(SessionError::NonEmptyParagraphOnInsertBlock);
            }
            prepared.push((op, env));
        }

        let mut result = SessionApplyResult::default();
        for (op, env) in prepared {
            let id = op.id;
            match self.sync.apply_one(op) {
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
}

fn check_operation_id_is_max(op: &Operation, env: &Envelope) -> Result<(), SessionError> {
    let max = max_opid_in_envelope(env);
    if op.id != max {
        return Err(SessionError::OperationIdMismatch);
    }
    Ok(())
}

fn max_opid_in_envelope(env: &Envelope) -> OpId {
    match &env.body {
        OpBody::Doc(DocOp::InsertBlock { id, block, .. }) => {
            max_opid(*id, max_opid_in_kind(&block.kind))
        }
        OpBody::Doc(DocOp::DeleteBlock { id, .. }) => *id,
    }
}

fn max_opid(a: OpId, b: Option<OpId>) -> OpId {
    match b {
        Some(other) if other > a => other,
        _ => a,
    }
}

fn max_opid_in_kind(kind: &BlockKindSkeleton) -> Option<OpId> {
    match kind {
        BlockKindSkeleton::BlockQuote { children } => {
            let mut max: Option<OpId> = None;
            for child in children {
                let candidate = max_opid(child.id, max_opid_in_kind(&child.block.kind));
                max = Some(match max {
                    Some(m) if m >= candidate => m,
                    _ => candidate,
                });
            }
            max
        }
        _ => None,
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
        OpBody::Doc(DocOp::DeleteBlock { id, target: _ }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
    }
    Ok(())
}

fn check_kind_peers(peer: PeerId, kind: &BlockKindSkeleton) -> Result<(), SessionError> {
    if let BlockKindSkeleton::BlockQuote { children } = kind {
        for child in children {
            if child.id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
            check_kind_peers(peer, &child.block.kind)?;
        }
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
                text.clone()
            },
        }),
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
        // Tables are not wire-ready until the table PR. Reject rather than silently
        // degrade to an empty block, which would drop the table from both the local
        // document and the wire.
        BlockKind::Table { .. } => Err(SessionError::UnsupportedBlockKind("table")),
    }
}

fn apply_envelope_to_document(document: &mut Document, envelope: &Envelope) {
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock {
            after,
            id,
            right_origin,
            block,
        }) => {
            let value = block_from_skeleton(block, *id);
            document.blocks.apply(SequenceOp::Insert {
                after: *after,
                id: *id,
                value,
                right_origin: *right_origin,
            });
        }
        OpBody::Doc(DocOp::DeleteBlock { target, id }) => {
            document.blocks.apply(SequenceOp::Delete {
                target: *target,
                id: *id,
            });
        }
    }
}

fn block_from_skeleton(skel: &BlockSkeleton, elem_id: OpId) -> Block {
    Block {
        id: skel.block_id,
        elem_id,
        kind: kind_from_skeleton(&skel.kind),
        marks: MarkSet::new(),
    }
}

fn kind_from_skeleton(kind: &BlockKindSkeleton) -> BlockKind {
    match kind {
        BlockKindSkeleton::Paragraph { text } => BlockKind::Paragraph { text: text.clone() },
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
                    kind: kind_from_skeleton(&child.block.kind),
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
    }
}
