use md_crdt::core::{Element, OpId, Sequence, SequenceOp};
use std::collections::HashSet;

#[test]
fn inv_unique_element_ids() {
    let mut seq = Sequence::new();
    seq.insert(
        None,
        'a',
        OpId {
            counter: 1,
            peer: 1,
        },
    );
    seq.insert(
        None,
        'b',
        OpId {
            counter: 2,
            peer: 1,
        },
    );

    let ids: HashSet<_> = seq.element_ids().into_iter().collect();
    assert_eq!(ids.len(), seq.element_ids().len());
}

#[test]
fn inv_after_references_exist() {
    let mut seq = Sequence::new();
    let a = OpId {
        counter: 1,
        peer: 1,
    };
    let b = OpId {
        counter: 2,
        peer: 1,
    };
    seq.insert(None, 'a', a);
    seq.insert(Some(a), 'b', b);

    for elem in seq.iter_all() {
        if let Some(after) = elem.after {
            assert!(seq.get_element(&after).is_some());
        }
    }
}

#[test]
fn inv_tombstones_persist() {
    let mut seq = Sequence::new();
    let a = OpId {
        counter: 1,
        peer: 1,
    };
    seq.insert(None, 'a', a);
    seq.delete(
        a,
        OpId {
            counter: 2,
            peer: 1,
        },
    );

    let elem = seq.get_element(&a).unwrap();
    assert!(elem.value.is_none());
}

#[test]
fn inv_total_order_deterministic() {
    let mut seq1 = Sequence::new();
    let mut seq2 = Sequence::new();

    let a = OpId {
        counter: 1,
        peer: 1,
    };
    let b = OpId {
        counter: 1,
        peer: 2,
    };
    seq1.apply(SequenceOp::Insert {
        after: None,
        id: a,
        value: 'a',
        right_origin: None,
    });
    seq1.apply(SequenceOp::Insert {
        after: None,
        id: b,
        value: 'b',
        right_origin: None,
    });

    seq2.apply(SequenceOp::Insert {
        after: None,
        id: b,
        value: 'b',
        right_origin: None,
    });
    seq2.apply(SequenceOp::Insert {
        after: None,
        id: a,
        value: 'a',
        right_origin: None,
    });

    assert_eq!(seq1.to_vec(), seq2.to_vec());
}

#[test]
fn inv_tombstone_after_reference() {
    let mut seq = Sequence::new();
    let a = OpId {
        counter: 1,
        peer: 1,
    };
    let b = OpId {
        counter: 2,
        peer: 1,
    };
    seq.insert(None, 'a', a);
    seq.delete(
        a,
        OpId {
            counter: 3,
            peer: 1,
        },
    );
    seq.insert(Some(a), 'b', b);

    let b_elem: &Element<char> = seq.get_element(&b).unwrap();
    assert_eq!(b_elem.after, Some(a));
}
