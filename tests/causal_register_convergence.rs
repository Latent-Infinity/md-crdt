use md_crdt::core::Sequence;
use md_crdt::doc::{
    BlockId, BlockKind, CodeFenceStyle, ColumnAlignment, ColumnDef, ListStyle, Table, TaskState,
};
use md_crdt::{
    ChangeMessage, CheckpointRequest, CollaborativeDocument, DocumentTombstonePolicy,
    EquivalenceMode, OpId, SessionSnapshot, StateVector, TextBlockKind, ValidationLimits,
    block_id_from_op,
};

#[derive(Clone, Copy, Debug)]
enum RegisterKind {
    CodeFence,
    RawBlock,
    TextKind,
    ListStyle,
    Task,
    Cell,
    Alignment,
    RowPlacement,
    ColumnPlacement,
    ListPlacement,
}

const KINDS: [RegisterKind; 10] = [
    RegisterKind::CodeFence,
    RegisterKind::RawBlock,
    RegisterKind::TextKind,
    RegisterKind::ListStyle,
    RegisterKind::Task,
    RegisterKind::Cell,
    RegisterKind::Alignment,
    RegisterKind::RowPlacement,
    RegisterKind::ColumnPlacement,
    RegisterKind::ListPlacement,
];

struct Fixture {
    kind: RegisterKind,
    root: BlockId,
    target: BlockId,
    anchors: Vec<OpId>,
}

fn apply(session: &mut CollaborativeDocument, message: ChangeMessage) {
    session
        .apply_remote(message, &ValidationLimits::default())
        .unwrap();
}

fn changes(session: &CollaborativeDocument, since: &StateVector) -> ChangeMessage {
    session.encode_changes_since(since).unwrap()
}

fn setup(kind: RegisterKind) -> (CollaborativeDocument, Fixture) {
    let mut base = CollaborativeDocument::new(9);
    let mut anchors = Vec::new();
    let mut target = None;
    let root = match kind {
        RegisterKind::CodeFence => base
            .insert_block(
                None,
                BlockKind::CodeFence {
                    style: CodeFenceStyle::default(),
                    info: None,
                    text: "base".into(),
                },
            )
            .unwrap(),
        RegisterKind::RawBlock => base
            .insert_block(
                None,
                BlockKind::RawBlock {
                    raw: "<div>base</div>".into(),
                },
            )
            .unwrap(),
        RegisterKind::TextKind => base.insert_paragraph(None, "base").unwrap(),
        RegisterKind::ListStyle | RegisterKind::Task | RegisterKind::ListPlacement => {
            let list = base
                .insert_block(
                    None,
                    BlockKind::List {
                        style: ListStyle::default(),
                        items: Sequence::new(),
                        pending_moves: Vec::new(),
                    },
                )
                .unwrap();
            let list_id = block_id_from_op(list);
            for _ in 0..3 {
                let anchor = base
                    .insert_list_item(list_id, anchors.last().copied(), None)
                    .unwrap();
                anchors.push(anchor);
            }
            target = Some(block_id_from_op(
                base.insert_list_item(list_id, anchors.last().copied(), None)
                    .unwrap(),
            ));
            list
        }
        RegisterKind::Cell
        | RegisterKind::Alignment
        | RegisterKind::RowPlacement
        | RegisterKind::ColumnPlacement => {
            let table = base
                .insert_table(
                    None,
                    vec![
                        ColumnDef {
                            alignment: ColumnAlignment::Default
                        };
                        4
                    ],
                    vec!["one".into(), "two".into(), "three".into(), "target".into()],
                )
                .unwrap();
            let table_id = block_id_from_op(table);
            if matches!(kind, RegisterKind::RowPlacement) {
                for _ in 0..3 {
                    let anchor = base
                        .insert_table_row(
                            table_id,
                            anchors.last().copied(),
                            vec!["anchor".into(); 4],
                        )
                        .unwrap();
                    anchors.push(anchor);
                }
                target = Some(block_id_from_op(
                    base.insert_table_row(
                        table_id,
                        anchors.last().copied(),
                        vec!["target".into(); 4],
                    )
                    .unwrap(),
                ));
            } else {
                let BlockKind::Table { table } =
                    &base.document().find_block_by_id(table_id).unwrap().kind
                else {
                    unreachable!()
                };
                anchors = table
                    .columns
                    .iter()
                    .take(3)
                    .map(|column| column.elem_id)
                    .collect();
                target = Some(table.columns.iter().last().unwrap().id);
            }
            table
        }
    };
    let root = block_id_from_op(root);
    (
        base,
        Fixture {
            kind,
            root,
            target: target.unwrap_or(root),
            anchors,
        },
    )
}

