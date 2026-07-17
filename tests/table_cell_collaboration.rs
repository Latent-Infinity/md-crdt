use md_crdt::doc::{ColumnAlignment, ColumnDef, EquivalenceMode, block_id_from_op};
use md_crdt::session::{CollaborativeDocument, SyncResponse};
use md_crdt::sync::{ChangeMessage, ValidationLimits};
use md_crdt::{CheckpointRequest, DocumentTombstonePolicy};

fn exchange(source: &CollaborativeDocument, target: &mut CollaborativeDocument) {
    let message = source.encode_changes_since(&target.state_vector()).unwrap();
    target
        .apply_remote(message, &ValidationLimits::default())
        .expect("apply remote table changes");
}

fn table_with_row(
    session: &mut CollaborativeDocument,
) -> (uuid::Uuid, uuid::Uuid, Vec<uuid::Uuid>) {
    let table_elem = session
        .insert_table(
            None,
            vec![
                ColumnDef {
                    alignment: ColumnAlignment::Left,
                },
                ColumnDef {
                    alignment: ColumnAlignment::Right,
                },
            ],
            vec!["name".into(), "score".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table_elem);
    let row_elem = session
        .insert_table_row(table_id, None, vec!["Ada".into(), "10".into()])
        .unwrap();
    let row_id = block_id_from_op(row_elem);
    let columns = session
        .document()
        .find_block_by_id(table_id)
        .and_then(|block| match &block.kind {
            md_crdt::doc::BlockKind::Table { table } => Some(
                table
                    .columns_in_order()
                    .into_iter()
                    .map(|column| column.id)
                    .collect(),
            ),
            _ => None,
        })
        .unwrap();
    (table_id, row_id, columns)
}

#[test]
fn concurrent_edits_to_different_cells_commute() {
    let mut first = CollaborativeDocument::new(1);
    let (table_id, row_id, columns) = table_with_row(&mut first);
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first
        .set_table_cell(table_id, row_id, columns[0], "Grace".into())
        .unwrap();
    second
        .set_table_cell(table_id, row_id, columns[1], "11".into())
        .unwrap();
    exchange(&first, &mut second);
    exchange(&second, &mut first);

    assert_eq!(first.document(), second.document());
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        "| name | score |\n| --- | ---: |\n| Grace | 11 |"
    );
}

#[test]
fn concurrent_edits_to_the_same_cell_have_one_deterministic_winner() {
    let mut first = CollaborativeDocument::new(1);
    let (table_id, row_id, columns) = table_with_row(&mut first);
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first
        .set_table_cell(table_id, row_id, columns[1], "11".into())
        .unwrap();
    second
        .set_table_cell(table_id, row_id, columns[1], "12".into())
        .unwrap();
    exchange(&first, &mut second);
    exchange(&second, &mut first);

    assert_eq!(first.document(), second.document());
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        "| name | score |\n| --- | ---: |\n| Ada | 11 |"
    );
}

#[test]
fn column_identity_survives_move_and_header_is_cell_addressable() {
    let mut session = CollaborativeDocument::new(1);
    let (table_id, _row_id, columns) = table_with_row(&mut session);
    let table = session.document().find_block_by_id(table_id).unwrap();
    let header_row_id = match &table.kind {
        md_crdt::doc::BlockKind::Table { table } => table.header_row_id(),
        _ => unreachable!(),
    };

    session
        .set_table_cell(table_id, header_row_id, columns[1], "points".into())
        .unwrap();
    session
        .move_table_column(table_id, columns[1], None)
        .unwrap();

    let table = session.document().find_block_by_id(table_id).unwrap();
    let table = match &table.kind {
        md_crdt::doc::BlockKind::Table { table } => table,
        _ => unreachable!(),
    };
    assert_eq!(table.columns_in_order()[0].id, columns[1]);
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        "| points | name |\n| ---: | --- |\n| 10 | Ada |"
    );
}

#[test]
fn deleting_a_column_wins_over_a_concurrent_cell_edit() {
    let mut first = CollaborativeDocument::new(1);
    let (table_id, row_id, columns) = table_with_row(&mut first);
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first.delete_table_column(table_id, columns[1]).unwrap();
    second
        .set_table_cell(table_id, row_id, columns[1], "99".into())
        .unwrap();
    exchange(&first, &mut second);
    exchange(&second, &mut first);

    assert_eq!(first.document(), second.document());
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        "| name |\n| --- |\n| Ada |"
    );
}

