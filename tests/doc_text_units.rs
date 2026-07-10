//! Paragraph TextUnit representation (parse/serialize).

use md_crdt::core::OpId;
use md_crdt::doc::{
    BlockKind, Parser, paragraph_visible_ids, paragraph_visible_string, units_from_str_at,
};

#[test]
fn units_from_str_preserves_graphemes() {
    let seq = units_from_str_at(
        "a🇺🇸b",
        OpId {
            counter: 1,
            peer: 0,
        },
    );
    assert_eq!(paragraph_visible_string(&seq), "a🇺🇸b");
    assert_eq!(paragraph_visible_ids(&seq).len(), 3);
}

#[test]
fn parse_serialize_round_trip_uses_units() {
    let input = "Hello 🌍 world";
    let doc = Parser::parse(input);
    let block = doc.blocks_in_order()[0];
    match &block.kind {
        BlockKind::Paragraph { text } => {
            assert!(text.len_visible() >= 3);
            assert_eq!(paragraph_visible_string(text), input);
            // Unit OpIds are sequential peer-0 after the block elem id.
            let ids = paragraph_visible_ids(text);
            assert!(ids.windows(2).all(|w| w[1].counter > w[0].counter));
        }
        _ => panic!("expected paragraph"),
    }
    let out = doc.serialize(md_crdt::doc::EquivalenceMode::Structural);
    assert_eq!(out, input);
}

#[test]
fn paragraph_helper_matches_units() {
    let kind = BlockKind::paragraph(
        "xy",
        OpId {
            counter: 5,
            peer: 2,
        },
    );
    match kind {
        BlockKind::Paragraph { text } => {
            let ids = paragraph_visible_ids(&text);
            assert_eq!(
                ids[0],
                OpId {
                    counter: 5,
                    peer: 2
                }
            );
            assert_eq!(
                ids[1],
                OpId {
                    counter: 6,
                    peer: 2
                }
            );
            assert_eq!(
                text.iter().map(|u| u.grapheme.as_str()).collect::<Vec<_>>(),
                vec!["x", "y"]
            );
        }
        _ => unreachable!(),
    }
}
