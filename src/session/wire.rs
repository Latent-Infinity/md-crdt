use super::*;

/// Counter span an op payload covers, for restoring pending ops. Falls back to 1 if the
/// payload cannot be decoded (trusted local disk, N5).
pub(super) fn span_of_payload(payload: &[u8]) -> u64 {
    JsonOpCodec
        .decode(payload)
        .map(|env| operation_extent(&env).1)
        .unwrap_or(1)
}

pub(super) fn check_operation_id_is_max(
    op: &Operation,
    env: &Envelope,
) -> Result<(), SessionError> {
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
pub(super) fn operation_extent(env: &Envelope) -> (OpId, u64) {
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
        OpBody::Doc(DocOp::DeleteBlock { id, .. } | DocOp::DeleteBlockById { id, .. }) => (*id, 1),
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
        OpBody::Doc(DocOp::SetMark { id, .. } | DocOp::RemoveMark { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::SetFrontmatterField { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::InitializeFrontmatter { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::MoveBlocks { id, blocks, .. }) => {
            let lo = blocks
                .iter()
                .map(|block| block.id.counter)
                .min()
                .unwrap_or(id.counter);
            (*id, id.counter.saturating_sub(lo).saturating_add(1))
        }
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
            | DocOp::InsertTableColumn { id, .. }
            | DocOp::SetTableCell { id, .. }
            | DocOp::DeleteTableRow { id, .. }
            | DocOp::DeleteTableRowById { id, .. }
            | DocOp::DeleteTableColumnById { id, .. }
            | DocOp::SetTableColumnAlignment { id, .. }
            | DocOp::MoveTableRow { id, .. }
            | DocOp::MoveTableColumn { id, .. }
            | DocOp::InsertListItem { id, .. }
            | DocOp::DeleteListItemById { id, .. }
            | DocOp::MoveListItem { id, .. }
            | DocOp::SetListStyle { id, .. }
            | DocOp::SetListItemTask { id, .. }
            | DocOp::SetCodeFence { id, .. }
            | DocOp::ConvertTextBlock { id, .. }
            | DocOp::ReplaceRawBlock { id, .. },
        ) => (*id, 1),
    }
}

/// Highest counter that `kind_from_skeleton` assigns when expanding `kind` under
/// `parent`. A paragraph seeds units at `parent.counter + 1 ..= parent.counter + G`.
pub(super) fn max_counter_in_kind(kind: &BlockKindSkeleton, parent: OpId) -> u64 {
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
                hi = hi.max(item.id.counter).max(item.task_op.counter);
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
        | BlockKindSkeleton::Table => parent.counter,
    }
}

pub(super) fn check_peer_consistency(op: &Operation, env: &Envelope) -> Result<(), SessionError> {
    let peer = op.id.peer;
    match &env.body {
        OpBody::Doc(DocOp::InsertBlock { id, block, .. }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
            check_kind_peers(peer, &block.kind)?;
        }
        OpBody::Doc(DocOp::DeleteBlock { id, .. } | DocOp::DeleteBlockById { id, .. }) => {
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
        OpBody::Doc(DocOp::SetMark { id, .. } | DocOp::RemoveMark { id, .. }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
        OpBody::Doc(DocOp::SetFrontmatterField { id, .. }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
        OpBody::Doc(DocOp::InitializeFrontmatter { id, .. }) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
        OpBody::Doc(DocOp::MoveBlocks { id, blocks, .. }) => {
            if id.peer != peer || blocks.iter().any(|block| block.id.peer != peer) {
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
            | DocOp::InsertTableColumn { id, .. }
            | DocOp::SetTableCell { id, .. }
            | DocOp::DeleteTableRow { id, .. }
            | DocOp::DeleteTableRowById { id, .. }
            | DocOp::DeleteTableColumnById { id, .. }
            | DocOp::SetTableColumnAlignment { id, .. }
            | DocOp::MoveTableRow { id, .. }
            | DocOp::MoveTableColumn { id, .. }
            | DocOp::InsertListItem { id, .. }
            | DocOp::DeleteListItemById { id, .. }
            | DocOp::MoveListItem { id, .. }
            | DocOp::SetListStyle { id, .. }
            | DocOp::SetListItemTask { id, .. }
            | DocOp::SetCodeFence { id, .. }
            | DocOp::ConvertTextBlock { id, .. }
            | DocOp::ReplaceRawBlock { id, .. },
        ) => {
            if id.peer != peer {
                return Err(SessionError::PeerMismatch);
            }
        }
    }
    Ok(())
}

pub(super) fn check_kind_peers(peer: PeerId, kind: &BlockKindSkeleton) -> Result<(), SessionError> {
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
                if item.id.peer != peer || item.task_op.peer != peer {
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

pub(super) fn block_kind_to_skeleton(
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
        BlockKind::List { style, items, .. } => {
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
                        task: item.task,
                        task_op: item.task_op,
                        children,
                    });
                }
            }
            Ok(BlockKindSkeleton::List {
                style: *style,
                items: wire_items,
            })
        }
        BlockKind::CodeFence { style, info, text } => Ok(BlockKindSkeleton::CodeFence {
            style: *style,
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
            if table.rows.iter().next().is_some() || table.columns.iter().next().is_some() {
                return Err(SessionError::NonEmptyTableOnInsertBlock);
            }
            Ok(BlockKindSkeleton::Table)
        }
    }
}

fn current_block_elem(document: &Document, block_id: BlockId, fallback: OpId) -> OpId {
    document.block_elem_id(block_id).unwrap_or(fallback)
}

pub(super) fn apply_envelope_to_document(document: &mut Document, envelope: &Envelope) {
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
        OpBody::Doc(DocOp::DeleteBlockById {
            parent,
            target,
            block_id,
            id,
        }) => {
            if let Some(block) = document.find_block_by_id(*block_id) {
                let current = block.elem_id;
                let current_parent = document.block_parent(*block_id).flatten();
                document.delete_block_at(current_parent, current, *id);
            } else {
                document.delete_block_at(*parent, *target, *id);
            }
        }
        OpBody::Doc(DocOp::InsertText {
            block_elem,
            block_id,
            units,
        }) => {
            let block_elem = document.block_elem_id(*block_id).unwrap_or(*block_elem);
            // block_elem may be nested inside a blockquote; search the whole tree.
            let _ = document.with_block_mut(block_elem, |block| {
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
            block_id,
            id,
            targets,
            ..
        }) => {
            let block_elem = document.block_elem_id(*block_id).unwrap_or(*block_elem);
            let _ = document.with_block_mut(block_elem, |block| {
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
        OpBody::Doc(DocOp::SetMark {
            block_elem,
            block_id,
            id,
            kind,
            start,
            end,
            attrs,
            ..
        }) => {
            let block_elem = document.block_elem_id(*block_id).unwrap_or(*block_elem);
            let _ = document.with_block_mut(block_elem, |block| {
                block
                    .marks
                    .set_mark(*id, kind.clone(), *start, *end, attrs.clone(), *id);
            });
        }
        OpBody::Doc(DocOp::RemoveMark {
            block_elem,
            block_id,
            interval_id,
            id,
            observed,
            ..
        }) => {
            let block_elem = document.block_elem_id(*block_id).unwrap_or(*block_elem);
            let _ = document.with_block_mut(block_elem, |block| {
                block.marks.remove_mark(*interval_id, observed.clone(), *id);
            });
        }
        OpBody::Doc(DocOp::SetFrontmatterField { id, key, value }) => {
            let _ = document.set_frontmatter_field(key.clone(), value.clone(), *id);
        }
        OpBody::Doc(DocOp::InitializeFrontmatter { frontmatter, .. }) => {
            if document.frontmatter.is_none() {
                document.frontmatter = Some(frontmatter.clone());
            }
        }
        OpBody::Doc(DocOp::MoveBlocks {
            to_parent,
            id,
            blocks,
        }) => {
            document.move_blocks_at(*to_parent, blocks, *id);
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
                    kind_op: *id,
                    kind_observed: StateVector::new(),
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
            table_id,
            after,
            id,
            right_origin,
            cells,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    let row_id = block_id_from_op(*id);
                    let row = crate::doc::TableRow {
                        id: row_id,
                        elem_id: *id,
                        deleted: crate::core::LwwRegister::new(false, *id),
                        placement_observed: StateVector::new(),
                    };
                    table.rows.apply(SequenceOp::Insert {
                        after: *after,
                        id: *id,
                        value: row,
                        right_origin: *right_origin,
                    });
                    for cell in cells {
                        table.set_cell(row_id, cell.column_id, cell.value.clone(), *id);
                    }
                    table.resolve_pending_row_moves(row_id);
                }
            });
        }
        OpBody::Doc(DocOp::InsertTableColumn {
            table_elem,
            table_id,
            after,
            id,
            right_origin,
            alignment,
            header,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    let column_id = block_id_from_op(*id);
                    let column = crate::doc::TableColumn {
                        id: column_id,
                        elem_id: *id,
                        deleted: crate::core::LwwRegister::new(false, *id),
                        alignment: crate::core::LwwRegister::new(
                            alignment_from_wire(*alignment),
                            *id,
                        ),
                        alignment_observed: StateVector::new(),
                        placement_observed: StateVector::new(),
                    };
                    table.columns.apply(SequenceOp::Insert {
                        after: *after,
                        id: *id,
                        value: column,
                        right_origin: *right_origin,
                    });
                    table.set_cell(table.header_row_id(), column_id, header.clone(), *id);
                    table.resolve_pending_column_ops(column_id);
                }
            });
        }
        OpBody::Doc(DocOp::SetTableCell {
            table_elem,
            table_id,
            row_id,
            column_id,
            id,
            value,
            observed,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    table.set_cell_observed(
                        *row_id,
                        *column_id,
                        value.clone(),
                        *id,
                        observed.clone(),
                    );
                }
            });
        }
        OpBody::Doc(DocOp::DeleteTableRow {
            table_elem,
            table_id,
            target,
            id,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    table.remove_row(*target, *id);
                }
            });
        }
        OpBody::Doc(DocOp::DeleteTableRowById {
            table_elem,
            table_id,
            target,
            row_id,
            id,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    let current = table.row_by_id(*row_id).map_or(*target, |row| row.elem_id);
                    table.remove_row(current, *id);
                }
            });
        }
        OpBody::Doc(DocOp::DeleteTableColumnById {
            table_elem,
            table_id,
            target,
            column_id,
            id,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    let current = table
                        .column_by_id(*column_id)
                        .map_or(*target, |column| column.elem_id);
                    table.columns.delete(current, *id);
                }
            });
        }
        OpBody::Doc(DocOp::SetTableColumnAlignment {
            table_elem,
            table_id,
            column_id,
            id,
            alignment,
            observed,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    table.set_column_alignment_observed(
                        *column_id,
                        alignment_from_wire(*alignment),
                        *id,
                        observed.clone(),
                    );
                }
            });
        }
        OpBody::Doc(DocOp::MoveTableRow {
            table_elem,
            table_id,
            row_id,
            target,
            id,
            after,
            right_origin,
            observed,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    table.move_row(
                        *row_id,
                        *target,
                        *id,
                        *after,
                        *right_origin,
                        observed.clone(),
                    );
                }
            });
        }
        OpBody::Doc(DocOp::MoveTableColumn {
            table_elem,
            table_id,
            column_id,
            target,
            id,
            after,
            right_origin,
            observed,
            ..
        }) => {
            let table_elem = current_block_elem(document, *table_id, *table_elem);
            let _ = document.with_block_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    table.move_column(
                        *column_id,
                        *target,
                        *id,
                        *after,
                        *right_origin,
                        observed.clone(),
                    );
                }
            });
        }
        OpBody::Doc(DocOp::InsertListItem {
            list_elem,
            list_id,
            after,
            id,
            right_origin,
            task,
            ..
        }) => {
            let list_elem = current_block_elem(document, *list_id, *list_elem);
            document.insert_list_item_at(list_elem, *after, *id, *right_origin, *task);
        }
        OpBody::Doc(DocOp::DeleteListItemById {
            list_elem,
            item_id,
            target,
            id,
            ..
        }) => {
            document.delete_list_item_at(*list_elem, *item_id, *target, *id);
        }
        OpBody::Doc(DocOp::MoveListItem {
            from_list_elem,
            to_list_elem,
            list_id,
            item_id,
            target,
            id,
            after,
            right_origin,
            observed,
            ..
        }) => {
            let to_list_elem = current_block_elem(document, *list_id, *to_list_elem);
            document.move_list_item_at(&crate::doc::PendingListItemMove {
                from_list_elem: *from_list_elem,
                to_list_elem,
                list_id: *list_id,
                item_id: *item_id,
                target: *target,
                id: *id,
                after: *after,
                right_origin: *right_origin,
                observed: observed.clone(),
            });
        }
        OpBody::Doc(DocOp::SetListStyle {
            block_elem,
            block_id,
            style,
            id,
            observed,
            ..
        }) => {
            let block_elem = current_block_elem(document, *block_id, *block_elem);
            document.set_list_style(block_elem, *style, *id, observed.clone());
        }
        OpBody::Doc(DocOp::SetListItemTask {
            item_id,
            task,
            id,
            observed,
            ..
        }) => {
            document.set_list_item_task(*item_id, *task, *id, observed.clone());
        }
        OpBody::Doc(DocOp::SetCodeFence {
            block_elem,
            block_id,
            style,
            info,
            text,
            id,
            observed,
            ..
        }) => {
            let block_elem = current_block_elem(document, *block_id, *block_elem);
            document.set_code_fence(
                block_elem,
                *style,
                info.clone(),
                text.clone(),
                *id,
                observed.clone(),
            );
        }
        OpBody::Doc(DocOp::ConvertTextBlock {
            block_elem,
            block_id,
            kind,
            id,
            observed,
            ..
        }) => {
            let block_elem = current_block_elem(document, *block_id, *block_elem);
            document.convert_text_block(block_elem, *kind, *id, observed.clone());
        }
        OpBody::Doc(DocOp::ReplaceRawBlock {
            block_elem,
            block_id,
            raw,
            id,
            observed,
            ..
        }) => {
            let block_elem = current_block_elem(document, *block_id, *block_elem);
            document.replace_raw_block(block_elem, raw.clone(), *id, observed.clone());
        }
    }
}

