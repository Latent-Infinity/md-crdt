//! Characterization pins for multi-op amortization (batch insert + bulk apply).

use md_crdt::EquivalenceMode;
use md_crdt::core::StateVector;
use md_crdt::doc::{
    block_id_from_op, block_text_seq, paragraph_visible_ids, paragraph_visible_string,
};
use md_crdt::session::CollaborativeDocument;
use md_crdt::sync::ValidationLimits;
use unicode_segmentation::UnicodeSegmentation;

fn visible_ids(session: &CollaborativeDocument, block_id: md_crdt::BlockId) -> Vec<md_crdt::OpId> {
    let block = session
        .document()
        .find_block_by_id(block_id)
        .expect("block");
    paragraph_visible_ids(block_text_seq(&block.kind).expect("paragraph"))
}

fn visible_text(session: &CollaborativeDocument, block_id: md_crdt::BlockId) -> String {
    let block = session
        .document()
        .find_block_by_id(block_id)
        .expect("block");
    paragraph_visible_string(block_text_seq(&block.kind).expect("paragraph"))
}

#[test]
fn batch_of_one_matches_insert_text() {
    let mut a = CollaborativeDocument::new(1);
    let elem = a.insert_paragraph(None, "seed").unwrap();
    let bid = block_id_from_op(elem);
    a.insert_text(bid, 4, "X").unwrap();

    let mut b = CollaborativeDocument::new(1);
    let elem = b.insert_paragraph(None, "seed").unwrap();
    let bid = block_id_from_op(elem);
    b.insert_text_batch(bid, 4, ["X"]).unwrap();

    assert_eq!(visible_text(&a, bid), visible_text(&b, bid));
    assert_eq!(visible_ids(&a, bid), visible_ids(&b, bid));
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn batch_insert_equals_sequential_inserts() {
    let m = 64usize;
    let seed = "base";
    let keystrokes: Vec<String> = (0..m)
        .map(|i| ((b'a' + (i % 26) as u8) as char).to_string())
        .collect();
    let parts: Vec<&str> = keystrokes.iter().map(String::as_str).collect();

    let mut sequential = CollaborativeDocument::new(2);
    let elem = sequential.insert_paragraph(None, seed).unwrap();
    let bid = block_id_from_op(elem);
    let mut offset = seed.chars().count();
    for part in &parts {
        sequential.insert_text(bid, offset, part).unwrap();
        offset += part.chars().count();
    }

    let mut batched = CollaborativeDocument::new(2);
    let elem = batched.insert_paragraph(None, seed).unwrap();
    let bid_b = block_id_from_op(elem);
    batched
        .insert_text_batch(bid_b, seed.chars().count(), parts)
        .unwrap();

    assert_eq!(
        visible_text(&sequential, bid),
        visible_text(&batched, bid_b)
    );
    assert_eq!(visible_ids(&sequential, bid), visible_ids(&batched, bid_b));
    assert_eq!(
        sequential.document().serialize(EquivalenceMode::Structural),
        batched.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn batch_insert_matches_sequential_grapheme_offsets_with_empty_parts() {
    let parts = ["e\u{301}", "", "👨‍👩‍👧‍👦", "界"];

    let mut sequential = CollaborativeDocument::new(4);
    let elem = sequential.insert_paragraph(None, "→").unwrap();
    let bid = block_id_from_op(elem);
    let mut offset = 1;
    for part in parts {
        sequential.insert_text(bid, offset, part).unwrap();
        offset += part.graphemes(true).count();
    }

    let mut batched = CollaborativeDocument::new(4);
    let elem = batched.insert_paragraph(None, "→").unwrap();
    let batched_bid = block_id_from_op(elem);
    batched.insert_text_batch(batched_bid, 1, parts).unwrap();

    assert_eq!(visible_text(&sequential, bid), "→e\u{301}👨‍👩‍👧‍👦界");
    assert_eq!(
        visible_text(&sequential, bid),
        visible_text(&batched, batched_bid)
    );
    assert_eq!(
        visible_ids(&sequential, bid),
        visible_ids(&batched, batched_bid)
    );
    assert_eq!(sequential.state_vector(), batched.state_vector());
}

#[test]
fn batch_insert_is_atomic_on_failure() {
    let mut session = CollaborativeDocument::new(3);
    let elem = session.insert_paragraph(None, "hi").unwrap();
    let bid = block_id_from_op(elem);
    let before = session.document().serialize(EquivalenceMode::Structural);
    let before_ids = visible_ids(&session, bid);
    let before_counter = session.state_vector().get(3).unwrap_or(0);

    let err = session
        .insert_text_batch(bid, 99, ["a", "b", "c"])
        .expect_err("offset past end");
    assert!(matches!(err, md_crdt::SessionError::InvalidOffset));
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        before
    );
    assert_eq!(visible_ids(&session, bid), before_ids);
    assert_eq!(session.state_vector().get(3).unwrap_or(0), before_counter);
}

#[test]
fn bulk_apply_remote_matches_sequential_and_buffered_paths() {
    // Peer 1 builds k lag keystrokes; peer 2 applies as one message vs one-by-one.
    let k = 100usize;
    let mut source = CollaborativeDocument::new(1);
    let elem = source.insert_paragraph(None, &"x".repeat(32)).unwrap();
    let bid = block_id_from_op(elem);
    let base_sv = source.state_vector();
    for i in 0..k {
        source.insert_text(bid, 32 + i, "z").expect("keystroke");
    }
    let delta = source.encode_changes_since(&base_sv).expect("encode");
    assert_eq!(delta.ops.len(), k);

    // Full history for empty peer (bulk).
    let full = source
        .encode_changes_since(&StateVector::default())
        .expect("full");

    let mut bulk = CollaborativeDocument::new(2);
    let bulk_result = bulk
        .apply_remote(full.clone(), &ValidationLimits::default())
        .expect("bulk apply");
    assert!(bulk_result.buffered.is_empty());

    // Reverse delivery forces the buffered fallback path, providing an oracle
    // independent of the clean in-order loop exercised above.
    let mut reversed_message = full.clone();
    reversed_message.ops.reverse();
    let mut buffered = CollaborativeDocument::new(3);
    let buffered_result = buffered
        .apply_remote(reversed_message, &ValidationLimits::default())
        .expect("buffered apply");
    assert!(buffered_result.buffered.is_empty());
    assert_eq!(
        bulk.document().serialize(EquivalenceMode::Structural),
        buffered.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(bulk.state_vector(), buffered.state_vector());

    let mut sequential = CollaborativeDocument::new(2);
    // Apply seed paragraph ops then each lag op one message at a time.
    let mut cursor = StateVector::default();
    for op in full.ops {
        let msg = md_crdt::ChangeMessage {
            since: cursor.clone(),
            ops: vec![op],
        };
        sequential
            .apply_remote(msg, &ValidationLimits::default())
            .expect("seq apply");
        // Advance cursor to include this op.
        cursor = sequential.state_vector();
    }

    assert_eq!(
        bulk.document().serialize(EquivalenceMode::Structural),
        sequential.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(bulk.state_vector(), sequential.state_vector());
    // Interleaved peers: empty receiver applying concurrent markers from two peers.
    let mut p1 = CollaborativeDocument::new(10);
    let e1 = p1.insert_paragraph(None, "aa").unwrap();
    let b1 = block_id_from_op(e1);
    p1.insert_text(b1, 2, "1").unwrap();
    let mut p2 = CollaborativeDocument::new(11);
    p2.apply_remote(
        p1.encode_changes_since(&StateVector::default()).unwrap(),
        &ValidationLimits::default(),
    )
    .unwrap();
    let b2 = block_id_from_op(
        p2.document()
            .blocks_in_order()
            .first()
            .expect("block")
            .elem_id,
    );
    p2.insert_text(b2, 2, "2").unwrap();
    p1.insert_text(b1, 2, "3").unwrap();

    let mut fan_bulk = CollaborativeDocument::new(12);
    let m1 = p1.encode_changes_since(&StateVector::default()).unwrap();
    let m2 = p2.encode_changes_since(&StateVector::default()).unwrap();
    // One combined message (both peers' ops) vs two sequential messages.
    let mut combined = m1.clone();
    combined.ops.extend(m2.ops.clone());
    fan_bulk
        .apply_remote(combined, &ValidationLimits::default())
        .unwrap();

    let mut fan_seq = CollaborativeDocument::new(12);
    fan_seq
        .apply_remote(m1, &ValidationLimits::default())
        .unwrap();
    fan_seq
        .apply_remote(m2, &ValidationLimits::default())
        .unwrap();

    assert_eq!(
        fan_bulk.document().serialize(EquivalenceMode::Structural),
        fan_seq.document().serialize(EquivalenceMode::Structural)
    );
}
