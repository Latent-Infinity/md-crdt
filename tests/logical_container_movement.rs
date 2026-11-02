use md_crdt::core::mark::MarkKind;
use md_crdt::doc::{BlockId, BlockKind, EquivalenceMode, block_id_from_op};
use md_crdt::sync::{ChangeMessage, CheckpointRequest, DocumentTombstonePolicy, ValidationLimits};
use md_crdt::{
    BlockDraft, CollaborativeDocument, ListItemDraft, ListStyle, OpId, SessionSnapshot, StateVector,
};
use std::collections::BTreeMap;

fn apply(session: &mut CollaborativeDocument, message: ChangeMessage) {
    session
        .apply_remote(message, &ValidationLimits::default())
        .unwrap();
}
fn history(session: &CollaborativeDocument) -> ChangeMessage {
    session.encode_changes_since(&StateVector::new()).unwrap()
}
fn restore(session: CollaborativeDocument, checkpoint: bool) -> CollaborativeDocument {
    let mut session = session;
    if checkpoint {
        session
            .checkpoint_history(&CheckpointRequest {
                max_retained_ops: 0,
                active_peer_leases: Vec::new(),
                tombstones: DocumentTombstonePolicy::KeepAll,
            })
            .unwrap();
        assert!(session.save_snapshot().unwrap().ops.is_empty());
    }
    let bytes = session.save_snapshot().unwrap().to_bytes().unwrap();
    CollaborativeDocument::restore_from_snapshot(SessionSnapshot::from_bytes(&bytes).unwrap())
        .unwrap()
}
fn rendered(session: &CollaborativeDocument) -> String {
    session.document().serialize(EquivalenceMode::Structural)
}
fn quote(session: &mut CollaborativeDocument, after: Option<OpId>, text: &str) -> OpId {
    session
        .insert_draft_in(
            None,
            after,
            &BlockDraft::BlockQuote {
                children: vec![BlockDraft::Paragraph { text: text.into() }],
            },
            Default::default(),
        )
        .unwrap()
}
fn replica(peer: u64, initial: &ChangeMessage) -> CollaborativeDocument {
    let mut session = CollaborativeDocument::new(peer);
    apply(&mut session, initial.clone());
    session
}
fn schedules(count: usize) -> Vec<Vec<usize>> {
    fn visit(prefix: &mut Vec<usize>, remaining: &mut Vec<usize>, result: &mut Vec<Vec<usize>>) {
        if remaining.is_empty() {
            result.push(prefix.clone());
            return;
        }
        for index in 0..remaining.len() {
            let value = remaining.remove(index);
            prefix.push(value);
            visit(prefix, remaining, result);
            prefix.pop();
            remaining.insert(index, value);
        }
    }
    let mut result = Vec::new();
    visit(&mut Vec::new(), &mut (0..count).collect(), &mut result);
    result
}
fn assert_clean(session: &CollaborativeDocument) {
    let snapshot = session.save_snapshot().unwrap();
    assert!(snapshot.pending.is_empty());
    assert!(snapshot.deferred.is_empty());
}

#[test]
fn moved_quote_keeps_late_staged_descendants_marks_and_alias_edits() {
    let mut base = CollaborativeDocument::new(1);
    let q = quote(&mut base, None, "seed");
    let tail = base.insert_paragraph(Some(q), "tail").unwrap();
    let initial = history(&base);
    let frontier = base.state_vector();
    let mut mover = replica(2, &initial);
    mover
        .move_block(block_id_from_op(q), None, Some(tail))
        .unwrap();
    let movement = mover.encode_changes_since(&frontier).unwrap();
    let mut writer = replica(3, &initial);
    let inner = writer
        .insert_draft_in(
            Some(q),
            None,
            &BlockDraft::BlockQuote {
                children: vec![BlockDraft::Paragraph {
                    text: "late".into(),
                }],
            },
            Default::default(),
        )
        .unwrap();
    let BlockKind::BlockQuote { children } = &writer.document().find_block(inner).unwrap().kind
    else {
        panic!("quote");
    };
    let paragraph = children.iter().next().unwrap().id;
    writer
        .set_mark(paragraph, 0..4, MarkKind::Bold, BTreeMap::new())
        .unwrap();
    let descendants = writer.encode_changes_since(&frontier).unwrap();
    let mut expected = None;
    for order in schedules(2) {
        for recovery in 0..3 {
            let mut receiver = replica(10, &initial);
            for (index, message) in order.iter().enumerate() {
                let mut delta = [movement.clone(), descendants.clone()][*message].clone();
                if recovery == 2 {
                    delta.ops.reverse();
                }
                apply(&mut receiver, delta);
                if index == 0 && recovery != 0 {
                    receiver = restore(receiver, recovery == 2);
                }
            }
            assert_clean(&receiver);
            assert!(rendered(&receiver).contains("**late**"));
            // The old physical quote reference resolves to its one live placement.
            receiver
                .insert_paragraph_in(Some(q), None, "continuation")
                .unwrap();
            receiver.insert_text(paragraph, 4, "!").unwrap();
            let actual = (
                rendered(&receiver),
                receiver.state_vector(),
                receiver.save_snapshot().unwrap().document,
            );
            if let Some(expected) = &expected {
                assert_eq!(&actual, expected);
            } else {
                expected = Some(actual);
            }
            let receiver = restore(receiver, true);
            assert!(rendered(&receiver).contains("continuation"));
            assert!(rendered(&receiver).contains("late"));
        }
    }
}

#[test]
fn reciprocal_and_longer_move_cycles_project_identically_across_delivery_and_recovery() {
    for count in [2usize, 3, 4] {
        let mut base = CollaborativeDocument::new(1);
        let mut quotes = Vec::new();
        for index in 0..count {
            quotes.push(quote(
                &mut base,
                quotes.last().copied(),
                &format!("content-{index}"),
            ));
        }
        let initial = history(&base);
        let frontier = base.state_vector();
        let mut moves = Vec::new();
        for index in 0..count {
            let mut author = replica(index as u64 + 2, &initial);
            author
                .move_block(
                    block_id_from_op(quotes[index]),
                    Some(quotes[(index + 1) % count]),
                    None,
                )
                .unwrap();
            moves.push(author.encode_changes_since(&frontier).unwrap());
        }
        let mut expected = None;
        for schedule in schedules(count) {
            for checkpoint in [false, true] {
                let mut receiver = replica(20, &initial);
                for index in schedule.iter().copied() {
                    apply(&mut receiver, moves[index].clone());
                    receiver = restore(receiver, checkpoint);
                }
                assert_clean(&receiver);
                for index in 0..count {
                    assert!(rendered(&receiver).contains(&format!("content-{index}")));
                }
                let actual = (
                    rendered(&receiver),
                    receiver.state_vector(),
                    receiver.save_snapshot().unwrap().document,
                );
                if let Some(expected) = &expected {
                    assert_eq!(&actual, expected);
                } else {
                    expected = Some(actual);
                }
            }
        }
    }
}

