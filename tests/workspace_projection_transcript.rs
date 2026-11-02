#![cfg(feature = "filesync")]

use md_crdt::filesync::VaultSession;
use md_crdt::{
    BlockProjectionKind, BlockProjectionStructure, EditBatch, ProjectionFields, ProjectionRequest,
    TaskState, WorkspaceEdit, WorkspaceMutation,
};
use std::fs;
use tempfile::tempdir;

fn projection_request(
    handle: &md_crdt::DocumentHandle,
    block_ids: Vec<md_crdt::BlockId>,
) -> ProjectionRequest {
    ProjectionRequest {
        document_id: handle.document_id,
        base_revision: handle.revision.clone(),
        block_ids,
        fields: ProjectionFields::SEMANTIC,
        max_items: 8,
        max_bytes: 16 * 1024,
        continuation: None,
    }
}

#[test]
fn paged_projection_tracks_structured_edits_and_rejects_stale_cursors() {
    let dir = tempdir().unwrap();
    let metadata = dir.path().join(".mdcrdt");
    fs::create_dir_all(metadata.join("document_ids")).unwrap();
    fs::write(metadata.join("peer_id"), "7\n").unwrap();
    fs::write(
        metadata.join("vault_id"),
        "00000000-0000-0000-0000-000000000001\n",
    )
    .unwrap();
    fs::write(
        metadata.join("document_ids/note.md.id"),
        "00000000-0000-0000-0000-000000000002\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("note.md"),
        "intro **bold**\n\n- [ ] task\n\n| Name | Value |\n| --- | --- |\n| one | 1 |\n\n> quoted\n\n```rust\nlet x = 1;\n```\n",
    )
    .unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let map = vault.descriptor_page("note.md", None, None, 3).unwrap();
    let next_cursor = map.next_cursor.clone().unwrap();
    let map_continuation = vault
        .descriptor_page("note.md", None, Some(&next_cursor), 3)
        .unwrap();
    let block_ids = map
        .items
        .iter()
        .chain(&map_continuation.items)
        .map(|item| item.id)
        .collect::<Vec<_>>();
    let initial_read = vault
        .project_blocks("note.md", projection_request(&handle, block_ids))
        .unwrap();

    let paragraph_id = initial_read
        .items
        .iter()
        .find(|item| matches!(item.kind, Some(BlockProjectionKind::Paragraph)))
        .unwrap()
        .id;
    let (list_id, item_id) = initial_read
        .items
        .iter()
        .find_map(|item| match &item.structure {
            Some(BlockProjectionStructure::ListItems { item_ids }) => Some((item.id, item_ids[0])),
            _ => None,
        })
        .unwrap();
    let (table_id, row_id, column_id) = initial_read
        .items
        .iter()
        .find_map(|item| match &item.structure {
            Some(BlockProjectionStructure::Table { columns, rows, .. }) => {
                Some((item.id, rows[0].id, columns[0].id))
            }
            _ => None,
        })
        .unwrap();
    let quote_id = initial_read
        .items
        .iter()
        .find(|item| matches!(item.kind, Some(BlockProjectionKind::BlockQuote)))
        .unwrap()
        .id;
    let quote_child_id = match &initial_read
        .items
        .iter()
        .find(|item| item.id == quote_id)
        .unwrap()
        .structure
    {
        Some(BlockProjectionStructure::Children { block_ids }) => block_ids[0],
        _ => panic!("quote child projection retained"),
    };
    let (code_id, code_style) = initial_read
        .items
        .iter()
        .find_map(|item| match &item.kind {
            Some(BlockProjectionKind::CodeFence { style, .. }) => Some((item.id, *style)),
            _ => None,
        })
        .unwrap();
    let anchored_point = vault.text_point("note.md", paragraph_id, 5).unwrap();
    let receipt = vault
        .apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision,
                operations: vec![
                    WorkspaceMutation::strict(WorkspaceEdit::InsertText {
                        at: anchored_point,
                        text: "!".into(),
                    }),
                    WorkspaceMutation::strict(WorkspaceEdit::SetListItemTask {
                        item_id,
                        task: Some(TaskState::Checked),
                    }),
                    WorkspaceMutation::strict(WorkspaceEdit::SetTableCell {
                        table_id,
                        row_id,
                        column_id,
                        value: "updated".into(),
                    }),
                    WorkspaceMutation::strict(WorkspaceEdit::UnwrapBlockQuote {
                        block_id: quote_id,
                    }),
                    WorkspaceMutation::strict(WorkspaceEdit::SetCodeFence {
                        block_id: code_id,
                        style: code_style,
                        info: Some("rust,ignore".into()),
                        text: "let x = 2;".into(),
                    }),
                ],
            },
        )
        .unwrap();
    assert!(matches!(
        vault.descriptor_page("note.md", None, Some(&next_cursor), 3),
        Err(md_crdt::filesync::VaultError::Descriptor(
            md_crdt::DescriptorError::CursorRevisionMismatch { .. }
        ))
    ));
    let restarted_map = vault.descriptor_page("note.md", None, None, 8).unwrap();
    let current = vault.open_document("note.md").unwrap();
    let mut affected_ids = receipt.changes.updated.clone();
    affected_ids.extend(receipt.changes.moved.iter().copied());
    affected_ids.sort_unstable();
    affected_ids.dedup();
    let affected_read = vault
        .project_blocks("note.md", projection_request(&current, affected_ids))
        .unwrap();

    assert_eq!(
        restarted_map
            .items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![paragraph_id, list_id, table_id, quote_child_id, code_id],
        "unwrapping promotes the original child identity instead of duplicating it"
    );
    let projected = |id| {
        affected_read
            .items
            .iter()
            .find(|item| item.id == id)
            .unwrap()
    };
    assert_eq!(projected(paragraph_id).text.as_deref(), Some("intro! bold"));
    assert!(matches!(
        projected(item_id).kind,
        Some(BlockProjectionKind::ListItem {
            task: Some(TaskState::Checked)
        })
    ));
    let Some(BlockProjectionStructure::Table { columns, rows, .. }) =
        &projected(table_id).structure
    else {
        panic!("edited table projection retained")
    };
    assert_eq!(columns[0].id, column_id);
    assert_eq!(rows[0].id, row_id);
    assert_eq!(rows[0].cells, ["updated", "1"]);
    assert_eq!(projected(quote_child_id).text.as_deref(), Some("quoted"));
    assert_eq!(
        projected(quote_child_id).kind,
        Some(BlockProjectionKind::Paragraph)
    );
    assert_eq!(projected(code_id).text.as_deref(), Some("let x = 2;"));
    assert_eq!(
        projected(code_id).kind,
        Some(BlockProjectionKind::CodeFence {
            style: code_style,
            info: Some("rust,ignore".into()),
        })
    );
}
