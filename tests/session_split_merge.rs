use md_crdt::codec::{DocOp, JsonOpCodec, OpBody, OpCodec};
use md_crdt::core::mark::{MarkInterval, MarkKind, MarkValue};
use md_crdt::doc::{
    BlockId, BlockKind, EquivalenceMode, block_id_from_op, block_text_seq, paragraph_visible_ids,
    paragraph_visible_string,
};
use md_crdt::session::{CollaborativeDocument, SessionError, SessionSnapshot};
use md_crdt::sync::{ChangeMessage, Operation, ValidationLimits};
use std::collections::BTreeMap;

fn exchange(from: &CollaborativeDocument, to: &mut CollaborativeDocument) {
    let message = from.encode_changes_since(&to.state_vector()).unwrap();
    to.apply_remote(message, &ValidationLimits::default())
        .expect("apply remote changes");
}

fn paragraph(doc: &CollaborativeDocument, block_id: BlockId) -> (String, Vec<md_crdt::OpId>) {
    let block = doc
        .document()
        .find_block_by_id(block_id)
        .expect("paragraph block");
    let text = match &block.kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => text,
        _ => panic!("expected text-bearing block"),
    };
    (paragraph_visible_string(text), paragraph_visible_ids(text))
}

#[test]
fn split_block_preserves_suffix_unit_ids_and_converges() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);
    let first_elem = a.insert_paragraph(None, "alpha").expect("paragraph");
    exchange(&a, &mut b);

    let first_id = block_id_from_op(first_elem);
    let original_ids = paragraph(&a, first_id).1;
    let second_elem = a.split_block(first_id, 2).expect("split");
    let second_id = block_id_from_op(second_elem);

    assert_eq!(
        paragraph(&a, first_id),
        ("al".into(), original_ids[..2].to_vec())
    );
    assert_eq!(
        paragraph(&a, second_id),
        ("pha".into(), original_ids[2..].to_vec())
    );

    let message = a.encode_changes_since(&b.state_vector()).unwrap();
    let envelope = JsonOpCodec
        .decode(&message.ops.last().expect("split op").payload)
        .expect("decode split");
    assert!(matches!(
        envelope.body,
        OpBody::Doc(DocOp::SplitBlock { ref units, .. }) if units.len() == 3
    ));

    b.apply_remote(message, &ValidationLimits::default())
        .expect("apply split");
    assert_eq!(paragraph(&b, first_id), paragraph(&a, first_id));
    assert_eq!(paragraph(&b, second_id), paragraph(&a, second_id));
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn merge_blocks_preserves_right_unit_ids_and_converges() {
    let mut a = CollaborativeDocument::new(3);
    let mut b = CollaborativeDocument::new(4);
    let left_elem = a.insert_paragraph(None, "ab").expect("left");
    let right_elem = a.insert_paragraph(Some(left_elem), "cd").expect("right");
    exchange(&a, &mut b);

    let left_id = block_id_from_op(left_elem);
    let right_id = block_id_from_op(right_elem);
    let mut expected_ids = paragraph(&a, left_id).1;
    expected_ids.extend(paragraph(&a, right_id).1);

    a.merge_blocks(left_id, right_id).expect("merge");
    assert_eq!(paragraph(&a, left_id), ("abcd".into(), expected_ids));
    assert!(a.document().find_block_by_id(right_id).is_none());

    exchange(&a, &mut b);
    assert_eq!(paragraph(&b, left_id), paragraph(&a, left_id));
    assert!(b.document().find_block_by_id(right_id).is_none());
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn split_validation_does_not_advance_clock() {
    let mut doc = CollaborativeDocument::new(5);
    let elem = doc.insert_paragraph(None, "x").expect("paragraph");
    let before = doc.peek_next_id();

    let error = doc
        .split_block(block_id_from_op(elem), 2)
        .expect_err("invalid offset");
    assert!(matches!(error, SessionError::InvalidOffset));
    assert_eq!(doc.peek_next_id(), before);
}

#[test]
fn merge_requires_adjacent_text_blocks_without_clock_burn() {
    let mut doc = CollaborativeDocument::new(6);
    let left = doc.insert_paragraph(None, "a").expect("left");
    let middle = doc.insert_paragraph(Some(left), "b").expect("middle");
    let right = doc.insert_paragraph(Some(middle), "c").expect("right");
    let before = doc.peek_next_id();

    let error = doc
        .merge_blocks(block_id_from_op(left), block_id_from_op(right))
        .expect_err("non-adjacent merge");
    assert!(matches!(error, SessionError::BlocksNotAdjacent));
    assert_eq!(doc.peek_next_id(), before);
}

#[test]
fn split_then_merge_reallocates_only_colliding_unit_ids() {
    let mut a = CollaborativeDocument::new(7);
    let mut b = CollaborativeDocument::new(8);
    let left_elem = a.insert_paragraph(None, "abcd").expect("paragraph");
    exchange(&a, &mut b);
    let left_id = block_id_from_op(left_elem);
    let original_ids = paragraph(&a, left_id).1;

    let right_elem = a.split_block(left_id, 2).expect("split");
    let right_id = block_id_from_op(right_elem);
    a.merge_blocks(left_id, right_id).expect("merge");

    let (text, merged_ids) = paragraph(&a, left_id);
    assert_eq!(text, "abcd");
    assert_eq!(&merged_ids[..2], &original_ids[..2]);
    assert_ne!(&merged_ids[2..], &original_ids[2..]);
    exchange(&a, &mut b);
    assert_eq!(paragraph(&b, left_id), paragraph(&a, left_id));
    assert_eq!(a.state_vector(), b.state_vector());
}

#[test]
fn nested_heading_can_split_and_merge() {
    use md_crdt::Sequence;

    let mut doc = CollaborativeDocument::new(9);
    let quote = doc
        .insert_block(
            None,
            BlockKind::BlockQuote {
                children: Sequence::new(),
            },
        )
        .expect("quote");
    let heading = doc
        .insert_block_in(
            Some(quote),
            None,
            BlockKind::Heading {
                level: 3,
                text: Sequence::new(),
            },
        )
        .expect("heading");
    let heading_id = block_id_from_op(heading);
    doc.insert_text(heading_id, 0, "title")
        .expect("heading text");

    let suffix = doc
        .split_block_in(Some(quote), heading_id, 2)
        .expect("nested split");
    let suffix_id = block_id_from_op(suffix);
    let suffix_block = doc
        .document()
        .find_block_by_id(suffix_id)
        .expect("suffix heading");
    assert!(matches!(
        suffix_block.kind,
        BlockKind::Heading { level: 3, .. }
    ));
    doc.merge_blocks_in(Some(quote), heading_id, suffix_id)
        .expect("nested merge");
    assert_eq!(paragraph(&doc, heading_id).0, "title");
}

#[test]
fn non_text_blocks_are_rejected_without_clock_burn() {
    use md_crdt::Sequence;

    let mut doc = CollaborativeDocument::new(10);
    let quote = doc
        .insert_block(
            None,
            BlockKind::BlockQuote {
                children: Sequence::new(),
            },
        )
        .expect("quote");
    let paragraph = doc
        .insert_paragraph(Some(quote), "text")
        .expect("paragraph");
    let before = doc.peek_next_id();

    assert!(matches!(
        doc.split_block(block_id_from_op(quote), 0),
        Err(SessionError::NotParagraph)
    ));
    assert!(matches!(
        doc.merge_blocks(block_id_from_op(quote), block_id_from_op(paragraph)),
        Err(SessionError::NotParagraph)
    ));
    assert_eq!(doc.peek_next_id(), before);
}

#[test]
fn remote_merge_rejects_foreign_replacement_ids() {
    let mut a = CollaborativeDocument::new(11);
    let mut b = CollaborativeDocument::new(12);
    let left = a.insert_paragraph(None, "ab").expect("paragraph");
    let left_id = block_id_from_op(left);
    let right = a.split_block(left_id, 1).expect("split");
    exchange(&a, &mut b);

    a.merge_blocks(left_id, block_id_from_op(right))
        .expect("merge");
    let mut message = a.encode_changes_since(&b.state_vector()).unwrap();
    let operation = message.ops.last_mut().expect("merge operation");
    let mut envelope = JsonOpCodec
        .decode(&operation.payload)
        .expect("decode merge");
    let OpBody::Doc(DocOp::MergeBlocks { id, units, .. }) = &mut envelope.body else {
        panic!("expected merge operation");
    };
    for unit in units {
        if unit.id != unit.source_id {
            unit.id.peer = 99;
        }
    }
    operation.id = *id;
    operation.payload = JsonOpCodec.encode(&envelope).expect("encode merge").into();

    let error = b
        .apply_remote(
            ChangeMessage {
                since: message.since,
                ops: vec![Operation {
                    id: operation.id,
                    payload: operation.payload.clone(),
                }],
            },
            &ValidationLimits::default(),
        )
        .expect_err("foreign replacement id");
    assert!(matches!(error, SessionError::PeerMismatch));
}

#[test]
fn concurrent_splits_of_same_block_converge() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);
    let elem = a.insert_paragraph(None, "alpha").expect("paragraph");
    exchange(&a, &mut b);
    let id = block_id_from_op(elem);

    // Concurrent splits of the same block at different offsets. Each split tombstones
    // an overlapping suffix in the original and re-homes those unit ids into a fresh
    // peer-local block, so the RGA must converge on both peers.
    a.split_block(id, 2).expect("split a"); // "al" | "pha"
    b.split_block(id, 4).expect("split b"); // "alph" | "a"

    let from_a = a.encode_changes_since(&b.state_vector()).unwrap();
    let from_b = b.encode_changes_since(&a.state_vector()).unwrap();
    b.apply_remote(from_a, &ValidationLimits::default())
        .expect("b applies a");
    a.apply_remote(from_b, &ValidationLimits::default())
        .expect("a applies b");

    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural),
        "concurrent splits must converge"
    );
    assert_eq!(a.state_vector(), b.state_vector());
    // The original block keeps only the common prefix; both suffixes survive as siblings.
    assert_eq!(paragraph(&a, id).0, "al");
}

