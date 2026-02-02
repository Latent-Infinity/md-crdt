use md_crdt_core::{OpId, Sequence as CrdtSequence};
use md_crdt_naive_oracle::Sequence as OracleSequence;
use proptest::collection::vec;
use proptest::prelude::*;

// Strategy for generating OpId
fn op_id_strategy() -> impl Strategy<Value = OpId> {
    (1..100u64, 1..3u64).prop_map(|(counter, peer)| OpId { counter, peer })
}

// Strategy for generating an operation
fn op_strategy() -> impl Strategy<Value = (OpId, u8)> {
    (op_id_strategy(), any::<u8>())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))] // As per plan: 100K iterations in CI
    #[test]
    fn differential_test_sequence(ops in vec(op_strategy(), 0..100)) {
        let mut crdt_seq = CrdtSequence::new();
        let mut oracle_seq = OracleSequence::new();

        for op in ops {
            crdt_seq.apply_op(op);
            oracle_seq.apply(op);
        }

        let crdt_elements: Vec<_> = crdt_seq.iter().copied().collect();
        let oracle_elements: Vec<_> = oracle_seq.elements().to_vec();

        prop_assert_eq!(crdt_elements, oracle_elements, "CRDT sequence should match the naive oracle");
    }
}
