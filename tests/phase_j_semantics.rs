use md_crdt::core::mark::{MarkKind, MarkValue};
use md_crdt::core::{OpId, StateVector};
use md_crdt::doc::{
    BlockKind, EquivalenceMode, Frontmatter, FrontmatterError, Parser, block_id_from_op,
    paragraph_visible_ids, paragraph_visible_string,
};
use md_crdt::session::CollaborativeDocument;
use md_crdt::sync::ValidationLimits;
use std::collections::BTreeMap;

fn exchange(from: &CollaborativeDocument, to: &mut CollaborativeDocument, since: &StateVector) {
    to.apply_remote(
        from.encode_changes_since(since).unwrap(),
        &ValidationLimits::default(),
    )
    .unwrap();
}

#[test]
fn parser_uses_semantic_text_and_causal_inline_marks() {
    let doc = Parser::parse("**bold** and [link](https://example.com)");
    let block = doc.blocks_in_order()[0];
    let BlockKind::Paragraph { text } = &block.kind else {
        panic!("expected paragraph");
    };
    assert_eq!(paragraph_visible_string(text), "bold and link");
    let kinds: Vec<_> = block
        .marks
        .iter_active_intervals()
        .map(|mark| mark.kind.clone())
        .collect();
    assert!(kinds.contains(&MarkKind::Bold));
    assert!(kinds.contains(&MarkKind::Link));
    let link = block
        .marks
        .iter_active_intervals()
        .find(|mark| mark.kind == MarkKind::Link)
        .unwrap();
    assert_eq!(
        link.attrs.get("href").unwrap().get(),
        MarkValue::String("https://example.com".into())
    );
    assert_eq!(
        doc.serialize(EquivalenceMode::Structural),
        "**bold** and [link](https://example.com)"
    );
}

#[test]
fn marks_exchange_and_survive_snapshot_restore() {
    let mut a = CollaborativeDocument::new(1);
    let elem = a.insert_paragraph(None, "hello").unwrap();
    let block_id = block_id_from_op(elem);
    let mark_id = a
        .set_mark(block_id, 0..5, MarkKind::Bold, BTreeMap::new())
        .unwrap();

    let mut b = CollaborativeDocument::new(2);
    exchange(&a, &mut b, &StateVector::new());
    assert_eq!(
        b.document().serialize(EquivalenceMode::Structural),
        "**hello**"
    );
    assert!(
        b.document()
            .find_block_by_id(block_id)
            .unwrap()
            .marks
            .is_active(&mark_id)
    );

    let restored =
        CollaborativeDocument::restore_from_snapshot(a.save_snapshot().unwrap()).unwrap();
    assert_eq!(
        restored.document().serialize(EquivalenceMode::Structural),
        "**hello**"
    );
    assert!(
        restored
            .document()
            .find_block_by_id(block_id)
            .unwrap()
            .marks
            .is_active(&mark_id)
    );
}

#[test]
fn frontmatter_field_edit_is_lossless_and_opaque_yaml_rejects_without_mutation() {
    let mut doc = Parser::parse("---\n# keep\ntitle: 'old' # note\ntags: [a, b]\n---\n\nbody\n");
    doc.set_frontmatter_field(
        "title".into(),
        Some("'new'".into()),
        OpId {
            peer: 7,
            counter: 1,
        },
    )
    .unwrap();
    assert_eq!(doc.frontmatter_field("title"), Some("'new'"));
    assert_eq!(
        doc.serialize(EquivalenceMode::Exact),
        "---\n# keep\ntitle: 'new' # note\ntags: [a, b]\n---\n\nbody\n"
    );

    let mut opaque = Parser::parse("---\nparent:\n  child: value\n---\nbody");
    let before = opaque.serialize(EquivalenceMode::Exact);
    assert_eq!(
        opaque.set_frontmatter_field(
            "parent".into(),
            Some("changed".into()),
            OpId {
                peer: 7,
                counter: 2
            },
        ),
        Err(FrontmatterError::Opaque)
    );
    assert_eq!(opaque.serialize(EquivalenceMode::Exact), before);
}

#[test]
fn frontmatter_fields_converge_by_per_key_lww() {
    let mut a = CollaborativeDocument::new(1);
    let mut b = CollaborativeDocument::new(2);
    a.set_frontmatter_field("title", Some("left".into()))
        .unwrap();
    b.set_frontmatter_field("title", Some("right".into()))
        .unwrap();
    let from_a = a.encode_changes_since(&StateVector::new()).unwrap();
    let from_b = b.encode_changes_since(&StateVector::new()).unwrap();
    a.apply_remote(from_b, &ValidationLimits::default())
        .unwrap();
    b.apply_remote(from_a, &ValidationLimits::default())
        .unwrap();
    assert_eq!(a.document(), b.document());
    assert_eq!(a.document().frontmatter_field("title"), Some("right"));
}

