#![cfg(feature = "filesync")]

use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{
    BlockDraft, BlockKind, BulletMarker, ChangeMessage, CodeFenceStyle, CollaborativeDocument,
    EditBatch, EquivalenceMode, FenceMarker, ListDelimiter, ListItemDraft, ListStyle,
    StructuredEditError, StructuredEditLimits, TaskState, TextBlockKind, ValidationLimits,
    WorkspaceEdit, WorkspaceMutation,
};
use std::fs;
use tempfile::tempdir;

fn batch(handle: &md_crdt::DocumentHandle, edits: Vec<WorkspaceEdit>) -> EditBatch {
    EditBatch {
        document_id: handle.document_id,
        base_revision: handle.revision.clone(),
        operations: edits.into_iter().map(WorkspaceMutation::strict).collect(),
    }
}

fn unordered_style() -> ListStyle {
    ListStyle {
        ordered: false,
        start: 1,
        delimiter: ListDelimiter::Period,
        bullet: BulletMarker::Dash,
        loose: false,
    }
}

fn exchange(source: &CollaborativeDocument, target: &mut CollaborativeDocument) {
    let message = source.encode_changes_since(&target.state_vector()).unwrap();
    target
        .apply_remote(message, &ValidationLimits::default())
        .unwrap();
}

#[test]
fn parser_preserves_ordered_unordered_task_and_fence_metadata() {
    let markdown = "0) zero\n\n1) [x] done\n\n+ [ ] open\n\n~~~rust,ignore\nfn main() {}\n~~~";
    let document = md_crdt::Parser::parse(markdown);
    let blocks = document.blocks_in_order();

    let BlockKind::List { style, items, .. } = &blocks[0].kind else {
        panic!("ordered list")
    };
    assert_eq!(style.start, 0);
    assert_eq!(style.delimiter, ListDelimiter::Parenthesis);
    assert!(style.loose);
    assert_eq!(items.iter().nth(1).unwrap().task, Some(TaskState::Checked));

    let BlockKind::List { style, items, .. } = &blocks[1].kind else {
        panic!("unordered list")
    };
    assert_eq!(style.bullet, BulletMarker::Plus);
    assert_eq!(
        items.iter().next().unwrap().task,
        Some(TaskState::Unchecked)
    );

    let BlockKind::CodeFence { style, info, .. } = &blocks[2].kind else {
        panic!("code fence")
    };
    assert_eq!(style.marker, FenceMarker::Tilde);
    assert_eq!(style.length, 3);
    assert_eq!(info.as_deref(), Some("rust,ignore"));
    assert_eq!(document.serialize(EquivalenceMode::Structural), markdown);
}

