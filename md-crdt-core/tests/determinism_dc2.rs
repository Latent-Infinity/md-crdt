use md_crdt_core::{LwwRegister, OpId, Sequence, SequenceOp};

fn op(counter: u64) -> OpId {
    OpId { counter, peer: 1 }
}

#[test]
fn dc2_lww_register_uses_highest_op_id() {
    let mut reg_a = LwwRegister::new("old".to_string(), op(1));
    reg_a.set("new".to_string(), op(2));

    let mut reg_b = LwwRegister::new("new".to_string(), op(2));
    reg_b.set("old".to_string(), op(1));

    assert_eq!(reg_a.get(), "new".to_string());
    assert_eq!(reg_b.get(), "new".to_string());
}

#[test]
fn dc2_sequence_sibling_order_descending_op_id() {
    let mut seq = Sequence::new();
    let op_a = op(1);
    let op_b = op(2);

    seq.apply(SequenceOp::Insert {
        after: None,
        value: 'a',
        id: op_a,
        right_origin: None,
    });
    seq.apply(SequenceOp::Insert {
        after: None,
        value: 'b',
        id: op_b,
        right_origin: None,
    });

    let ids = seq.element_ids();
    assert_eq!(ids, vec![op_b, op_a]);
}

#[test]
fn dc2_conflict_resolution_order_independent() {
    let op_a = op(1);
    let op_b = op(2);

    let mut seq_a = Sequence::new();
    seq_a.apply(SequenceOp::Insert {
        after: None,
        value: 'a',
        id: op_a,
        right_origin: None,
    });
    seq_a.apply(SequenceOp::Insert {
        after: None,
        value: 'b',
        id: op_b,
        right_origin: None,
    });

    let mut seq_b = Sequence::new();
    seq_b.apply(SequenceOp::Insert {
        after: None,
        value: 'b',
        id: op_b,
        right_origin: None,
    });
    seq_b.apply(SequenceOp::Insert {
        after: None,
        value: 'a',
        id: op_a,
        right_origin: None,
    });

    assert_eq!(seq_a.element_ids(), seq_b.element_ids());
    assert_eq!(seq_a.to_vec(), seq_b.to_vec());
}
