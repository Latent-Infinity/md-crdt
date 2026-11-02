#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt::{
    CheckpointRequest, CollaborativeDocument, DocumentDto, DocumentTombstonePolicy,
    EquivalenceMode, Parser, SessionSnapshot, StateVector, ValidationLimits, block_id_from_op,
};

const SEED: &str = include_str!("../seeds/snapshot_restore/materialized.md");
const MAX_INPUT: usize = 4096;
const MAX_EXPANSION: u64 = 2 * 1024 * 1024;

fn bounded_restore(bytes: &[u8]) {
    // The production decoder permits 512 MiB. Keep individual fuzz iterations
    // smaller without changing the production contract or ignoring invariants.
    if bytes.starts_with(b"MDSN\x01") && bytes.len() >= 13 {
        let declared = u64::from_le_bytes(bytes[5..13].try_into().unwrap());
        if declared > MAX_EXPANSION {
            return;
        }
    }
    if let Ok(restored) = CollaborativeDocument::restore_from_snapshot_bytes(bytes) {
        let snapshot = restored.save_snapshot().unwrap();
        let second =
            CollaborativeDocument::restore_from_snapshot_bytes(&snapshot.to_bytes().unwrap())
                .unwrap();
        assert_eq!(second.document(), restored.document());
        assert_eq!(second.state_vector(), restored.state_vector());
        assert_eq!(second.peek_next_id(), restored.peek_next_id());
        assert_eq!(second.save_snapshot().unwrap(), snapshot);
    }
}

fuzz_target!(|input: &[u8]| {
    let data = &input[..input.len().min(MAX_INPUT)];
    bounded_restore(data);
    // Start every iteration with an actual parsed, materialized Markdown seed,
    // including nested containers, marks, a fence, table and frontmatter.
    let mut seed = CollaborativeDocument::import_state(
        DocumentDto::from_document(&Parser::try_parse(SEED).unwrap()),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        1,
        true,
    )
    .unwrap();
    let initial = seed.save_snapshot().unwrap();
    let text = String::from_utf8_lossy(&data[..data.len().min(256)]);
    let paragraph = seed
        .insert_paragraph(None, &format!("real seed {text}"))
        .unwrap();
    let block = block_id_from_op(paragraph);
    let base = seed.save_snapshot().unwrap();
    let mut editor = CollaborativeDocument::restore_from_snapshot(base.clone()).unwrap();
    editor.rebind_peer(2);
    editor
        .insert_text(block, 0, &format!("late {text}"))
        .unwrap();
    // Receipt is contiguous for peer 2, but peer 1's parent is absent: deferred.
    let mut held = CollaborativeDocument::restore_from_snapshot(initial.clone()).unwrap();
    held.rebind_peer(3);
    held.apply_remote(
        editor.encode_changes_since(&seed.state_vector()).unwrap(),
        &ValidationLimits::default(),
    )
    .unwrap();
    assert!(!held.save_snapshot().unwrap().deferred.is_empty());
    // For the same peer, withholding InsertBlock leaves a causal pending gap.
    let mut pending = CollaborativeDocument::restore_from_snapshot(initial).unwrap();
    pending.rebind_peer(4);
    let mut gap = seed.encode_changes_since(&StateVector::new()).unwrap();
    gap.ops.retain(|operation| operation.id != paragraph);
    pending
        .apply_remote(gap, &ValidationLimits::default())
        .unwrap();
    assert!(!pending.save_snapshot().unwrap().pending.is_empty());
    for (session, expected) in [(&seed, &seed), (&held, &editor), (&pending, &seed)] {
        let snapshot = session.save_snapshot().unwrap();
        let bytes = snapshot.to_bytes().unwrap();
        let decoded = SessionSnapshot::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, snapshot);
        let mut restored = CollaborativeDocument::restore_from_snapshot(decoded).unwrap();
        assert_eq!(restored.save_snapshot().unwrap(), snapshot);
        if session.peer() != 1 {
            // Replay the creator ops over the common imported materialization.
            restored
                .apply_remote(
                    seed.encode_changes_since(&restored.state_vector()).unwrap(),
                    &ValidationLimits::default(),
                )
                .unwrap();
            let visible = restored.document().find_block_by_id(block).unwrap();
            let expected_block = expected.document().find_block_by_id(block).unwrap();
            assert_eq!(visible, expected_block);
            let recovered = restored.save_snapshot().unwrap();
            assert!(recovered.pending.is_empty());
            assert!(recovered.deferred.is_empty());
        }
        let before = restored.document().serialize(EquivalenceMode::Structural);
        restored
            .checkpoint_history(&CheckpointRequest {
                max_retained_ops: 0,
                active_peer_leases: Vec::new(),
                tombstones: DocumentTombstonePolicy::KeepAll,
            })
            .unwrap();
        let mut rebased =
            CollaborativeDocument::rebase_from_snapshot(restored.save_snapshot().unwrap(), 9)
                .unwrap();
        assert_eq!(
            rebased.document().serialize(EquivalenceMode::Structural),
            before
        );
        rebased.insert_paragraph(None, "post-rebase edit").unwrap();
        bounded_restore(&rebased.save_snapshot().unwrap().to_bytes().unwrap());
        // Mutation starts from valid encoded snapshots, not almost-always-invalid
        // random bytes. Corruptions may reject; accepted states must be stable.
        let mut mutated = bytes;
        for pair in data.chunks_exact(2).take(16) {
            let index = usize::from(pair[0]) % mutated.len();
            mutated[index] ^= pair[1];
        }
        bounded_restore(&mutated);
    }
});
