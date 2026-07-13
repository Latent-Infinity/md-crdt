use md_crdt::{Block, BlockKind, DocumentDto, EditError, EquivalenceMode, OpId, Parser};

#[test]
fn exact_serialization_preserves_all_original_bytes() {
    let input = "---\r\ntitle:  Test\r\n---\r\n\r\n##  Heading  ##\r\n\r\nalpha beta\r\n\r\n:::opaque\r\n  untouched\r\n";
    let document = Parser::parse(input);
    assert_eq!(document.serialize(EquivalenceMode::Exact), input);
}

#[test]
fn one_word_edit_rerenders_only_its_source_region() {
    let input = "##  Heading  ##\r\n\r\nalpha beta\r\n\r\n:::opaque\r\n  untouched\r\n";
    let mut document = Parser::parse(input);
    let paragraph = document.blocks_in_order()[1].id;

    document
        .insert_text(
            paragraph,
            6,
            "brave ",
            OpId {
                peer: 7,
                counter: 1,
            },
        )
        .unwrap();

    assert_eq!(
        document.serialize(EquivalenceMode::Exact),
        "##  Heading  ##\r\n\r\nalpha brave beta\r\n\r\n:::opaque\r\n  untouched\r\n"
    );
}

#[test]
fn deleting_the_first_block_does_not_leave_orphaned_leading_trivia() {
    let mut document = Parser::parse("alpha\n\nbeta\n");
    let first = document.blocks_in_order()[0].elem_id;
    assert!(document.delete_block_at(
        None,
        first,
        OpId {
            peer: 9,
            counter: 1,
        },
    ));
    assert_eq!(document.serialize(EquivalenceMode::Exact), "beta\n");
}

#[test]
fn source_regions_survive_document_dto_json_round_trip() {
    let mut document = Parser::parse("#  Styled  ##\n\nalpha beta\n");
    let paragraph = document.blocks_in_order()[1].id;
    document
        .insert_text(
            paragraph,
            6,
            "brave ",
            OpId {
                peer: 5,
                counter: 1,
            },
        )
        .unwrap();
    let bytes = serde_json::to_vec(&DocumentDto::from_document(&document)).unwrap();
    let restored: DocumentDto = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        restored.into_document().serialize(EquivalenceMode::Exact),
        "#  Styled  ##\n\nalpha brave beta\n"
    );
}

#[test]
fn inserted_block_gets_one_deterministic_separator_without_rewriting_neighbors() {
    let mut document = Parser::parse("alpha\n\nbeta\n");
    let first = document.blocks_in_order()[0].elem_id;
    let right_origin = document.compute_child_right_origin(None, Some(first));
    let block_id = OpId {
        peer: 11,
        counter: 1,
    };
    let text_id = OpId {
        peer: 11,
        counter: 2,
    };
    assert!(document.insert_block_at(
        None,
        Some(first),
        block_id,
        Block::new(BlockKind::paragraph("middle", text_id), block_id),
        right_origin,
    ));
    assert_eq!(
        document.serialize(EquivalenceMode::Exact),
        "alpha\n\nmiddle\n\nbeta\n"
    );
}

#[test]
fn opaque_source_block_rejects_structured_text_mutation_without_losing_bytes() {
    let input = ":::opaque\n  untouched\n";
    let mut document = Parser::parse(input);
    let raw_block = document.blocks_in_order()[0].id;

    let error = document
        .insert_text(
            raw_block,
            0,
            "changed",
            OpId {
                peer: 13,
                counter: 1,
            },
        )
        .unwrap_err();

    assert_eq!(error, EditError::InvalidOffset);
    assert_eq!(document.serialize(EquivalenceMode::Exact), input);
}

#[test]
fn untouched_blockquote_root_with_nested_children_stays_byte_exact() {
    // A blockquote is one source root spanning nested children. Editing a top-level
    // sibling must leave the whole untouched quote root sliced out byte-for-byte,
    // including its non-canonical double spaces and multi-line body.
    let input = ">  quoted  words\r\n>  second line\r\n\r\ngamma delta\r\n";
    let mut document = Parser::parse(input);
    assert!(matches!(
        document.blocks_in_order()[0].kind,
        BlockKind::BlockQuote { .. }
    ));
    let sibling = document.blocks_in_order()[1].id;

    document
        .insert_text(
            sibling,
            6,
            "X",
            OpId {
                peer: 9,
                counter: 1,
            },
        )
        .unwrap();

    let output = document.serialize(EquivalenceMode::Exact);
    assert!(
        output.contains(">  quoted  words\r\n>  second line\r\n"),
        "untouched blockquote root must be byte-preserved: {output:?}"
    );
    assert!(output.contains("gamma Xdelta"), "sibling edit: {output:?}");
}

#[test]
fn multi_region_edit_leaves_untouched_unicode_region_byte_identical() {
    // Two roots dirtied, the multibyte middle root untouched: it must be sliced out of
    // the immutable original on a codepoint boundary, never mid-codepoint.
    let input = "alpha one\r\n\r\ncafé — naïve ☕ 世界\r\n\r\ngamma three\r\n";
    let mut document = Parser::parse(input);
    let first = document.blocks_in_order()[0].id;
    let third = document.blocks_in_order()[2].id;

    document
        .insert_text(
            first,
            6,
            "X",
            OpId {
                peer: 1,
                counter: 1,
            },
        )
        .unwrap();
    document
        .insert_text(
            third,
            6,
            "Y",
            OpId {
                peer: 2,
                counter: 1,
            },
        )
        .unwrap();

    let output = document.serialize(EquivalenceMode::Exact);
    assert!(
        output.contains("café — naïve ☕ 世界"),
        "untouched unicode region must be byte-exact: {output:?}"
    );
    assert!(output.contains("alpha Xone"), "first edit: {output:?}");
    assert!(output.contains("gamma Ythree"), "third edit: {output:?}");
}