#[test]
fn frontmatter_initialization_new_key_delete_and_validation_exchange() {
    let mut a = CollaborativeDocument::new(1);
    let base = Frontmatter::parse("title: old\nquoted: \"x # y\" # keep".into());
    assert!(a.initialize_frontmatter(base).unwrap().is_some());
    assert!(
        a.initialize_frontmatter(Frontmatter::parse("ignored: yes".into()))
            .unwrap()
            .is_none()
    );
    a.set_frontmatter_field("added", Some("value".into()))
        .unwrap();
    a.set_frontmatter_field("title", None).unwrap();
    let before = a.peek_next_id();
    assert!(matches!(
        a.set_frontmatter_field("bad key", Some("no".into())),
        Err(md_crdt::session::SessionError::Frontmatter(
            FrontmatterError::InvalidKey
        ))
    ));
    assert_eq!(a.peek_next_id(), before);

    let mut b = CollaborativeDocument::new(2);
    exchange(&a, &mut b, &StateVector::new());
    assert_eq!(a.document(), b.document());
    assert_eq!(b.document().frontmatter_field("title"), None);
    assert_eq!(b.document().frontmatter_field("added"), Some("value"));
    let rendered = b.document().serialize(EquivalenceMode::Structural);
    assert!(rendered.contains("added: value"));
    assert!(rendered.contains("# keep"));

    for raw in ["dup: one\ndup: two", "body: |\n  text", "- item"] {
        assert!(!Frontmatter::parse(raw.into()).is_structured());
    }
}

#[test]
fn inline_mark_styles_and_remove_exchange() {
    let parsed = Parser::parse("*italic* and `code`");
    let block = parsed.blocks_in_order()[0];
    let kinds: Vec<_> = block
        .marks
        .iter_active_intervals()
        .map(|mark| mark.kind.clone())
        .collect();
    assert!(kinds.contains(&MarkKind::Italic));
    assert!(kinds.contains(&MarkKind::Code));

    let mut a = CollaborativeDocument::new(1);
    let elem = a.insert_paragraph(None, "styled").unwrap();
    let block_id = block_id_from_op(elem);
    let mut attrs = BTreeMap::new();
    attrs.insert("delimiter".into(), MarkValue::Bool(true));
    let mark = a.set_mark(block_id, 0..6, MarkKind::Bold, attrs).unwrap();
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        "**styled**"
    );
    a.remove_mark(block_id, mark).unwrap();
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        "styled"
    );
    let before = a.peek_next_id();
    assert!(matches!(
        a.remove_mark(
            block_id,
            OpId {
                peer: 99,
                counter: 99
            }
        ),
        Err(md_crdt::session::SessionError::InvalidOffset)
    ));
    assert_eq!(a.peek_next_id(), before);
    let mut link_attrs = BTreeMap::new();
    link_attrs.insert("href".into(), MarkValue::Bool(false));
    a.set_mark(block_id, 0..6, MarkKind::Link, link_attrs)
        .unwrap();
    a.set_mark(
        block_id,
        0..6,
        MarkKind::Custom("annotation".into()),
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        "[styled]()"
    );
    let _snapshot = a.save_snapshot().unwrap();
    let mut b = CollaborativeDocument::new(2);
    exchange(&a, &mut b, &StateVector::new());
    assert_eq!(a.document(), b.document());
}

#[test]
fn code_span_contents_do_not_acquire_nested_emphasis_marks() {
    let doc = Parser::parse("`code *is literal*` and **bold**");
    let block = doc.blocks_in_order()[0];
    let kinds: Vec<_> = block
        .marks
        .iter_active_intervals()
        .map(|mark| mark.kind.clone())
        .collect();
    assert_eq!(
        kinds.iter().filter(|kind| **kind == MarkKind::Code).count(),
        1
    );
    assert_eq!(
        kinds.iter().filter(|kind| **kind == MarkKind::Bold).count(),
        1
    );
    assert!(!kinds.contains(&MarkKind::Italic));
    assert_eq!(
        doc.serialize(EquivalenceMode::Structural),
        "`code *is literal*` and **bold**"
    );
}

