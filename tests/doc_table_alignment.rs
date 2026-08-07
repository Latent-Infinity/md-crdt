//! A column with no alignment marker must stay unaligned through a re-serialize.
//!
//! `---` and `:---` both used to parse to `ColumnAlignment::Left`, so any table
//! rebuilt from the model — which is what happens once a cell is edited —
//! emitted `---` for a column the author had written as `:---`, silently
//! dropping an explicit left alignment.

use md_crdt::doc::{ColumnAlignment, EquivalenceMode, Parser};

const TABLE: &str =
    "| Default | Left | Center | Right |\n| --- | :--- | :---: | ---: |\n| a | b | c | d |\n";

#[test]
fn all_four_delimiter_forms_survive_a_structural_reserialize() {
    // Structural mode rebuilds from the model and does not re-add the document's
    // trailing newline, so compare without it; the delimiter row is the subject.
    let document = Parser::parse(TABLE);
    assert_eq!(
        document.serialize(EquivalenceMode::Structural),
        TABLE.trim_end_matches('\n')
    );
}

#[test]
fn all_four_delimiter_forms_survive_an_exact_reserialize() {
    let document = Parser::parse(TABLE);
    assert_eq!(document.serialize(EquivalenceMode::Exact), TABLE);
}

#[test]
fn an_unmarked_column_parses_as_default_not_left() {
    let document = Parser::parse(TABLE);
    let blocks = document.blocks_in_order();
    let table = blocks
        .iter()
        .find_map(|block| match &block.kind {
            md_crdt::doc::BlockKind::Table { table } => Some(table),
            _ => None,
        })
        .expect("a table");
    let alignments: Vec<ColumnAlignment> = table
        .columns_in_order()
        .iter()
        .map(|column| column.alignment.get_ref().clone())
        .collect();
    assert_eq!(
        alignments,
        vec![
            ColumnAlignment::Default,
            ColumnAlignment::Left,
            ColumnAlignment::Center,
            ColumnAlignment::Right,
        ]
    );
}
