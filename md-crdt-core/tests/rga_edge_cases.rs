use md_crdt_core::{OpId, Sequence};

#[test]
fn edge_empty_sequence_operations() {
    let mut seq: Sequence<char> = Sequence::new();
    seq.delete(
        OpId {
            counter: 1,
            peer: 1,
        },
        OpId {
            counter: 2,
            peer: 1,
        },
    );
    assert!(seq.to_vec().is_empty());
}

#[test]
fn edge_single_element_sequence() {
    let mut seq = Sequence::new();
    let id = OpId {
        counter: 1,
        peer: 1,
    };
    seq.insert(None, 'a', id);
    assert_eq!(seq.to_vec(), vec!['a']);
}

#[test]
fn edge_delete_non_existent_element() {
    let mut seq = Sequence::new();
    seq.insert(
        None,
        'a',
        OpId {
            counter: 1,
            peer: 1,
        },
    );
    seq.delete(
        OpId {
            counter: 99,
            peer: 9,
        },
        OpId {
            counter: 2,
            peer: 1,
        },
    );
    assert_eq!(seq.to_vec(), vec!['a']);
}

#[test]
fn edge_insert_after_deleted_element() {
    let mut seq = Sequence::new();
    let a = OpId {
        counter: 1,
        peer: 1,
    };
    let b = OpId {
        counter: 2,
        peer: 1,
    };
    seq.insert(None, 'A', a);
    seq.delete(
        a,
        OpId {
            counter: 3,
            peer: 1,
        },
    );
    seq.insert(Some(a), 'B', b);
    assert_eq!(seq.to_vec(), vec!['B']);
}

#[test]
fn edge_concurrent_delete_and_insert() {
    let mut seq1 = Sequence::new();
    let mut seq2 = Sequence::new();

    let a = OpId {
        counter: 1,
        peer: 1,
    };
    let del = OpId {
        counter: 2,
        peer: 1,
    };
    let b = OpId {
        counter: 1,
        peer: 2,
    };

    seq1.insert(None, 'A', a);
    seq1.delete(a, del);
    seq1.insert(Some(a), 'B', b);

    seq2.insert(None, 'A', a);
    seq2.insert(Some(a), 'B', b);
    seq2.delete(a, del);

    assert_eq!(seq1.to_vec(), seq2.to_vec());
}