#[test]
fn a_cell_edit_delivered_before_its_row_is_retained() {
    let mut author = CollaborativeDocument::new(1);
    let table_elem = author
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["name".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table_elem);
    let mut editor = CollaborativeDocument::new(2);
    let mut delayed = CollaborativeDocument::new(3);
    exchange(&author, &mut editor);
    exchange(&author, &mut delayed);

    let row_elem = author
        .insert_table_row(table_id, None, vec!["Ada".into()])
        .unwrap();
    let row_id = block_id_from_op(row_elem);
    exchange(&author, &mut editor);
    let column_id = match &author.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table.columns_in_order()[0].id,
        _ => unreachable!(),
    };
    editor
        .set_table_cell(table_id, row_id, column_id, "Grace".into())
        .unwrap();

    let editor_delta = editor
        .encode_changes_since(&delayed.state_vector())
        .unwrap();
    let edit_first = ChangeMessage {
        since: editor_delta.since,
        ops: editor_delta
            .ops
            .into_iter()
            .filter(|operation| operation.id.peer == 2)
            .collect(),
    };
    delayed
        .apply_remote(edit_first, &ValidationLimits::default())
        .unwrap();
    exchange(&author, &mut delayed);

    assert_eq!(
        delayed.document().serialize(EquivalenceMode::Structural),
        "| name |\n| --- |\n| Grace |"
    );
}

#[test]
fn snapshot_round_trip_preserves_cell_and_column_identity() {
    let mut session = CollaborativeDocument::new(1);
    let (table_id, row_id, columns) = table_with_row(&mut session);
    session
        .set_table_cell(table_id, row_id, columns[1], "11".into())
        .unwrap();

    let restored =
        CollaborativeDocument::restore_from_snapshot(session.save_snapshot().unwrap()).unwrap();
    let table = match &restored.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table,
        _ => unreachable!(),
    };
    assert_eq!(
        table
            .columns_in_order()
            .into_iter()
            .map(|column| column.id)
            .collect::<Vec<_>>(),
        columns
    );
    assert_eq!(table.cell_value(row_id, columns[1]), Some("11"));
}

#[test]
fn deleting_a_row_wins_over_a_concurrent_cell_edit() {
    let mut first = CollaborativeDocument::new(1);
    let (table_id, row_id, columns) = table_with_row(&mut first);
    let row_elem = match &first.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table.row_by_id(row_id).unwrap().elem_id,
        _ => unreachable!(),
    };
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first.delete_table_row(table_id, row_elem).unwrap();
    second
        .set_table_cell(table_id, row_id, columns[0], "Grace".into())
        .unwrap();
    exchange(&first, &mut second);
    exchange(&second, &mut first);

    assert_eq!(first.document(), second.document());
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        "| name | score |\n| --- | ---: |"
    );
}

#[test]
fn checkpoint_rebase_preserves_cell_addresses() {
    let mut source = CollaborativeDocument::new(1);
    let (table_id, row_id, columns) = table_with_row(&mut source);
    source
        .set_table_cell(table_id, row_id, columns[1], "11".into())
        .unwrap();
    source
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();

    let SyncResponse::Rebase { checkpoint } = source
        .sync_since(&Default::default())
        .expect("late peer receives a checkpoint")
    else {
        panic!("expected checkpoint rebase");
    };
    let restored = CollaborativeDocument::restore_from_snapshot(*checkpoint).unwrap();
    let table = match &restored.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table,
        _ => unreachable!(),
    };
    assert_eq!(table.cell_value(row_id, columns[1]), Some("11"));
}

fn one_cell_wire_bytes(columns: usize) -> usize {
    let mut session = CollaborativeDocument::new(1);
    let table_elem = session
        .insert_table(
            None,
            (0..columns)
                .map(|_| ColumnDef {
                    alignment: ColumnAlignment::Left,
                })
                .collect(),
            (0..columns).map(|index| format!("h{index}")).collect(),
        )
        .unwrap();
    let table_id = block_id_from_op(table_elem);
    let row_elem = session
        .insert_table_row(table_id, None, vec!["value".into(); columns])
        .unwrap();
    let row_id = block_id_from_op(row_elem);
    let column_id = match &session.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table.columns_in_order()[columns / 2].id,
        _ => unreachable!(),
    };
    let before = session.state_vector();
    session
        .set_table_cell(table_id, row_id, column_id, "updated".into())
        .unwrap();
    session
        .encode_changes_since(&before)
        .unwrap()
        .ops
        .iter()
        .map(|operation| operation.payload.len())
        .sum()
}

