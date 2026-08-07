//! The public inline-link scanner, used for text that is not a parsed block.
//!
//! Table cells are stored as raw strings and never inline-parsed, so a caller
//! that wants their links needs the same definition of "link" the parser uses —
//! including code-span and escape handling — without a Block to hang marks on.

use md_crdt::doc::inline_links;

#[test]
fn a_markdown_link_reports_label_target_and_span() {
    let input = "a [label](./target.md) b";
    let links = inline_links(input);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].label, "label");
    assert_eq!(links[0].target, "./target.md");
    assert!(!links[0].wikilink);
    assert!(!links[0].embed);
    assert_eq!(&input[links[0].start..links[0].end], "[label](./target.md)");
}

#[test]
fn a_bare_wikilink_reports_itself_as_label_and_target() {
    let input = "a [[note-a]] b";
    let links = inline_links(input);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].label, "note-a");
    assert_eq!(links[0].target, "note-a");
    assert!(links[0].wikilink);
    assert!(!links[0].embed);
    assert_eq!(&input[links[0].start..links[0].end], "[[note-a]]");
}

#[test]
fn an_aliased_wikilink_separates_label_from_target() {
    let input = "a [[note-b|the second note]] b";
    let links = inline_links(input);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].label, "the second note");
    assert_eq!(links[0].target, "note-b");
    assert!(links[0].wikilink);
    assert_eq!(
        &input[links[0].start..links[0].end],
        "[[note-b|the second note]]"
    );
}

#[test]
fn an_embed_is_flagged() {
    let links = inline_links("![[note-a]]");
    assert_eq!(links.len(), 1);
    assert!(links[0].wikilink);
    assert!(links[0].embed);
    assert_eq!(links[0].target, "note-a");
}

#[test]
fn several_links_in_one_string_are_reported_in_order() {
    let input = "[one](./a.md) then [[note-b]] then [two](./c.md)";
    let links = inline_links(input);
    assert_eq!(
        links
            .iter()
            .map(|link| link.target.as_str())
            .collect::<Vec<_>>(),
        vec!["./a.md", "note-b", "./c.md"]
    );
    for link in &links {
        assert!(input[link.start..link.end].contains(&link.target));
    }
}

#[test]
fn links_inside_inline_code_are_not_reported() {
    assert!(inline_links("`[label](./target.md)`").is_empty());
    assert!(inline_links("`[[note-a]]`").is_empty());
    assert!(inline_links("a `df[[\"close\", \"volume\"]]` b").is_empty());
}

#[test]
fn an_escaped_wikilink_is_not_reported() {
    assert!(inline_links("\\[\\[note-a\\]\\]").is_empty());
}

#[test]
fn a_literal_bracket_before_a_link_does_not_extend_it() {
    let input = "As shown in [1] and a link [label](./target.md).";
    let links = inline_links(input);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].label, "label");
    assert_eq!(&input[links[0].start..links[0].end], "[label](./target.md)");
}

#[test]
fn an_escaped_pipe_in_a_table_cell_is_not_a_link() {
    assert!(inline_links("`a \\| b`").is_empty());
    assert!(inline_links("plain text with no link").is_empty());
}
