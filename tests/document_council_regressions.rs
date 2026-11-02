use md_crdt::core::{OpId, Sequence, SequenceOp};
use md_crdt::doc::{
    BlockKind, ColumnAlignment, EditError, EditOp, EquivalenceMode, InsertTextRun, Parser, Table,
    block_id_from_op,
};
use md_crdt::limits::{InputLimitError, MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS};
use md_crdt::session::{CollaborativeDocument, DocumentDto, SessionError, SessionSnapshot};
use std::sync::{Arc, Barrier};

fn op(counter: u64) -> OpId {
    OpId { peer: 77, counter }
}

#[test]
fn table_limits_refuse_width_height_and_rectangular_expansion() {
    let wide = format!(
        "|{}\n|{}\n",
        "x|".repeat(MAX_TABLE_COLUMNS + 1),
        "---|".repeat(MAX_TABLE_COLUMNS + 1)
    );
    assert_eq!(
        Parser::try_parse(&wide).unwrap_err(),
        InputLimitError::TableWork
    );
    let tall = format!("| x |\n| --- |\n{}", "| a |\n".repeat(MAX_TABLE_ROWS));
    assert_eq!(
        Parser::try_parse(&tall).unwrap_err(),
        InputLimitError::TableWork
    );
    let width = 128;
    let row = format!("|{}\n", "x|".repeat(width));
    let rectangular = format!(
        "{}|{}\n{}",
        row,
        "---|".repeat(width),
        row.repeat(MAX_TABLE_CELLS / width - 1)
    );
    assert_eq!(
        Parser::try_parse(&rectangular).unwrap_err(),
        InputLimitError::TableWork
    );
    let boundary = format!(
        "|{}\n|{}\n",
        "x|".repeat(MAX_TABLE_COLUMNS),
        "---|".repeat(MAX_TABLE_COLUMNS)
    );
    assert_eq!(
        Parser::try_parse(&boundary)
            .unwrap()
            .serialize(EquivalenceMode::Exact),
        boundary
    );
}

#[test]
fn bulk_table_parse_preserves_sequence_identity_and_exact_trivia() {
    let input = "| a | b |\r\n| :--- | ---: |\r\n| x | y |\r\n| z | |\r\n";
    let parsed = Parser::try_parse(input).unwrap();
    assert_eq!(parsed.serialize(EquivalenceMode::Exact), input);
    let BlockKind::Table { table } = &parsed.blocks_in_order()[0].kind else {
        panic!("table")
    };
    let id = |counter| OpId { peer: 0, counter };
    let mut expected = Table::new(block_id_from_op(id(1)), id(1), id(1));
    expected.insert_column(None, ColumnAlignment::Left, "a".into(), id(2));
    expected.insert_column(Some(id(2)), ColumnAlignment::Right, "b".into(), id(3));
    expected.insert_row(
        None,
        vec![
            (block_id_from_op(id(2)), "x".into()),
            (block_id_from_op(id(3)), "y".into()),
        ],
        id(4),
    );
    expected.insert_row(
        Some(id(4)),
        vec![
            (block_id_from_op(id(2)), "z".into()),
            (block_id_from_op(id(3)), "".into()),
        ],
        id(5),
    );
    assert_eq!(**table, expected);
}

#[test]
fn invalid_direct_edits_leave_text_marks_and_exact_source_unchanged() {
    let mut document = Parser::parse("#  Styled ##\r\n\r\néclair\r\n");
    let block_id = document.blocks_in_order()[1].id;
    let before = document.clone();
    assert_eq!(
        document.insert_text(block_id, 99, "lost", op(1)),
        Err(EditError::InvalidOffset)
    );
    assert_eq!(document, before);
    assert_eq!(
        document.insert_text(block_id, 1, "ab", op(u64::MAX)),
        Err(EditError::InvalidOffset)
    );
    assert_eq!(document, before);
    let run = InsertTextRun {
        block_id,
        grapheme_offset: 1,
        byte_offset: 1,
        text: "lost".into(),
        op_id: op(1),
    };
    assert_eq!(
        document.raw_apply_op(EditOp::InsertText(run), true),
        Err(EditError::InvalidOffset)
    );
    assert_eq!(document, before);
    document.insert_text(block_id, 1, "!", op(1)).unwrap();
    assert_eq!(
        document.serialize(EquivalenceMode::Exact),
        "#  Styled ##\r\n\r\né!clair\r\n"
    );
}