#[test]
fn one_cell_wire_payload_does_not_scale_with_row_width() {
    let narrow = one_cell_wire_bytes(5);
    let wide = one_cell_wire_bytes(50);
    assert!(wide <= narrow + 32, "narrow={narrow}, wide={wide}");
}

#[test]
fn delayed_column_move_and_alignment_wait_for_the_column_insert() {
    let mut author = CollaborativeDocument::new(1);
    let table_elem = author
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["first".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table_elem);
    let mut editor = CollaborativeDocument::new(2);
    let mut delayed = CollaborativeDocument::new(3);
    exchange(&author, &mut editor);
    exchange(&author, &mut delayed);

    let first_column = match &author.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table.columns_in_order()[0].clone(),
        _ => unreachable!(),
    };
    let inserted = author
        .insert_table_column(
            table_id,
            Some(first_column.elem_id),
            ColumnAlignment::Left,
            "second".into(),
        )
        .unwrap();
    let second_column = block_id_from_op(inserted);
    exchange(&author, &mut editor);
    editor
        .move_table_column(table_id, second_column, None)
        .unwrap();
    editor
        .set_table_column_alignment(table_id, second_column, ColumnAlignment::Right)
        .unwrap();

    exchange(&editor, &mut delayed);
    assert_eq!(editor.document(), delayed.document());
    assert_eq!(
        delayed.document().serialize(EquivalenceMode::Structural),
        "| second | first |\n| ---: | --- |"
    );
}

#[test]
fn delayed_row_move_waits_for_the_row_insert() {
    let mut author = CollaborativeDocument::new(1);
    let (table_id, first_row_id, _) = table_with_row(&mut author);
    let first_row_elem = match &author.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table.row_by_id(first_row_id).unwrap().elem_id,
        _ => unreachable!(),
    };
    let mut editor = CollaborativeDocument::new(2);
    let mut delayed = CollaborativeDocument::new(3);
    exchange(&author, &mut editor);
    exchange(&author, &mut delayed);

    let second_row_elem = author
        .insert_table_row(
            table_id,
            Some(first_row_elem),
            vec!["Grace".into(), "11".into()],
        )
        .unwrap();
    let second_row_id = block_id_from_op(second_row_elem);
    exchange(&author, &mut editor);
    editor
        .move_table_row(table_id, second_row_id, None)
        .unwrap();

    exchange(&editor, &mut delayed);
    assert_eq!(editor.document(), delayed.document());
    assert_eq!(
        delayed.document().serialize(EquivalenceMode::Structural),
        "| name | score |\n| --- | ---: |\n| Grace | 11 |\n| Ada | 10 |"
    );
}

#[test]
fn snapshot_preserves_pending_column_operations() {
    let mut author = CollaborativeDocument::new(1);
    let table_elem = author
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["first".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table_elem);
    let mut editor = CollaborativeDocument::new(2);
    let mut delayed = CollaborativeDocument::new(3);
    exchange(&author, &mut editor);
    exchange(&author, &mut delayed);
    let first = match &author.document().find_block_by_id(table_id).unwrap().kind {
        md_crdt::doc::BlockKind::Table { table } => table.columns_in_order()[0].clone(),
        _ => unreachable!(),
    };
    let inserted = author
        .insert_table_column(
            table_id,
            Some(first.elem_id),
            ColumnAlignment::Left,
            "second".into(),
        )
        .unwrap();
    let second = block_id_from_op(inserted);
    exchange(&author, &mut editor);
    editor.move_table_column(table_id, second, None).unwrap();
    editor
        .set_table_column_alignment(table_id, second, ColumnAlignment::Right)
        .unwrap();

    let editor_delta = editor
        .encode_changes_since(&delayed.state_vector())
        .unwrap();
    delayed
        .apply_remote(
            ChangeMessage {
                since: editor_delta.since,
                ops: editor_delta
                    .ops
                    .into_iter()
                    .filter(|operation| operation.id.peer == 2)
                    .collect(),
            },
            &ValidationLimits::default(),
        )
        .unwrap();
    let mut restored =
        CollaborativeDocument::restore_from_snapshot(delayed.save_snapshot().unwrap()).unwrap();
    exchange(&author, &mut restored);

    assert_eq!(editor.document(), restored.document());
}
