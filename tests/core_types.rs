use md_crdt::core::{LwwRegister, MarkInterval, MarkSet, OpId, StateVector, TextAnchor};

#[test]
fn test_op_id_ordering() {
    // This will fail to compile until Ord is derived or implemented for OpId
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
    // This will also fail to compile until Ord is implemented
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
    // This will fail because the placeholder for is_empty returns false
    let sv = StateVector::new();
    assert!(sv.is_empty());
}

#[test]
fn test_state_vector_get_set() {
    let mut sv = StateVector::new();

    // Test get on empty vector
    assert_eq!(sv.get(1), None);

    // Test set and get
    sv.set(1, 42);
    assert_eq!(sv.get(1), Some(42));

    // Test update existing
    sv.set(1, 100);
    assert_eq!(sv.get(1), Some(100));

    // Test multiple peers
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
    let mut set = MarkSet::<String, String>::new();
    let add_id = OpId {
        counter: 1,
        peer: 1,
    };
    let remove_id = OpId {
        counter: 2,
        peer: 1,
    };

    // Test is_active on non-existent interval
    assert!(!set.is_active(&add_id));

    // Add interval
    let interval = MarkInterval::new(
        add_id,
        TextAnchor {
            op_id: OpId {
                counter: 0,
                peer: 0,
            },
        },
        TextAnchor {
            op_id: OpId {
                counter: 10,
                peer: 0,
            },
        },
    );
    set.add(interval);
    assert!(set.is_active(&add_id));

    // Remove with lower OpId (add wins)
    let lower_remove = OpId {
        counter: 0,
        peer: 1,
    };
    set.remove(add_id, lower_remove);
    assert!(set.is_active(&add_id));

    // Remove with equal OpId (not active)
    set.remove(add_id, add_id);
    assert!(!set.is_active(&add_id));

    // Remove again with same remove_id (idempotent, should keep highest remove)
    set.remove(add_id, lower_remove);
    assert!(!set.is_active(&add_id));

    // Remove with higher OpId
    set.remove(add_id, remove_id);
    assert!(!set.is_active(&add_id));
}
