use md_crdt_core::{LwwRegister, OpId};
use proptest::prelude::*;

// Strategy for generating OpId
fn op_id_strategy() -> impl Strategy<Value = OpId> {
    (1..100u64, 1..3u64).prop_map(|(counter, peer)| OpId { counter, peer })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn test_lww_register_conflict_resolution(
        (val1, id1) in (any::<u8>(), op_id_strategy()),
        (val2, id2) in (any::<u8>(), op_id_strategy()),
    ) {
        prop_assume!(id1 != id2);

        let mut register1 = LwwRegister::new(0, OpId { counter: 0, peer: 0 });
        let mut register2 = LwwRegister::new(0, OpId { counter: 0, peer: 0 });

        // Apply in different orders
        register1.set(val1, id1);
        register1.set(val2, id2);

        register2.set(val2, id2);
        register2.set(val1, id1);

        let expected_val = if id1 > id2 { val1 } else { val2 };

        prop_assert_eq!(register1.get(), expected_val, "Register 1 should have the value of the op with the highest OpId");
        prop_assert_eq!(register2.get(), expected_val, "Register 2 should have the value of the op with the highest OpId");
        prop_assert_eq!(register1.get(), register2.get(), "Registers should converge");
    }
}

use md_crdt_core::Map;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn test_map_conflict_resolution(
        key in any::<u8>(),
        (val1, id1) in (any::<u8>(), op_id_strategy()),
        (val2, id2) in (any::<u8>(), op_id_strategy()),
    ) {
        prop_assume!(id1 != id2);

        let mut map1 = Map::new();
        let mut map2 = Map::new();

        // Apply in different orders
        map1.set(key, val1, id1);
        map1.set(key, val2, id2);

        map2.set(key, val2, id2);
        map2.set(key, val1, id1);

        let expected_val = if id1 > id2 { val1 } else { val2 };

        prop_assert_eq!(map1.get(&key).unwrap(), expected_val, "Map 1 should have the value of the op with the highest OpId");
        prop_assert_eq!(map2.get(&key).unwrap(), expected_val, "Map 2 should have the value of the op with the highest OpId");
    }
}
