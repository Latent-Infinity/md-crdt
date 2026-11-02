use md_crdt::codec::{DocOp, Envelope, OpBody, TextUnitWire, WIRE_VERSION};
use md_crdt::core::mark::MarkKind;
use md_crdt::core::{OpId, Sequence, StateVector};
use md_crdt::doc::{BlockKind, EquivalenceMode, block_id_from_op};
use md_crdt::session::{CollaborativeDocument, SessionError};
use md_crdt::sync::{ChangeMessage, Operation, SyncState, ValidationError, ValidationLimits};
use md_crdt::{BlockDraft, ListItemDraft, ListStyle, StructuredEditLimits};
use std::collections::BTreeMap;

fn history(session: &CollaborativeDocument) -> ChangeMessage {
    session.encode_changes_since(&StateVector::new()).unwrap()
}

fn apply(session: &mut CollaborativeDocument, message: ChangeMessage) {
    session
        .apply_remote(message, &ValidationLimits::default())
        .unwrap();
}

fn rendered(session: &CollaborativeDocument) -> String {
    session.document().serialize(EquivalenceMode::Structural)
}

fn recovered(session: CollaborativeDocument) -> CollaborativeDocument {
    let bytes = session.save_snapshot().unwrap().to_bytes().unwrap();
    let snapshot = md_crdt::SessionSnapshot::from_bytes(&bytes).unwrap();
    CollaborativeDocument::restore_from_snapshot(snapshot).unwrap()
}

#[test]
fn cross_peer_text_and_marks_wait_for_block_creation_across_recovery() {
    let mut a = CollaborativeDocument::new(1);
    let block = a.insert_paragraph(None, "A").unwrap();
    let block_id = block_id_from_op(block);
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, history(&a));
    b.insert_text(block_id, 1, "B").unwrap();
    b.set_mark(block_id, 0..2, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    let delta = b.encode_changes_since(&a.state_vector()).unwrap();

    for reverse in [false, true] {
        let mut c = CollaborativeDocument::new(3);
        let mut early = delta.clone();
        if reverse {
            early.ops.reverse();
        }
        let result = c.apply_remote(early, &ValidationLimits::default()).unwrap();
        assert!(result.applied.is_empty());
        assert_eq!(result.buffered.len(), 2);
        assert!(rendered(&c).is_empty());
        assert_eq!(c.save_snapshot().unwrap().deferred.len(), 2);
        c = recovered(c);
        apply(&mut c, history(&a));
        assert_eq!(rendered(&c), rendered(&b));
        assert!(rendered(&c).contains("**AB**"));
        assert_eq!(c.state_vector(), b.state_vector());
        assert!(c.save_snapshot().unwrap().deferred.is_empty());
    }
}

#[test]
fn edits_to_intentionally_deleted_blocks_do_not_remain_deferred() {
    let mut a = CollaborativeDocument::new(1);
    let block = a.insert_paragraph(None, "A").unwrap();
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, history(&a));
    b.insert_text(block_id_from_op(block), 1, "B").unwrap();
    let delta = b.encode_changes_since(&a.state_vector()).unwrap();
    a.delete_block(block).unwrap();
    let mut c = CollaborativeDocument::new(3);
    apply(&mut c, history(&a));
    let result = c.apply_remote(delta, &ValidationLimits::default()).unwrap();
    assert_eq!(result.applied.len(), 1);
    assert!(result.buffered.is_empty());
    assert!(rendered(&c).is_empty());
    assert!(c.save_snapshot().unwrap().deferred.is_empty());
}

#[test]
fn nested_parent_schedules_converge_through_snapshot_boundaries() {
    let mut a = CollaborativeDocument::new(1);
    let outer = a
        .insert_block(
            None,
            BlockKind::BlockQuote {
                children: Sequence::new(),
            },
        )
        .unwrap();
    let a_history = history(&a);
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, a_history.clone());
    let inner = b
        .insert_block_in(
            Some(outer),
            None,
            BlockKind::BlockQuote {
                children: Sequence::new(),
            },
        )
        .unwrap();
    let b_delta = b.encode_changes_since(&a.state_vector()).unwrap();
    let mut c = CollaborativeDocument::new(3);
    apply(&mut c, history(&b));
    c.insert_paragraph_in(Some(inner), None, "nested").unwrap();
    let c_delta = c.encode_changes_since(&b.state_vector()).unwrap();
    let messages = [a_history, b_delta, c_delta];
    for schedule in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut target = CollaborativeDocument::new(4);
        for index in schedule {
            apply(&mut target, messages[index].clone());
            target = recovered(target);
        }
        assert_eq!(rendered(&target), rendered(&c));
        assert!(rendered(&target).contains("nested"));
        assert_eq!(target.state_vector(), c.state_vector());
        let snapshot = target.save_snapshot().unwrap();
        assert!(snapshot.pending.is_empty());
        assert!(snapshot.deferred.is_empty());
    }
}

#[test]
fn descendant_list_move_is_typed_and_preserves_clock_and_state() {
    let mut session = CollaborativeDocument::new(7);
    let root = session
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![
                        BlockDraft::Paragraph {
                            text: "outer".into(),
                        },
                        BlockDraft::List {
                            style: ListStyle::default(),
                            items: vec![ListItemDraft {
                                task: None,
                                children: vec![BlockDraft::Paragraph {
                                    text: "inner".into(),
                                }],
                            }],
                        },
                    ],
                }],
            },
            StructuredEditLimits::default(),
        )
        .unwrap();
    let (item_id, nested_id) = {
        let list = session.document().find_block(root).unwrap();
        let BlockKind::List { items, .. } = &list.kind else {
            panic!("list")
        };
        let outer = items.iter().next().unwrap();
        let nested = outer
            .children
            .iter()
            .find(|block| matches!(block.kind, BlockKind::List { .. }))
            .unwrap();
        (outer.id, nested.id)
    };
    let before = session.save_snapshot().unwrap();
    let clock = session.peek_next_id();
    assert!(matches!(
        session.move_list_item(item_id, nested_id, None),
        Err(SessionError::MoveCycle)
    ));
    assert_eq!(session.peek_next_id(), clock);
    assert_eq!(session.save_snapshot().unwrap(), before);
    assert!(rendered(&session).contains("outer"));
    assert!(rendered(&session).contains("inner"));
}

