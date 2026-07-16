#![cfg(feature = "filesync")]

use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{
    CollaborativeDocument, ColumnAlignment, ColumnDef, EditBatch, EquivalenceMode, MarkKind,
    TextPoint, TextPosition, ValidationLimits, WorkspaceEdit, WorkspaceMutation,
    WorkspaceTargetError, block_id_from_op,
};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

fn scoped_batch(
    handle: &md_crdt::DocumentHandle,
    edit: WorkspaceEdit,
    preconditions: Vec<md_crdt::TargetPrecondition>,
) -> EditBatch {
    EditBatch {
        document_id: handle.document_id,
        base_revision: handle.revision.clone(),
        operations: vec![WorkspaceMutation::scoped(edit, preconditions)],
    }
}

#[test]
fn scoped_edit_accepts_an_unrelated_revision_change() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let alpha = page.items[0].id;
    let beta = page.items[1].id;
    let edit = WorkspaceEdit::InsertText {
        at: vault.text_point("note.md", alpha, 5).unwrap(),
        text: "!".into(),
    };
    let preconditions = vault.preconditions_for_edit("note.md", &edit).unwrap();

    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(beta, 4, " changed")
        .unwrap();
    let current_before_apply = vault.revision("note.md").unwrap();
    assert_ne!(current_before_apply, handle.revision);

    let receipt = vault
        .apply_edit_batch("note.md", scoped_batch(&handle, edit, preconditions))
        .unwrap();
    assert_eq!(receipt.previous_revision, current_before_apply);
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        "alpha!\n\nbeta changed"
    );
}

#[test]
fn changed_target_rejects_the_whole_batch_without_clock_burn() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let alpha = page.items[0].id;
    let beta = page.items[1].id;

    let first = WorkspaceEdit::InsertText {
        at: vault.text_point("note.md", beta, 4).unwrap(),
        text: "!".into(),
    };
    let first_preconditions = vault.preconditions_for_edit("note.md", &first).unwrap();
    let second = WorkspaceEdit::DeleteText {
        range: vault.text_range("note.md", alpha, 0..5).unwrap(),
    };
    let second_preconditions = vault.preconditions_for_edit("note.md", &second).unwrap();

    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(alpha, 0, "changed ")
        .unwrap();
    let (before, next_id) = {
        let session = vault.session_mut("note.md").unwrap();
        (
            session.document().serialize(EquivalenceMode::Structural),
            session.peek_next_id(),
        )
    };
    let request = EditBatch {
        document_id: handle.document_id,
        base_revision: handle.revision,
        operations: vec![
            WorkspaceMutation::scoped(first, first_preconditions),
            WorkspaceMutation::scoped(second, second_preconditions),
        ],
    };

    assert!(matches!(
        vault.apply_edit_batch("note.md", request),
        Err(VaultError::TargetPrecondition {
            operation_index: 1,
            source: WorkspaceTargetError::PreconditionMismatch,
        })
    ));
    let session = vault.session_mut("note.md").unwrap();
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        before
    );
    assert_eq!(session.peek_next_id(), next_id);
}

#[test]
fn missing_scoped_preconditions_preserve_strict_revision_behavior() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let alpha = page.items[0].id;
    let beta = page.items[1].id;
    let edit = WorkspaceEdit::InsertText {
        at: vault.text_point("note.md", alpha, 5).unwrap(),
        text: "!".into(),
    };

    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(beta, 4, " changed")
        .unwrap();
    let request = EditBatch {
        document_id: handle.document_id,
        base_revision: handle.revision,
        operations: vec![WorkspaceMutation::strict(edit)],
    };
    assert!(matches!(
        vault.apply_edit_batch("note.md", request),
        Err(VaultError::StaleRevision { .. })
    ));
}

