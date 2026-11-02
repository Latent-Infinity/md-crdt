use super::*;

pub(super) fn check_operation_id_is_max(id: OpId, env: &Envelope) -> Result<(), SessionError> {
    let (max, _span) = operation_extent(env);
    if id != max {
        return Err(SessionError::OperationIdMismatch);
    }
    Ok(())
}

pub(super) fn validate_operation_envelope(id: OpId, env: &Envelope) -> Result<(), SessionError> {
    let invalid = || {
        SessionError::Validation(ValidationError::MalformedOperation {
            op_id: id,
            kind: crate::sync::MalformedKind::InvalidSequence,
        })
    };
    check_peer_consistency(id, env)?;
    match &env.body {
        OpBody::Doc(DocOp::InsertText { units, .. }) => {
            let first = units.first().ok_or_else(invalid)?;
            if first.id.counter == 0 {
                return Err(invalid());
            }
            let last = first
                .id
                .counter
                .checked_add(units.len() as u64 - 1)
                .ok_or_else(invalid)?;
            for (offset, unit) in units.iter().enumerate() {
                if first.id.counter.checked_add(offset as u64) != Some(unit.id.counter) {
                    return Err(invalid());
                }
                if [unit.after, unit.right_origin]
                    .into_iter()
                    .flatten()
                    .any(|anchor| {
                        anchor.peer == first.id.peer
                            && anchor.counter >= unit.id.counter
                            && anchor.counter <= last
                    })
                {
                    return Err(invalid());
                }
            }
        }
        OpBody::Doc(DocOp::MoveBlocks {
            id: move_id,
            blocks,
            ..
        }) => {
            let first = blocks.first().ok_or_else(invalid)?;
            if first.id.counter == 0 {
                return Err(invalid());
            }
            let mut logical_ids = std::collections::BTreeSet::new();
            for (offset, block) in blocks.iter().enumerate() {
                if first.id.counter.checked_add(offset as u64) != Some(block.id.counter)
                    || !logical_ids.insert(block.block_id)
                {
                    return Err(invalid());
                }
            }
            if blocks.last().map(|block| block.id) != Some(*move_id) || *move_id != id {
                return Err(invalid());
            }
        }
        OpBody::Doc(DocOp::InsertBlock { id, block, .. }) => {
            fn checked_kind(kind: &BlockKindSkeleton, id: OpId) -> bool {
                if id.counter == 0 {
                    return false;
                }
                match kind {
                    BlockKindSkeleton::Paragraph { text }
                    | BlockKindSkeleton::Heading { text, .. } => id
                        .counter
                        .checked_add(grapheme_count(text) as u64)
                        .is_some(),
                    BlockKindSkeleton::BlockQuote { children } => children
                        .iter()
                        .all(|child| checked_kind(&child.block.kind, child.id)),
                    BlockKindSkeleton::List { items, .. } => items.iter().all(|item| {
                        item.id.counter != 0
                            && item
                                .children
                                .iter()
                                .all(|child| checked_kind(&child.block.kind, child.id))
                    }),
                    _ => true,
                }
            }
            if !checked_kind(&block.kind, *id) {
                return Err(invalid());
            }
        }
        OpBody::Doc(
            DocOp::DeleteBlock { subtree, .. }
            | DocOp::DeleteBlockById { subtree, .. }
            | DocOp::DeleteListItemById { subtree, .. },
        ) => {
            if let Some(root) = subtree.first() {
                let expected = match &env.body {
                    OpBody::Doc(DocOp::DeleteBlockById { block_id, .. }) => {
                        crate::doc::LogicalContainer::Block(*block_id)
                    }
                    OpBody::Doc(DocOp::DeleteListItemById { item_id, .. }) => {
                        crate::doc::LogicalContainer::Item(*item_id)
                    }
                    _ if matches!(root.container, crate::doc::LogicalContainer::Block(_)) => {
                        root.container
                    }
                    _ => return Err(invalid()),
                };
                let mut unique = std::collections::BTreeSet::new();
                if root.container != expected
                    || subtree.iter().any(|target| {
                        target.origin.counter == 0 || !unique.insert(target.container)
                    })
                {
                    return Err(invalid());
                }
            } else if !matches!(&env.body, OpBody::Doc(DocOp::DeleteBlock { .. })) {
                return Err(invalid());
            }
        }
        _ => {}
    }
    let (end, span) = operation_extent(env);
    if span == 0
        || end
            .counter
            .checked_sub(span - 1)
            .is_none_or(|start| start == 0)
    {
        return Err(invalid());
    }
    if let OpBody::Doc(
        DocOp::MoveBlocks { observed, .. }
        | DocOp::DeleteBlock { observed, .. }
        | DocOp::DeleteBlockById { observed, .. }
        | DocOp::DeleteListItemById { observed, .. },
    ) = &env.body
        && observed.get(end.peer).unwrap_or(0) >= end.counter.saturating_sub(span - 1)
    {
        return Err(invalid());
    }
    check_operation_id_is_max(id, env)
}

fn resolved_delete_target(
    metadata: &crate::doc::BlockTopology,
    envelope: &Envelope,
) -> Option<crate::doc::LogicalTarget> {
    use crate::doc::LogicalContainer;
    let (expected, physical) = match &envelope.body {
        OpBody::Doc(DocOp::DeleteBlock { target, .. }) => (None, *target),
        OpBody::Doc(DocOp::DeleteBlockById {
            block_id, target, ..
        }) => (Some(LogicalContainer::Block(*block_id)), *target),
        OpBody::Doc(DocOp::DeleteListItemById {
            item_id, target, ..
        }) => (Some(LogicalContainer::Item(*item_id)), *target),
        _ => return None,
    };
    metadata.resolve_deletion_target(expected, physical)
}

pub(super) fn envelope_dependency_work(envelope: &Envelope) -> usize {
    fn kind_work(kind: &BlockKindSkeleton) -> usize {
        match kind {
            BlockKindSkeleton::Paragraph { text } | BlockKindSkeleton::Heading { text, .. } => {
                grapheme_count(text)
            }
            BlockKindSkeleton::BlockQuote { children } => {
                children.iter().fold(0usize, |sum, child| {
                    sum.saturating_add(1)
                        .saturating_add(kind_work(&child.block.kind))
                })
            }
            BlockKindSkeleton::List { items, .. } => items.iter().fold(0usize, |sum, item| {
                item.children
                    .iter()
                    .fold(sum.saturating_add(1), |sum, child| {
                        sum.saturating_add(1)
                            .saturating_add(kind_work(&child.block.kind))
                    })
            }),
            _ => 0,
        }
    }
    let elements = match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock { block, .. }) => kind_work(&block.kind),
        OpBody::Doc(DocOp::InsertText { units, .. }) => units.len().saturating_mul(2),
        OpBody::Doc(DocOp::DeleteText { targets, .. }) => targets.len(),
        OpBody::Doc(
            DocOp::DeleteBlock { subtree, .. }
            | DocOp::DeleteBlockById { subtree, .. }
            | DocOp::DeleteListItemById { subtree, .. },
        ) => subtree.len().saturating_add(1).saturating_mul(2),
        OpBody::Doc(DocOp::MoveBlocks { blocks, .. }) => blocks.len().saturating_mul(3),
        OpBody::Doc(DocOp::SplitBlock { units, .. } | DocOp::MergeBlocks { units, .. }) => {
            units.len()
        }
        OpBody::Doc(DocOp::InsertTableRow { cells, .. }) => cells.len(),
        _ => 0,
    };
    8usize.saturating_add(elements).saturating_add(
        envelope_observed_frontier(envelope).map_or(0, |frontier| frontier.iter().count()),
    )
}

/// Borrowed DTO-shaped views keep live admission and snapshot accounting identical
/// without constructing a DTO, cloning a pending value, or allocating JSON bytes.
trait PendingValue: Clone {
    type Dto;
    fn serialize_view<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
}

struct ValueRef<'a, T>(&'a T);

impl<T: PendingValue> serde::Serialize for ValueRef<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_view(serializer)
    }
}

#[derive(serde::Serialize)]
enum PendingOpRef<V> {
    Insert {
        after: Option<OpId>,
        id: OpId,
        value: V,
        right_origin: Option<OpId>,
    },
    Delete {
        target: OpId,
        id: OpId,
    },
}

struct PendingOpsRef<'a, T>(&'a Sequence<T>);
struct ElementsRef<'a, T>(&'a Sequence<T>);

impl<T: PendingValue> serde::Serialize for PendingOpsRef<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut output = serializer.serialize_seq(Some(self.0.pending_op_count()))?;
        for operation in self.0.pending_ops_iter() {
            let operation = match operation {
                SequenceOp::Insert {
                    after,
                    id,
                    value,
                    right_origin,
                } => PendingOpRef::Insert {
                    after: *after,
                    id: *id,
                    value: ValueRef(value),
                    right_origin: *right_origin,
                },
                SequenceOp::Delete { target, id } => PendingOpRef::Delete {
                    target: *target,
                    id: *id,
                },
            };
            output.serialize_element(&operation)?;
        }
        output.end()
    }
}

impl<T: PendingValue> serde::Serialize for ElementsRef<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        #[derive(serde::Serialize)]
        struct ElementRef<V> {
            id: OpId,
            value: Option<V>,
            after: Option<OpId>,
            right_origin: Option<OpId>,
        }
        let mut output = serializer.serialize_seq(None)?;
        for element in self.0.iter_all() {
            output.serialize_element(&ElementRef {
                id: element.id,
                value: element.value.as_ref().map(ValueRef),
                after: element.after,
                right_origin: element.right_origin,
            })?;
        }
        output.end()
    }
}

struct SequenceRef<'a, T>(&'a Sequence<T>);

impl<T: PendingValue> serde::Serialize for SequenceRef<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut output = serializer.serialize_struct("SequenceDto", 2)?;
        output.serialize_field("elements", &ElementsRef(self.0))?;
        output.serialize_field("pending", &PendingOpsRef(self.0))?;
        output.end()
    }
}

impl PendingValue for TextUnit {
    type Dto = snapshot::TextUnitDto;

    fn serialize_view<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::Serialize;
        #[derive(serde::Serialize)]
        struct TextRef<'a> {
            grapheme: &'a str,
        }
        TextRef {
            grapheme: &self.grapheme,
        }
        .serialize(serializer)
    }
}

#[derive(serde::Serialize)]
enum KindRef<'a> {
    Paragraph {
        units: SequenceRef<'a, TextUnit>,
    },
    Heading {
        level: u8,
        units: SequenceRef<'a, TextUnit>,
    },
    List {
        style: &'a crate::doc::ListStyle,
        items: SequenceRef<'a, ListItem>,
        pending_moves: &'a [crate::doc::PendingListItemMove],
    },
    CodeFence {
        style: &'a crate::doc::CodeFenceStyle,
        info: &'a Option<String>,
        text: &'a str,
    },
    BlockQuote {
        children: SequenceRef<'a, Block>,
    },
    RawBlock {
        raw: &'a str,
    },
    Table {
        table: TableRef<'a>,
    },
}

fn kind_ref(kind: &BlockKind) -> KindRef<'_> {
    match kind {
        BlockKind::Paragraph { text } => KindRef::Paragraph {
            units: SequenceRef(text),
        },
        BlockKind::Heading { level, text } => KindRef::Heading {
            level: *level,
            units: SequenceRef(text),
        },
        BlockKind::List {
            style,
            items,
            pending_moves,
        } => KindRef::List {
            style,
            items: SequenceRef(items),
            pending_moves,
        },
        BlockKind::CodeFence { style, info, text } => KindRef::CodeFence { style, info, text },
        BlockKind::BlockQuote { children } => KindRef::BlockQuote {
            children: SequenceRef(children),
        },
        BlockKind::RawBlock { raw } => KindRef::RawBlock { raw },
        BlockKind::Table { table } => KindRef::Table {
            table: TableRef(table),
        },
    }
}

impl PendingValue for Block {
    type Dto = snapshot::BlockDto;

    fn serialize_view<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut output = serializer.serialize_struct("BlockDto", 7)?;
        output.serialize_field("id", &self.id)?;
        output.serialize_field("elem_id", &self.elem_id)?;
        output.serialize_field("kind_op", &self.kind_op)?;
        output.serialize_field("kind_observed", &self.kind_observed)?;
        output.serialize_field("kind_conflicts", &self.kind_conflicts)?;
        output.serialize_field("kind", &kind_ref(&self.kind))?;
        output.serialize_field("marks", &self.marks)?;
        output.end()
    }
}

impl PendingValue for ListItem {
    type Dto = snapshot::ListItemDto;

    fn serialize_view<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut output = serializer.serialize_struct("ListItemDto", 9)?;
        output.serialize_field("id", &self.id)?;
        output.serialize_field("elem_id", &self.elem_id)?;
        output.serialize_field("task", &self.task)?;
        output.serialize_field("task_op", &self.task_op)?;
        output.serialize_field("task_observed", &self.task_observed)?;
        output.serialize_field("task_conflicts", &self.task_conflicts)?;
        output.serialize_field("placement_observed", &self.placement_observed)?;
        output.serialize_field("placement_conflicts", &self.placement_conflicts)?;
        output.serialize_field("children", &SequenceRef(&self.children))?;
        output.end()
    }
}

impl PendingValue for crate::doc::TableColumn {
    type Dto = snapshot::TableColumnDto;

    fn serialize_view<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut output = serializer.serialize_struct("TableColumnDto", 8)?;
        output.serialize_field("id", &self.id)?;
        output.serialize_field("elem_id", &self.elem_id)?;
        output.serialize_field("deleted", &self.deleted)?;
        output.serialize_field("alignment", &self.alignment)?;
        output.serialize_field("alignment_observed", &self.alignment_observed)?;
        output.serialize_field("alignment_conflicts", &self.alignment_conflicts)?;
        output.serialize_field("placement_observed", &self.placement_observed)?;
        output.serialize_field("placement_conflicts", &self.placement_conflicts)?;
        output.end()
    }
}

