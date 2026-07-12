//! Unit-mode text CRDT: InsertText / DeleteText wire + concurrent paragraph collab.

use md_crdt::codec::{DocOp, JsonOpCodec, OpBody, OpCodec, WIRE_VERSION};
use md_crdt::core::OpId;
use md_crdt::doc::{BlockKind, EquivalenceMode, block_id_from_op, paragraph_visible_string};
use md_crdt::session::{CollaborativeDocument, SessionError};
use md_crdt::sync::{ChangeMessage, Operation, ValidationLimits};

fn exchange(from: &CollaborativeDocument, to: &mut CollaborativeDocument) {
    let msg = from.encode_changes_since(&to.state_vector());
    to.apply_remote(msg, &ValidationLimits::default())
        .expect("apply_remote");
}

fn para_text(doc: &CollaborativeDocument, idx: usize) -> String {
    match &doc.document().blocks_in_order()[idx].kind {
        BlockKind::Paragraph { text } => paragraph_visible_string(text),
        _ => String::new(),
    }
}

#[test]
fn insert_paragraph_emits_two_ops_for_nonempty_body() {
    let mut a = CollaborativeDocument::new(1);
    let elem = a.insert_paragraph(None, "hi").expect("insert_paragraph");
    assert_eq!(elem.counter, 1);
    assert_eq!(para_text(&a, 0), "hi");

    let msg = a.encode_changes_since(&md_crdt::core::StateVector::new());
    assert_eq!(msg.ops.len(), 2);

    let env0 = JsonOpCodec.decode(&msg.ops[0].payload).expect("dec0");
    let env1 = JsonOpCodec.decode(&msg.ops[1].payload).expect("dec1");
    match env0.body {
        OpBody::Doc(DocOp::InsertBlock { id, block, .. }) => {
            assert_eq!(id, elem);
            match block.kind {
                md_crdt::codec::BlockKindSkeleton::Paragraph { text } => {
                    assert!(text.is_empty(), "N6-d: skeleton must be empty");
                }
                _ => panic!("expected paragraph"),
            }
        }
        _ => panic!("op0 InsertBlock"),
    }
    match env1.body {
        OpBody::Doc(DocOp::InsertText { units, .. }) => {
            assert_eq!(units.len(), 2);
            assert_eq!(units[0].grapheme, "h");
            assert_eq!(units[1].grapheme, "i");
            // Operation.id is max unit id (N1).
            assert_eq!(msg.ops[1].id, units[1].id);
            // N4: right_origin field is present on the wire (serde includes Option).
            let _ = units[0].right_origin;
            let _ = units[1].right_origin;
        }
        _ => panic!("op1 InsertText"),
    }
}

#[test]
fn insert_paragraph_empty_is_block_only() {
    let mut a = CollaborativeDocument::new(1);
    let _ = a.insert_paragraph(None, "").expect("empty");
    let msg = a.encode_changes_since(&md_crdt::core::StateVector::new());
    assert_eq!(msg.ops.len(), 1);
    let env = JsonOpCodec.decode(&msg.ops[0].payload).expect("dec");
    assert!(matches!(env.body, OpBody::Doc(DocOp::InsertBlock { .. })));
}

#[test]
fn insert_text_wire_round_trip_preserves_right_origin() {
    let mut a = CollaborativeDocument::new(3);
    let elem = a.insert_paragraph(None, "a").expect("p");
    let bid = block_id_from_op(elem);
    a.insert_text(bid, 1, "bc").expect("paste");

    let msg = a.encode_changes_since(&md_crdt::core::StateVector::new());
    // InsertBlock + InsertText("a") + InsertText("bc")
    assert_eq!(msg.ops.len(), 3);
    let last = msg.ops.last().unwrap();
    let env = JsonOpCodec.decode(&last.payload).expect("dec");
    let re = JsonOpCodec
        .encode(&env)
        .and_then(|b| JsonOpCodec.decode(&b))
        .expect("round-trip");
    match (env.body, re.body) {
        (
            OpBody::Doc(DocOp::InsertText { units: u1, .. }),
            OpBody::Doc(DocOp::InsertText { units: u2, .. }),
        ) => {
            assert_eq!(u1, u2);
            assert_eq!(u1.len(), 2);
            // Chain: second unit after first; first has after of prior unit "a".
            assert_eq!(u1[1].after, Some(u1[0].id));
        }
        _ => panic!("expected InsertText"),
    }
}

