use md_crdt_core::{OpId, Sequence, SequenceOp};
use proptest::collection::vec;
use proptest::prelude::*;
mod proptest_config;

fn op_id(peer: u64, counter: u64) -> OpId {
    OpId { counter, peer }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]
    #[test]
    fn test_convergence(ops_values in vec(any::<u8>(), 0..10)) {
        let ops: Vec<_> = ops_values
            .iter()
            .enumerate()
            .map(|(i, val)| SequenceOp::Insert {
                after: None,
                id: op_id(1, i as u64 + 1),
                value: *val,
                right_origin: None,
            })
            .collect();

        let mut replica1 = Sequence::new();
        let mut replica2 = Sequence::new();

        for op in &ops {
            replica1.apply(op.clone());
        }
        for op in ops.iter().rev() {
            replica2.apply(op.clone());
        }

        prop_assert_eq!(replica1.to_vec(), replica2.to_vec());
    }
}

#[test]
fn test_concurrent_insert_ordering() {
    let mut sequence = Sequence::new();

    let op1 = SequenceOp::Insert {
        after: None,
        id: op_id(1, 1),
        value: 'a',
        right_origin: None,
    };
    let op2 = SequenceOp::Insert {
        after: None,
        id: op_id(1, 2),
        value: 'b',
        right_origin: None,
    };
    let op3 = SequenceOp::Insert {
        after: None,
        id: op_id(2, 2),
        value: 'c',
        right_origin: None,
    };

    sequence.apply(op2);
    sequence.apply(op1);
    sequence.apply(op3);

    let expected = vec!['c', 'b', 'a'];
    let actual: Vec<char> = sequence.iter().copied().collect();

    assert_eq!(
        actual, expected,
        "Concurrent inserts should be ordered by descending OpId"
    );
}