impl PendingValue for crate::doc::TableRow {
    type Dto = snapshot::TableRowDto;

    fn serialize_view<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut output = serializer.serialize_struct("TableRowDto", 5)?;
        output.serialize_field("id", &self.id)?;
        output.serialize_field("elem_id", &self.elem_id)?;
        output.serialize_field("deleted", &self.deleted)?;
        output.serialize_field("placement_observed", &self.placement_observed)?;
        output.serialize_field("placement_conflicts", &self.placement_conflicts)?;
        output.end()
    }
}

struct TableCellsRef<'a>(&'a Table);

impl serde::Serialize for TableCellsRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        #[derive(serde::Serialize)]
        struct CellRef<'a> {
            row_id: crate::doc::RowId,
            column_id: crate::doc::ColumnId,
            value: snapshot::LwwDto<&'a crate::doc::CellContent>,
            observed: &'a StateVector,
            conflicts: &'a [crate::core::causal::CausalCandidate<crate::doc::CellContent>],
        }
        let mut output = serializer.serialize_seq(Some(self.0.cells.len()))?;
        for (address, cell) in &self.0.cells {
            output.serialize_element(&CellRef {
                row_id: address.row_id,
                column_id: address.column_id,
                value: snapshot::LwwDto {
                    value: &cell.value,
                    op_id: cell.op_id,
                },
                observed: &cell.observed,
                conflicts: &cell.conflicts,
            })?;
        }
        output.end()
    }
}

struct TableRef<'a>(&'a Table);

impl serde::Serialize for TableRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let table = self.0;
        let mut output = serializer.serialize_struct("TableDto", 9)?;
        output.serialize_field("id", &table.id)?;
        output.serialize_field("elem_id", &table.elem_id)?;
        output.serialize_field("deleted", &table.deleted)?;
        output.serialize_field("columns", &SequenceRef(&table.columns))?;
        output.serialize_field("rows", &SequenceRef(&table.rows))?;
        output.serialize_field("cells", &TableCellsRef(table))?;
        output.serialize_field("pending_row_moves", &table.pending_row_moves)?;
        output.serialize_field("pending_column_moves", &table.pending_column_moves)?;
        output.serialize_field(
            "pending_column_alignments",
            &table.pending_column_alignments,
        )?;
        output.end()
    }
}

fn serialized_bytes(value: &impl serde::Serialize) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    if serde_json::to_writer(&mut counter, value).is_ok() {
        counter.0
    } else {
        usize::MAX
    }
}

fn pending_sequence_resources<T: PendingValue>(sequence: &Sequence<T>) -> (usize, usize) {
    let count = sequence.pending_op_count();
    if count == 0 {
        return (0, 0);
    }
    (
        count,
        serialized_bytes(&PendingOpsRef(sequence)).saturating_add(
            count.saturating_mul(std::mem::size_of::<snapshot::SequenceOpDto<T::Dto>>()),
        ),
    )
}

fn pending_slice_resources<T: serde::Serialize>(pending: &[T]) -> (usize, usize) {
    if pending.is_empty() {
        return (0, 0);
    }
    (
        pending.len(),
        serialized_bytes(&pending)
            .saturating_add(pending.len().saturating_mul(std::mem::size_of::<T>())),
    )
}

fn add_resources(total: &mut (usize, usize), extra: (usize, usize)) {
    total.0 = total.0.saturating_add(extra.0);
    total.1 = total.1.saturating_add(extra.1);
}

fn sequence_values<T: Clone>(sequence: &Sequence<T>) -> impl Iterator<Item = &T> {
    sequence
        .iter_all()
        .filter_map(|element| element.value.as_ref())
        .chain(
            sequence
                .pending_ops_iter()
                .filter_map(|operation| match operation {
                    SequenceOp::Insert { value, .. } => Some(value),
                    SequenceOp::Delete { .. } => None,
                }),
        )
}

fn block_pending_resources(block: &Block) -> (usize, usize) {
    match &block.kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
            pending_sequence_resources(text)
        }
        BlockKind::BlockQuote { children } => blocks_pending_resources(children),
        BlockKind::List {
            items,
            pending_moves,
            ..
        } => {
            let mut resources = pending_sequence_resources(items);
            add_resources(&mut resources, pending_slice_resources(pending_moves));
            for item in sequence_values(items) {
                add_resources(&mut resources, blocks_pending_resources(&item.children));
            }
            resources
        }
        BlockKind::Table { table } => {
            let mut resources = pending_sequence_resources(&table.columns);
            add_resources(&mut resources, pending_sequence_resources(&table.rows));
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
            resources
        }
        _ => (0, 0),
    }
}

fn blocks_pending_resources(sequence: &Sequence<Block>) -> (usize, usize) {
    let mut resources = pending_sequence_resources(sequence);
    for block in sequence_values(sequence) {
        add_resources(&mut resources, block_pending_resources(block));
    }
    resources
}

pub(super) fn document_pending_resources(document: &Document) -> (usize, usize) {
    blocks_pending_resources(document.blocks())
}

#[derive(Default)]
pub(super) struct DocumentPendingStatus {
    pub count: usize,
}

impl DocumentPendingStatus {
    fn add(&mut self, other: Self) {
        self.count = self.count.saturating_add(other.count);
    }
}

fn blocks_pending_status(sequence: &Sequence<Block>) -> DocumentPendingStatus {
    let mut status = DocumentPendingStatus {
        count: sequence.pending_op_count(),
    };
    for block in sequence_values(sequence) {
        status.add(block_pending_status(block));
    }
    status
}

fn block_pending_status(block: &Block) -> DocumentPendingStatus {
    let mut status = DocumentPendingStatus::default();
    match &block.kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
            status.count = text.pending_op_count();
        }
        BlockKind::BlockQuote { children } => status.add(blocks_pending_status(children)),
        BlockKind::List {
            items,
            pending_moves,
            ..
        } => {
            status.count = items.pending_op_count().saturating_add(pending_moves.len());
            for item in sequence_values(items) {
                status.add(blocks_pending_status(&item.children));
            }
        }
        BlockKind::Table { table } => {
            let moves = table
                .pending_row_moves
                .len()
                .saturating_add(table.pending_column_moves.len())
                .saturating_add(table.pending_column_alignments.len());
            status.count = table
                .rows
                .pending_op_count()
                .saturating_add(table.columns.pending_op_count())
                .saturating_add(moves);
        }
        _ => {}
    }
    status
}

pub(super) fn document_pending_status(document: &Document) -> DocumentPendingStatus {
    blocks_pending_status(document.blocks())
}

pub(super) fn staged_pending_count(block: Option<&Block>) -> usize {
    block.map_or(0, |block| block_pending_status(block).count)
}