impl Fixture {
    fn write(&self, session: &mut CollaborativeDocument, value: usize) -> OpId {
        let label = ["A", "B", "C"][value];
        match self.kind {
            RegisterKind::CodeFence => session
                .set_code_fence(
                    self.root,
                    CodeFenceStyle::default(),
                    Some(label.into()),
                    label.into(),
                )
                .unwrap(),
            RegisterKind::RawBlock => session
                .replace_raw_block(self.root, format!("<div>{label}</div>"))
                .unwrap(),
            RegisterKind::TextKind => session
                .convert_text_block(
                    self.root,
                    TextBlockKind::Heading {
                        level: value as u8 + 1,
                    },
                )
                .unwrap(),
            RegisterKind::ListStyle => session
                .set_list_style(
                    self.root,
                    ListStyle {
                        ordered: true,
                        start: value as u32 + 1,
                        ..ListStyle::default()
                    },
                )
                .unwrap(),
            RegisterKind::Task => session
                .set_list_item_task(
                    self.target,
                    [None, Some(TaskState::Unchecked), Some(TaskState::Checked)][value],
                )
                .unwrap(),
            RegisterKind::Cell => session
                .set_table_cell(self.root, self.root, self.target, label.into())
                .unwrap(),
            RegisterKind::Alignment => session
                .set_table_column_alignment(
                    self.root,
                    self.target,
                    [
                        ColumnAlignment::Left,
                        ColumnAlignment::Center,
                        ColumnAlignment::Right,
                    ][value]
                        .clone(),
                )
                .unwrap(),
            RegisterKind::RowPlacement => session
                .move_table_row(self.root, self.target, Some(self.anchors[value]))
                .unwrap(),
            RegisterKind::ColumnPlacement => session
                .move_table_column(self.root, self.target, Some(self.anchors[value]))
                .unwrap(),
            RegisterKind::ListPlacement => session
                .move_list_item(self.target, self.root, Some(self.anchors[value]))
                .unwrap(),
        }
    }

