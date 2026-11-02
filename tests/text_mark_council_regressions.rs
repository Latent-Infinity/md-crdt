use md_crdt::core::mark::{
    Anchor, AnchorBias, MarkInterval, MarkKind, MarkSet, MarkValue, RemoveMark, Span,
};
use md_crdt::core::{LwwRegister, OpId, Sequence, StateVector};
use md_crdt::doc::{
    EquivalenceMode, Parser, block_id_from_op, block_text_seq, paragraph_visible_string,
};
use md_crdt::session::{CollaborativeDocument, SessionSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn op(counter: u64) -> OpId {
    OpId { counter, peer: 1 }
}

fn attrs(key: &str, value: &str) -> BTreeMap<String, MarkValue> {
    BTreeMap::from([(key.to_owned(), MarkValue::String(value.to_owned()))])
}

#[test]
fn unicode_comment_whitespace_survives_parse_and_field_mutation() {
    for whitespace in ['\u{00a0}', '\u{2003}', '\u{3000}'] {
        let input = format!(
            "---\ntitle: x{whitespace}# comment\nquoted: 'x{whitespace}# literal'\n---\n\nbody\n"
        );
        let mut document = Parser::try_parse(&input).unwrap();
        assert_eq!(document.frontmatter_field("title"), Some("x"));
        assert_eq!(document.serialize(EquivalenceMode::Exact), input);
        document
            .set_frontmatter_field("title".into(), Some("new".into()), op(100))
            .unwrap();
        let rendered = document.serialize(EquivalenceMode::Exact);
        assert!(rendered.contains(&format!("title: new{whitespace}# comment")));
        let reparsed = Parser::try_parse(&rendered).unwrap();
        assert_eq!(reparsed.frontmatter_field("title"), Some("new"));
        assert_eq!(
            reparsed.frontmatter_field("quoted"),
            Some(format!("'x{whitespace}# literal'").as_str())
        );
    }
}

#[test]
fn inline_ranges_follow_final_graphemes_across_removed_syntax() {
    for (input, visible, expected) in [
        ("a**\u{301}**z", "a\u{301}z", vec![(MarkKind::Bold, 0, 1)]),
        (
            "a[**\u{301}b**](target)c",
            "a\u{301}bc",
            vec![(MarkKind::Bold, 0, 2), (MarkKind::Link, 0, 2)],
        ),
        (
            "👩**\u{200d}💻**!",
            "👩\u{200d}💻!",
            vec![(MarkKind::Bold, 0, 1)],
        ),
        ("🇺**🇸🇨**🇦", "🇺🇸🇨🇦", vec![(MarkKind::Bold, 0, 2)]),
        ("**🇺**🇸x", "🇺🇸x", vec![(MarkKind::Bold, 0, 1)]),
        (
            "a[[target|\u{301}]]z",
            "a\u{301}z",
            vec![(MarkKind::Link, 0, 1)],
        ),
    ] {
        let document = Parser::try_parse(input).unwrap();
        let block = document.blocks_in_order()[0];
        let text = block_text_seq(&block.kind).unwrap();
        assert_eq!(paragraph_visible_string(text), visible, "{input}");
        let mut actual: Vec<_> = block
            .marks
            .resolved_intervals_in_sequence(text)
            .into_iter()
            .map(|(interval, start, end)| (interval.kind.clone(), start, end))
            .collect();
        actual.sort();
        assert_eq!(actual, expected, "{input}");
        let structural = document.serialize(EquivalenceMode::Structural);
        let reparsed = Parser::try_parse(&structural).unwrap();
        let reparsed_block = reparsed.blocks_in_order()[0];
        assert_eq!(
            paragraph_visible_string(block_text_seq(&reparsed_block.kind).unwrap()),
            visible,
            "structural serialization changed visible text: {input}"
        );
    }
}

#[test]
fn same_position_nesting_preserves_delimiters_and_snapshot_roundtrip() {
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, "abcdef").unwrap();
    let block = block_id_from_op(elem);
    session
        .set_mark(block, 0..6, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    session
        .set_mark(block, 0..6, MarkKind::Italic, attrs("delimiter", "_"))
        .unwrap();
    session
        .set_mark(block, 0..6, MarkKind::Link, attrs("href", "target"))
        .unwrap();
    let removed = session
        .set_mark(block, 0..6, MarkKind::Code, BTreeMap::new())
        .unwrap();
    session.remove_mark(block, removed).unwrap();
    let expected = "[**_abcdef_**](target)";
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        expected
    );
    let bytes = session.save_snapshot().unwrap().to_bytes().unwrap();
    let restored =
        CollaborativeDocument::restore_from_snapshot(SessionSnapshot::from_bytes(&bytes).unwrap())
            .unwrap();
    assert_eq!(
        restored.document().serialize(EquivalenceMode::Structural),
        expected
    );
    assert_eq!(restored.state_vector(), session.state_vector());
    assert_eq!(restored.peek_next_id(), session.peek_next_id());
}