#[test]
fn causally_later_low_counter_move_supersedes_observed_high_counter_move() {
    let mut high = CollaborativeDocument::new(1);
    let q = quote(&mut high, None, "preserved");
    let destination = quote(&mut high, Some(q), "destination");
    let mut after = Some(destination);
    for _ in 0..20 {
        after = Some(high.insert_paragraph(after, "clock").unwrap());
    }
    let high_id = high
        .move_block(block_id_from_op(q), Some(destination), None)
        .unwrap();
    let initial = history(&high);
    let mut low = replica(2, &initial);
    let low_id = low.move_block(block_id_from_op(q), None, after).unwrap();
    assert!(low_id.counter < high_id.counter);
    let delta = low.encode_changes_since(&high.state_vector()).unwrap();
    high = restore(high, true);
    apply(&mut high, delta);
    assert_eq!(
        high.document().block_elem_id(block_id_from_op(q)),
        Some(low_id)
    );
    assert_eq!(
        high.document().block_parent(block_id_from_op(q)),
        Some(None)
    );
    assert_eq!(rendered(&high), rendered(&low));
    assert_eq!(high.state_vector(), low.state_vector());
    assert_eq!(
        restore(high, true).document(),
        restore(low, true).document()
    );
}

#[test]
fn genuine_container_deletion_wins_move_and_late_descendant_without_resurrection() {
    let mut base = CollaborativeDocument::new(1);
    let q = quote(&mut base, None, "deleted seed");
    let tail = base.insert_paragraph(Some(q), "survivor").unwrap();
    let initial = history(&base);
    let frontier = base.state_vector();
    let mut deleter = replica(2, &initial);
    deleter.delete_block(q).unwrap();
    let mut mover = replica(3, &initial);
    mover
        .move_block(block_id_from_op(q), None, Some(tail))
        .unwrap();
    let mut writer = replica(4, &initial);
    writer
        .insert_paragraph_in(Some(q), None, "deleted late child")
        .unwrap();
    let messages = [
        deleter.encode_changes_since(&frontier).unwrap(),
        mover.encode_changes_since(&frontier).unwrap(),
        writer.encode_changes_since(&frontier).unwrap(),
    ];
    let mut expected = None;
    for schedule in schedules(3) {
        let mut receiver = replica(10, &initial);
        for index in schedule {
            apply(&mut receiver, messages[index].clone());
            receiver = restore(receiver, true);
        }
        assert_clean(&receiver);
        assert_eq!(rendered(&receiver), "survivor");
        assert!(
            receiver
                .document()
                .find_block_by_id(block_id_from_op(q))
                .is_none()
        );
        let actual = receiver.save_snapshot().unwrap().document;
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn overlapping_heading_ranges_keep_the_winning_transaction_atomic() {
    let mut base = CollaborativeDocument::new(1);
    let h1 = base
        .insert_draft_in(
            None,
            None,
            &BlockDraft::Heading {
                level: 1,
                text: "H1".into(),
            },
            Default::default(),
        )
        .unwrap();
    let a = base.insert_paragraph(Some(h1), "A").unwrap();
    let h2 = base
        .insert_draft_in(
            None,
            Some(a),
            &BlockDraft::Heading {
                level: 2,
                text: "H2".into(),
            },
            Default::default(),
        )
        .unwrap();
    let b = base.insert_paragraph(Some(h2), "B").unwrap();
    let h3 = base
        .insert_draft_in(
            None,
            Some(b),
            &BlockDraft::Heading {
                level: 1,
                text: "H3".into(),
            },
            Default::default(),
        )
        .unwrap();
    let initial = history(&base);
    let frontier = base.state_vector();
    let mut outer = replica(2, &initial);
    outer.move_section(block_id_from_op(h1), Some(h3)).unwrap();
    let mut inner = replica(3, &initial);
    inner.move_section(block_id_from_op(h2), None).unwrap();
    let messages = [
        outer.encode_changes_since(&frontier).unwrap(),
        inner.encode_changes_since(&frontier).unwrap(),
    ];
    let expected_ids: Vec<BlockId> = [h3, h1, a, h2, b]
        .into_iter()
        .map(block_id_from_op)
        .collect();
    let mut expected = None;
    for schedule in schedules(2) {
        let mut receiver = replica(9, &initial);
        for index in schedule {
            apply(&mut receiver, messages[index].clone());
            receiver = restore(receiver, true);
        }
        assert_eq!(
            receiver
                .document()
                .blocks()
                .iter()
                .map(|block| block.id)
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert_clean(&receiver);
        let actual = receiver.save_snapshot().unwrap().document;
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
    let mut deleter = replica(4, &initial);
    deleter.delete_block(b).unwrap();
    let deleted = deleter.encode_changes_since(&frontier).unwrap();
    let expected_ids: Vec<_> = [h1, a, h2, h3].into_iter().map(block_id_from_op).collect();
    let mut expected = None;
    for schedule in schedules(3) {
        let mut receiver = replica(9, &initial);
        for index in schedule {
            apply(
                &mut receiver,
                if index == 2 {
                    deleted.clone()
                } else {
                    messages[index].clone()
                },
            );
            receiver = restore(receiver, true);
        }
        assert_clean(&receiver);
        assert_eq!(
            receiver
                .document()
                .blocks()
                .iter()
                .map(|block| block.id)
                .collect::<Vec<_>>(),
            expected_ids
        );
        let actual = receiver.save_snapshot().unwrap().document;
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn quote_list_destination_cycles_preserve_all_live_descendants() {
    let mut base = CollaborativeDocument::new(1);
    let q = quote(&mut base, None, "quote-child");
    let list = base
        .insert_draft_in(
            None,
            Some(q),
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::Paragraph {
                        text: "list-child".into(),
                    }],
                }],
            },
            Default::default(),
        )
        .unwrap();
    let BlockKind::List { items, .. } = &base.document().find_block(list).unwrap().kind else {
        panic!("list");
    };
    let item = items.iter().next().unwrap().elem_id;
    let initial = history(&base);
    let frontier = base.state_vector();
    let mut first = replica(2, &initial);
    first
        .move_block(block_id_from_op(q), Some(item), None)
        .unwrap();
    let mut second = replica(3, &initial);
    second
        .move_block(block_id_from_op(list), Some(q), None)
        .unwrap();
    let mut third = replica(4, &initial);
    third
        .insert_paragraph_in(Some(item), None, "late-list-child")
        .unwrap();
    let messages = [
        first.encode_changes_since(&frontier).unwrap(),
        second.encode_changes_since(&frontier).unwrap(),
        third.encode_changes_since(&frontier).unwrap(),
    ];
    let mut expected = None;
    for schedule in schedules(3) {
        let mut receiver = replica(10, &initial);
        for index in schedule {
            apply(&mut receiver, messages[index].clone());
            receiver = restore(receiver, true);
        }
        assert_clean(&receiver);
        for text in ["quote-child", "list-child", "late-list-child"] {
            assert!(rendered(&receiver).contains(text));
        }
        let actual = receiver.save_snapshot().unwrap().document;
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn concurrent_item_and_ancestor_block_moves_share_one_acyclic_projection() {
    let mut base = CollaborativeDocument::new(1);
    let source = base
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::BlockQuote {
                        children: vec![BlockDraft::Paragraph {
                            text: "inside-item".into(),
                        }],
                    }],
                }],
            },
            Default::default(),
        )
        .unwrap();
    let destination = base
        .insert_draft_in(
            None,
            Some(source),
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::Paragraph {
                        text: "destination-item".into(),
                    }],
                }],
            },
            Default::default(),
        )
        .unwrap();
    let BlockKind::List { items, .. } = &base.document().find_block(source).unwrap().kind else {
        panic!("list");
    };
    let item = items.iter().next().unwrap();
    let item_id = item.id;
    let old_item = item.elem_id;
    let q = item.children.iter().next().unwrap().elem_id;
    let initial = history(&base);
    let frontier = base.state_vector();
    let mut first = replica(2, &initial);
    first
        .move_list_item(item_id, block_id_from_op(destination), None)
        .unwrap();
    let mut second = replica(3, &initial);
    second
        .move_block(block_id_from_op(destination), Some(q), None)
        .unwrap();
    let mut third = replica(4, &initial);
    third
        .insert_paragraph_in(Some(old_item), None, "late-item-child")
        .unwrap();
    let messages = [
        first.encode_changes_since(&frontier).unwrap(),
        second.encode_changes_since(&frontier).unwrap(),
        third.encode_changes_since(&frontier).unwrap(),
    ];
    let mut expected = None;
    for schedule in schedules(3) {
        for single_batch in [false, true] {
            let mut receiver = replica(10, &initial);
            if single_batch {
                let mut combined = messages[0].clone();
                combined.ops = schedule
                    .iter()
                    .flat_map(|index| messages[*index].ops.clone())
                    .collect();
                apply(&mut receiver, combined);
            } else {
                for index in schedule.iter().copied() {
                    apply(&mut receiver, messages[index].clone());
                    receiver = restore(receiver, true);
                }
            }
            assert_clean(&receiver);
            for text in ["inside-item", "destination-item", "late-item-child"] {
                assert!(rendered(&receiver).contains(text));
            }
            receiver
                .insert_paragraph_in(Some(old_item), None, "continued-item-child")
                .unwrap();
            let actual = (
                rendered(&receiver),
                receiver.state_vector(),
                receiver.save_snapshot().unwrap().document,
            );
            if let Some(expected) = &expected {
                assert_eq!(&actual, expected);
            } else {
                expected = Some(actual);
            }
        }
    }
}

