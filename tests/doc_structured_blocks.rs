use md_crdt::doc::{BlockKind, EquivalenceMode, Parser, paragraph_visible_string};
use md_crdt::{BlockDraft, CollaborativeDocument, ListItemDraft, ListStyle};

fn assert_structural_idempotence(input: &str) {
    let first = Parser::parse(input).serialize(EquivalenceMode::Structural);
    let second = Parser::parse(&first).serialize(EquivalenceMode::Structural);
    assert_eq!(first, second, "first normalization was not stable");
}

#[test]
fn parses_atx_and_setext_headings_as_structured_blocks() {
    let doc = Parser::parse("## ATX\n\nSetext\n===");
    let blocks = doc.blocks_in_order();

    let BlockKind::Heading { level, text } = &blocks[0].kind else {
        panic!("expected an ATX heading");
    };
    assert_eq!(*level, 2);
    assert_eq!(paragraph_visible_string(text), "ATX");

    let BlockKind::Heading { level, text } = &blocks[1].kind else {
        panic!("expected a setext heading");
    };
    assert_eq!(*level, 1);
    assert_eq!(paragraph_visible_string(text), "Setext");
}

#[test]
fn atx_marker_requires_whitespace_before_content() {
    let doc = Parser::parse("#hashtag");
    assert!(matches!(
        doc.blocks_in_order()[0].kind,
        BlockKind::Paragraph { .. }
    ));
}

#[test]
fn parses_nested_lists_as_list_item_children() {
    let doc = Parser::parse("- parent\n  1. child\n- sibling");
    let blocks = doc.blocks_in_order();
    let BlockKind::List { style, items, .. } = &blocks[0].kind else {
        panic!("expected a structured list");
    };
    assert!(!style.ordered);
    let items: Vec<_> = items.iter_asc().collect();
    assert_eq!(items.len(), 2);
    assert!(
        items[0]
            .children
            .iter_asc()
            .any(|block| matches!(block.kind, BlockKind::List { style, .. } if style.ordered))
    );
}

#[test]
fn multiline_and_loose_list_items_normalize_once() {
    assert_structural_idempotence("- one\n  continued");
    assert_structural_idempotence("- one\n\n  second paragraph\n\n- two");
    assert_structural_idempotence("  - one\n\n\tcontinued");
    assert_structural_idempotence("999999999. one\n999999999. two");
}

#[test]
fn loose_paragraph_after_nested_list_remains_a_distinct_child() {
    let markdown =
        "- parent **bold**\n\n  - nested *italic*\n\n  trailing [link](after.md)\n\n- next";
    let doc = Parser::parse(markdown);
    let BlockKind::List { items, .. } = &doc.blocks_in_order()[0].kind else {
        panic!("expected a structured list");
    };
    let children: Vec<_> = items.iter().next().unwrap().children.iter().collect();
    assert_eq!(children.len(), 3);
    assert!(matches!(children[0].kind, BlockKind::Paragraph { .. }));
    assert!(matches!(children[1].kind, BlockKind::List { .. }));
    assert!(matches!(children[2].kind, BlockKind::Paragraph { .. }));

    let rendered = doc.serialize(EquivalenceMode::Structural);
    assert_eq!(
        rendered,
        "- parent **bold**\n  - nested *italic*\n\n  trailing [link](after.md)\n\n- next"
    );
    assert_structural_idempotence(&rendered);
}

#[test]
fn fenced_code_adjacent_to_list_normalizes_once() {
    assert_structural_idempotence("- item\n\n```\ncode\n```\n\nafter");
}

#[test]
fn nested_list_as_first_item_child_preserves_the_outer_item() {
    let mut session = CollaborativeDocument::new(1);
    session
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::List {
                        style: ListStyle::default(),
                        items: vec![ListItemDraft {
                            task: None,
                            children: vec![BlockDraft::Paragraph {
                                text: "nested".into(),
                            }],
                        }],
                    }],
                }],
            },
            Default::default(),
        )
        .unwrap();

    let rendered = session.document().serialize(EquivalenceMode::Structural);
    assert_eq!(rendered, "-\n  - nested");
    let reparsed = Parser::parse(&rendered);
    let BlockKind::List { items, .. } = &reparsed.blocks_in_order()[0].kind else {
        panic!("expected outer list")
    };
    let children: Vec<_> = items.iter().next().unwrap().children.iter().collect();
    assert_eq!(children.len(), 1);
    assert!(matches!(children[0].kind, BlockKind::List { .. }));
}