#[test]
fn noncontiguous_max_counter_insert_is_rejected_before_any_effect() {
    let mut target = CollaborativeDocument::new(3);
    let mut valid = CollaborativeDocument::new(8);
    valid.insert_paragraph(None, "").unwrap();
    let block_elem = OpId {
        counter: 1,
        peer: 1,
    };
    let malformed = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertText {
            block_elem,
            block_id: block_id_from_op(block_elem),
            units: vec![
                TextUnitWire {
                    id: OpId {
                        counter: 1,
                        peer: 9,
                    },
                    after: None,
                    right_origin: None,
                    grapheme: "x".into(),
                },
                TextUnitWire {
                    id: OpId {
                        counter: u64::MAX,
                        peer: 9,
                    },
                    after: Some(OpId {
                        counter: 1,
                        peer: 9,
                    }),
                    right_origin: None,
                    grapheme: "y".into(),
                },
            ],
        }),
    };
    let mut message = history(&valid);
    message.ops.push(Operation {
        id: OpId {
            counter: u64::MAX,
            peer: 9,
        },
        payload: serde_json::to_vec(&malformed).unwrap().into(),
    });
    let before = target.save_snapshot().unwrap();
    assert!(matches!(
        target.apply_remote(message, &ValidationLimits::default()),
        Err(SessionError::Validation(
            ValidationError::MalformedOperation { .. }
        ))
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
}

#[test]
fn exhausted_frontiers_and_local_counter_reservations_are_safe() {
    let mut sync = SyncState::new();
    sync.apply_op(Operation {
        id: OpId {
            counter: u64::MAX,
            peer: 9,
        },
        payload: vec![1].into(),
    });
    let result = sync.apply_changes(ChangeMessage {
        since: StateVector::new(),
        ops: vec![Operation {
            id: OpId {
                counter: u64::MAX - 1,
                peer: 9,
            },
            payload: vec![2].into(),
        }],
    });
    assert!(result.applied.is_empty());
    assert_eq!(sync.state_vector().get(9), Some(u64::MAX));

    let mut original = CollaborativeDocument::new(9);
    let block = original.insert_paragraph(None, "A").unwrap();
    let mut snapshot = original.save_snapshot().unwrap();
    snapshot.next_counter = u64::MAX - 1;
    snapshot.state_vector.set(9, u64::MAX - 2);
    snapshot.delta_floor.set(9, u64::MAX - 2);
    let mut session = CollaborativeDocument::restore_from_snapshot(snapshot).unwrap();
    let before = session.save_snapshot().unwrap();
    assert!(matches!(
        session.insert_text(block_id_from_op(block), 1, "XY"),
        Err(SessionError::CounterExhausted)
    ));
    assert_eq!(session.save_snapshot().unwrap(), before);
    assert!(matches!(
        session.insert_paragraph(None, "B"),
        Err(SessionError::CounterExhausted)
    ));
    assert_eq!(session.save_snapshot().unwrap(), before);
    session
        .insert_text(block_id_from_op(block), 1, "X")
        .unwrap();
    assert_eq!(session.peek_next_id().counter, u64::MAX);
    let before = session.save_snapshot().unwrap();
    assert!(matches!(
        session.insert_text(block_id_from_op(block), 2, "Y"),
        Err(SessionError::CounterExhausted)
    ));
    assert_eq!(session.save_snapshot().unwrap(), before);
    session.rebind_peer(9);
    assert_eq!(session.peek_next_id().counter, u64::MAX);
}

#[test]
fn retained_byte_backpressure_is_atomic_and_deferred_state_recovers() {
    let mut a = CollaborativeDocument::new(1);
    let block = a.insert_paragraph(None, "A").unwrap();
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, history(&a));
    b.insert_text(block_id_from_op(block), 1, "B").unwrap();
    let delta = b.encode_changes_since(&a.state_vector()).unwrap();
    let mut target = CollaborativeDocument::new(3);
    let limits = ValidationLimits {
        max_retained_bytes: 1,
        ..ValidationLimits::default()
    };
    let before = target.save_snapshot().unwrap();
    assert!(matches!(
        target.apply_remote(delta.clone(), &limits),
        Err(SessionError::Validation(
            ValidationError::ResourceLimitExceeded { limit: 1, .. }
        ))
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
    apply(&mut target, delta);
    let before = target.save_snapshot().unwrap();
    assert_eq!(before.deferred.len(), 1);

    let mut unrelated = CollaborativeDocument::new(4);
    apply(&mut unrelated, history(&a));
    unrelated
        .insert_text(block_id_from_op(block), 1, "C")
        .unwrap();
    let unrelated_delta = unrelated.encode_changes_since(&a.state_vector()).unwrap();
    assert!(matches!(
        target.apply_remote(unrelated_delta, &limits),
        Err(SessionError::Validation(
            ValidationError::ResourceLimitExceeded { .. }
        ))
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
    target = recovered(target);
    apply(&mut target, history(&a));
    assert_eq!(rendered(&target), rendered(&b));
    assert!(target.save_snapshot().unwrap().deferred.is_empty());
}

#[test]
fn full_pending_queue_accepts_dependency_delivery_without_expiring_work() {
    let mut a = CollaborativeDocument::new(1);
    let block = a.insert_paragraph(None, "A").unwrap();
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, history(&a));
    b.insert_text(block_id_from_op(block), 1, "B").unwrap();
    let mut target = CollaborativeDocument::new(3);
    apply(
        &mut target,
        b.encode_changes_since(&a.state_vector()).unwrap(),
    );
    let limits = ValidationLimits {
        max_pending_buffer: 1,
        ..ValidationLimits::default()
    };
    for op in history(&a).ops {
        target
            .apply_remote(
                ChangeMessage {
                    since: StateVector::new(),
                    ops: vec![op],
                },
                &limits,
            )
            .unwrap();
    }
    assert_eq!(rendered(&target), rendered(&b));
    assert!(target.save_snapshot().unwrap().deferred.is_empty());
}

#[test]
fn exhausted_call_work_is_preserved_for_a_later_apply_after_restore() {
    let mut source = CollaborativeDocument::new(1);
    source.insert_paragraph(None, "text").unwrap();
    let mut target = CollaborativeDocument::new(2);
    let limits = ValidationLimits {
        max_dependency_work: 0,
        ..ValidationLimits::default()
    };
    let result = target.apply_remote(history(&source), &limits).unwrap();
    assert!(result.applied.is_empty());
    assert!(rendered(&target).is_empty());
    target = recovered(target);
    apply(
        &mut target,
        ChangeMessage {
            since: StateVector::new(),
            ops: Vec::new(),
        },
    );
    assert_eq!(rendered(&target), rendered(&source));
    assert!(target.save_snapshot().unwrap().deferred.is_empty());
}

#[test]
fn retained_byte_limit_is_aggregate_and_clones_are_isolated() {
    let mut a = CollaborativeDocument::new(1);
    let block = a.insert_paragraph(None, "A").unwrap();
    let mut b = CollaborativeDocument::new(2);
    apply(&mut b, history(&a));
    b.insert_text(block_id_from_op(block), 1, "B").unwrap();
    let mut d = CollaborativeDocument::new(4);
    apply(&mut d, history(&a));
    d.insert_text(block_id_from_op(block), 1, "D").unwrap();
    let b_delta = b.encode_changes_since(&a.state_vector()).unwrap();
    let d_delta = d.encode_changes_since(&a.state_vector()).unwrap();
    let limits = ValidationLimits {
        max_retained_bytes: 4 * 1024,
        ..ValidationLimits::default()
    };
    // Either tiny deferred edit fits on its own, but their retained aggregate does not.
    let mut one = CollaborativeDocument::new(3);
    one.apply_remote(b_delta, &limits).unwrap();
    let mut other = CollaborativeDocument::new(3);
    other.apply_remote(d_delta.clone(), &limits).unwrap();
    let before = one.save_snapshot().unwrap();
    assert!(matches!(
        one.apply_remote(d_delta, &limits),
        Err(SessionError::Validation(
            ValidationError::ResourceLimitExceeded { .. }
        ))
    ));
    assert_eq!(one.save_snapshot().unwrap(), before);

    let mut isolated = one.clone();
    apply(&mut isolated, history(&a));
    assert_eq!(rendered(&isolated), rendered(&b));
    assert!(rendered(&one).is_empty());
    assert_eq!(one.save_snapshot().unwrap(), before);
    apply(&mut one, history(&a));
    assert_eq!(rendered(&one), rendered(&isolated));
}

fn operation(id: OpId, body: DocOp) -> Operation {
    Operation {
        id,
        payload: serde_json::to_vec(&Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(body),
        })
        .unwrap()
        .into(),
    }
}

fn message(ops: Vec<Operation>) -> ChangeMessage {
    ChangeMessage {
        since: StateVector::new(),
        ops,
    }
}

fn pending_quote(children: usize, recursive: bool) -> (Operation, Operation, OpId) {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert};
    let root = OpId {
        peer: 7,
        counter: 1,
    };
    let missing = OpId {
        peer: 99,
        counter: 1,
    };
    let parent = if recursive {
        OpId {
            peer: 7,
            counter: 2,
        }
    } else {
        root
    };
    let first = if recursive { 3 } else { 2 };
    let children: Vec<_> = (first..first + children as u64)
        .map(|counter| {
            let id = OpId { peer: 7, counter };
            BlockSkeletonInsert {
                id,
                after: Some(missing),
                right_origin: None,
                block: BlockSkeleton {
                    block_id: block_id_from_op(id),
                    kind: BlockKindSkeleton::Paragraph {
                        text: String::new(),
                    },
                },
            }
        })
        .collect();
    let end = children.last().unwrap().id;
    let children = if recursive {
        vec![BlockSkeletonInsert {
            id: parent,
            after: None,
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id_from_op(parent),
                kind: BlockKindSkeleton::BlockQuote { children },
            },
        }]
    } else {
        children
    };
    let root = operation(
        end,
        DocOp::InsertBlock {
            parent: None,
            after: None,
            id: root,
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id_from_op(root),
                kind: BlockKindSkeleton::BlockQuote { children },
            },
        },
    );
    let anchor = operation(
        missing,
        DocOp::InsertBlock {
            parent: Some(parent),
            after: None,
            id: missing,
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id_from_op(missing),
                kind: BlockKindSkeleton::Paragraph {
                    text: String::new(),
                },
            },
        },
    );
    (root, anchor, parent)
}

