use md_crdt::core::{MarkInterval, MarkSet, OpId, TextAnchor};
use proptest::prelude::*;
mod proptest_config;

fn op_id_strategy() -> impl Strategy<Value = OpId> {
    (1..100u64, 1..3u64).prop_map(|(counter, peer)| OpId { counter, peer })
}

fn text_anchor_strategy() -> impl Strategy<Value = TextAnchor> {
    op_id_strategy().prop_map(|op_id| TextAnchor { op_id })
}

// Strategy for generating two distinct OpIds (avoids global rejects at high case counts)
fn distinct_op_id_pair() -> impl Strategy<Value = (OpId, OpId)> {
    (op_id_strategy(), op_id_strategy()).prop_filter("id1 != id2", |(a, b)| a != b)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]
    #[test]
    fn test_causal_add_wins(
        (add_id, remove_id) in distinct_op_id_pair(),
        start in text_anchor_strategy(),
        end in text_anchor_strategy(),
    ) {

        let interval = MarkInterval::<String, String>::new(add_id, start, end);

        let mut set1 = MarkSet::new();
        let mut set2 = MarkSet::new();

        // Apply in different orders
        set1.add(interval.clone());
        set1.remove(add_id, remove_id);

        set2.remove(add_id, remove_id);
        set2.add(interval.clone());

        // Causal add wins, so the interval should be active if add_id > remove_id
        let expected = add_id > remove_id;
        prop_assert_eq!(set1.is_active(&add_id), expected, "Set 1 CausalAddWins failed");
        prop_assert_eq!(set2.is_active(&add_id), expected, "Set 2 CausalAddWins failed");
        prop_assert_eq!(set1.is_active(&add_id), set2.is_active(&add_id), "Sets should converge");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_config::cases()))]
    #[test]
    fn test_lww_attribute_update(
        add_id in op_id_strategy(),
        start in text_anchor_strategy(),
        end in text_anchor_strategy(),
        attr_key in any::<String>(),
        attr_val1 in any::<u8>(),
        attr_val2 in any::<u8>(),
        (attr_id1, attr_id2) in distinct_op_id_pair(),
    ) {

        let mut interval1 = MarkInterval::new(add_id, start, end);
        let mut interval2 = MarkInterval::new(add_id, start, end);

        // Apply in different orders
        interval1.update_attribute(attr_key.clone(), attr_val1, attr_id1);
        interval1.update_attribute(attr_key.clone(), attr_val2, attr_id2);

        interval2.update_attribute(attr_key.clone(), attr_val2, attr_id2);
        interval2.update_attribute(attr_key.clone(), attr_val1, attr_id1);

        let expected_val = if attr_id1 > attr_id2 { attr_val1 } else { attr_val2 };

        prop_assert_eq!(interval1.attributes.get(&attr_key).unwrap().get(), expected_val, "Interval 1 should have the attribute value of the op with the highest OpId");
        prop_assert_eq!(interval2.attributes.get(&attr_key).unwrap().get(), expected_val, "Interval 2 should have the attribute value of the op with the highest OpId");
    }
}