    fn table<'a>(&self, session: &'a CollaborativeDocument) -> &'a Table {
        let BlockKind::Table { table } =
            &session.document().find_block_by_id(self.root).unwrap().kind
        else {
            unreachable!()
        };
        table
    }

    fn value(&self, session: &CollaborativeDocument) -> String {
        let block = session.document().find_block_by_id(self.root).unwrap();
        match self.kind {
            RegisterKind::CodeFence => match &block.kind {
                BlockKind::CodeFence { text, .. } => text.clone(),
                _ => unreachable!(),
            },
            RegisterKind::RawBlock => match &block.kind {
                BlockKind::RawBlock { raw } => raw.clone(),
                _ => unreachable!(),
            },
            RegisterKind::TextKind => match &block.kind {
                BlockKind::Heading { level, .. } => level.to_string(),
                _ => unreachable!(),
            },
            RegisterKind::ListStyle => match &block.kind {
                BlockKind::List { style, .. } => style.start.to_string(),
                _ => unreachable!(),
            },
            RegisterKind::Task => format!(
                "{:?}",
                session
                    .document()
                    .find_list_item_by_id(self.target)
                    .unwrap()
                    .task
            ),
            RegisterKind::Cell => self
                .table(session)
                .cell_value(self.root, self.target)
                .unwrap()
                .into(),
            RegisterKind::Alignment => format!(
                "{:?}",
                self.table(session)
                    .column_by_id(self.target)
                    .unwrap()
                    .alignment
                    .get_ref()
            ),
            RegisterKind::RowPlacement => {
                let table = self.table(session);
                assert_eq!(
                    table
                        .rows
                        .iter()
                        .filter(|row| row.id == self.target)
                        .count(),
                    1
                );
                table
                    .rows
                    .iter()
                    .map(|row| row.id.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            }
            RegisterKind::ColumnPlacement => {
                let table = self.table(session);
                assert_eq!(
                    table
                        .columns
                        .iter()
                        .filter(|column| column.id == self.target)
                        .count(),
                    1
                );
                table
                    .columns
                    .iter()
                    .map(|column| column.id.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            }
            RegisterKind::ListPlacement => {
                let BlockKind::List { items, .. } = &block.kind else {
                    unreachable!()
                };
                assert_eq!(
                    items.iter().filter(|item| item.id == self.target).count(),
                    1
                );
                items
                    .iter()
                    .map(|item| item.id.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            }
        }
    }
}

fn history(
    kind: RegisterKind,
) -> (
    Fixture,
    ChangeMessage,
    [ChangeMessage; 3],
    String,
    StateVector,
) {
    let (base, fixture) = setup(kind);
    let initial = changes(&base, &StateVector::new());
    let mut a = CollaborativeDocument::new(1);
    apply(&mut a, initial.clone());
    a.insert_paragraph(None, &"x".repeat(98)).unwrap();
    assert_eq!(
        fixture.write(&mut a, 0),
        OpId {
            counter: 100,
            peer: 1
        }
    );
    let am = changes(&a, &base.state_vector());
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, changes(&a, &StateVector::new()));
    assert_eq!(
        fixture.write(&mut b, 1),
        OpId {
            counter: 1,
            peer: 2
        }
    );
    let bm = changes(&b, &a.state_vector());
    let mut c = CollaborativeDocument::new(3);
    apply(&mut c, initial.clone());
    c.insert_paragraph(None, &"y".repeat(48)).unwrap();
    assert_eq!(
        fixture.write(&mut c, 2),
        OpId {
            counter: 50,
            peer: 3
        }
    );
    let expected = fixture.value(&c);
    assert_ne!(
        fixture.value(&a),
        expected,
        "{kind:?}: A must be a distinct visible write"
    );
    assert_ne!(
        fixture.value(&b),
        expected,
        "{kind:?}: B must be a distinct visible write"
    );
    let cm = changes(&c, &base.state_vector());
    apply(&mut b, cm.clone());
    (fixture, initial, [am, bm, cm], expected, b.state_vector())
}

#[test]
fn every_register_converges_under_all_delivery_permutations_without_foreign_clock_jumps() {
    for kind in KINDS {
        let (fixture, initial, messages, expected, vector) = history(kind);
        let mut rendered = None;
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [2, 0, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 1, 0],
        ] {
            let mut receiver = CollaborativeDocument::new(4);
            apply(&mut receiver, initial.clone());
            for index in order {
                apply(&mut receiver, messages[index].clone());
            }
            assert_eq!(
                fixture.value(&receiver),
                expected,
                "{kind:?} schedule {order:?}"
            );
            assert_eq!(
                receiver.state_vector(),
                vector,
                "{kind:?} schedule {order:?}"
            );
            assert_eq!(
                receiver.peek_next_id(),
                OpId {
                    counter: 1,
                    peer: 4
                }
            );
            let markdown = receiver.document().serialize(EquivalenceMode::Structural);
            if let Some(previous) = &rendered {
                assert_eq!(&markdown, previous, "{kind:?} schedule {order:?}");
            }
            rendered = Some(markdown);
        }
    }
}