#[test]
fn inline_parser_handles_adjacent_nesting_and_balanced_link_targets() {
    for (markdown, visible) in [
        ("***bold italic***", "bold italic"),
        ("**bold and *italic***", "bold and italic"),
        ("*italic and **bold***", "italic and bold"),
    ] {
        let doc = Parser::parse(markdown);
        assert_eq!(
            doc.serialize(EquivalenceMode::Structural),
            markdown,
            "inline structure changed for {markdown:?}"
        );
        let block = doc.blocks_in_order()[0];
        let BlockKind::Paragraph { text } = &block.kind else {
            panic!("expected paragraph");
        };
        assert_eq!(paragraph_visible_string(text), visible);
        let kinds: Vec<_> = block
            .marks
            .iter_active_intervals()
            .map(|mark| mark.kind.clone())
            .collect();
        assert!(kinds.contains(&MarkKind::Bold));
        assert!(kinds.contains(&MarkKind::Italic));
    }

    for markdown in ["[label](target_(v2).md)", r"[label](target_\(escaped\).md)"] {
        assert_eq!(
            Parser::parse(markdown).serialize(EquivalenceMode::Structural),
            markdown,
            "link structure changed for {markdown:?}"
        );
    }

    let doc = Parser::parse("[label](target_(v2).md)");
    let link = doc.blocks_in_order()[0]
        .marks
        .iter_active_intervals()
        .find(|mark| mark.kind == MarkKind::Link)
        .unwrap();
    assert_eq!(
        link.attrs.get("href").unwrap().get(),
        MarkValue::String("target_(v2).md".into())
    );
}

#[test]
fn crossing_inline_marks_serialize_as_equivalent_non_crossing_segments() {
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, "abcd").unwrap();
    let block_id = block_id_from_op(elem);
    session
        .set_mark(block_id, 0..3, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    session
        .set_mark(block_id, 1..4, MarkKind::Italic, BTreeMap::new())
        .unwrap();

    let rendered = session.document().serialize(EquivalenceMode::Structural);
    let reparsed = Parser::parse(&rendered);
    let block = reparsed.blocks_in_order()[0];
    let BlockKind::Paragraph { text } = &block.kind else {
        panic!("expected paragraph");
    };
    assert_eq!(paragraph_visible_string(text), "abcd");

    let ids = paragraph_visible_ids(text);
    let intervals = block.marks.resolved_intervals(&ids);
    let kinds_at = |index| {
        intervals
            .iter()
            .filter(|(_, start, end)| *start <= index && index < *end)
            .map(|(interval, _, _)| interval.kind.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds_at(0), vec![MarkKind::Bold]);
    assert!(kinds_at(1).contains(&MarkKind::Bold));
    assert!(kinds_at(1).contains(&MarkKind::Italic));
    assert!(kinds_at(2).contains(&MarkKind::Bold));
    assert!(kinds_at(2).contains(&MarkKind::Italic));
    assert_eq!(kinds_at(3), vec![MarkKind::Italic]);
    assert_eq!(
        reparsed.serialize(EquivalenceMode::Structural),
        rendered,
        "crossing-mark normalization must be stable"
    );
}

#[test]
fn overlapping_same_kind_marks_serialize_as_their_semantic_union() {
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, "abcd").unwrap();
    let block_id = block_id_from_op(elem);
    session
        .set_mark(block_id, 0..3, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    session
        .set_mark(block_id, 1..4, MarkKind::Bold, BTreeMap::new())
        .unwrap();

    let rendered = session.document().serialize(EquivalenceMode::Structural);
    assert_eq!(rendered, "**abcd**");
    let reparsed = Parser::parse(&rendered);
    let block = reparsed.blocks_in_order()[0];
    let BlockKind::Paragraph { text } = &block.kind else {
        panic!("expected paragraph");
    };
    assert_eq!(paragraph_visible_string(text), "abcd");
    assert_eq!(
        block
            .marks
            .resolved_intervals(&paragraph_visible_ids(text))
            .iter()
            .map(|(interval, start, end)| (interval.kind.clone(), *start, *end))
            .collect::<Vec<_>>(),
        vec![(MarkKind::Bold, 0, 4)]
    );
}

#[test]
fn conflicting_link_marks_serialize_with_newer_overlap_winning() {
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, "abcd").unwrap();
    let block_id = block_id_from_op(elem);
    let mut first = BTreeMap::new();
    first.insert("href".into(), MarkValue::String("first.md".into()));
    session
        .set_mark(block_id, 0..3, MarkKind::Link, first)
        .unwrap();
    let mut second = BTreeMap::new();
    second.insert("href".into(), MarkValue::String("second.md".into()));
    session
        .set_mark(block_id, 1..4, MarkKind::Link, second)
        .unwrap();

    let rendered = session.document().serialize(EquivalenceMode::Structural);
    assert_eq!(rendered, "[a](first.md)[bcd](second.md)");
    let reparsed = Parser::parse(&rendered);
    let block = reparsed.blocks_in_order()[0];
    let BlockKind::Paragraph { text } = &block.kind else {
        panic!("expected paragraph");
    };
    assert_eq!(paragraph_visible_string(text), "abcd");
    let intervals = block.marks.resolved_intervals(&paragraph_visible_ids(text));
    let href_at = |index| {
        intervals
            .iter()
            .find(|(interval, start, end)| {
                interval.kind == MarkKind::Link && *start <= index && index < *end
            })
            .and_then(|(interval, _, _)| interval.attrs.get("href"))
            .map(|value| value.get())
    };
    assert_eq!(href_at(0), Some(MarkValue::String("first.md".into())));
    for index in 1..4 {
        assert_eq!(href_at(index), Some(MarkValue::String("second.md".into())));
    }
}

