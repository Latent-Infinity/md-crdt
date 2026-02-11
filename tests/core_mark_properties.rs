//! Property-based tests for Mark CRDT behavior

use md_crdt::core::mark::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
use md_crdt::core::{OpId, StateVector};
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::BTreeMap;
mod proptest_config;

fn op_id_strategy() -> impl Strategy<Value = OpId> {
    (1u64..10, 1u64..4).prop_map(|(counter, peer)| OpId { counter, peer })
}

fn distinct_op_id_pair_strategy() -> impl Strategy<Value = (OpId, OpId)> {
    (1u64..10, 1u64..4, 1u64..10, 1u64..4).prop_map(|(c1, p1, c2, p2)| {
        let op_id1 = OpId {
            counter: c1,
            peer: p1,
        };
        let mut op_id2 = OpId {
            counter: c2,
            peer: p2,
        };
        if op_id1 == op_id2 {
            op_id2.counter = if c2 < 10 { c2 + 1 } else { c2 - 1 };
        }
        (op_id1, op_id2)
    })
}

fn anchor_strategy() -> impl Strategy<Value = Anchor> {
    (op_id_strategy(), prop::bool::ANY).prop_map(|(elem_id, before)| Anchor {
        elem_id,
        bias: if before {
            AnchorBias::Before
        } else {
            AnchorBias::After
        },
    })
}

fn mark_kind_strategy() -> impl Strategy<Value = MarkKind> {
    prop_oneof![
        Just(MarkKind::Bold),
        Just(MarkKind::Italic),
        Just(MarkKind::Code),
        Just(MarkKind::Link),
    ]
}

/// Mark operation template (without unique op_id assigned yet)
#[derive(Clone, Debug)]
enum MarkOpTemplate {
    Set {
        interval_id: OpId,
        kind: MarkKind,
        start: Anchor,
        end: Anchor,
    },
    Remove {
        interval_id: OpId,
        observed: StateVector,
    },
}

#[derive(Clone, Debug)]
enum MarkOp {
    Set {
        interval_id: OpId,
        kind: MarkKind,
        start: Anchor,
        end: Anchor,
        op_id: OpId,
    },
    Remove {
        interval_id: OpId,
        observed: StateVector,
        op_id: OpId,
    },
}

fn mark_op_template_strategy() -> impl Strategy<Value = MarkOpTemplate> {
    prop_oneof![
        (
            op_id_strategy(),
            mark_kind_strategy(),
            anchor_strategy(),
            anchor_strategy(),
        )
            .prop_map(|(interval_id, kind, start, end)| MarkOpTemplate::Set {
                interval_id,
                kind,
                start,
                end,
            }),
        (op_id_strategy(), 0u64..5).prop_map(|(interval_id, seen_counter)| {
            let mut observed = StateVector::new();
            observed.set(1, seen_counter);
            MarkOpTemplate::Remove {
                interval_id,
                observed,
            }
        }),
    ]
}

/// Assign unique op_ids to each operation template based on index
fn assign_unique_op_ids(templates: Vec<MarkOpTemplate>) -> Vec<MarkOp> {
    templates
        .into_iter()
        .enumerate()
        .map(|(idx, template)| {
            let op_id = OpId {
                counter: (idx + 1) as u64,
                peer: 100, // Use a dedicated peer for convergence test ops
            };
            match template {
                MarkOpTemplate::Set {
                    interval_id,
                    kind,
                    start,
                    end,
                } => MarkOp::Set {
                    interval_id,
                    kind,
                    start,
                    end,
                    op_id,
                },
                MarkOpTemplate::Remove {
                    interval_id,
                    observed,
                } => MarkOp::Remove {
                    interval_id,
                    observed,
                    op_id,
                },
            }
        })
        .collect()
}

fn mark_op_strategy() -> impl Strategy<Value = MarkOp> {
    prop_oneof![
        (
            op_id_strategy(),
            mark_kind_strategy(),
            anchor_strategy(),
            anchor_strategy(),
            op_id_strategy()
        )
            .prop_map(|(interval_id, kind, start, end, op_id)| MarkOp::Set {
                interval_id,
                kind,
                start,
                end,
                op_id
            }),
        (op_id_strategy(), 0u64..5, op_id_strategy()).prop_map(
            |(interval_id, seen_counter, op_id)| {
                let mut observed = StateVector::new();
                observed.set(1, seen_counter);
                MarkOp::Remove {
                    interval_id,
                    observed,
                    op_id,
                }
            }
        ),
    ]
}

