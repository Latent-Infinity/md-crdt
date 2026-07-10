//! Collaborative session: multi-peer block insert/delete over the wire.

use md_crdt::codec::{DocOp, Envelope, JsonOpCodec, OpBody, OpCodec, WIRE_VERSION};
use md_crdt::core::OpId;
use md_crdt::doc::{BlockKind, EquivalenceMode, block_id_from_op};
use md_crdt::session::{CollaborativeDocument, SessionError};
use md_crdt::sync::{ChangeMessage, Operation, ValidationLimits};

fn para(text: &str) -> BlockKind {
    BlockKind::Paragraph { text: text.into() }
}

fn exchange(from: &CollaborativeDocument, to: &mut CollaborativeDocument) {
    let msg = from.encode_changes_since(&to.state_vector());
    to.apply_remote(msg, &ValidationLimits::default())
        .expect("apply_remote");
}

#[test]
fn two_peers_concurrent_block_inserts_converge() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let id_a = a.insert_block(None, para("from-a")).expect("a");
    let id_b = b.insert_block(None, para("from-b")).expect("b");
    assert_ne!(id_a, id_b);

    exchange(&a, &mut b);
    exchange(&b, &mut a);

    let out_a = a.document().serialize(EquivalenceMode::Structural);
    let out_b = b.document().serialize(EquivalenceMode::Structural);
    assert_eq!(out_a, out_b);

    let texts_a: Vec<_> = a
        .document()
        .blocks_in_order()
        .iter()
        .map(|bl| match &bl.kind {
            BlockKind::Paragraph { text } => text.as_str(),
            _ => "",
        })
        .collect();
    assert_eq!(texts_a.len(), 2);
    // RGA sibling order: higher OpId first when both after None.
    // peer 2 > peer 1 at same counter → "from-b" then "from-a" or vice versa
    // depending on OpId ordering (counter, peer).
    assert!(texts_a.contains(&"from-a"));
    assert!(texts_a.contains(&"from-b"));
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn insert_then_delete_propagates() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let id = a.insert_block(None, para("temp")).expect("insert");
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
fn operation_id_equals_block_elem_on_insert() {
    let mut a = CollaborativeDocument::new(5);
    let elem = a.insert_block(None, para("x")).expect("insert");
    let outbox = {
        // Access via encode from empty SV
        let msg = a.encode_changes_since(&md_crdt::core::StateVector::new());
        assert_eq!(msg.ops.len(), 1);
        msg.ops[0].clone()
    };
    assert_eq!(outbox.id, elem);
    let env = JsonOpCodec.decode(&outbox.payload).expect("decode");
    match env.body {
        OpBody::Doc(DocOp::InsertBlock { id, block, .. }) => {
            assert_eq!(id, elem);
            assert_eq!(block.block_id, block_id_from_op(elem));
        }
        _ => panic!("expected InsertBlock"),
    }
}

#[test]
fn out_of_order_remote_ops_buffer_then_apply() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let first = a.insert_block(None, para("one")).expect("1");
    let second = a.insert_block(Some(first), para("two")).expect("2");

    let msg = a.encode_changes_since(&b.state_vector());
    assert_eq!(msg.ops.len(), 2);

    // Deliver only the second op first (by counter for peer 1).
    let op2 = msg
        .ops
        .iter()
        .find(|o| o.id == second)
        .expect("op2")
        .clone();
    let partial = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![op2],
    };
    let r = b
        .apply_remote(partial, &ValidationLimits::default())
        .expect("buffer");
    assert!(r.buffered.contains(&second));
    assert!(r.applied.is_empty());
    assert_eq!(b.document().blocks_in_order().len(), 0);

    let op1 = msg.ops.iter().find(|o| o.id == first).expect("op1").clone();
    let rest = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![op1],
    };
    let r2 = b
        .apply_remote(rest, &ValidationLimits::default())
        .expect("apply both");
    assert!(r2.applied.contains(&first));
    assert!(r2.applied.contains(&second));
    assert_eq!(b.document().blocks_in_order().len(), 2);
}

#[test]
fn unknown_wire_version_rejected_without_mutation() {
    let mut b = CollaborativeDocument::new(2);
    let mut bad = Envelope {
        version: WIRE_VERSION + 9,
        body: OpBody::Doc(DocOp::DeleteBlock {
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
    // Force version after construction
    bad.version = 99;
    let payload = serde_json::to_vec(&bad).expect("json");
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation {
            id: OpId {
                counter: 1,
                peer: 1,
            },
            payload,
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
    let mut a = CollaborativeDocument::with_codec(1, JsonOpCodec, true);
    // Local insert_block strips paragraph body on the wire and document apply
    // follows the envelope (empty); body text arrives later via InsertText.
    let id = a
        .insert_block(None, para("local-body-stripped"))
        .expect("local ok");
    assert_eq!(
        match &a.document().blocks_in_order()[0].kind {
            BlockKind::Paragraph { text } => text.as_str(),
            _ => "?",
        },
        ""
    );
    assert_eq!(id.counter, 1);

    // Craft a malicious remote InsertBlock with non-empty paragraph text.
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
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
    let mut b = CollaborativeDocument::with_codec(2, JsonOpCodec, true);
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation {
            id: OpId {
                counter: 1,
                peer: 9,
            },
            payload,
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
        .insert_block(Some(ghost), para("x"))
        .expect_err("missing anchor");
    assert!(matches!(err, SessionError::MissingAfterAnchor));
    // N3: failed commit must not advance the clock.
    assert_eq!(a.peek_next_id().counter, 1);
    assert!(a.document().blocks_in_order().is_empty());
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
    let mut b = CollaborativeDocument::new(2);
    let block_op = OpId {
        counter: 3,
        peer: 1,
    };
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
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
    // Operation.id (counter 4) disagrees with the envelope's max embedded id (counter 3).
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation {
            id: OpId {
                counter: 4,
                peer: 1,
            },
            payload,
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
    // Nested child carries a different peer but a lower counter, so `top` is still
    // the max embedded id — the id-max check passes and peer consistency must reject.
    let child = OpId {
        counter: 5,
        peer: 8,
    };
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
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
                            kind: BlockKindSkeleton::Paragraph { text: "x".into() },
                        },
                    }],
                },
            },
        }),
    };
    let payload = JsonOpCodec.encode(&env).expect("enc");
    let msg = ChangeMessage {
        since: md_crdt::core::StateVector::new(),
        ops: vec![Operation { id: top, payload }],
    };
    let err = b
        .apply_remote(msg, &ValidationLimits::default())
        .expect_err("peer mismatch");
    assert!(matches!(err, SessionError::PeerMismatch));
    assert!(b.document().blocks_in_order().is_empty());
}

#[test]
fn insert_block_rejects_table_until_wire_ready() {
    use md_crdt::doc::{ColumnAlignment, ColumnDef, Table};
    let mut a = CollaborativeDocument::new(1);
    let table_op = OpId {
        counter: 1,
        peer: 1,
    };
    let table = Table::new(
        block_id_from_op(table_op),
        table_op,
        vec![ColumnDef {
            alignment: ColumnAlignment::Left,
        }],
        vec!["h".into()],
        table_op,
    );
    let err = a
        .insert_block(None, BlockKind::Table { table })
        .expect_err("table unsupported");
    assert!(matches!(err, SessionError::UnsupportedBlockKind("table")));
    // Fail-loud: nothing inserted locally and the clock is not advanced.
    assert!(a.document().blocks_in_order().is_empty());
    assert_eq!(a.peek_next_id().counter, 1);
}