/// Document-level deferred primitives also make their origin's receipt vector
/// an unsafe observation frontier. Borrow their IDs, including hidden values.
pub(super) fn lower_document_pending_frontier(document: &Document, frontier: &mut StateVector) {
    fn lower(frontier: &mut StateVector, id: OpId) {
        let before = id.counter.saturating_sub(1);
        if frontier.get(id.peer).unwrap_or(0) > before {
            frontier.set(id.peer, before);
        }
    }
    fn sequence<T: Clone>(sequence: &Sequence<T>, frontier: &mut StateVector) {
        for operation in sequence.pending_ops_iter() {
            let id = match operation {
                SequenceOp::Insert { id, .. } | SequenceOp::Delete { id, .. } => *id,
            };
            lower(frontier, id);
        }
    }
    fn walk(blocks: &Sequence<Block>, frontier: &mut StateVector) {
        sequence(blocks, frontier);
        for block in sequence_values(blocks) {
            match &block.kind {
                BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
                    sequence(text, frontier)
                }
                BlockKind::BlockQuote { children } => walk(children, frontier),
                BlockKind::List {
                    items,
                    pending_moves,
                    ..
                } => {
                    sequence(items, frontier);
                    for movement in pending_moves {
                        lower(frontier, movement.id);
                    }
                    for item in sequence_values(items) {
                        walk(&item.children, frontier);
                    }
                }
                BlockKind::Table { table } => {
                    sequence(&table.rows, frontier);
                    sequence(&table.columns, frontier);
                    for movement in table
                        .pending_row_moves
                        .iter()
                        .chain(&table.pending_column_moves)
                    {
                        lower(frontier, movement.id);
                    }
                    for alignment in &table.pending_column_alignments {
                        lower(frontier, alignment.id);
                    }
                }
                _ => {}
            }
        }
    }
    walk(document.blocks(), frontier);
}
/// Move imported causal queues into the session's deferred layer before restore.
/// Dropping a document container must not expose its unmaterialized receipts.
pub(super) fn take_document_causal_pending(
    document: &mut DocumentDto,
    mut visit: impl FnMut(Envelope) -> Result<(), SnapshotError>,
) -> Result<(), SnapshotError> {
    use super::snapshot::{BlockDto, BlockKindDto, SequenceDto, SequenceOpDto};

    fn values<T>(sequence: &mut SequenceDto<T>) -> impl Iterator<Item = &mut T> {
        sequence
            .elements
            .iter_mut()
            .filter_map(|element| element.value.as_mut())
            .chain(
                sequence
                    .pending
                    .iter_mut()
                    .filter_map(|operation| match operation {
                        SequenceOpDto::Insert { value, .. } => Some(value),
                        SequenceOpDto::Delete { .. } => None,
                    }),
            )
    }

    fn walk(
        blocks: &mut SequenceDto<BlockDto>,
        visit: &mut impl FnMut(Envelope) -> Result<(), SnapshotError>,
    ) -> Result<(), SnapshotError> {
        for block in values(blocks) {
            let mut emit = |operation| {
                visit(Envelope {
                    version: WIRE_VERSION,
                    body: OpBody::Doc(operation),
                })
            };
            match &mut block.kind {
                BlockKindDto::BlockQuote { children } => walk(children, visit)?,
                BlockKindDto::List {
                    items,
                    pending_moves,
                    ..
                } => {
                    for movement in std::mem::take(pending_moves) {
                        emit(DocOp::MoveListItem {
                            from_list_elem: movement.from_list_elem,
                            to_list_elem: movement.to_list_elem,
                            list_id: movement.list_id,
                            item_id: movement.item_id,
                            target: movement.target,
                            id: movement.id,
                            after: movement.after,
                            right_origin: movement.right_origin,
                            observed: movement.observed,
                        })?;
                    }
                    for item in values(items) {
                        walk(&mut item.children, visit)?;
                    }
                }
                BlockKindDto::Table { table } => {
                    for movement in std::mem::take(&mut table.pending_row_moves) {
                        emit(DocOp::MoveTableRow {
                            table_elem: table.elem_id,
                            table_id: table.id,
                            row_id: movement.logical_id,
                            target: movement.target,
                            id: movement.id,
                            after: movement.after,
                            right_origin: movement.right_origin,
                            observed: movement.observed,
                        })?;
                    }
                    for movement in std::mem::take(&mut table.pending_column_moves) {
                        emit(DocOp::MoveTableColumn {
                            table_elem: table.elem_id,
                            table_id: table.id,
                            column_id: movement.logical_id,
                            target: movement.target,
                            id: movement.id,
                            after: movement.after,
                            right_origin: movement.right_origin,
                            observed: movement.observed,
                        })?;
                    }
                    for alignment in std::mem::take(&mut table.pending_column_alignments) {
                        emit(DocOp::SetTableColumnAlignment {
                            table_elem: table.elem_id,
                            table_id: table.id,
                            column_id: alignment.column_id,
                            alignment: alignment_to_wire(&alignment.alignment),
                            id: alignment.id,
                            observed: alignment.observed,
                        })?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    walk(&mut document.blocks, &mut visit)
}

fn logical_origin(id: BlockId) -> OpId {
    let raw = id.as_u128();
    OpId {
        peer: (raw >> 64) as u64,
        counter: raw as u64,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AdmissionTarget {
    Blocks(Option<OpId>),
    Text(BlockId),
    Items(BlockId),
    Rows(BlockId),
    Columns(BlockId),
}

/// Only identifiers created by earlier materialized effects are staged. Payloads
/// stay in the prepared envelope; contiguous text runs occupy one interval.
#[derive(Default)]
pub(super) struct AdmissionInsertions {
    spans: BTreeMap<(AdmissionTarget, PeerId), BTreeMap<u64, u64>>,
    created_text: std::collections::BTreeSet<BlockId>,
    created_lists: std::collections::BTreeSet<BlockId>,
    created_tables: std::collections::BTreeSet<BlockId>,
    parent_aliases: BTreeMap<OpId, OpId>,
    block_ids: BTreeMap<OpId, BlockId>,
    baseline: Option<StateVector>,
    table_registers: BTreeMap<(AdmissionTarget, BlockId), PlannedRegister<crate::doc::Placement>>,
    item_registers: BTreeMap<BlockId, PlannedRegister<crate::doc::ListItemPlacement>>,
    topology: AdmissionTopology,
    item_tombstones: std::collections::BTreeSet<(BlockId, OpId)>,
    planned_placements: Option<crate::doc::BlockTopology>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AdmissionObject {
    Block(BlockId),
    Item(BlockId),
    New(usize),
}

#[derive(Clone, Copy)]
struct AdmissionNode {
    parent: Option<AdmissionObject>,
    physical: OpId,
    deleted: bool,
    container: bool,
}

#[derive(Default)]
struct AdmissionTopology {
    nodes: BTreeMap<AdmissionObject, AdmissionNode>,
    blocks: BTreeMap<BlockId, AdmissionObject>,
    items: BTreeMap<BlockId, AdmissionObject>,
    physical: BTreeMap<OpId, AdmissionObject>,
    next: usize,
}

impl AdmissionTopology {
    fn block(&self, document: &Document, id: BlockId) -> Option<AdmissionObject> {
        self.blocks.get(&id).copied().or_else(|| {
            document
                .find_block_by_id(id)
                .map(|_| AdmissionObject::Block(id))
        })
    }

    fn item(&self, document: &Document, id: BlockId) -> Option<AdmissionObject> {
        self.items.get(&id).copied().or_else(|| {
            document
                .find_list_item_by_id(id)
                .map(|_| AdmissionObject::Item(id))
        })
    }

    fn frozen_physical(document: &Document, id: OpId) -> Option<AdmissionObject> {
        document
            .logical_block_for_elem(id)
            .map(AdmissionObject::Block)
            .or_else(|| {
                document
                    .logical_item_for_elem(id)
                    .map(AdmissionObject::Item)
            })
    }

    fn node(&self, document: &Document, object: AdmissionObject) -> Option<AdmissionNode> {
        if let Some(node) = self.nodes.get(&object) {
            return Some(*node);
        }
        match object {
            AdmissionObject::Block(id) => {
                let block = document.find_block_by_id(id)?;
                let parent = document
                    .block_parent(id)?
                    .and_then(|parent| Self::frozen_physical(document, parent));
                Some(AdmissionNode {
                    parent,
                    physical: block.elem_id,
                    deleted: false,
                    container: matches!(block.kind, BlockKind::BlockQuote { .. }),
                })
            }
            AdmissionObject::Item(id) => {
                let (list, _, physical) = document.list_item_placement(id)?;
                Some(AdmissionNode {
                    parent: Some(AdmissionObject::Block(list)),
                    physical,
                    deleted: false,
                    container: true,
                })
            }
            AdmissionObject::New(_) => None,
        }
    }

    fn active(&self, document: &Document, mut object: AdmissionObject) -> bool {
        // Acyclic document topology is preserved by the same cycle checks as publication.
        loop {
            let Some(node) = self.node(document, object) else {
                return false;
            };
            if node.deleted {
                return false;
            }
            let Some(parent) = node.parent else {
                return true;
            };
            object = parent;
        }
    }

    fn physical_object(&self, document: &Document, id: OpId) -> Option<AdmissionObject> {
        let object = self
            .physical
            .get(&id)
            .copied()
            .or_else(|| Self::frozen_physical(document, id))?;
        self.active(document, object).then_some(object)
    }

    fn container(&self, document: &Document, id: OpId) -> bool {
        self.physical_object(document, id)
            .and_then(|object| self.node(document, object))
            .is_some_and(|node| node.container)
    }

    fn create(
        &mut self,
        document: &Document,
        logical: BlockId,
        node: AdmissionNode,
        item: bool,
        error_id: OpId,
    ) -> Result<(), SessionError> {
        let previous = if item {
            self.item(document, logical)
        } else {
            self.block(document, logical)
        };
        if previous.is_some_and(|object| self.active(document, object))
            || self.physical_object(document, node.physical).is_some()
        {
            return Err(SessionError::Validation(
                ValidationError::MalformedOperation {
                    op_id: error_id,
                    kind: crate::sync::MalformedKind::InvalidSequence,
                },
            ));
        }
        let object = AdmissionObject::New(self.next);
        self.next += 1;
        self.nodes.insert(object, node);
        if item {
            self.items.insert(logical, object);
        } else {
            self.blocks.insert(logical, object);
        }
        self.physical.insert(node.physical, object);
        Ok(())
    }

    fn delete(&mut self, document: &Document, object: AdmissionObject) {
        if let Some(mut node) = self.node(document, object) {
            node.deleted = true;
            self.nodes.insert(object, node);
        }
    }

    fn relocate(
        &mut self,
        document: &Document,
        object: AdmissionObject,
        physical: OpId,
        parent: Option<AdmissionObject>,
    ) {
        if let Some(mut node) = self.node(document, object) {
            node.physical = physical;
            node.parent = parent;
            self.nodes.insert(object, node);
            self.physical.insert(physical, object);
        }
    }
}

struct PlannedRegister<T> {
    value: T,
    id: OpId,
    observed: StateVector,
    conflicts: Vec<crate::core::causal::CausalCandidate<T>>,
    implicit: bool,
}

impl<T: Copy> PlannedRegister<T> {
    fn copy_from(
        value: T,
        id: OpId,
        observed: &StateVector,
        conflicts: &[crate::core::causal::CausalCandidate<T>],
        implicit: bool,
        work: &mut usize,
    ) -> Option<Self> {
        for context in
            std::iter::once(observed).chain(conflicts.iter().map(|candidate| &candidate.observed))
        {
            for _ in context.iter() {
                if !spend_admission_work(work, 1) {
                    return None;
                }
            }
        }
        if !spend_admission_work(work, conflicts.len().saturating_add(1)) {
            return None;
        }
        Some(Self {
            value,
            id,
            observed: observed.clone(),
            conflicts: conflicts.to_vec(),
            implicit,
        })
    }

    fn selected(
        &self,
        value: T,
        id: OpId,
        observed: &StateVector,
        frontier: &StateVector,
        work: &mut usize,
    ) -> Option<(T, OpId)> {
        use crate::core::causal::{WinnerAfter, winner_after};
        if self.implicit {
            return Some((value, id));
        }
        if !spend_admission_work(
            work,
            self.conflicts.len().saturating_mul(2).saturating_add(1),
        ) {
            return None;
        }
        Some(
            match winner_after(self.id, &self.observed, &self.conflicts, id, observed) {
                WinnerAfter::Current => (self.value, self.id),
                WinnerAfter::Incoming => (value, id),
                WinnerAfter::Conflict(index) => {
                    let candidate = &self.conflicts[index];
                    if !context_ready_after(
                        candidate.op_id,
                        &candidate.observed,
                        frontier,
                        id,
                        work,
                    ) {
                        return None;
                    }
                    (candidate.value, candidate.op_id)
                }
            },
        )
    }

    fn accept(&mut self, value: T, id: OpId, observed: &StateVector, work: &mut usize) -> bool {
        for _ in observed.iter() {
            if !spend_admission_work(work, 1) {
                return false;
            }
        }
        if self.implicit {
            self.value = value;
            self.id = id;
            self.observed = observed.clone();
            self.implicit = false;
        } else {
            crate::core::causal::apply_write(
                &mut self.value,
                &mut self.id,
                &mut self.observed,
                &mut self.conflicts,
                crate::core::causal::CausalCandidate {
                    value,
                    op_id: id,
                    observed: observed.clone(),
                },
            );
        }
        true
    }
}

#[derive(Clone, Copy)]
struct PlacementWrite<'a> {
    target: AdmissionTarget,
    logical: BlockId,
    id: OpId,
    after: Option<OpId>,
    right_origin: Option<OpId>,
    observed: &'a StateVector,
}

impl AdmissionInsertions {
    fn object_identity(&self, object: AdmissionObject) -> Option<(bool, BlockId)> {
        match object {
            AdmissionObject::Block(id) => Some((false, id)),
            AdmissionObject::Item(id) => Some((true, id)),
            AdmissionObject::New(_) => {
                self.topology
                    .blocks
                    .iter()
                    .find_map(|(id, candidate)| (*candidate == object).then_some((false, *id)))
                    .or_else(|| {
                        self.topology.items.iter().find_map(|(id, candidate)| {
                            (*candidate == object).then_some((true, *id))
                        })
                    })
            }
        }
    }

    fn placement_container(
        &self,
        document: &Document,
        element: OpId,
    ) -> Option<crate::doc::LogicalContainer> {
        let object = self
            .topology
            .physical
            .get(&element)
            .copied()
            .or_else(|| AdmissionTopology::frozen_physical(document, element))?;
        self.object_identity(object).map(|(item, id)| {
            if item {
                crate::doc::LogicalContainer::Item(id)
            } else {
                crate::doc::LogicalContainer::Block(id)
            }
        })
    }

    fn ensure_placement_projection(&mut self, document: &Document) {
        if self.planned_placements.is_some() {
            return;
        }
        let mut metadata = document.topology_state();
        for (logical, object) in &self.topology.blocks {
            let Some(node) = self.topology.node(document, *object) else {
                continue;
            };
            let parent = node
                .parent
                .and_then(|parent| self.object_identity(parent))
                .map(|(item, id)| {
                    if item {
                        crate::doc::LogicalContainer::Item(id)
                    } else {
                        crate::doc::LogicalContainer::Block(id)
                    }
                });
            let replace = metadata
                .initial
                .get(logical)
                .is_some_and(|old| old.id != node.physical);
            let parent_origin = parent
                .and_then(|parent| metadata.current_target(parent))
                .map(|target| target.origin);
            metadata.record_creation(
                *logical,
                crate::doc::BlockPlacement {
                    parent,
                    id: node.physical,
                    after: None,
                    right_origin: None,
                    parent_origin,
                },
                replace,
            );
        }
        for (logical, object) in &self.topology.items {
            let Some(node) = self.topology.node(document, *object) else {
                continue;
            };
            let Some((_, list_id)) = node.parent.and_then(|parent| self.object_identity(parent))
            else {
                continue;
            };
            let placement = self
                .item_registers
                .get(logical)
                .map(|register| register.value.placement)
                .unwrap_or(crate::doc::Placement {
                    after: None,
                    right_origin: None,
                });
            let list_origin = metadata.initial[&list_id].id;
            metadata.record_item_creation(
                *logical,
                crate::doc::ItemPlacement {
                    list_id,
                    id: node.physical,
                    after: placement.after,
                    right_origin: placement.right_origin,
                    list_origin,
                },
                false,
            );
        }
        self.planned_placements = Some(metadata);
    }

    fn refresh_placement_projection(&mut self, document: &Document) {
        let deleted: Vec<_> = self
            .topology
            .nodes
            .iter()
            .filter(|(_, node)| node.deleted)
            .filter_map(|(object, _)| {
                let (item, logical) = self.object_identity(*object)?;
                let current = if item {
                    self.topology.item(document, logical)
                } else {
                    self.topology.block(document, logical)
                };
                (current == Some(*object)).then_some((item, logical))
            })
            .collect();
        let Some(metadata) = &mut self.planned_placements else {
            return;
        };
        for (item, logical) in deleted {
            let container = if item {
                crate::doc::LogicalContainer::Item(logical)
            } else {
                crate::doc::LogicalContainer::Block(logical)
            };
            if let Some(target) = metadata.current_target(container) {
                metadata.direct_deleted.insert(target);
            }
        }
        metadata.recompute_deletions();
        let removed_blocks = metadata.deleted.clone();
        let removed_items = metadata.deleted_items.clone();
        let selected = metadata.project();
        for logical in removed_blocks {
            if let Some(object) = self.topology.block(document, logical) {
                self.topology.delete(document, object);
            }
        }
        for logical in removed_items {
            if let Some(object) = self.topology.item(document, logical) {
                if let Some(node) = self.topology.node(document, object)
                    && let Some((false, list)) =
                        node.parent.and_then(|parent| self.object_identity(parent))
                {
                    self.item_tombstones.insert((list, node.physical));
                }
                self.topology.delete(document, object);
            }
        }
        for (logical, placement) in selected.blocks {
            let Some(object) = self.topology.block(document, logical) else {
                continue;
            };
            let parent = placement.parent.map(|parent| match parent {
                crate::doc::LogicalContainer::Block(id) => self
                    .topology
                    .block(document, id)
                    .unwrap_or(AdmissionObject::Block(id)),
                crate::doc::LogicalContainer::Item(id) => self
                    .topology
                    .item(document, id)
                    .unwrap_or(AdmissionObject::Item(id)),
            });
            self.topology
                .relocate(document, object, placement.id, parent);
        }
        for (logical, placement) in selected.items {
            let Some(object) = self.topology.item(document, logical) else {
                continue;
            };
            let parent = self
                .topology
                .block(document, placement.list_id)
                .unwrap_or(AdmissionObject::Block(placement.list_id));
            self.topology
                .relocate(document, object, placement.id, Some(parent));
        }
    }

    fn placement_ready(
        &mut self,
        document: &Document,
        write: PlacementWrite<'_>,
        frontier: &StateVector,
        work: &mut usize,
    ) -> bool {
        use crate::doc::{ListItemPlacement, Placement};
        let incoming = Placement {
            after: write.after,
            right_origin: write.right_origin,
        };
        match write.target {
            AdmissionTarget::Items(list) => {
                if !self.list_exists(document, list)
                    || self
                        .topology
                        .item(document, write.logical)
                        .is_none_or(|object| !self.topology.active(document, object))
                {
                    return true;
                }
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    self.item_registers.entry(write.logical)
                {
                    let Some(state) = document.item_placement_register(write.logical) else {
                        return true;
                    };
                    let Some(register) = PlannedRegister::copy_from(
                        state.value,
                        state.id,
                        &state.observed,
                        &state.conflicts,
                        state.implicit,
                        work,
                    ) else {
                        return false;
                    };
                    entry.insert(register);
                }
                let value = ListItemPlacement {
                    list_id: list,
                    placement: incoming,
                };
                let register = &self.item_registers[&write.logical];
                let previous = register.id;
                let Some((selected, winner)) =
                    register.selected(value, write.id, write.observed, frontier, work)
                else {
                    return false;
                };
                let destination_live = self.list_exists(document, selected.list_id);
                let destination_known = destination_live
                    || document
                        .blocks
                        .block_topology
                        .initial
                        .contains_key(&selected.list_id)
                    || self
                        .planned_placements
                        .as_ref()
                        .is_some_and(|p| p.initial.contains_key(&selected.list_id));
                if !destination_known
                    || (destination_live
                        && selected.placement.after.is_some_and(|id| {
                            !self.known(document, AdmissionTarget::Items(selected.list_id), id)
                        }))
                {
                    return false;
                }
                if !self
                    .item_registers
                    .get_mut(&write.logical)
                    .expect("staged item register")
                    .accept(value, write.id, write.observed, work)
                {
                    return false;
                }
                if winner != previous {
                    let canonical = self
                        .parent_aliases
                        .get(&previous)
                        .copied()
                        .unwrap_or(logical_origin(write.logical));
                    self.parent_aliases.insert(winner, canonical);
                }
                self.insert_span(
                    document,
                    AdmissionTarget::Items(selected.list_id),
                    winner,
                    1,
                );
                if let Some(object) = self.topology.item(document, write.logical) {
                    let parent = self
                        .topology
                        .node(document, object)
                        .and_then(|node| node.parent);
                    self.topology.relocate(document, object, winner, parent);
                }
                true
            }
            AdmissionTarget::Rows(table_id) | AdmissionTarget::Columns(table_id) => {
                if !self.table_exists(document, table_id) {
                    return true;
                }
                let key = (write.target, write.logical);
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    self.table_registers.entry(key)
                {
                    let Some(Block {
                        kind: BlockKind::Table { table },
                        ..
                    }) = document.find_block_by_id(table_id)
                    else {
                        return true;
                    };
                    let metadata = match write.target {
                        AdmissionTarget::Rows(_) => {
                            table.row_by_id(write.logical).and_then(|row| {
                                table.rows.get_element(&row.elem_id).map(|element| {
                                    (
                                        row.elem_id,
                                        &row.placement_observed,
                                        row.placement_conflicts.as_slice(),
                                        Placement {
                                            after: element.after,
                                            right_origin: element.right_origin,
                                        },
                                    )
                                })
                            })
                        }
                        _ => table.column_by_id(write.logical).and_then(|column| {
                            table.columns.get_element(&column.elem_id).map(|element| {
                                (
                                    column.elem_id,
                                    &column.placement_observed,
                                    column.placement_conflicts.as_slice(),
                                    Placement {
                                        after: element.after,
                                        right_origin: element.right_origin,
                                    },
                                )
                            })
                        }),
                    };
                    let Some((id, observed, conflicts, value)) = metadata else {
                        return true;
                    };
                    // Newly staged writes only introduce already-proven anchors.
                    // Without old conflicts, winner changes cannot expand pending state.
                    if conflicts.is_empty() {
                        return true;
                    }
                    let Some(register) = PlannedRegister::copy_from(
                        value,
                        id,
                        observed,
                        conflicts,
                        block_id_from_op(id) == write.logical,
                        work,
                    ) else {
                        return false;
                    };
                    entry.insert(register);
                }
                let Some((selected, winner)) = self.table_registers[&key].selected(
                    incoming,
                    write.id,
                    write.observed,
                    frontier,
                    work,
                ) else {
                    return false;
                };
                if selected
                    .after
                    .is_some_and(|id| !self.known(document, write.target, id))
                {
                    return false;
                }
                if !self
                    .table_registers
                    .get_mut(&key)
                    .expect("staged table register")
                    .accept(incoming, write.id, write.observed, work)
                {
                    return false;
                }
                self.insert_span(document, write.target, winner, 1);
                true
            }
            _ => true,
        }
    }

    pub(super) fn document_effect_ready(
        &mut self,
        document: &Document,
        envelope: &Envelope,
        frontier: &StateVector,
        work: &mut usize,
    ) -> bool {
        let write = match &envelope.body {
            OpBody::Doc(DocOp::MoveTableRow {
                table_id,
                row_id,
                id,
                after,
                right_origin,
                observed,
                ..
            }) => PlacementWrite {
                target: AdmissionTarget::Rows(*table_id),
                logical: *row_id,
                id: *id,
                after: *after,
                right_origin: *right_origin,
                observed,
            },
            OpBody::Doc(DocOp::MoveTableColumn {
                table_id,
                column_id,
                id,
                after,
                right_origin,
                observed,
                ..
            }) => PlacementWrite {
                target: AdmissionTarget::Columns(*table_id),
                logical: *column_id,
                id: *id,
                after: *after,
                right_origin: *right_origin,
                observed,
            },
            OpBody::Doc(DocOp::MoveListItem {
                list_id,
                item_id,
                id,
                after,
                right_origin,
                observed,
                ..
            }) => PlacementWrite {
                target: AdmissionTarget::Items(*list_id),
                logical: *item_id,
                id: *id,
                after: *after,
                right_origin: *right_origin,
                observed,
            },
            _ => return true,
        };
        self.placement_ready(document, write, frontier, work)
    }

    fn canonical_target(&self, document: &Document, target: AdmissionTarget) -> AdmissionTarget {
        match target {
            AdmissionTarget::Blocks(Some(parent)) => AdmissionTarget::Blocks(Some(
                self.parent_aliases
                    .get(&parent)
                    .copied()
                    .or_else(|| {
                        self.logical_block(document, parent)
                            .or_else(|| document.logical_item_for_elem(parent))
                            .map(logical_origin)
                    })
                    .unwrap_or(parent),
            )),
            _ => target,
        }
    }

    fn logical_block(&self, document: &Document, element: OpId) -> Option<BlockId> {
        self.block_ids
            .get(&element)
            .copied()
            .or_else(|| document.logical_block_for_elem(element))
    }

    fn container_exists(&self, document: &Document, parent: Option<OpId>) -> bool {
        parent.is_none_or(|parent| self.topology.container(document, parent))
    }

    fn list_exists(&self, document: &Document, id: BlockId) -> bool {
        self.topology.block(document, id).is_some_and(|object| {
            self.topology.active(document, object)
                && match object {
                    AdmissionObject::New(_) => self.created_lists.contains(&id),
                    _ => document.list_items(id).is_some(),
                }
        })
    }

    fn table_exists(&self, document: &Document, id: BlockId) -> bool {
        self.topology.block(document, id).is_some_and(|object| {
            self.topology.active(document, object)
                && match object {
                    AdmissionObject::New(_) => self.created_tables.contains(&id),
                    _ => document
                        .find_block_by_id(id)
                        .is_some_and(|block| matches!(block.kind, BlockKind::Table { .. })),
                }
        })
    }

    fn text_exists(&self, document: &Document, id: BlockId) -> bool {
        self.topology.block(document, id).is_some_and(|object| {
            self.topology.active(document, object)
                && match object {
                    AdmissionObject::New(_) => self.created_text.contains(&id),
                    _ => document
                        .find_block_by_id(id)
                        .and_then(|block| crate::doc::block_text_seq(&block.kind))
                        .is_some(),
                }
        })
    }

    fn item_move_source(&self, document: &Document, source: OpId, target: OpId) -> Option<bool> {
        self.topology.physical_object(document, source)?;
        let list = self.logical_block(document, source)?;
        let metadata = self
            .planned_placements
            .as_ref()
            .unwrap_or(&document.blocks.block_topology);
        let reference =
            metadata.reference_target(crate::doc::LogicalContainer::Block(list), source);
        if reference.is_some_and(|reference| !metadata.live_target(reference)) {
            return None;
        }
        if !self.list_exists(document, list) {
            return None;
        }
        Some(
            self.item_tombstones.contains(&(list, target))
                || (!self.created_lists.contains(&list)
                    && document
                        .list_items(list)
                        .and_then(|items| items.get_element(&target))
                        .is_some_and(|element| element.value.is_none())),
        )
    }

    pub(super) fn container_exists_for(&self, document: &Document, envelope: &Envelope) -> bool {
        let parent = match &envelope.body {
            OpBody::Doc(
                DocOp::InsertBlock { parent, .. }
                | DocOp::DeleteBlock { parent, .. }
                | DocOp::DeleteBlockById { parent, .. }
                | DocOp::MergeBlocks { parent, .. },
            ) => *parent,
            OpBody::Doc(DocOp::MoveBlocks { to_parent, .. }) => *to_parent,
            _ => return true,
        };
        self.container_exists(document, parent)
    }

    #[cfg(feature = "filesync")]
    pub(super) fn for_replay(baseline: StateVector) -> Self {
        Self {
            baseline: Some(baseline),
            ..Self::default()
        }
    }

    fn insert_span(&mut self, document: &Document, target: AdmissionTarget, id: OpId, span: u64) {
        let target = self.canonical_target(document, target);
        let spans = self.spans.entry((target, id.peer)).or_default();
        let mut start = id.counter.saturating_sub(span.saturating_sub(1));
        let mut end = id.counter;
        if let Some((&before, &last)) = spans.range(..=start).next_back()
            && last.saturating_add(1) >= start
        {
            start = before;
            end = end.max(last);
            spans.remove(&before);
        }
        while let Some((&next, &last)) = spans.range(start..).next() {
            if next > end.saturating_add(1) {
                break;
            }
            end = end.max(last);
            spans.remove(&next);
        }
        spans.insert(start, end);
    }

    fn contains(&self, document: &Document, target: AdmissionTarget, id: OpId) -> bool {
        let target = self.canonical_target(document, target);
        self.spans
            .get(&(target, id.peer))
            .and_then(|spans| spans.range(..=id.counter).next_back())
            .is_some_and(|(_, end)| *end >= id.counter)
    }

    fn known(&self, document: &Document, target: AdmissionTarget, id: OpId) -> bool {
        let target = self.canonical_target(document, target);
        self.contains(document, target, id)
            || (self
                .baseline
                .as_ref()
                .is_none_or(|baseline| baseline.get(id.peer).unwrap_or(0) >= id.counter)
                && match target {
                    AdmissionTarget::Text(block) if self.created_text.contains(&block) => false,
                    AdmissionTarget::Items(list) if self.created_lists.contains(&list) => false,
                    AdmissionTarget::Rows(table) | AdmissionTarget::Columns(table)
                        if self.created_tables.contains(&table) =>
                    {
                        false
                    }
                    AdmissionTarget::Blocks(parent) => document
                        .container_children(parent)
                        .is_some_and(|sequence| sequence.get_element(&id).is_some()),
                    AdmissionTarget::Text(block) => document
                        .find_block_by_id(block)
                        .and_then(|block| crate::doc::block_text_seq(&block.kind))
                        .is_some_and(|sequence| sequence.get_element(&id).is_some()),
                    AdmissionTarget::Items(list) => document
                        .list_items(list)
                        .is_some_and(|sequence| sequence.get_element(&id).is_some()),
                    AdmissionTarget::Rows(table) | AdmissionTarget::Columns(table) => document
                        .find_block_by_id(table)
                        .is_some_and(|block| match &block.kind {
                            BlockKind::Table { table } => match target {
                                AdmissionTarget::Rows(_) => table.rows.get_element(&id).is_some(),
                                _ => table.columns.get_element(&id).is_some(),
                            },
                            _ => false,
                        }),
                })
    }

    fn reset_target(&mut self, target: AdmissionTarget) {
        while let Some((&key, _)) = self.spans.range((target, 0)..=(target, u64::MAX)).next() {
            self.spans.remove(&key);
        }
    }

    fn new_block(
        &mut self,
        document: &Document,
        logical: BlockId,
        id: OpId,
        parent: Option<OpId>,
        container: bool,
        error_id: OpId,
    ) -> Result<(), SessionError> {
        let reborn = self.topology.block(document, logical).is_some();
        let owner = parent.and_then(|parent| self.topology.physical_object(document, parent));
        self.topology.create(
            document,
            logical,
            AdmissionNode {
                parent: owner,
                physical: id,
                deleted: false,
                container,
            },
            false,
            error_id,
        )?;
        self.parent_aliases.remove(&id);
        if reborn {
            for target in [
                AdmissionTarget::Text(logical),
                AdmissionTarget::Items(logical),
                AdmissionTarget::Rows(logical),
                AdmissionTarget::Columns(logical),
            ] {
                self.reset_target(target);
            }
            self.created_text.remove(&logical);
            self.created_lists.remove(&logical);
            self.created_tables.remove(&logical);
        }
        self.reset_target(AdmissionTarget::Blocks(Some(id)));
        self.insert_span(document, AdmissionTarget::Blocks(parent), id, 1);
        self.block_ids.insert(id, logical);
        let logical_parent = parent.and_then(|parent| self.placement_container(document, parent));
        if let Some(metadata) = &mut self.planned_placements {
            let parent_origin = logical_parent
                .and_then(|parent| metadata.current_target(parent))
                .map(|target| target.origin);
            metadata.record_creation(
                logical,
                crate::doc::BlockPlacement {
                    parent: logical_parent,
                    id,
                    after: None,
                    right_origin: None,
                    parent_origin,
                },
                reborn
                    && metadata
                        .initial
                        .get(&logical)
                        .is_some_and(|old| old.id != id),
            );
        }
        Ok(())
    }

    fn new_item(
        &mut self,
        document: &Document,
        list: BlockId,
        logical: BlockId,
        id: OpId,
        placement: crate::doc::Placement,
        error_id: OpId,
    ) -> Result<(), SessionError> {
        let parent = self.topology.block(document, list);
        self.topology.create(
            document,
            logical,
            AdmissionNode {
                parent,
                physical: id,
                deleted: false,
                container: true,
            },
            true,
            error_id,
        )?;
        self.parent_aliases.remove(&id);
        self.reset_target(AdmissionTarget::Blocks(Some(id)));
        self.insert_span(document, AdmissionTarget::Items(list), id, 1);
        self.item_registers.insert(
            logical,
            PlannedRegister {
                value: crate::doc::ListItemPlacement {
                    list_id: list,
                    placement,
                },
                id,
                observed: StateVector::new(),
                conflicts: Vec::new(),
                implicit: block_id_from_op(id) == logical,
            },
        );
        if let Some(metadata) = &mut self.planned_placements {
            let list_origin = metadata.initial[&list].id;
            metadata.record_item_creation(
                logical,
                crate::doc::ItemPlacement {
                    list_id: list,
                    id,
                    after: placement.after,
                    right_origin: placement.right_origin,
                    list_origin,
                },
                false,
            );
        }
        Ok(())
    }

    fn record_block(
        &mut self,
        document: &Document,
        block: &Block,
        parent: Option<OpId>,
        error_id: OpId,
    ) -> Result<(), SessionError> {
        self.new_block(
            document,
            block.id,
            block.elem_id,
            parent,
            matches!(block.kind, BlockKind::BlockQuote { .. }),
            error_id,
        )?;
        match &block.kind {
            BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
                self.created_text.insert(block.id);
                for element in text.iter_all() {
                    self.insert_span(document, AdmissionTarget::Text(block.id), element.id, 1);
                }
            }
            BlockKind::BlockQuote { children } => {
                for child in children.iter() {
                    self.record_block(document, child, Some(block.elem_id), error_id)?;
                }
            }
            BlockKind::List { items, .. } => {
                self.created_lists.insert(block.id);
                for element in items.iter_all() {
                    let Some(item) = &element.value else {
                        continue;
                    };
                    self.new_item(
                        document,
                        block.id,
                        item.id,
                        item.elem_id,
                        crate::doc::Placement {
                            after: element.after,
                            right_origin: element.right_origin,
                        },
                        error_id,
                    )?;
                    for child in item.children.iter() {
                        self.record_block(document, child, Some(item.elem_id), error_id)?;
                    }
                }
            }
            BlockKind::Table { .. } => {
                self.created_tables.insert(block.id);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn record(
        &mut self,
        document: &Document,
        envelope: &Envelope,
        staged: Option<&Block>,
    ) -> Result<(), SessionError> {
        let error_id = operation_extent(envelope).0;
        match &envelope.body {
            OpBody::Doc(DocOp::InsertBlock {
                parent,
                id,
                block,
                after,
                right_origin,
            }) => {
                let live_container = self.container_exists_for(document, envelope);
                if !live_container {
                    self.ensure_placement_projection(document);
                }
                if let Some(metadata) = &mut self.planned_placements {
                    let target = match parent {
                        Some(element) => {
                            let Some(container) = metadata.container_for_element(*element) else {
                                return Ok(());
                            };
                            let Some(target) = metadata.reference_target(container, *element)
                            else {
                                return Ok(());
                            };
                            if !live_container && metadata.live_target(target) {
                                return Ok(());
                            }
                            Some(target)
                        }
                        None => None,
                    };
                    let placement = crate::doc::BlockPlacement {
                        parent: target.map(|target| target.container),
                        parent_origin: target.map(|target| target.origin),
                        id: *id,
                        after: *after,
                        right_origin: *right_origin,
                    };
                    if let Some(staged) = staged {
                        metadata.record_initial_tree(staged, placement);
                    } else {
                        let replace = metadata.deleted.contains(&block.block_id)
                            && metadata
                                .initial
                                .get(&block.block_id)
                                .is_some_and(|old| old.id != *id);
                        metadata.record_creation(block.block_id, placement, replace);
                    }
                    metadata.recompute_deletions();
                    if metadata.deleted.contains(&block.block_id) {
                        return Ok(());
                    }
                }
                if !live_container || self.known(document, AdmissionTarget::Blocks(*parent), *id) {
                    return Ok(());
                }
                if let Some(staged) = staged {
                    self.record_block(document, staged, *parent, error_id)?;
                } else {
                    self.new_block(
                        document,
                        block.block_id,
                        *id,
                        *parent,
                        matches!(block.kind, BlockKindSkeleton::BlockQuote { .. }),
                        error_id,
                    )?;
                    match &block.kind {
                        BlockKindSkeleton::List { .. } => {
                            self.created_lists.insert(block.block_id);
                        }
                        BlockKindSkeleton::Table => {
                            self.created_tables.insert(block.block_id);
                        }
                        BlockKindSkeleton::Paragraph { text }
                        | BlockKindSkeleton::Heading { text, .. } => {
                            self.created_text.insert(block.block_id);
                            let count = grapheme_count(text) as u64;
                            if count != 0 {
                                self.insert_span(
                                    document,
                                    AdmissionTarget::Text(block.block_id),
                                    OpId {
                                        counter: id.counter.saturating_add(count),
                                        peer: id.peer,
                                    },
                                    count,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            OpBody::Doc(DocOp::InsertText {
                block_id, units, ..
            }) => {
                if !self.text_exists(document, *block_id) {
                    return Ok(());
                }
                for unit in units {
                    self.insert_span(document, AdmissionTarget::Text(*block_id), unit.id, 1);
                }
            }
            OpBody::Doc(DocOp::InsertListItem {
                list_elem,
                list_id,
                id,
                after,
                right_origin,
                ..
            }) => {
                let live_list = self.list_exists(document, *list_id);
                self.ensure_placement_projection(document);
                let metadata = self.planned_placements.as_mut().expect("placement plan");
                let Some(reference) = metadata
                    .reference_target(crate::doc::LogicalContainer::Block(*list_id), *list_elem)
                else {
                    return Ok(());
                };
                if !live_list && metadata.live_target(reference) {
                    return Ok(());
                }
                metadata.record_item_creation(
                    block_id_from_op(*id),
                    crate::doc::ItemPlacement {
                        list_id: *list_id,
                        list_origin: reference.origin,
                        id: *id,
                        after: *after,
                        right_origin: *right_origin,
                    },
                    false,
                );
                metadata.recompute_deletions();
                if metadata.deleted_items.contains(&block_id_from_op(*id))
                    || metadata.current_target(reference.container) != Some(reference)
                {
                    return Ok(());
                }
                if self.list_exists(document, *list_id)
                    && !self.known(document, AdmissionTarget::Items(*list_id), *id)
                {
                    self.new_item(
                        document,
                        *list_id,
                        block_id_from_op(*id),
                        *id,
                        crate::doc::Placement {
                            after: *after,
                            right_origin: *right_origin,
                        },
                        error_id,
                    )?;
                }
            }
            OpBody::Doc(
                DocOp::InsertTableRow { table_id, id, .. }
                | DocOp::MoveTableRow { table_id, id, .. },
            ) => {
                if self.table_exists(document, *table_id) {
                    self.insert_span(document, AdmissionTarget::Rows(*table_id), *id, 1);
                }
            }
            OpBody::Doc(
                DocOp::InsertTableColumn { table_id, id, .. }
                | DocOp::MoveTableColumn { table_id, id, .. },
            ) => {
                if self.table_exists(document, *table_id) {
                    self.insert_span(document, AdmissionTarget::Columns(*table_id), *id, 1);
                }
            }
            OpBody::Doc(DocOp::MoveBlocks {
                to_parent,
                blocks,
                id,
                observed,
            }) => {
                self.ensure_placement_projection(document);
                let parent =
                    to_parent.and_then(|parent| self.placement_container(document, parent));
                if to_parent.is_some() && parent.is_none() {
                    return Ok(());
                }
                let metadata = self.planned_placements.as_mut().expect("placement plan");
                if !metadata.moves.iter().any(|movement| movement.id == *id) {
                    let parent_origin = to_parent.and_then(|element| {
                        metadata
                            .reference_target(parent?, element)
                            .map(|target| target.origin)
                    });
                    metadata.moves.push(crate::doc::BlockMove {
                        id: *id,
                        parent,
                        blocks: blocks.clone(),
                        observed: observed.clone(),
                        parent_origin,
                    });
                    metadata.moves.sort_by_key(|movement| movement.id);
                }
                for movement in blocks {
                    self.insert_span(
                        document,
                        AdmissionTarget::Blocks(*to_parent),
                        movement.id,
                        1,
                    );
                    let Some(object) = self.topology.block(document, movement.block_id) else {
                        continue;
                    };
                    let Some(node) = self.topology.node(document, object) else {
                        continue;
                    };
                    let canonical = logical_origin(movement.block_id);
                    self.parent_aliases.insert(node.physical, canonical);
                    self.parent_aliases.insert(movement.id, canonical);
                    self.block_ids.insert(movement.id, movement.block_id);
                    self.topology.physical.insert(movement.id, object);
                }
            }
            OpBody::Doc(DocOp::SplitBlock {
                parent,
                id,
                new_block_id,
                units,
                ..
            }) => {
                if !self.container_exists(document, *parent) {
                    return Ok(());
                }
                self.new_block(document, *new_block_id, *id, *parent, false, error_id)?;
                self.created_text.insert(*new_block_id);
                for unit in units {
                    self.insert_span(document, AdmissionTarget::Text(*new_block_id), unit.id, 1);
                }
            }
            OpBody::Doc(DocOp::MergeBlocks {
                parent,
                left,
                right,
                units,
                ..
            }) => {
                if let Some(block) = self.logical_block(document, *left)
                    && self.topology.physical_object(document, *left).is_some()
                {
                    for unit in units {
                        self.insert_span(document, AdmissionTarget::Text(block), unit.id, 1);
                    }
                }
                if self.container_exists(document, *parent)
                    && let Some(object) = self.topology.physical_object(document, *right)
                {
                    self.topology.delete(document, object);
                }
            }
            OpBody::Doc(
                DocOp::DeleteBlock { .. }
                | DocOp::DeleteBlockById { .. }
                | DocOp::DeleteListItemById { .. },
            ) => {}
            OpBody::Doc(DocOp::MoveListItem {
                from_list_elem,
                to_list_elem,
                target,
                item_id,
                list_id,
                id,
                after,
                right_origin,
                observed,
            }) => {
                self.ensure_placement_projection(document);
                let metadata = self.planned_placements.as_mut().expect("placement plan");
                if !metadata
                    .item_moves
                    .iter()
                    .any(|movement| movement.id == *id)
                {
                    metadata.item_moves.push(crate::doc::PendingListItemMove {
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
                    metadata.item_moves.sort_by_key(|movement| movement.id);
                }
                if let Some(object) = self.topology.item(document, *item_id) {
                    self.topology.physical.insert(*id, object);
                }
                if self.list_exists(document, *list_id) {
                    if self
                        .item_registers
                        .get(item_id)
                        .is_none_or(|register| register.id != *id)
                    {
                        self.item_tombstones.insert((*list_id, *id));
                    }
                    self.insert_span(document, AdmissionTarget::Items(*list_id), *id, 1);
                }
            }
            _ => {}
        }
        if let OpBody::Doc(
            DocOp::DeleteBlock {
                id,
                observed,
                subtree,
                ..
            }
            | DocOp::DeleteBlockById {
                id,
                observed,
                subtree,
                ..
            }
            | DocOp::DeleteListItemById {
                id,
                observed,
                subtree,
                ..
            },
        ) = &envelope.body
        {
            self.ensure_placement_projection(document);
            let metadata = self.planned_placements.as_mut().expect("placement plan");
            let target = resolved_delete_target(metadata, envelope);
            if subtree.first().is_some_and(|root| Some(*root) != target) {
                return Err(SessionError::Validation(
                    crate::sync::ValidationError::MalformedOperation {
                        op_id: *id,
                        kind: crate::sync::MalformedKind::InvalidSequence,
                    },
                ));
            }
            if subtree
                .iter()
                .any(|target| !metadata.valid_target_reference(*target))
            {
                return Err(SessionError::Validation(
                    crate::sync::ValidationError::MalformedOperation {
                        op_id: *id,
                        kind: crate::sync::MalformedKind::InvalidSequence,
                    },
                ));
            }
            if let Some(target) = target
                && !metadata.deletions.iter().any(|deletion| deletion.id == *id)
            {
                metadata.deletions.push(crate::doc::SubtreeDeletion {
                    id: *id,
                    target,
                    observed: observed.clone(),
                    subtree: subtree.clone(),
                });
                metadata.deletions.sort_by_key(|deletion| deletion.id);
            }
        }
        if matches!(
            &envelope.body,
            OpBody::Doc(
                DocOp::MoveBlocks { .. }
                    | DocOp::MoveListItem { .. }
                    | DocOp::DeleteBlock { .. }
                    | DocOp::DeleteBlockById { .. }
                    | DocOp::DeleteListItemById { .. }
                    | DocOp::MergeBlocks { .. }
            )
        ) {
            self.refresh_placement_projection(document);
        }
        Ok(())
    }
}

/// Defer an unresolved leaf envelope intact rather than expanding it into an
/// unbounded number of document-level operations. Nested insertions are staged
/// separately and charged for their exact recursively pending effects.
pub(super) fn envelope_may_buffer_in_document(
    document: &Document,
    envelope: &Envelope,
    inserted: &AdmissionInsertions,
) -> bool {
    let known = |target, id| inserted.known(document, target, id);
    let anchor = |target, id: Option<OpId>| id.is_none_or(|id| known(target, id));
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock { parent, after, id, .. }) =>
            inserted.container_exists_for(document, envelope)
                && !known(AdmissionTarget::Blocks(*parent), *id)
                && !anchor(AdmissionTarget::Blocks(*parent), *after),
        OpBody::Doc(DocOp::DeleteBlock { parent, target, .. }) =>
            inserted.container_exists_for(document, envelope) && !known(AdmissionTarget::Blocks(*parent), *target),
        OpBody::Doc(DocOp::DeleteBlockById { parent, target, block_id, .. }) =>
            inserted.container_exists_for(document, envelope) && document.find_block_by_id(*block_id).is_none()
                && !known(AdmissionTarget::Blocks(*parent), *target),
        OpBody::Doc(DocOp::InsertText { block_id, units, .. }) => {
            if !inserted.text_exists(document, *block_id) {
                return inserted.baseline.is_some();
            }
            let (end, span) = operation_extent(envelope);
            let start = end.counter.saturating_sub(span.saturating_sub(1));
            units.iter().any(|unit| unit.after.is_some_and(|id| {
                !(id.peer == end.peer && id.counter >= start && id.counter < unit.id.counter)
                    && !known(AdmissionTarget::Text(*block_id), id)
            }))
        }
        OpBody::Doc(DocOp::DeleteText { block_id, targets, .. }) => {
            if !inserted.text_exists(document, *block_id) {
                return inserted.baseline.is_some();
            }
            targets.iter().any(|id| !known(AdmissionTarget::Text(*block_id), *id))
        }
        OpBody::Doc(DocOp::InsertListItem { list_id, after, .. }) =>
            inserted.list_exists(document, *list_id) && !anchor(AdmissionTarget::Items(*list_id), *after),
        OpBody::Doc(DocOp::InsertTableRow { table_id, after, .. }) =>
            inserted.table_exists(document, *table_id) && !anchor(AdmissionTarget::Rows(*table_id), *after),
        OpBody::Doc(DocOp::InsertTableColumn { table_id, after, .. }) =>
            inserted.table_exists(document, *table_id) && !anchor(AdmissionTarget::Columns(*table_id), *after),
        OpBody::Doc(DocOp::DeleteTableRow { table_id, target, .. }) =>
            inserted.table_exists(document, *table_id) && !known(AdmissionTarget::Rows(*table_id), *target),
        OpBody::Doc(DocOp::DeleteTableRowById { table_id, row_id, target, .. }) =>
            inserted.table_exists(document, *table_id)
                && !document.find_block_by_id(*table_id).is_some_and(|block| {
                    matches!(&block.kind, BlockKind::Table { table } if table.row_by_id(*row_id).is_some())
                }) && !known(AdmissionTarget::Rows(*table_id), *target),
        OpBody::Doc(DocOp::DeleteTableColumnById { table_id, column_id, target, .. }) =>
            inserted.table_exists(document, *table_id)
                && !document.find_block_by_id(*table_id).is_some_and(|block| {
                    matches!(&block.kind, BlockKind::Table { table } if table.column_by_id(*column_id).is_some())
                }) && !known(AdmissionTarget::Columns(*table_id), *target),
        OpBody::Doc(DocOp::DeleteListItemById { list_id, item_id, target, .. }) =>
            inserted.list_exists(document, *list_id) && document.find_list_item_by_id(*item_id).is_none()
                && !known(AdmissionTarget::Items(*list_id), *target),
        OpBody::Doc(DocOp::MoveBlocks { to_parent, blocks, .. }) => {
            if !inserted.container_exists_for(document, envelope) { return false; }
            let mut prior = std::collections::BTreeSet::new();
            blocks.iter().any(|block| {
                let missing = block.after.is_some_and(|after| {
                    !known(AdmissionTarget::Blocks(*to_parent), after) && !prior.contains(&after)
                });
                prior.insert(block.id);
                missing
            })
        }
        OpBody::Doc(DocOp::SplitBlock { parent, target, units, .. }) => {
            let block = inserted.logical_block(document, *target);
            !known(AdmissionTarget::Blocks(*parent), *target)
                || block.is_some_and(|block| units.iter().any(|unit| {
                    !known(AdmissionTarget::Text(block), unit.source_id)
                }))
        }
        OpBody::Doc(DocOp::MergeBlocks { parent, left, right, after, units, .. }) =>
            (inserted.container_exists_for(document, envelope)
                && !known(AdmissionTarget::Blocks(*parent), *right))
                || (!units.is_empty() && inserted.logical_block(document, *left).is_some_and(|block| {
                    !anchor(AdmissionTarget::Text(block), *after)
                }))
                || inserted.logical_block(document, *right).is_some_and(|block| {
                    units.iter().any(|unit| !known(AdmissionTarget::Text(block), unit.source_id))
                }),
        OpBody::Doc(DocOp::MoveTableRow { table_id, row_id, target, after, .. }) =>
            inserted.table_exists(document, *table_id)
                && (!anchor(AdmissionTarget::Rows(*table_id), *after)
                    || (!known(AdmissionTarget::Rows(*table_id), *target)
                        && !inserted.contains(document, AdmissionTarget::Rows(*table_id), logical_origin(*row_id))
                        && !document.find_block_by_id(*table_id).is_some_and(|block| {
                            matches!(&block.kind, BlockKind::Table { table } if table.row_by_id(*row_id).is_some())
                        }))),
        OpBody::Doc(DocOp::MoveTableColumn { table_id, column_id, target, after, .. }) =>
            inserted.table_exists(document, *table_id)
                && (!anchor(AdmissionTarget::Columns(*table_id), *after)
                    || (!known(AdmissionTarget::Columns(*table_id), *target)
                        && !inserted.contains(document, AdmissionTarget::Columns(*table_id), logical_origin(*column_id))
                        && !document.find_block_by_id(*table_id).is_some_and(|block| {
                            matches!(&block.kind, BlockKind::Table { table } if table.column_by_id(*column_id).is_some())
                        }))),
        OpBody::Doc(DocOp::MoveListItem { from_list_elem, list_id, item_id, target, after, .. }) => {
            if !inserted.list_exists(document, *list_id) { return false; }
            let active = inserted.topology.item(document, *item_id)
                .is_some_and(|object| inserted.topology.active(document, object));
            if !active {
                match inserted.item_move_source(document, *from_list_elem, *target) {
                    None => return false,
                    Some(false) => return true,
                    Some(true) => {}
                }
            }
            !anchor(AdmissionTarget::Items(*list_id), *after)
        }
        _ => false,
    }
}

/// Reserve unmaterialized declarations across senders. A captured or physical
/// deletion before a new creator's own counter separates exact incarnations;
/// provisional publication still rejects any duplicate that remains live.
pub(super) fn validate_identity_declarations<'a>(
    document: &Document,
    received: &StateVector,
    has_document_pending: bool,
    held: impl Iterator<Item = &'a Envelope> + Clone,
    incoming: impl Iterator<Item = (OpId, &'a Envelope)> + Clone,
) -> Result<(), SessionError> {
    use crate::doc::LogicalContainer;
    #[derive(Clone, Copy)]
    struct Declaration {
        owner: OpId,
        origin: OpId,
        start: u64,
    }
    struct Reservation {
        last_owner: OpId,
        ready: Option<Declaration>,
        waiting: Option<Declaration>,
    }
    impl Reservation {
        fn retained(owner: OpId, origin: OpId) -> Self {
            Self {
                last_owner: owner,
                ready: Some(Declaration {
                    owner,
                    origin,
                    start: owner.counter,
                }),
                waiting: None,
            }
        }
    }
    type Declarations = BTreeMap<BlockId, Reservation>;
    type Retirements = BTreeMap<(Option<LogicalContainer>, OpId, PeerId), u64>;
    struct ReservationContext {
        retirements: Retirements,
        covered: StateVector,
    }

    fn retired_before(
        retirements: &Retirements,
        container: LogicalContainer,
        old_origin: OpId,
        new_owner: OpId,
        new_start: u64,
    ) -> bool {
        let captured = retirements.get(&(Some(container), old_origin, new_owner.peer));
        let physical = matches!(container, LogicalContainer::Block(_))
            .then(|| retirements.get(&(None, old_origin, new_owner.peer)))
            .flatten();
        captured
            .into_iter()
            .chain(physical)
            .any(|counter| *counter < new_start)
    }

    fn record_retirements(document: &Document, envelope: &Envelope, retirements: &mut Retirements) {
        let (id, subtree) = match &envelope.body {
            OpBody::Doc(DocOp::DeleteBlock {
                id,
                target,
                subtree,
                ..
            }) => {
                // An unknown physical target is matched only to an actual
                // declaration's origin below, never to a UUID derived from it.
                retirements
                    .entry((None, *target, id.peer))
                    .and_modify(|counter| *counter = (*counter).min(id.counter))
                    .or_insert(id.counter);
                (*id, subtree)
            }
            OpBody::Doc(
                DocOp::DeleteBlockById { id, subtree, .. }
                | DocOp::DeleteListItemById { id, subtree, .. },
            ) => (*id, subtree),
            _ => return,
        };
        for target in resolved_delete_target(&document.blocks.block_topology, envelope)
            .into_iter()
            .chain(subtree.iter().copied())
        {
            retirements
                .entry((Some(target.container), target.origin, id.peer))
                .and_modify(|counter| *counter = (*counter).min(id.counter))
                .or_insert(id.counter);
        }
    }
    fn declares(envelope: &Envelope) -> bool {
        matches!(
            &envelope.body,
            OpBody::Doc(
                DocOp::InsertBlock { .. } | DocOp::InsertListItem { .. } | DocOp::SplitBlock { .. }
            )
        )
    }
    fn reserve(
        declarations: &mut Declarations,
        logical: BlockId,
        transaction: (OpId, u64),
        origin: OpId,
        item: bool,
        context: &ReservationContext,
    ) -> Result<(), SessionError> {
        let (owner, start) = transaction;
        let container = if item {
            LogicalContainer::Item(logical)
        } else {
            LogicalContainer::Block(logical)
        };
        let prefix_ready = context.covered.get(owner.peer).unwrap_or(0) >= start.saturating_sub(1);
        let current = Declaration {
            owner,
            origin,
            start,
        };
        let previous = match declarations.entry(logical) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Reservation {
                    last_owner: owner,
                    ready: prefix_ready.then_some(current),
                    waiting: (!prefix_ready).then_some(current),
                });
                return Ok(());
            }
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        };
        let comparable = if prefix_ready {
            previous.ready
        } else {
            previous.waiting
        };
        let conflict = comparable.is_some_and(|previous| {
            previous.owner.peer != owner.peer
                && !retired_before(
                    &context.retirements,
                    container,
                    previous.origin,
                    owner,
                    start,
                )
                && !retired_before(
                    &context.retirements,
                    container,
                    origin,
                    previous.owner,
                    previous.start,
                )
        });
        if previous.last_owner == owner || conflict {
            return Err(SessionError::Validation(
                ValidationError::MalformedOperation {
                    op_id: owner,
                    kind: crate::sync::MalformedKind::InvalidSequence,
                },
            ));
        }
        // A missing author prefix may contain the separating delete. It must
        // not displace a prefix-ready reservation and conceal another duplicate.
        previous.last_owner = owner;
        if prefix_ready {
            previous.ready = Some(current);
        } else {
            previous.waiting = Some(current);
        }
        Ok(())
    }
    fn block(
        value: &BlockSkeleton,
        physical: OpId,
        transaction: (OpId, u64),
        blocks: &mut Declarations,
        items: &mut Declarations,
        context: &ReservationContext,
        physical_ids: &mut std::collections::BTreeSet<OpId>,
    ) -> Result<(), SessionError> {
        if !physical_ids.insert(physical) {
            return Err(SessionError::Validation(
                ValidationError::MalformedOperation {
                    op_id: transaction.0,
                    kind: crate::sync::MalformedKind::InvalidSequence,
                },
            ));
        }
        reserve(
            blocks,
            value.block_id,
            transaction,
            physical,
            false,
            context,
        )?;
        match &value.kind {
            BlockKindSkeleton::BlockQuote { children } => {
                for child in children {
                    block(
                        &child.block,
                        child.id,
                        transaction,
                        blocks,
                        items,
                        context,
                        physical_ids,
                    )?;
                }
            }
            BlockKindSkeleton::List {
                items: children, ..
            } => {
                for item in children {
                    if !physical_ids.insert(item.id) {
                        return Err(SessionError::Validation(
                            ValidationError::MalformedOperation {
                                op_id: transaction.0,
                                kind: crate::sync::MalformedKind::InvalidSequence,
                            },
                        ));
                    }
                    reserve(items, item.block_id, transaction, item.id, true, context)?;
                    for child in &item.children {
                        block(
                            &child.block,
                            child.id,
                            transaction,
                            blocks,
                            items,
                            context,
                            physical_ids,
                        )?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn envelope(
        value: &Envelope,
        owner: OpId,
        blocks: &mut Declarations,
        items: &mut Declarations,
        context: &ReservationContext,
    ) -> Result<(), SessionError> {
        let (_, span) = operation_extent(value);
        let transaction = (owner, owner.counter.saturating_sub(span.saturating_sub(1)));
        match &value.body {
            OpBody::Doc(DocOp::InsertBlock {
                block: value, id, ..
            }) => block(
                value,
                *id,
                transaction,
                blocks,
                items,
                context,
                &mut std::collections::BTreeSet::new(),
            ),
            OpBody::Doc(DocOp::InsertListItem { id, .. }) => reserve(
                items,
                block_id_from_op(*id),
                transaction,
                *id,
                true,
                context,
            ),
            OpBody::Doc(DocOp::SplitBlock {
                id, new_block_id, ..
            }) => reserve(blocks, *new_block_id, transaction, *id, false, context),
            _ => Ok(()),
        }
    }
    fn pending_values<T: Clone>(sequence: &Sequence<T>) -> impl Iterator<Item = &T> {
        let deleted: std::collections::BTreeSet<_> = sequence
            .pending_ops_iter()
            .filter_map(|operation| match operation {
                SequenceOp::Delete { target, .. } => Some(*target),
                _ => None,
            })
            .collect();
        sequence
            .pending_ops_iter()
            .filter_map(move |operation| match operation {
                SequenceOp::Insert { id, value, .. } if !deleted.contains(id) => Some(value),
                _ => None,
            })
    }
    fn retained_block(
        value: &Block,
        owner: OpId,
        blocks: &mut Declarations,
        items: &mut Declarations,
    ) {
        blocks
            .entry(value.id)
            .or_insert_with(|| Reservation::retained(owner, value.elem_id));
        match &value.kind {
            BlockKind::BlockQuote { children } => {
                for child in children.iter().chain(pending_values(children)) {
                    retained_block(child, owner, blocks, items);
                }
            }
            BlockKind::List {
                items: children, ..
            } => {
                for item in children.iter().chain(pending_values(children)) {
                    items
                        .entry(item.id)
                        .or_insert_with(|| Reservation::retained(owner, item.elem_id));
                    for child in item.children.iter().chain(pending_values(&item.children)) {
                        retained_block(child, owner, blocks, items);
                    }
                }
            }
            _ => {}
        }
    }
    fn retained_declarations(
        sequence: &Sequence<Block>,
        blocks: &mut Declarations,
        items: &mut Declarations,
    ) {
        for value in pending_values(sequence) {
            retained_block(value, value.elem_id, blocks, items);
        }
        for value in sequence.iter() {
            match &value.kind {
                BlockKind::BlockQuote { children } => {
                    retained_declarations(children, blocks, items)
                }
                BlockKind::List {
                    items: children, ..
                } => {
                    for item in pending_values(children) {
                        items
                            .entry(item.id)
                            .or_insert_with(|| Reservation::retained(item.elem_id, item.elem_id));
                        for child in item.children.iter().chain(pending_values(&item.children)) {
                            retained_block(child, item.elem_id, blocks, items);
                        }
                    }
                    for item in children.iter() {
                        retained_declarations(&item.children, blocks, items);
                    }
                }
                _ => {}
            }
        }
    }
    let all_incoming = incoming.clone();
    let mut incoming = incoming
        .filter(|(_, envelope)| declares(envelope))
        .peekable();
    if incoming.peek().is_none() && !held.clone().any(declares) {
        return Ok(());
    }
    let mut context = ReservationContext {
        retirements: BTreeMap::new(),
        covered: received.clone(),
    };
    let mut spans = BTreeMap::new();
    for value in held.clone().chain(all_incoming.map(|(_, value)| value)) {
        record_retirements(document, value, &mut context.retirements);
        let (end, span) = operation_extent(value);
        let start = end.counter.saturating_sub(span.saturating_sub(1));
        spans
            .entry((end.peer, start))
            .and_modify(|counter: &mut u64| *counter = (*counter).max(end.counter))
            .or_insert(end.counter);
    }
    // This is receipt coverage for validation only, not a materialized frontier
    // and never document authority or an imported-origin vector exemption.
    for ((peer, start), end) in spans {
        let current = context.covered.get(peer).unwrap_or(0);
        if start <= current.saturating_add(1) {
            context.covered.set(peer, current.max(end));
        }
    }
    let mut blocks = BTreeMap::new();
    let mut items = BTreeMap::new();
    if has_document_pending {
        retained_declarations(document.blocks(), &mut blocks, &mut items);
    }
    for value in held.filter(|envelope| declares(envelope)) {
        envelope(
            value,
            operation_extent(value).0,
            &mut blocks,
            &mut items,
            &context,
        )?;
    }
    for (id, value) in incoming {
        envelope(value, id, &mut blocks, &mut items, &context)?;
    }
    Ok(())
}

fn spend_admission_work(work: &mut usize, cost: usize) -> bool {
    if cost > *work {
        return false;
    }
    *work -= cost;
    true
}

fn context_ready_after(
    id: OpId,
    observed: &StateVector,
    frontier: &StateVector,
    completing: OpId,
    work: &mut usize,
) -> bool {
    let available = |peer| {
        frontier
            .get(peer)
            .unwrap_or(0)
            .max(if peer == completing.peer {
                completing.counter
            } else {
                0
            })
    };
    spend_admission_work(work, 1)
        && available(id.peer) >= id.counter.saturating_sub(1)
        && observed
            .iter()
            .all(|(peer, counter)| spend_admission_work(work, 1) && available(peer) >= counter)
}

pub(super) fn envelope_has_nested_block(envelope: &Envelope) -> bool {
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock { block, .. }) => match &block.kind {
            BlockKindSkeleton::BlockQuote { children } => !children.is_empty(),
            BlockKindSkeleton::List { items, .. } => !items.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

pub(super) fn stage_nested_block(envelope: &Envelope) -> Option<Block> {
    if !envelope_has_nested_block(envelope) {
        return None;
    }
    let OpBody::Doc(DocOp::InsertBlock { id, block, .. }) = &envelope.body else {
        return None;
    };
    Some(block_from_skeleton(block, *id))
}

fn staged_contains_container(block: &Block, parent: OpId) -> bool {
    match &block.kind {
        BlockKind::BlockQuote { children } => {
            block.elem_id == parent
                || children
                    .iter()
                    .any(|child| staged_contains_container(child, parent))
        }
        BlockKind::List { items, .. } => items.iter().any(|item| {
            item.elem_id == parent
                || item
                    .children
                    .iter()
                    .any(|child| staged_contains_container(child, parent))
        }),
        _ => false,
    }
}

fn staged_container(block: &mut Block, parent: OpId) -> Option<&mut Sequence<Block>> {
    match &mut block.kind {
        BlockKind::BlockQuote { children } => {
            if block.elem_id == parent {
                return Some(children);
            }
            let child = children.iter_all().find_map(|element| {
                element
                    .value
                    .as_ref()
                    .filter(|child| staged_contains_container(child, parent))
                    .map(|_| element.id)
            })?;
            staged_container(children.value_mut(child)?, parent)
        }
        BlockKind::List { items, .. } => {
            let item = items.iter_all().find_map(|element| {
                element
                    .value
                    .as_ref()
                    .filter(|item| {
                        item.elem_id == parent
                            || item
                                .children
                                .iter()
                                .any(|child| staged_contains_container(child, parent))
                    })
                    .map(|_| element.id)
            })?;
            let item = items.value_mut(item)?;
            if item.elem_id == parent {
                return Some(&mut item.children);
            }
            let child = item.children.iter_all().find_map(|element| {
                element
                    .value
                    .as_ref()
                    .filter(|child| staged_contains_container(child, parent))
                    .map(|_| element.id)
            })?;
            staged_container(item.children.value_mut(child)?, parent)
        }
        _ => None,
    }
}

fn staged_contains_block(block: &Block, id: BlockId) -> bool {
    block.id == id
        || match &block.kind {
            BlockKind::BlockQuote { children } => children
                .iter()
                .any(|child| staged_contains_block(child, id)),
            BlockKind::List { items, .. } => items.iter().any(|item| {
                item.children
                    .iter()
                    .any(|child| staged_contains_block(child, id))
            }),
            _ => false,
        }
}

fn staged_block_mut(block: &mut Block, id: BlockId) -> Option<&mut Block> {
    if block.id == id {
        return Some(block);
    }
    match &mut block.kind {
        BlockKind::BlockQuote { children } => {
            let child = children.iter_all().find_map(|element| {
                element
                    .value
                    .as_ref()
                    .filter(|child| staged_contains_block(child, id))
                    .map(|_| element.id)
            })?;
            staged_block_mut(children.value_mut(child)?, id)
        }
        BlockKind::List { items, .. } => {
            let item = items.iter_all().find_map(|element| {
                element
                    .value
                    .as_ref()
                    .filter(|item| {
                        item.children
                            .iter()
                            .any(|child| staged_contains_block(child, id))
                    })
                    .map(|_| element.id)
            })?;
            let children = &mut items.value_mut(item)?.children;
            let child = children.iter_all().find_map(|element| {
                element
                    .value
                    .as_ref()
                    .filter(|child| staged_contains_block(child, id))
                    .map(|_| element.id)
            })?;
            staged_block_mut(children.value_mut(child)?, id)
        }
        _ => None,
    }
}

/// Fold a dependency directly into the affected, unpublished subtree. Publishing
/// only the resolved subtree avoids exposing an over-capacity intermediate tree.
pub(super) fn fold_staged_dependency(block: &mut Block, envelope: &Envelope) -> bool {
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock {
            parent: Some(parent),
            after,
            id,
            right_origin,
            block: skeleton,
        }) => {
            let Some(children) = staged_container(block, *parent) else {
                return false;
            };
            children.apply(SequenceOp::Insert {
                after: *after,
                id: *id,
                value: block_from_skeleton(skeleton, *id),
                right_origin: *right_origin,
            });
            true
        }
        OpBody::Doc(DocOp::InsertListItem {
            list_id,
            after,
            id,
            right_origin,
            task,
            ..
        }) => {
            let Some(Block {
                kind: BlockKind::List { items, .. },
                ..
            }) = staged_block_mut(block, *list_id)
            else {
                return false;
            };
            items.apply(SequenceOp::Insert {
                after: *after,
                id: *id,
                right_origin: *right_origin,
                value: ListItem {
                    id: block_id_from_op(*id),
                    elem_id: *id,
                    task: *task,
                    task_op: *id,
                    task_observed: StateVector::new(),
                    task_conflicts: Vec::new(),
                    placement_observed: StateVector::new(),
                    placement_conflicts: Vec::new(),
                    children: Sequence::new(),
                },
            });
            true
        }
        _ => false,
    }
}

/// Readiness is based on effects, not merely receipt of the creating operation.
/// A materialized frontier excludes every deferred counter span. Once a creator
/// is materialized, an absent target is a deleted target and its edit is a no-op.
pub(super) fn envelope_dependencies_ready(
    envelope: &Envelope,
    frontier: &StateVector,
    document: &Document,
) -> bool {
    let (own_end, own_span) = operation_extent(envelope);
    let own_start = own_end.counter.saturating_sub(own_span.saturating_sub(1));
    // Receipt is not a causal observation. A later operation from one origin
    // cannot materialize while an earlier counter span is still deferred.
    if frontier.get(own_end.peer).unwrap_or(0) < own_start.saturating_sub(1) {
        return false;
    }
    let ready = |id: OpId| {
        id.counter == 0
            || (id.peer == own_end.peer && id.counter >= own_start && id.counter <= own_end.counter)
            || frontier.get(id.peer).unwrap_or(0) >= id.counter
    };
    let optional = |id: Option<OpId>| id.is_none_or(ready);
    let logical = |id: BlockId| {
        let raw = id.as_u128();
        ready(OpId {
            peer: (raw >> 64) as u64,
            counter: raw as u64,
        })
    };
    let known_block = |id: OpId| ready(id) || document.logical_block_for_elem(id).is_some();
    let known_container = |id: Option<OpId>| {
        optional(id) || id.is_some_and(|id| document.logical_container(id).is_some())
    };
    let table_for = |id: BlockId| {
        document
            .find_block_by_id(id)
            .and_then(|block| match &block.kind {
                BlockKind::Table { table } => Some(table.as_ref()),
                _ => None,
            })
    };
    if let Some(observed) = envelope_observed_frontier(envelope)
        && !observed
            .iter()
            .all(|(peer, counter)| frontier.get(peer).unwrap_or(0) >= counter)
    {
        return false;
    }
    if let OpBody::Doc(
        DocOp::DeleteBlock { subtree, .. }
        | DocOp::DeleteBlockById { subtree, .. }
        | DocOp::DeleteListItemById { subtree, .. },
    ) = &envelope.body
        && !subtree.iter().all(|target| {
            ready(target.origin)
                || document
                    .blocks
                    .block_topology
                    .reference_target(target.container, target.origin)
                    == Some(*target)
        })
    {
        return false;
    }
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock {
            parent,
            after,
            right_origin,
            ..
        }) => {
            let children = document.container_children(*parent);
            known_container(*parent)
                && [*after, *right_origin].into_iter().all(|anchor| {
                    anchor.is_none_or(|id| {
                        known_block(id)
                            || children.is_some_and(|children| children.get_element(&id).is_some())
                    })
                })
        }
        OpBody::Doc(
            DocOp::DeleteBlock { parent, target, .. }
            | DocOp::DeleteBlockById { parent, target, .. },
        ) => {
            known_container(*parent)
                && (known_block(*target)
                    || document
                        .container_children(*parent)
                        .is_some_and(|children| children.get_element(target).is_some()))
        }
        OpBody::Doc(DocOp::InsertText {
            block_elem,
            block_id,
            units,
        }) => {
            let block = document.find_block_by_id(*block_id);
            let body = block.and_then(|block| crate::doc::block_text_seq(&block.kind));
            (block.is_some() || ready(*block_elem))
                && units.iter().all(|unit| {
                    [unit.after, unit.right_origin].into_iter().all(|anchor| {
                        anchor.is_none_or(|id| {
                            ready(id) || body.is_some_and(|body| body.get_element(&id).is_some())
                        })
                    })
                })
        }
        OpBody::Doc(DocOp::DeleteText {
            block_elem,
            block_id,
            targets,
            ..
        }) => {
            let block = document.find_block_by_id(*block_id);
            let body = block.and_then(|block| crate::doc::block_text_seq(&block.kind));
            (block.is_some() || ready(*block_elem))
                && targets
                    .iter()
                    .all(|id| ready(*id) || body.is_some_and(|body| body.get_element(id).is_some()))
        }
        OpBody::Doc(DocOp::SetMark {
            block_elem,
            block_id,
            start,
            end,
            ..
        }) => {
            let block = document.find_block_by_id(*block_id);
            let body = block.and_then(|block| crate::doc::block_text_seq(&block.kind));
            (block.is_some() || ready(*block_elem))
                && [start.elem_id, end.elem_id]
                    .into_iter()
                    .all(|id| ready(id) || body.is_some_and(|body| body.get_element(&id).is_some()))
        }
        OpBody::Doc(DocOp::RemoveMark {
            block_elem,
            block_id,
            interval_id,
            ..
        }) => {
            let block = document.find_block_by_id(*block_id);
            (block.is_some() || ready(*block_elem))
                && (ready(*interval_id)
                    || block.is_some_and(|block| block.marks.interval(interval_id).is_some()))
        }
        OpBody::Doc(DocOp::MoveBlocks {
            to_parent, blocks, ..
        }) => {
            let children = document.container_children(*to_parent);
            known_container(*to_parent)
                && blocks.iter().all(|block| {
                    known_block(block.target)
                        && [block.after, block.right_origin].into_iter().all(|anchor| {
                            anchor.is_none_or(|id| {
                                known_block(id)
                                    || children
                                        .is_some_and(|children| children.get_element(&id).is_some())
                            })
                        })
                })
        }
        OpBody::Doc(DocOp::SplitBlock {
            parent,
            target,
            units,
            ..
        }) => {
            let body = document
                .find_block(*target)
                .and_then(|block| crate::doc::block_text_seq(&block.kind));
            known_container(*parent)
                && known_block(*target)
                && units.iter().all(|unit| {
                    ready(unit.source_id)
                        || body.is_some_and(|body| body.get_element(&unit.source_id).is_some())
                })
        }
        OpBody::Doc(DocOp::MergeBlocks {
            parent,
            left,
            right,
            after,
            right_origin,
            units,
            ..
        }) => {
            let left_body = document
                .find_block(*left)
                .and_then(|block| crate::doc::block_text_seq(&block.kind));
            let right_body = document
                .find_block(*right)
                .and_then(|block| crate::doc::block_text_seq(&block.kind));
            known_container(*parent)
                && known_block(*left)
                && known_block(*right)
                && [*after, *right_origin].into_iter().all(|anchor| {
                    optional(anchor)
                        || anchor.is_some_and(|id| {
                            left_body.is_some_and(|body| body.get_element(&id).is_some())
                        })
                })
                && units.iter().all(|unit| {
                    ready(unit.source_id)
                        || right_body
                            .is_some_and(|body| body.get_element(&unit.source_id).is_some())
                })
        }
        OpBody::Doc(DocOp::InsertTableRow {
            table_elem,
            table_id,
            after,
            right_origin,
            cells,
            ..
        }) => {
            let table = table_for(*table_id);
            (table.is_some() || ready(*table_elem))
                && [*after, *right_origin].into_iter().all(|anchor| {
                    optional(anchor)
                        || anchor.is_some_and(|id| {
                            table.is_some_and(|table| table.rows.get_element(&id).is_some())
                        })
                })
                && cells.iter().all(|cell| {
                    logical(cell.column_id)
                        || table.is_some_and(|table| table.column_by_id(cell.column_id).is_some())
                })
        }
        OpBody::Doc(DocOp::InsertTableColumn {
            table_elem,
            table_id,
            after,
            right_origin,
            ..
        }) => {
            let table = table_for(*table_id);
            (table.is_some() || ready(*table_elem))
                && [*after, *right_origin].into_iter().all(|anchor| {
                    optional(anchor)
                        || anchor.is_some_and(|id| {
                            table.is_some_and(|table| table.columns.get_element(&id).is_some())
                        })
                })
        }
        OpBody::Doc(DocOp::SetTableCell {
            table_elem,
            table_id,
            row_id,
            column_id,
            ..
        }) => {
            let table = table_for(*table_id);
            (table.is_some() || ready(*table_elem))
                && (logical(*row_id)
                    || table.is_some_and(|table| {
                        *row_id == table.id || table.row_by_id(*row_id).is_some()
                    }))
                && (logical(*column_id)
                    || table.is_some_and(|table| table.column_by_id(*column_id).is_some()))
        }
        OpBody::Doc(
            DocOp::DeleteTableRow {
                table_elem,
                table_id,
                target,
                ..
            }
            | DocOp::DeleteTableRowById {
                table_elem,
                table_id,
                target,
                ..
            }
            | DocOp::DeleteTableColumnById {
                table_elem,
                table_id,
                target,
                ..
            },
        ) => {
            let table = table_for(*table_id);
            (table.is_some() || ready(*table_elem))
                && (ready(*target)
                    || table.is_some_and(|table| {
                        table.rows.get_element(target).is_some()
                            || table.columns.get_element(target).is_some()
                    }))
        }
        OpBody::Doc(DocOp::SetTableColumnAlignment {
            table_elem,
            table_id,
            column_id,
            ..
        }) => {
            let table = table_for(*table_id);
            (table.is_some() || ready(*table_elem))
                && (logical(*column_id)
                    || table.is_some_and(|table| table.column_by_id(*column_id).is_some()))
        }
        OpBody::Doc(
            DocOp::MoveTableRow {
                table_elem,
                table_id,
                target,
                after,
                right_origin,
                ..
            }
            | DocOp::MoveTableColumn {
                table_elem,
                table_id,
                target,
                after,
                right_origin,
                ..
            },
        ) => {
            let table = table_for(*table_id);
            let known = |id: OpId| {
                ready(id)
                    || table.is_some_and(|table| {
                        table.rows.get_element(&id).is_some()
                            || table.columns.get_element(&id).is_some()
                    })
            };
            (table.is_some() || ready(*table_elem))
                && known(*target)
                && after.is_none_or(known)
                && right_origin.is_none_or(known)
        }
        OpBody::Doc(DocOp::InsertListItem {
            list_elem,
            list_id,
            after,
            right_origin,
            ..
        }) => {
            let items = document.list_items(*list_id);
            (items.is_some() || ready(*list_elem))
                && [*after, *right_origin].into_iter().all(|anchor| {
                    optional(anchor)
                        || anchor.is_some_and(|id| {
                            items.is_some_and(|items| items.get_element(&id).is_some())
                        })
                })
        }
        OpBody::Doc(DocOp::DeleteListItemById {
            list_elem,
            list_id,
            target,
            ..
        }) => {
            let items = document.list_items(*list_id);
            (items.is_some() || ready(*list_elem))
                && (ready(*target)
                    || items.is_some_and(|items| items.get_element(target).is_some()))
        }
        OpBody::Doc(DocOp::MoveListItem {
            from_list_elem,
            to_list_elem,
            list_id,
            item_id,
            target,
            after,
            right_origin,
            ..
        }) => {
            let items = document.list_items(*list_id);
            known_block(*from_list_elem)
                && (items.is_some() || known_block(*to_list_elem))
                && (ready(*target) || document.find_list_item_by_id(*item_id).is_some())
                && [*after, *right_origin].into_iter().all(|anchor| {
                    optional(anchor)
                        || anchor.is_some_and(|id| {
                            items.is_some_and(|items| items.get_element(&id).is_some())
                        })
                })
        }
        OpBody::Doc(
            DocOp::SetListStyle { block_elem, .. }
            | DocOp::SetCodeFence { block_elem, .. }
            | DocOp::ConvertTextBlock { block_elem, .. }
            | DocOp::ReplaceRawBlock { block_elem, .. },
        ) => known_block(*block_elem),
        OpBody::Doc(DocOp::SetListItemTask { item_id, .. }) => {
            logical(*item_id) || document.find_list_item_by_id(*item_id).is_some()
        }
        OpBody::Doc(DocOp::SetFrontmatterField { .. } | DocOp::InitializeFrontmatter { .. }) => {
            true
        }
    }
}

/// Logical retained-byte charge: decoded JSON bytes plus per-node bookkeeping.
/// The streaming count avoids a second payload allocation and does not claim RSS.
pub(super) fn envelope_retained_bytes(envelope: &Envelope) -> usize {
    struct Charge(usize);
    impl std::io::Write for Charge {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut charge = Charge(
        std::mem::size_of::<Envelope>()
            .saturating_add(envelope_dependency_work(envelope).saturating_mul(256)),
    );
    if serde_json::to_writer(&mut charge, envelope).is_err() {
        return usize::MAX;
    }
    charge.0
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
            let Some(last) = units.last() else {
                return (
                    OpId {
                        counter: 0,
                        peer: 0,
                    },
                    1,
                );
            };
            // Validation requires a consecutive, increasing run. Its allocated
            // length, never a min/max gap, is the causal counter coverage.
            (last.id, units.len() as u64)
        }
        OpBody::Doc(DocOp::DeleteText { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::SetMark { id, .. } | DocOp::RemoveMark { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::SetFrontmatterField { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::InitializeFrontmatter { id, .. }) => (*id, 1),
        OpBody::Doc(DocOp::MoveBlocks { id, blocks, .. }) => (*id, blocks.len() as u64),
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

pub(super) fn check_peer_consistency(id: OpId, env: &Envelope) -> Result<(), SessionError> {
    let peer = id.peer;
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
        OpBody::Doc(
            DocOp::DeleteBlock {
                id,
                observed,
                subtree,
                ..
            }
            | DocOp::DeleteBlockById {
                id,
                observed,
                subtree,
                ..
            },
        ) => {
            if let Some(target) = resolved_delete_target(&document.blocks.block_topology, envelope)
            {
                document.delete_logical_subtree(*id, target, observed, subtree);
            }
        }
        OpBody::Doc(DocOp::InsertText {
            block_elem,
            block_id,
            units,
        }) => {
            let block_elem = document.block_elem_id(*block_id).unwrap_or(*block_elem);
            // block_elem may be nested inside a blockquote; search the whole tree.
            let _ = document.with_block_content_mut(block_elem, |block| {
                let Some(body) = crate::doc::block_text_seq_mut(&mut block.kind) else {
                    return;
                };
                body.apply_batch(units.iter().map(|u| SequenceOp::Insert {
                    after: u.after,
                    id: u.id,
                    value: TextUnit {
                        grapheme: u.grapheme.clone(),
                    },
                    right_origin: u.right_origin,
                }));
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
            let _ = document.with_block_content_mut(block_elem, |block| {
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
            let _ = document.with_block_content_mut(block_elem, |block| {
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
            let _ = document.with_block_content_mut(block_elem, |block| {
                block.marks.remove_mark(*interval_id, observed.clone(), *id);
            });
        }
        OpBody::Doc(DocOp::SetFrontmatterField { id, key, value }) => {
            let _ = document
                .frontmatter
                .get_or_insert_with(crate::doc::Frontmatter::without_base)
                .receive_field(key.clone(), value.clone(), *id);
        }
        OpBody::Doc(DocOp::InitializeFrontmatter { id, frontmatter }) => {
            document
                .frontmatter
                .get_or_insert_with(crate::doc::Frontmatter::without_base)
                .initialize(frontmatter, *id);
        }
        OpBody::Doc(DocOp::MoveBlocks {
            to_parent,
            id,
            blocks,
            observed,
        }) => {
            document.move_blocks_at(*to_parent, blocks, *id, observed);
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
            let marks = document.with_block_content_mut(*target, |block| {
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
                    kind_conflicts: Vec::new(),
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
            let anchor_mapping: BTreeMap<OpId, OpId> = units
                .iter()
                .filter(|unit| unit.source_id != unit.id)
                .map(|unit| (unit.source_id, unit.id))
                .collect();
            let right_marks = document.find_block(*right).map(|block| {
                let mut marks = block.marks.clone();
                marks.remap_anchors(&anchor_mapping);
                marks
            });
            let _ = document.with_block_content_mut(*left, |block| {
                // Split retains equal-version copies on the left, including
                // removed intervals. Retarget both sides before deduplication.
                block.marks.remap_anchors(&anchor_mapping);
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
            let _ = document.with_block_content_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind {
                    let row_id = block_id_from_op(*id);
                    let row = crate::doc::TableRow {
                        id: row_id,
                        elem_id: *id,
                        deleted: crate::core::LwwRegister::new(false, *id),
                        placement_observed: StateVector::new(),
                        placement_conflicts: Vec::new(),
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
            let _ = document.with_block_content_mut(table_elem, |block| {
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
                        alignment_conflicts: Vec::new(),
                        placement_observed: StateVector::new(),
                        placement_conflicts: Vec::new(),
                    };
                    table.columns.apply(SequenceOp::Insert {
                        after: *after,
                        id: *id,
                        value: column,
                        right_origin: *right_origin,
                    });
                    table.set_cell(table.header_row_id(), column_id, header.clone(), *id);
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
            let _ = document.with_block_content_mut(table_elem, |block| {
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
            let _ = document.with_block_content_mut(table_elem, |block| {
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
            let _ = document.with_block_content_mut(table_elem, |block| {
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
            let _ = document.with_block_content_mut(table_elem, |block| {
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
            let _ = document.with_block_content_mut(table_elem, |block| {
                if let BlockKind::Table { table } = &mut block.kind
                    && table.column_by_id(*column_id).is_some()
                {
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
            let _ = document.with_block_content_mut(table_elem, |block| {
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
            let _ = document.with_block_content_mut(table_elem, |block| {
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
            if !document.record_list_item_creation(*list_id, *list_elem, *id, *after, *right_origin)
            {
                return;
            }
            let list_elem = current_block_elem(document, *list_id, *list_elem);
            let _ = document.with_block_mut(list_elem, |block| {
                if let BlockKind::List { items, .. } = &mut block.kind {
                    items.apply(SequenceOp::Insert {
                        after: *after,
                        id: *id,
                        value: ListItem {
                            id: block_id_from_op(*id),
                            elem_id: *id,
                            task: *task,
                            task_op: *id,
                            task_observed: StateVector::new(),
                            task_conflicts: Vec::new(),
                            placement_observed: StateVector::new(),
                            placement_conflicts: Vec::new(),
                            children: Sequence::new(),
                        },
                        right_origin: *right_origin,
                    });
                }
            });
        }
        OpBody::Doc(DocOp::DeleteListItemById {
            id,
            observed,
            subtree,
            ..
        }) => {
            if let Some(target) = resolved_delete_target(&document.blocks.block_topology, envelope)
            {
                document.delete_logical_subtree(*id, target, observed, subtree);
            }
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
            document.apply_item_movement(&crate::doc::PendingListItemMove {
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
        kind_conflicts: Vec::new(),
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
                        kind_conflicts: Vec::new(),
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
                    task_conflicts: Vec::new(),
                    placement_observed: StateVector::new(),
                    placement_conflicts: Vec::new(),
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
                    kind_conflicts: Vec::new(),
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
        ColumnAlignment::Default => ColumnAlignmentWire::Default,
    }
}

pub(super) fn alignment_from_wire(alignment: ColumnAlignmentWire) -> ColumnAlignment {
    match alignment {
        ColumnAlignmentWire::Left => ColumnAlignment::Left,
        ColumnAlignmentWire::Center => ColumnAlignment::Center,
        ColumnAlignmentWire::Right => ColumnAlignment::Right,
        ColumnAlignmentWire::Default => ColumnAlignment::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(counter: u64) -> OpId {
        OpId { counter, peer: 1 }
    }

    #[test]
    fn span_helpers_reject_invalid_and_empty_payloads() {
        assert!(JsonOpCodec.decode(b"not-json").is_err());

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
        assert!(validate_operation_envelope(id(1), &envelope).is_err());
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
            task_conflicts: Vec::new(),
            placement_observed: StateVector::new(),
            placement_conflicts: Vec::new(),
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