#[test]
fn concurrent_losers_survive_snapshot_and_checkpoint_before_becoming_visible() {
    for kind in KINDS {
        for checkpoint in [false, true] {
            let (fixture, initial, messages, expected, vector) = history(kind);
            let mut receiver = CollaborativeDocument::new(4);
            apply(&mut receiver, initial);
            apply(&mut receiver, messages[0].clone());
            apply(&mut receiver, messages[2].clone());
            if checkpoint {
                receiver
                    .checkpoint_history(&CheckpointRequest {
                        max_retained_ops: 0,
                        active_peer_leases: Vec::new(),
                        tombstones: DocumentTombstonePolicy::KeepAll,
                    })
                    .unwrap();
            }
            let bytes = receiver.save_snapshot().unwrap().to_bytes().unwrap();
            let snapshot = SessionSnapshot::from_bytes(&bytes).unwrap();
            let mut restored = CollaborativeDocument::restore_from_snapshot(snapshot).unwrap();
            apply(&mut restored, messages[1].clone());
            assert_eq!(
                fixture.value(&restored),
                expected,
                "{kind:?}, checkpoint={checkpoint}"
            );
            assert_eq!(restored.state_vector(), vector);
            // Replaying a dominated candidate after restore cannot revive it.
            apply(&mut restored, messages[0].clone());
            assert_eq!(fixture.value(&restored), expected);
            fixture.write(&mut restored, 1);
            assert_eq!(
                restored.peek_next_id(),
                OpId {
                    counter: 2,
                    peer: 4
                }
            );
        }
    }
}

#[test]
fn promoted_list_placement_moves_the_live_item_and_its_edited_children_between_containers() {
    let mut base = CollaborativeDocument::new(9);
    let mut lists = Vec::new();
    for _ in 0..4 {
        let list = base
            .insert_block(
                None,
                BlockKind::List {
                    style: ListStyle::default(),
                    items: Sequence::new(),
                    pending_moves: Vec::new(),
                },
            )
            .unwrap();
        lists.push(block_id_from_op(list));
    }
    let item = base.insert_list_item(lists[0], None, None).unwrap();
    let item_id = block_id_from_op(item);
    let paragraph = block_id_from_op(
        base.insert_paragraph_in(Some(item), None, "retained")
            .unwrap(),
    );
    let initial = changes(&base, &StateVector::new());
    let mut a = CollaborativeDocument::new(1);
    apply(&mut a, initial.clone());
    a.insert_paragraph(None, &"x".repeat(98)).unwrap();
    assert_eq!(
        a.move_list_item(item_id, lists[1], None).unwrap(),
        OpId {
            counter: 100,
            peer: 1
        }
    );
    let am = changes(&a, &base.state_vector());
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, changes(&a, &StateVector::new()));
    assert_eq!(
        b.move_list_item(item_id, lists[2], None).unwrap(),
        OpId {
            counter: 1,
            peer: 2
        }
    );
    let bm = changes(&b, &a.state_vector());
    let mut c = CollaborativeDocument::new(3);
    apply(&mut c, initial.clone());
    c.insert_paragraph(None, &"y".repeat(48)).unwrap();
    assert_eq!(
        c.move_list_item(item_id, lists[3], None).unwrap(),
        OpId {
            counter: 50,
            peer: 3
        }
    );
    let cm = changes(&c, &base.state_vector());
    for order in [[0, 1, 2], [0, 2, 1], [2, 0, 1]] {
        let mut receiver = CollaborativeDocument::new(4);
        apply(&mut receiver, initial.clone());
        let messages = [am.clone(), bm.clone(), cm.clone()];
        for (step, index) in order.into_iter().enumerate() {
            apply(&mut receiver, messages[index].clone());
            if step == 1 && order == [0, 2, 1] {
                receiver.insert_text(paragraph, 0, "X").unwrap();
                // Both the retained winner's destination and the incoming
                // operation's destination may acquire fresh block placements.
                receiver.move_block(lists[3], None, None).unwrap();
                receiver.move_block(lists[2], None, None).unwrap();
                receiver
                    .checkpoint_history(&CheckpointRequest {
                        max_retained_ops: 0,
                        active_peer_leases: Vec::new(),
                        tombstones: DocumentTombstonePolicy::KeepAll,
                    })
                    .unwrap();
                let bytes = receiver.save_snapshot().unwrap().to_bytes().unwrap();
                receiver = CollaborativeDocument::restore_from_snapshot(
                    SessionSnapshot::from_bytes(&bytes).unwrap(),
                )
                .unwrap();
            }
        }
        assert_eq!(
            receiver.document().list_containing_item(item_id).unwrap().0,
            lists[3]
        );
        for (index, list) in lists.iter().enumerate() {
            let BlockKind::List { items, .. } =
                &receiver.document().find_block_by_id(*list).unwrap().kind
            else {
                unreachable!()
            };
            assert_eq!(
                items.iter().filter(|item| item.id == item_id).count(),
                usize::from(index == 3)
            );
        }
        let expected = if order == [0, 2, 1] {
            "Xretained"
        } else {
            "retained"
        };
        let block = receiver.document().find_block_by_id(paragraph).unwrap();
        let BlockKind::Paragraph { text } = &block.kind else {
            unreachable!()
        };
        assert_eq!(md_crdt::doc::paragraph_visible_string(text), expected);
    }
}