#[test]
fn moved_container_race_preserves_exact_unrelated_source_and_descendant_marks_after_rebase() {
    let source = "> original **bold**\r\n\r\n#  Untouched ##\r\n";
    let mut snapshot = CollaborativeDocument::new(1).save_snapshot().unwrap();
    snapshot.document =
        md_crdt::session::snapshot::DocumentDto::from_document(&md_crdt::Parser::parse(source));
    let base = CollaborativeDocument::restore_from_snapshot(snapshot.clone()).unwrap();
    let q = base.document().blocks().iter().next().unwrap().id;
    let old_q = base.document().block_elem_id(q).unwrap();
    let tail = base.document().blocks().iter().last().unwrap().elem_id;
    let mut mover = CollaborativeDocument::rebase_from_snapshot(snapshot.clone(), 2).unwrap();
    let mut writer = CollaborativeDocument::rebase_from_snapshot(snapshot.clone(), 3).unwrap();
    mover.move_block(q, None, Some(tail)).unwrap();
    writer
        .insert_paragraph_in(Some(old_q), None, "late-source-child")
        .unwrap();
    let messages = [history(&mover), history(&writer)];
    let mut expected = None;
    for schedule in schedules(2) {
        let mut receiver =
            CollaborativeDocument::rebase_from_snapshot(snapshot.clone(), 10).unwrap();
        for index in schedule {
            apply(&mut receiver, messages[index].clone());
            receiver = restore(receiver, true);
        }
        assert_clean(&receiver);
        let exact = receiver.document().serialize(EquivalenceMode::Exact);
        assert!(exact.contains("#  Untouched ##\r\n"));
        assert!(exact.contains("**bold**"));
        assert!(exact.contains("late-source-child"));
        if let Some(expected) = &expected {
            assert_eq!(&exact, expected);
        } else {
            expected = Some(exact);
        }
    }
}

