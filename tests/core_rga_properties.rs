use md_crdt::core::{OpId, Sequence, SequenceOp};
use proptest::collection::vec;
use proptest::prelude::*;
mod proptest_config;

#[derive(Clone, Debug)]
enum OpSpec {
    Insert {
        after: Option<usize>,
        value: u8,
        peer: u64,
    },
    Delete {
        target: Option<usize>,
        peer: u64,
    },
}

fn op_specs() -> impl Strategy<Value = Vec<OpSpec>> {
    vec(
        prop_oneof![
            (
                any::<Option<prop::sample::Index>>(),
                any::<u8>(),
                1u64..4u64
            )
                .prop_map(|(idx, value, peer)| OpSpec::Insert {
                    after: idx.map(|i| i.index(128)),
                    value,
                    peer,
                },),
            (any::<Option<prop::sample::Index>>(), 1u64..4u64).prop_map(|(idx, peer)| {
                OpSpec::Delete {
                    target: idx.map(|i| i.index(128)),
                    peer,
                }
            }),
        ],
        0..60,
    )
}

fn realize_ops(specs: &[OpSpec]) -> Vec<SequenceOp<u8>> {
    let mut ops = Vec::new();
    let mut ids: Vec<OpId> = Vec::new();
    let mut counters: std::collections::BTreeMap<u64, u64> = Default::default();

    for spec in specs {
        match *spec {
            OpSpec::Insert { after, value, peer } => {
                let counter = counters.entry(peer).or_default();
                *counter += 1;
                let id = OpId {
                    counter: *counter,
                    peer,
                };
                let after_id = if ids.is_empty() {
                    None
                } else {
                    after.and_then(|idx| ids.get(idx % ids.len()).copied())
                };
                ops.push(SequenceOp::Insert {
                    after: after_id,
                    id,
                    value,
                    right_origin: None,
                });
                ids.push(id);
            }
            OpSpec::Delete { target, peer } => {
                let counter = counters.entry(peer).or_default();
                *counter += 1;
                let id = OpId {
                    counter: *counter,
                    peer,
                };
                let target_id = if ids.is_empty() {
                    OpId {
                        counter: 9999,
                        peer: 9999,
                    }
                } else {
                    target
                        .and_then(|idx| ids.get(idx % ids.len()).copied())
                        .unwrap_or(OpId {
                            counter: 9999,
                            peer: 9999,
                        })
                };
                ops.push(SequenceOp::Delete {
                    target: target_id,
                    id,
                });
            }
        }
    }

    ops
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]
    #[test]
    fn prop_convergence_n_peers(specs in op_specs()) {
        let ops = realize_ops(&specs);
        let mut a = Sequence::new();
        let mut b = Sequence::new();

        for op in &ops {
            a.apply(op.clone());
        }
        for op in ops.iter().rev() {
            b.apply(op.clone());
        }

        prop_assert_eq!(a.to_vec(), b.to_vec());
    }

    #[test]
    fn prop_idempotence(specs in op_specs()) {
        let ops = realize_ops(&specs);
        let mut a = Sequence::new();
        let mut b = Sequence::new();

        for op in &ops {
            a.apply(op.clone());
        }
        for op in &ops {
            b.apply(op.clone());
            b.apply(op.clone());
        }

        prop_assert_eq!(a.to_vec(), b.to_vec());
    }

    #[test]
    fn prop_commutativity(specs in op_specs()) {
        let ops = realize_ops(&specs);
        if ops.len() < 2 {
            return Ok(());
        }
        let mut a = Sequence::new();
        let mut b = Sequence::new();
        let op_a = ops[0].clone();
        let op_b = ops[1].clone();

        a.apply(op_a.clone());
        a.apply(op_b.clone());

        b.apply(op_b);
        b.apply(op_a);

        prop_assert_eq!(a.to_vec(), b.to_vec());
    }

    #[test]
    fn prop_associativity(specs in op_specs()) {
        let ops = realize_ops(&specs);
        let third = ops.len() / 3;
        let (a_ops, rest) = ops.split_at(third);
        let (b_ops, c_ops) = rest.split_at(third);

        let mut left = Sequence::new();
        for op in a_ops {
            left.apply(op.clone());
        }
        for op in b_ops {
            left.apply(op.clone());
        }
        for op in c_ops {
            left.apply(op.clone());
        }

        let mut right = Sequence::new();
        for op in a_ops {
            right.apply(op.clone());
        }
        for op in b_ops {
            right.apply(op.clone());
        }
        for op in c_ops {
            right.apply(op.clone());
        }

        prop_assert_eq!(left.to_vec(), right.to_vec());
    }
}