#[test]
fn local_writes_during_held_dependencies_observe_materialized_state_and_converge() {
    for kind in KINDS {
        for restore_while_held in [false, true] {
            let (base, fixture) = setup(kind);
            let initial = changes(&base, &StateVector::new());
            let mut a = CollaborativeDocument::new(1);
            apply(&mut a, initial.clone());
            a.insert_paragraph(None, &"x".repeat(98)).unwrap();
            assert_eq!(
                fixture.write(&mut a, 0),
                OpId {
                    counter: 100,
                    peer: 1
                }
            );
            let mut b = CollaborativeDocument::new(2);
            apply(&mut b, changes(&a, &StateVector::new()));
            assert_eq!(
                fixture.write(&mut b, 1),
                OpId {
                    counter: 1,
                    peer: 2
                }
            );
            let b_only = changes(&b, &a.state_vector());

            let mut editing = CollaborativeDocument::new(4);
            apply(&mut editing, initial.clone());
            let held = editing
                .apply_remote(b_only, &ValidationLimits::default())
                .unwrap();
            assert_eq!(
                held.buffered,
                vec![OpId {
                    counter: 1,
                    peer: 2
                }]
            );
            if restore_while_held {
                let bytes = editing.save_snapshot().unwrap().to_bytes().unwrap();
                editing = CollaborativeDocument::restore_from_snapshot(
                    SessionSnapshot::from_bytes(&bytes).unwrap(),
                )
                .unwrap();
            }
            assert_eq!(
                fixture.write(&mut editing, 2),
                OpId {
                    counter: 1,
                    peer: 4
                }
            );
            let expected = fixture.value(&editing);
            let edited = changes(&editing, &b.state_vector());

            let mut receiving = CollaborativeDocument::new(5);
            apply(&mut receiving, changes(&b, &StateVector::new()));
            apply(&mut receiving, edited);
            apply(&mut editing, changes(&a, &StateVector::new()));

            assert_eq!(
                fixture.value(&editing),
                expected,
                "{kind:?}: late dependency"
            );
            assert_eq!(
                fixture.value(&receiving),
                expected,
                "{kind:?}: causal delivery"
            );
            assert_eq!(editing.state_vector(), receiving.state_vector());
            assert_eq!(
                editing.document().serialize(EquivalenceMode::Structural),
                receiving.document().serialize(EquivalenceMode::Structural),
                "{kind:?}: held restore={restore_while_held}",
            );
        }
    }
}