#[test]
fn structured_workspace_contract_creates_and_targets_every_non_table_kind() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "seed\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let seed = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;

    let draft = BlockDraft::BlockQuote {
        children: vec![
            BlockDraft::Heading {
                level: 2,
                text: "Work".into(),
            },
            BlockDraft::List {
                style: unordered_style(),
                items: vec![ListItemDraft {
                    task: Some(TaskState::Unchecked),
                    children: vec![BlockDraft::Paragraph {
                        text: "first".into(),
                    }],
                }],
            },
            BlockDraft::CodeFence {
                style: CodeFenceStyle::default(),
                info: Some("rust".into()),
                text: "let x = 1;".into(),
            },
            BlockDraft::RawBlock {
                raw: ":::note".into(),
            },
        ],
    };
    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &handle,
                vec![WorkspaceEdit::InsertBlock {
                    parent: None,
                    after: Some(seed),
                    draft,
                }],
            ),
        )
        .unwrap();

    let quote = vault
        .descriptor_page("note.md", None, None, 8)
        .unwrap()
        .items[1]
        .id;
    let quote_children = vault
        .descriptor_page("note.md", Some(quote), None, 8)
        .unwrap()
        .items;
    let list = quote_children[1].id;
    let code = quote_children[2].id;
    let raw = quote_children[3].id;
    let item = vault
        .descriptor_page("note.md", Some(list), None, 8)
        .unwrap()
        .items[0]
        .id;
    let raw_digest = quote_children[3].node_digest;
    let current = vault.open_document("note.md").unwrap();
    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &current,
                vec![
                    WorkspaceEdit::SetListStyle {
                        list_id: list,
                        style: ListStyle {
                            ordered: true,
                            start: 0,
                            delimiter: ListDelimiter::Parenthesis,
                            bullet: BulletMarker::Dash,
                            loose: false,
                        },
                    },
                    WorkspaceEdit::SetListItemTask {
                        item_id: item,
                        task: Some(TaskState::Checked),
                    },
                    WorkspaceEdit::InsertListItem {
                        list_id: list,
                        after: Some(item),
                        item: ListItemDraft {
                            task: None,
                            children: vec![BlockDraft::Paragraph {
                                text: "second".into(),
                            }],
                        },
                    },
                    WorkspaceEdit::SetCodeFence {
                        block_id: code,
                        style: CodeFenceStyle {
                            marker: FenceMarker::Tilde,
                            length: 4,
                        },
                        info: Some("rust,ignore".into()),
                        text: "let x = 2;".into(),
                    },
                    WorkspaceEdit::ReplaceRawBlock {
                        block_id: raw,
                        expected_digest: raw_digest,
                        raw: ":::warning".into(),
                    },
                    WorkspaceEdit::ConvertTextBlock {
                        block_id: seed,
                        kind: TextBlockKind::Heading { level: 3 },
                    },
                ],
            ),
        )
        .unwrap();

    let current = vault.open_document("note.md").unwrap();
    let second_item = vault
        .descriptor_page("note.md", Some(list), None, 8)
        .unwrap()
        .items[1]
        .id;
    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &current,
                vec![
                    WorkspaceEdit::MoveListItem {
                        item_id: second_item,
                        list_id: list,
                        after: None,
                    },
                    WorkspaceEdit::DeleteListItem {
                        item_id: second_item,
                    },
                ],
            ),
        )
        .unwrap();

    let edited = vault.open_document("note.md").unwrap();
    vault
        .export_markdown("note.md", &edited.revision, edited.disk_fingerprint)
        .unwrap();
    let output = fs::read_to_string(&path).unwrap();
    assert!(output.starts_with("### seed\n\n> ## Work"));
    assert!(output.contains("> 0) [x] first"));
    assert!(!output.contains("second"));
    assert!(output.contains("> ~~~~rust,ignore\n> let x = 2;\n> ~~~~"));
    assert!(output.contains("> :::warning"));

    drop(vault);
    let mut reopened = VaultSession::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        output.trim_end()
    );
}