#[test]
fn list_item_parent_and_container_membership_are_addressable() {
    let doc = Parser::parse("- child");
    let list = doc.blocks_in_order()[0];
    let BlockKind::List { items, .. } = &list.kind else {
        panic!("list expected")
    };
    let item = items.iter().next().unwrap();
    let child = item.children.iter().next().unwrap();
    assert_eq!(doc.block_parent(child.id), Some(Some(item.elem_id)));
    assert!(doc.block_contains_container(list.id, item.elem_id));
    assert!(doc.block_contains_container(list.id, child.elem_id));
}

#[test]
fn block_move_preserves_logical_content_and_concurrent_moves_converge() {
    let mut a = CollaborativeDocument::new(1);
    let one = a.insert_paragraph(None, "one").unwrap();
    let two = a.insert_paragraph(Some(one), "two").unwrap();
    let three = a.insert_paragraph(Some(two), "three").unwrap();
    let two_id = block_id_from_op(two);
    a.set_mark(two_id, 0..3, MarkKind::Italic, BTreeMap::new())
        .unwrap();
    let unit_ids = a
        .document()
        .find_block_by_id(two_id)
        .and_then(|block| match &block.kind {
            BlockKind::Paragraph { text } => Some(paragraph_visible_ids(text)),
            _ => None,
        })
        .unwrap();

    let mut b = CollaborativeDocument::new(2);
    exchange(&a, &mut b, &StateVector::new());
    let a_seen = a.state_vector();
    let b_seen = b.state_vector();
    a.move_block(two_id, None, Some(three)).unwrap();
    b.move_block(two_id, None, None).unwrap();
    exchange(&a, &mut b, &b_seen);
    exchange(&b, &mut a, &a_seen);
    assert_eq!(a.document(), b.document());
    let moved = a.document().find_block_by_id(two_id).unwrap();
    let BlockKind::Paragraph { text } = &moved.kind else {
        panic!("expected paragraph");
    };
    assert_eq!(paragraph_visible_ids(text), unit_ids);
    assert!(
        moved
            .marks
            .iter_active_intervals()
            .any(|mark| mark.kind == MarkKind::Italic)
    );
}

#[test]
fn section_move_is_atomic_and_preserves_all_block_ids() {
    let mut session = CollaborativeDocument::new(9);
    let heading = session
        .insert_block(
            None,
            BlockKind::heading(
                1,
                "",
                OpId {
                    peer: 0,
                    counter: 1,
                },
            ),
        )
        .unwrap();
    session
        .insert_text(block_id_from_op(heading), 0, "First")
        .unwrap();
    let child = session.insert_paragraph(Some(heading), "child").unwrap();
    let second = session
        .insert_block(
            Some(child),
            BlockKind::heading(
                1,
                "",
                OpId {
                    peer: 0,
                    counter: 1,
                },
            ),
        )
        .unwrap();
    session
        .insert_text(block_id_from_op(second), 0, "Second")
        .unwrap();
    let tail = session.insert_paragraph(Some(second), "tail").unwrap();
    let ids_before: Vec<_> = session
        .document()
        .blocks()
        .iter()
        .map(|block| block.id)
        .collect();
    session
        .move_section(block_id_from_op(heading), Some(tail))
        .unwrap();
    let ids_after: Vec<_> = session
        .document()
        .blocks()
        .iter()
        .map(|block| block.id)
        .collect();
    assert_eq!(
        ids_after,
        vec![ids_before[2], ids_before[3], ids_before[0], ids_before[1]]
    );
}