#[test]
fn scoped_validation_rejects_targetless_spoofing_and_reports_capture_errors() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let current = vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let alpha = page.items[0].id;
    let beta = page.items[1].id;
    let target_edit = WorkspaceEdit::InsertText {
        at: vault.text_point("note.md", alpha, 5).unwrap(),
        text: "!".into(),
    };
    let unrelated_precondition = vault
        .preconditions_for_edit("note.md", &target_edit)
        .unwrap();
    let frontmatter = WorkspaceEdit::SetFrontmatterField {
        key: "status".into(),
        value: Some("ready".into()),
    };
    let current_spoof = EditBatch {
        document_id: current.document_id,
        base_revision: current.revision.clone(),
        operations: vec![WorkspaceMutation::scoped(
            frontmatter.clone(),
            unrelated_precondition.clone(),
        )],
    };
    assert!(matches!(
        vault.preview_edit_batch("note.md", &current_spoof),
        Err(VaultError::TargetPrecondition {
            operation_index: 0,
            source: WorkspaceTargetError::PreconditionMismatch,
        })
    ));

    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(beta, 4, "!")
        .unwrap();
    let stale_spoof = EditBatch {
        document_id: current.document_id,
        base_revision: current.revision.clone(),
        operations: vec![WorkspaceMutation::scoped(
            frontmatter,
            unrelated_precondition.clone(),
        )],
    };
    assert!(matches!(
        vault.preview_edit_batch("note.md", &stale_spoof),
        Err(VaultError::StaleRevision { .. })
    ));

    let missing = uuid::Uuid::from_u128(u128::MAX);
    let invalid_target = EditBatch {
        document_id: current.document_id,
        base_revision: current.revision,
        operations: vec![WorkspaceMutation::scoped(
            WorkspaceEdit::InsertText {
                at: TextPoint {
                    block_id: missing,
                    position: TextPosition::Start,
                },
                text: "!".into(),
            },
            unrelated_precondition,
        )],
    };
    assert!(matches!(
        vault.preview_edit_batch("note.md", &invalid_target),
        Err(VaultError::TargetPrecondition {
            operation_index: 0,
            source: WorkspaceTargetError::BlockNotFound { block_id },
        }) if block_id == missing
    ));
}

#[test]
fn text_points_cover_empty_unicode_deleted_and_wrong_block_targets() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "a👍🏽é\n\nother\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let unicode = page.items[0].id;
    let other = page.items[1].id;

    let range = vault.text_range("note.md", unicode, 1..3).unwrap();
    assert_eq!(vault.resolve_text_range("note.md", &range).unwrap(), 1..3);

    let middle = vault.text_point("note.md", unicode, 2).unwrap();
    let wrong = TextPoint {
        block_id: other,
        position: middle.position,
    };
    assert!(matches!(
        vault.resolve_text_point("note.md", &wrong),
        Err(VaultError::Target(WorkspaceTargetError::WrongBlock { .. }))
    ));

    vault
        .session_mut("note.md")
        .unwrap()
        .delete_text(unicode, 2, 1)
        .unwrap();
    assert!(matches!(
        vault.resolve_text_point("note.md", &middle),
        Err(VaultError::Target(
            WorkspaceTargetError::DeletedAnchor { .. }
        ))
    ));

    let empty = WorkspaceEdit::InsertParagraph {
        parent: None,
        after: Some(other),
        text: String::new(),
    };
    let handle = vault.open_document("note.md").unwrap();
    let receipt = vault
        .apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision,
                operations: vec![WorkspaceMutation::strict(empty)],
            },
        )
        .unwrap();
    let empty_id = receipt.changes.created[0];
    let point = vault.text_point("note.md", empty_id, 0).unwrap();
    assert_eq!(point.position, TextPosition::Start);
    assert_eq!(vault.resolve_text_point("note.md", &point).unwrap(), 0);
}

