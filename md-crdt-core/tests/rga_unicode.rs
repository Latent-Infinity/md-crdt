use md_crdt_core::{OpId, Sequence};

fn insert_chars(seq: &mut Sequence<char>, text: &str, peer: u64) {
    let mut prev = None;
    for (i, ch) in text.chars().enumerate() {
        let id = OpId {
            counter: i as u64 + 1,
            peer,
        };
        seq.insert(prev, ch, id);
        prev = Some(id);
    }
}

#[test]
fn unicode_multi_byte() {
    let mut seq = Sequence::new();
    insert_chars(&mut seq, "Café", 1);
    let result: String = seq.iter().collect();
    assert_eq!(result, "Café");
}

#[test]
fn unicode_combining() {
    let mut seq = Sequence::new();
    insert_chars(&mut seq, "e\u{0301}", 1);
    let result: String = seq.iter().collect();
    assert_eq!(result, "e\u{0301}");
}

#[test]
fn unicode_rtl() {
    let mut seq = Sequence::new();
    insert_chars(&mut seq, "שלום", 1);
    let result: String = seq.iter().collect();
    assert_eq!(result, "שלום");
}

#[test]
fn unicode_emoji_sequence() {
    let mut seq = Sequence::new();
    insert_chars(&mut seq, "👨‍👩‍👧‍👦", 1);
    let result: String = seq.iter().collect();
    assert_eq!(result, "👨‍👩‍👧‍👦");
}
