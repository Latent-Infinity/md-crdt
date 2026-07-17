#![cfg(feature = "filesync")]

use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{
    ColumnAlignment, ColumnDef, DocumentEditBatch, EditBatch, MarkKind, TargetPrecondition,
    TaskState, WorkspaceEdit, WorkspaceMutation, block_id_from_op,
};
use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

fn batch(handle: &md_crdt::DocumentHandle, operations: Vec<WorkspaceEdit>) -> EditBatch {
    EditBatch {
        document_id: handle.document_id,
        base_revision: handle.revision.clone(),
        operations: operations
            .into_iter()
            .map(WorkspaceMutation::strict)
            .collect(),
    }
}

fn insert_text(
    vault: &mut VaultSession,
    path: &str,
    block_id: md_crdt::BlockId,
    grapheme_offset: usize,
    text: &str,
) -> WorkspaceEdit {
    WorkspaceEdit::InsertText {
        at: vault.text_point(path, block_id, grapheme_offset).unwrap(),
        text: text.into(),
    }
}

fn set_mark(
    vault: &mut VaultSession,
    path: &str,
    block_id: md_crdt::BlockId,
    range: std::ops::Range<usize>,
    kind: MarkKind,
) -> WorkspaceEdit {
    WorkspaceEdit::SetMark {
        range: vault.text_range(path, block_id, range).unwrap(),
        kind,
        attrs: BTreeMap::new(),
    }
}

fn split_block(
    vault: &mut VaultSession,
    path: &str,
    block_id: md_crdt::BlockId,
    grapheme_offset: usize,
) -> WorkspaceEdit {
    WorkspaceEdit::SplitBlock {
        at: vault.text_point(path, block_id, grapheme_offset).unwrap(),
    }
}

#[test]
fn preview_is_non_mutating_and_apply_returns_the_same_compact_delta() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let block_id = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let insert = insert_text(&mut vault, "note.md", block_id, 5, " beta");
    let mark = set_mark(&mut vault, "note.md", block_id, 0..5, MarkKind::Bold);
    let request = batch(&handle, vec![insert, mark]);

    let preview = vault.preview_edit_batch("note.md", &request).unwrap();
    assert_eq!(preview.token.to_string().len(), 32);
    assert_eq!(vault.revision("note.md").unwrap(), handle.revision);
    assert_eq!(preview.changes.updated, vec![block_id]);
    assert_eq!(preview.changes.operation_count, 2);

    let receipt = vault
        .apply_previewed_batch("note.md", request, &preview.token)
        .unwrap();
    assert_eq!(receipt.previous_revision, handle.revision);
    assert_eq!(receipt.revision, preview.revision);
    assert_eq!(receipt.changes, preview.changes);
    assert_eq!(vault.revision("note.md").unwrap(), receipt.revision);
}

#[test]
fn invalid_later_operation_rolls_back_content_revision_and_clock() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let (block_id, next_id, before_text) = {
        let session = vault.session_mut("note.md").unwrap();
        (
            session.document().blocks_in_order()[0].id,
            session.peek_next_id(),
            session
                .document()
                .serialize(md_crdt::EquivalenceMode::Structural),
        )
    };
    let accepted = insert_text(&mut vault, "note.md", block_id, 5, " accepted-on-probe");
    let request = batch(
        &handle,
        vec![
            accepted,
            WorkspaceEdit::InsertText {
                at: md_crdt::TextPoint {
                    block_id: uuid::Uuid::from_u128(u128::MAX),
                    position: md_crdt::TextPosition::Start,
                },
                text: "invalid".into(),
            },
        ],
    );

    assert!(matches!(
        vault.apply_edit_batch("note.md", request),
        Err(VaultError::Session(_))
    ));
    let session = vault.session_mut("note.md").unwrap();
    assert_eq!(session.peek_next_id(), next_id);
    assert_eq!(
        session
            .document()
            .serialize(md_crdt::EquivalenceMode::Structural),
        before_text
    );
    assert_eq!(vault.revision("note.md").unwrap(), handle.revision);
}

#[test]
fn preview_token_rejects_a_different_operation_sequence_without_mutation() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let block_id = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let first_edit = insert_text(&mut vault, "note.md", block_id, 5, " one");
    let first = batch(&handle, vec![first_edit]);
    let preview = vault.preview_edit_batch("note.md", &first).unwrap();
    let second_edit = insert_text(&mut vault, "note.md", block_id, 5, " two");
    let second = batch(&handle, vec![second_edit]);

    assert!(matches!(
        vault.apply_previewed_batch("note.md", second, &preview.token),
        Err(VaultError::PreviewMismatch)
    ));
    assert_eq!(vault.revision("note.md").unwrap(), handle.revision);
}