#[test]
fn overlapping_links_choose_causal_winner_then_resume_outer_link() {
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, "abcdefgh").unwrap();
    let block = block_id_from_op(elem);
    session
        .set_mark(block, 0..8, MarkKind::Link, attrs("href", "outer"))
        .unwrap();
    let inner = session
        .set_mark(block, 2..6, MarkKind::Link, attrs("href", "inner"))
        .unwrap();
    let newest = session
        .set_mark(block, 4..8, MarkKind::Link, attrs("href", "newest"))
        .unwrap();
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        "[ab](outer)[cd](inner)[efgh](newest)"
    );
    session.remove_mark(block, newest).unwrap();
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        "[ab](outer)[cdef](inner)[gh](outer)"
    );
    session.remove_mark(block, inner).unwrap();
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        "[abcdefgh](outer)"
    );
}

#[test]
fn crossing_nonlink_marks_close_and_reopen_only_the_changed_suffix() {
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, "abcdef").unwrap();
    let block = block_id_from_op(elem);
    session
        .set_mark(block, 0..4, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    session
        .set_mark(block, 2..6, MarkKind::Italic, attrs("delimiter", "_"))
        .unwrap();
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        "**ab_cd_**_ef_"
    );
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyMarkSet {
    intervals: Vec<MarkInterval>,
    removes: Vec<(OpId, RemoveMark)>,
}

#[test]
fn mark_serialization_interoperates_with_existing_sequence_fields() {
    let interval = MarkInterval {
        id: op(10),
        kind: MarkKind::Link,
        start: Anchor {
            elem_id: op(1),
            bias: AnchorBias::Before,
        },
        end: Anchor {
            elem_id: op(3),
            bias: AnchorBias::After,
        },
        attrs: BTreeMap::from([(
            "href".into(),
            LwwRegister::new(MarkValue::String("target".repeat(64)), op(11)),
        )]),
        op_id: op(11),
    };
    let mut observed = StateVector::new();
    observed.set(1, 10);
    let legacy = LegacyMarkSet {
        intervals: vec![interval],
        removes: vec![(
            op(10),
            RemoveMark {
                observed,
                op_id: op(12),
            },
        )],
    };
    let mark_set: MarkSet = serde_json::from_slice(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(!mark_set.is_active(&op(10)));
    let decoded: LegacyMarkSet =
        serde_json::from_slice(&serde_json::to_vec(&mark_set).unwrap()).unwrap();
    assert_eq!(decoded, legacy);
    let binary_set: MarkSet =
        postcard::from_bytes(&postcard::to_allocvec(&legacy).unwrap()).unwrap();
    assert_eq!(binary_set, mark_set);
    let binary_legacy: LegacyMarkSet =
        postcard::from_bytes(&postcard::to_allocvec(&binary_set).unwrap()).unwrap();
    assert_eq!(binary_legacy, legacy);
}

#[test]
fn sparse_spans_preserve_overlaps_zero_width_removal_and_deleted_anchors() {
    let mut sequence = Sequence::new();
    for counter in 1..=6 {
        sequence.insert((counter > 1).then(|| op(counter - 1)), counter, op(counter));
    }
    let mut marks = MarkSet::new();
    let before = |counter| Anchor {
        elem_id: op(counter),
        bias: AnchorBias::Before,
    };
    let after = |counter| Anchor {
        elem_id: op(counter),
        bias: AnchorBias::After,
    };
    marks.set_mark(
        op(10),
        MarkKind::Bold,
        before(2),
        after(5),
        BTreeMap::new(),
        op(10),
    );
    marks.set_mark(
        op(11),
        MarkKind::Italic,
        before(2),
        after(3),
        BTreeMap::new(),
        op(11),
    );
    marks.set_mark(
        op(12),
        MarkKind::Code,
        before(4),
        before(4),
        BTreeMap::new(),
        op(12),
    );
    marks.set_mark(
        op(13),
        MarkKind::Link,
        before(1),
        after(6),
        BTreeMap::new(),
        op(13),
    );
    let mut observed = StateVector::new();
    observed.set(1, 13);
    marks.remove_mark(op(13), observed, op(14));
    let expected = vec![
        Span {
            start: 0,
            end: 1,
            marks: vec![],
        },
        Span {
            start: 1,
            end: 3,
            marks: vec![op(10), op(11)],
        },
        Span {
            start: 3,
            end: 5,
            marks: vec![op(10)],
        },
        Span {
            start: 5,
            end: 6,
            marks: vec![],
        },
    ];
    assert_eq!(marks.render_spans_in_sequence(&sequence), expected);
    sequence.delete(op(2), op(20));
    let spans = marks.render_spans_in_sequence(&sequence);
    assert_eq!(
        spans.first().unwrap(),
        &Span {
            start: 0,
            end: 1,
            marks: vec![]
        }
    );
    assert_eq!(
        spans.last().unwrap(),
        &Span {
            start: 4,
            end: 5,
            marks: vec![]
        }
    );
    assert_eq!(
        spans[1],
        Span {
            start: 1,
            end: 2,
            marks: vec![op(10), op(11)]
        }
    );
    assert_eq!(
        spans[2],
        Span {
            start: 2,
            end: 4,
            marks: vec![op(10)]
        }
    );
}