#[test]
fn text_target_boundaries_and_valid_delete_are_explicit() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let alpha = page.items[0].id;
    let beta = page.items[1].id;

    assert!(matches!(
        vault.text_point("note.md", alpha, 6),
        Err(VaultError::Target(
            WorkspaceTargetError::InvalidOffset { .. }
        ))
    ));
    let reversed_start = page.items[0].text_bytes - 1;
    assert!(matches!(
        vault.text_range("note.md", alpha, reversed_start..2),
        Err(VaultError::Target(WorkspaceTargetError::InvalidRange))
    ));
    let empty_start = vault.text_range("note.md", alpha, 0..0).unwrap();
    assert_eq!(empty_start.start.position, TextPosition::Start);
    assert_eq!(empty_start.end.position, TextPosition::Start);
    let empty_end = vault.text_range("note.md", alpha, 5..5).unwrap();
    assert_eq!(empty_end.start.position, TextPosition::End);
    assert_eq!(empty_end.end.position, TextPosition::End);
    let interior = vault.text_range("note.md", alpha, 1..4).unwrap();
    assert!(matches!(
        interior.end.position,
        TextPosition::Unit(md_crdt::Anchor {
            bias: md_crdt::AnchorBias::After,
            ..
        })
    ));
    assert_eq!(
        vault.resolve_text_range("note.md", &interior).unwrap(),
        1..4
    );

    let cross_block = md_crdt::TextRange {
        start: md_crdt::TextPoint {
            block_id: alpha,
            position: TextPosition::Start,
        },
        end: md_crdt::TextPoint {
            block_id: beta,
            position: TextPosition::Start,
        },
    };
    assert!(matches!(
        vault.resolve_text_range("note.md", &cross_block),
        Err(VaultError::Target(WorkspaceTargetError::InvalidRange))
    ));
    let backwards = md_crdt::TextRange {
        start: md_crdt::TextPoint {
            block_id: alpha,
            position: TextPosition::End,
        },
        end: md_crdt::TextPoint {
            block_id: alpha,
            position: TextPosition::Start,
        },
    };
    assert!(matches!(
        vault.resolve_text_range("note.md", &backwards),
        Err(VaultError::Target(WorkspaceTargetError::InvalidRange))
    ));

    let delete_range = vault.text_range("note.md", alpha, 1..2).unwrap();
    vault
        .apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision,
                operations: vec![WorkspaceMutation::strict(WorkspaceEdit::DeleteText {
                    range: delete_range,
                })],
            },
        )
        .unwrap();
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        "apha\n\nbeta"
    );
}

#[test]
fn every_structural_precondition_shape_is_captured_and_invalid_parents_fail_early() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "# one\n\nalpha\n\n# two\n\nbeta\n\n> nested\n\n| A |\n| --- |\n| x |\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let root = vault.descriptor_page("note.md", None, 0, 8).unwrap();
    let first_heading = root.items[0].id;
    let alpha = root.items[1].id;
    let second_heading = root.items[2].id;
    let beta = root.items[3].id;
    let quote = root.items[4].id;
    let table = root.items[5].id;
    let row = {
        let block = vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .find_block_by_id(table)
            .unwrap();
        let md_crdt::BlockKind::Table { table } = &block.kind else {
            panic!("fixture table must parse structurally");
        };
        table.rows.iter().next().unwrap().id
    };
    let columns = vec![ColumnDef {
        alignment: ColumnAlignment::Left,
    }];
    let edits = vec![
        WorkspaceEdit::InsertHeading {
            parent: None,
            after: Some(beta),
            level: 2,
            text: "new".into(),
        },
        WorkspaceEdit::InsertTable {
            parent: None,
            after: Some(beta),
            columns: columns.clone(),
            header: vec!["A".into()],
        },
        WorkspaceEdit::DeleteBlock { block_id: alpha },
        WorkspaceEdit::RemoveMark {
            block_id: alpha,
            interval_id: md_crdt::OpId {
                counter: 1,
                peer: 99,
            },
        },
        WorkspaceEdit::MoveSection {
            heading_id: first_heading,
            after: Some(second_heading),
        },
        WorkspaceEdit::MergeBlocks {
            left_id: alpha,
            right_id: beta,
        },
        WorkspaceEdit::InsertTableRow {
            table_id: table,
            after: Some(row),
            cells: vec!["y".into()],
        },
        WorkspaceEdit::SetTableRowCells {
            table_id: table,
            row_id: row,
            cells: vec!["z".into()],
        },
        WorkspaceEdit::DeleteTableRow {
            table_id: table,
            row_id: row,
        },
        WorkspaceEdit::SetTableMetadata {
            table_id: table,
            columns: columns.clone(),
            header: vec!["B".into()],
        },
        WorkspaceEdit::MoveTableRow {
            table_id: table,
            row_id: row,
            after: None,
        },
    ];
    for edit in edits {
        assert!(
            !vault
                .preconditions_for_edit("note.md", &edit)
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        vault
            .preconditions_for_edit(
                "note.md",
                &WorkspaceEdit::SetFrontmatterField {
                    key: "status".into(),
                    value: Some("ready".into()),
                },
            )
            .unwrap()
            .is_empty()
    );

    let leaf_parent = WorkspaceEdit::InsertParagraph {
        parent: Some(alpha),
        after: None,
        text: "invalid".into(),
    };
    assert!(matches!(
        vault.preconditions_for_edit("note.md", &leaf_parent),
        Err(VaultError::Target(
            WorkspaceTargetError::PreconditionMismatch
        ))
    ));
    let missing_parent = WorkspaceEdit::InsertParagraph {
        parent: Some(uuid::Uuid::from_u128(u128::MAX)),
        after: None,
        text: "invalid".into(),
    };
    assert!(matches!(
        vault.preconditions_for_edit("note.md", &missing_parent),
        Err(VaultError::Target(
            WorkspaceTargetError::BlockNotFound { .. }
        ))
    ));
    let nested = vault
        .descriptor_page("note.md", Some(quote), 0, 1)
        .unwrap()
        .items[0]
        .id;
    let wrong_parent_neighbor = WorkspaceEdit::InsertParagraph {
        parent: Some(quote),
        after: Some(alpha),
        text: "invalid".into(),
    };
    assert!(matches!(
        vault.preconditions_for_edit("note.md", &wrong_parent_neighbor),
        Err(VaultError::Target(
            WorkspaceTargetError::PreconditionMismatch
        ))
    ));
    assert_ne!(nested, alpha);
}