fn apply_op(set: &mut MarkSet, op: &MarkOp) {
    match op {
        MarkOp::Set {
            interval_id,
            kind,
            start,
            end,
            op_id,
        } => {
            set.set_mark(
                *interval_id,
                kind.clone(),
                *start,
                *end,
                BTreeMap::new(),
                *op_id,
            );
        }
        MarkOp::Remove {
            interval_id,
            observed,
            op_id,
        } => {
            set.remove_mark(*interval_id, observed.clone(), *op_id);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]

    /// Property: Convergence - N peers applying same ops in different orders converge
    #[test]
    fn prop_mark_convergence(templates in vec(mark_op_template_strategy(), 0..30)) {
        // Assign unique op_ids to ensure each operation is distinct
        let ops = assign_unique_op_ids(templates);

        let mut set_forward = MarkSet::new();
        let mut set_reverse = MarkSet::new();

        // Apply ops in forward order
        for op in &ops {
            apply_op(&mut set_forward, op);
        }

        // Apply ops in reverse order
        for op in ops.iter().rev() {
            apply_op(&mut set_reverse, op);
        }

        // Both should have same active intervals
        let order = vec![
            OpId { counter: 1, peer: 1 },
            OpId { counter: 2, peer: 1 },
            OpId { counter: 3, peer: 1 },
        ];
        let spans_forward = set_forward.render_spans(&order, 3);
        let spans_reverse = set_reverse.render_spans(&order, 3);

        prop_assert_eq!(spans_forward, spans_reverse, "Mark sets should converge regardless of op order");
    }

    /// Property: CausalAddWins - add always wins over concurrent remove
    #[test]
    fn prop_causal_add_wins(
        interval_id in op_id_strategy(),
        kind in mark_kind_strategy(),
        start in anchor_strategy(),
        end in anchor_strategy(),
        remove_op_id in op_id_strategy(),
    ) {
        let mut set1 = MarkSet::new();
        let mut set2 = MarkSet::new();

        // Create observed StateVector that did NOT see the add
        let observed = StateVector::new();

        // Set1: add then remove
        set1.set_mark(interval_id, kind.clone(), start, end, BTreeMap::new(), interval_id);
        set1.remove_mark(interval_id, observed.clone(), remove_op_id);

        // Set2: remove then add
        set2.remove_mark(interval_id, observed, remove_op_id);
        set2.set_mark(interval_id, kind, start, end, BTreeMap::new(), interval_id);

        // Both should have mark active (add wins because remove didn't observe it)
        prop_assert!(set1.is_active(&interval_id), "Add should win in set1");
        prop_assert!(set2.is_active(&interval_id), "Add should win in set2");
    }

    /// Property: Idempotence - applying same op twice has no additional effect
    #[test]
    fn prop_mark_idempotence(ops in vec(mark_op_strategy(), 1..20)) {
        let mut set_once = MarkSet::new();
        let mut set_twice = MarkSet::new();

        for op in &ops {
            apply_op(&mut set_once, op);
        }

        for op in &ops {
            apply_op(&mut set_twice, op);
            apply_op(&mut set_twice, op); // Apply twice
        }

        let order = vec![
            OpId { counter: 1, peer: 1 },
            OpId { counter: 2, peer: 1 },
        ];
        let spans_once = set_once.render_spans(&order, 2);
        let spans_twice = set_twice.render_spans(&order, 2);

        prop_assert_eq!(spans_once, spans_twice, "Applying ops twice should be idempotent");
    }

    /// Edge case: Mark on empty text range (zero-width mark)
    #[test]
    fn prop_mark_empty_range(
        interval_id in op_id_strategy(),
        kind in mark_kind_strategy(),
        anchor in anchor_strategy(),
    ) {
        let mut set = MarkSet::new();

        // Create mark where start == end
        set.set_mark(interval_id, kind, anchor, anchor, BTreeMap::new(), interval_id);

        prop_assert!(set.is_active(&interval_id), "Zero-width mark should be valid");
    }

    /// Edge case: Concurrent SetMark with different attrs for same interval
    #[test]
    fn prop_concurrent_attrs_lww(
        interval_id in op_id_strategy(),
        kind in mark_kind_strategy(),
        start in anchor_strategy(),
        end in anchor_strategy(),
        op_ids in distinct_op_id_pair_strategy(),
    ) {
        let (op_id1, op_id2) = op_ids;

        let mut set1 = MarkSet::new();
        let mut set2 = MarkSet::new();

        let mut attrs1 = BTreeMap::new();
        attrs1.insert("key".to_string(), MarkValue::String("value1".into()));

        let mut attrs2 = BTreeMap::new();
        attrs2.insert("key".to_string(), MarkValue::String("value2".into()));

        // Apply in different orders
        set1.set_mark(interval_id, kind.clone(), start, end, attrs1.clone(), op_id1);
        set1.set_mark(interval_id, kind.clone(), start, end, attrs2.clone(), op_id2);

        set2.set_mark(interval_id, kind.clone(), start, end, attrs2, op_id2);
        set2.set_mark(interval_id, kind, start, end, attrs1, op_id1);

        // Both should converge to same attribute value (LWW by OpId)
        let interval1 = &set1.active_intervals()[0];
        let interval2 = &set2.active_intervals()[0];

        let value1 = interval1.attrs.get("key").map(|r| r.get());
        let value2 = interval2.attrs.get("key").map(|r| r.get());

        prop_assert_eq!(value1, value2, "LWW should converge to same attribute value");
    }
}
