use md_crdt::core::OpId;
use md_crdt::doc::{
    BlockKind, CellContent, ColumnAlignment, EquivalenceMode, Parser, Table, block_id_from_op,
};

fn op(counter: u64) -> OpId {
    OpId { counter, peer: 1 }
}

fn one_column_table() -> (Table, uuid::Uuid) {
    let mut table = Table::new(block_id_from_op(op(1)), op(1), op(1));
    table.insert_column(None, ColumnAlignment::Left, "h".into(), op(2));
    (table, block_id_from_op(op(2)))
}

#[test]
fn table_row_sequence_ordering() {
    let (mut table, column_id) = one_column_table();

    table.insert_row(None, vec![(column_id, "row-a".to_string())], op(3));
    table.insert_row(None, vec![(column_id, "row-b".to_string())], op(4));

    let rows = table.rows_in_order();
    assert_eq!(rows.len(), 2);
    assert_eq!(table.row_cells(rows[0].id), vec!["row-b".to_string()]);
    assert_eq!(table.row_cells(rows[1].id), vec!["row-a".to_string()]);
}

#[test]
fn cell_level_lww_updates() {
    let (mut table, column_id) = one_column_table();

    table.insert_row(None, vec![(column_id, "one".to_string())], op(3));
    let row_id = table.rows_in_order()[0].id;

    table.set_cell(row_id, column_id, "old".to_string(), op(4));
    table.set_cell(row_id, column_id, "new".to_string(), op(5));

    assert_eq!(table.row_cells(row_id), vec!["new".to_string()]);
}

#[test]
fn concurrent_row_insert_delete() {
    let (mut table, column_id) = one_column_table();

    table.insert_row(None, vec![(column_id, "row".to_string())], op(3));
    let row_id = table.rows_in_order()[0].elem_id;
    table.remove_row(row_id, op(4));

    let rows = table.rows_in_order();
    assert_eq!(rows.len(), 0);
}

#[test]
fn cell_content_is_text() {
    let (mut table, column_id) = one_column_table();

    let text: CellContent = "unicode ✓".to_string();
    table.insert_row(None, vec![(column_id, text.clone())], op(3));
    let rows = table.rows_in_order();
    assert_eq!(table.row_cells(rows[0].id), vec![text]);
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

    assert_eq!(
        table.row_cells(table.header_row_id()),
        vec!["Name", "Score", "Rank"]
    );
    assert_eq!(
        table
            .columns_in_order()
            .into_iter()
            .map(|column| column.alignment.get())
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
            .map(|row| table.row_cells(row.id))
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

#[test]
fn escaped_pipes_and_backslashes_round_trip_as_cell_content() {
    let input = "| a\\|b | c\\\\d |\n| --- | --- |\n| x\\|y | z\\\\w |";
    let document = Parser::parse(input);
    let BlockKind::Table { table } = &document.blocks_in_order()[0].kind else {
        panic!("expected a structured table")
    };
    assert_eq!(table.row_cells(table.header_row_id()), vec!["a|b", "c\\d"]);
    let row = table.rows_in_order()[0].id;
    assert_eq!(table.row_cells(row), vec!["x|y", "z\\w"]);

    let rendered = document.serialize(EquivalenceMode::Structural);
    assert_eq!(rendered, input);
    assert_eq!(
        Parser::parse(&rendered).serialize(EquivalenceMode::Structural),
        rendered
    );
}

#[test]
fn escaped_pipe_at_end_of_a_table_line_is_cell_content() {
    let input = "| first | second\\|\n| --- | ---\n| x | y\\|";
    let document = Parser::parse(input);
    let BlockKind::Table { table } = &document.blocks_in_order()[0].kind else {
        panic!("expected a structured table")
    };
    assert_eq!(
        table.row_cells(table.header_row_id()),
        vec!["first", "second|"]
    );
    let row = table.rows_in_order()[0].id;
    assert_eq!(table.row_cells(row), vec!["x", "y|"]);
}
