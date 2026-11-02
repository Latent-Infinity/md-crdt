use md_crdt::core::{OpId, Sequence, SequenceOp};
use md_crdt_naive_oracle::Sequence as OracleSequence;

fn id(peer: u64, counter: u64) -> OpId {
    OpId { counter, peer }
}

fn insert(after: Option<OpId>, id: OpId) -> SequenceOp<OpId> {
    SequenceOp::Insert {
        after,
        id,
        value: id,
        right_origin: None,
    }
}

#[test]
fn deep_chain_concurrent_appends_preserve_subtrees_and_sibling_order() {
    let root = id(1, 1_000);
    let tail = id(9, 1);
    let mut history = vec![insert(None, root)];
    let mut anchor = root;
    for counter in 1_001..=1_384 {
        let next = id(1, counter);
        history.push(insert(Some(anchor), next));
        anchor = next;
    }
    history.push(insert(None, tail));

    let mut left = Sequence::new();
    let mut right = Sequence::new();
    let mut oracle = OracleSequence::new();
    for op in history {
        left.apply(op.clone());
        right.apply(op.clone());
        oracle.apply(op);
    }

    let mut concurrent = Vec::new();
    for peer in 2..=5 {
        concurrent.push(insert(Some(root), id(peer, 2)));
        concurrent.push(insert(Some(id(peer, 2)), id(peer, 3)));
        concurrent.push(insert(Some(id(peer, 3)), id(peer, 4)));
    }
    for op in &concurrent {
        left.apply(op.clone());
        oracle.apply(op.clone());
        assert_eq!(left.to_vec(), oracle.elements());
    }
    // Deliver each branch backwards, and peers in the opposite order. This
    // promotes delayed branches rather than relying only on ready inserts.
    for op in concurrent.into_iter().rev() {
        right.apply(op);
    }

    let mut expected = vec![root];
    expected.extend((1_001..=1_384).map(|counter| id(1, counter)));
    for peer in (2..=5).rev() {
        expected.extend((2..=4).map(|counter| id(peer, counter)));
    }
    expected.push(tail);
    assert_eq!(left.to_vec(), expected);
    assert_eq!(right.to_vec(), expected);
    assert_eq!(left.element_ids(), right.element_ids());
    assert_eq!(left.pending_op_count(), 0);
    assert_eq!(right.pending_op_count(), 0);
}

#[test]
fn nested_sibling_placement_with_right_origins_matches_rebuild_oracle() {
    let mut sequence = Sequence::new();
    let mut oracle = OracleSequence::new();
    let root = id(1, 100);
    let mut ops = vec![insert(None, root)];
    let mut anchor = root;
    for counter in 101..=228 {
        let next = id(1, counter);
        ops.push(insert(Some(anchor), next));
        anchor = next;
    }
    ops.push(insert(None, id(9, 1)));
    for (peer, parent, right_origin) in [
        (2, id(1, 164), None),
        (3, root, Some(id(1, 101))),
        (4, id(1, 164), Some(id(1, 165))),
        (5, root, None),
    ] {
        ops.push(SequenceOp::Insert {
            after: Some(parent),
            id: id(peer, 2),
            value: id(peer, 2),
            right_origin,
        });
        ops.push(insert(Some(id(peer, 2)), id(peer, 3)));
    }
    for op in ops {
        sequence.apply(op.clone());
        oracle.apply(op);
        assert_eq!(sequence.to_vec(), oracle.elements());
    }
}

#[test]
fn dangling_anchor_accounting_includes_every_retained_insert_and_delete() {
    let root = id(1, 1);
    let branch = id(2, 1);
    let leaf = id(2, 2);
    let unrelated = id(3, 1);
    let mut sequence = Sequence::new();
    sequence.apply(insert(Some(branch), leaf));
    sequence.apply(insert(Some(root), branch));
    sequence.apply(SequenceOp::Delete {
        target: leaf,
        id: id(4, 1),
    });
    sequence.apply(SequenceOp::Delete {
        target: leaf,
        id: id(5, 1),
    });
    sequence.apply(insert(Some(unrelated), id(3, 2)));
    assert_eq!(sequence.pending_op_count(), 5);
    assert_eq!(sequence.pending_ops_iter().count(), 5);
    assert_eq!(
        sequence
            .pending_ops_iter()
            .filter(|operation| matches!(operation, SequenceOp::Insert { .. }))
            .count(),
        3
    );
    assert!(sequence.to_vec().is_empty());

    // Unrelated activity cannot expire dependencies that have not arrived.
    for counter in 1..=32 {
        sequence.apply(insert(None, id(10, counter)));
    }
    assert_eq!(sequence.pending_op_count(), 5);
    sequence.apply(insert(None, root));
    assert_eq!(sequence.pending_op_count(), 1);
    assert_eq!(sequence.pending_ops_iter().count(), 1);
    assert_eq!(sequence.get_element(&leaf).unwrap().value, None);
    assert_eq!(sequence.get_element(&branch).unwrap().value, Some(branch));

    sequence.apply(insert(None, unrelated));
    assert_eq!(sequence.pending_op_count(), 0);
    assert_eq!(sequence.pending_ops_iter().count(), 0);
    assert_eq!(
        sequence.get_element(&id(3, 2)).unwrap().value,
        Some(id(3, 2))
    );
    assert!(!sequence.to_vec().contains(&leaf));
}