#[test]
fn multi_document_prevalidation_is_all_or_nothing() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "alpha\n").unwrap();
    fs::write(dir.path().join("b.md"), "beta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let a = vault.open_document("a.md").unwrap();
    let b = vault.open_document("b.md").unwrap();
    let a_id = vault.descriptor_page("a.md", None, None, 1).unwrap().items[0].id;
    let b_id = vault.descriptor_page("b.md", None, None, 1).unwrap().items[0].id;
    let a_next = vault.session_mut("a.md").unwrap().peek_next_id();
    let b_next = vault.session_mut("b.md").unwrap().peek_next_id();

    let a_edit = insert_text(&mut vault, "a.md", a_id, 5, " changed");
    let invalid_range = md_crdt::TextRange {
        start: md_crdt::TextPoint {
            block_id: b_id,
            position: md_crdt::TextPosition::Start,
        },
        end: md_crdt::TextPoint {
            block_id: b_id,
            position: md_crdt::TextPosition::Unit(md_crdt::Anchor {
                elem_id: md_crdt::OpId {
                    peer: u64::MAX,
                    counter: u64::MAX,
                },
                bias: md_crdt::AnchorBias::After,
            }),
        },
    };
    let requests = vec![
        DocumentEditBatch {
            path: "a.md".into(),
            batch: batch(&a, vec![a_edit]),
        },
        DocumentEditBatch {
            path: "b.md".into(),
            batch: batch(
                &b,
                vec![WorkspaceEdit::DeleteText {
                    range: invalid_range,
                }],
            ),
        },
    ];

    assert!(vault.apply_edit_batches(requests).is_err());
    assert_eq!(vault.revision("a.md").unwrap(), a.revision);
    assert_eq!(vault.revision("b.md").unwrap(), b.revision);
    assert_eq!(vault.session_mut("a.md").unwrap().peek_next_id(), a_next);
    assert_eq!(vault.session_mut("b.md").unwrap().peek_next_id(), b_next);
}

#[test]
fn multi_document_batches_enforce_identity_and_uniqueness_then_install_together() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "alpha\n").unwrap();
    fs::write(dir.path().join("b.md"), "beta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let a = vault.open_document("a.md").unwrap();
    let b = vault.open_document("b.md").unwrap();
    let a_id = vault.descriptor_page("a.md", None, None, 1).unwrap().items[0].id;
    let b_id = vault.descriptor_page("b.md", None, None, 1).unwrap().items[0].id;

    let wrong_identity = EditBatch {
        document_id: md_crdt::DocumentId::from_u128(1),
        base_revision: a.revision.clone(),
        operations: Vec::new(),
    };
    assert!(matches!(
        vault.apply_edit_batch("a.md", wrong_identity),
        Err(VaultError::DocumentIdMismatch { .. })
    ));

    let duplicate = DocumentEditBatch {
        path: "a.md".into(),
        batch: batch(&a, Vec::new()),
    };
    assert!(matches!(
        vault.apply_edit_batches(vec![duplicate.clone(), duplicate]),
        Err(VaultError::DuplicateDocumentBatch(path)) if path == std::path::Path::new("a.md")
    ));

    let a_edit = insert_text(&mut vault, "a.md", a_id, 5, " one");
    let b_edit = insert_text(&mut vault, "b.md", b_id, 4, " two");
    let outcome = vault
        .apply_edit_batches(vec![
            DocumentEditBatch {
                path: "a.md".into(),
                batch: batch(&a, vec![a_edit]),
            },
            DocumentEditBatch {
                path: "b.md".into(),
                batch: batch(&b, vec![b_edit]),
            },
        ])
        .unwrap();

    assert_eq!(outcome.receipts.len(), 2);
    assert_ne!(vault.revision("a.md").unwrap(), a.revision);
    assert_ne!(vault.revision("b.md").unwrap(), b.revision);
}

fn apply_one(
    vault: &mut VaultSession,
    path: &str,
    handle: &md_crdt::DocumentHandle,
    operation: WorkspaceEdit,
) -> (md_crdt::BatchReceipt, md_crdt::DocumentHandle) {
    let receipt = vault
        .apply_edit_batch(path, batch(handle, vec![operation]))
        .unwrap();
    let handle = vault.open_document(path).unwrap();
    (receipt, handle)
}

