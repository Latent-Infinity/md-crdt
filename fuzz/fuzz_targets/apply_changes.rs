#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt::core::{OpId, StateVector};
use md_crdt::sync::{ChangeMessage, Operation, SyncState};

fuzz_target!(|data: &[u8]| {
    let mut ops = Vec::new();
    for chunk in data.chunks(10) {
        if chunk.len() < 2 {
            break;
        }
        let counter = if chunk[1] == 0 { 1 } else { chunk[1] } as u64;
        let op = Operation {
            id: OpId {
                counter,
                peer: chunk[0] as u64,
            },
            payload: chunk[2..].to_vec().into(),
        };
        ops.push(op);
    }
    let message = ChangeMessage {
        since: StateVector::new(),
        ops,
    };
    let mut doc = SyncState::new();
    let _ = doc.apply_changes(message.clone());
    // Decode and apply through the actual document/session boundary as well.
    let mut session = md_crdt::CollaborativeDocument::new(255);
    let _ = session.apply_remote(message, &md_crdt::ValidationLimits::default());

    // Generate valid cross-peer edits, then deliver their dependencies late.
    // Arbitrary opaque payloads above almost always stop at decoding.
    let mut author = md_crdt::CollaborativeDocument::new(1);
    let block = author.insert_paragraph(None, "").unwrap();
    let mut editor = md_crdt::CollaborativeDocument::new(2);
    let limits = md_crdt::ValidationLimits::default();
    editor
        .apply_remote(
            author.encode_changes_since(&editor.state_vector()).unwrap(),
            &limits,
        )
        .unwrap();
    let text = String::from_utf8_lossy(&data[..data.len().min(256)]);
    if !text.is_empty() {
        editor
            .insert_text(md_crdt::block_id_from_op(block), 0, &text)
            .unwrap();
    }
    let delta = editor.encode_changes_since(&author.state_vector()).unwrap();
    let mut delayed = md_crdt::CollaborativeDocument::new(3);
    delayed.apply_remote(delta, &limits).unwrap();
    delayed
        .apply_remote(
            author
                .encode_changes_since(&delayed.state_vector())
                .unwrap(),
            &limits,
        )
        .unwrap();
    assert_eq!(
        editor
            .document()
            .serialize(md_crdt::EquivalenceMode::Structural),
        delayed
            .document()
            .serialize(md_crdt::EquivalenceMode::Structural),
    );
});
