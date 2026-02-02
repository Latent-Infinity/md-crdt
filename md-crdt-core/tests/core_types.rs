use md_crdt_core::{OpId, StateVector};

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