fn import_pending_row_move(
    initial: ChangeMessage,
    movement: &ChangeMessage,
    root: BlockId,
) -> CollaborativeDocument {
    use md_crdt::codec::{DocOp, Envelope, OpBody};
    use md_crdt::doc::PendingTableMove;
    use md_crdt::session::snapshot::BlockKindDto;

    let operation = &movement.ops[0];
    let OpBody::Doc(DocOp::MoveTableRow {
        row_id,
        target,
        id,
        after,
        right_origin,
        observed,
        ..
    }) = serde_json::from_slice::<Envelope>(&operation.payload)
        .unwrap()
        .body
    else {
        unreachable!()
    };
    let mut editing = CollaborativeDocument::new(4);
    apply(&mut editing, initial);
    let mut snapshot = editing.save_snapshot().unwrap();
    snapshot
        .state_vector
        .set(operation.id.peer, operation.id.counter);
    snapshot
        .ops
        .push((operation.id, operation.payload.to_vec()));
    let block = snapshot
        .document
        .blocks
        .elements
        .iter_mut()
        .filter_map(|element| element.value.as_mut())
        .find(|block| block.id == root)
        .unwrap();
    let BlockKindDto::Table { table } = &mut block.kind else {
        unreachable!()
    };
    table.pending_row_moves.push(PendingTableMove {
        logical_id: row_id,
        target,
        id,
        after,
        right_origin,
        observed,
    });
    CollaborativeDocument::restore_from_snapshot(
        SessionSnapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap(),
    )
    .unwrap()
}

#[test]
fn local_moves_while_imported_causal_moves_wait_for_anchors_converge() {
    for restore_after_local_write in [false, true] {
        let (base, fixture) = setup(RegisterKind::RowPlacement);
        let initial = changes(&base, &StateVector::new());
        let mut a = CollaborativeDocument::new(1);
        apply(&mut a, initial.clone());
        a.insert_paragraph(None, &"x".repeat(98)).unwrap();
        assert_eq!(
            fixture.write(&mut a, 0),
            OpId {
                counter: 100,
                peer: 1
            }
        );
        let am = changes(&a, &base.state_vector());

        let mut anchor = CollaborativeDocument::new(88);
        apply(&mut anchor, initial.clone());
        let future = anchor
            .insert_table_row(
                fixture.root,
                Some(fixture.anchors[2]),
                vec!["future".into(); 4],
            )
            .unwrap();
        let anchor_message = changes(&anchor, &base.state_vector());

        let mut b = CollaborativeDocument::new(2);
        apply(&mut b, changes(&a, &StateVector::new()));
        apply(&mut b, anchor_message.clone());
        let before_b = b.state_vector();
        assert_eq!(
            b.move_table_row(fixture.root, fixture.target, Some(future))
                .unwrap(),
            OpId {
                counter: 1,
                peer: 2
            }
        );
        let bm = changes(&b, &before_b);

        // Import the supported document-pending representation, with B received
        // but its observed write A and placement anchor still unavailable.
        let mut editing = import_pending_row_move(initial.clone(), &bm, fixture.root);
        let before_d = editing.state_vector();
        fixture.write(&mut editing, 1);
        let dm = changes(&editing, &before_d);
        if restore_after_local_write {
            editing = CollaborativeDocument::restore_from_snapshot(
                SessionSnapshot::from_bytes(&editing.save_snapshot().unwrap().to_bytes().unwrap())
                    .unwrap(),
            )
            .unwrap();
        }

        let mut receiving = CollaborativeDocument::new(5);
        apply(&mut receiving, initial);
        apply(&mut receiving, am.clone());
        apply(&mut receiving, anchor_message.clone());
        apply(&mut receiving, bm);
        apply(&mut receiving, dm);
        apply(&mut editing, am);
        apply(&mut editing, anchor_message);

        assert_eq!(fixture.value(&editing), fixture.value(&receiving));
        assert_eq!(editing.state_vector(), receiving.state_vector());
        assert_eq!(
            editing.document().serialize(EquivalenceMode::Structural),
            receiving.document().serialize(EquivalenceMode::Structural)
        );
        let rows = fixture
            .table(&editing)
            .rows
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let anchor_position = rows
            .iter()
            .position(|row| *row == block_id_from_op(fixture.anchors[1]))
            .unwrap();
        assert_eq!(rows[anchor_position + 1], fixture.target);
    }
}