#[test]
fn structured_preconditions_cover_each_new_target_shape() {
    let document =
        md_crdt::Parser::parse("seed\n\n- [ ] task\n\n> quote\n\n```rust\ncode\n```\n\n:::raw");
    let roots = document.blocks_in_order();
    let seed = roots[0].id;
    let list = roots[1].id;
    let item = match &roots[1].kind {
        BlockKind::List { items, .. } => items.iter().next().unwrap().id,
        _ => panic!("list"),
    };
    let quote = roots[2].id;
    let code = roots[3].id;
    let raw = roots[4].id;
    let cases = [
        (
            WorkspaceEdit::InsertBlock {
                parent: None,
                after: Some(seed),
                draft: BlockDraft::Paragraph { text: "new".into() },
            },
            1,
        ),
        (
            WorkspaceEdit::InsertListItem {
                list_id: list,
                after: Some(item),
                item: ListItemDraft {
                    task: None,
                    children: Vec::new(),
                },
            },
            1,
        ),
        (WorkspaceEdit::DeleteListItem { item_id: item }, 1),
        (
            WorkspaceEdit::MoveListItem {
                item_id: item,
                list_id: list,
                after: None,
            },
            3,
        ),
        (
            WorkspaceEdit::SetListStyle {
                list_id: list,
                style: unordered_style(),
            },
            1,
        ),
        (
            WorkspaceEdit::SetListItemTask {
                item_id: item,
                task: Some(TaskState::Checked),
            },
            1,
        ),
        (
            WorkspaceEdit::WrapBlocks {
                block_ids: vec![seed],
            },
            2,
        ),
        (WorkspaceEdit::UnwrapBlockQuote { block_id: quote }, 1),
        (
            WorkspaceEdit::SetCodeFence {
                block_id: code,
                style: CodeFenceStyle::default(),
                info: None,
                text: "changed".into(),
            },
            1,
        ),
        (
            WorkspaceEdit::ConvertTextBlock {
                block_id: seed,
                kind: TextBlockKind::Heading { level: 2 },
            },
            1,
        ),
        (
            WorkspaceEdit::ReplaceRawBlock {
                block_id: raw,
                expected_digest: 0,
                raw: ":::changed".into(),
            },
            1,
        ),
    ];

    for (edit, expected) in cases {
        assert_eq!(
            document.preconditions_for_edit(&edit).unwrap().len(),
            expected
        );
    }
}

#[test]
fn structured_draft_validation_rejects_each_limit_and_fence_error() {
    let paragraph = BlockDraft::Paragraph {
        text: "four".into(),
    };
    assert!(matches!(
        paragraph.validate(StructuredEditLimits {
            max_bytes: 3,
            ..Default::default()
        }),
        Err(StructuredEditError::Bytes { max_bytes: 3 })
    ));
    assert!(matches!(
        paragraph.validate(StructuredEditLimits {
            max_items: 0,
            ..Default::default()
        }),
        Err(StructuredEditError::Items { max_items: 0 })
    ));
    assert_eq!(
        BlockDraft::Heading {
            level: 0,
            text: "bad".into(),
        }
        .validate(Default::default()),
        Err(StructuredEditError::HeadingLevel)
    );
    assert_eq!(
        BlockDraft::CodeFence {
            style: CodeFenceStyle {
                marker: FenceMarker::Backtick,
                length: 3,
            },
            info: None,
            text: "```".into(),
        }
        .validate(Default::default()),
        Err(StructuredEditError::CodeFence)
    );
    assert_eq!(
        BlockDraft::CodeFence {
            style: CodeFenceStyle::default(),
            info: Some("bad`info".into()),
            text: String::new(),
        }
        .validate(Default::default()),
        Err(StructuredEditError::CodeFenceInfo)
    );
    assert_eq!(
        BlockDraft::List {
            style: ListStyle {
                ordered: true,
                start: 1_000_000_000,
                ..unordered_style()
            },
            items: Vec::new(),
        }
        .validate(Default::default()),
        Err(StructuredEditError::OrderedListStart)
    );

    let mut session = CollaborativeDocument::new(1);
    let list_elem = session
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: unordered_style(),
                items: Vec::new(),
            },
            Default::default(),
        )
        .unwrap();
    let next = session.peek_next_id();
    assert!(matches!(
        session.set_list_style(
            md_crdt::block_id_from_op(list_elem),
            ListStyle {
                ordered: true,
                start: 1_000_000_000,
                ..unordered_style()
            }
        ),
        Err(md_crdt::SessionError::StructuredEdit(
            StructuredEditError::OrderedListStart
        ))
    ));
    assert_eq!(session.peek_next_id(), next);
}

