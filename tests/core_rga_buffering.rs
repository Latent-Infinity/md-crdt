use md_crdt::core::{OpId, Sequence};

#[test]
fn test_out_of_order_buffering() {
    let mut seq = Sequence::new();
    let a = OpId {
        counter: 1,
        peer: 1,
    };
    let b = OpId {
        counter: 2,
        peer: 1,
    };

    seq.insert(Some(a), 'B', b);
    assert!(seq.to_vec().is_empty());

    seq.insert(None, 'A', a);
    assert_eq!(seq.to_vec(), vec!['A', 'B']);
}

#[test]
fn test_long_causal_chain_reverse() {
    let mut seq = Sequence::new();
    let mut ids = Vec::new();

    let total = 10_000u64;
    for i in 0..total {
        ids.push(OpId {
            counter: i + 1,
            peer: 1,
        });
    }

    for i in (1..total).rev() {
        let after = ids[(i - 1) as usize];
        seq.insert(Some(after), (i % 26) as u8, ids[i as usize]);
    }

    assert!(seq.to_vec().is_empty());

    seq.insert(None, 0, ids[0]);
    assert_eq!(seq.len_visible() as u64, total);
}