#[test]
fn batch_supports_structural_mark_frontmatter_and_section_operations() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "---\ntitle: old\n---\n# One\n\nalpha\n\n# Two\n\nbeta\n\n> nested\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let mut handle = vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, None, 8).unwrap();
    let first_heading = page.items[0].id;
    let alpha = page.items[1].id;
    let second_heading = page.items[2].id;
    let beta = page.items[3].id;
    let quote = page.items[4].id;

    let (frontmatter, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::SetFrontmatterField {
            key: "title".into(),
            value: Some("new".into()),
        },
    );
    handle = next;
    assert!(frontmatter.changes.updated.is_empty());

    let mark = set_mark(&mut vault, "note.md", alpha, 0..5, MarkKind::Bold);
    let (_, next) = apply_one(&mut vault, "note.md", &handle, mark);
    handle = next;
    let interval_id = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .find_block_by_id(alpha)
        .unwrap()
        .marks
        .iter_active_intervals()
        .next()
        .unwrap()
        .id;
    let (_, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::RemoveMark {
            block_id: alpha,
            interval_id,
        },
    );
    handle = next;

    let split_edit = split_block(&mut vault, "note.md", beta, 2);
    let (split, next) = apply_one(&mut vault, "note.md", &handle, split_edit);
    handle = next;
    let suffix = split.changes.created[0];
    let (merged, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::MergeBlocks {
            left_id: beta,
            right_id: suffix,
        },
    );
    handle = next;
    assert_eq!(merged.changes.deleted, vec![suffix]);

    let (section, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::MoveSection {
            heading_id: second_heading,
            after: None,
        },
    );
    handle = next;
    assert!(section.changes.moved.contains(&second_heading));

    let (moved, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::MoveBlock {
            block_id: alpha,
            parent: Some(quote),
            after: None,
        },
    );
    handle = next;
    assert_eq!(moved.changes.affected_parents, vec![quote]);

    let (inserted, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::InsertParagraph {
            parent: Some(quote),
            after: Some(alpha),
            text: "added".into(),
        },
    );
    handle = next;
    let added = inserted.changes.created[0];
    let (heading, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::InsertHeading {
            parent: None,
            after: Some(first_heading),
            level: 2,
            text: "Inserted".into(),
        },
    );
    handle = next;
    assert_eq!(heading.changes.created.len(), 1);
    let (deleted, _) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::DeleteBlock { block_id: added },
    );
    assert_eq!(deleted.changes.deleted, vec![added]);
}

#[test]
fn batch_supports_table_metadata_rows_and_reorder() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "anchor\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let mut handle = vault.open_document("note.md").unwrap();
    let anchor = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let table_id = block_id_from_op(vault.session_mut("note.md").unwrap().peek_next_id());
    let (inserted, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::InsertTable {
            parent: None,
            after: Some(anchor),
            columns: vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            header: vec!["name".into()],
        },
    );
    handle = next;
    assert_eq!(inserted.changes.created, vec![table_id]);

    let row_one = block_id_from_op(vault.session_mut("note.md").unwrap().peek_next_id());
    let (_, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::InsertTableRow {
            table_id,
            after: None,
            cells: vec!["one".into()],
        },
    );
    handle = next;
    let row_two = block_id_from_op(vault.session_mut("note.md").unwrap().peek_next_id());
    let (_, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::InsertTableRow {
            table_id,
            after: Some(row_one),
            cells: vec!["two".into()],
        },
    );
    handle = next;
    let initial_column = match &vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .find_block_by_id(table_id)
        .unwrap()
        .kind
    {
        md_crdt::BlockKind::Table { table } => table.columns_in_order()[0].id,
        _ => unreachable!(),
    };
    let added_column = block_id_from_op(vault.session_mut("note.md").unwrap().peek_next_id());
    let (_, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::InsertTableColumn {
            table_id,
            after: Some(initial_column),
            alignment: ColumnAlignment::Center,
            header: "score".into(),
        },
    );
    handle = next;
    for operation in [
        WorkspaceEdit::SetTableCell {
            table_id,
            row_id: row_one,
            column_id: added_column,
            value: "10".into(),
        },
        WorkspaceEdit::SetTableColumnAlignment {
            table_id,
            column_id: added_column,
            alignment: ColumnAlignment::Right,
        },
        WorkspaceEdit::MoveTableColumn {
            table_id,
            column_id: added_column,
            after: None,
        },
        WorkspaceEdit::SetTableRowCells {
            table_id,
            row_id: row_one,
            cells: vec!["10".into(), "ONE".into()],
        },
        WorkspaceEdit::SetTableMetadata {
            table_id,
            columns: vec![
                ColumnDef {
                    alignment: ColumnAlignment::Right,
                },
                ColumnDef {
                    alignment: ColumnAlignment::Left,
                },
            ],
            header: vec!["SCORE".into(), "NAME".into()],
        },
        WorkspaceEdit::MoveTableRow {
            table_id,
            row_id: row_two,
            after: None,
        },
        WorkspaceEdit::DeleteTableRow {
            table_id,
            row_id: row_one,
        },
        WorkspaceEdit::DeleteTableColumn {
            table_id,
            column_id: initial_column,
        },
    ] {
        let (receipt, next) = apply_one(&mut vault, "note.md", &handle, operation);
        assert_eq!(receipt.changes.updated, vec![table_id]);
        handle = next;
    }
    let (deleted, _) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::DeleteBlock { block_id: table_id },
    );
    assert_eq!(deleted.changes.deleted, vec![table_id]);
}