#[test]
fn anchored_target_survives_snapshot_reopen() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    let point = {
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.open_document("note.md").unwrap();
        let block = vault.descriptor_page("note.md", None, 0, 1).unwrap().items[0].id;
        let point = vault.text_point("note.md", block, 2).unwrap();
        vault.save_all_state().unwrap();
        point
    };

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    reopened.open_document("note.md").unwrap();
    assert_eq!(reopened.resolve_text_point("note.md", &point).unwrap(), 2);
}

#[test]
fn text_precondition_rejects_a_semantic_mark_change() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let alpha = vault.descriptor_page("note.md", None, 0, 2).unwrap().items[0].id;
    let edit = WorkspaceEdit::DeleteText {
        range: vault.text_range("note.md", alpha, 0..5).unwrap(),
    };
    let preconditions = vault.preconditions_for_edit("note.md", &edit).unwrap();

    vault
        .session_mut("note.md")
        .unwrap()
        .set_mark(alpha, 0..5, MarkKind::Bold, BTreeMap::new())
        .unwrap();

    assert!(matches!(
        vault.apply_edit_batch("note.md", scoped_batch(&handle, edit, preconditions)),
        Err(VaultError::TargetPrecondition {
            operation_index: 0,
            source: WorkspaceTargetError::PreconditionMismatch,
        })
    ));
}

#[test]
fn placement_precondition_accepts_unrelated_text_but_rejects_sibling_changes() {
    let accepted_dir = tempdir().unwrap();
    fs::write(
        accepted_dir.path().join("note.md"),
        "alpha\n\nbeta\n\ngamma\n",
    )
    .unwrap();
    let mut accepted = VaultSession::open(accepted_dir.path()).unwrap();
    let accepted_handle = accepted.open_document("note.md").unwrap();
    let accepted_page = accepted.descriptor_page("note.md", None, 0, 3).unwrap();
    let accepted_edit = WorkspaceEdit::InsertParagraph {
        parent: None,
        after: Some(accepted_page.items[0].id),
        text: "inserted".into(),
    };
    let accepted_preconditions = accepted
        .preconditions_for_edit("note.md", &accepted_edit)
        .unwrap();
    accepted
        .session_mut("note.md")
        .unwrap()
        .insert_text(accepted_page.items[2].id, 5, "!")
        .unwrap();
    accepted
        .apply_edit_batch(
            "note.md",
            scoped_batch(&accepted_handle, accepted_edit, accepted_preconditions),
        )
        .unwrap();

    let rejected_dir = tempdir().unwrap();
    fs::write(
        rejected_dir.path().join("note.md"),
        "alpha\n\nbeta\n\ngamma\n",
    )
    .unwrap();
    let mut rejected = VaultSession::open(rejected_dir.path()).unwrap();
    let rejected_handle = rejected.open_document("note.md").unwrap();
    let rejected_page = rejected.descriptor_page("note.md", None, 0, 3).unwrap();
    let rejected_edit = WorkspaceEdit::InsertParagraph {
        parent: None,
        after: Some(rejected_page.items[0].id),
        text: "inserted".into(),
    };
    let rejected_preconditions = rejected
        .preconditions_for_edit("note.md", &rejected_edit)
        .unwrap();
    rejected
        .session_mut("note.md")
        .unwrap()
        .insert_paragraph(None, "new sibling")
        .unwrap();

    assert!(matches!(
        rejected.apply_edit_batch(
            "note.md",
            scoped_batch(&rejected_handle, rejected_edit, rejected_preconditions),
        ),
        Err(VaultError::TargetPrecondition {
            operation_index: 0,
            source: WorkspaceTargetError::PreconditionMismatch,
        })
    ));
}

