//! Checkpoint compaction regression coverage for retained session history.

use md_crdt::doc::{block_text_seq, paragraph_visible_string};
use md_crdt::session::CollaborativeDocument;
use md_crdt::{CheckpointRequest, DocumentTombstonePolicy, EquivalenceMode, block_id_from_op};

#[test]
fn checkpoint_reduces_retained_op_payloads_without_changing_visible_text() {
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, "hello").unwrap();
    let bid = block_id_from_op(elem);
    for i in 0..64 {
        session.insert_text(bid, 5 + i, "z").expect("keystroke");
    }
    let text_before = {
        let block = session.document().find_block_by_id(bid).unwrap();
        paragraph_visible_string(block_text_seq(&block.kind).unwrap())
    };
    let structural_before = session.document().serialize(EquivalenceMode::Structural);
    let bytes_before = session.save_snapshot().unwrap().to_bytes().unwrap().len();

    let report = session
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 8,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .expect("checkpoint");

    let text_after = {
        let block = session.document().find_block_by_id(bid).unwrap();
        paragraph_visible_string(block_text_seq(&block.kind).unwrap())
    };
    let structural_after = session.document().serialize(EquivalenceMode::Structural);
    let bytes_after = session.save_snapshot().unwrap().to_bytes().unwrap().len();

    assert_eq!(text_before, text_after);
    assert_eq!(structural_before, structural_after);
    assert!(report.pruned_ops > 0);
    assert_eq!(report.retained_ops, 8);
    assert!(
        bytes_after < bytes_before,
        "checkpoint should shrink snapshot: before={bytes_before} after={bytes_after}"
    );
}
