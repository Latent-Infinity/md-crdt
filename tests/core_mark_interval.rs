//! Property tests for the unified rich MarkSet (causal remove-wins + LWW attrs).

use md_crdt::core::mark::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
use md_crdt::core::{OpId, StateVector};
use proptest::prelude::*;
use std::collections::BTreeMap;
mod proptest_config;

fn op_id_strategy() -> impl Strategy<Value = OpId> {
    (1..100u64, 1..3u64).prop_map(|(counter, peer)| OpId { counter, peer })
}

fn anchor_strategy() -> impl Strategy<Value = Anchor> {
    (
        op_id_strategy(),
        prop_oneof![Just(AnchorBias::Before), Just(AnchorBias::After)],
    )
        .prop_map(|(elem_id, bias)| Anchor { elem_id, bias })
}

fn distinct_op_id_pair() -> impl Strategy<Value = (OpId, OpId)> {
    (op_id_strategy(), op_id_strategy()).prop_filter("id1 != id2", |(a, b)| a != b)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]
    #[test]
    fn test_causal_remove_wins(
        interval_id in op_id_strategy(),
        remove_id in op_id_strategy(),
        start in anchor_strategy(),
        end in anchor_strategy(),
    ) {
        let mut set1 = MarkSet::new();
        let mut set2 = MarkSet::new();

        let mut observed = StateVector::new();
        observed.set(interval_id.peer, interval_id.counter);

        // Apply set then remove in different orders.
        set1.set_mark(interval_id, MarkKind::Bold, start, end, BTreeMap::new(), interval_id);
        set1.remove_mark(interval_id, observed.clone(), remove_id);

        set2.remove_mark(interval_id, observed, remove_id);
        set2.set_mark(interval_id, MarkKind::Bold, start, end, BTreeMap::new(), interval_id);

        // With observed covering the add, remove always deactivates regardless of order.
        prop_assert!(!set1.is_active(&interval_id));
        prop_assert!(!set2.is_active(&interval_id));
        prop_assert_eq!(set1.is_active(&interval_id), set2.is_active(&interval_id));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]
    #[test]
    fn test_lww_attribute_update(
        interval_id in op_id_strategy(),
        start in anchor_strategy(),
        end in anchor_strategy(),
        attr_key in "[a-z]{1,4}",
        attr_val1 in any::<u8>(),
        attr_val2 in any::<u8>(),
        (attr_id1, attr_id2) in distinct_op_id_pair(),
    ) {
        let mut set1 = MarkSet::new();
        let mut set2 = MarkSet::new();

        let attrs1 = BTreeMap::from([(
            attr_key.clone(),
            MarkValue::String(attr_val1.to_string()),
        )]);
        let attrs2 = BTreeMap::from([(
            attr_key.clone(),
            MarkValue::String(attr_val2.to_string()),
        )]);

        set1.set_mark(interval_id, MarkKind::Link, start, end, attrs1.clone(), attr_id1);
        set1.set_mark(interval_id, MarkKind::Link, start, end, attrs2.clone(), attr_id2);

        set2.set_mark(interval_id, MarkKind::Link, start, end, attrs2, attr_id2);
        set2.set_mark(interval_id, MarkKind::Link, start, end, attrs1, attr_id1);

        let expected = if attr_id1 > attr_id2 {
            MarkValue::String(attr_val1.to_string())
        } else {
            MarkValue::String(attr_val2.to_string())
        };

        let v1 = set1
            .interval(&interval_id)
            .and_then(|i| i.attrs.get(&attr_key))
            .map(|r| r.get());
        let v2 = set2
            .interval(&interval_id)
            .and_then(|i| i.attrs.get(&attr_key))
            .map(|r| r.get());
        prop_assert_eq!(v1.as_ref(), Some(&expected));
        prop_assert_eq!(v2.as_ref(), Some(&expected));
    }
}