#[test]
fn public_block_sequence_swap_keeps_placement_history_with_its_owned_content() {
    let mut first = CollaborativeDocument::new(1);
    let first_quote = quote(&mut first, None, "first-owned-content");
    let first_tail = first
        .insert_paragraph(Some(first_quote), "first-tail")
        .unwrap();
    first
        .move_block(block_id_from_op(first_quote), None, Some(first_tail))
        .unwrap();
    let mut second = CollaborativeDocument::new(2);
    let second_quote = quote(&mut second, None, "second-owned-content");
    let second_tail = second
        .insert_paragraph(Some(second_quote), "second-tail")
        .unwrap();
    second
        .move_block(block_id_from_op(second_quote), None, Some(second_tail))
        .unwrap();
    let mut first_snapshot = first.save_snapshot().unwrap();
    let mut second_snapshot = second.save_snapshot().unwrap();
    let mut left = first_snapshot.document.clone().into_document();
    let mut right = second_snapshot.document.clone().into_document();
    assert!(
        left.find_block_by_id(block_id_from_op(first_quote))
            .is_some()
    );
    assert!(
        right
            .find_block_by_id(block_id_from_op(second_quote))
            .is_some()
    );
    std::mem::swap(&mut left.blocks, &mut right.blocks);
    assert!(
        left.find_block_by_id(block_id_from_op(first_quote))
            .is_none()
    );
    assert!(
        right
            .find_block_by_id(block_id_from_op(second_quote))
            .is_none()
    );
    first_snapshot.document = md_crdt::session::snapshot::DocumentDto::from_document(&right);
    second_snapshot.document = md_crdt::session::snapshot::DocumentDto::from_document(&left);
    let mut first_restored = CollaborativeDocument::restore_from_snapshot(first_snapshot).unwrap();
    let mut second_restored =
        CollaborativeDocument::restore_from_snapshot(second_snapshot).unwrap();
    for (session, old_quote, text) in [
        (&mut first_restored, first_quote, "first-continuation"),
        (&mut second_restored, second_quote, "second-continuation"),
    ] {
        session
            .move_block(block_id_from_op(old_quote), None, None)
            .unwrap();
        session
            .insert_paragraph_in(Some(old_quote), None, text)
            .unwrap();
        assert!(rendered(session).contains(text));
        assert_clean(session);
    }
    assert!(!rendered(&first_restored).contains("second-owned-content"));
    assert!(!rendered(&second_restored).contains("first-owned-content"));
}

#[test]
fn explicit_recreation_starts_a_new_placement_incarnation_without_old_move_resurrection() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, DocOp, Envelope, OpBody, WIRE_VERSION};
    use md_crdt::sync::Operation;
    let mut base = CollaborativeDocument::new(1);
    let original = base.insert_paragraph(None, "old-body").unwrap();
    let logical = block_id_from_op(original);
    let tail = base.insert_paragraph(Some(original), "tail").unwrap();
    let initial = history(&base);
    let frontier = base.state_vector();
    let mut mover = replica(2, &initial);
    mover.move_block(logical, None, Some(tail)).unwrap();
    let movement = mover.encode_changes_since(&frontier).unwrap();
    let mut deleter = replica(3, &initial);
    deleter.delete_block(original).unwrap();
    let mut replacement = deleter.encode_changes_since(&frontier).unwrap();
    let id = OpId {
        peer: 3,
        counter: 2,
    };
    replacement.ops.push(Operation {
        id,
        payload: serde_json::to_vec(&Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertBlock {
                parent: None,
                after: None,
                id,
                right_origin: None,
                block: BlockSkeleton {
                    block_id: logical,
                    kind: BlockKindSkeleton::RawBlock {
                        raw: "reborn-body".into(),
                    },
                },
            }),
        })
        .unwrap()
        .into(),
    });
    let mut expected = None;
    for schedule in schedules(2) {
        let mut receiver = replica(9, &initial);
        for index in schedule {
            apply(
                &mut receiver,
                [movement.clone(), replacement.clone()][index].clone(),
            );
            receiver = restore(receiver, true);
        }
        assert_clean(&receiver);
        assert!(rendered(&receiver).contains("reborn-body"));
        assert!(!rendered(&receiver).contains("old-body"));
        receiver.move_block(logical, None, None).unwrap();
        let actual = (
            rendered(&receiver),
            receiver.state_vector(),
            receiver.save_snapshot().unwrap().document,
        );
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

fn deletion_permutations(
    initial: &SessionSnapshot,
    messages: &[ChangeMessage],
    check: impl Fn(&CollaborativeDocument),
) {
    let mut expected = None;
    for schedule in schedules(messages.len()) {
        for recovery in 0..3 {
            let mut receiver =
                CollaborativeDocument::rebase_from_snapshot(initial.clone(), 10).unwrap();
            if recovery == 2 {
                let mut combined = messages[0].clone();
                combined.ops = schedule
                    .iter()
                    .flat_map(|index| messages[*index].ops.clone())
                    .collect();
                apply(&mut receiver, combined);
            } else {
                for index in schedule.iter().copied() {
                    apply(&mut receiver, messages[index].clone());
                    if recovery == 1 {
                        receiver = restore(receiver, false);
                    }
                }
            }
            assert_clean(&receiver);
            check(&receiver);
            let materialization = |session: &CollaborativeDocument| {
                (
                    session.document().serialize(EquivalenceMode::Exact),
                    session.state_vector(),
                    serde_json::to_value(session.save_snapshot().unwrap().document).unwrap(),
                )
            };
            let actual = materialization(&receiver);
            if let Some(expected) = &expected {
                assert_eq!(&actual, expected);
            } else {
                expected = Some(actual.clone());
            }
            receiver = restore(receiver, true);
            check(&receiver);
            assert_eq!(materialization(&receiver), actual);
        }
    }
}

#[test]
fn captured_subtree_deletion_preserves_unaware_entries_but_kills_captured_escapes() {
    for initially_inside in [false, true] {
        for physical_delete in [false, true] {
            let mut base = CollaborativeDocument::new(1);
            let q = quote(&mut base, None, "captured-seed");
            let p = base
                .insert_paragraph_in(initially_inside.then_some(q), None, "moving-body")
                .unwrap();
            let tail = base.insert_paragraph(Some(q), "surviving-tail").unwrap();
            let initial = base.save_snapshot().unwrap();
            let frontier = base.state_vector();
            let mut mover = replica(2, &history(&base));
            mover
                .move_block(
                    block_id_from_op(p),
                    if initially_inside { None } else { Some(q) },
                    if initially_inside { Some(tail) } else { None },
                )
                .unwrap();
            let mut deleter = replica(3, &history(&base));
            deleter.delete_block(q).unwrap();
            let mut deletion = deleter.encode_changes_since(&frontier).unwrap();
            if physical_delete {
                use md_crdt::codec::{DocOp, Envelope, OpBody};
                for operation in &mut deletion.ops {
                    let mut envelope: Envelope =
                        serde_json::from_slice(&operation.payload).unwrap();
                    if let OpBody::Doc(DocOp::DeleteBlockById {
                        parent,
                        target,
                        id,
                        observed,
                        subtree,
                        ..
                    }) = envelope.body
                    {
                        envelope.body = OpBody::Doc(DocOp::DeleteBlock {
                            parent,
                            target,
                            id,
                            observed,
                            subtree,
                        });
                    } else {
                        panic!("native container deletion");
                    }
                    operation.payload = serde_json::to_vec(&envelope).unwrap().into();
                }
            }
            deletion_permutations(
                &initial,
                &[mover.encode_changes_since(&frontier).unwrap(), deletion],
                |receiver| {
                    assert!(
                        receiver
                            .document()
                            .find_block_by_id(block_id_from_op(q))
                            .is_none()
                    );
                    assert!(!rendered(receiver).contains("captured-seed"));
                    assert_eq!(
                        receiver
                            .document()
                            .find_block_by_id(block_id_from_op(p))
                            .is_some(),
                        !initially_inside
                    );
                    if !initially_inside {
                        assert_eq!(
                            receiver.document().block_parent(block_id_from_op(p)),
                            Some(None)
                        );
                        assert!(rendered(receiver).contains("moving-body"));
                    }
                    assert!(rendered(receiver).contains("surviving-tail"));
                    assert_eq!(
                        receiver
                            .save_snapshot()
                            .unwrap()
                            .document
                            .block_topology
                            .deletions
                            .len(),
                        1
                    );
                },
            );
        }
    }
}

#[test]
fn causally_observed_escape_survives_a_later_ancestor_delete() {
    let mut base = CollaborativeDocument::new(1);
    let q = quote(&mut base, None, "captured-seed");
    let p = base
        .insert_paragraph_in(Some(q), None, "observed-escape")
        .unwrap();
    let frontier = base.state_vector();
    let initial = base.save_snapshot().unwrap();
    let mut mover = replica(2, &history(&base));
    mover.move_block(block_id_from_op(p), None, None).unwrap();
    let movement = mover.encode_changes_since(&frontier).unwrap();
    let mut deleter = replica(3, &history(&base));
    apply(&mut deleter, movement.clone());
    let before_delete = deleter.state_vector();
    deleter.delete_block(q).unwrap();
    deletion_permutations(
        &initial,
        &[
            movement,
            deleter.encode_changes_since(&before_delete).unwrap(),
        ],
        |receiver| {
            assert_eq!(
                receiver.document().block_parent(block_id_from_op(p)),
                Some(None)
            );
            assert!(rendered(receiver).contains("observed-escape"));
            assert!(!rendered(receiver).contains("captured-seed"));
        },
    );
}

#[test]
fn late_birth_deletion_follows_creation_parent_not_a_concurrent_escape_or_entry() {
    for born_inside in [false, true] {
        let mut base = CollaborativeDocument::new(1);
        let q = quote(&mut base, None, "captured-seed");
        base.insert_paragraph(Some(q), "surviving-tail").unwrap();
        let frontier = base.state_vector();
        let initial = base.save_snapshot().unwrap();
        let mut writer = replica(2, &history(&base));
        let child = writer
            .insert_paragraph_in(born_inside.then_some(q), None, "late-birth")
            .unwrap();
        let birth = writer.encode_changes_since(&frontier).unwrap();
        let after_birth = writer.state_vector();
        writer
            .move_block(
                block_id_from_op(child),
                if born_inside { None } else { Some(q) },
                None,
            )
            .unwrap();
        let escape_or_entry = writer.encode_changes_since(&after_birth).unwrap();
        let mut deleter = replica(3, &history(&base));
        deleter.delete_block(q).unwrap();
        deletion_permutations(
            &initial,
            &[
                birth,
                escape_or_entry,
                deleter.encode_changes_since(&frontier).unwrap(),
            ],
            |receiver| {
                assert_eq!(
                    receiver
                        .document()
                        .find_block_by_id(block_id_from_op(child))
                        .is_some(),
                    !born_inside
                );
                assert_eq!(rendered(receiver).contains("late-birth"), !born_inside);
                if !born_inside {
                    assert_eq!(
                        receiver.document().block_parent(block_id_from_op(child)),
                        Some(None)
                    );
                }
                assert!(!rendered(receiver).contains("captured-seed"));
                assert!(rendered(receiver).contains("surviving-tail"));
            },
        );
    }
}

fn list(session: &mut CollaborativeDocument, after: Option<OpId>, text: &str) -> (OpId, BlockId) {
    let element = session
        .insert_draft_in(
            None,
            after,
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::Paragraph { text: text.into() }],
                }],
            },
            Default::default(),
        )
        .unwrap();
    let BlockKind::List { items, .. } = &session.document().find_block(element).unwrap().kind
    else {
        panic!("list");
    };
    (element, items.iter().next().unwrap().id)
}