#[test]
fn concurrent_same_paragraph_inserts_converge() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let elem = a.insert_paragraph(None, "X").expect("base");
    exchange(&a, &mut b);
    let bid = block_id_from_op(elem);

    // Concurrent inserts at the same offset (after "X").
    a.insert_text(bid, 1, "A").expect("a");
    b.insert_text(bid, 1, "B").expect("b");

    exchange(&a, &mut b);
    exchange(&b, &mut a);

    let ta = para_text(&a, 0);
    let tb = para_text(&b, 0);
    assert_eq!(ta, tb);
    assert!(ta.contains('X') && ta.contains('A') && ta.contains('B'));
    assert_eq!(ta.len(), 3);
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn concurrent_multi_unit_paste_converges_without_interleaving() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let elem = a.insert_paragraph(None, "X").expect("base");
    exchange(&a, &mut b);
    let bid = block_id_from_op(elem);

    // Concurrent *multi-grapheme* pastes at the same offset (after "X"). Chain units
    // carry right_origin = None; the RGA's `after`-tree must still keep each run
    // contiguous and converge on both peers.
    a.insert_text(bid, 1, "AB").expect("a paste");
    b.insert_text(bid, 1, "CD").expect("b paste");

    exchange(&a, &mut b);
    exchange(&b, &mut a);

    let ta = para_text(&a, 0);
    let tb = para_text(&b, 0);
    assert_eq!(ta, tb, "peers must converge");
    assert_eq!(ta.chars().count(), 5);
    // No interleaving: each pasted run stays contiguous.
    assert!(
        ta == "XABCD" || ta == "XCDAB",
        "runs must not interleave; got {ta:?}"
    );
    assert_eq!(a.state_vector(), b.state_vector());
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn concurrent_insert_and_delete_converge() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    let elem = a.insert_paragraph(None, "abc").expect("base");
    exchange(&a, &mut b);
    let bid = block_id_from_op(elem);

    // a deletes "b" (offset 1, len 1); b inserts "Z" at start.
    a.delete_text(bid, 1, 1).expect("del");
    b.insert_text(bid, 0, "Z").expect("ins");

    exchange(&a, &mut b);
    exchange(&b, &mut a);

    assert_eq!(para_text(&a, 0), para_text(&b, 0));
    let t = para_text(&a, 0);
    assert!(t.contains('a') && t.contains('c') && t.contains('Z'));
    assert!(!t.contains('b'));
}