#[test]
fn invalid_heading_level_is_rejected_without_mutation() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "body\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let request = batch(
        &handle,
        vec![WorkspaceEdit::InsertHeading {
            parent: None,
            after: None,
            level: 0,
            text: "bad".into(),
        }],
    );
    assert!(matches!(
        vault.apply_edit_batch("note.md", request),
        Err(VaultError::Session(message)) if message.contains("heading level")
    ));
    assert_eq!(vault.revision("note.md").unwrap(), handle.revision);
}

#[test]
fn table_cell_preconditions_accept_other_cell_changes_and_reject_same_cell_changes() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "| first | second |\n| --- | --- |\n| a | b |\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let initial = vault.open_document("note.md").unwrap();
    let (table_id, row_id, first_column, second_column) = {
        let document = vault.session_mut("note.md").unwrap().document();
        let block = document.blocks_in_order()[0];
        let md_crdt::BlockKind::Table { table } = &block.kind else {
            panic!("table expected")
        };
        (
            block.id,
            table.rows_in_order()[0].id,
            table.columns_in_order()[0].id,
            table.columns_in_order()[1].id,
        )
    };
    let first_edit = WorkspaceEdit::SetTableCell {
        table_id,
        row_id,
        column_id: first_column,
        value: "first changed".into(),
    };
    let first_preconditions = vault
        .preconditions_for_edit("note.md", &first_edit)
        .unwrap();
    assert!(matches!(
        first_preconditions.as_slice(),
        [TargetPrecondition::TableCell {
            table_id: actual_table,
            row_id: actual_row,
            column_id: actual_column,
            ..
        }] if (*actual_table, *actual_row, *actual_column)
            == (table_id, row_id, first_column)
    ));

    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &initial,
                vec![WorkspaceEdit::SetTableCell {
                    table_id,
                    row_id,
                    column_id: second_column,
                    value: "second changed".into(),
                }],
            ),
        )
        .unwrap();
    vault
        .apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: initial.document_id,
                base_revision: initial.revision,
                operations: vec![WorkspaceMutation::scoped(first_edit, first_preconditions)],
            },
        )
        .expect("an unrelated cell edit must not make the target cell stale");

    let current = vault.open_document("note.md").unwrap();
    let intended = WorkspaceEdit::SetTableCell {
        table_id,
        row_id,
        column_id: first_column,
        value: "intended".into(),
    };
    let intended_preconditions = vault.preconditions_for_edit("note.md", &intended).unwrap();
    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &current,
                vec![WorkspaceEdit::SetTableCell {
                    table_id,
                    row_id,
                    column_id: first_column,
                    value: "competing".into(),
                }],
            ),
        )
        .unwrap();
    assert!(matches!(
        vault.apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: current.document_id,
                base_revision: current.revision,
                operations: vec![WorkspaceMutation::scoped(intended, intended_preconditions)],
            },
        ),
        Err(VaultError::TargetPrecondition { .. })
    ));
}

#[test]
fn list_task_digest_drives_scoped_conflicts_and_change_summaries() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "- [ ] task\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let initial = vault.open_document("note.md").unwrap();
    let list_id = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let item_id = vault
        .descriptor_page("note.md", Some(list_id), None, 1)
        .unwrap()
        .items[0]
        .id;
    let intended = WorkspaceEdit::SetListItemTask {
        item_id,
        task: Some(TaskState::Checked),
    };
    let intended_preconditions = vault.preconditions_for_edit("note.md", &intended).unwrap();

    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &initial,
                vec![WorkspaceEdit::SetListItemTask {
                    item_id,
                    task: None,
                }],
            ),
        )
        .unwrap();
    assert!(matches!(
        vault.apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: initial.document_id,
                base_revision: initial.revision,
                operations: vec![WorkspaceMutation::scoped(intended, intended_preconditions)],
            },
        ),
        Err(VaultError::TargetPrecondition { .. })
    ));

    let current = vault.open_document("note.md").unwrap();
    let receipt = vault
        .apply_edit_batch(
            "note.md",
            batch(
                &current,
                vec![WorkspaceEdit::SetListItemTask {
                    item_id,
                    task: Some(TaskState::Checked),
                }],
            ),
        )
        .unwrap();
    assert_eq!(receipt.changes.updated, vec![item_id]);
}