#[test]
fn wrap_and_unwrap_preserve_logical_block_ids() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "one\n\ntwo\n\nthree\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let ids: Vec<_> = vault
        .descriptor_page("note.md", None, None, 8)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.id)
        .collect();
    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &handle,
                vec![WorkspaceEdit::WrapBlocks {
                    block_ids: ids[..2].to_vec(),
                }],
            ),
        )
        .unwrap();
    let wrapped = vault
        .descriptor_page("note.md", None, None, 8)
        .unwrap()
        .items[0]
        .id;
    let children: Vec<_> = vault
        .descriptor_page("note.md", Some(wrapped), None, 8)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(children, ids[..2]);

    let current = vault.open_document("note.md").unwrap();
    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &current,
                vec![WorkspaceEdit::UnwrapBlockQuote { block_id: wrapped }],
            ),
        )
        .unwrap();
    let unwrapped: Vec<_> = vault
        .descriptor_page("note.md", None, None, 8)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(unwrapped, ids);
}

#[test]
fn invalid_nested_draft_is_atomic_and_does_not_burn_the_clock() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "seed\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let next = vault.session_mut("note.md").unwrap().peek_next_id();
    let mut draft = BlockDraft::Paragraph {
        text: "leaf".into(),
    };
    for _ in 0..20 {
        draft = BlockDraft::BlockQuote {
            children: vec![draft],
        };
    }

    assert!(matches!(
        vault.apply_edit_batch(
            "note.md",
            batch(
                &handle,
                vec![WorkspaceEdit::InsertBlock {
                    parent: None,
                    after: None,
                    draft,
                }],
            ),
        ),
        Err(VaultError::Session(_))
    ));
    assert_eq!(vault.session_mut("note.md").unwrap().peek_next_id(), next);
}

#[test]
fn raw_replacement_requires_the_exact_digest() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), ":::note\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let raw = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;

    let error = vault
        .apply_edit_batch(
            "note.md",
            batch(
                &handle,
                vec![WorkspaceEdit::ReplaceRawBlock {
                    block_id: raw,
                    expected_digest: 0,
                    raw: ":::warning".into(),
                }],
            ),
        )
        .unwrap_err();
    assert!(matches!(error, VaultError::Session(_)));
}

#[test]
fn focused_structured_operations_converge_and_survive_snapshot_reopen() {
    let mut first = CollaborativeDocument::new(1);
    let list_elem = first
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: unordered_style(),
                items: vec![
                    ListItemDraft {
                        task: Some(TaskState::Unchecked),
                        children: vec![BlockDraft::Paragraph { text: "one".into() }],
                    },
                    ListItemDraft {
                        task: None,
                        children: vec![BlockDraft::Paragraph { text: "two".into() }],
                    },
                ],
            },
            Default::default(),
        )
        .unwrap();
    let list_id = md_crdt::block_id_from_op(list_elem);
    let code_elem = first
        .insert_draft_in(
            None,
            Some(list_elem),
            &BlockDraft::CodeFence {
                style: CodeFenceStyle::default(),
                info: None,
                text: "old".into(),
            },
            Default::default(),
        )
        .unwrap();
    let raw_elem = first
        .insert_draft_in(
            None,
            Some(code_elem),
            &BlockDraft::RawBlock { raw: ":::a".into() },
            Default::default(),
        )
        .unwrap();
    let code_id = md_crdt::block_id_from_op(code_elem);
    let raw_id = md_crdt::block_id_from_op(raw_elem);
    let items: Vec<_> = first
        .document()
        .list_items(list_id)
        .unwrap()
        .iter()
        .map(|item| (item.id, item.elem_id))
        .collect();
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first
        .set_list_item_task(items[0].0, Some(TaskState::Checked))
        .unwrap();
    second
        .set_list_style(
            list_id,
            ListStyle {
                ordered: true,
                start: 3,
                delimiter: ListDelimiter::Parenthesis,
                ..unordered_style()
            },
        )
        .unwrap();
    first
        .set_code_fence(
            code_id,
            CodeFenceStyle::default(),
            Some("text".into()),
            "first".into(),
        )
        .unwrap();
    second
        .set_code_fence(
            code_id,
            CodeFenceStyle::default(),
            Some("text".into()),
            "second".into(),
        )
        .unwrap();
    first.replace_raw_block(raw_id, ":::first".into()).unwrap();
    second
        .replace_raw_block(raw_id, ":::second".into())
        .unwrap();
    first.delete_list_item(items[1].0).unwrap();
    second.move_list_item(items[1].0, list_id, None).unwrap();

    exchange(&first, &mut second);
    exchange(&second, &mut first);
    assert_eq!(first.document(), second.document());
    let markdown = first.document().serialize(EquivalenceMode::Structural);
    assert!(markdown.contains("3) [x] one"));
    assert!(!markdown.contains("two"));
    assert!(markdown.contains("first"));
    assert!(markdown.contains(":::first"));

    let restored =
        CollaborativeDocument::restore_from_snapshot(first.save_snapshot().unwrap()).unwrap();
    assert_eq!(restored.document(), first.document());
    assert_eq!(restored.state_vector(), first.state_vector());
}

