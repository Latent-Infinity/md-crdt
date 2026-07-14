#![cfg(feature = "filesync")]

use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{
    ColumnAlignment, ColumnDef, DocumentEditBatch, EditBatch, MarkKind, WorkspaceEdit,
    block_id_from_op,
};
use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

fn batch(handle: &md_crdt::DocumentHandle, operations: Vec<WorkspaceEdit>) -> EditBatch {
    EditBatch {
        document_id: handle.document_id,
        expected_revision: handle.revision.clone(),
        operations,
    }
}

#[test]
fn preview_is_non_mutating_and_apply_returns_the_same_compact_delta() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let block_id = vault.descriptor_page("note.md", None, 0, 1).unwrap().items[0].id;
    let request = batch(
        &handle,
        vec![
            WorkspaceEdit::InsertText {
                block_id,
                grapheme_offset: 5,
                text: " beta".into(),
            },
            WorkspaceEdit::SetMark {
                block_id,
                start: 0,
                end: 5,
                kind: MarkKind::Bold,
                attrs: BTreeMap::new(),
            },
        ],
    );

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
    let request = batch(
        &handle,
        vec![
            WorkspaceEdit::InsertText {
                block_id,
                grapheme_offset: 5,
                text: " accepted-on-probe".into(),
            },
            WorkspaceEdit::InsertText {
                block_id: uuid::Uuid::from_u128(u128::MAX),
                grapheme_offset: 0,
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
    let block_id = vault.descriptor_page("note.md", None, 0, 1).unwrap().items[0].id;
    let first = batch(
        &handle,
        vec![WorkspaceEdit::InsertText {
            block_id,
            grapheme_offset: 5,
            text: " one".into(),
        }],
    );
    let preview = vault.preview_edit_batch("note.md", &first).unwrap();
    let second = batch(
        &handle,
        vec![WorkspaceEdit::InsertText {
            block_id,
            grapheme_offset: 5,
            text: " two".into(),
        }],
    );

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
    let a_id = vault.descriptor_page("a.md", None, 0, 1).unwrap().items[0].id;
    let b_id = vault.descriptor_page("b.md", None, 0, 1).unwrap().items[0].id;
    let a_next = vault.session_mut("a.md").unwrap().peek_next_id();
    let b_next = vault.session_mut("b.md").unwrap().peek_next_id();

    let requests = vec![
        DocumentEditBatch {
            path: "a.md".into(),
            batch: batch(
                &a,
                vec![WorkspaceEdit::InsertText {
                    block_id: a_id,
                    grapheme_offset: 5,
                    text: " changed".into(),
                }],
            ),
        },
        DocumentEditBatch {
            path: "b.md".into(),
            batch: batch(
                &b,
                vec![WorkspaceEdit::DeleteText {
                    block_id: b_id,
                    grapheme_offset: 99,
                    grapheme_count: 1,
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
    let a_id = vault.descriptor_page("a.md", None, 0, 1).unwrap().items[0].id;
    let b_id = vault.descriptor_page("b.md", None, 0, 1).unwrap().items[0].id;

    let wrong_identity = EditBatch {
        document_id: md_crdt::DocumentId::from_u128(1),
        expected_revision: a.revision.clone(),
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

    let outcome = vault
        .apply_edit_batches(vec![
            DocumentEditBatch {
                path: "a.md".into(),
                batch: batch(
                    &a,
                    vec![WorkspaceEdit::InsertText {
                        block_id: a_id,
                        grapheme_offset: 5,
                        text: " one".into(),
                    }],
                ),
            },
            DocumentEditBatch {
                path: "b.md".into(),
                batch: batch(
                    &b,
                    vec![WorkspaceEdit::InsertText {
                        block_id: b_id,
                        grapheme_offset: 4,
                        text: " two".into(),
                    }],
                ),
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
    let page = vault.descriptor_page("note.md", None, 0, 8).unwrap();
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

    let (_, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::SetMark {
            block_id: alpha,
            start: 0,
            end: 5,
            kind: MarkKind::Bold,
            attrs: BTreeMap::new(),
        },
    );
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

    let (split, next) = apply_one(
        &mut vault,
        "note.md",
        &handle,
        WorkspaceEdit::SplitBlock {
            block_id: beta,
            grapheme_offset: 2,
        },
    );
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
    let anchor = vault.descriptor_page("note.md", None, 0, 1).unwrap().items[0].id;
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
    for operation in [
        WorkspaceEdit::SetTableRowCells {
            table_id,
            row_id: row_one,
            cells: vec!["ONE".into()],
        },
        WorkspaceEdit::SetTableMetadata {
            table_id,
            columns: vec![ColumnDef {
                alignment: ColumnAlignment::Right,
            }],
            header: vec!["NAME".into()],
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
fn batch_with_a_stale_expected_revision_is_rejected_without_mutation_or_clock_burn() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let stale_handle = vault.open_document("note.md").unwrap();
    let block_id = vault.descriptor_page("note.md", None, 0, 1).unwrap().items[0].id;

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

    // A batch carrying the pre-edit revision must be rejected before any op is applied.
    let request = batch(
        &stale_handle,
        vec![WorkspaceEdit::InsertText {
            block_id,
            grapheme_offset: 0,
            text: "X".into(),
        }],
    );
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
fn previewed_batch_rejects_apply_after_an_intervening_edit() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let block_id = vault.descriptor_page("note.md", None, 0, 1).unwrap().items[0].id;
    let request = batch(
        &handle,
        vec![WorkspaceEdit::InsertText {
            block_id,
            grapheme_offset: 5,
            text: " beta".into(),
        }],
    );

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
