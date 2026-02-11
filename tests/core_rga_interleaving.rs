use md_crdt::core::{OpId, Sequence};

fn insert_run(seq: &mut Sequence<char>, after: Option<OpId>, peer: u64, text: &str) -> Vec<OpId> {
    let mut ids = Vec::new();
    let mut prev = after;
    for (i, ch) in text.chars().enumerate() {
        let id = OpId {
            counter: i as u64 + 1,
            peer,
        };
        seq.insert(prev, ch, id);
        prev = Some(id);
        ids.push(id);
    }
    ids
}

#[test]
fn test_concurrent_runs_do_not_interleave() {
    let mut seq_a = Sequence::new();
    let mut seq_b = Sequence::new();

    let alice_ops = insert_run(&mut seq_a, None, 1, "hello");
    let bob_ops = insert_run(&mut seq_b, None, 2, "world");

    let mut seq = Sequence::new();
    for (i, id) in alice_ops.iter().enumerate() {
        let after = if i == 0 { None } else { Some(alice_ops[i - 1]) };
        seq.insert(after, "hello".chars().nth(i).unwrap(), *id);
    }
    for (i, id) in bob_ops.iter().enumerate() {
        let after = if i == 0 { None } else { Some(bob_ops[i - 1]) };
        seq.insert(after, "world".chars().nth(i).unwrap(), *id);
    }

    let result: String = seq.iter().collect();
    assert!(result == "helloworld" || result == "worldhello");
}

#[test]
fn test_right_origin_tracked() {
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
    seq.insert(None, 'B', b);

    let elem_b = seq.get_element(&b).unwrap();
    assert_eq!(elem_b.right_origin, Some(a));
}