#[test]
fn moved_list_item_remains_addressable_for_later_task_updates() {
    let mut first = CollaborativeDocument::new(1);
    let list_elem = first
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: unordered_style(),
                items: vec![
                    ListItemDraft {
                        task: None,
                        children: vec![BlockDraft::Paragraph { text: "one".into() }],
                    },
                    ListItemDraft {
                        task: None,
                        children: vec![BlockDraft::Paragraph { text: "two".into() }],
                    },
                ],
            },
            Default::default(),
        )
        .unwrap();
    let list_id = md_crdt::block_id_from_op(list_elem);
    let moved_id = first
        .document()
        .list_items(list_id)
        .unwrap()
        .iter()
        .nth(1)
        .unwrap()
        .id;
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first.move_list_item(moved_id, list_id, None).unwrap();
    first
        .set_list_item_task(moved_id, Some(TaskState::Checked))
        .unwrap();
    exchange(&first, &mut second);

    for document in [first.document(), second.document()] {
        assert_eq!(
            document.find_list_item_by_id(moved_id).unwrap().task,
            Some(TaskState::Checked)
        );
    }
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        second.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn causally_newer_low_counter_structured_writes_are_not_dropped() {
    let mut author = CollaborativeDocument::new(10);
    for index in 0..20 {
        author
            .insert_paragraph(None, &format!("padding {index}"))
            .unwrap();
    }
    let list_elem = author
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: unordered_style(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::Paragraph {
                        text: "task".into(),
                    }],
                }],
            },
            Default::default(),
        )
        .unwrap();
    let code_elem = author
        .insert_draft_in(
            None,
            Some(list_elem),
            &BlockDraft::CodeFence {
                style: CodeFenceStyle::default(),
                info: None,
                text: "old".into(),
            },
            Default::default(),
        )
        .unwrap();
    let raw_elem = author
        .insert_draft_in(
            None,
            Some(code_elem),
            &BlockDraft::RawBlock { raw: "old".into() },
            Default::default(),
        )
        .unwrap();
    let text_elem = author.insert_paragraph(Some(raw_elem), "heading").unwrap();
    let list_id = md_crdt::block_id_from_op(list_elem);
    let code_id = md_crdt::block_id_from_op(code_elem);
    let raw_id = md_crdt::block_id_from_op(raw_elem);
    let text_id = md_crdt::block_id_from_op(text_elem);
    let item_id = author
        .document()
        .list_items(list_id)
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .id;

    let mut editor = CollaborativeDocument::new(11);
    exchange(&author, &mut editor);
    assert_eq!(editor.peek_next_id().counter, 1);
    editor
        .set_list_item_task(item_id, Some(TaskState::Checked))
        .unwrap();
    let mut ordered = unordered_style();
    ordered.ordered = true;
    editor.set_list_style(list_id, ordered).unwrap();
    editor
        .set_code_fence(
            code_id,
            CodeFenceStyle::default(),
            Some("rust".into()),
            "new".into(),
        )
        .unwrap();
    editor.replace_raw_block(raw_id, ":::new".into()).unwrap();
    editor
        .convert_text_block(text_id, TextBlockKind::Heading { level: 2 })
        .unwrap();
    exchange(&editor, &mut author);

    assert_eq!(author.document(), editor.document());
    let mut late_joiner = CollaborativeDocument::new(12);
    exchange(&editor, &mut late_joiner);
    assert_eq!(editor.document(), late_joiner.document());
    let markdown = author.document().serialize(EquivalenceMode::Structural);
    assert!(markdown.contains("[x] task"), "{markdown}");
    assert!(markdown.contains("```rust\nnew\n```"), "{markdown}");
    assert!(markdown.contains(":::new"), "{markdown}");
    assert!(markdown.contains("## heading"), "{markdown}");
}

