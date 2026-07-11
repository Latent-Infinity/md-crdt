use md_crdt::doc::{BlockKind, EquivalenceMode, Parser, paragraph_visible_string};

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
    let BlockKind::List { ordered, items } = &blocks[0].kind else {
        panic!("expected a structured list");
    };
    assert!(!ordered);
    let items: Vec<_> = items.iter_asc().collect();
    assert_eq!(items.len(), 2);
    assert!(
        items[0]
            .children
            .iter_asc()
            .any(|block| matches!(block.kind, BlockKind::List { ordered: true, .. }))
    );
}

#[test]
fn multiline_and_loose_list_items_normalize_once() {
    assert_structural_idempotence("- one\n  continued");
    assert_structural_idempotence("- one\n\n  second paragraph\n\n- two");
    assert_structural_idempotence("  - one\n\n\tcontinued");
}

#[test]
fn fenced_code_adjacent_to_list_normalizes_once() {
    assert_structural_idempotence("- item\n\n```\ncode\n```\n\nafter");
}
