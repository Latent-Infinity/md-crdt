use md_crdt_core::{OpId, Sequence};
use proptest::collection::vec;
use proptest::prelude::*;

// Strategy for generating OpId
fn op_id_strategy() -> impl Strategy<Value = OpId> {
    (1..100u64, 1..3u64) // Keep peer and counter values small and non-zero
        .prop_map(|(counter, peer)| OpId { counter, peer })
}

// Strategy for generating an operation
fn op_strategy() -> impl Strategy<Value = (OpId, u8)> {
    (op_id_strategy(), any::<u8>())
}

// Property: Applying the same set of operations in different orders
// to two separate replicas should result in the same final state.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn test_convergence(ops_values in vec(any::<u8>(), 0..10)) {
        let ops: Vec<_> = ops_values.into_iter().enumerate().map(|(i, val)| {
            (OpId { counter: i as u64 + 1, peer: 1 }, val)
        }).collect();

        let mut replica1 = Sequence::new();
        let mut replica2 = Sequence::new();

        // Apply ops to replica1
        for op in &ops {
            replica1.apply_op(*op);
        }

        // Apply ops in reverse order to replica2
        for op in ops.iter().rev() {
            replica2.apply_op(*op);
        }

        prop_assert_eq!(
            replica1.iter().copied().collect::<Vec<_>>(),
            replica2.iter().copied().collect::<Vec<_>>(),
            "Replicas should converge to the same state"
        );
    }
}

// Property: Applying the same operations in any order should
// result in the same final state (commutativity).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn test_commutativity(ops_values in vec(any::<u8>(), 0..10)) {
        // Generate ops with unique OpIds to ensure valid CRDT semantics
        let ops: Vec<_> = ops_values.into_iter().enumerate().map(|(i, val)| {
            (OpId { counter: i as u64 + 1, peer: 1 }, val)
        }).collect();

        let mut replica1 = Sequence::new();
        let mut replica2 = Sequence::new();

        // Apply ops in original order to replica1
        for op in &ops {
            replica1.apply_op(*op);
        }

        // Apply ops in shuffled order to replica2
        let mut shuffled = ops.clone();
        shuffled.reverse(); // Simple reordering for determinism
        for op in &shuffled {
            replica2.apply_op(*op);
        }

        prop_assert_eq!(
            replica1.iter().copied().collect::<Vec<_>>(),
            replica2.iter().copied().collect::<Vec<_>>(),
            "Applying operations in different orders should produce the same result"
        );
    }
}

// Property: Applying an operation multiple times should be the
// same as applying it once.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn test_idempotence(op in op_strategy()) {
        let mut replica1 = Sequence::new();
        replica1.apply_op(op);

        let mut replica2 = Sequence::new();
        replica2.apply_op(op);
        replica2.apply_op(op);

        prop_assert_eq!(
            replica1.iter().copied().collect::<Vec<_>>(),
            replica2.iter().copied().collect::<Vec<_>>(),
            "Applying an operation twice should be idempotent"
        );
    }
}

#[test]
fn test_concurrent_insert_ordering() {
    // This test verifies that concurrent inserts are ordered by descending OpId.
    let mut sequence = Sequence::new();

    let op1 = (
        OpId {
            counter: 1,
            peer: 1,
        },
        'a',
    );
    let op2 = (
        OpId {
            counter: 2,
            peer: 1,
        },
        'b',
    ); // Higher counter
    let op3 = (
        OpId {
            counter: 2,
            peer: 2,
        },
        'c',
    ); // Same counter, higher peer ID

    // Apply in some order
    sequence.apply_op(op2);
    sequence.apply_op(op1);
    sequence.apply_op(op3);

    let expected = vec!['c', 'b', 'a'];
    let actual: Vec<char> = sequence.iter().copied().collect();

    assert_eq!(
        actual, expected,
        "Concurrent inserts should be ordered by descending OpId"
    );
}