#[test]
fn snapshot_preserves_structured_write_waiting_for_observed_source_history() {
    let mut author = CollaborativeDocument::new(20);
    for index in 0..10 {
        author
            .insert_paragraph(None, &format!("padding {index}"))
            .unwrap();
    }
    let code_elem = author
        .insert_draft_in(
            None,
            None,
            &BlockDraft::CodeFence {
                style: CodeFenceStyle::default(),
                info: None,
                text: "old".into(),
            },
            Default::default(),
        )
        .unwrap();
    let code_id = md_crdt::block_id_from_op(code_elem);
    let mut editor = CollaborativeDocument::new(21);
    exchange(&author, &mut editor);
    editor
        .set_code_fence(
            code_id,
            CodeFenceStyle::default(),
            Some("rust".into()),
            "new".into(),
        )
        .unwrap();

    let mut delayed = CollaborativeDocument::new(22);
    let delta = editor
        .encode_changes_since(&delayed.state_vector())
        .unwrap();
    let result = delayed
        .apply_remote(
            ChangeMessage {
                since: delta.since,
                ops: delta
                    .ops
                    .into_iter()
                    .filter(|operation| operation.id.peer == 21)
                    .collect(),
            },
            &ValidationLimits::default(),
        )
        .unwrap();
    assert_eq!(result.buffered.len(), 1);

    let mut restored =
        CollaborativeDocument::restore_from_snapshot(delayed.save_snapshot().unwrap()).unwrap();
    exchange(&author, &mut restored);
    assert_eq!(editor.document(), restored.document());
}

