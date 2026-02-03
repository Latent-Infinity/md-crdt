use md_crdt_core::mark::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
use md_crdt_core::{OpId, StateVector};
use md_crdt_naive_oracle::mark::NaiveMarkSet;
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::BTreeMap;
mod proptest_config;

#[derive(Clone, Debug)]
enum MarkOp {
    Set {
        id: OpId,
        kind: MarkKind,
        start: OpId,
        end: OpId,
    },
    Remove {
        id: OpId,
        observed: StateVector,
    },
}

fn ops() -> impl Strategy<Value = Vec<MarkOp>> {
    vec(
        prop_oneof![
            (1u64..4u64, 1u64..4u64, 1u64..4u64, 1u64..4u64).prop_map(
                |(peer, counter, start, end)| MarkOp::Set {
                    id: OpId { counter, peer },
                    kind: MarkKind::Bold,
                    start: OpId {
                        counter: start,
                        peer: 1
                    },
                    end: OpId {
                        counter: end,
                        peer: 1
                    },
                },
            ),
            (1u64..4u64, 1u64..4u64).prop_map(|(peer, counter)| {
                let mut sv = StateVector::new();
                sv.set(1, counter);
                MarkOp::Remove {
                    id: OpId { counter, peer },
                    observed: sv,
                }
            }),
        ],
        0..40,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]
    #[test]
    fn mark_differential(ops in ops()) {
        let mut real = MarkSet::new();
        let mut naive = NaiveMarkSet::new();
        let mut attrs = BTreeMap::new();
        attrs.insert("k".to_string(), MarkValue::String("v".into()));

        for op in ops {
            match op {
                MarkOp::Set { id, kind, start, end } => {
                    let start_anchor = Anchor { elem_id: start, bias: AnchorBias::Before };
                    let end_anchor = Anchor { elem_id: end, bias: AnchorBias::After };
                    real.set_mark(id, kind.clone(), start_anchor, end_anchor, attrs.clone(), id);
                    naive.set_mark(id, kind, start_anchor, end_anchor, attrs.clone(), id);
                }
                MarkOp::Remove { id, observed } => {
                    real.remove_mark(id, observed.clone(), id);
                    naive.remove_mark(id, observed, id);
                }
            }
        }

        let order = vec![OpId { counter: 1, peer: 1 }, OpId { counter: 2, peer: 1 }];
        let real_spans = real.render_spans(&order, 2);
        let naive_spans = naive.render_spans(&order, 2);
        prop_assert_eq!(real_spans, naive_spans);
    }
}