fn restore(session: &CollaborativeDocument) -> CollaborativeDocument {
    let bytes = session.save_snapshot().unwrap().to_bytes().unwrap();
    CollaborativeDocument::restore_from_snapshot(SessionSnapshot::from_bytes(&bytes).unwrap())
        .unwrap()
}

fn assert_merged_interval(
    session: &CollaborativeDocument,
    block_id: BlockId,
    original: &MarkInterval,
    range: std::ops::Range<usize>,
    active: bool,
) {
    let block = session.document().find_block_by_id(block_id).unwrap();
    let text = block_text_seq(&block.kind).unwrap();
    let ids = paragraph_visible_ids(text);
    let mut expected = original.clone();
    expected.start.elem_id = ids[range.start];
    expected.end.elem_id = ids[range.end - 1];
    assert_eq!(block.marks.interval(&original.id), Some(&expected));
    assert_eq!(block.marks.is_active(&original.id), active);
    let resolved: Vec<_> = block
        .marks
        .resolved_intervals_in_sequence(text)
        .into_iter()
        .filter(|(interval, _, _)| interval.id == original.id)
        .map(|(_, start, end)| (start, end))
        .collect();
    let expected_ranges = if active {
        vec![(range.start, range.end)]
    } else {
        Vec::new()
    };
    assert_eq!(resolved, expected_ranges);
}

