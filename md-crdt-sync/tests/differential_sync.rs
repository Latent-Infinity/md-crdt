//! Differential tests for sync protocol
//!
//! These tests verify that the real sync implementation produces the same
//! results as the naive oracle when operations are applied in different orders.

use md_crdt_core::OpId;
use md_crdt_naive_oracle::SyncOracle;
use md_crdt_sync::{ChangeMessage, Document, Operation};
use proptest::prelude::*;

/// Generate a valid sequence of operations for a single peer
fn gen_peer_ops(peer: u64, count: usize) -> Vec<Operation> {
    (1..=count as u64)
        .map(|counter| Operation {
            id: OpId { counter, peer },
            payload: vec![peer as u8, counter as u8],
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Test that two documents converge to the same state when they
    /// receive the same operations in different orders
    #[test]
    fn test_sync_convergence(
        peer1_ops in 1usize..5,
        peer2_ops in 1usize..5,
    ) {
        // Generate operations from two peers
        let ops1 = gen_peer_ops(1, peer1_ops);
        let ops2 = gen_peer_ops(2, peer2_ops);

        // Create real documents
        let mut doc_a = Document::new();
        let mut doc_b = Document::new();

        // Create oracle documents
        let mut oracle_a = SyncOracle::new();
        let mut oracle_b = SyncOracle::new();

        // Document A applies peer1 ops first, then peer2
        for op in &ops1 {
            doc_a.apply_changes(ChangeMessage {
                since: doc_a.state_vector(),
                ops: vec![op.clone()],
            });
            oracle_a.apply(op.id, op.payload.clone());
        }
        for op in &ops2 {
            doc_a.apply_changes(ChangeMessage {
                since: doc_a.state_vector(),
                ops: vec![op.clone()],
            });
            oracle_a.apply(op.id, op.payload.clone());
        }

        // Document B applies peer2 ops first, then peer1
        for op in &ops2 {
            doc_b.apply_changes(ChangeMessage {
                since: doc_b.state_vector(),
                ops: vec![op.clone()],
            });
            oracle_b.apply(op.id, op.payload.clone());
        }
        for op in &ops1 {
            doc_b.apply_changes(ChangeMessage {
                since: doc_b.state_vector(),
                ops: vec![op.clone()],
            });
            oracle_b.apply(op.id, op.payload.clone());
        }

        // Both real documents should have the same state vector
        prop_assert_eq!(
            doc_a.state_vector(),
            doc_b.state_vector(),
            "Real documents should converge to same state"
        );

        // Both oracles should have the same state
        prop_assert!(
            oracle_a.same_state(&oracle_b),
            "Oracle documents should converge to same state"
        );

        // Real and oracle should match
        prop_assert_eq!(
            doc_a.state_vector(),
            oracle_a.state_vector(),
            "Real document A should match oracle A"
        );
        prop_assert_eq!(
            doc_b.state_vector(),
            oracle_b.state_vector(),
            "Real document B should match oracle B"
        );
    }

    /// Test that encode_changes_since produces correct delta
    #[test]
    fn test_sync_delta_encoding(
        peer1_ops in 1usize..5,
        peer2_ops in 1usize..5,
    ) {
        let ops1 = gen_peer_ops(1, peer1_ops);
        let ops2 = gen_peer_ops(2, peer2_ops);

        // Create document with all ops
        let mut doc = Document::new();
        let mut oracle = SyncOracle::new();

        for op in ops1.iter().chain(ops2.iter()) {
            doc.apply_changes(ChangeMessage {
                since: doc.state_vector(),
                ops: vec![op.clone()],
            });
            oracle.apply(op.id, op.payload.clone());
        }

        // Create partial state vector (only peer1's first op)
        let mut partial_sv = md_crdt_core::StateVector::new();
        partial_sv.set(1, 1);

        // Get changes since partial state
        let real_changes = doc.encode_changes_since(&partial_sv);
        let oracle_changes = oracle.changes_since(&partial_sv);

        // Should have same number of changes
        prop_assert_eq!(
            real_changes.ops.len(),
            oracle_changes.len(),
            "Real and oracle should produce same number of changes"
        );

        // All ops from oracle should be in real changes
        for (op_id, _payload) in &oracle_changes {
            prop_assert!(
                real_changes.ops.iter().any(|op| op.id == *op_id),
                "Oracle op {:?} should be in real changes", op_id
            );
        }
    }

    /// Test that operations are never dropped when delivered out of order
    #[test]
    fn test_sync_no_op_loss(
        peer_ops in 1usize..5,
    ) {
        let ops = gen_peer_ops(1, peer_ops);

        // Deliver ops in reverse order (worst case for causal buffering)
        let mut doc = Document::new();
        let mut oracle = SyncOracle::new();

        for op in ops.iter().rev() {
            doc.apply_changes(ChangeMessage {
                since: md_crdt_core::StateVector::new(),
                ops: vec![op.clone()],
            });
            oracle.apply(op.id, op.payload.clone());
        }

        // State vectors should match
        prop_assert_eq!(
            doc.state_vector(),
            oracle.state_vector(),
            "Document should have all ops even when delivered out of order"
        );

        // Should have applied all ops
        prop_assert_eq!(
            doc.state_vector().get(1),
            Some(peer_ops as u64),
            "Should have counter = {} for peer 1", peer_ops
        );
    }
}

#[test]
fn test_two_peer_sync_simulation() {
    // Create two peers with their own documents
    let mut peer1_doc = Document::new();
    let mut peer2_doc = Document::new();

    // Peer 1 generates operations
    let peer1_op1 = Operation {
        id: OpId {
            counter: 1,
            peer: 1,
        },
        payload: vec![1, 1],
    };
    let peer1_op2 = Operation {
        id: OpId {
            counter: 2,
            peer: 1,
        },
        payload: vec![1, 2],
    };

    // Peer 2 generates operations
    let peer2_op1 = Operation {
        id: OpId {
            counter: 1,
            peer: 2,
        },
        payload: vec![2, 1],
    };

    // Peer 1 applies its own operations
    peer1_doc.add_local_op(peer1_op1.clone());
    peer1_doc.add_local_op(peer1_op2.clone());

    // Peer 2 applies its own operation
    peer2_doc.add_local_op(peer2_op1.clone());

    // Simulate sync: peer 1 sends to peer 2
    let peer1_outbox = peer1_doc.outbox();
    let message_to_peer2 = ChangeMessage {
        since: peer2_doc.state_vector(),
        ops: peer1_outbox,
    };
    peer2_doc.apply_changes(message_to_peer2);

    // Simulate sync: peer 2 sends to peer 1
    let peer2_changes = peer2_doc.encode_changes_since(&peer1_doc.state_vector());
    peer1_doc.apply_changes(peer2_changes);

    // Both should now have the same state
    assert_eq!(peer1_doc.state_vector(), peer2_doc.state_vector());
    assert_eq!(peer1_doc.state_vector().get(1), Some(2));
    assert_eq!(peer1_doc.state_vector().get(2), Some(1));
}
