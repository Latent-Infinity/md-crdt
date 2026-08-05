//! Characterization pins for local insert/paste hot-path refactors.
//!
//! These must stay green across sequence apply optimizations: same visible text,
//! same unit OpIds, mark intervals surviving inserts at the mark boundary.

use md_crdt::EquivalenceMode;
use md_crdt::core::mark::MarkKind;
use md_crdt::doc::{
    block_id_from_op, block_text_seq, paragraph_visible_ids, paragraph_visible_string,
};
use md_crdt::session::CollaborativeDocument;
use std::collections::BTreeMap;

fn visible_unit_ids(
    session: &CollaborativeDocument,
    block_id: md_crdt::BlockId,
) -> Vec<md_crdt::OpId> {
    let block = session
        .document()
        .find_block_by_id(block_id)
        .expect("block");
    let seq = block_text_seq(&block.kind).expect("paragraph");
    paragraph_visible_ids(seq)
}

fn visible_text(session: &CollaborativeDocument, block_id: md_crdt::BlockId) -> String {
    let block = session
        .document()
        .find_block_by_id(block_id)
        .expect("block");
    let seq = block_text_seq(&block.kind).expect("paragraph");
    paragraph_visible_string(seq)
}

fn seed_paragraph(peer: u64, text: &str) -> (CollaborativeDocument, md_crdt::BlockId) {
    let mut session = CollaborativeDocument::new(peer);
    let elem = session.insert_paragraph(None, text).expect("seed");
    (session, block_id_from_op(elem))
}

/// Full document serialize + every visible unit OpId must be deterministic for a
/// fixed insert of `n` identical graphemes (control for hot-path refactors).
#[test]
fn insert_text_is_deterministic_for_document_and_opids() {
    for n in [1usize, 1_000] {
        let seed = "x".repeat(32);
        let insert = "y".repeat(n);

        let (mut a, block_id) = seed_paragraph(1, &seed);
        let before_ids = visible_unit_ids(&a, block_id);
        a.insert_text(block_id, 16, &insert).expect("insert");
        let after_text = visible_text(&a, block_id);
        let after_ids = visible_unit_ids(&a, block_id);
        let after_doc = a.document().serialize(EquivalenceMode::Structural);

        // Re-run from the same seed: same text, OpIds, and structural serialize.
        let (mut b, block_b) = seed_paragraph(1, &seed);
        b.insert_text(block_b, 16, &insert).expect("insert");
        assert_eq!(visible_text(&b, block_b), after_text);
        assert_eq!(visible_unit_ids(&b, block_b), after_ids);
        assert_eq!(
            b.document().serialize(EquivalenceMode::Structural),
            after_doc
        );

        // Inserted unit ids form a contiguous counter run for this peer.
        let inserted: Vec<_> = after_ids
            .iter()
            .filter(|id| !before_ids.contains(id))
            .copied()
            .collect();
        assert_eq!(inserted.len(), n);
        for window in inserted.windows(2) {
            assert_eq!(window[0].peer, 1);
            assert_eq!(window[1].peer, 1);
            assert_eq!(window[1].counter, window[0].counter + 1);
        }
        assert_eq!(
            after_text,
            format!("{}{}{}", &seed[..16], insert, &seed[16..])
        );
    }
}

/// One multi-grapheme paste must match r sequential one-grapheme inserts.
#[test]
fn paste_matches_sequential_inserts() {
    let r = 4_000usize;
    let seed = "x".repeat(64);
    let paste: String = (0..r).map(|i| char::from(b'a' + (i % 26) as u8)).collect();

    let (mut batched, block_b) = seed_paragraph(2, &seed);
    batched
        .insert_text(block_b, 32, &paste)
        .expect("paste once");
    let batched_text = visible_text(&batched, block_b);
    let batched_ids = visible_unit_ids(&batched, block_b);
    let batched_doc = batched.document().serialize(EquivalenceMode::Structural);

    let (mut sequential, block_s) = seed_paragraph(2, &seed);
    for (i, ch) in paste.chars().enumerate() {
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        sequential
            .insert_text(block_s, 32 + i, s)
            .expect("seq insert");
    }
    assert_eq!(visible_text(&sequential, block_s), batched_text);
    assert_eq!(visible_unit_ids(&sequential, block_s), batched_ids);
    assert_eq!(
        sequential.document().serialize(EquivalenceMode::Structural),
        batched_doc
    );
    assert_eq!(
        batched_text,
        format!("{}{}{}", &seed[..32], paste, &seed[32..])
    );
}

/// A mark spanning the insertion point keeps its interval identity and still
/// covers the intended range after a multi-unit insert at the mark start.
#[test]
fn marks_survive_batched_insert() {
    let (mut session, block_id) = seed_paragraph(3, "abcdefghij");
    // Mark graphemes [2, 8) = "cdefgh"
    let mark_id = session
        .set_mark(block_id, 2..8, MarkKind::Bold, BTreeMap::new())
        .expect("set_mark");

    session
        .insert_text(block_id, 2, "XYZ")
        .expect("insert at mark start");

    let block = session
        .document()
        .find_block_by_id(block_id)
        .expect("block");
    let interval = block
        .marks
        .interval(&mark_id)
        .expect("mark interval retained");
    assert_eq!(interval.kind, MarkKind::Bold);
    // Visible text: ab + XYZ + cdefghij
    assert_eq!(visible_text(&session, block_id), "abXYZcdefghij");

    // The insertion precedes the mark's Before-biased start anchor, so the
    // original "cdefgh" range shifts right by three and remains exact.
    let spans = session
        .document()
        .render_paragraph_spans(block_id)
        .expect("spans");
    assert!(
        spans
            .iter()
            .any(|span| span.start == 5 && span.end == 11 && span.marks == [mark_id]),
        "bold interval should remain exactly on cdefgh, got {spans:?}"
    );
}