#[test]
fn imported_move_target_arrival_does_not_materialize_an_unobserved_causal_write() {
    for restore_after_local_write in [false, true] {
        let (base, mut fixture) = setup(RegisterKind::RowPlacement);
        let initial = changes(&base, &StateVector::new());
        let mut creator = CollaborativeDocument::new(66);
        apply(&mut creator, initial.clone());
        let target = creator
            .insert_table_row(
                fixture.root,
                Some(fixture.anchors[2]),
                vec!["late target".into(); 4],
            )
            .unwrap();
        fixture.target = block_id_from_op(target);
        let creation = changes(&creator, &base.state_vector());
        let mut a = CollaborativeDocument::new(1);
        apply(&mut a, changes(&creator, &StateVector::new()));
        a.insert_paragraph(None, &"x".repeat(98)).unwrap();
        assert_eq!(
            fixture.write(&mut a, 0),
            OpId {
                counter: 100,
                peer: 1
            }
        );
        let am = changes(&a, &creator.state_vector());
        let mut b = CollaborativeDocument::new(2);
        apply(&mut b, changes(&a, &StateVector::new()));
        let before_b = b.state_vector();
        fixture.write(&mut b, 2);
        let bm = changes(&b, &before_b);

        let mut editing = import_pending_row_move(initial.clone(), &bm, fixture.root);
        apply(&mut editing, creation.clone());
        let before_d = editing.state_vector();
        fixture.write(&mut editing, 1);
        let dm = changes(&editing, &before_d);
        if restore_after_local_write {
            editing = CollaborativeDocument::restore_from_snapshot(
                SessionSnapshot::from_bytes(&editing.save_snapshot().unwrap().to_bytes().unwrap())
                    .unwrap(),
            )
            .unwrap();
        }
        let mut receiving = CollaborativeDocument::new(5);
        apply(&mut receiving, initial);
        apply(&mut receiving, creation);
        apply(&mut receiving, am.clone());
        apply(&mut receiving, bm);
        apply(&mut receiving, dm);
        apply(&mut editing, am);

        assert_eq!(fixture.value(&editing), fixture.value(&receiving));
        assert_eq!(editing.state_vector(), receiving.state_vector());
        assert_eq!(
            editing.document().serialize(EquivalenceMode::Structural),
            receiving.document().serialize(EquivalenceMode::Structural)
        );
        let rows = fixture
            .table(&editing)
            .rows
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let anchor_position = rows
            .iter()
            .position(|row| *row == block_id_from_op(fixture.anchors[1]))
            .unwrap();
        assert_eq!(rows[anchor_position + 1], fixture.target);
    }
}