#[test]
fn nested_direct_edit_uses_live_index_without_touching_unrelated_source() {
    let mut document = Parser::parse("> nested\r\n\r\n#  Unchanged ##\r\n");
    let BlockKind::BlockQuote { children } = &document.blocks_in_order()[0].kind else {
        panic!("quote")
    };
    let child = children.iter().next().unwrap().id;
    document.insert_text(child, 6, "!", op(1)).unwrap();
    assert_eq!(
        document.serialize(EquivalenceMode::Exact),
        "> nested!\r\n\r\n#  Unchanged ##\r\n"
    );
}

#[test]
fn malformed_snapshot_source_ranges_are_refused_before_rendering() {
    let mut snapshot = CollaborativeDocument::new(77).save_snapshot().unwrap();
    snapshot.document = DocumentDto::from_document(&Parser::parse("éclair\n\nsecond\n"));
    let valid = serde_json::to_value(snapshot).unwrap();
    for field in ["preamble_end", "trailer_start"] {
        for offset in [1, 99999] {
            let mut malformed = valid.clone();
            malformed["document"]["source"][field] = offset.into();
            let snapshot: SessionSnapshot = serde_json::from_value(malformed).unwrap();
            assert!(CollaborativeDocument::restore_from_snapshot(snapshot).is_err());
        }
    }
    for field in ["leading_start", "leading_end", "body_start", "body_end"] {
        for offset in [1, 99999] {
            let mut malformed = valid.clone();
            let regions = malformed["document"]["source"]["regions"]
                .as_object_mut()
                .unwrap();
            regions.values_mut().next().unwrap()[field] = offset.into();
            let snapshot: SessionSnapshot = serde_json::from_value(malformed).unwrap();
            assert!(CollaborativeDocument::restore_from_snapshot(snapshot).is_err());
        }
    }
    let restored =
        CollaborativeDocument::restore_from_snapshot(serde_json::from_value(valid).unwrap())
            .unwrap();
    assert_eq!(
        restored.document().serialize(EquivalenceMode::Exact),
        "éclair\n\nsecond\n"
    );
}

#[test]
fn moving_list_item_into_descendant_is_typed_and_atomic() {
    let mut snapshot = CollaborativeDocument::new(77).save_snapshot().unwrap();
    snapshot.document = DocumentDto::from_document(&Parser::parse("- outer\n  - inner\n"));
    let mut session = CollaborativeDocument::restore_from_snapshot(snapshot).unwrap();
    let BlockKind::List { items, .. } = &session.document().blocks_in_order()[0].kind else {
        panic!("list")
    };
    let item = items.iter().next().unwrap();
    let item_id = item.id;
    let nested = item
        .children
        .iter()
        .find(|block| matches!(block.kind, BlockKind::List { .. }))
        .unwrap()
        .id;
    let before = session.save_snapshot().unwrap();
    assert!(matches!(
        session.move_list_item(item_id, nested, None),
        Err(SessionError::MoveCycle)
    ));
    assert_eq!(session.save_snapshot().unwrap(), before);
    assert_eq!(
        session.document().serialize(EquivalenceMode::Exact),
        "- outer\n  - inner\n"
    );
}