#[test]
fn item_entry_falls_back_to_a_real_list_and_captured_item_escape_dies() {
    for delete_source in [false, true] {
        let mut base = CollaborativeDocument::new(1);
        let (source, item) = list(&mut base, None, "moving-item");
        let (destination, _) = list(&mut base, Some(source), "destination-item");
        let frontier = base.state_vector();
        let initial = base.save_snapshot().unwrap();
        let mut mover = replica(2, &history(&base));
        mover
            .move_list_item(item, block_id_from_op(destination), None)
            .unwrap();
        let mut deleter = replica(3, &history(&base));
        deleter
            .delete_block(if delete_source { source } else { destination })
            .unwrap();
        deletion_permutations(
            &initial,
            &[
                mover.encode_changes_since(&frontier).unwrap(),
                deleter.encode_changes_since(&frontier).unwrap(),
            ],
            |receiver| {
                if delete_source {
                    assert!(receiver.document().find_list_item_by_id(item).is_none());
                    assert!(!rendered(receiver).contains("moving-item"));
                    assert!(rendered(receiver).contains("destination-item"));
                } else {
                    assert_eq!(
                        receiver.document().list_containing_item(item).unwrap().0,
                        block_id_from_op(source)
                    );
                    assert!(rendered(receiver).contains("moving-item"));
                    assert!(!rendered(receiver).contains("destination-item"));
                }
                assert_eq!(receiver.document().blocks().iter().count(), 1);
            },
        );
    }
}

