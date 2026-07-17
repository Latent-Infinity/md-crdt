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
    ChangeMessage, CheckpointError, CheckpointReport, CheckpointRequest, IntegrateResult,
    Operation, RebaseRequired, SyncState, ValidationError, ValidationLimits, validate_changes,
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

    pub(crate) fn document_mut(&mut self) -> &mut Document {
        &mut self.document
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

    /// Insert one validated structured block tree without parsing Markdown fragments.
    pub fn insert_draft_in(
        &mut self,
        parent: Option<OpId>,
        after: Option<OpId>,
        draft: &BlockDraft,
        limits: StructuredEditLimits,
    ) -> Result<OpId, SessionError> {
        draft.validate(limits)?;
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
        let observed = self.state_vector();
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
                observed: self.state_vector(),
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
        let key = key.into();
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

    /// Establish a lossless frontmatter base. Existing frontmatter is never overwritten.
    pub fn initialize_frontmatter(
        &mut self,
        frontmatter: crate::doc::Frontmatter,
    ) -> Result<Option<OpId>, SessionError> {
        if self.document.frontmatter.is_some() {
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
            }),
        };
        let payload = self.codec.encode(&envelope).map_err(codec_err)?;
        apply_envelope_to_document(&mut self.document, &envelope);
        self.sync.add_local_op(Operation {
            id,
            payload: payload.into(),
        });
        self.next_counter = id.counter + 1;
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
        let deferred_count = self
            .pending_envelopes
            .keys()
            .filter(|id| self.sync.contains(**id))
            .count();
        validate_changes(
            &message,
            limits,
            self.sync.pending_count().saturating_add(deferred_count),
        )?;

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
            if let OpBody::Doc(DocOp::InsertBlock { block, .. }) = &env.body {
                validate_block_skeleton(&block.kind)?;
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
                    self.apply_or_defer(id, env, &mut result);
                    self.drain_promoted(&mut result)?;
                    self.drain_deferred(&mut result);
                }
            }
        }
        self.drain_deferred(&mut result);
        Ok(result)
    }

    fn drain_promoted(&mut self, result: &mut SessionApplyResult) -> Result<(), SessionError> {
        for promoted in self.sync.promote_ready_pending() {
            let env = if let Some(env) = self.pending_envelopes.remove(&promoted.id) {
                env
            } else {
                self.codec.decode(&promoted.payload).map_err(codec_err)?
            };
            self.apply_or_defer(promoted.id, env, result);
        }
        Ok(())
    }

    fn observed_frontier_is_ready(&self, envelope: &Envelope) -> bool {
        let Some(observed) = envelope_observed_frontier(envelope) else {
            return true;
        };
        let current = self.sync.state_vector();
        observed
            .iter()
            .all(|(peer, counter)| current.get(peer).unwrap_or(0) >= counter)
    }

    fn apply_or_defer(&mut self, id: OpId, envelope: Envelope, result: &mut SessionApplyResult) {
        if !self.observed_frontier_is_ready(&envelope) {
            self.pending_envelopes.insert(id, envelope);
            if !result.buffered.contains(&id) {
                result.buffered.push(id);
            }
            return;
        }
        apply_envelope_to_document(&mut self.document, &envelope);
        result.buffered.retain(|pending| *pending != id);
        if !result.applied.contains(&id) {
            result.applied.push(id);
        }
    }

    fn drain_deferred(&mut self, result: &mut SessionApplyResult) {
        loop {
            let ready: Vec<_> = self
                .pending_envelopes
                .iter()
                .filter(|(id, envelope)| {
                    self.sync.contains(**id) && self.observed_frontier_is_ready(envelope)
                })
                .map(|(id, _)| *id)
                .collect();
            if ready.is_empty() {
                break;
            }
            for id in ready {
                if let Some(envelope) = self.pending_envelopes.remove(&id) {
                    self.apply_or_defer(id, envelope, result);
                }
            }
        }
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
        let deferred = self
            .pending_envelopes
            .iter()
            .filter(|(id, _)| self.sync.contains(**id))
            .map(|(id, envelope)| {
                self.codec
                    .encode(envelope)
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
        let ops = self.sync.applied_ops();
        let mut max = max_counter_for_peer(local_peer, &ops, &self.document);
        max = max
            .max(self.sync.state_vector().get(local_peer).unwrap_or(0))
            .max(self.sync.delta_floor().get(local_peer).unwrap_or(0));
        for op in self.sync.pending() {
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
    /// Crash recovery for the **same** peer: restore peer id and `next_counter`.
    pub fn restore_from_snapshot(snap: SessionSnapshot) -> Result<Self, SnapshotError> {
        if snap.format_version != SNAPSHOT_FORMAT_VERSION {
            return Err(SnapshotError::ReinitializeRequired {
                found: snap.format_version,
                expected: SNAPSHOT_FORMAT_VERSION,
            });
        }
        let doc = snap.document.clone().into_document();
        let mut max = max_counter_for_peer(snap.peer, &snap.ops, &doc)
            .max(snap.state_vector.get(snap.peer).unwrap_or(0))
            .max(snap.delta_floor.get(snap.peer).unwrap_or(0));
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
        sync.restore_history(snap.state_vector, snap.checkpoint_epoch, snap.delta_floor);

        let codec = JsonOpCodec;
        let mut pending_envelopes = BTreeMap::new();
        for (id, payload) in snap.deferred {
            let envelope = codec
                .decode(&payload)
                .map_err(|error| SnapshotError::Serde(error.to_string()))?;
            pending_envelopes.insert(id, envelope);
        }

        Ok(Self {
            peer: snap.peer,
            next_counter: snap.next_counter,
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

        let codec = JsonOpCodec;
        let mut pending_envelopes = BTreeMap::new();
        for (id, payload) in deferred {
            let envelope = codec
                .decode(&payload)
                .map_err(|error| SnapshotError::Serde(error.to_string()))?;
            pending_envelopes.insert(id, envelope);
        }

        Ok(Self {
            peer: local_peer,
            next_counter,
            document: doc,
            sync,
            codec,
            unit_mode,
            pending_envelopes,
        })
    }

    #[cfg(feature = "storage")]
    pub fn read_from_storage(storage: &crate::storage::Storage) -> Result<Self, SnapshotError> {
        let (bytes, _, _) = storage.read_snapshot()?;
        let snap = SessionSnapshot::from_bytes(&bytes)?;
        Self::restore_from_snapshot(snap)
    }
}