#[test]
fn deleting_imported_pending_container_does_not_expose_unmaterialized_receipts() {
    for remote_delete in [false, true] {
        for restore_after_delete in [false, true] {
            for restore_after_write in [false, true] {
                let (mut base, fixture) = setup(RegisterKind::RowPlacement);
                let survivor = block_id_from_op(
                    base.insert_block(
                        None,
                        BlockKind::RawBlock {
                            raw: "<div>base</div>".into(),
                        },
                    )
                    .unwrap(),
                );
                let initial = changes(&base, &StateVector::new());
                let mut a = CollaborativeDocument::new(1);
                apply(&mut a, initial.clone());
                a.insert_paragraph(None, &"x".repeat(98)).unwrap();
                assert_eq!(
                    a.replace_raw_block(survivor, "<div>A</div>".into())
                        .unwrap(),
                    OpId {
                        counter: 100,
                        peer: 1
                    }
                );
                let am = changes(&a, &base.state_vector());

                let mut anchor = CollaborativeDocument::new(88);
                apply(&mut anchor, initial.clone());
                let future = anchor
                    .insert_table_row(
                        fixture.root,
                        Some(fixture.anchors[2]),
                        vec!["future".into(); 4],
                    )
                    .unwrap();
                let anchor_message = changes(&anchor, &base.state_vector());
                let mut b = CollaborativeDocument::new(2);
                apply(&mut b, changes(&a, &StateVector::new()));
                apply(&mut b, anchor_message.clone());
                let before_b = b.state_vector();
                b.move_table_row(fixture.root, fixture.target, Some(future))
                    .unwrap();
                let bm = changes(&b, &before_b);
                let mut editing = import_pending_row_move(initial.clone(), &bm, fixture.root);
                let table_elem = base
                    .document()
                    .find_block_by_id(fixture.root)
                    .unwrap()
                    .elem_id;
                let deletion = if remote_delete {
                    let mut deleter = CollaborativeDocument::new(77);
                    apply(&mut deleter, initial.clone());
                    deleter.delete_block(table_elem).unwrap();
                    let message = changes(&deleter, &base.state_vector());
                    apply(&mut editing, message.clone());
                    message
                } else {
                    let before = editing.state_vector();
                    editing.delete_block(table_elem).unwrap();
                    changes(&editing, &before)
                };
                if restore_after_delete {
                    editing
                        .checkpoint_history(&CheckpointRequest {
                            max_retained_ops: 0,
                            active_peer_leases: Vec::new(),
                            tombstones: DocumentTombstonePolicy::KeepAll,
                        })
                        .unwrap();
                    editing = CollaborativeDocument::restore_from_snapshot(
                        SessionSnapshot::from_bytes(
                            &editing.save_snapshot().unwrap().to_bytes().unwrap(),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
                let before_d = editing.state_vector();
                let d = editing
                    .replace_raw_block(survivor, "<div>D</div>".into())
                    .unwrap();
                let dm = changes(&editing, &before_d);
                if restore_after_write {
                    editing
                        .checkpoint_history(&CheckpointRequest {
                            max_retained_ops: 0,
                            active_peer_leases: Vec::new(),
                            tombstones: DocumentTombstonePolicy::KeepAll,
                        })
                        .unwrap();
                    editing = CollaborativeDocument::restore_from_snapshot(
                        SessionSnapshot::from_bytes(
                            &editing.save_snapshot().unwrap().to_bytes().unwrap(),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }

                // Neither A nor B has materialized on this peer. The unrelated
                // survivor edit must not wait on B merely because its container
                // was deleted after receipt of the imported queue.
                let mut early = CollaborativeDocument::new(5);
                apply(&mut early, initial.clone());
                apply(&mut early, deletion.clone());
                let result = early
                    .apply_remote(dm.clone(), &ValidationLimits::default())
                    .unwrap();
                assert!(
                    result.applied.contains(&d),
                    "remote={remote_delete}, delete restore={restore_after_delete}, write restore={restore_after_write}: {result:?}"
                );
                assert!(
                    early
                        .document()
                        .serialize(EquivalenceMode::Structural)
                        .contains("<div>D</div>")
                );

                let mut causal = CollaborativeDocument::new(6);
                apply(&mut causal, initial);
                apply(&mut causal, am.clone());
                apply(&mut causal, anchor_message.clone());
                apply(&mut causal, bm.clone());
                apply(&mut causal, deletion);
                apply(&mut causal, dm);
                for session in [&mut editing, &mut early] {
                    apply(session, am.clone());
                    apply(session, anchor_message.clone());
                    apply(session, bm.clone());
                }
                for session in [&mut editing, &mut early] {
                    assert_eq!(session.state_vector(), causal.state_vector());
                    assert_eq!(
                        session.document().serialize(EquivalenceMode::Structural),
                        causal.document().serialize(EquivalenceMode::Structural)
                    );
                    assert!(session.document().find_block_by_id(fixture.root).is_none());
                    assert!(
                        session
                            .document()
                            .serialize(EquivalenceMode::Structural)
                            .contains("<div>A</div>")
                    );
                    // Once A and the anchor arrive, even a zero-capacity
                    // admission must see no unresolved debt from the deleted table.
                    let empty = changes(session, &session.state_vector());
                    session
                        .apply_remote(
                            empty,
                            &ValidationLimits {
                                max_pending_buffer: 0,
                                ..ValidationLimits::default()
                            },
                        )
                        .unwrap();
                    session
                        .checkpoint_history(&CheckpointRequest {
                            max_retained_ops: 0,
                            active_peer_leases: Vec::new(),
                            tombstones: DocumentTombstonePolicy::KeepAll,
                        })
                        .unwrap();
                    *session = CollaborativeDocument::restore_from_snapshot(
                        SessionSnapshot::from_bytes(
                            &session.save_snapshot().unwrap().to_bytes().unwrap(),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
            }
        }
    }
}
