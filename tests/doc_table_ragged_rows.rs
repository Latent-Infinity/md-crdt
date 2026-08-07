//! A row carrying more cells than the header declares keeps them.
//!
//! Cells are keyed by column identity, so zipping a row onto the declared
//! columns dropped any surplus at parse. That loss was invisible until a table
//! was edited: an untouched document replays its source regions and still wrote
//! the cell out, while changing any one cell rebuilt the table without it.

use md_crdt::doc::{BlockKind, ColumnAlignment, EquivalenceMode, Parser};

const RAGGED: &str = "| A | B |\n| --- | --- |\n| one | two | THREE |\n";

fn table_of(input: &str) -> (Vec<ColumnAlignment>, Vec<Vec<String>>) {
    let document = Parser::parse(input);
    let blocks = document.blocks_in_order();
    let BlockKind::Table { table } = &blocks[0].kind else {
        panic!("expected a table");
    };
    let alignments = table
        .columns_in_order()
        .iter()
        .map(|column| column.alignment.get_ref().clone())
        .collect();
    let rows = table
        .rows_in_order()
        .into_iter()
        .map(|row| table.row_cells(row.id))
        .collect();
    (alignments, rows)
}

#[test]
fn a_surplus_cell_is_kept_rather_than_dropped_at_parse() {
    let (alignments, rows) = table_of(RAGGED);
    assert_eq!(
        alignments.len(),
        3,
        "the table is as wide as its widest row, so no cell is orphaned"
    );
    assert_eq!(
        alignments[2],
        ColumnAlignment::Default,
        "a column the delimiter row never described is unaligned"
    );
    assert_eq!(rows, vec![vec!["one", "two", "THREE"]]);
}

#[test]
fn a_surplus_cell_survives_a_structural_reserialize() {
    let rebuilt = Parser::parse(RAGGED).serialize(EquivalenceMode::Structural);
    assert!(
        rebuilt.contains("THREE"),
        "editing any cell rebuilds the table; the surplus must still be there: {rebuilt}"
    );
    assert_eq!(
        rebuilt, "| A | B |  |\n| --- | --- | --- |\n| one | two | THREE |",
        "the widened column carries the empty header the author left blank"
    );
}

#[test]
fn an_untouched_ragged_table_still_round_trips_exactly() {
    assert_eq!(
        Parser::parse(RAGGED).serialize(EquivalenceMode::Exact),
        RAGGED
    );
}

#[test]
fn a_short_row_is_still_padded_rather_than_widening_the_table() {
    // The control: fewer cells than columns must not change the table's width.
    let short = "| A | B | C |\n| --- | --- | --- |\n| one | two |\n";
    let (alignments, rows) = table_of(short);
    assert_eq!(alignments.len(), 3, "a short row does not narrow the table");
    assert_eq!(rows, vec![vec!["one", "two", ""]]);
    assert_eq!(
        Parser::parse(short).serialize(EquivalenceMode::Exact),
        short
    );
}