#[test]
fn observed_item_escape_and_mixed_cycle_keep_the_last_viable_real_list() {
    let mut base = CollaborativeDocument::new(1);
    let source = base
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::BlockQuote {
                        children: vec![BlockDraft::Paragraph {
                            text: "inside-escaping-item".into(),
                        }],
                    }],
                }],
            },
            Default::default(),
        )
        .unwrap();
    let BlockKind::List { items, .. } = &base.document().find_block(source).unwrap().kind else {
        panic!("list");
    };
    let item = items.iter().next().unwrap();
    let item_id = item.id;
    let q = item.children.iter().next().unwrap().elem_id;
    let (destination, _) = list(&mut base, Some(source), "destination-item");
    let frontier = base.state_vector();
    let initial = base.save_snapshot().unwrap();
    let mut mover = replica(2, &history(&base));
    mover
        .move_list_item(item_id, block_id_from_op(destination), None)
        .unwrap();
    let movement = mover.encode_changes_since(&frontier).unwrap();
    let mut deleter = replica(4, &history(&base));
    apply(&mut deleter, movement.clone());
    let before_delete = deleter.state_vector();
    deleter.delete_block(source).unwrap();
    let mut cycler = replica(3, &history(&base));
    cycler
        .move_block(block_id_from_op(destination), Some(q), None)
        .unwrap();
    deletion_permutations(
        &initial,
        &[
            movement,
            deleter.encode_changes_since(&before_delete).unwrap(),
            cycler.encode_changes_since(&frontier).unwrap(),
        ],
        |receiver| {
            assert!(
                receiver
                    .document()
                    .find_block_by_id(block_id_from_op(source))
                    .is_none()
            );
            assert_eq!(
                receiver.document().list_containing_item(item_id).unwrap().0,
                block_id_from_op(destination)
            );
            assert_eq!(
                receiver
                    .document()
                    .block_parent(block_id_from_op(destination)),
                Some(None)
            );
            for text in ["inside-escaping-item", "destination-item"] {
                assert!(rendered(receiver).contains(text));
            }
        },
    );
}

#[test]
fn losing_entry_and_ancestor_deletion_have_identical_exact_source_regions() {
    let source = "outside *body*\r\n\r\n> captured **seed**\r\n\r\n#  Untouched ##\r\n";
    let mut initial = CollaborativeDocument::new(1).save_snapshot().unwrap();
    initial.document =
        md_crdt::session::snapshot::DocumentDto::from_document(&md_crdt::Parser::parse(source));
    let base = CollaborativeDocument::restore_from_snapshot(initial.clone()).unwrap();
    let mut roots = base.document().blocks().iter();
    let p = roots.next().unwrap().id;
    let q = roots.next().unwrap().elem_id;
    let frontier = base.state_vector();
    let mut mover = CollaborativeDocument::rebase_from_snapshot(initial.clone(), 2).unwrap();
    let mut deleter = CollaborativeDocument::rebase_from_snapshot(initial.clone(), 3).unwrap();
    mover.move_block(p, Some(q), None).unwrap();
    deleter.delete_block(q).unwrap();
    deletion_permutations(
        &initial,
        &[
            mover.encode_changes_since(&frontier).unwrap(),
            deleter.encode_changes_since(&frontier).unwrap(),
        ],
        |receiver| {
            let exact = receiver.document().serialize(EquivalenceMode::Exact);
            assert!(exact.contains("outside *body*"));
            assert!(exact.contains("#  Untouched ##\r\n"));
            assert!(!exact.contains("captured"));
            assert_eq!(receiver.document().block_parent(p), Some(None));
        },
    );
}

#[test]
fn old_container_aliases_cannot_publish_late_births_into_a_fresh_incarnation() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, DocOp, Envelope, OpBody, WIRE_VERSION};
    use md_crdt::sync::Operation;
    for is_list in [false, true] {
        let mut base = CollaborativeDocument::new(1);
        let original = if is_list {
            list(&mut base, None, "old-container-body").0
        } else {
            quote(&mut base, None, "old-container-body")
        };
        let logical = block_id_from_op(original);
        let (destination, _) = list(&mut base, Some(original), "surviving-list");
        let initial = base.save_snapshot().unwrap();
        let frontier = base.state_vector();
        let mut writer = replica(2, &history(&base));
        let old_birth = if is_list {
            writer
                .insert_list_item_draft(
                    logical,
                    None,
                    &ListItemDraft {
                        task: None,
                        children: vec![BlockDraft::Paragraph {
                            text: "late-old-incarnation".into(),
                        }],
                    },
                    Default::default(),
                )
                .unwrap()
        } else {
            writer
                .insert_paragraph_in(Some(original), None, "late-old-incarnation")
                .unwrap()
        };
        let birth = writer.encode_changes_since(&frontier).unwrap();
        let before_escape = writer.state_vector();
        if is_list {
            writer
                .move_list_item(
                    block_id_from_op(old_birth),
                    block_id_from_op(destination),
                    None,
                )
                .unwrap();
        } else {
            writer
                .move_block(block_id_from_op(old_birth), None, None)
                .unwrap();
        }
        let escape = writer.encode_changes_since(&before_escape).unwrap();
        let mut creator = replica(3, &history(&base));
        creator.delete_block(original).unwrap();
        let mut replacement = creator.encode_changes_since(&frontier).unwrap();
        let new_element = OpId {
            peer: 3,
            counter: 2,
        };
        replacement.ops.push(Operation {
            id: new_element,
            payload: serde_json::to_vec(&Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::InsertBlock {
                    parent: None,
                    after: None,
                    id: new_element,
                    right_origin: None,
                    block: BlockSkeleton {
                        block_id: logical,
                        kind: if is_list {
                            BlockKindSkeleton::List {
                                style: ListStyle::default(),
                                items: Vec::new(),
                            }
                        } else {
                            BlockKindSkeleton::BlockQuote {
                                children: Vec::new(),
                            }
                        },
                    },
                }),
            })
            .unwrap()
            .into(),
        });
        apply(&mut creator, replacement);
        // The raw recreation used this producer's peer; reclaim its clock before
        // returning to local authoring rather than reusing the injected counter.
        creator.rebind_peer(3);
        if is_list {
            creator
                .insert_list_item_draft(
                    logical,
                    None,
                    &ListItemDraft {
                        task: None,
                        children: vec![BlockDraft::Paragraph {
                            text: "new-incarnation-body".into(),
                        }],
                    },
                    Default::default(),
                )
                .unwrap();
        } else {
            creator
                .insert_paragraph_in(Some(new_element), None, "new-incarnation-body")
                .unwrap();
        }
        deletion_permutations(
            &initial,
            &[
                birth,
                escape,
                creator.encode_changes_since(&frontier).unwrap(),
            ],
            |receiver| {
                assert_eq!(
                    receiver.document().block_elem_id(logical),
                    Some(new_element)
                );
                assert!(rendered(receiver).contains("new-incarnation-body"));
                assert!(rendered(receiver).contains("surviving-list"));
                assert!(!rendered(receiver).contains("old-container-body"));
                assert!(!rendered(receiver).contains("late-old-incarnation"));
            },
        );
    }
}

