//! Collaborative session: multi-peer block insert/delete over the wire.

use md_crdt::codec::{DocOp, Envelope, JsonOpCodec, OpBody, OpCodec, WIRE_VERSION};
use md_crdt::core::OpId;
use md_crdt::doc::{BlockKind, EquivalenceMode, block_id_from_op};
use md_crdt::session::{CollaborativeDocument, SessionError};
use md_crdt::sync::{ChangeMessage, Operation, ValidationLimits};

fn para(text: &str) -> BlockKind {
    BlockKind::paragraph(
        text,
        OpId {
            counter: 1,
            peer: 0,
        },
    )
}

/// String-mode session (legacy InsertBlock body expansion).
fn string_mode(peer: u64) -> CollaborativeDocument {
    CollaborativeDocument::with_codec(peer, JsonOpCodec, false)
}

fn exchange(from: &CollaborativeDocument, to: &mut CollaborativeDocument) {
    let msg = from.encode_changes_since(&to.state_vector()).unwrap();
    to.apply_remote(msg, &ValidationLimits::default())
        .expect("apply_remote");
}

#[test]
fn two_peers_concurrent_block_inserts_converge() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let id_a = a.insert_paragraph(None, "from-a").expect("a");
    let id_b = b.insert_paragraph(None, "from-b").expect("b");
    assert_ne!(id_a, id_b);

    exchange(&a, &mut b);
    exchange(&b, &mut a);

    let out_a = a.document().serialize(EquivalenceMode::Structural);
    let out_b = b.document().serialize(EquivalenceMode::Structural);
    assert_eq!(out_a, out_b);

    let texts_a: Vec<String> = a
        .document()
        .blocks_in_order()
        .iter()
        .map(|bl| match &bl.kind {
            BlockKind::Paragraph { text } => md_crdt::doc::paragraph_visible_string(text),
            _ => String::new(),
        })
        .collect();
    assert_eq!(texts_a.len(), 2);
    assert!(texts_a.iter().any(|s| s == "from-a"));
    assert!(texts_a.iter().any(|s| s == "from-b"));
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn insert_then_delete_propagates() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let id = a.insert_paragraph(None, "temp").expect("insert");
    exchange(&a, &mut b);
    assert_eq!(b.document().blocks_in_order().len(), 1);

    a.delete_block(id).expect("delete");
    exchange(&a, &mut b);
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.document().blocks_in_order().len(), 0);
    assert_eq!(b.document().blocks_in_order().len(), 0);
}

#[test]
fn operation_id_is_max_embedded_unit_on_insert_text() {
    let mut a = CollaborativeDocument::new(5);
    let elem = a.insert_paragraph(None, "xy").expect("insert");
    let msg = a
        .encode_changes_since(&md_crdt::core::StateVector::new())
        .unwrap();
    // InsertBlock (empty) + InsertText (two units)
    assert_eq!(msg.ops.len(), 2);
    assert_eq!(msg.ops[0].id, elem); // block-only span 1
    let text_op = msg.ops[1].clone();
    assert_eq!(text_op.id.peer, elem.peer);
    assert_eq!(text_op.id.counter, elem.counter + 2); // two unit ids after block
    let env = JsonOpCodec.decode(&text_op.payload).expect("decode");
    match env.body {
        OpBody::Doc(DocOp::InsertText { units, .. }) => {
            assert_eq!(units.len(), 2);
            assert_eq!(text_op.id, units[1].id);
        }
        _ => panic!("expected InsertText"),
    }
}

#[test]
fn out_of_order_remote_ops_buffer_then_apply() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let first = a.insert_paragraph(None, "one").expect("1");
    let _second = a.insert_paragraph(Some(first), "two").expect("2");

    let msg = a.encode_changes_since(&b.state_vector()).unwrap();
    // Each paragraph: InsertBlock + InsertText → 4 ops
    assert_eq!(msg.ops.len(), 4);
    let mut ops = msg.ops.clone();
    ops.sort_by_key(|o| o.id.counter);
    let op_lo = ops[0].clone();
    let op_hi = ops[ops.len() - 1].clone();

    // Deliver the highest op first → buffers.
    let r = b
        .apply_remote(
            ChangeMessage {
                since: md_crdt::core::StateVector::new(),
                ops: vec![op_hi.clone()],
            },
            &ValidationLimits::default(),
        )
        .expect("buffer");
    assert!(r.buffered.contains(&op_hi.id));
    assert!(r.applied.is_empty());

    // Deliver remaining in order → promote.
    let rest: Vec<_> = ops.into_iter().filter(|o| o.id != op_hi.id).collect();
    let r2 = b
        .apply_remote(
            ChangeMessage {
                since: md_crdt::core::StateVector::new(),
                ops: rest,
            },
            &ValidationLimits::default(),
        )
        .expect("apply rest");
    assert!(r2.applied.contains(&op_lo.id));
    assert_eq!(b.document().blocks_in_order().len(), 2);
}

