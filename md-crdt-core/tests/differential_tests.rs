use md_crdt_core::{OpId, Sequence, SequenceOp};
use md_crdt_naive_oracle::Sequence as OracleSequence;
use proptest::collection::vec;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum OpSpec {
    Insert {
        after_index: Option<usize>,
        value: u8,
        peer: u64,
    },
    Delete {
        target_index: Option<usize>,
        peer: u64,
    },
}

fn op_spec_strategy() -> impl Strategy<Value = Vec<OpSpec>> {
    vec(
        prop_oneof![
            (
                any::<Option<prop::sample::Index>>(),
                any::<u8>(),
                1u64..4u64
            )
                .prop_map(|(idx, value, peer)| OpSpec::Insert {
                    after_index: idx.map(|i| i.index(128)),
                    value,
                    peer,
                },),
            (any::<Option<prop::sample::Index>>(), 1u64..4u64).prop_map(|(idx, peer)| {
                OpSpec::Delete {
                    target_index: idx.map(|i| i.index(128)),
                    peer,
                }
            }),
        ],
        0..50,
    )
}

fn realize_ops(specs: &[OpSpec]) -> Vec<SequenceOp<u8>> {
    let mut ops = Vec::new();
    let mut ids: Vec<OpId> = Vec::new();
    let mut counters: std::collections::BTreeMap<u64, u64> = Default::default();

    for spec in specs {
        match *spec {
            OpSpec::Insert {
                after_index,
                value,
                peer,
            } => {
                let counter = counters.entry(peer).or_default();
                *counter += 1;
                let id = OpId {
                    counter: *counter,
                    peer,
                };
                let after = if ids.is_empty() {
                    None
                } else {
                    after_index.and_then(|idx| ids.get(idx % ids.len()).copied())
                };
                ops.push(SequenceOp::Insert {
                    after,
                    id,
                    value,
                    right_origin: None,
                });
                ids.push(id);
            }
            OpSpec::Delete { target_index, peer } => {
                let counter = counters.entry(peer).or_default();
                *counter += 1;
                let id = OpId {
                    counter: *counter,
                    peer,
                };
                let target = if ids.is_empty() {
                    OpId {
                        counter: 9999,
                        peer: 9999,
                    }
                } else {
                    target_index
                        .and_then(|idx| ids.get(idx % ids.len()).copied())
                        .unwrap_or(OpId {
                            counter: 9999,
                            peer: 9999,
                        })
                };
                ops.push(SequenceOp::Delete { target, id });
            }
        }
    }

    ops
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn differential_test_sequence(specs in op_spec_strategy()) {
        let ops = realize_ops(&specs);
        let mut crdt_seq = Sequence::new();
        let mut oracle_seq = OracleSequence::new();

        for op in ops {
            crdt_seq.apply(op.clone());
            oracle_seq.apply(op);
        }

        let crdt_elements = crdt_seq.to_vec();
        let oracle_elements = oracle_seq.elements();

        prop_assert_eq!(crdt_elements, oracle_elements, "CRDT sequence should match the naive oracle");
    }
}