#[test]
fn split_merge_preserves_suffix_crossing_and_retained_mark_ranges() {
    let link_attrs = BTreeMap::from([
        ("href".into(), MarkValue::String("target".into())),
        ("title".into(), MarkValue::String("kept".into())),
        ("custom".into(), MarkValue::Bool(true)),
    ]);
    for (kind, range, attrs, markdown) in [
        (MarkKind::Bold, 1..3, BTreeMap::new(), "a**bc**"),
        (MarkKind::Link, 1..3, link_attrs, "a[bc](target)"),
        (MarkKind::Bold, 0..2, BTreeMap::new(), "**ab**c"),
        (MarkKind::Bold, 0..1, BTreeMap::new(), "**a**bc"),
    ] {
        let mut local = CollaborativeDocument::new(21);
        let mut remote = CollaborativeDocument::new(22);
        let mut late_replay = CollaborativeDocument::new(23);
        let elem = local.insert_paragraph(None, "abc").unwrap();
        let block_id = block_id_from_op(elem);
        let mark_id = local
            .set_mark(block_id, range.clone(), kind, attrs)
            .unwrap();
        let original = local
            .document()
            .find_block_by_id(block_id)
            .unwrap()
            .marks
            .interval(&mark_id)
            .unwrap()
            .clone();
        assert_eq!(
            local.document().serialize(EquivalenceMode::Structural),
            markdown
        );
        exchange(&local, &mut remote);
        let suffix = local.split_block(block_id, 1).unwrap();
        exchange(&local, &mut remote);
        let mut restored_split = restore(&local);

        local
            .merge_blocks(block_id, block_id_from_op(suffix))
            .unwrap();
        restored_split
            .merge_blocks(block_id, block_id_from_op(suffix))
            .unwrap();
        exchange(&local, &mut remote);
        exchange(&local, &mut late_replay);
        let restored_merged = restore(&local);

        for session in [
            &local,
            &remote,
            &late_replay,
            &restored_split,
            &restored_merged,
        ] {
            assert_eq!(
                session.document().serialize(EquivalenceMode::Structural),
                markdown
            );
            assert_eq!(paragraph(session, block_id), paragraph(&local, block_id));
            assert_merged_interval(session, block_id, &original, range.clone(), true);
            assert_eq!(session.state_vector(), local.state_vector());
        }
    }
}

