//! Obsidian-style wikilinks parse to Link marks and round-trip to their source.
//!
//! `[[target]]`, `[[target|alias]]` and their `![[…]]` embed forms carry the
//! target in visible text or in the opening delimiter rather than in a trailing
//! `(href)`, so the mark stores the form in its `delimiter` attribute and
//! serialization rebuilds from that — the same mechanism Bold, Italic and Code
//! already use.
//!
//! The negatives matter as much: nothing inside a fence, inside inline code, or
//! escaped may become a link.

use md_crdt::core::mark::{MarkKind, MarkValue};
use md_crdt::doc::{BlockKind, EquivalenceMode, Parser, paragraph_visible_string};

fn paragraph(input: &str) -> (String, Vec<(String, String)>) {
    let document = Parser::parse(input);
    let blocks = document.blocks_in_order();
    let block = blocks
        .iter()
        .find(|block| matches!(block.kind, BlockKind::Paragraph { .. }))
        .expect("a paragraph");
    let BlockKind::Paragraph { text } = &block.kind else {
        unreachable!("filtered to paragraphs")
    };
    let links = block
        .marks
        .iter_active_intervals()
        .filter(|mark| mark.kind == MarkKind::Link)
        .map(|mark| {
            let href = match mark.attrs.get("href").map(|value| value.get()) {
                Some(MarkValue::String(value)) => value,
                _ => String::new(),
            };
            let delimiter = match mark.attrs.get("delimiter").map(|value| value.get()) {
                Some(MarkValue::String(value)) => value,
                _ => String::new(),
            };
            (href, delimiter)
        })
        .collect();
    (paragraph_visible_string(text), links)
}

fn round_trips(input: &str) {
    let document = Parser::parse(input);
    assert_eq!(
        document.serialize(EquivalenceMode::Exact),
        input,
        "exact round trip changed: {input:?}"
    );
    assert_eq!(
        document.serialize(EquivalenceMode::Structural),
        input.trim_end_matches('\n'),
        "structural round trip changed: {input:?}"
    );
}

#[test]
fn a_bare_wikilink_targets_its_own_visible_text() {
    let (text, links) = paragraph("A bare link to [[note-a]] here.\n");
    assert_eq!(text, "A bare link to note-a here.");
    assert_eq!(links, vec![("note-a".to_string(), "[[]]".to_string())]);
}

#[test]
fn an_aliased_wikilink_shows_the_alias_and_targets_the_file() {
    let (text, links) = paragraph("See [[note-b|the second note]] here.\n");
    assert_eq!(text, "See the second note here.");
    assert_eq!(links, vec![("note-b".to_string(), "[[|]]".to_string())]);
}

#[test]
fn a_heading_fragment_stays_part_of_the_target() {
    let (text, links) = paragraph("Into [[note-c#Third Section]] here.\n");
    assert_eq!(text, "Into note-c#Third Section here.");
    assert_eq!(
        links,
        vec![("note-c#Third Section".to_string(), "[[]]".to_string())]
    );
}

#[test]
fn a_block_reference_stays_part_of_the_target() {
    let (_, links) = paragraph("Into [[note-c#^anchor-one]] here.\n");
    assert_eq!(
        links,
        vec![("note-c#^anchor-one".to_string(), "[[]]".to_string())]
    );
}

#[test]
fn a_subdirectory_target_is_kept_whole() {
    let (text, links) = paragraph("Daily [[daily/2026-08-06]] here.\n");
    assert_eq!(text, "Daily daily/2026-08-06 here.");
    assert_eq!(
        links,
        vec![("daily/2026-08-06".to_string(), "[[]]".to_string())]
    );
}

#[test]
fn an_embed_is_distinguished_from_a_link_by_its_delimiter() {
    let (text, links) = paragraph("![[note-a]]\n");
    assert_eq!(text, "note-a");
    assert_eq!(links, vec![("note-a".to_string(), "![[]]".to_string())]);
}

#[test]
fn an_aliased_embed_keeps_both_target_and_alias() {
    let (text, links) = paragraph("![[note-a|shown]]\n");
    assert_eq!(text, "shown");
    assert_eq!(links, vec![("note-a".to_string(), "![[|]]".to_string())]);
}

#[test]
fn a_markdown_link_on_the_same_line_is_unaffected() {
    let (text, links) = paragraph("Both [[note-a]] and [label](./note-a.md) here.\n");
    assert_eq!(text, "Both note-a and label here.");
    assert_eq!(
        links,
        vec![
            ("note-a".to_string(), "[[]]".to_string()),
            ("./note-a.md".to_string(), "[]()".to_string()),
        ]
    );
}

#[test]
fn a_wikilink_inside_inline_code_is_not_a_link() {
    let (text, links) = paragraph("Inline `[[note-a]]` stays literal.\n");
    assert_eq!(text, "Inline [[note-a]] stays literal.");
    assert!(links.is_empty(), "inline code must not produce a link");
}

#[test]
fn a_python_slice_inside_inline_code_is_not_a_link() {
    let (text, links) = paragraph("A slice `df[[\"close\", \"volume\"]]` is not a link.\n");
    assert_eq!(text, "A slice df[[\"close\", \"volume\"]] is not a link.");
    assert!(links.is_empty(), "a slice in code must not produce a link");
}

#[test]
fn an_escaped_wikilink_is_not_a_link() {
    let (text, links) = paragraph("An escaped \\[\\[note-a\\]\\] stays visible.\n");
    assert_eq!(text, "An escaped \\[\\[note-a\\]\\] stays visible.");
    assert!(links.is_empty(), "an escaped pair must not produce a link");
}

#[test]
fn a_wikilink_inside_a_fence_is_not_a_link() {
    let document = Parser::parse("# Doc\n\n```markdown\n[[note-a]] is literal.\n```\n");
    let has_link = document.blocks_in_order().iter().any(|block| {
        block
            .marks
            .iter_active_intervals()
            .any(|mark| mark.kind == MarkKind::Link)
    });
    assert!(!has_link, "fence content must not produce a link");
}

#[test]
fn an_unclosed_wikilink_stays_literal() {
    let (text, links) = paragraph("An unclosed [[note-a and then some text.\n");
    assert_eq!(text, "An unclosed [[note-a and then some text.");
    assert!(links.is_empty(), "an unclosed pair must not produce a link");
}

#[test]
fn every_wikilink_form_round_trips_to_its_source() {
    for input in [
        "A bare link to [[note-a]] here.\n",
        "See [[note-b|the second note]] here.\n",
        "Into [[note-c#Third Section]] here.\n",
        "Into [[note-c#^anchor-one]] here.\n",
        "Daily [[daily/2026-08-06]] here.\n",
        "![[note-a]]\n",
        "![[note-a|shown]]\n",
        "Both [[note-a]] and [label](./note-a.md) here.\n",
        "Inline `[[note-a]]` stays literal.\n",
        "An escaped \\[\\[note-a\\]\\] stays visible.\n",
        "Two [[note-a]] and [[note-b]] in one line.\n",
    ] {
        round_trips(input);
    }
}