#[test]
fn unknown_wire_version_rejected_without_mutation() {
    let mut b = CollaborativeDocument::new(2);
    let mut bad = Envelope {
        version: WIRE_VERSION + 9,
        body: OpBody::Doc(DocOp::DeleteBlock {
            parent: None,
            target: OpId {
                counter: 1,
                peer: 1,
            },
            id: OpId {
                counter: 1,
                peer: 1,
            },
        }),
    };
    bad.version = 99;
    let payload = serde_json::to_vec(&bad).expect("json");
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation {
            id: OpId {
                counter: 1,
                peer: 1,
            },
            payload: payload.into(),
        }],
    };
    let err = b
        .apply_remote(msg, &ValidationLimits::default())
        .expect_err("version");
    assert!(matches!(err, SessionError::UnknownWireVersion(99)));
    assert_eq!(b.document().blocks_in_order().len(), 0);
}

#[test]
fn unit_mode_rejects_non_empty_paragraph_on_insert_block() {
    let mut a = CollaborativeDocument::new(1);
    // Local insert_block strips paragraph body on the wire and document apply
    // follows the envelope (empty); body text arrives later via InsertText.
    let id = a
        .insert_block(None, para("local-body-stripped"))
        .expect("local ok");
    assert_eq!(
        match &a.document().blocks_in_order()[0].kind {
            BlockKind::Paragraph { text } => md_crdt::doc::paragraph_visible_string(text),
            _ => "?".into(),
        },
        ""
    );
    assert_eq!(id.counter, 1);

    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: OpId {
                counter: 1,
                peer: 9,
            },
            right_origin: None,
            block: md_crdt::codec::BlockSkeleton {
                block_id: block_id_from_op(OpId {
                    counter: 1,
                    peer: 9,
                }),
                kind: md_crdt::codec::BlockKindSkeleton::Paragraph {
                    text: "sneaky".into(),
                },
            },
        }),
    };
    let payload = JsonOpCodec.encode(&env).expect("enc");
    let mut b = CollaborativeDocument::new(2);
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation {
            id: OpId {
                counter: 1,
                peer: 9,
            },
            payload: payload.into(),
        }],
    };
    let err = b
        .apply_remote(msg, &ValidationLimits::default())
        .expect_err("reject");
    assert!(matches!(err, SessionError::NonEmptyParagraphOnInsertBlock));
}

#[test]
fn insert_block_missing_after_anchor_errors_without_clock_burn() {
    let mut a = CollaborativeDocument::new(1);
    let ghost = OpId {
        counter: 999,
        peer: 7,
    };
    let err = a
        .insert_block(Some(ghost), para(""))
        .expect_err("missing anchor");
    assert!(matches!(err, SessionError::MissingAfterAnchor));
    assert_eq!(a.peek_next_id().counter, 1);
    assert!(a.document().blocks_in_order().is_empty());
}

#[test]
fn direct_structured_insert_rejects_invalid_metadata_without_clock_burn() {
    let mut session = CollaborativeDocument::new(1);
    let before = session.peek_next_id();
    let heading = BlockKind::Heading {
        level: 0,
        text: md_crdt::core::Sequence::new(),
    };
    assert!(matches!(
        session.insert_block(None, heading),
        Err(SessionError::InvalidHeadingLevel)
    ));
    let fence = BlockKind::CodeFence {
        style: md_crdt::CodeFenceStyle {
            marker: md_crdt::FenceMarker::Backtick,
            length: 2,
        },
        info: None,
        text: String::new(),
    };
    assert!(matches!(
        session.insert_block(None, fence),
        Err(SessionError::StructuredEdit(_))
    ));
    assert_eq!(session.peek_next_id(), before);
    assert!(session.document().blocks_in_order().is_empty());
}

#[test]
fn delete_block_missing_target_errors_without_clock_burn() {
    let mut a = CollaborativeDocument::new(1);
    let ghost = OpId {
        counter: 42,
        peer: 3,
    };
    let err = a.delete_block(ghost).expect_err("missing target");
    assert!(matches!(err, SessionError::MissingDeleteTarget));
    assert_eq!(a.peek_next_id().counter, 1);
}

