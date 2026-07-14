use md_crdt::core::OpId;
use md_crdt::doc::{
    BlockKind, ColumnAlignment, ColumnDef, EquivalenceMode, Table, block_id_from_op,
};
use md_crdt::session::{CollaborativeDocument, SessionError};
use md_crdt::sync::ValidationLimits;

fn exchange(source: &CollaborativeDocument, target: &mut CollaborativeDocument) {
    let message = source.encode_changes_since(&target.state_vector()).unwrap();
    target
        .apply_remote(message, &ValidationLimits::default())
        .expect("apply remote table changes");
}

#[test]
fn table_rows_insert_update_delete_converge() {
    let mut first = CollaborativeDocument::new(1);
    let table_elem = first
        .insert_table(
            None,
            vec![
                ColumnDef {
                    alignment: ColumnAlignment::Left,
                },
                ColumnDef {
                    alignment: ColumnAlignment::Right,
                },
                ColumnDef {
                    alignment: ColumnAlignment::Center,
                },
            ],
            vec!["Name".into(), "Score".into(), "Rank".into()],
        )
        .expect("insert table");
    let table_id = md_crdt::doc::block_id_from_op(table_elem);
    let row = first
        .insert_table_row(
            table_id,
            None,
            vec!["Alice".into(), "10".into(), "1".into()],
        )
        .expect("insert row");

    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);
    second
        .set_table_row_cells(table_id, row, vec!["Alice".into(), "11".into(), "1".into()])
        .expect("update row");
    exchange(&second, &mut first);
    first.delete_table_row(table_id, row).expect("delete row");
    exchange(&first, &mut second);

    assert_eq!(first.state_vector(), second.state_vector());
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        second.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        "| Name | Score | Rank |\n| --- | ---: | :---: |"
    );
}

#[test]
fn concurrent_row_inserts_converge() {
    let mut first = CollaborativeDocument::new(1);
    let table_elem = first
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["value".into()],
        )
        .expect("insert table");
    let table_id = block_id_from_op(table_elem);
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first
        .insert_table_row(table_id, None, vec!["first".into()])
        .expect("first row");
    second
        .insert_table_row(table_id, None, vec!["second".into()])
        .expect("second row");
    exchange(&first, &mut second);
    exchange(&second, &mut first);

    assert_eq!(first.state_vector(), second.state_vector());
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        second.document().serialize(EquivalenceMode::Structural)
    );
    let output = first.document().serialize(EquivalenceMode::Structural);
    assert!(output.contains("| first |"));
    assert!(output.contains("| second |"));
}

#[test]
fn invalid_table_mutations_do_not_advance_clock() {
    let mut session = CollaborativeDocument::new(1);
    let paragraph = session.insert_paragraph(None, "text").expect("paragraph");
    let paragraph_id = block_id_from_op(paragraph);
    let before = session.peek_next_id();
    assert!(matches!(
        session.insert_table_row(paragraph_id, None, vec!["x".into()]),
        Err(SessionError::NotTable)
    ));
    assert!(matches!(
        session.set_table_row_cells(
            paragraph_id,
            OpId {
                counter: 1,
                peer: 99,
            },
            vec!["x".into()]
        ),
        Err(SessionError::NotTable)
    ));
    assert!(matches!(
        session.delete_table_row(paragraph_id, paragraph),
        Err(SessionError::NotTable)
    ));
    assert!(matches!(
        session.set_table_metadata(paragraph_id, Vec::new(), Vec::new()),
        Err(SessionError::NotTable)
    ));
    assert!(matches!(
        session.move_table_row(paragraph_id, paragraph_id, None),
        Err(SessionError::NotTable)
    ));
    assert_eq!(session.peek_next_id(), before);

    let table_elem = session
        .insert_table(
            Some(paragraph),
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["h".into()],
        )
        .expect("table");
    let table_id = block_id_from_op(table_elem);
    let before = session.peek_next_id();
    let missing = OpId {
        counter: 999,
        peer: 1,
    };
    assert!(matches!(
        session.insert_table_row(table_id, Some(missing), vec!["x".into()]),
        Err(SessionError::TableRowNotFound)
    ));
    assert!(matches!(
        session.set_table_row_cells(table_id, missing, vec!["x".into()]),
        Err(SessionError::TableRowNotFound)
    ));
    assert!(matches!(
        session.delete_table_row(table_id, missing),
        Err(SessionError::TableRowNotFound)
    ));
    assert_eq!(session.peek_next_id(), before);
    let row = session
        .insert_table_row(table_id, None, vec!["row".into()])
        .unwrap();
    let row_id = block_id_from_op(row);
    let before_move = session.peek_next_id();
    assert!(matches!(
        session.move_table_row(table_id, row_id, Some(row)),
        Err(SessionError::InvalidMove)
    ));
    assert!(matches!(
        session.move_table_row(table_id, row_id, Some(missing)),
        Err(SessionError::InvalidMove)
    ));
    assert_eq!(session.peek_next_id(), before_move);
}

#[test]
fn nonempty_table_block_requires_row_operations() {
    let mut session = CollaborativeDocument::new(1);
    let id = session.peek_next_id();
    let mut table = Table::new(
        block_id_from_op(id),
        id,
        vec![ColumnDef {
            alignment: ColumnAlignment::Left,
        }],
        vec!["h".into()],
        id,
    );
    table.insert_row(
        None,
        vec!["body".into()],
        OpId {
            counter: 2,
            peer: 1,
        },
    );

    assert!(matches!(
        session.insert_block(None, BlockKind::Table { table }),
        Err(SessionError::NonEmptyTableOnInsertBlock)
    ));
    assert_eq!(session.peek_next_id(), id);
}

#[test]
fn table_metadata_move_and_logical_delete_exchange() {
    let mut first = CollaborativeDocument::new(1);
    let table_elem = first
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["old".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table_elem);
    let row_a = first
        .insert_table_row(table_id, None, vec!["a".into()])
        .unwrap();
    let row_b = first
        .insert_table_row(table_id, Some(row_a), vec!["b".into()])
        .unwrap();
    let row_b_id = block_id_from_op(row_b);
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first
        .set_table_metadata(
            table_id,
            vec![ColumnDef {
                alignment: ColumnAlignment::Center,
            }],
            vec!["new".into()],
        )
        .unwrap();
    first.move_table_row(table_id, row_b_id, None).unwrap();
    first.delete_table_row(table_id, row_a).unwrap();
    exchange(&first, &mut second);
    assert_eq!(first.document(), second.document());
    assert_eq!(
        second.document().serialize(EquivalenceMode::Structural),
        "| new |\n| :---: |\n| b |"
    );
}

#[test]
fn concurrent_row_move_and_delete_converge_with_delete_wins() {
    let mut first = CollaborativeDocument::new(1);
    let table_elem = first
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["h".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table_elem);
    let target = first
        .insert_table_row(table_id, None, vec!["target".into()])
        .unwrap();
    let anchor = first
        .insert_table_row(table_id, Some(target), vec!["anchor".into()])
        .unwrap();
    let target_id = block_id_from_op(target);
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first
        .move_table_row(table_id, target_id, Some(anchor))
        .unwrap();
    second.delete_table_row(table_id, target).unwrap();
    exchange(&first, &mut second);
    exchange(&second, &mut first);
    assert_eq!(first.document(), second.document());
    assert!(
        !first
            .document()
            .serialize(EquivalenceMode::Structural)
            .contains("target")
    );
}