#[test]
fn malformed_captured_incarnations_are_rejected_atomically_and_on_restore() {
    use md_crdt::codec::{DocOp, Envelope, OpBody};
    use md_crdt::session::snapshot::SnapshotError;
    let mut base = CollaborativeDocument::new(1);
    let q = quote(&mut base, None, "captured-body");
    let tail = base.insert_paragraph(Some(q), "unrelated-body").unwrap();
    let initial = base.save_snapshot().unwrap();
    let frontier = base.state_vector();
    let mut deleter = replica(2, &history(&base));
    deleter.delete_block(q).unwrap();
    let deletion = deleter.encode_changes_since(&frontier).unwrap();
    for duplicate in [false, true] {
        let mut malformed = deletion.clone();
        let mut envelope: Envelope = serde_json::from_slice(&malformed.ops[0].payload).unwrap();
        let OpBody::Doc(DocOp::DeleteBlockById { subtree, .. }) = &mut envelope.body else {
            panic!("delete");
        };
        if duplicate {
            subtree.push(subtree[0]);
        } else {
            subtree[0].origin = tail;
        }
        malformed.ops[0].payload = serde_json::to_vec(&envelope).unwrap().into();
        let mut receiver =
            CollaborativeDocument::rebase_from_snapshot(initial.clone(), 10).unwrap();
        let before = serde_json::to_value(receiver.save_snapshot().unwrap()).unwrap();
        assert!(
            receiver
                .apply_remote(malformed, &ValidationLimits::default())
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(receiver.save_snapshot().unwrap()).unwrap(),
            before
        );
    }
    let mut wrong_origin = deleter.save_snapshot().unwrap();
    wrong_origin.document.block_topology.deletions[0].subtree[0].origin = tail;
    assert!(matches!(
        CollaborativeDocument::restore_from_snapshot(wrong_origin),
        Err(SnapshotError::InvalidState(_))
    ));
    let mut missing_metadata = initial;
    missing_metadata
        .document
        .block_topology
        .initial
        .remove(&block_id_from_op(q));
    assert!(matches!(
        CollaborativeDocument::restore_from_snapshot(missing_metadata),
        Err(SnapshotError::InvalidState(_))
    ));
}

#[test]
fn direct_document_subtree_deletion_preserves_the_same_owned_history_on_restore() {
    let mut base = CollaborativeDocument::new(1);
    let q = quote(&mut base, None, "direct-captured-body");
    let tail = base.insert_paragraph(Some(q), "surviving-tail").unwrap();
    base.move_block(block_id_from_op(q), None, Some(tail))
        .unwrap();
    let mut snapshot = base.save_snapshot().unwrap();
    let mut document = snapshot.document.into_document();
    let current = document.block_elem_id(block_id_from_op(q)).unwrap();
    assert!(document.delete_block_at(
        None,
        current,
        OpId {
            peer: 99,
            counter: 1
        }
    ));
    snapshot.document = md_crdt::session::snapshot::DocumentDto::from_document(&document);
    let restored = restore(
        CollaborativeDocument::restore_from_snapshot(snapshot).unwrap(),
        true,
    );
    assert_eq!(rendered(&restored), "surviving-tail");
    assert!(
        restored
            .save_snapshot()
            .unwrap()
            .document
            .block_topology
            .deleted
            .len()
            >= 2
    );
}

#[test]
fn direct_item_deletion_captures_children_without_absorbing_unaware_entries() {
    for initially_inside in [false, true] {
        let mut base = CollaborativeDocument::new(1);
        let (source, item) = list(&mut base, None, "captured-item-seed");
        let old_item = base.document().find_list_item_by_id(item).unwrap().elem_id;
        let p = base
            .insert_paragraph_in(initially_inside.then_some(old_item), None, "moving-block")
            .unwrap();
        let initial = base.save_snapshot().unwrap();
        let frontier = base.state_vector();
        let mut mover = replica(2, &history(&base));
        mover
            .move_block(
                block_id_from_op(p),
                if initially_inside {
                    None
                } else {
                    Some(old_item)
                },
                None,
            )
            .unwrap();
        let mut deleter = replica(3, &history(&base));
        deleter.delete_list_item(item).unwrap();
        deletion_permutations(
            &initial,
            &[
                mover.encode_changes_since(&frontier).unwrap(),
                deleter.encode_changes_since(&frontier).unwrap(),
            ],
            |receiver| {
                assert!(
                    receiver
                        .document()
                        .find_block_by_id(block_id_from_op(source))
                        .is_some()
                );
                assert!(receiver.document().find_list_item_by_id(item).is_none());
                assert!(!rendered(receiver).contains("captured-item-seed"));
                assert_eq!(
                    receiver
                        .document()
                        .find_block_by_id(block_id_from_op(p))
                        .is_some(),
                    !initially_inside
                );
                if !initially_inside {
                    assert_eq!(
                        receiver.document().block_parent(block_id_from_op(p)),
                        Some(None)
                    );
                }
            },
        );
    }
}

