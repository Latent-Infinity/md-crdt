//! A literal `[` before an inline link must not be swallowed by that link.
//!
//! The inline parser used to locate a link's label end with `rest.find("](")`,
//! which scans the whole remainder of the line. Any earlier literal bracket —
//! a citation `[1]`, a subscript `x[i]`, a wikilink — therefore became the
//! link's opening delimiter and everything up to the real link's target was
//! absorbed into the label.

use md_crdt::doc::{BlockKind, EquivalenceMode, Parser, paragraph_visible_string};

fn visible(input: &str) -> String {
    let document = Parser::parse(input);
    let blocks = document.blocks_in_order();
    let block = blocks
        .iter()
        .find(|block| matches!(block.kind, BlockKind::Paragraph { .. }))
        .expect("a paragraph");
    match &block.kind {
        BlockKind::Paragraph { text } => paragraph_visible_string(text),
        _ => unreachable!("filtered to paragraphs"),
    }
}

#[test]
fn link_alone_renders_its_label() {
    assert_eq!(
        visible("A link [label](./target.md) and trailing text.\n"),
        "A link label and trailing text."
    );
}

#[test]
fn matched_literal_bracket_before_a_link_survives() {
    assert_eq!(
        visible("A literal [bracket] and a link [label](./target.md) and trailing text.\n"),
        "A literal [bracket] and a link label and trailing text."
    );
}

#[test]
fn unmatched_bracket_before_a_link_survives() {
    assert_eq!(
        visible("An unmatched [ bracket and a link [label](./target.md) and trailing.\n"),
        "An unmatched [ bracket and a link label and trailing."
    );
}

#[test]
fn subscript_before_a_link_survives() {
    assert_eq!(
        visible("Code like x[i] and a link [label](./target.md) and trailing text.\n"),
        "Code like x[i] and a link label and trailing text."
    );
}

#[test]
fn citation_marker_before_a_link_survives() {
    assert_eq!(
        visible("As shown in [1] and a link [label](./target.md) and trailing text.\n"),
        "As shown in [1] and a link label and trailing text."
    );
}

#[test]
fn wikilink_before_a_link_survives() {
    // The wikilink is now a link in its own right, so it renders as its target
    // rather than staying literal. What matters here is unchanged: the markdown
    // link that follows is parsed on its own and neither swallows the other.
    assert_eq!(
        visible("A wikilink [[note-a]] and a link [label](./target.md) and trailing.\n"),
        "A wikilink note-a and a link label and trailing."
    );
}

#[test]
fn a_bracket_after_the_link_was_already_correct() {
    assert_eq!(
        visible("A link [label](./target.md) then a literal [bracket] and trailing.\n"),
        "A link label then a literal [bracket] and trailing."
    );
}

#[test]
fn nested_brackets_inside_a_link_label_still_parse_as_one_link() {
    assert_eq!(
        visible("A link [label [inner] more](./target.md) and trailing text.\n"),
        "A link label [inner] more and trailing text."
    );
}

#[test]
fn escaped_bracket_before_a_link_survives() {
    assert_eq!(
        visible("An escaped \\[bracket\\] and a link [label](./target.md) and trailing.\n"),
        "An escaped \\[bracket\\] and a link label and trailing."
    );
}

#[test]
fn brackets_before_a_link_round_trip_exactly() {
    for input in [
        "As shown in [1] and a link [label](./target.md) and trailing text.\n",
        "Code like x[i] and a link [label](./target.md) and trailing text.\n",
        "A wikilink [[note-a]] and a link [label](./target.md) and trailing.\n",
        "A literal [bracket] and a link [label](./target.md) and trailing text.\n",
    ] {
        let document = Parser::parse(input);
        assert_eq!(
            document.serialize(EquivalenceMode::Exact),
            input,
            "round trip changed: {input:?}"
        );
    }
}
