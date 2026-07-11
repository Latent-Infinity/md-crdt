use md_crdt::core::mark::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
use md_crdt::core::{LwwRegister, OpId, StateVector};
use std::collections::BTreeMap;

#[test]
fn test_op_id_ordering() {
    let op1 = OpId {
        counter: 1,
        peer: 1,
    };
    let op2 = OpId {
        counter: 2,
        peer: 1,
    };
    assert!(op2 > op1);
}

#[test]
fn test_op_id_lexicographic_tie_breaking() {
    let op1 = OpId {
        counter: 1,
        peer: 1,
    };
    let op2 = OpId {
        counter: 1,
        peer: 2,
    };
    assert!(op2 > op1);
}

#[test]
fn test_state_vector_new_is_empty() {
    let sv = StateVector::new();
    assert!(sv.is_empty());
}

#[test]
fn test_state_vector_get_set() {
    let mut sv = StateVector::new();

    assert_eq!(sv.get(1), None);

    sv.set(1, 42);
    assert_eq!(sv.get(1), Some(42));

    sv.set(1, 100);
    assert_eq!(sv.get(1), Some(100));

    sv.set(2, 50);
    assert_eq!(sv.get(2), Some(50));
    assert_eq!(sv.get(1), Some(100));
}

#[test]
fn test_lww_register_op_id() {
    let op1 = OpId {
        counter: 1,
        peer: 1,
    };
    let op2 = OpId {
        counter: 2,
        peer: 1,
    };

    let mut reg = LwwRegister::new(42, op1);
    assert_eq!(reg.op_id(), op1);

    reg.set(100, op2);
    assert_eq!(reg.op_id(), op2);
}

#[test]
fn test_mark_set_edge_cases() {
    let mut set = MarkSet::new();
    let add_id = OpId {
        counter: 1,
        peer: 1,
    };
    let remove_id = OpId {
        counter: 2,
        peer: 1,
    };

    assert!(!set.is_active(&add_id));

    let start = Anchor {
        elem_id: OpId {
            counter: 0,
            peer: 0,
        },
        bias: AnchorBias::Before,
    };
    let end = Anchor {
        elem_id: OpId {
            counter: 10,
            peer: 0,
        },
        bias: AnchorBias::After,
    };
    set.set_mark(
        add_id,
        MarkKind::Bold,
        start,
        end,
        BTreeMap::from([("k".into(), MarkValue::String("v".into()))]),
        add_id,
    );
    assert!(set.is_active(&add_id));

    // Causal remove: observed state vector must include the add for remove to win.
    let mut observed = StateVector::new();
    observed.set(add_id.peer, add_id.counter);
    set.remove_mark(add_id, observed, remove_id);
    assert!(!set.is_active(&add_id));
}