#[test]
fn unseen_physical_delete_binds_the_future_opaque_incarnation_not_a_derived_uuid() {
    use md_crdt::codec::{BlockKindSkeleton, BlockSkeleton, DocOp, Envelope, OpBody, WIRE_VERSION};
    use md_crdt::doc::LogicalContainer;
    use md_crdt::sync::Operation;
    let mut base = CollaborativeDocument::new(1);
    let destination = quote(&mut base, None, "surviving-destination");
    let initial = base.save_snapshot().unwrap();
    let opaque = uuid::Uuid::from_u128(0x7020_eed1_b227_4f62_a6b3_963f_21d0_d5ca);
    let original = OpId {
        peer: 20,
        counter: 1,
    };
    let deletion_id = OpId {
        peer: 30,
        counter: 1,
    };
    let fresh = OpId {
        peer: 30,
        counter: 2,
    };
    let message = |id: OpId, body: DocOp| {
        let mut message = history(&base);
        message.ops = vec![Operation {
            id,
            payload: serde_json::to_vec(&Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(body),
            })
            .unwrap()
            .into(),
        }];
        message
    };
    let create = message(
        original,
        DocOp::InsertBlock {
            parent: None,
            after: None,
            id: original,
            right_origin: None,
            block: BlockSkeleton {
                block_id: opaque,
                kind: BlockKindSkeleton::RawBlock {
                    raw: "opaque-original".into(),
                },
            },
        },
    );
    let deletion = message(
        deletion_id,
        DocOp::DeleteBlock {
            parent: None,
            target: original,
            id: deletion_id,
            observed: StateVector::new(),
            subtree: Vec::new(),
        },
    );
    let recreation = message(
        fresh,
        DocOp::InsertBlock {
            parent: None,
            after: None,
            id: fresh,
            right_origin: None,
            block: BlockSkeleton {
                block_id: opaque,
                kind: BlockKindSkeleton::RawBlock {
                    raw: "opaque-fresh".into(),
                },
            },
        },
    );
    // A fresh declaration can precede its own delete and the foreign creator.
    // It remains unpublished, including after restore, without granting another
    // causally ready producer permission to reuse the still-live identity.
    for checkpoint in [false, true] {
        let mut receiver =
            CollaborativeDocument::rebase_from_snapshot(initial.clone(), 10).unwrap();
        apply(&mut receiver, recreation.clone());
        receiver = restore(receiver, checkpoint);
        assert!(receiver.document().find_block_by_id(opaque).is_none());
        let duplicate_id = OpId {
            peer: 21,
            counter: 1,
        };
        let mut conflicting = create.clone();
        conflicting.ops.extend(
            message(
                duplicate_id,
                DocOp::InsertBlock {
                    parent: None,
                    after: Some(OpId {
                        peer: 99,
                        counter: 1,
                    }),
                    id: duplicate_id,
                    right_origin: None,
                    block: BlockSkeleton {
                        block_id: opaque,
                        kind: BlockKindSkeleton::RawBlock {
                            raw: "unready-duplicate".into(),
                        },
                    },
                },
            )
            .ops,
        );
        let before = receiver.save_snapshot().unwrap();
        assert!(matches!(
            receiver.apply_remote(conflicting, &ValidationLimits::default()),
            Err(md_crdt::SessionError::Validation(md_crdt::sync::ValidationError::MalformedOperation {
                op_id, kind: md_crdt::sync::MalformedKind::InvalidSequence,
            })) if op_id == duplicate_id
        ));
        assert_eq!(receiver.save_snapshot().unwrap(), before);
        apply(&mut receiver, create.clone());
        assert_eq!(receiver.document().block_elem_id(opaque), Some(original));
        assert!(!rendered(&receiver).contains("opaque-fresh"));
        let duplicate = OpId {
            peer: 21,
            counter: 1,
        };
        let duplicate = message(
            duplicate,
            DocOp::InsertBlock {
                parent: None,
                after: None,
                id: duplicate,
                right_origin: None,
                block: BlockSkeleton {
                    block_id: opaque,
                    kind: BlockKindSkeleton::RawBlock {
                        raw: "invalid-duplicate".into(),
                    },
                },
            },
        );
        let before = receiver.save_snapshot().unwrap();
        assert!(matches!(
            receiver.apply_remote(duplicate, &ValidationLimits::default()),
            Err(md_crdt::SessionError::Validation(
                md_crdt::sync::ValidationError::MalformedOperation {
                    kind: md_crdt::sync::MalformedKind::InvalidSequence,
                    ..
                }
            ))
        ));
        assert_eq!(receiver.save_snapshot().unwrap(), before);
        apply(&mut receiver, deletion.clone());
        assert_clean(&receiver);
        assert_eq!(receiver.document().block_elem_id(opaque), Some(fresh));
    }
    let mut mover = replica(40, &history(&base));
    apply(&mut mover, create.clone());
    let after_create = mover.state_vector();
    mover.move_block(opaque, Some(destination), None).unwrap();
    let movement = mover.encode_changes_since(&after_create).unwrap();
    for include_fresh in [false, true] {
        let mut messages = vec![create.clone(), deletion.clone(), movement.clone()];
        if include_fresh {
            messages.push(recreation.clone());
        }
        deletion_permutations(&initial, &messages, |receiver| {
            assert!(!rendered(receiver).contains("opaque-original"));
            assert_eq!(
                receiver.document().find_block_by_id(opaque).is_some(),
                include_fresh
            );
            assert_eq!(rendered(receiver).contains("opaque-fresh"), include_fresh);
            assert!(rendered(receiver).contains("surviving-destination"));
            let state = receiver.save_snapshot().unwrap().document.block_topology;
            assert_eq!(state.deletions.len(), 1);
            assert_eq!(
                state.deletions[0].target.container,
                LogicalContainer::Block(opaque)
            );
            assert_eq!(state.deletions[0].target.origin, original);
            assert!(state.deletions[0].subtree.is_empty());
            if include_fresh {
                assert_eq!(receiver.document().block_elem_id(opaque), Some(fresh));
                assert_eq!(receiver.document().block_parent(opaque), Some(None));
            }
        });
    }
}

#[test]
fn imported_incarnations_survive_observed_escape_without_exempting_later_births() {
    let mut initial = CollaborativeDocument::new(1).save_snapshot().unwrap();
    initial.document = md_crdt::session::snapshot::DocumentDto::from_document(
        &md_crdt::Parser::parse("> imported **body**\r\n\r\n#  Untouched ##\r\n"),
    );
    let base = CollaborativeDocument::restore_from_snapshot(initial.clone()).unwrap();
    let q = base.document().blocks().iter().next().unwrap().elem_id;
    let p = base
        .document()
        .container_children(Some(q))
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .id;
    let mut mover = CollaborativeDocument::rebase_from_snapshot(initial.clone(), 2).unwrap();
    mover.move_block(p, None, None).unwrap();
    let movement = history(&mover);
    let mut deleter = CollaborativeDocument::rebase_from_snapshot(initial.clone(), 3).unwrap();
    apply(&mut deleter, movement.clone());
    let before_delete = deleter.state_vector();
    deleter.delete_block(q).unwrap();
    let mut writer = CollaborativeDocument::rebase_from_snapshot(initial.clone(), 4).unwrap();
    let child = writer
        .insert_paragraph_in(Some(q), None, "late-unseen-birth")
        .unwrap();
    writer
        .move_block(block_id_from_op(child), None, None)
        .unwrap();
    deletion_permutations(
        &initial,
        &[
            movement,
            deleter.encode_changes_since(&before_delete).unwrap(),
            history(&writer),
        ],
        |receiver| {
            assert_eq!(receiver.document().block_parent(p), Some(None));
            assert!(
                receiver
                    .document()
                    .find_block_by_id(block_id_from_op(child))
                    .is_none()
            );
            assert_eq!(
                receiver.state_vector().get(q.peer),
                None,
                "imported identities do not inflate receipt coverage"
            );
            let exact = receiver.document().serialize(EquivalenceMode::Exact);
            assert!(exact.contains("imported **body**"));
            assert!(exact.contains("#  Untouched ##\r\n"));
            assert!(!exact.contains("late-unseen-birth"));
        },
    );
}