#[test]
fn list_item_move_preconditions_bind_source_and_destination_placement() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "- a\n- x\n\n+ b\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let initial = vault.open_document("note.md").unwrap();
    let roots = vault
        .descriptor_page("note.md", None, None, 4)
        .unwrap()
        .items;
    let source_list = roots[0].id;
    let destination_list = roots[1].id;
    let source_items = vault
        .descriptor_page("note.md", Some(source_list), None, 4)
        .unwrap()
        .items;
    let destination_item = vault
        .descriptor_page("note.md", Some(destination_list), None, 1)
        .unwrap()
        .items[0]
        .id;
    let moved_item = source_items[0].id;
    let intended = WorkspaceEdit::MoveListItem {
        item_id: moved_item,
        list_id: destination_list,
        after: Some(destination_item),
    };
    let intended_preconditions = vault.preconditions_for_edit("note.md", &intended).unwrap();
    assert_eq!(intended_preconditions.len(), 3);

    vault
        .apply_edit_batch(
            "note.md",
            batch(
                &initial,
                vec![WorkspaceEdit::MoveListItem {
                    item_id: moved_item,
                    list_id: source_list,
                    after: Some(source_items[1].id),
                }],
            ),
        )
        .unwrap();
    assert!(matches!(
        vault.apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: initial.document_id,
                base_revision: initial.revision,
                operations: vec![WorkspaceMutation::scoped(intended, intended_preconditions)],
            },
        ),
        Err(VaultError::TargetPrecondition { .. })
    ));
}

#[test]
fn strict_batch_with_a_stale_base_revision_is_rejected_without_mutation_or_clock_burn() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let stale_handle = vault.open_document("note.md").unwrap();
    let block_id = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let stale_edit = insert_text(&mut vault, "note.md", block_id, 0, "X");

    // Advance the document so `stale_handle.revision` is no longer current.
    let (before_text, before_next) = {
        let session = vault.session_mut("note.md").unwrap();
        session.insert_text(block_id, 5, " beta").unwrap();
        (
            session
                .document()
                .serialize(md_crdt::EquivalenceMode::Structural),
            session.peek_next_id(),
        )
    };
    assert_ne!(vault.revision("note.md").unwrap(), stale_handle.revision);

    // A strict batch carrying the pre-edit revision must be rejected before any op is applied.
    let request = batch(&stale_handle, vec![stale_edit]);
    assert!(matches!(
        vault.apply_edit_batch("note.md", request),
        Err(VaultError::StaleRevision { .. })
    ));

    // The rejected batch left no content change and did not advance the clock.
    let session = vault.session_mut("note.md").unwrap();
    assert_eq!(
        session
            .document()
            .serialize(md_crdt::EquivalenceMode::Structural),
        before_text
    );
    assert_eq!(session.peek_next_id(), before_next);
}

#[test]
fn empty_batch_retains_strict_stale_revision_behavior() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let stale_handle = vault.open_document("note.md").unwrap();
    let block_id = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(block_id, 5, "!")
        .unwrap();

    assert!(matches!(
        vault.apply_edit_batch("note.md", batch(&stale_handle, Vec::new())),
        Err(VaultError::StaleRevision { .. })
    ));
}

#[test]
fn previewed_batch_rejects_apply_after_an_intervening_edit() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let block_id = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let edit = insert_text(&mut vault, "note.md", block_id, 5, " beta");
    let request = batch(&handle, vec![edit]);

    let preview = vault.preview_edit_batch("note.md", &request).unwrap();

    // An unrelated edit lands between preview and apply, bumping the live revision.
    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(block_id, 0, "Z")
        .unwrap();

    // The previewed batch still carries the pre-preview revision, so apply must be rejected
    // by the revision precondition (the TOCTOU guard) — the matching token cannot let it through.
    assert!(matches!(
        vault.apply_previewed_batch("note.md", request, &preview.token),
        Err(VaultError::StaleRevision { .. })
    ));
}