#[test]
fn remote_operation_id_not_max_rejected() {
    // String-mode: non-empty InsertBlock expands units so max id is block+G.
    let mut b = string_mode(2);
    let block_op = OpId {
        counter: 3,
        peer: 1,
    };
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: block_op,
            right_origin: None,
            block: md_crdt::codec::BlockSkeleton {
                block_id: block_id_from_op(block_op),
                kind: md_crdt::codec::BlockKindSkeleton::Paragraph { text: "x".into() },
            },
        }),
    };
    let payload = JsonOpCodec.encode(&env).expect("enc");
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation {
            id: OpId {
                counter: 5,
                peer: 1,
            },
            payload: payload.into(),
        }],
    };
    let err = b
        .apply_remote(msg, &ValidationLimits::default())
        .expect_err("id mismatch");
    assert!(matches!(err, SessionError::OperationIdMismatch));
    assert!(b.document().blocks_in_order().is_empty());
}

#[test]
fn remote_peer_mismatch_in_nested_child_rejected() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert};
    let mut b = CollaborativeDocument::new(2);
    let top = OpId {
        counter: 10,
        peer: 1,
    };
    let child = OpId {
        counter: 5,
        peer: 8,
    };
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: top,
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id_from_op(top),
                kind: BlockKindSkeleton::BlockQuote {
                    children: vec![BlockSkeletonInsert {
                        after: None,
                        id: child,
                        right_origin: None,
                        block: BlockSkeleton {
                            block_id: block_id_from_op(child),
                            kind: BlockKindSkeleton::Paragraph {
                                text: String::new(),
                            },
                        },
                    }],
                },
            },
        }),
    };
    let payload = JsonOpCodec.encode(&env).expect("enc");
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation {
            id: top,
            payload: payload.into(),
        }],
    };
    let err = b
        .apply_remote(msg, &ValidationLimits::default())
        .expect_err("peer mismatch");
    assert!(matches!(err, SessionError::PeerMismatch));
    assert!(b.document().blocks_in_order().is_empty());
}

#[test]
fn insert_empty_table_propagates_over_wire() {
    use md_crdt::doc::Table;
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);
    let table_op = OpId {
        counter: 1,
        peer: 1,
    };
    let table = Table::new(block_id_from_op(table_op), table_op, table_op);
    let elem = a
        .insert_block(
            None,
            BlockKind::Table {
                table: Box::new(table),
            },
        )
        .expect("table supported");
    assert_eq!(elem, table_op);
    exchange(&a, &mut b);
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
}

// Regression: string-mode InsertBlock expands the paragraph body into units at b+1..b+G.
// Span-aware sync makes the operation cover the whole [b, b+G] range (Operation.id = b+G),
// so the clock advances past the units and no later id collides with them.
#[test]
fn string_mode_paragraph_units_do_not_collide_with_block_ids() {
    use md_crdt::doc::paragraph_visible_ids;
    use std::collections::HashSet;
    let mut a = string_mode(1);
    a.insert_block(None, para("ab")).expect("p1");
    a.insert_block(None, para("cd")).expect("p2");

    let mut seen: HashSet<OpId> = HashSet::new();
    for bl in a.document().blocks_in_order() {
        assert!(
            seen.insert(bl.elem_id),
            "OpId {:?} reused as a block elem_id",
            bl.elem_id
        );
        if let BlockKind::Paragraph { text } = &bl.kind {
            for uid in paragraph_visible_ids(text) {
                assert!(
                    seen.insert(uid),
                    "OpId {uid:?} shared by a block and/or another text unit"
                );
            }
        }
    }
}

#[test]
fn unit_mode_paragraph_units_do_not_collide_with_block_ids() {
    use md_crdt::doc::paragraph_visible_ids;
    use std::collections::HashSet;
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "ab").expect("p1");
    a.insert_paragraph(None, "cd").expect("p2");

    let mut seen: HashSet<OpId> = HashSet::new();
    for bl in a.document().blocks_in_order() {
        assert!(seen.insert(bl.elem_id), "block OpId {:?}", bl.elem_id);
        if let BlockKind::Paragraph { text } = &bl.kind {
            for uid in paragraph_visible_ids(text) {
                assert!(seen.insert(uid), "unit OpId {uid:?}");
            }
        }
    }
}
