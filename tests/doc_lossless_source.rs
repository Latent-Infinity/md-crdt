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

fn parsed_session(source: &str, peer: u64) -> md_crdt::CollaborativeDocument {
    let mut snapshot = md_crdt::CollaborativeDocument::new(peer)
        .save_snapshot()
        .unwrap();
    snapshot.document = DocumentDto::from_document(&Parser::parse(source));
    md_crdt::CollaborativeDocument::restore_from_snapshot(snapshot).unwrap()
}

#[test]
fn direct_document_move_then_edit_dirties_the_logical_source_region() {
    let mut document = Parser::parse("alpha\n\nbeta\n");
    let mut beta = document.blocks_in_order()[1].clone();
    let target = beta.elem_id;
    let logical_id = beta.id;
    let moved = OpId {
        peer: 42,
        counter: 1,
    };
    beta.elem_id = moved;
    assert!(document.delete_block_at(None, target, moved));
    assert!(document.insert_block_at(None, None, moved, beta, None));
    document
        .insert_text(
            logical_id,
            0,
            "X",
            OpId {
                peer: 42,
                counter: 2,
            },
        )
        .unwrap();
    assert_eq!(
        document.serialize(EquivalenceMode::Exact),
        "Xbeta\n\nalpha\n"
    );
}

#[test]
fn session_move_then_edit_survives_remote_replay_and_snapshot_restore() {
    for restore_between in [false, true] {
        let mut sender = parsed_session("alpha\n\nbeta\n", 42);
        let baseline = sender.save_snapshot().unwrap();
        let beta = sender.document().blocks_in_order()[1].id;
        sender.move_block(beta, None, None).unwrap();
        if restore_between {
            let bytes = sender.save_snapshot().unwrap().to_bytes().unwrap();
            sender = md_crdt::CollaborativeDocument::restore_from_snapshot(
                md_crdt::SessionSnapshot::from_bytes(&bytes).unwrap(),
            )
            .unwrap();
        }
        sender.insert_text(beta, 0, "X").unwrap();
        assert_eq!(
            sender.document().serialize(EquivalenceMode::Exact),
            "Xbeta\n\nalpha\n"
        );
        let mut receiver =
            md_crdt::CollaborativeDocument::rebase_from_snapshot(baseline, 43).unwrap();
        receiver
            .apply_remote(
                sender
                    .encode_changes_since(&receiver.state_vector())
                    .unwrap(),
                &md_crdt::ValidationLimits::default(),
            )
            .unwrap();
        assert_eq!(
            receiver.document().serialize(EquivalenceMode::Exact),
            "Xbeta\n\nalpha\n"
        );
    }
}

#[test]
fn nested_move_dirties_both_container_roots_but_preserves_unrelated_trivia() {
    let input = "> alpha\n>\n> beta\n\n> gamma\n\n#  Untouched ##\r\n";
    let mut sender = parsed_session(input, 42);
    let roots = sender.document().blocks_in_order();
    let BlockKind::BlockQuote { children } = &roots[0].kind else {
        panic!("first quote")
    };
    let beta = children.iter().last().unwrap().id;
    let destination = roots[1].elem_id;
    let baseline = sender.save_snapshot().unwrap();
    sender.move_block(beta, Some(destination), None).unwrap();
    let bytes = sender.save_snapshot().unwrap().to_bytes().unwrap();
    sender = md_crdt::CollaborativeDocument::restore_from_snapshot(
        md_crdt::SessionSnapshot::from_bytes(&bytes).unwrap(),
    )
    .unwrap();
    sender.insert_text(beta, 0, "X").unwrap();
    let exact = sender.document().serialize(EquivalenceMode::Exact);
    assert_eq!(exact.matches("Xbeta").count(), 1);
    assert!(exact.ends_with("#  Untouched ##\r\n"), "{exact:?}");
    assert_eq!(
        Parser::parse(&exact).serialize(EquivalenceMode::Structural),
        sender.document().serialize(EquivalenceMode::Structural),
    );
    let mut receiver = md_crdt::CollaborativeDocument::rebase_from_snapshot(baseline, 43).unwrap();
    receiver
        .apply_remote(
            sender
                .encode_changes_since(&receiver.state_vector())
                .unwrap(),
            &md_crdt::ValidationLimits::default(),
        )
        .unwrap();
    assert_eq!(receiver.document().serialize(EquivalenceMode::Exact), exact);
}
