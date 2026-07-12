use md_crdt::core::OpId;
use md_crdt::doc::{
    BlockKind, CellContent, ColumnAlignment, ColumnDef, EquivalenceMode, Parser, Table,
};
use uuid::Uuid;

fn op(counter: u64) -> OpId {
    OpId { counter, peer: 1 }
}

#[test]
fn fr15_table_row_sequence_ordering() {
    let mut table = Table::new(
        Uuid::new_v4(),
        op(1),
        vec![ColumnDef {
            alignment: ColumnAlignment::Left,
        }],
        vec!["h".to_string()],
        op(1),
    );

    table.insert_row(None, vec!["row-a".to_string()], op(2));
    table.insert_row(None, vec!["row-b".to_string()], op(3));

    let rows = table.rows_in_order();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].cells.get(), vec!["row-b".to_string()]);
    assert_eq!(rows[1].cells.get(), vec!["row-a".to_string()]);
}

#[test]
fn fr15_row_level_lww_cells() {
    let mut table = Table::new(
        Uuid::new_v4(),
        op(1),
        vec![ColumnDef {
            alignment: ColumnAlignment::Left,
        }],
        vec!["h".to_string()],
        op(1),
    );

    table.insert_row(None, vec!["one".to_string()], op(2));
    let row_id = table.rows_in_order()[0].elem_id;

    table.set_row_cells(row_id, vec!["old".to_string()], op(3));
    table.set_row_cells(row_id, vec!["new".to_string()], op(4));

    let rows = table.rows_in_order();
    assert_eq!(rows[0].cells.get(), vec!["new".to_string()]);
}

#[test]
fn fr15_concurrent_row_insert_delete() {
    let mut table = Table::new(
        Uuid::new_v4(),
        op(1),
        vec![ColumnDef {
            alignment: ColumnAlignment::Left,
        }],
        vec!["h".to_string()],
        op(1),
    );

    table.insert_row(None, vec!["row".to_string()], op(2));
    let row_id = table.rows_in_order()[0].elem_id;
    table.remove_row(row_id, op(3));

    let rows = table.rows_in_order();
    assert_eq!(rows.len(), 0);
}

#[test]
fn fr15_cell_content_is_text() {
    let mut table = Table::new(
        Uuid::new_v4(),
        op(1),
        vec![ColumnDef {
            alignment: ColumnAlignment::Left,
        }],
        vec!["h".to_string()],
        op(1),
    );

    let text: CellContent = "unicode ✓".to_string();
    table.insert_row(None, vec![text.clone()], op(2));
    let rows = table.rows_in_order();
    assert_eq!(rows[0].cells.get(), vec![text]);
}

#[test]
fn parser_emits_gfm_table_with_alignment_and_rows() {
    let input =
        "| Name | Score | Rank |\n| :--- | ---: | :---: |\n| Alice | 10 | 1 |\n| Bob | 8 | 2 |";
    let document = Parser::parse(input);
    let blocks = document.blocks_in_order();
    let BlockKind::Table { table } = &blocks[0].kind else {
        panic!("expected a structured table");
    };

    assert_eq!(table.header.get(), vec!["Name", "Score", "Rank"]);
    assert_eq!(
        table
            .columns
            .get()
            .into_iter()
            .map(|column| column.alignment)
            .collect::<Vec<_>>(),
        vec![
            ColumnAlignment::Left,
            ColumnAlignment::Right,
            ColumnAlignment::Center
        ]
    );
    assert_eq!(
        table
            .rows_in_order()
            .into_iter()
            .map(|row| row.cells.get())
            .collect::<Vec<_>>(),
        vec![vec!["Alice", "10", "1"], vec!["Bob", "8", "2"]]
    );

    let normalized = document.serialize(EquivalenceMode::Structural);
    assert_eq!(
        normalized,
        Parser::parse(&normalized).serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn invalid_table_delimiter_stays_a_paragraph() {
    let document = Parser::parse("| A | B |\n| -- | --- |");
    assert!(matches!(
        document.blocks_in_order()[0].kind,
        BlockKind::Paragraph { .. }
    ));
}