pub(super) fn block_from_skeleton(skel: &BlockSkeleton, elem_id: OpId) -> Block {
    Block {
        id: skel.block_id,
        elem_id,
        kind_op: elem_id,
        kind_observed: StateVector::new(),
        kind: kind_from_skeleton(&skel.kind, elem_id),
        marks: MarkSet::new(),
    }
}

pub(super) fn kind_from_skeleton(kind: &BlockKindSkeleton, parent_elem: OpId) -> BlockKind {
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
        BlockKindSkeleton::List { style, items } => {
            let mut seq = Sequence::new();
            for item in items {
                let mut child_seq = Sequence::new();
                for child in &item.children {
                    let block = Block {
                        id: child.block.block_id,
                        elem_id: child.id,
                        kind_op: child.id,
                        kind_observed: StateVector::new(),
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
                    task: item.task,
                    task_op: item.task_op,
                    task_observed: StateVector::new(),
                    placement_observed: StateVector::new(),
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
                style: *style,
                items: seq,
                pending_moves: Vec::new(),
            }
        }
        BlockKindSkeleton::CodeFence { style, info, text } => BlockKind::CodeFence {
            style: *style,
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
                    kind_op: child.id,
                    kind_observed: StateVector::new(),
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
        BlockKindSkeleton::Table => BlockKind::Table {
            table: Box::new(Table::new(
                block_id_from_op(parent_elem),
                parent_elem,
                parent_elem,
            )),
        },
    }
}

pub(super) fn alignment_to_wire(alignment: &ColumnAlignment) -> ColumnAlignmentWire {
    match alignment {
        ColumnAlignment::Left => ColumnAlignmentWire::Left,
        ColumnAlignment::Center => ColumnAlignmentWire::Center,
        ColumnAlignment::Right => ColumnAlignmentWire::Right,
    }
}

pub(super) fn alignment_from_wire(alignment: ColumnAlignmentWire) -> ColumnAlignment {
    match alignment {
        ColumnAlignmentWire::Left => ColumnAlignment::Left,
        ColumnAlignmentWire::Center => ColumnAlignment::Center,
        ColumnAlignmentWire::Right => ColumnAlignment::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(counter: u64) -> OpId {
        OpId { counter, peer: 1 }
    }

    #[test]
    fn span_helpers_handle_invalid_and_empty_payloads() {
        assert_eq!(span_of_payload(b"not-json"), 1);

        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertText {
                block_elem: id(1),
                block_id: block_id_from_op(id(1)),
                units: vec![],
            }),
        };
        assert_eq!(
            operation_extent(&envelope),
            (
                OpId {
                    counter: 0,
                    peer: 0
                },
                1
            )
        );
        let payload = JsonOpCodec.encode(&envelope).unwrap();
        assert_eq!(span_of_payload(&payload), 1);
    }

    #[test]
    fn nested_kind_wire_round_trip_preserves_supported_shapes() {
        let mut text_counter = 4;
        let paragraph = Block::new(
            BlockKind::Paragraph {
                text: units_from_str("body", &mut text_counter, 1),
            },
            id(3),
        );
        let raw = Block::new(
            BlockKind::RawBlock {
                raw: ":::note".into(),
            },
            id(6),
        );
        let quote = Block::new(
            BlockKind::BlockQuote {
                children: Sequence::from_ordered(vec![(id(6), raw)]),
            },
            id(5),
        );
        let item = ListItem {
            id: block_id_from_op(id(2)),
            elem_id: id(2),
            task: None,
            task_op: id(2),
            task_observed: StateVector::new(),
            placement_observed: StateVector::new(),
            children: Sequence::from_ordered(vec![(id(3), paragraph), (id(5), quote)]),
        };
        let list = BlockKind::List {
            style: crate::doc::ListStyle {
                ordered: true,
                ..crate::doc::ListStyle::default()
            },
            items: Sequence::from_ordered(vec![(id(2), item)]),
            pending_moves: Vec::new(),
        };

        let skeleton = block_kind_to_skeleton(&list, false).unwrap();
        let BlockKindSkeleton::List { style, items } = &skeleton else {
            panic!("expected list skeleton");
        };
        assert!(style.ordered);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].children.len(), 2);
        assert!(matches!(
            items[0].children[1].block.kind,
            BlockKindSkeleton::BlockQuote { .. }
        ));

        let restored = kind_from_skeleton(&skeleton, id(1));
        let BlockKind::List { items, .. } = restored else {
            panic!("expected restored list");
        };
        let restored_item = items.iter_asc().next().unwrap();
        assert_eq!(restored_item.children.iter_asc().count(), 2);
        assert!(matches!(
            restored_item.children.iter_asc().nth(1).unwrap().kind,
            BlockKind::BlockQuote { .. }
        ));

        let table = BlockKind::Table {
            table: Box::new(Table::new(block_id_from_op(id(20)), id(20), id(20))),
        };
        let table_skeleton = block_kind_to_skeleton(&table, false).unwrap();
        assert!(matches!(&table_skeleton, BlockKindSkeleton::Table));
        assert!(matches!(
            kind_from_skeleton(&table_skeleton, id(20)),
            BlockKind::Table { .. }
        ));

        let code = BlockKind::CodeFence {
            style: crate::doc::CodeFenceStyle::default(),
            info: Some("rs".into()),
            text: "fn main() {}".into(),
        };
        assert!(matches!(
            kind_from_skeleton(&block_kind_to_skeleton(&code, false).unwrap(), id(30)),
            BlockKind::CodeFence { .. }
        ));

        let mut heading_counter = 32;
        let heading = BlockKind::Heading {
            level: 2,
            text: units_from_str("title", &mut heading_counter, 1),
        };
        assert!(matches!(
            block_kind_to_skeleton(&heading, false).unwrap(),
            BlockKindSkeleton::Heading { level: 2, text } if text == "title"
        ));
    }
}