#[test]
fn split_merge_keeps_right_only_marks_and_removal_history() {
    let mut local = CollaborativeDocument::new(24);
    let mut remote = CollaborativeDocument::new(25);
    let elem = local.insert_paragraph(None, "abc").unwrap();
    let block_id = block_id_from_op(elem);
    let removed_id = local
        .set_mark(block_id, 1..3, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    let removed_interval = local
        .document()
        .find_block_by_id(block_id)
        .unwrap()
        .marks
        .interval(&removed_id)
        .unwrap()
        .clone();
    let suffix = local.split_block(block_id, 1).unwrap();
    let suffix_id = block_id_from_op(suffix);
    local.remove_mark(suffix_id, removed_id).unwrap();
    let link_id = local
        .set_mark(
            suffix_id,
            0..2,
            MarkKind::Link,
            BTreeMap::from([
                ("href".into(), MarkValue::String("target".into())),
                ("custom".into(), MarkValue::Bool(false)),
            ]),
        )
        .unwrap();
    let link_interval = local
        .document()
        .find_block_by_id(suffix_id)
        .unwrap()
        .marks
        .interval(&link_id)
        .unwrap()
        .clone();
    assert!(
        local
            .document()
            .find_block_by_id(block_id)
            .unwrap()
            .marks
            .is_active(&removed_id)
    );
    exchange(&local, &mut remote);
    let mut restored_split = restore(&local);
    local.merge_blocks(block_id, suffix_id).unwrap();
    restored_split.merge_blocks(block_id, suffix_id).unwrap();
    exchange(&local, &mut remote);
    let restored_merged = restore(&local);

    for session in [&local, &remote, &restored_split, &restored_merged] {
        assert_eq!(
            session.document().serialize(EquivalenceMode::Structural),
            "a[bc](target)"
        );
        assert_merged_interval(session, block_id, &removed_interval, 1..3, false);
        assert_merged_interval(session, block_id, &link_interval, 1..3, true);
        assert_eq!(paragraph(session, block_id), paragraph(&local, block_id));
        assert_eq!(session.state_vector(), local.state_vector());
    }
}