#[test]
fn multi_peer_insert_paragraph_propagates() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    a.insert_paragraph(None, "hello").expect("p");
    exchange(&a, &mut b);
    assert_eq!(para_text(&b, 0), "hello");
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn nested_paragraph_in_blockquote_converges() {
    use md_crdt::core::Sequence;
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    // Insert an empty top-level blockquote, then a paragraph inside it.
    let quote = a
        .insert_block(
            None,
            BlockKind::BlockQuote {
                children: Sequence::new(),
            },
        )
        .expect("quote");
    let child = a
        .insert_paragraph_in(Some(quote), None, "quoted")
        .expect("nested para");
    // Edit the nested paragraph's text.
    a.insert_text(block_id_from_op(child), 6, "!")
        .expect("edit");

    exchange(&a, &mut b);

    // Peer B must reconstruct the nested structure and text.
    let quote_block = &b.document().blocks_in_order()[0];
    let BlockKind::BlockQuote { children } = &quote_block.kind else {
        panic!("expected blockquote at top level");
    };
    let kids: Vec<_> = children.iter().collect();
    assert_eq!(kids.len(), 1);
    match &kids[0].kind {
        BlockKind::Paragraph { text } => {
            assert_eq!(paragraph_visible_string(text), "quoted!");
        }
        _ => panic!("expected nested paragraph"),
    }
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn nested_paragraph_in_list_item_converges() {
    use md_crdt::core::Sequence;
    use md_crdt::doc::{ListItem, block_id_from_op};
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);

    // Insert a list with one empty item (item id contiguous with the list block id).
    let item_elem = OpId {
        counter: 2,
        peer: 1,
    };
    let items = Sequence::from_ordered(vec![(
        item_elem,
        ListItem {
            id: block_id_from_op(item_elem),
            elem_id: item_elem,
            children: Sequence::new(),
        },
    )]);
    a.insert_block(
        None,
        BlockKind::List {
            ordered: false,
            items,
        },
    )
    .expect("list");
    // Insert a paragraph into the list item, then edit it.
    let para = a
        .insert_paragraph_in(Some(item_elem), None, "task")
        .expect("para in item");
    a.insert_text(block_id_from_op(para), 4, "!").expect("edit");

    exchange(&a, &mut b);

    // Peer B reconstructs list → item → paragraph "task!".
    let top = &b.document().blocks_in_order()[0];
    let BlockKind::List { items, .. } = &top.kind else {
        panic!("expected list");
    };
    let item = items.iter().next().expect("one item");
    let child = item.children.iter().next().expect("item child");
    match &child.kind {
        BlockKind::Paragraph { text } => assert_eq!(paragraph_visible_string(text), "task!"),
        _ => panic!("expected nested paragraph"),
    }
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn unit_mode_default_is_true() {
    let a = CollaborativeDocument::new(1);
    assert!(a.unit_mode());
}

#[test]
fn insert_text_invalid_offset_errors_without_clock_burn() {
    let mut a = CollaborativeDocument::new(1);
    let elem = a.insert_paragraph(None, "x").expect("p");
    let bid = block_id_from_op(elem);
    let before = a.peek_next_id().counter;
    let err = a.insert_text(bid, 5, "y").expect_err("offset");
    assert!(matches!(err, SessionError::InvalidOffset));
    assert_eq!(a.peek_next_id().counter, before);
}

#[test]
fn delete_text_empty_range_is_noop() {
    let mut a = CollaborativeDocument::new(1);
    let elem = a.insert_paragraph(None, "x").expect("p");
    let bid = block_id_from_op(elem);
    let before = a.peek_next_id().counter;
    assert!(a.delete_text(bid, 0, 0).expect("noop").is_none());
    assert_eq!(a.peek_next_id().counter, before);
}

#[test]
fn remote_insert_text_peer_mismatch_rejected() {
    let mut a = CollaborativeDocument::new(1);
    let elem = a.insert_paragraph(None, "").expect("empty p");
    let bid = block_id_from_op(elem);
    let msg_base = a.encode_changes_since(&md_crdt::core::StateVector::new());

    let mut b = CollaborativeDocument::new(2);
    b.apply_remote(msg_base, &ValidationLimits::default())
        .expect("base");

    // Max unit id matches Operation.id (peer 1, counter 11) so the N1 check passes;
    // a nested unit with a foreign peer must still trip PeerMismatch.
    let env = md_crdt::codec::Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertText {
            block_elem: elem,
            block_id: bid,
            units: vec![
                md_crdt::codec::TextUnitWire {
                    id: OpId {
                        counter: 10,
                        peer: 1,
                    },
                    after: None,
                    right_origin: None,
                    grapheme: "y".into(),
                },
                md_crdt::codec::TextUnitWire {
                    id: OpId {
                        counter: 11,
                        peer: 9, // foreign peer
                    },
                    after: Some(OpId {
                        counter: 10,
                        peer: 1,
                    }),
                    right_origin: None,
                    grapheme: "z".into(),
                },
            ],
        }),
    };
    let payload = JsonOpCodec.encode(&env).expect("enc");
    let msg = ChangeMessage {
        since: b.state_vector(),
        ops: vec![Operation {
            id: OpId {
                counter: 11,
                peer: 1,
            },
            payload: payload.into(),
        }],
    };
    let err = b
        .apply_remote(msg, &ValidationLimits::default())
        .expect_err("peer");
    assert!(matches!(err, SessionError::PeerMismatch));
}