#[test]
fn concurrent_first_and_missing_lookups_survive_public_sequence_swap() {
    let left = Parser::parse("left\n\n# heading\n");
    let ids: Vec<_> = left
        .blocks_in_order()
        .iter()
        .map(|block| (block.id, block.elem_id))
        .collect();
    let shared = Arc::new(left);
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let shared = Arc::clone(&shared);
            let barrier = Arc::clone(&barrier);
            let ids = &ids;
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..32 {
                    for (id, elem) in ids {
                        assert_eq!(shared.block_elem_id(*id), Some(*elem));
                        assert_eq!(shared.find_block(*elem).unwrap().id, *id);
                    }
                    assert!(shared.find_block_by_id(block_id_from_op(op(999))).is_none());
                }
            });
        }
    });
    let mut left = Arc::try_unwrap(shared).unwrap();
    let mut right = md_crdt::doc::Document::new();
    let replacement = op(99);
    right.insert_block_at(
        None,
        None,
        replacement,
        md_crdt::doc::Block::new(
            BlockKind::Paragraph {
                text: Sequence::new(),
            },
            replacement,
        ),
        None,
    );
    assert_eq!(
        right.block_elem_id(block_id_from_op(replacement)),
        Some(replacement)
    );
    std::mem::swap(&mut left.blocks, &mut right.blocks);
    assert_eq!(
        left.block_elem_id(block_id_from_op(replacement)),
        Some(replacement)
    );
    assert!(left.find_block_by_id(ids[0].0).is_none());
    assert_eq!(right.block_elem_id(ids[0].0), Some(ids[0].1));
}

#[test]
fn source_ordinals_keep_untouched_trivia_after_reorder_edit_and_restore() {
    let mut document = Parser::parse("alpha\r\n\r\n#  Styled ##\r\n\r\nbeta\r\n");
    let mut first = (*document.blocks_in_order()[0]).clone();
    let last = document.blocks_in_order()[2].elem_id;
    let last_id = document.blocks_in_order()[2].id;
    document.delete_block_at(None, first.elem_id, op(1));
    first.elem_id = op(2);
    document.insert_block_at(None, Some(last), op(2), first, None);
    document.insert_text(last_id, 4, "!", op(3)).unwrap();
    let exact = document.serialize(EquivalenceMode::Exact);
    // Untouched regions retain their own source bytes; separators generated for
    // the edited paragraph are not a public LF-only formatting contract.
    assert!(exact.starts_with("#  Styled ##\r\n\r\n"));
    assert!(exact.ends_with("alpha\r\n"));
    let BlockKind::Paragraph { text } = &document.find_block_by_id(last_id).unwrap().kind else {
        panic!("edited paragraph retained")
    };
    assert_eq!(md_crdt::doc::paragraph_visible_string(text), "beta!");
    let dto: DocumentDto = serde_json::from_slice(
        &serde_json::to_vec(&DocumentDto::from_document(&document)).unwrap(),
    )
    .unwrap();
    let restored = dto.into_document();
    assert_eq!(restored.serialize(EquivalenceMode::Exact), exact);
    assert_eq!(restored, document);
}

#[test]
fn table_rendering_follows_live_columns_and_relocated_rows() {
    let mut document = Parser::parse(
        "| a | b | c |\n| --- | --- | --- |\n| x | y | z |\n| u | v | w |\n| gone | gone | gone |\n",
    );
    let elem = document.blocks_in_order()[0].elem_id;
    document
        .with_block_mut(elem, |block| {
            let BlockKind::Table { table } = &mut block.kind else {
                panic!("table")
            };
            let columns = table.columns_in_order();
            let rows = table.rows_in_order();
            table.remove_column(columns[1].id, op(1));
            table.remove_row(rows[2].elem_id, op(2));
            let mut moved = rows[1].clone();
            table.rows.delete(moved.elem_id, op(3));
            moved.elem_id = op(4);
            table.rows.apply(SequenceOp::Insert {
                after: None,
                id: op(4),
                value: moved,
                right_origin: Some(rows[0].elem_id),
            });
        })
        .unwrap();
    assert_eq!(
        document.serialize(EquivalenceMode::Structural),
        "| a | c |\n| --- | --- |\n| u | w |\n| x | z |"
    );
}