#[test]
fn concurrent_moves_of_the_same_list_item_converge() {
    let mut first = CollaborativeDocument::new(1);
    let list_elem = first
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: unordered_style(),
                items: ["a", "b", "c"]
                    .into_iter()
                    .map(|text| ListItemDraft {
                        task: None,
                        children: vec![BlockDraft::Paragraph { text: text.into() }],
                    })
                    .collect(),
            },
            Default::default(),
        )
        .unwrap();
    let list_id = md_crdt::block_id_from_op(list_elem);
    let items: Vec<_> = first
        .document()
        .list_items(list_id)
        .unwrap()
        .iter()
        .collect();
    let moved = items[1].id;
    let after = items[2].elem_id;
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first.move_list_item(moved, list_id, Some(after)).unwrap();
    second.move_list_item(moved, list_id, None).unwrap();
    exchange(&first, &mut second);
    exchange(&second, &mut first);

    assert_eq!(first.document(), second.document());
    assert_eq!(
        first.document().serialize(EquivalenceMode::Structural),
        second.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn code_fence_edit_concurrent_with_block_move_converges() {
    let mut first = CollaborativeDocument::new(1);
    let code_elem = first
        .insert_draft_in(
            None,
            None,
            &BlockDraft::CodeFence {
                style: CodeFenceStyle::default(),
                info: None,
                text: "old".into(),
            },
            Default::default(),
        )
        .unwrap();
    let anchor = first.insert_paragraph(Some(code_elem), "anchor").unwrap();
    let code_id = md_crdt::block_id_from_op(code_elem);
    let mut second = CollaborativeDocument::new(2);
    exchange(&first, &mut second);

    first.move_block(code_id, None, Some(anchor)).unwrap();
    second
        .set_code_fence(
            code_id,
            CodeFenceStyle::default(),
            Some("rust".into()),
            "new".into(),
        )
        .unwrap();
    exchange(&first, &mut second);
    exchange(&second, &mut first);

    assert_eq!(first.document(), second.document());
    assert!(
        first
            .document()
            .serialize(EquivalenceMode::Structural)
            .contains("```rust\nnew\n```")
    );
}

#[test]
fn scoped_structured_edits_preserve_unrelated_source_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(
        &path,
        "before  \n\n- [ ] task\n\n```txt\nold\n```\n\nafter  \n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let roots = vault
        .descriptor_page("note.md", None, None, 8)
        .unwrap()
        .items;
    let list_id = roots[1].id;
    let code_id = roots[2].id;
    let item_id = vault
        .descriptor_page("note.md", Some(list_id), None, 1)
        .unwrap()
        .items[0]
        .id;
    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &handle,
                vec![
                    WorkspaceEdit::SetListItemTask {
                        item_id,
                        task: Some(TaskState::Checked),
                    },
                    WorkspaceEdit::SetCodeFence {
                        block_id: code_id,
                        style: CodeFenceStyle::default(),
                        info: Some("txt".into()),
                        text: "new".into(),
                    },
                ],
            ),
        )
        .unwrap();
    let edited = vault.open_document("note.md").unwrap();
    vault
        .export_markdown("note.md", &edited.revision, edited.disk_fingerprint)
        .unwrap();
    let output = fs::read_to_string(path).unwrap();
    assert!(output.starts_with("before  \n\n"));
    assert!(output.ends_with("\n\nafter  \n"));
    assert!(output.contains("- [x] task"));
    assert!(output.contains("```txt\nnew\n```"));
}

#[test]
fn delayed_list_move_survives_arrival_before_insert_and_snapshot() {
    let mut author = CollaborativeDocument::new(1);
    let list_elem = author
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: unordered_style(),
                items: vec![ListItemDraft {
                    task: None,
                    children: vec![BlockDraft::Paragraph {
                        text: "first".into(),
                    }],
                }],
            },
            Default::default(),
        )
        .unwrap();
    let list_id = md_crdt::block_id_from_op(list_elem);
    let first = author
        .document()
        .list_items(list_id)
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .elem_id;
    let mut editor = CollaborativeDocument::new(2);
    let mut delayed = CollaborativeDocument::new(3);
    exchange(&author, &mut editor);
    exchange(&author, &mut delayed);

    let inserted = author
        .insert_list_item_draft(
            list_id,
            Some(first),
            &ListItemDraft {
                task: None,
                children: vec![BlockDraft::Paragraph {
                    text: "second".into(),
                }],
            },
            Default::default(),
        )
        .unwrap();
    let inserted_id = md_crdt::block_id_from_op(inserted);
    exchange(&author, &mut editor);
    editor.move_list_item(inserted_id, list_id, None).unwrap();

    let delta = editor
        .encode_changes_since(&delayed.state_vector())
        .unwrap();
    delayed
        .apply_remote(
            ChangeMessage {
                since: delta.since,
                ops: delta
                    .ops
                    .into_iter()
                    .filter(|operation| operation.id.peer == 2)
                    .collect(),
            },
            &ValidationLimits::default(),
        )
        .unwrap();
    let mut restored =
        CollaborativeDocument::restore_from_snapshot(delayed.save_snapshot().unwrap()).unwrap();
    exchange(&author, &mut restored);

    assert_eq!(restored.document(), editor.document());
    assert!(
        restored
            .document()
            .serialize(EquivalenceMode::Structural)
            .starts_with("- second\n- first")
    );
}
