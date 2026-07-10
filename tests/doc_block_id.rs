//! Deterministic BlockId derivation from OpId and parse stability.

use md_crdt::core::OpId;
use md_crdt::doc::{Block, BlockKind, Parser, block_id_from_op};

fn op(counter: u64, peer: u64) -> OpId {
    OpId { counter, peer }
}

#[test]
fn block_id_from_op_is_deterministic() {
    let a = block_id_from_op(op(1, 42));
    let b = block_id_from_op(op(1, 42));
    assert_eq!(a, b);
}

#[test]
fn block_id_from_op_differs_when_counter_or_peer_differs() {
    let base = block_id_from_op(op(1, 1));
    assert_ne!(base, block_id_from_op(op(2, 1)));
    assert_ne!(base, block_id_from_op(op(1, 2)));
}

#[test]
fn block_new_uses_block_id_from_op() {
    let insert = op(7, 3);
    let block = Block::new(BlockKind::Paragraph { text: "hi".into() }, insert);
    assert_eq!(block.elem_id, insert);
    assert_eq!(block.id, block_id_from_op(insert));
}

#[test]
fn parser_assigns_stable_block_ids_for_same_input() {
    let input = "# Title\n\nHello\n\n```rust\nlet x = 1;\n```\n";
    let doc1 = Parser::parse(input);
    let doc2 = Parser::parse(input);
    let ids1: Vec<_> = doc1.blocks_in_order().iter().map(|b| b.id).collect();
    let ids2: Vec<_> = doc2.blocks_in_order().iter().map(|b| b.id).collect();
    assert_eq!(ids1, ids2);
    assert!(!ids1.is_empty());
    // Distinct blocks get distinct ids under sequential peer-0 OpIds.
    let mut sorted = ids1.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids1.len());
}

#[test]
fn table_insert_row_id_matches_op() {
    use md_crdt::doc::{ColumnAlignment, ColumnDef, Table};
    let table_op = op(1, 1);
    let mut table = Table::new(
        block_id_from_op(table_op),
        table_op,
        vec![ColumnDef {
            alignment: ColumnAlignment::Left,
        }],
        vec!["h".into()],
        table_op,
    );
    let row_op = op(2, 1);
    table.insert_row(None, vec!["c".into()], row_op);
    let row = table.rows_in_order().into_iter().next().expect("row");
    assert_eq!(row.id, block_id_from_op(row_op));
    assert_eq!(row.elem_id, row_op);
}