#[test]
fn anchors_survive_marks_and_moves_and_report_cross_block_after_split_merge() {
    let mut session = CollaborativeDocument::new(7);
    let first = session.insert_paragraph(None, "alpha").unwrap();
    let second = session.insert_paragraph(Some(first), "beta").unwrap();
    let first_id = block_id_from_op(first);
    let second_id = block_id_from_op(second);
    let first_point = session.document().text_point(first_id, 2).unwrap();

    session
        .set_mark(first_id, 0..5, MarkKind::Italic, BTreeMap::new())
        .unwrap();
    session.move_block(first_id, None, Some(second)).unwrap();
    assert_eq!(
        session.document().resolve_text_point(&first_point).unwrap(),
        2
    );

    let split = session.split_block(first_id, 1).unwrap();
    let split_id = block_id_from_op(split);
    assert!(matches!(
        session.document().resolve_text_point(&first_point),
        Err(WorkspaceTargetError::DeletedAnchor { .. })
    ));

    let split_point = TextPoint {
        block_id: split_id,
        position: first_point.position,
    };
    assert_eq!(
        session.document().resolve_text_point(&split_point).unwrap(),
        1
    );
    let ambiguous = TextPoint {
        block_id: second_id,
        position: first_point.position,
    };
    assert!(matches!(
        session.document().resolve_text_point(&ambiguous),
        Err(WorkspaceTargetError::AmbiguousAnchor { .. })
    ));
    session.merge_blocks(first_id, split_id).unwrap();
    assert!(matches!(
        session.document().resolve_text_point(&split_point),
        Err(WorkspaceTargetError::BlockNotFound { block_id }) if block_id == split_id
    ));
    let merged_point = TextPoint {
        block_id: first_id,
        position: split_point.position,
    };
    assert!(matches!(
        session.document().resolve_text_point(&merged_point),
        Err(WorkspaceTargetError::DeletedAnchor { .. })
    ));
    assert!(session.document().find_block_by_id(second_id).is_some());
}

#[test]
fn container_block_precondition_includes_descendant_content() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "> nested\n\noutside\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let root = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let quote = root.items[0].id;
    let outside = root.items[1].id;
    let nested = vault
        .descriptor_page("note.md", Some(quote), 0, 1)
        .unwrap()
        .items[0]
        .id;
    let edit = WorkspaceEdit::MoveBlock {
        block_id: quote,
        parent: None,
        after: Some(outside),
    };
    let preconditions = vault.preconditions_for_edit("note.md", &edit).unwrap();

    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(nested, 6, "!")
        .unwrap();

    assert!(matches!(
        vault.apply_edit_batch("note.md", scoped_batch(&handle, edit, preconditions)),
        Err(VaultError::TargetPrecondition {
            operation_index: 0,
            source: WorkspaceTargetError::PreconditionMismatch,
        })
    ));
}