#[test]
fn move_validation_rejects_nonheading_missing_and_internal_anchors_without_clock_burn() {
    let mut session = CollaborativeDocument::new(17);
    let paragraph = session.insert_paragraph(None, "p").unwrap();
    let heading = session
        .insert_block(
            Some(paragraph),
            BlockKind::heading(
                1,
                "",
                OpId {
                    peer: 0,
                    counter: 1,
                },
            ),
        )
        .unwrap();
    session
        .insert_text(block_id_from_op(heading), 0, "h")
        .unwrap();
    let child = session.insert_paragraph(Some(heading), "child").unwrap();
    let before = session.peek_next_id();
    assert!(matches!(
        session.move_section(block_id_from_op(paragraph), None),
        Err(md_crdt::session::SessionError::InvalidMove)
    ));
    assert!(matches!(
        session.move_block(
            block_id_from_op(paragraph),
            None,
            Some(OpId {
                peer: 99,
                counter: 99
            })
        ),
        Err(md_crdt::session::SessionError::MissingAfterAnchor)
    ));
    assert!(matches!(
        session.move_section(block_id_from_op(heading), Some(child)),
        Err(md_crdt::session::SessionError::InvalidMove)
    ));
    assert_eq!(session.peek_next_id(), before);
}

#[test]
fn nested_move_preserves_identity_and_cycle_rejection_burns_no_clock() {
    let mut session = CollaborativeDocument::new(12);
    let quote = session
        .insert_block(
            None,
            BlockKind::BlockQuote {
                children: md_crdt::core::Sequence::new(),
            },
        )
        .unwrap();
    let paragraph = session.insert_paragraph(Some(quote), "nested").unwrap();
    let block_id = block_id_from_op(paragraph);
    let units_before = session
        .document()
        .find_block_by_id(block_id)
        .and_then(|block| match &block.kind {
            BlockKind::Paragraph { text } => Some(paragraph_visible_ids(text)),
            _ => None,
        })
        .unwrap();
    session.move_block(block_id, None, Some(quote)).unwrap();
    assert_eq!(session.document().block_parent(block_id), Some(None));
    let BlockKind::Paragraph { text } =
        &session.document().find_block_by_id(block_id).unwrap().kind
    else {
        panic!("paragraph expected")
    };
    assert_eq!(paragraph_visible_ids(text), units_before);

    let before = session.peek_next_id();
    assert!(matches!(
        session.move_block(block_id_from_op(quote), Some(quote), None),
        Err(md_crdt::session::SessionError::MoveCycle)
    ));
    assert_eq!(session.peek_next_id(), before);
}

#[test]
fn concurrent_move_and_delete_converge_with_delete_wins() {
    let mut a = CollaborativeDocument::new(1);
    let target = a.insert_paragraph(None, "gone").unwrap();
    let anchor = a.insert_paragraph(Some(target), "anchor").unwrap();
    let block_id = block_id_from_op(target);
    let mut b = CollaborativeDocument::new(2);
    exchange(&a, &mut b, &StateVector::new());
    let a_seen = a.state_vector();
    let b_seen = b.state_vector();
    a.move_block(block_id, None, Some(anchor)).unwrap();
    b.delete_block(target).unwrap();
    exchange(&a, &mut b, &b_seen);
    exchange(&b, &mut a, &a_seen);
    assert_eq!(a.document(), b.document());
    assert!(a.document().find_block_by_id(block_id).is_none());
}

#[test]
fn unicode_byte_and_grapheme_ranges_address_the_same_units() {
    let doc = Parser::parse("a🇺🇸e\u{301}z");
    let block = doc.blocks_in_order()[0];
    let byte_start = "a".len();
    let byte_end = "a🇺🇸e\u{301}".len();
    assert_eq!(
        doc.byte_range_to_anchors(block.id, byte_start..byte_end)
            .unwrap(),
        doc.grapheme_range_to_anchors(block.id, 1..3).unwrap()
    );
    assert!(doc.grapheme_range_to_anchors(block.id, 1..1).is_err());
    assert!(doc.grapheme_range_to_anchors(block.id, 1..99).is_err());
    assert!(doc.byte_range_to_anchors(block.id, 1..1).is_err());
    assert!(doc.byte_range_to_anchors(block.id, 1..999).is_err());
    assert!(doc.byte_range_to_anchors(block.id, 2..3).is_err());
}
