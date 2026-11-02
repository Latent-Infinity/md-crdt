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
    JsonOpCodec, ListItemSkeleton, MovedBlockWire, MovedTextUnitWire, OpBody, OpCodec,
    TableCellWire, TextBlockKindWire, TextUnitWire, WIRE_VERSION, insert_block_paragraph_is_empty,
};
use crate::core::mark::{MarkKind, MarkSet, MarkValue};
use crate::core::{OpId, PeerId, Sequence, SequenceOp, StateVector};
use crate::doc::{
    Block, BlockId, BlockKind, ColumnAlignment, ColumnDef, ColumnId, Document, ListItem, RowId,
    Table, TextUnit, after_for_grapheme_offset, block_id_from_op, grapheme_count,
    paragraph_visible_ids, paragraph_visible_string, units_from_str,
};
use crate::sync::{
    ChangeMessage, CheckpointError, CheckpointReport, CheckpointRequest, Operation, RebaseRequired,
    SyncState, ValidationError, ValidationLimits,
};
use crate::workspace::{
    BlockDraft, ListItemDraft, StructuredEditError, StructuredEditLimits, TextBlockKind,
    validate_code_fence, validate_list_style,
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
    WireCodec(#[from] crate::codec::CodecError),
    #[error("local operation counter is exhausted")]
    CounterExhausted,
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
    #[error("table InsertBlock must be empty; use table row/column operations")]
    NonEmptyTableOnInsertBlock,
    #[error("block not found")]
    BlockNotFound,
    #[error("target is not a paragraph")]
    NotParagraph,
    #[error("target is not a table")]
    NotTable,
    #[error("table row not found")]
    TableRowNotFound,
    #[error("table column not found")]
    TableColumnNotFound,
    #[error("table row/header cell count must match the live column count")]
    InvalidTableShape,
    #[error("table cells must be single-line text")]
    InvalidTableCell,
    #[error("blocks must be adjacent siblings")]
    BlocksNotAdjacent,
    #[error("grapheme offset out of range")]
    InvalidOffset,
    #[error("heading level must be in 1..=6")]
    InvalidHeadingLevel,
    #[error("move target/range or destination anchor is invalid")]
    InvalidMove,
    #[error("a block cannot be moved into itself or its descendants")]
    MoveCycle,
    #[error(transparent)]
    StructuredEdit(#[from] StructuredEditError),
    #[error("target is not a list")]
    NotList,
    #[error("list item not found")]
    ListItemNotFound,
    #[error("target is not a code fence")]
    NotCodeFence,
    #[error("target is not an opaque raw block")]
    NotRawBlock,
    #[error("raw block digest precondition does not match")]
    RawDigestMismatch,
    #[error(transparent)]
    Frontmatter(#[from] crate::doc::FrontmatterError),
}

fn codec_err<E: std::error::Error + 'static>(e: E) -> SessionError {
    if let Some(error) = (&e as &dyn std::error::Error).downcast_ref::<crate::codec::CodecError>() {
        return match error {
            crate::codec::CodecError::UnknownVersion(version) => {
                SessionError::UnknownWireVersion(*version)
            }
            error => SessionError::WireCodec(error.clone()),
        };
    }
    SessionError::Codec(e.to_string())
}

fn validate_table_cell(value: &str) -> Result<(), SessionError> {
    if value.contains(['\r', '\n']) {
        return Err(SessionError::InvalidTableCell);
    }
    Ok(())
}

fn validate_table_cells(values: &[String]) -> Result<(), SessionError> {
    values
        .iter()
        .try_for_each(|value| validate_table_cell(value))
}

fn validate_block_skeleton(kind: &BlockKindSkeleton) -> Result<(), SessionError> {
    match kind {
        BlockKindSkeleton::Paragraph { .. } | BlockKindSkeleton::RawBlock { .. } => Ok(()),
        BlockKindSkeleton::Heading { level, .. } => {
            if (1..=6).contains(level) {
                Ok(())
            } else {
                Err(SessionError::InvalidHeadingLevel)
            }
        }
        BlockKindSkeleton::List { style, items } => {
            validate_list_style(*style)?;
            for item in items {
                for child in &item.children {
                    validate_block_skeleton(&child.block.kind)?;
                }
            }
            Ok(())
        }
        BlockKindSkeleton::CodeFence { style, info, text } => {
            validate_code_fence(*style, info.as_deref(), text)?;
            Ok(())
        }
        BlockKindSkeleton::BlockQuote { children } => {
            for child in children {
                validate_block_skeleton(&child.block.kind)?;
            }
            Ok(())
        }
        BlockKindSkeleton::Table => Ok(()),
    }
}

fn envelope_observed_frontier(envelope: &Envelope) -> Option<&StateVector> {
    match &envelope.body {
        OpBody::Doc(
            DocOp::RemoveMark { observed, .. }
            | DocOp::MoveBlocks { observed, .. }
            | DocOp::DeleteBlock { observed, .. }
            | DocOp::DeleteBlockById { observed, .. }
            | DocOp::DeleteListItemById { observed, .. }
            | DocOp::SetTableCell { observed, .. }
            | DocOp::SetTableColumnAlignment { observed, .. }
            | DocOp::MoveTableRow { observed, .. }
            | DocOp::MoveTableColumn { observed, .. }
            | DocOp::MoveListItem { observed, .. }
            | DocOp::SetListStyle { observed, .. }
            | DocOp::SetListItemTask { observed, .. }
            | DocOp::SetCodeFence { observed, .. }
            | DocOp::ConvertTextBlock { observed, .. }
            | DocOp::ReplaceRawBlock { observed, .. },
        ) => Some(observed),
        _ => None,
    }
}

fn draft_counter_count(draft: &BlockDraft) -> Option<u64> {
    match draft {
        BlockDraft::Paragraph { text } | BlockDraft::Heading { text, .. } => {
            1u64.checked_add(grapheme_count(text) as u64)
        }
        BlockDraft::BlockQuote { children } => children.iter().try_fold(1u64, |count, child| {
            count.checked_add(draft_counter_count(child)?)
        }),
        BlockDraft::List { items, .. } => items.iter().try_fold(1u64, |count, item| {
            item.children
                .iter()
                .try_fold(count.checked_add(1)?, |count, child| {
                    count.checked_add(draft_counter_count(child)?)
                })
        }),
        BlockDraft::CodeFence { .. } | BlockDraft::RawBlock { .. } => Some(1),
    }
}

/// Result of applying a remote change message at the session layer.
#[derive(Debug, Clone, Default)]
pub struct SessionApplyResult {
    pub applied: Vec<OpId>,
    pub buffered: Vec<OpId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncResponse {
    Delta(ChangeMessage),
    Rebase { checkpoint: Box<SessionSnapshot> },
}

struct PreparedOperation {
    id: OpId,
    raw_bytes: usize,
    op: Option<Operation>,
    envelope: Envelope,
    ready: bool,
    staged_block: Option<Block>,
    charge: usize,
    folded: bool,
    committed_operations: usize,
    discarded: bool,
}

#[derive(Clone)]
struct PendingEnvelope {
    envelope: Envelope,
    decoded_bytes: usize,
    /// JSON snapshot raw charge when larger than the retained transport payload.
    snapshot_raw_bytes: usize,
}

impl PendingEnvelope {
    fn raw_charge(&self, retained: usize) -> usize {
        if retained == 0 {
            0 // Checkpointing removed the raw applied copy.
        } else {
            retained.max(self.snapshot_raw_bytes)
        }
    }
}

struct AdmissionView<'a> {
    received: &'a StateVector,
    materialized: &'a StateVector,
    inserted: &'a AdmissionInsertions,
}

enum PlannedEffect {
    New(usize),
    Held(OpId),
}

fn advance_materialized(frontier: &mut StateVector, envelope: &Envelope) {
    let (end, span) = operation_extent(envelope);
    let start = end.counter.saturating_sub(span.saturating_sub(1));
    if frontier
        .get(end.peer)
        .unwrap_or(0)
        .checked_add(1)
        .is_none_or(|next| start <= next)
    {
        frontier.set(
            end.peer,
            frontier.get(end.peer).unwrap_or(0).max(end.counter),
        );
    }
}

fn validate_retained_resources(
    resources: (usize, usize),
    limits: &ValidationLimits,
) -> Result<(), SessionError> {
    let max_pending = limits
        .max_pending_buffer
        .min(crate::limits::MAX_DURABLE_PENDING_OPERATIONS);
    let max_bytes = limits
        .max_retained_bytes
        .min(crate::limits::MAX_DURABLE_RETAINED_BYTES);
    if resources.0 > max_pending {
        return Err(ValidationError::BufferFull {
            capacity: max_pending,
        }
        .into());
    }
    if resources.1 > max_bytes {
        return Err(ValidationError::ResourceLimitExceeded {
            limit: max_bytes,
            actual: resources.1,
        }
        .into());
    }
    Ok(())
}

/// Document + sync log + local peer clock for collaborative editing.
#[derive(Clone)]
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
    pending_envelopes: BTreeMap<OpId, PendingEnvelope>,
    /// Derived admission fast path; snapshots rebuild it from document pending state.
    has_document_pending: bool,
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
            has_document_pending: false,
        }
    }

    pub fn peer(&self) -> PeerId {
        self.peer
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    #[cfg(feature = "filesync")]
    pub(crate) fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    pub fn state_vector(&self) -> StateVector {
        self.sync.state_vector()
    }

    /// Whether recovery needs a full snapshot to preserve unapplied operations.
    ///
    /// Incremental change messages contain only the applied operation log. An
    /// out-of-order operation buffered by sync, or an applied operation waiting
    /// for its observed frontier, exists only in snapshot state.
    #[cfg(feature = "filesync")]
    pub(crate) fn requires_full_snapshot(&self) -> bool {
        self.sync.pending_count() != 0
            || !self.pending_envelopes.is_empty()
            || self.has_document_pending
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
    pub fn encode_changes_since(
        &self,
        since: &StateVector,
    ) -> Result<ChangeMessage, RebaseRequired> {
        self.sync.encode_changes_since(since)
    }

    pub fn sync_since(&self, since: &StateVector) -> Result<SyncResponse, SnapshotError> {
        match self.encode_changes_since(since) {
            Ok(message) => Ok(SyncResponse::Delta(message)),
            Err(_) => Ok(SyncResponse::Rebase {
                checkpoint: Box::new(self.save_snapshot()?),
            }),
        }
    }

    /// Prove that recovery applies this delta in order without retaining work.
    /// Source rewrites not provable from the post-edit document need a snapshot.
    #[cfg(feature = "filesync")]
    pub(crate) fn delta_replay_admissible(
        &self,
        message: &ChangeMessage,
        limits: &ValidationLimits,
    ) -> Result<bool, SessionError> {
        if crate::sync::validate_message(message, limits).is_err() {
            return Ok(false);
        }
        let mut frontier = message.since.clone();
        let mut inserted = AdmissionInsertions::for_replay(frontier.clone());
        let empty = Document::new();
        let mut work = limits.max_dependency_work;
        for op in &message.ops {
            let envelope = self.decode_remote_envelope(op)?;
            self.validate_remote_envelope(op.id, &envelope)?;
            // The final document cannot prove pre-rewrite sequence membership.
            if matches!(
                &envelope.body,
                OpBody::Doc(
                    DocOp::MoveBlocks { .. }
                        | DocOp::SplitBlock { .. }
                        | DocOp::MergeBlocks { .. }
                        | DocOp::MoveListItem { .. }
                        | DocOp::MoveTableRow { .. }
                        | DocOp::MoveTableColumn { .. }
                        | DocOp::DeleteBlock { .. }
                        | DocOp::DeleteBlockById { .. }
                        | DocOp::DeleteTableRow { .. }
                        | DocOp::DeleteTableRowById { .. }
                        | DocOp::DeleteTableColumnById { .. }
                        | DocOp::DeleteListItemById { .. }
                        | DocOp::SetCodeFence { .. }
                        | DocOp::ConvertTextBlock { .. }
                        | DocOp::ReplaceRawBlock { .. }
                )
            ) {
                return Ok(false);
            }
            let (_, span) = operation_extent(&envelope);
            let start = op.id.counter.saturating_sub(span.saturating_sub(1));
            let cost = envelope_dependency_work(&envelope);
            if !frontier
                .get(op.id.peer)
                .unwrap_or(0)
                .checked_add(1)
                .is_some_and(|next| start <= next)
                || cost > work
                || !envelope_dependencies_ready(&envelope, &frontier, &empty)
                || envelope_may_buffer_in_document(&self.document, &envelope, &inserted)
            {
                return Ok(false);
            }
            let staged = stage_nested_block(&envelope);
            if staged_pending_count(staged.as_ref()) != 0 {
                return Ok(false);
            }
            work -= cost;
            if inserted
                .record(&self.document, &envelope, staged.as_ref())
                .is_err()
            {
                return Ok(false);
            }
            frontier.set(
                op.id.peer,
                frontier.get(op.id.peer).unwrap_or(0).max(op.id.counter),
            );
        }
        Ok(true)
    }

    pub fn checkpoint_history(
        &mut self,
        request: &CheckpointRequest,
    ) -> Result<CheckpointReport, CheckpointError> {
        self.sync.checkpoint(request)
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
        validate_block_skeleton(&skeleton)?;
        check_kind_peers(self.peer, &skeleton)?;
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
        let next = op_id
            .counter
            .checked_add(1)
            .ok_or(SessionError::CounterExhausted)?;
        validate_operation_envelope(op_id, &envelope)?;
        let staged = stage_nested_block(&envelope);
        validate_identity_declarations(
            &self.document,
            self.sync.frontier(),
            self.has_document_pending,
            self.pending_envelopes
                .values()
                .map(|pending| &pending.envelope),
            std::iter::once((op_id, &envelope)),
        )?;
        AdmissionInsertions::default().record(&self.document, &envelope, staged.as_ref())?;
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        // Apply to document before advancing clock / logging (N3).
        self.apply_document_effect(&envelope, staged);
        self.sync.add_local_op(Operation {
            id: op_id,
            payload: payload.into(),
        });
        // Advance past the whole reserved range so later ids never collide with the units.
        self.next_counter = next;
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
        let block_id = self
            .document
            .container_children(parent)
            .and_then(|children| children.get_element(&target))
            .and_then(|element| element.value.as_ref())
            .map(|block| block.id)
            .ok_or(SessionError::MissingDeleteTarget)?;

        let b = self.next_counter;
        let delete_id = OpId {
            peer: self.peer,
            counter: b,
        };
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::DeleteBlockById {
                parent,
                target,
                block_id,
                id: delete_id,
                observed: self.materialized_frontier().into_owned(),
                subtree: self
                    .document
                    .logical_subtree(crate::doc::LogicalContainer::Block(block_id)),
            }),
        };
        let next = b.checked_add(1).ok_or(SessionError::CounterExhausted)?;
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        self.apply_document_envelope(&envelope);
        self.sync.add_local_op(Operation {
            id: delete_id,
            payload: payload.into(),
        });
        self.next_counter = next;
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
        self.next_counter
            .checked_add(1)
            .and_then(|next| next.checked_add(grapheme_count(text) as u64))
            .ok_or(SessionError::CounterExhausted)?;
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

    /// Insert one validated structured block tree without parsing Markdown fragments.
    pub fn insert_draft_in(
        &mut self,
        parent: Option<OpId>,
        after: Option<OpId>,
        draft: &BlockDraft,
        limits: StructuredEditLimits,
    ) -> Result<OpId, SessionError> {
        draft.validate(limits)?;
        draft_counter_count(draft)
            .and_then(|count| self.next_counter.checked_add(count))
            .ok_or(SessionError::CounterExhausted)?;
        self.insert_validated_draft(parent, after, draft)
    }

    fn insert_validated_draft(
        &mut self,
        parent: Option<OpId>,
        after: Option<OpId>,
        draft: &BlockDraft,
    ) -> Result<OpId, SessionError> {
        match draft {
            BlockDraft::Paragraph { text } => self.insert_paragraph_in(parent, after, text),
            BlockDraft::Heading { level, text } => {
                let elem = self.insert_block_in(
                    parent,
                    after,
                    BlockKind::Heading {
                        level: *level,
                        text: Sequence::new(),
                    },
                )?;
                if !text.is_empty() {
                    self.insert_text(block_id_from_op(elem), 0, text)?;
                }
                Ok(elem)
            }
            BlockDraft::List { style, items } => {
                let list_elem = self.insert_block_in(
                    parent,
                    after,
                    BlockKind::List {
                        style: *style,
                        items: Sequence::new(),
                        pending_moves: Vec::new(),
                    },
                )?;
                let list_id = block_id_from_op(list_elem);
                let mut after_item = None;
                for item in items {
                    let item_elem = self.insert_list_item(list_id, after_item, item.task)?;
                    let mut after_child = None;
                    for child in &item.children {
                        after_child = Some(self.insert_validated_draft(
                            Some(item_elem),
                            after_child,
                            child,
                        )?);
                    }
                    after_item = Some(item_elem);
                }
                Ok(list_elem)
            }
            BlockDraft::CodeFence { style, info, text } => self.insert_block_in(
                parent,
                after,
                BlockKind::CodeFence {
                    style: *style,
                    info: info.clone(),
                    text: text.clone(),
                },
            ),
            BlockDraft::BlockQuote { children } => {
                let quote_elem = self.insert_block_in(
                    parent,
                    after,
                    BlockKind::BlockQuote {
                        children: Sequence::new(),
                    },
                )?;
                let mut after_child = None;
                for child in children {
                    after_child =
                        Some(self.insert_validated_draft(Some(quote_elem), after_child, child)?);
                }
                Ok(quote_elem)
            }
            BlockDraft::RawBlock { raw } => {
                self.insert_block_in(parent, after, BlockKind::RawBlock { raw: raw.clone() })
            }
        }
    }

    pub fn insert_list_item(
        &mut self,
        list_id: BlockId,
        after: Option<OpId>,
        task: Option<crate::doc::TaskState>,
    ) -> Result<OpId, SessionError> {
        let list = self
            .document
            .find_block_by_id(list_id)
            .ok_or(SessionError::BlockNotFound)?;
        let BlockKind::List { items, .. } = &list.kind else {
            return Err(SessionError::NotList);
        };
        if after.is_some_and(|anchor| items.get_element(&anchor).is_none()) {
            return Err(SessionError::MissingAfterAnchor);
        }
        let list_elem = list.elem_id;
        let id = self.peek_next_id();
        let right_origin = items.compute_right_origin(after);
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertListItem {
                list_elem,
                list_id,
                after,
                id,
                right_origin,
                task,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    pub fn insert_list_item_draft(
        &mut self,
        list_id: BlockId,
        after: Option<OpId>,
        item: &ListItemDraft,
        limits: StructuredEditLimits,
    ) -> Result<OpId, SessionError> {
        BlockDraft::List {
            style: crate::doc::ListStyle::default(),
            items: vec![item.clone()],
        }
        .validate(limits)?;
        item.children
            .iter()
            .try_fold(1u64, |count, child| {
                count.checked_add(draft_counter_count(child)?)
            })
            .and_then(|count| self.next_counter.checked_add(count))
            .ok_or(SessionError::CounterExhausted)?;
        let item_elem = self.insert_list_item(list_id, after, item.task)?;
        let mut after_child = None;
        for child in &item.children {
            after_child = Some(self.insert_validated_draft(Some(item_elem), after_child, child)?);
        }
        Ok(item_elem)
    }

    pub fn delete_list_item(&mut self, item_id: BlockId) -> Result<OpId, SessionError> {
        let (_, list_elem) = self
            .document
            .list_containing_item(item_id)
            .ok_or(SessionError::ListItemNotFound)?;
        let target = self
            .document
            .list_item_placement(item_id)
            .ok_or(SessionError::ListItemNotFound)?
            .2;
        let list_id = self
            .document
            .find_block(list_elem)
            .ok_or(SessionError::NotList)?
            .id;
        let id = self.peek_next_id();
        self.commit_single_id(
            Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::DeleteListItemById {
                    list_elem,
                    list_id,
                    target,
                    item_id,
                    id,
                    observed: self.materialized_frontier().into_owned(),
                    subtree: self
                        .document
                        .logical_subtree(crate::doc::LogicalContainer::Item(item_id)),
                }),
            },
            id,
        )
    }

    pub fn move_list_item(
        &mut self,
        item_id: BlockId,
        list_id: BlockId,
        after: Option<OpId>,
    ) -> Result<OpId, SessionError> {
        if self.document.list_item_contains_block(item_id, list_id) {
            return Err(SessionError::MoveCycle);
        }
        let (_, from_list_elem) = self
            .document
            .list_containing_item(item_id)
            .ok_or(SessionError::ListItemNotFound)?;
        let target = self
            .document
            .list_item_placement(item_id)
            .ok_or(SessionError::ListItemNotFound)?
            .2;
        let destination = self
            .document
            .find_block_by_id(list_id)
            .ok_or(SessionError::BlockNotFound)?;
        let BlockKind::List { items, .. } = &destination.kind else {
            return Err(SessionError::NotList);
        };
        if after.is_some_and(|anchor| items.get_element(&anchor).is_none()) || after == Some(target)
        {
            return Err(SessionError::InvalidMove);
        }
        let to_list_elem = destination.elem_id;
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        let right_origin = items.compute_right_origin(after);
        self.commit_single_id(
            Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::MoveListItem {
                    from_list_elem,
                    to_list_elem,
                    list_id,
                    item_id,
                    target,
                    id,
                    after,
                    right_origin,
                    observed,
                }),
            },
            id,
        )
    }

    pub fn set_list_style(
        &mut self,
        list_id: BlockId,
        style: crate::doc::ListStyle,
    ) -> Result<OpId, SessionError> {
        validate_list_style(style)?;
        let block = self
            .document
            .find_block_by_id(list_id)
            .ok_or(SessionError::BlockNotFound)?;
        if !matches!(block.kind, BlockKind::List { .. }) {
            return Err(SessionError::NotList);
        }
        let block_elem = block.elem_id;
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        self.commit_single_id(
            Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::SetListStyle {
                    block_elem,
                    block_id: list_id,
                    id,
                    style,
                    observed,
                }),
            },
            id,
        )
    }

    pub fn set_list_item_task(
        &mut self,
        item_id: BlockId,
        task: Option<crate::doc::TaskState>,
    ) -> Result<OpId, SessionError> {
        self.document
            .find_list_item_by_id(item_id)
            .ok_or(SessionError::ListItemNotFound)?;
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        self.commit_single_id(
            Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::SetListItemTask {
                    item_id,
                    id,
                    task,
                    observed,
                }),
            },
            id,
        )
    }

    pub fn set_code_fence(
        &mut self,
        block_id: BlockId,
        style: crate::doc::CodeFenceStyle,
        info: Option<String>,
        text: String,
    ) -> Result<OpId, SessionError> {
        validate_code_fence(style, info.as_deref(), &text)?;
        let block = self
            .document
            .find_block_by_id(block_id)
            .ok_or(SessionError::BlockNotFound)?;
        if !matches!(block.kind, BlockKind::CodeFence { .. }) {
            return Err(SessionError::NotCodeFence);
        }
        let block_elem = block.elem_id;
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        self.commit_single_id(
            Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::SetCodeFence {
                    block_elem,
                    block_id,
                    id,
                    style,
                    info,
                    text,
                    observed,
                }),
            },
            id,
        )
    }

    pub fn convert_text_block(
        &mut self,
        block_id: BlockId,
        kind: TextBlockKind,
    ) -> Result<OpId, SessionError> {
        let block = self
            .document
            .find_block_by_id(block_id)
            .ok_or(SessionError::BlockNotFound)?;
        if !matches!(
            block.kind,
            BlockKind::Paragraph { .. } | BlockKind::Heading { .. }
        ) {
            return Err(SessionError::NotParagraph);
        }
        let kind = match kind {
            TextBlockKind::Paragraph => TextBlockKindWire::Paragraph,
            TextBlockKind::Heading { level } if (1..=6).contains(&level) => {
                TextBlockKindWire::Heading { level }
            }
            TextBlockKind::Heading { .. } => return Err(SessionError::InvalidHeadingLevel),
        };
        let block_elem = block.elem_id;
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        self.commit_single_id(
            Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::ConvertTextBlock {
                    block_elem,
                    block_id,
                    id,
                    kind,
                    observed,
                }),
            },
            id,
        )
    }

    pub fn replace_raw_block(
        &mut self,
        block_id: BlockId,
        raw: String,
    ) -> Result<OpId, SessionError> {
        let block = self
            .document
            .find_block_by_id(block_id)
            .ok_or(SessionError::BlockNotFound)?;
        if !matches!(block.kind, BlockKind::RawBlock { .. }) {
            return Err(SessionError::NotRawBlock);
        }
        let block_elem = block.elem_id;
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        self.commit_single_id(
            Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::ReplaceRawBlock {
                    block_elem,
                    block_id,
                    id,
                    raw,
                    observed,
                }),
            },
            id,
        )
    }

    /// Insert an empty table block. Rows are added with [`Self::insert_table_row`].
    pub fn insert_table(
        &mut self,
        after: Option<OpId>,
        columns: Vec<ColumnDef>,
        header: Vec<String>,
    ) -> Result<OpId, SessionError> {
        self.insert_table_in(None, after, columns, header)
    }

    pub fn insert_table_in(
        &mut self,
        parent: Option<OpId>,
        after: Option<OpId>,
        columns: Vec<ColumnDef>,
        header: Vec<String>,
    ) -> Result<OpId, SessionError> {
        if columns.len() != header.len() {
            return Err(SessionError::InvalidTableShape);
        }
        validate_table_cells(&header)?;
        self.next_counter
            .checked_add(1)
            .and_then(|next| next.checked_add(columns.len() as u64))
            .ok_or(SessionError::CounterExhausted)?;
        let elem_id = self.peek_next_id();
        let table = Table::new(block_id_from_op(elem_id), elem_id, elem_id);
        let table_elem = self.insert_block_in(
            parent,
            after,
            BlockKind::Table {
                table: Box::new(table),
            },
        )?;
        let table_id = block_id_from_op(table_elem);
        let mut after_column = None;
        for (column, header) in columns.into_iter().zip(header) {
            let inserted =
                self.insert_table_column(table_id, after_column, column.alignment, header)?;
            after_column = Some(inserted);
        }
        Ok(table_elem)
    }

    pub fn insert_table_column(
        &mut self,
        table_id: BlockId,
        after: Option<OpId>,
        alignment: ColumnAlignment,
        header: String,
    ) -> Result<OpId, SessionError> {
        validate_table_cell(&header)?;
        let (table_elem, right_origin) = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            if after.is_some_and(|id| table.columns.get_element(&id).is_none()) {
                return Err(SessionError::TableColumnNotFound);
            }
            (block.elem_id, table.columns.compute_right_origin(after))
        };
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertTableColumn {
                table_elem,
                table_id,
                after,
                id,
                right_origin,
                alignment: alignment_to_wire(&alignment),
                header,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Insert a row into a table's row sequence.
    pub fn insert_table_row(
        &mut self,
        table_id: BlockId,
        after: Option<OpId>,
        cells: Vec<String>,
    ) -> Result<OpId, SessionError> {
        validate_table_cells(&cells)?;
        let (table_elem, right_origin, column_ids) = {
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
            let column_ids: Vec<_> = table
                .columns_in_order()
                .into_iter()
                .map(|column| column.id)
                .collect();
            if cells.len() != column_ids.len() {
                return Err(SessionError::InvalidTableShape);
            }
            (
                block.elem_id,
                table.rows.compute_right_origin(after),
                column_ids,
            )
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
                cells: column_ids
                    .into_iter()
                    .zip(cells)
                    .map(|(column_id, value)| TableCellWire { column_id, value })
                    .collect(),
            }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Replace a row through independent cell operations.
    pub fn set_table_row_cells(
        &mut self,
        table_id: BlockId,
        row: OpId,
        cells: Vec<String>,
    ) -> Result<OpId, SessionError> {
        validate_table_cells(&cells)?;
        let (row_id, columns) = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            let row_id = table
                .rows
                .get_element(&row)
                .and_then(|element| element.value.as_ref())
                .map(|row| row.id)
                .ok_or(SessionError::TableRowNotFound)?;
            (row_id, table.columns_in_order())
        };
        if cells.len() != columns.len() {
            return Err(SessionError::InvalidTableShape);
        }
        let mut last = None;
        for (column, value) in columns.into_iter().zip(cells) {
            last = Some(self.set_table_cell(table_id, row_id, column.id, value)?);
        }
        last.ok_or(SessionError::InvalidTableShape)
    }

    pub fn set_table_cell(
        &mut self,
        table_id: BlockId,
        row_id: RowId,
        column_id: ColumnId,
        value: String,
    ) -> Result<OpId, SessionError> {
        validate_table_cell(&value)?;
        let table_elem = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            if row_id != table.header_row_id() && table.row_by_id(row_id).is_none() {
                return Err(SessionError::TableRowNotFound);
            }
            if table.column_by_id(column_id).is_none() {
                return Err(SessionError::TableColumnNotFound);
            }
            block.elem_id
        };
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::SetTableCell {
                table_elem,
                table_id,
                row_id,
                column_id,
                id,
                value,
                observed,
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
        let (table_elem, row_id) = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            let row = table
                .rows
                .get_element(&target)
                .and_then(|element| element.value.as_ref())
                .ok_or(SessionError::TableRowNotFound)?;
            (block.elem_id, row.id)
        };
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::DeleteTableRowById {
                table_elem,
                table_id,
                target,
                row_id,
                id,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    pub fn set_table_metadata(
        &mut self,
        table_id: BlockId,
        columns: Vec<ColumnDef>,
        header: Vec<String>,
    ) -> Result<OpId, SessionError> {
        if columns.len() != header.len() {
            return Err(SessionError::InvalidTableShape);
        }
        validate_table_cells(&header)?;
        let existing = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            table.columns_in_order()
        };
        let mut last = None;
        for ((column, definition), value) in existing.iter().zip(&columns).zip(&header) {
            if column.alignment.get_ref() != &definition.alignment {
                last = Some(self.set_table_column_alignment(
                    table_id,
                    column.id,
                    definition.alignment.clone(),
                )?);
            }
            let current_header =
                self.document
                    .find_block_by_id(table_id)
                    .and_then(|block| match &block.kind {
                        BlockKind::Table { table } => {
                            table.cell_value(table.header_row_id(), column.id)
                        }
                        _ => None,
                    });
            if current_header != Some(value.as_str()) {
                last = Some(self.set_table_cell(table_id, table_id, column.id, value.clone())?);
            }
        }
        for column in existing.iter().skip(columns.len()) {
            last = Some(self.delete_table_column(table_id, column.id)?);
        }
        let mut after = existing
            .get(columns.len().min(existing.len()).saturating_sub(1))
            .map(|column| column.elem_id);
        for (definition, value) in columns.into_iter().zip(header).skip(existing.len()) {
            let id = self.insert_table_column(table_id, after, definition.alignment, value)?;
            after = Some(id);
            last = Some(id);
        }
        last.ok_or(SessionError::InvalidTableShape)
    }

    pub fn set_table_column_alignment(
        &mut self,
        table_id: BlockId,
        column_id: ColumnId,
        alignment: ColumnAlignment,
    ) -> Result<OpId, SessionError> {
        let table_elem = self.table_elem_with_column(table_id, column_id)?;
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::SetTableColumnAlignment {
                table_elem,
                table_id,
                column_id,
                id,
                alignment: alignment_to_wire(&alignment),
                observed,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    pub fn delete_table_column(
        &mut self,
        table_id: BlockId,
        column_id: ColumnId,
    ) -> Result<OpId, SessionError> {
        let (table_elem, target) = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            let column = table
                .column_by_id(column_id)
                .ok_or(SessionError::TableColumnNotFound)?;
            (block.elem_id, column.elem_id)
        };
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::DeleteTableColumnById {
                table_elem,
                table_id,
                target,
                column_id,
                id,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    pub fn move_table_column(
        &mut self,
        table_id: BlockId,
        column_id: ColumnId,
        after: Option<OpId>,
    ) -> Result<OpId, SessionError> {
        let (table_elem, target, right_origin) = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            let column = table
                .column_by_id(column_id)
                .ok_or(SessionError::TableColumnNotFound)?;
            if after == Some(column.elem_id)
                || after.is_some_and(|anchor| table.columns.get_element(&anchor).is_none())
            {
                return Err(SessionError::InvalidMove);
            }
            (
                block.elem_id,
                column.elem_id,
                table.columns.compute_right_origin(after),
            )
        };
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::MoveTableColumn {
                table_elem,
                table_id,
                column_id,
                target,
                id,
                after,
                right_origin,
                observed,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    pub fn move_table_row(
        &mut self,
        table_id: BlockId,
        row_id: crate::doc::RowId,
        after: Option<OpId>,
    ) -> Result<OpId, SessionError> {
        let (table_elem, target, right_origin) = {
            let block = self
                .document
                .find_block_by_id(table_id)
                .ok_or(SessionError::BlockNotFound)?;
            let BlockKind::Table { table } = &block.kind else {
                return Err(SessionError::NotTable);
            };
            let row = table
                .row_by_id(row_id)
                .ok_or(SessionError::TableRowNotFound)?;
            if after == Some(row.elem_id)
                || after.is_some_and(|anchor| table.rows.get_element(&anchor).is_none())
            {
                return Err(SessionError::InvalidMove);
            }
            (
                block.elem_id,
                row.elem_id,
                table.rows.compute_right_origin(after),
            )
        };
        let id = self.peek_next_id();
        let observed = self.materialized_frontier().into_owned();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::MoveTableRow {
                table_elem,
                table_id,
                row_id,
                target,
                id,
                after,
                right_origin,
                observed,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    fn table_elem_with_column(
        &self,
        table_id: BlockId,
        column_id: ColumnId,
    ) -> Result<OpId, SessionError> {
        let block = self
            .document
            .find_block_by_id(table_id)
            .ok_or(SessionError::BlockNotFound)?;
        let BlockKind::Table { table } = &block.kind else {
            return Err(SessionError::NotTable);
        };
        table
            .column_by_id(column_id)
            .ok_or(SessionError::TableColumnNotFound)?;
        Ok(block.elem_id)
    }

    fn commit_single_id(&mut self, envelope: Envelope, id: OpId) -> Result<OpId, SessionError> {
        let next = id
            .counter
            .checked_add(1)
            .ok_or(SessionError::CounterExhausted)?;
        if matches!(
            &envelope.body,
            OpBody::Doc(DocOp::InsertListItem { .. } | DocOp::SplitBlock { .. })
        ) {
            validate_identity_declarations(
                &self.document,
                self.sync.frontier(),
                self.has_document_pending,
                self.pending_envelopes
                    .values()
                    .map(|pending| &pending.envelope),
                std::iter::once((id, &envelope)),
            )?;
            AdmissionInsertions::default().record(&self.document, &envelope, None)?;
        }
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        self.apply_document_envelope(&envelope);
        self.sync.add_local_op(Operation {
            id,
            payload: payload.into(),
        });
        self.next_counter = next;
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

        // Spans are exclusive (not nested) so attribution shares sum cleanly.
        let block = crate::perf::timed(crate::perf::Span::BlockLookup, || {
            self.document
                .find_block_by_id(block_id)
                .ok_or(SessionError::BlockNotFound)
        })?;
        let (block_elem, units) = crate::perf::timed(crate::perf::Span::UnitExpand, || {
            let block_elem = block.elem_id;
            let Some(body) = crate::doc::block_text_seq(&block.kind) else {
                return Err(SessionError::NotParagraph);
            };
            if grapheme_offset > body.len_visible() {
                return Err(SessionError::InvalidOffset);
            }

            let mut after = after_for_grapheme_offset(body, grapheme_offset);
            // First unit only: stamp right_origin once; chain tail uses None.
            let first_right_origin = body.compute_right_origin(after);
            let mut counter = self.next_counter;
            let graphemes = unicode_segmentation::UnicodeSegmentation::graphemes(text, true);
            // Byte length is the exact unit count only for ASCII. Using it as
            // capacity for arbitrary UTF-8 can massively over-allocate on one
            // grapheme made from many combining scalars.
            let mut units = Vec::with_capacity(if text.is_ascii() { text.len() } else { 0 });
            for g in graphemes {
                let id = OpId {
                    counter,
                    peer: self.peer,
                };
                counter = counter
                    .checked_add(1)
                    .ok_or(SessionError::CounterExhausted)?;
                let right_origin = if units.is_empty() {
                    first_right_origin
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
            Ok::<_, SessionError>((block_elem, units))
        })?;

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
        let next = op_id
            .counter
            .checked_add(1)
            .ok_or(SessionError::CounterExhausted)?;
        let payload = crate::perf::timed(crate::perf::Span::EnvelopeEncode, || {
            self.codec.encode(&envelope).map_err(codec_err)
        })?;
        crate::perf::timed(crate::perf::Span::SequenceApply, || {
            self.apply_document_envelope(&envelope);
        });
        crate::perf::timed(crate::perf::Span::SyncLogAppend, || {
            self.sync.add_local_op(Operation {
                id: op_id,
                payload: payload.into(),
            });
        });
        self.next_counter = next;
        Ok(Some(op_id))
    }

    /// Insert many text runs in **one** commit (one wire envelope).
    ///
    /// Runs are concatenated left-to-right and inserted at `grapheme_offset`,
    /// matching sequential `insert_text` calls at successive offsets when every
    /// run is a pure insert (no interleaved deletes). Empty runs are skipped.
    /// When every run is empty, this is a no-op that does not advance the clock.
    ///
    /// Prefer this over M separate `insert_text` calls when the agent already
    /// knows the full keystroke/paste run (Band C amortization).
    pub fn insert_text_batch(
        &mut self,
        block_id: BlockId,
        grapheme_offset: usize,
        parts: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Option<OpId>, SessionError> {
        let mut combined = String::new();
        for part in parts {
            let part = part.as_ref();
            if !part.is_empty() {
                combined.push_str(part);
            }
        }
        self.insert_text(block_id, grapheme_offset, &combined)
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
        let next = delete_id
            .counter
            .checked_add(1)
            .ok_or(SessionError::CounterExhausted)?;
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        self.apply_document_envelope(&envelope);
        self.sync.add_local_op(Operation {
            id: delete_id,
            payload: payload.into(),
        });
        self.next_counter = next;
        Ok(Some(delete_id))
    }

    /// Set a mark over a non-empty half-open grapheme range.
    pub fn set_mark(
        &mut self,
        block_id: BlockId,
        range: std::ops::Range<usize>,
        kind: MarkKind,
        attrs: BTreeMap<String, MarkValue>,
    ) -> Result<OpId, SessionError> {
        let block = self
            .document
            .find_block_by_id(block_id)
            .ok_or(SessionError::BlockNotFound)?;
        let block_elem = block.elem_id;
        let (start, end) = self
            .document
            .grapheme_range_to_anchors(block_id, range)
            .map_err(|_| SessionError::InvalidOffset)?;
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::SetMark {
                block_elem,
                block_id,
                id,
                kind,
                start,
                end,
                attrs,
            }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Remove one mark interval using the session's observed state.
    pub fn remove_mark(
        &mut self,
        block_id: BlockId,
        interval_id: OpId,
    ) -> Result<OpId, SessionError> {
        let block = self
            .document
            .find_block_by_id(block_id)
            .ok_or(SessionError::BlockNotFound)?;
        if block.marks.interval(&interval_id).is_none() {
            return Err(SessionError::InvalidOffset);
        }
        let block_elem = block.elem_id;
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::RemoveMark {
                block_elem,
                block_id,
                interval_id,
                id,
                observed: self.materialized_frontier().into_owned(),
            }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Set or delete a supported top-level frontmatter field.
    pub fn set_frontmatter_field(
        &mut self,
        key: impl Into<String>,
        value: Option<String>,
    ) -> Result<OpId, SessionError> {
        self.document.validate_frontmatter_edit()?;
        self.commit_frontmatter_field(key.into(), value)
    }

    #[cfg(feature = "filesync")]
    pub(crate) fn set_frontmatter_field_from_parsed(
        &mut self,
        key: String,
        value: Option<String>,
        parsed: &crate::doc::Document,
    ) -> Result<OpId, SessionError> {
        if parsed
            .frontmatter
            .as_ref()
            .is_some_and(|frontmatter| !frontmatter.is_structured())
        {
            return Err(crate::doc::FrontmatterError::Opaque.into());
        }
        // Only source ingestion may replace a stale opaque formatting baseline
        // with parsed supported YAML. The selected base's own restriction remains.
        self.commit_frontmatter_field(key, value)
    }

    fn commit_frontmatter_field(
        &mut self,
        key: String,
        value: Option<String>,
    ) -> Result<OpId, SessionError> {
        let id = self.peek_next_id();
        // Prevalidate without burning the clock or changing the live document.
        let mut probe = self
            .document
            .frontmatter
            .clone()
            .unwrap_or_else(crate::doc::Frontmatter::empty);
        probe.set(key.clone(), value.clone(), id)?;
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::SetFrontmatterField { id, key, value }),
        };
        self.commit_single_id(envelope, id)
    }

    /// Propose a lossless base if none is known; concurrent bases select the greatest OpId.
    pub fn initialize_frontmatter(
        &mut self,
        frontmatter: crate::doc::Frontmatter,
    ) -> Result<Option<OpId>, SessionError> {
        if self
            .document
            .frontmatter
            .as_ref()
            .is_some_and(crate::doc::Frontmatter::has_base)
        {
            return Ok(None);
        }
        let id = self.peek_next_id();
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InitializeFrontmatter { id, frontmatter }),
        };
        self.commit_single_id(envelope, id).map(Some)
    }

    /// Atomically move one logical block, preserving its contents and descendant identities.
    pub fn move_block(
        &mut self,
        block_id: BlockId,
        to_parent: Option<OpId>,
        after: Option<OpId>,
    ) -> Result<OpId, SessionError> {
        self.move_block_ids(&[block_id], to_parent, after)
    }

    /// Wrap contiguous sibling blocks in a new blockquote while retaining their logical ids.
    pub fn wrap_blocks(&mut self, block_ids: &[BlockId]) -> Result<BlockId, SessionError> {
        if block_ids.is_empty() {
            return Err(SessionError::InvalidMove);
        }
        let parent = self
            .document
            .block_parent(block_ids[0])
            .ok_or(SessionError::BlockNotFound)?;
        let siblings = self
            .document
            .container_children(parent)
            .ok_or(SessionError::InvalidMove)?;
        let ordered: Vec<_> = siblings.iter().map(|block| block.id).collect();
        let start = ordered
            .iter()
            .position(|id| *id == block_ids[0])
            .ok_or(SessionError::BlockNotFound)?;
        if ordered.get(start..start + block_ids.len()) != Some(block_ids)
            || block_ids
                .iter()
                .any(|block_id| self.document.block_parent(*block_id) != Some(parent))
        {
            return Err(SessionError::InvalidMove);
        }
        let after = start.checked_sub(1).and_then(|index| {
            let prior = ordered[index];
            self.document
                .find_block_by_id(prior)
                .map(|block| block.elem_id)
        });
        let quote_elem = self.insert_block_in(
            parent,
            after,
            BlockKind::BlockQuote {
                children: Sequence::new(),
            },
        )?;
        self.move_block_ids(block_ids, Some(quote_elem), None)?;
        Ok(block_id_from_op(quote_elem))
    }

    /// Remove one blockquote container and place its children at the same sibling position.
    pub fn unwrap_blockquote(&mut self, block_id: BlockId) -> Result<OpId, SessionError> {
        let block = self
            .document
            .find_block_by_id(block_id)
            .ok_or(SessionError::BlockNotFound)?;
        let BlockKind::BlockQuote { children } = &block.kind else {
            return Err(SessionError::InvalidMove);
        };
        let child_ids: Vec<_> = children.iter().map(|child| child.id).collect();
        let quote_elem = block.elem_id;
        let parent = self
            .document
            .block_parent(block_id)
            .ok_or(SessionError::InvalidMove)?;
        let siblings = self
            .document
            .container_children(parent)
            .ok_or(SessionError::InvalidMove)?;
        let ordered: Vec<_> = siblings.iter().collect();
        let index = ordered
            .iter()
            .position(|candidate| candidate.id == block_id)
            .ok_or(SessionError::BlockNotFound)?;
        let after = index.checked_sub(1).map(|prior| ordered[prior].elem_id);
        if !child_ids.is_empty() {
            self.move_block_ids(&child_ids, parent, after)?;
        }
        self.delete_block_in(parent, quote_elem)
    }

    /// Atomically move a top-level heading and every following block in its section.
    pub fn move_section(
        &mut self,
        heading_id: BlockId,
        after: Option<OpId>,
    ) -> Result<OpId, SessionError> {
        let blocks: Vec<&Block> = self.document.blocks().iter().collect();
        let start = blocks
            .iter()
            .position(|block| block.id == heading_id)
            .ok_or(SessionError::BlockNotFound)?;
        let BlockKind::Heading { level, .. } = blocks[start].kind else {
            return Err(SessionError::InvalidMove);
        };
        let end = blocks[start + 1..]
            .iter()
            .position(|block| matches!(block.kind, BlockKind::Heading { level: next, .. } if next <= level))
            .map_or(blocks.len(), |relative| start + 1 + relative);
        let ids: Vec<BlockId> = blocks[start..end].iter().map(|block| block.id).collect();
        self.move_block_ids(&ids, None, after)
    }

    fn move_block_ids(
        &mut self,
        block_ids: &[BlockId],
        to_parent: Option<OpId>,
        after: Option<OpId>,
    ) -> Result<OpId, SessionError> {
        if block_ids.is_empty() {
            return Err(SessionError::InvalidMove);
        }
        let destination = self
            .document
            .container_children(to_parent)
            .ok_or(SessionError::InvalidMove)?;
        if after.is_some_and(|anchor| destination.get_element(&anchor).is_none()) {
            return Err(SessionError::MissingAfterAnchor);
        }
        let mut source_parent = None;
        let mut targets = Vec::with_capacity(block_ids.len());
        for block_id in block_ids {
            let block = self
                .document
                .find_block_by_id(*block_id)
                .ok_or(SessionError::BlockNotFound)?;
            let parent = self
                .document
                .block_parent(*block_id)
                .ok_or(SessionError::InvalidMove)?;
            if source_parent.is_some() && source_parent != Some(parent) {
                return Err(SessionError::InvalidMove);
            }
            source_parent = Some(parent);
            if to_parent.is_some_and(|candidate| {
                self.document.block_contains_container(*block_id, candidate)
            }) {
                return Err(SessionError::MoveCycle);
            }
            targets.push((block.id, block.elem_id));
        }
        if after.is_some_and(|anchor| targets.iter().any(|(_, target)| *target == anchor)) {
            return Err(SessionError::InvalidMove);
        }
        let base = self.next_counter;
        let next = base
            .checked_add(targets.len() as u64)
            .ok_or(SessionError::CounterExhausted)?;
        let mut moves = Vec::with_capacity(targets.len());
        let mut placement_after = after;
        for (offset, (block_id, target)) in targets.into_iter().enumerate() {
            let id = OpId {
                peer: self.peer,
                counter: base + offset as u64,
            };
            let right_origin = if offset == 0 {
                destination.compute_right_origin(after)
            } else {
                None
            };
            moves.push(MovedBlockWire {
                block_id,
                target,
                id,
                after: placement_after,
                right_origin,
            });
            placement_after = Some(id);
        }
        let id = moves.last().expect("non-empty move").id;
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::MoveBlocks {
                to_parent,
                id,
                blocks: moves,
                observed: self.materialized_frontier().into_owned(),
            }),
        };
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        self.apply_document_envelope(&envelope);
        self.sync.add_local_op(Operation {
            id,
            payload: payload.into(),
        });
        self.next_counter = next;
        Ok(id)
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

        let replacement_count = source_units
            .iter()
            .filter(|(id, _)| occupied.contains(id))
            .count() as u64;
        action_id
            .counter
            .checked_add(replacement_count)
            .and_then(|last| last.checked_add(1))
            .ok_or(SessionError::CounterExhausted)?;
        let mut replacement_counter = action_id.counter + 1;
        let units = source_units
            .into_iter()
            .map(|(source_id, grapheme)| {
                let id = if occupied.contains(&source_id) {
                    let replacement = OpId {
                        counter: replacement_counter,
                        peer: self.peer,
                    };
                    replacement_counter += 1;
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
        let next = op_id
            .counter
            .checked_add(1)
            .ok_or(SessionError::CounterExhausted)?;
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        self.apply_document_envelope(&envelope);
        self.sync.add_local_op(Operation {
            id: op_id,
            payload: payload.into(),
        });
        self.next_counter = next;
        Ok(op_id)
    }

    fn decode_remote_envelope(&self, op: &Operation) -> Result<Envelope, SessionError> {
        crate::perf::timed(crate::perf::Span::ApplyDecode, || {
            self.codec.decode(&op.payload).map_err(codec_err)
        })
    }

    fn validate_remote_envelope(&self, id: OpId, envelope: &Envelope) -> Result<(), SessionError> {
        if envelope.version != WIRE_VERSION {
            return Err(SessionError::UnknownWireVersion(envelope.version));
        }
        if let OpBody::Doc(DocOp::InsertBlock { block, .. }) = &envelope.body {
            validate_block_skeleton(&block.kind)?;
        }
        if self.unit_mode && !insert_block_paragraph_is_empty(envelope) {
            return Err(SessionError::NonEmptyParagraphOnInsertBlock);
        }
        validate_operation_envelope(id, envelope)
    }

    fn pending_charge(&self, id: OpId) -> usize {
        let pending = &self.pending_envelopes[&id];
        pending
            .decoded_bytes
            .saturating_add(pending.raw_charge(self.sync.retained_payload_len(id)))
    }

    fn retained_resources(&self) -> (usize, usize) {
        let mut resources = if self.has_document_pending {
            document_pending_resources(&self.document)
        } else {
            (0, 0)
        };
        resources.0 = resources.0.saturating_add(self.pending_envelopes.len());
        resources.1 = self
            .pending_envelopes
            .iter()
            .fold(resources.1, |sum, (id, pending)| {
                sum.saturating_add(pending.decoded_bytes)
                    .saturating_add(pending.raw_charge(self.sync.retained_payload_len(*id)))
            });
        resources
    }

    fn advance_received(
        &self,
        received: &mut StateVector,
        prepared: &[PreparedOperation],
        indices: &BTreeMap<OpId, usize>,
    ) {
        let mut incoming = indices.iter().peekable();
        let mut held = self.pending_envelopes.iter().peekable();
        loop {
            let envelope = match (incoming.peek(), held.peek()) {
                (Some((incoming_id, _)), Some((held_id, _))) if incoming_id <= held_id => {
                    let (_, index) = incoming.next().expect("peeked incoming operation");
                    &prepared[*index].envelope
                }
                (_, Some(_)) => &held.next().expect("peeked held operation").1.envelope,
                (Some(_), None) => {
                    let (_, index) = incoming.next().expect("peeked incoming operation");
                    &prepared[*index].envelope
                }
                (None, None) => break,
            };
            advance_materialized(received, envelope);
        }
    }

    fn prepare_effect(
        &self,
        prepared: &mut PreparedOperation,
        received: &StateVector,
        materialized: &mut StateVector,
        inserted: &mut AdmissionInsertions,
        work: &mut usize,
    ) -> Result<bool, SessionError> {
        if prepared.ready
            || prepared.discarded
            || prepared.id.counter > received.get(prepared.id.peer).unwrap_or(0)
        {
            return Ok(false);
        }
        let cost = envelope_dependency_work(&prepared.envelope);
        if cost > *work {
            return Ok(false);
        }
        *work -= cost;
        if !envelope_dependencies_ready(&prepared.envelope, materialized, &self.document)
            || envelope_may_buffer_in_document(&self.document, &prepared.envelope, inserted)
            || !inserted.document_effect_ready(
                &self.document,
                &prepared.envelope,
                materialized,
                work,
            )
        {
            return Ok(false);
        }
        prepared.staged_block = stage_nested_block(&prepared.envelope);
        if staged_pending_count(prepared.staged_block.as_ref()) != 0 {
            return Ok(false);
        }
        prepared.ready = true;
        inserted.record(
            &self.document,
            &prepared.envelope,
            prepared.staged_block.as_ref(),
        )?;
        advance_materialized(materialized, &prepared.envelope);
        Ok(true)
    }

    fn resolve_nested_root(
        &self,
        envelope: &Envelope,
        mut block: Block,
        prepared: &[PreparedOperation],
        view: AdmissionView<'_>,
        resolved_held: &std::collections::BTreeSet<OpId>,
        work: &mut usize,
    ) -> Option<(Block, std::collections::BTreeSet<OpId>, StateVector)> {
        if !envelope_dependencies_ready(envelope, view.materialized, &self.document)
            || envelope_may_buffer_in_document(&self.document, envelope, view.inserted)
        {
            return None;
        }
        let (root, span) = operation_extent(envelope);
        if root.counter > view.received.get(root.peer).unwrap_or(0)
            || view
                .materialized
                .get(root.peer)
                .unwrap_or(0)
                .checked_add(1)
                .is_some_and(|next| root.counter.saturating_sub(span.saturating_sub(1)) > next)
        {
            return None;
        }
        let mut frontier = view.materialized.clone();
        advance_materialized(&mut frontier, envelope);
        let mut absorbed = std::collections::BTreeSet::new();
        if !view.inserted.container_exists_for(&self.document, envelope) {
            return Some((block, absorbed, frontier));
        }
        let root_work = envelope_dependency_work(envelope);
        loop {
            if staged_pending_count(Some(&block)) == 0 {
                return Some((block, absorbed, frontier));
            }
            let mut progress = false;
            let candidates = prepared
                .iter()
                .filter(|candidate| !candidate.folded && !candidate.discarded)
                .map(|candidate| (candidate.id, &candidate.envelope))
                .chain(
                    self.pending_envelopes
                        .iter()
                        .filter(|(id, _)| !resolved_held.contains(id))
                        .map(|(id, pending)| (*id, &pending.envelope)),
                );
            for (id, candidate) in candidates {
                if id == root
                    || absorbed.contains(&id)
                    || id.counter > view.received.get(id.peer).unwrap_or(0)
                    || !matches!(
                        &candidate.body,
                        OpBody::Doc(
                            DocOp::InsertBlock {
                                parent: Some(_),
                                ..
                            } | DocOp::InsertListItem { .. }
                        )
                    )
                {
                    continue;
                }
                // A staged tree has no document index. Charge the bounded tree
                // traversal as well as the candidate's dependency inspection.
                let cost = root_work.saturating_add(envelope_dependency_work(candidate));
                if cost > *work {
                    continue;
                }
                *work -= cost;
                if envelope_dependencies_ready(candidate, &frontier, &self.document)
                    && fold_staged_dependency(&mut block, candidate)
                {
                    absorbed.insert(id);
                    advance_materialized(&mut frontier, candidate);
                    progress = true;
                }
            }
            if !progress {
                return None;
            }
        }
    }

    /// Apply remote changes: validate and budget the entire message before mutation.
    pub fn apply_remote(
        &mut self,
        message: ChangeMessage,
        limits: &ValidationLimits,
    ) -> Result<SessionApplyResult, SessionError> {
        crate::perf::timed(crate::perf::Span::ApplyValidate, || {
            crate::sync::validate_message(&message, limits)
        })?;
        let mut prepared = Vec::with_capacity(message.ops.len());
        let mut seen = BTreeMap::new();
        for op in message.ops {
            if self.sync.has_received(op.id) {
                continue;
            }
            match seen.entry(op.id) {
                std::collections::btree_map::Entry::Occupied(_) => continue,
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(prepared.len());
                }
            }
            let envelope = self.decode_remote_envelope(&op)?;
            prepared.push(PreparedOperation {
                id: op.id,
                raw_bytes: op.payload.len(),
                op: Some(op),
                envelope,
                ready: false,
                staged_block: None,
                charge: 0,
                folded: false,
                committed_operations: 1,
                discarded: false,
            });
        }
        if prepared.is_empty() && self.pending_envelopes.is_empty() {
            crate::perf::timed(crate::perf::Span::ApplyValidate, || {
                validate_retained_resources(self.retained_resources(), limits)
            })?;
            return Ok(SessionApplyResult::default());
        }

        let (mut work, order, mut held_roots, resolved_held) =
            crate::perf::timed(crate::perf::Span::ApplyValidate, || {
                for operation in &prepared {
                    self.validate_remote_envelope(operation.id, &operation.envelope)?;
                }
                validate_identity_declarations(
                    &self.document,
                    self.sync.frontier(),
                    self.has_document_pending,
                    self.pending_envelopes
                        .values()
                        .map(|pending| &pending.envelope),
                    prepared
                        .iter()
                        .map(|operation| (operation.id, &operation.envelope)),
                )?;

                let document_resources = if self.has_document_pending {
                    document_pending_resources(&self.document)
                } else {
                    (0, 0)
                };
                let document_pending = document_resources.0;
                let mut retained_count =
                    document_pending.saturating_add(self.pending_envelopes.len());
                let mut retained_bytes = self.pending_envelopes.iter().fold(
                    document_resources.1,
                    |sum, (id, pending)| {
                        sum.saturating_add(pending.decoded_bytes)
                            .saturating_add(pending.raw_charge(self.sync.retained_payload_len(*id)))
                    },
                );
                let mut received = self.sync.state_vector();
                let mut materialized = self.materialized_frontier().into_owned();
                let mut inserted = AdmissionInsertions::default();
                let mut work = limits.max_dependency_work.saturating_sub(document_pending);
                let indices = seen;
                let mut order = Vec::with_capacity(prepared.len());
                for (index, prepared) in prepared.iter_mut().enumerate() {
                    if prepared.id.counter <= received.get(prepared.id.peer).unwrap_or(0) {
                        prepared.discarded = true;
                        prepared.op = None;
                        continue;
                    }
                    let (_, span) = operation_extent(&prepared.envelope);
                    let start = prepared.id.counter.saturating_sub(span.saturating_sub(1));
                    if received
                        .get(prepared.id.peer)
                        .unwrap_or(0)
                        .checked_add(1)
                        .is_none_or(|next| start <= next)
                    {
                        received.set(
                            prepared.id.peer,
                            received
                                .get(prepared.id.peer)
                                .unwrap_or(0)
                                .max(prepared.id.counter),
                        );
                    }
                    if self.prepare_effect(
                        prepared,
                        &received,
                        &mut materialized,
                        &mut inserted,
                        &mut work,
                    )? {
                        order.push(PlannedEffect::New(index));
                    }
                }
                self.advance_received(&mut received, &prepared, &indices);
                let mut resolved_held = std::collections::BTreeSet::new();
                let mut held_roots = BTreeMap::new();
                loop {
                    let mut progress = false;
                    for index in 0..prepared.len() {
                        if prepared[index].ready {
                            continue;
                        }
                        let Some(block) = prepared[index].staged_block.take() else {
                            continue;
                        };
                        let resolved = self.resolve_nested_root(
                            &prepared[index].envelope,
                            block,
                            &prepared,
                            AdmissionView {
                                received: &received,
                                materialized: &materialized,
                                inserted: &inserted,
                            },
                            &resolved_held,
                            &mut work,
                        );
                        if let Some((block, absorbed, frontier)) = resolved {
                            prepared[index].committed_operations = 1 + absorbed.len();
                            materialized = frontier;
                            inserted.record(
                                &self.document,
                                &prepared[index].envelope,
                                Some(&block),
                            )?;
                            prepared[index].ready = true;
                            prepared[index].staged_block = Some(block);
                            order.push(PlannedEffect::New(index));
                            for id in absorbed {
                                if let Some(&child) = indices.get(&id) {
                                    prepared[child].ready = true;
                                    prepared[child].folded = true;
                                    prepared[child].staged_block = None;
                                } else {
                                    resolved_held.insert(id);
                                }
                            }
                            progress = true;
                        }
                    }
                    for (id, pending) in &self.pending_envelopes {
                        let envelope = &pending.envelope;
                        if resolved_held.contains(id)
                            || id.counter > received.get(id.peer).unwrap_or(0)
                        {
                            continue;
                        }
                        let cost = envelope_dependency_work(envelope);
                        if cost > work {
                            continue;
                        }
                        work -= cost;
                        if !envelope_dependencies_ready(envelope, &materialized, &self.document)
                            || envelope_may_buffer_in_document(&self.document, envelope, &inserted)
                            || !inserted.document_effect_ready(
                                &self.document,
                                envelope,
                                &materialized,
                                &mut work,
                            )
                        {
                            continue;
                        }
                        let Some(block) = stage_nested_block(envelope) else {
                            resolved_held.insert(*id);
                            order.push(PlannedEffect::Held(*id));
                            inserted.record(&self.document, envelope, None)?;
                            advance_materialized(&mut materialized, envelope);
                            progress = true;
                            continue;
                        };
                        if let Some((block, absorbed, frontier)) = self.resolve_nested_root(
                            envelope,
                            block,
                            &prepared,
                            AdmissionView {
                                received: &received,
                                materialized: &materialized,
                                inserted: &inserted,
                            },
                            &resolved_held,
                            &mut work,
                        ) {
                            materialized = frontier;
                            inserted.record(&self.document, envelope, Some(&block))?;
                            held_roots.insert(*id, (block, 1 + absorbed.len()));
                            resolved_held.insert(*id);
                            order.push(PlannedEffect::Held(*id));
                            for id in absorbed {
                                if let Some(&child) = indices.get(&id) {
                                    prepared[child].ready = true;
                                    prepared[child].folded = true;
                                    prepared[child].staged_block = None;
                                } else {
                                    resolved_held.insert(id);
                                }
                            }
                            progress = true;
                        }
                    }
                    for (index, prepared) in prepared.iter_mut().enumerate() {
                        if self.prepare_effect(
                            prepared,
                            &received,
                            &mut materialized,
                            &mut inserted,
                            &mut work,
                        )? {
                            order.push(PlannedEffect::New(index));
                            progress = true;
                        }
                    }
                    if !progress {
                        break;
                    }
                }
                for id in &resolved_held {
                    retained_count = retained_count.saturating_sub(1);
                    retained_bytes = retained_bytes.saturating_sub(self.pending_charge(*id));
                }
                for prepared in &mut prepared {
                    if !prepared.ready && !prepared.discarded {
                        prepared.charge = envelope_retained_bytes(&prepared.envelope);
                        let raw = &prepared.op.as_ref().expect("uncommitted operation").payload;
                        prepared.raw_bytes = prepared.raw_bytes.max(
                            self.codec
                                .snapshot_payload_len(raw, &prepared.envelope)
                                .map_err(codec_err)?,
                        );
                        retained_count = retained_count.saturating_add(1);
                        retained_bytes = retained_bytes
                            .saturating_add(prepared.raw_bytes)
                            .saturating_add(prepared.charge);
                    }
                }
                validate_retained_resources((retained_count, retained_bytes), limits)?;
                Ok::<_, SessionError>((work, order, held_roots, resolved_held))
            })?;

        let mut result = SessionApplyResult::default();
        for prepared in &mut prepared {
            if let Some(op) = prepared.op.take() {
                let span = operation_extent(&prepared.envelope).1;
                self.sync.apply_one(op, span);
            }
        }
        self.sync.promote_pending_log();
        let committed = !order.is_empty();
        for effect in order {
            match effect {
                PlannedEffect::New(index) if !prepared[index].folded => {
                    let staged = prepared[index].staged_block.take();
                    self.integrate_document_envelope(
                        &prepared[index].envelope,
                        staged,
                        prepared[index].committed_operations,
                    );
                    result.applied.push(prepared[index].id);
                }
                PlannedEffect::Held(id) => {
                    if let Some(pending) = self.pending_envelopes.remove(&id) {
                        let (staged, count) = held_roots
                            .remove(&id)
                            .map_or((None, 1), |(block, count)| (Some(block), count));
                        self.integrate_document_envelope(&pending.envelope, staged, count);
                        result.applied.push(id);
                    }
                }
                PlannedEffect::New(_) => {}
            }
        }
        for id in resolved_held {
            if self.pending_envelopes.remove(&id).is_some() {
                result.applied.push(id);
            }
        }
        for prepared in prepared {
            if prepared.discarded {
                continue;
            }
            if prepared.folded {
                result.applied.push(prepared.id);
            } else if !prepared.ready {
                self.pending_envelopes.insert(
                    prepared.id,
                    PendingEnvelope {
                        envelope: prepared.envelope,
                        decoded_bytes: prepared.charge,
                        snapshot_raw_bytes: prepared.raw_bytes,
                    },
                );
                result.buffered.push(prepared.id);
            }
        }
        if committed {
            self.drain_deferred(&mut result, &mut work, limits);
        }
        Ok(result)
    }

    fn apply_document_envelope(&mut self, envelope: &Envelope) {
        if let OpBody::Doc(DocOp::MoveListItem {
            from_list_elem,
            to_list_elem,
            list_id,
            item_id,
            target,
            id,
            after,
            right_origin,
            observed,
        }) = &envelope.body
        {
            // Native mutations refuse current-tree descendant moves. Replicated
            // proposals use apply_document_effect so concurrent cycle histories
            // remain available to the shared deterministic projection.
            self.document
                .move_list_item_at(&crate::doc::PendingListItemMove {
                    from_list_elem: *from_list_elem,
                    to_list_elem: *to_list_elem,
                    list_id: *list_id,
                    item_id: *item_id,
                    target: *target,
                    id: *id,
                    after: *after,
                    right_origin: *right_origin,
                    observed: observed.clone(),
                });
            if self.has_document_pending {
                self.refresh_document_pending();
            }
        } else {
            self.apply_document_effect(envelope, None);
        }
    }

    fn integrate_document_envelope(
        &mut self,
        envelope: &Envelope,
        staged_block: Option<Block>,
        committed_operations: usize,
    ) {
        crate::perf::timed_count(
            crate::perf::Span::ApplyIntegrate,
            committed_operations,
            || {
                self.apply_document_effect(envelope, staged_block);
            },
        );
    }

    fn apply_document_effect(&mut self, envelope: &Envelope, staged_block: Option<Block>) {
        let refresh_pending = self.has_document_pending
            || staged_block.is_some()
            || envelope_has_nested_block(envelope);
        if let (
            Some(block),
            OpBody::Doc(DocOp::InsertBlock {
                parent,
                after,
                id,
                right_origin,
                ..
            }),
        ) = (staged_block, &envelope.body)
        {
            self.document
                .insert_block_at(*parent, *after, *id, block, *right_origin);
        } else {
            apply_envelope_to_document(&mut self.document, envelope);
        }
        if refresh_pending {
            self.refresh_document_pending();
        }
    }

    fn refresh_document_pending(&mut self) {
        let status = document_pending_status(&self.document);
        self.has_document_pending = status.count != 0;
    }

    fn materialized_frontier(&self) -> std::borrow::Cow<'_, StateVector> {
        if self.pending_envelopes.is_empty() && !self.has_document_pending {
            return std::borrow::Cow::Borrowed(self.sync.frontier());
        }
        let mut frontier = self.sync.state_vector();
        if self.has_document_pending {
            lower_document_pending_frontier(&self.document, &mut frontier);
        }
        for pending in self.pending_envelopes.values() {
            let (end, span) = operation_extent(&pending.envelope);
            let before = end.counter.saturating_sub(span);
            if frontier
                .get(end.peer)
                .is_some_and(|current| current > before)
            {
                frontier.set(end.peer, before);
            }
        }
        std::borrow::Cow::Owned(frontier)
    }

    fn drain_deferred(
        &mut self,
        result: &mut SessionApplyResult,
        work: &mut usize,
        limits: &ValidationLimits,
    ) {
        while !self.pending_envelopes.is_empty() && *work != 0 {
            let ready = crate::perf::timed(crate::perf::Span::ApplyValidate, || {
                let mut frontier = self.materialized_frontier().into_owned();
                let mut inserted = AdmissionInsertions::default();
                let mut resources = self.retained_resources();
                let mut ready = Vec::new();
                for (id, pending) in &self.pending_envelopes {
                    let envelope = &pending.envelope;
                    if envelope_has_nested_block(envelope) {
                        continue;
                    }
                    if id.counter > self.sync.frontier().get(id.peer).unwrap_or(0) {
                        continue;
                    }
                    let cost = envelope_dependency_work(envelope);
                    if cost > *work {
                        break;
                    }
                    *work -= cost;
                    if !envelope_dependencies_ready(envelope, &frontier, &self.document)
                        || envelope_may_buffer_in_document(&self.document, envelope, &inserted)
                        || !inserted.document_effect_ready(
                            &self.document,
                            envelope,
                            &frontier,
                            work,
                        )
                    {
                        continue;
                    }
                    let count = resources.0.saturating_sub(1);
                    let bytes = resources.1.saturating_sub(self.pending_charge(*id));
                    if count
                        > limits
                            .max_pending_buffer
                            .min(crate::limits::MAX_DURABLE_PENDING_OPERATIONS)
                        || bytes
                            > limits
                                .max_retained_bytes
                                .min(crate::limits::MAX_DURABLE_RETAINED_BYTES)
                    {
                        continue;
                    }
                    if inserted.record(&self.document, envelope, None).is_err() {
                        continue;
                    }
                    resources = (count, bytes);
                    ready.push(*id);
                    let (end, span) = operation_extent(envelope);
                    if frontier.get(end.peer).unwrap_or(0) == end.counter.saturating_sub(span) {
                        frontier.set(end.peer, end.counter);
                    }
                }
                ready
            });
            if ready.is_empty() {
                break;
            }
            for id in ready {
                if let Some(pending) = self.pending_envelopes.remove(&id) {
                    self.integrate_document_envelope(&pending.envelope, None, 1);
                    result.buffered.retain(|pending| *pending != id);
                    result.applied.push(id);
                }
            }
        }
    }

    /// Serialize current session state (document + op log + clock).
    ///
    /// Includes the full applied op log for retransmission; growth without
    /// compaction is accepted for 0.1.
    pub fn save_snapshot(&self) -> Result<SessionSnapshot, SnapshotError> {
        let applied = self.sync.applied_operations();
        let mut ops = Vec::with_capacity(applied.len());
        for (id, payload) in applied {
            let canonical = self
                .codec
                .snapshot_payload(payload, None)
                .map_err(|error| SnapshotError::Serde(error.to_string()))?;
            ops.push((id, canonical.into_owned()));
        }
        let pending = self
            .sync
            .pending_operations()
            .map(|op| {
                self.codec
                    .snapshot_payload(&op.payload, Some(&self.pending_envelopes[&op.id].envelope))
                    .map(|payload| (op.id, payload.into_owned()))
                    .map_err(|error| SnapshotError::Serde(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let deferred = self
            .pending_envelopes
            .iter()
            .filter(|(id, _)| id.counter <= self.sync.frontier().get(id.peer).unwrap_or(0))
            .map(|(id, pending)| {
                JsonOpCodec
                    .encode(&pending.envelope)
                    .map(|payload| (*id, payload))
                    .map_err(|error| SnapshotError::Serde(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SessionSnapshot {
            format_version: SNAPSHOT_FORMAT_VERSION,
            peer: self.peer,
            next_counter: self.next_counter,
            unit_mode: self.unit_mode,
            state_vector: self.sync.state_vector(),
            checkpoint_epoch: self.sync.checkpoint_epoch(),
            delta_floor: self.sync.delta_floor().clone(),
            document: DocumentDto::from_document(&self.document),
            ops,
            pending,
            deferred,
        })
    }

    /// Switch peer identity after restore (late join without reloading bytes).
    pub fn rebind_peer(&mut self, local_peer: PeerId) {
        self.peer = local_peer;
        let mut max = max_counter_for_peer(local_peer, &[], &self.document);
        max = max
            .max(self.sync.frontier().get(local_peer).unwrap_or(0))
            .max(self.sync.delta_floor().get(local_peer).unwrap_or(0));
        for op in self.sync.pending_operations() {
            if op.id.peer == local_peer {
                max = max.max(op.id.counter);
            }
        }
        self.next_counter = max.saturating_add(1).max(1);
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
    /// Decode and restore native snapshot bytes.
    pub fn restore_from_snapshot_bytes(bytes: &[u8]) -> Result<Self, SnapshotError> {
        Self::restore_validated_snapshot(snapshot::ValidatedSnapshot::from_bytes(bytes)?)
    }

    /// Crash recovery for the **same** peer: restore peer id and `next_counter`.
    pub fn restore_from_snapshot(snap: SessionSnapshot) -> Result<Self, SnapshotError> {
        Self::restore_validated_snapshot(snapshot::ValidatedSnapshot::new(snap)?)
    }

    fn restore_validated_snapshot(
        validated: snapshot::ValidatedSnapshot,
    ) -> Result<Self, SnapshotError> {
        let (snap, pending_envelopes) = validated.into_parts();
        let doc = snap.document.into_document();
        let mut max = max_counter_for_peer(snap.peer, &snap.ops, &doc)
            .max(snap.state_vector.get(snap.peer).unwrap_or(0))
            .max(snap.delta_floor.get(snap.peer).unwrap_or(0));
        for (id, _) in &snap.pending {
            if id.peer == snap.peer {
                max = max.max(id.counter);
            }
        }
        if snap.next_counter <= max && !(snap.next_counter == u64::MAX && max == u64::MAX) {
            return Err(SnapshotError::ClockBehind {
                peer: snap.peer,
                next: snap.next_counter,
                max,
            });
        }

        let codec = JsonOpCodec;
        let mut sync = SyncState::new();
        sync.restore_applied(snap.ops);
        let pending_ops: Vec<(Operation, u64)> = snap
            .pending
            .into_iter()
            .map(|(id, payload)| {
                let span = operation_extent(&pending_envelopes[&id].envelope).1;
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
        sync.restore_history(snap.state_vector, snap.checkpoint_epoch, snap.delta_floor);

        drop(snap.deferred);

        let pending_status = document_pending_status(&doc);
        validate_identity_declarations(
            &doc,
            sync.frontier(),
            pending_status.count != 0,
            std::iter::empty(),
            pending_envelopes
                .iter()
                .map(|(id, pending)| (*id, &pending.envelope)),
        )
        .map_err(|error| match error {
            SessionError::Validation(cause) => SnapshotError::Validation(cause),
            _ => SnapshotError::InvalidState("invalid pending identity declarations"),
        })?;
        Ok(Self {
            peer: snap.peer,
            next_counter: snap.next_counter,
            has_document_pending: pending_status.count != 0,
            document: doc,
            sync,
            codec,
            unit_mode: snap.unit_mode,
            pending_envelopes,
        })
    }

    /// Install a full checkpoint for a lagging peer while retaining the local peer identity.
    pub fn rebase_from_snapshot(
        snap: SessionSnapshot,
        local_peer: PeerId,
    ) -> Result<Self, SnapshotError> {
        let mut session = Self::restore_from_snapshot(snap)?;
        session.rebind_peer(local_peer);
        Ok(session)
    }

    /// Late join: load compact state as `local_peer` (does not adopt snapshot peer).
    pub fn import_state(
        document: DocumentDto,
        ops: Vec<(OpId, Vec<u8>)>,
        pending: Vec<(OpId, Vec<u8>)>,
        deferred: Vec<(OpId, Vec<u8>)>,
        local_peer: PeerId,
        unit_mode: bool,
    ) -> Result<Self, SnapshotError> {
        let mut state_vector = StateVector::new();
        for (id, _) in &ops {
            let counter = state_vector.get(id.peer).unwrap_or(0).max(id.counter);
            state_vector.set(id.peer, counter);
        }
        let mut session = Self::restore_from_snapshot(SessionSnapshot {
            format_version: SNAPSHOT_FORMAT_VERSION,
            peer: local_peer,
            // Reserve the exhausted sentinel until rebind derives the actual clock
            // from every imported layer; no local operation runs before that.
            next_counter: u64::MAX,
            unit_mode,
            state_vector,
            checkpoint_epoch: 0,
            delta_floor: StateVector::new(),
            document,
            ops,
            pending,
            deferred,
        })?;
        session.rebind_peer(local_peer);
        Ok(session)
    }

    #[cfg(feature = "storage")]
    pub fn read_from_storage(storage: &crate::storage::Storage) -> Result<Self, SnapshotError> {
        let (bytes, _, _) = storage.read_snapshot()?;
        Self::restore_from_snapshot_bytes(&bytes)
    }
}