fn run_scoped_peer_schedule(remote_before_batch: bool) {
    let first_dir = tempdir().unwrap();
    let second_dir = tempdir().unwrap();
    fs::write(first_dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    fs::write(second_dir.path().join("note.md"), "").unwrap();
    let mut first = VaultSession::open(first_dir.path()).unwrap();
    let mut second = VaultSession::open(second_dir.path()).unwrap();
    let handle = first.open_document("note.md").unwrap();
    let initial = first
        .encode_changes_since("note.md", &second.state_vector("note.md").unwrap())
        .unwrap();
    second
        .apply_remote("note.md", initial, &ValidationLimits::default())
        .unwrap();
    let page = first.descriptor_page("note.md", None, 0, 2).unwrap();
    let identities: Vec<_> = page.items.iter().map(|item| item.id).collect();
    let edit = WorkspaceEdit::SetMark {
        range: first.text_range("note.md", page.items[0].id, 0..5).unwrap(),
        kind: MarkKind::Bold,
        attrs: BTreeMap::new(),
    };
    let preconditions = first.preconditions_for_edit("note.md", &edit).unwrap();
    let batch = scoped_batch(&handle, edit, preconditions);

    second
        .session_mut("note.md")
        .unwrap()
        .insert_text(page.items[1].id, 4, "!")
        .unwrap();
    if remote_before_batch {
        let remote = second
            .encode_changes_since("note.md", &first.state_vector("note.md").unwrap())
            .unwrap();
        first
            .apply_remote("note.md", remote, &ValidationLimits::default())
            .unwrap();
    }
    first.apply_edit_batch("note.md", batch).unwrap();

    let to_first = second
        .encode_changes_since("note.md", &first.state_vector("note.md").unwrap())
        .unwrap();
    let to_second = first
        .encode_changes_since("note.md", &second.state_vector("note.md").unwrap())
        .unwrap();
    first
        .apply_remote("note.md", to_first, &ValidationLimits::default())
        .unwrap();
    second
        .apply_remote("note.md", to_second, &ValidationLimits::default())
        .unwrap();

    assert_eq!(
        first.state_vector("note.md").unwrap(),
        second.state_vector("note.md").unwrap()
    );
    let first_markdown = first
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    let second_markdown = second
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert_eq!(first_markdown, second_markdown);
    assert_eq!(first_markdown, "**alpha**\n\nbeta!");
    assert_eq!(
        identities,
        first
            .descriptor_page("note.md", None, 0, 2)
            .unwrap()
            .items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn accepted_scoped_batches_converge_in_both_peer_delivery_orders() {
    run_scoped_peer_schedule(false);
    run_scoped_peer_schedule(true);
}

#[test]
fn scoped_replay_accepts_every_unrelated_churn_attempt_while_exact_revision_retries() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    let alpha = page.items[0].id;
    let beta = page.items[1].id;
    let mut strict_retries = 0;
    let mut scoped_first_try = 0;

    for _ in 0..100 {
        let handle = vault.open_document("note.md").unwrap();
        let edit = WorkspaceEdit::InsertText {
            at: vault.text_point("note.md", alpha, 5).unwrap(),
            text: "!".into(),
        };
        let preconditions = vault.preconditions_for_edit("note.md", &edit).unwrap();
        vault
            .session_mut("note.md")
            .unwrap()
            .insert_text(beta, 4, "!")
            .unwrap();

        let strict = EditBatch {
            document_id: handle.document_id,
            base_revision: handle.revision.clone(),
            operations: vec![WorkspaceMutation::strict(edit.clone())],
        };
        strict_retries += usize::from(matches!(
            vault.preview_edit_batch("note.md", &strict),
            Err(VaultError::StaleRevision { .. })
        ));
        let scoped = scoped_batch(&handle, edit, preconditions);
        scoped_first_try += usize::from(vault.preview_edit_batch("note.md", &scoped).is_ok());
    }

    assert_eq!(strict_retries, 100);
    assert_eq!(scoped_first_try, 100);
}

#[test]
fn fuzzy_context_relocation_is_ambiguous_for_repeated_markdown() {
    let markdown = "same **same** same";
    let candidates: Vec<_> = markdown
        .match_indices("same")
        .map(|(offset, _)| offset)
        .collect();
    assert_eq!(candidates, vec![0, 7, 14]);
}

proptest! {
    #[test]
    fn interior_text_points_follow_original_units_through_insertions(
        text in "[a-z]{2,20}",
        point_seed in any::<usize>(),
        insert_seed in any::<usize>(),
    ) {
        let mut session = CollaborativeDocument::new(11);
        let elem = session.insert_paragraph(None, &text).unwrap();
        let block_id = block_id_from_op(elem);
        let len = text.len();
        let point_offset = 1 + point_seed % (len - 1);
        let insert_offset = insert_seed % (len + 1);
        let point = session.document().text_point(block_id, point_offset).unwrap();

        session.insert_text(block_id, insert_offset, "z").unwrap();

        let expected = point_offset + usize::from(insert_offset <= point_offset);
        prop_assert_eq!(session.document().resolve_text_point(&point).unwrap(), expected);
    }
}