#[test]
fn in_order_block_text_and_mark_batch_needs_no_retained_capacity() {
    let mut source = CollaborativeDocument::new(1);
    let block = source.insert_paragraph(None, "").unwrap();
    let block_id = block_id_from_op(block);
    source.insert_text(block_id, 0, "abcdef").unwrap();
    source
        .set_mark(block_id, 0..6, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    let limits = ValidationLimits {
        max_pending_buffer: 0,
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    let mut target = CollaborativeDocument::new(2);
    let result = target.apply_remote(history(&source), &limits).unwrap();
    assert_eq!(result.applied.len(), 3);
    assert!(result.buffered.is_empty());
    assert_eq!(rendered(&target), "**abcdef**");
    assert_eq!(target.state_vector(), source.state_vector());
    target = recovered(target);
    target.apply_remote(message(Vec::new()), &limits).unwrap();
    assert_eq!(rendered(&target), "**abcdef**");
}

#[test]
fn nested_missing_anchor_progresses_at_one_slot_in_both_delivery_orders() {
    let limits = ValidationLimits {
        max_pending_buffer: 1,
        ..ValidationLimits::default()
    };
    for recursive in [false, true] {
        for anchor_first in [false, true] {
            let (root, anchor, parent) = pending_quote(100, recursive);
            let (first, second) = if anchor_first {
                (anchor, root)
            } else {
                (root, anchor)
            };
            let mut target = CollaborativeDocument::new(10);
            let first_result = target.apply_remote(message(vec![first]), &limits).unwrap();
            assert_eq!(first_result.buffered.len(), 1);
            assert!(target.document().blocks_in_order().is_empty());
            target = recovered(target);
            let result = target.apply_remote(message(vec![second]), &limits).unwrap();
            assert_eq!(result.applied.len(), 2);
            assert!(result.buffered.is_empty());
            let quote = target.document().find_block(parent).unwrap();
            let BlockKind::BlockQuote { children } = &quote.kind else {
                panic!("quote expected")
            };
            assert_eq!(children.len_visible(), 101);
            assert_eq!(children.pending_op_count(), 0);
            assert!(target.save_snapshot().unwrap().deferred.is_empty());
            target
                .insert_text(
                    block_id_from_op(OpId {
                        peer: 99,
                        counter: 1,
                    }),
                    0,
                    "visible",
                )
                .unwrap();
            assert!(rendered(&target).contains("visible"));
            target = recovered(target);
            assert!(rendered(&target).contains("visible"));
        }
    }
}

#[test]
fn nested_anchor_and_root_in_one_message_need_no_retained_slots() {
    let limits = ValidationLimits {
        max_pending_buffer: 0,
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    for reverse in [false, true] {
        let (root, anchor, parent) = pending_quote(100, true);
        let ops = if reverse {
            vec![anchor, root]
        } else {
            vec![root, anchor]
        };
        let mut target = CollaborativeDocument::new(10);
        let result = target.apply_remote(message(ops), &limits).unwrap();
        assert_eq!(result.applied.len(), 2);
        assert!(result.buffered.is_empty());
        let BlockKind::BlockQuote { children } =
            &target.document().find_block(parent).unwrap().kind
        else {
            panic!("quote expected")
        };
        assert_eq!(children.len_visible(), 101);
        assert_eq!(children.pending_op_count(), 0);
    }
}

#[test]
fn nested_fixed_point_is_not_published_when_another_operation_refuses() {
    let (root, anchor, _) = pending_quote(100, false);
    let mut source = CollaborativeDocument::new(20);
    let block = source.insert_paragraph(None, "").unwrap();
    let mut dependent = CollaborativeDocument::new(21);
    apply(&mut dependent, history(&source));
    dependent
        .insert_text(block_id_from_op(block), 0, "x")
        .unwrap();
    let mut ops = vec![root, anchor];
    ops.extend(
        dependent
            .encode_changes_since(&source.state_vector())
            .unwrap()
            .ops,
    );
    let mut target = CollaborativeDocument::new(10);
    let before = target.save_snapshot().unwrap();
    let limits = ValidationLimits {
        max_pending_buffer: 0,
        ..ValidationLimits::default()
    };
    assert!(matches!(
        target.apply_remote(message(ops), &limits),
        Err(SessionError::Validation(ValidationError::BufferFull {
            capacity: 0
        }))
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
}

#[test]
fn retained_count_and_byte_boundaries_survive_restore_without_recharging() {
    let (root, anchor, _) = pending_quote(100, false);
    let mut refused = CollaborativeDocument::new(10);
    let before = refused.save_snapshot().unwrap();
    let zero_bytes = ValidationLimits {
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    let actual = match refused.apply_remote(message(vec![root.clone()]), &zero_bytes) {
        Err(SessionError::Validation(ValidationError::ResourceLimitExceeded {
            limit: 0,
            actual,
        })) => actual,
        other => panic!("expected retained-byte refusal, got {other:?}"),
    };
    assert_eq!(refused.save_snapshot().unwrap(), before);
    let below = ValidationLimits {
        max_retained_bytes: actual - 1,
        max_pending_buffer: 1,
        ..ValidationLimits::default()
    };
    assert!(matches!(
        refused.apply_remote(message(vec![root.clone()]), &below),
        Err(SessionError::Validation(
            ValidationError::ResourceLimitExceeded { .. }
        ))
    ));
    assert_eq!(refused.save_snapshot().unwrap(), before);
    let no_slots = ValidationLimits {
        max_pending_buffer: 0,
        ..ValidationLimits::default()
    };
    assert!(matches!(
        refused.apply_remote(message(vec![root.clone()]), &no_slots),
        Err(SessionError::Validation(ValidationError::BufferFull {
            capacity: 0
        }))
    ));
    let exact = ValidationLimits {
        max_retained_bytes: actual,
        max_pending_buffer: 1,
        ..ValidationLimits::default()
    };
    refused.apply_remote(message(vec![root]), &exact).unwrap();
    refused = recovered(refused);
    let held = refused.save_snapshot().unwrap();
    refused.apply_remote(message(Vec::new()), &exact).unwrap();
    assert_eq!(refused.save_snapshot().unwrap(), held);
    refused.apply_remote(message(vec![anchor]), &exact).unwrap();
    assert!(refused.save_snapshot().unwrap().deferred.is_empty());
}

#[test]
fn imported_document_pending_bytes_have_exact_boundaries_and_dependency_progress() {
    use md_crdt::session::snapshot::{BlockKindDto, SequenceOpDto};
    let mut source = CollaborativeDocument::new(1);
    let block = source.insert_paragraph(None, "base").unwrap();
    let block_id = block_id_from_op(block);
    let tail = source
        .document()
        .find_block(block)
        .and_then(|block| {
            md_crdt::doc::block_text_seq(&block.kind)
                .and_then(|text| text.iter_all().last().map(|element| element.id))
        })
        .unwrap();
    let missing = OpId {
        peer: 99,
        counter: 1,
    };
    let deletion = OpId {
        peer: 2,
        counter: 1,
    };
    let delete = operation(
        deletion,
        DocOp::DeleteText {
            block_elem: block,
            block_id,
            id: deletion,
            targets: vec![missing],
        },
    );
    let mut snapshot = source.save_snapshot().unwrap();
    snapshot.ops.push((delete.id, delete.payload.to_vec()));
    snapshot.state_vector.set(deletion.peer, deletion.counter);
    let block = snapshot
        .document
        .blocks
        .elements
        .iter_mut()
        .find_map(|element| element.value.as_mut())
        .unwrap();
    let BlockKindDto::Paragraph { units } = &mut block.kind else {
        panic!("paragraph expected")
    };
    units.pending.push(SequenceOpDto::Delete {
        target: missing,
        id: deletion,
    });
    let mut target = CollaborativeDocument::restore_from_snapshot(snapshot).unwrap();
    let before = target.save_snapshot().unwrap();
    let zero = ValidationLimits {
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    let actual = match target.apply_remote(message(Vec::new()), &zero) {
        Err(SessionError::Validation(ValidationError::ResourceLimitExceeded {
            limit: 0,
            actual,
        })) => actual,
        other => panic!("expected document-pending byte charge, got {other:?}"),
    };
    assert_eq!(target.save_snapshot().unwrap(), before);
    let below = ValidationLimits {
        max_retained_bytes: actual - 1,
        ..ValidationLimits::default()
    };
    assert!(matches!(
        target.apply_remote(message(Vec::new()), &below),
        Err(SessionError::Validation(
            ValidationError::ResourceLimitExceeded { .. }
        ))
    ));
    let exact = ValidationLimits {
        max_pending_buffer: 1,
        max_retained_bytes: actual,
        ..ValidationLimits::default()
    };
    target.apply_remote(message(Vec::new()), &exact).unwrap();
    target = recovered(target);
    target.apply_remote(message(Vec::new()), &exact).unwrap();
    let insert = operation(
        missing,
        DocOp::InsertText {
            block_elem: source.document().blocks_in_order()[0].elem_id,
            block_id,
            units: vec![TextUnitWire {
                id: missing,
                after: Some(tail),
                right_origin: None,
                grapheme: "x".into(),
            }],
        },
    );
    let result = target.apply_remote(message(vec![insert]), &exact).unwrap();
    assert_eq!(result.applied, vec![missing]);
    assert_eq!(rendered(&target), "base");
    let none = ValidationLimits {
        max_pending_buffer: 0,
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    target.apply_remote(message(Vec::new()), &none).unwrap();
    target = recovered(target);
    assert_eq!(rendered(&target), "base");
}

#[test]
fn nested_list_item_anchor_progresses_without_expanding_a_full_queue() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, ListItemSkeleton};
    let list = OpId {
        peer: 7,
        counter: 1,
    };
    let missing = OpId {
        peer: 99,
        counter: 1,
    };
    let items = (2..=101)
        .map(|counter| {
            let id = OpId { peer: 7, counter };
            ListItemSkeleton {
                id,
                after: Some(missing),
                right_origin: None,
                block_id: block_id_from_op(id),
                task: None,
                task_op: id,
                children: Vec::new(),
            }
        })
        .collect();
    let root = operation(
        OpId {
            peer: 7,
            counter: 101,
        },
        DocOp::InsertBlock {
            parent: None,
            after: None,
            id: list,
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id_from_op(list),
                kind: BlockKindSkeleton::List {
                    style: ListStyle::default(),
                    items,
                },
            },
        },
    );
    let anchor = operation(
        missing,
        DocOp::InsertListItem {
            list_elem: list,
            list_id: block_id_from_op(list),
            after: None,
            id: missing,
            right_origin: None,
            task: None,
        },
    );
    let limits = ValidationLimits {
        max_pending_buffer: 1,
        ..ValidationLimits::default()
    };
    for anchor_first in [false, true] {
        let mut target = CollaborativeDocument::new(10);
        let first = if anchor_first {
            anchor.clone()
        } else {
            root.clone()
        };
        let second = if anchor_first {
            root.clone()
        } else {
            anchor.clone()
        };
        target.apply_remote(message(vec![first]), &limits).unwrap();
        assert!(target.document().blocks_in_order().is_empty());
        target = recovered(target);
        target.apply_remote(message(vec![second]), &limits).unwrap();
        let BlockKind::List { items, .. } = &target.document().find_block(list).unwrap().kind
        else {
            panic!("list expected")
        };
        assert_eq!(items.len_visible(), 101);
        assert_eq!(items.pending_op_count(), 0);
        target
            .insert_paragraph_in(Some(missing), None, "visible")
            .unwrap();
        assert!(rendered(&target).contains("visible"));
    }
}

fn retained_byte_charge(session: &mut CollaborativeDocument) -> usize {
    let zero = ValidationLimits {
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    match session.apply_remote(message(Vec::new()), &zero) {
        Err(SessionError::Validation(ValidationError::ResourceLimitExceeded {
            limit: 0,
            actual,
        })) => actual,
        other => panic!("expected a retained byte charge, got {other:?}"),
    }
}

#[test]
fn deferred_accounting_tracks_retained_raw_payload_through_checkpoint_and_restore() {
    let (mut root, anchor, _) = pending_quote(100, false);
    let envelope: Envelope = serde_json::from_slice(&root.payload).unwrap();
    root.payload = serde_json::to_vec_pretty(&envelope).unwrap().into();
    let raw_bytes = root.payload.len();
    let mut target = CollaborativeDocument::new(10);
    apply(&mut target, message(vec![root]));
    let before = retained_byte_charge(&mut target);
    target = recovered(target);
    assert_eq!(retained_byte_charge(&mut target), before);
    target
        .checkpoint_history(&md_crdt::sync::CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: md_crdt::sync::DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let compacted = retained_byte_charge(&mut target);
    assert_eq!(before - compacted, raw_bytes);
    target = recovered(target);
    assert_eq!(retained_byte_charge(&mut target), compacted);
    let exact = ValidationLimits {
        max_pending_buffer: 1,
        max_retained_bytes: compacted,
        ..ValidationLimits::default()
    };
    target.apply_remote(message(vec![anchor]), &exact).unwrap();
    assert!(target.save_snapshot().unwrap().deferred.is_empty());
    assert_eq!(target.document().blocks_in_order().len(), 1);
}

#[test]
fn counter_span_duplicate_does_not_apply_or_reserve_retained_capacity() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, JsonOpCodec};
    let root = OpId {
        peer: 1,
        counter: 1,
    };
    let end = OpId {
        peer: 1,
        counter: 2,
    };
    let insertion = operation(
        end,
        DocOp::InsertBlock {
            parent: None,
            after: None,
            id: root,
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id_from_op(root),
                kind: BlockKindSkeleton::Paragraph { text: "a".into() },
            },
        },
    );
    let covered = operation(
        root,
        DocOp::DeleteBlock {
            parent: None,
            target: OpId {
                peer: 99,
                counter: 1,
            },
            id: root,
            observed: StateVector::new(),
            subtree: Vec::new(),
        },
    );
    let mut target = CollaborativeDocument::with_codec(2, JsonOpCodec, false);
    let limits = ValidationLimits {
        max_pending_buffer: 0,
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    let result = target
        .apply_remote(message(vec![insertion, covered]), &limits)
        .unwrap();
    assert_eq!(result.applied, vec![end]);
    assert!(result.buffered.is_empty());
    assert_eq!(rendered(&target), "a");
    assert_eq!(history(&target).ops.len(), 1);
}

#[test]
fn duplicate_active_opaque_identity_refuses_the_entire_message() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, JsonOpCodec};
    let opaque = block_id_from_op(OpId {
        peer: 900,
        counter: 777,
    });
    let insert = |id, logical| {
        operation(
            id,
            DocOp::InsertBlock {
                parent: None,
                after: None,
                id,
                right_origin: None,
                block: BlockSkeleton {
                    block_id: logical,
                    kind: BlockKindSkeleton::Paragraph {
                        text: String::new(),
                    },
                },
            },
        )
    };
    let mut target = CollaborativeDocument::with_codec(7, JsonOpCodec, false);
    target
        .apply_remote(
            message(vec![insert(
                OpId {
                    peer: 1,
                    counter: 1,
                },
                opaque,
            )]),
            &ValidationLimits::default(),
        )
        .unwrap();
    let before = target.save_snapshot().unwrap();
    let duplicate = OpId {
        peer: 2,
        counter: 1,
    };
    let valid = OpId {
        peer: 3,
        counter: 1,
    };
    assert!(matches!(
        target.apply_remote(message(vec![insert(valid, block_id_from_op(valid)), insert(duplicate, opaque)]), &ValidationLimits::default()),
        Err(SessionError::Validation(ValidationError::MalformedOperation {
            op_id, kind: md_crdt::sync::MalformedKind::InvalidSequence,
        })) if op_id == duplicate
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
}

#[test]
fn deleted_opaque_identity_can_be_recreated_in_the_same_message() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, JsonOpCodec};
    let old = OpId {
        peer: 1,
        counter: 1,
    };
    let opaque = block_id_from_op(OpId {
        peer: 900,
        counter: 777,
    });
    let insert = |id, text: &str| DocOp::InsertBlock {
        parent: None,
        after: None,
        id,
        right_origin: None,
        block: BlockSkeleton {
            block_id: opaque,
            kind: BlockKindSkeleton::Paragraph { text: text.into() },
        },
    };
    let mut target = CollaborativeDocument::with_codec(7, JsonOpCodec, false);
    target
        .apply_remote(
            message(vec![operation(old, insert(old, ""))]),
            &ValidationLimits::default(),
        )
        .unwrap();
    let deletion = OpId {
        peer: 2,
        counter: 1,
    };
    let replacement = OpId {
        peer: 2,
        counter: 2,
    };
    let end = OpId {
        peer: 2,
        counter: 5,
    };
    let limits = ValidationLimits {
        max_pending_buffer: 0,
        max_retained_bytes: 0,
        ..ValidationLimits::default()
    };
    let result = target
        .apply_remote(
            message(vec![
                operation(
                    deletion,
                    DocOp::DeleteBlockById {
                        parent: None,
                        block_id: opaque,
                        target: old,
                        id: deletion,
                        observed: StateVector::new(),
                        subtree: vec![md_crdt::doc::LogicalTarget {
                            container: md_crdt::doc::LogicalContainer::Block(opaque),
                            origin: old,
                        }],
                    },
                ),
                operation(end, insert(replacement, "new")),
            ]),
            &limits,
        )
        .unwrap();
    assert_eq!(result.applied.len(), 2);
    assert!(result.buffered.is_empty());
    assert_eq!(rendered(&target), "new");
    assert_eq!(rendered(&recovered(target)), "new");
}

#[test]
fn imported_pending_row_move_accepts_anchor_and_observed_follower_at_capacity() {
    use md_crdt::doc::{ColumnAlignment, ColumnDef, PendingTableMove};
    use md_crdt::session::snapshot::BlockKindDto;

    let mut base = CollaborativeDocument::new(1);
    let table = base
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Default,
            }],
            vec!["header".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table);
    let row = base
        .insert_table_row(table_id, None, vec!["original".into()])
        .unwrap();
    let row_id = block_id_from_op(row);
    let column_id = {
        let BlockKind::Table { table } = &base.document().find_block(table).unwrap().kind else {
            unreachable!()
        };
        table.columns_in_order()[0].id
    };
    let mut anchor_source = CollaborativeDocument::new(2);
    apply(&mut anchor_source, history(&base));
    let anchor = anchor_source
        .insert_table_row(table_id, None, vec!["anchor".into()])
        .unwrap();
    let dependency = anchor_source
        .encode_changes_since(&base.state_vector())
        .unwrap();
    let mut mover = CollaborativeDocument::new(3);
    apply(&mut mover, history(&anchor_source));
    let movement = mover
        .move_table_row(table_id, row_id, Some(anchor))
        .unwrap();
    let movement_message = mover
        .encode_changes_since(&anchor_source.state_vector())
        .unwrap();
    let operation = &movement_message.ops[0];
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
    let mut snapshot = base.save_snapshot().unwrap();
    snapshot.peer = 9;
    snapshot.next_counter = 1;
    snapshot.state_vector.set(movement.peer, movement.counter);
    snapshot
        .ops
        .push((operation.id, operation.payload.to_vec()));
    let block = snapshot
        .document
        .blocks
        .elements
        .iter_mut()
        .filter_map(|element| element.value.as_mut())
        .find(|block| block.id == table_id)
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
    let imported = md_crdt::SessionSnapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap();
    let mut follower_source = CollaborativeDocument::new(4);
    apply(&mut follower_source, history(&mover));
    let follower = follower_source
        .set_table_cell(table_id, row_id, column_id, "updated".into())
        .unwrap();
    let follower_message = follower_source
        .encode_changes_since(&mover.state_vector())
        .unwrap();
    let limits = ValidationLimits {
        max_pending_buffer: 1,
        max_dependency_work: 1_000_000,
        ..ValidationLimits::default()
    };
    let restore = || CollaborativeDocument::restore_from_snapshot(imported.clone()).unwrap();
    let assert_drained = |session: &CollaborativeDocument| {
        let BlockKind::Table { table } =
            &session.document().find_block_by_id(table_id).unwrap().kind
        else {
            unreachable!()
        };
        assert!(table.pending_row_moves.is_empty());
        assert_eq!(table.rows.pending_op_count(), 0);
        assert!(session.save_snapshot().unwrap().deferred.is_empty());
        assert!(session.save_snapshot().unwrap().pending.is_empty());
        assert_eq!(
            table.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![block_id_from_op(anchor), row_id]
        );
    };
    let mut sequential = restore();
    let BlockKind::Table { table } = &sequential
        .document()
        .find_block_by_id(table_id)
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    // The sole retained operation is the imported move; both sequences are clean.
    let before = sequential.save_snapshot().unwrap();
    assert_eq!(
        table.pending_row_moves.len() + before.deferred.len() + before.pending.len(),
        limits.max_pending_buffer
    );
    assert_eq!(table.rows.pending_op_count(), 0);
    assert_eq!(table.columns.pending_op_count(), 0);
    sequential
        .apply_remote(message(Vec::new()), &limits)
        .unwrap();
    assert_eq!(sequential.save_snapshot().unwrap(), before);
    assert!(matches!(
        sequential.apply_remote(follower_message.clone(), &limits),
        Err(SessionError::Validation(ValidationError::BufferFull {
            capacity: 1
        }))
    ));
    assert_eq!(sequential.save_snapshot().unwrap(), before);
    let result = sequential
        .apply_remote(dependency.clone(), &limits)
        .unwrap();
    assert!(result.applied.contains(&movement));
    assert_drained(&sequential);
    sequential
        .apply_remote(follower_message.clone(), &limits)
        .unwrap();
    assert_drained(&sequential);
    assert_eq!(rendered(&sequential), rendered(&follower_source));

    for reverse in [false, true] {
        let mut combined = restore();
        let mut ops = dependency.ops.clone();
        ops.extend(follower_message.ops.clone());
        if reverse {
            ops.reverse();
        }
        let before = combined.save_snapshot().unwrap();
        let no_work = ValidationLimits {
            max_dependency_work: 0,
            ..limits.clone()
        };
        assert!(matches!(
            combined.apply_remote(message(ops.clone()), &no_work),
            Err(SessionError::Validation(ValidationError::BufferFull {
                capacity: 1
            }))
        ));
        assert_eq!(combined.save_snapshot().unwrap(), before);
        let result = combined.apply_remote(message(ops), &limits);
        if result.is_err() {
            assert_eq!(combined.save_snapshot().unwrap(), before);
        }
        let result = result.unwrap();
        assert!(result.applied.contains(&movement));
        assert!(result.applied.contains(&follower));
        assert!(result.buffered.is_empty());
        assert_drained(&combined);
        assert_eq!(rendered(&combined), rendered(&sequential));
        assert_eq!(combined.state_vector(), sequential.state_vector());
    }
}

#[test]
fn malformed_move_placement_spans_are_atomic_and_do_not_claim_future_counters() {
    let mut base = CollaborativeDocument::new(1);
    let a = base
        .insert_block(None, BlockKind::RawBlock { raw: "A".into() })
        .unwrap();
    let b = base
        .insert_block(Some(a), BlockKind::RawBlock { raw: "B".into() })
        .unwrap();
    let mut origin = CollaborativeDocument::new(9);
    apply(&mut origin, history(&base));
    let start = origin.peek_next_id().counter;
    let end = start + 1;
    let mut unrelated = CollaborativeDocument::new(8);
    unrelated
        .insert_block(
            None,
            BlockKind::RawBlock {
                raw: "unrelated".into(),
            },
        )
        .unwrap();
    let placements = [
        ("empty", Vec::new(), end, end),
        ("duplicate", vec![end, end], end, end),
        ("duplicate logical source", vec![start, end], end, end),
        ("gap", vec![start, end + 1], end + 1, end + 1),
        ("above envelope", vec![end + 1, end + 2], end, end),
        ("descending", vec![end, start], end, end),
        ("ends below envelope", vec![start], end, end),
        ("outer id mismatch", vec![start, end], end, end + 1),
        ("zero", vec![0, 1], end, end),
        ("overflow", vec![u64::MAX, 0], end, end),
    ];
    for (case, counters, move_counter, operation_counter) in placements {
        let mut target = CollaborativeDocument::new(3);
        apply(&mut target, history(&base));
        let mut blocks = Vec::new();
        for (index, counter) in counters.into_iter().enumerate() {
            let placement = OpId { peer: 9, counter };
            let target = if case == "duplicate logical source" {
                a
            } else {
                [a, b][index]
            };
            blocks.push(md_crdt::codec::MovedBlockWire {
                block_id: block_id_from_op(target),
                target,
                id: placement,
                after: blocks
                    .last()
                    .map(|block: &md_crdt::codec::MovedBlockWire| block.id),
                right_origin: None,
            });
        }
        let id = OpId {
            peer: 9,
            counter: operation_counter,
        };
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::MoveBlocks {
                to_parent: None,
                id: OpId {
                    peer: 9,
                    counter: move_counter,
                },
                observed: md_crdt::StateVector::new(),
                blocks,
            }),
        };
        // A valid operation in the same call must not commit either.
        let mut message = history(&unrelated);
        message.ops.push(Operation {
            id,
            payload: serde_json::to_vec(&envelope).unwrap().into(),
        });
        let before = target.save_snapshot().unwrap();
        assert!(
            matches!(
                target.apply_remote(message, &ValidationLimits::default()),
                Err(SessionError::Validation(ValidationError::MalformedOperation {
                    op_id,
                    kind: md_crdt::sync::MalformedKind::InvalidSequence,
                })) if op_id == id
            ),
            "{case}"
        );
        assert_eq!(target.save_snapshot().unwrap(), before, "{case}");
        target = recovered(target);
        let mut genuine = origin.clone();
        for label in ["genuine-1", "genuine-2", "genuine-3", "genuine-4"] {
            let elem = genuine
                .insert_block(None, BlockKind::RawBlock { raw: label.into() })
                .unwrap();
            let delta = genuine
                .encode_changes_since(&target.state_vector())
                .unwrap();
            apply(&mut target, delta);
            assert!(
                target
                    .document()
                    .find_block_by_id(block_id_from_op(elem))
                    .is_some(),
                "{case}: {label}"
            );
            assert_eq!(rendered(&target), rendered(&genuine), "{case}: {label}");
            assert_eq!(
                target.state_vector(),
                genuine.state_vector(),
                "{case}: {label}"
            );
        }
    }
}

#[test]
fn move_placement_peer_mismatch_is_typed_and_atomic() {
    let mut base = CollaborativeDocument::new(1);
    let elem = base.insert_paragraph(None, "A").unwrap();
    let mut target = CollaborativeDocument::new(3);
    apply(&mut target, history(&base));
    let id = OpId {
        peer: 9,
        counter: 1,
    };
    let envelope = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::MoveBlocks {
            to_parent: None,
            id,
            observed: md_crdt::StateVector::new(),
            blocks: vec![md_crdt::codec::MovedBlockWire {
                block_id: block_id_from_op(elem),
                target: elem,
                id: OpId {
                    peer: 8,
                    counter: 1,
                },
                after: None,
                right_origin: None,
            }],
        }),
    };
    let before = target.save_snapshot().unwrap();
    assert!(matches!(
        target.apply_remote(
            ChangeMessage {
                since: target.state_vector(),
                ops: vec![Operation {
                    id,
                    payload: serde_json::to_vec(&envelope).unwrap().into(),
                }],
            },
            &ValidationLimits::default()
        ),
        Err(SessionError::PeerMismatch)
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
}
