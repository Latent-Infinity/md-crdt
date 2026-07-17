#![cfg(feature = "filesync")]

use md_crdt::filesync::VaultSession;
use md_crdt::{
    BlockProjectionKind, BlockProjectionStructure, EditBatch, ProjectionFields, ProjectionRequest,
    TaskState, WorkspaceEdit, WorkspaceMutation,
};
use serde_json::{Value, json};
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

fn transcript() -> Value {
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
    let item_id = initial_read
        .items
        .iter()
        .find_map(|item| match &item.structure {
            Some(BlockProjectionStructure::ListItems { item_ids }) => Some(item_ids[0]),
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
    let stale_cursor_error = vault
        .descriptor_page("note.md", None, Some(&next_cursor), 3)
        .unwrap_err()
        .to_string();
    let restarted_map = vault.descriptor_page("note.md", None, None, 8).unwrap();
    let current = vault.open_document("note.md").unwrap();
    let mut affected_ids = receipt.changes.updated.clone();
    affected_ids.extend(receipt.changes.moved.iter().copied());
    affected_ids.sort_unstable();
    affected_ids.dedup();
    let affected_read = vault
        .project_blocks("note.md", projection_request(&current, affected_ids))
        .unwrap();

    let map_bytes = serde_json::to_vec(&map).unwrap().len();
    let continuation_bytes = serde_json::to_vec(&map_continuation).unwrap().len();
    let edit_bytes = serde_json::to_vec(&receipt).unwrap().len();
    let restart_bytes = serde_json::to_vec(&restarted_map).unwrap().len();
    let total_core_response_bytes = map_bytes
        + continuation_bytes
        + initial_read.bytes_used
        + edit_bytes
        + restart_bytes
        + affected_read.bytes_used;
    json!({
        "fixture_version": 3,
        "map": map,
        "map_continuation": map_continuation,
        "initial_read": initial_read,
        "edit_receipt": receipt,
        "stale_cursor_error": stale_cursor_error,
        "restarted_map": restarted_map,
        "affected_read": affected_read,
        "response_bytes": {
            "map": map_bytes,
            "map_continuation": continuation_bytes,
            "initial_read": initial_read.bytes_used,
            "edit": edit_bytes,
            "restarted_map": restart_bytes,
            "affected_read": affected_read.bytes_used,
            "total": total_core_response_bytes,
        },
    })
}

#[test]
fn deterministic_map_read_edit_read_transcript_matches_fixture() {
    let actual = transcript();
    if std::env::var_os("MD_CRDT_UPDATE_FIXTURES").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/workspace-projection-transcript-v3.json");
        fs::write(path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
        return;
    }
    let frozen: Value = serde_json::from_str(include_str!(
        "fixtures/workspace-projection-transcript-v3.json"
    ))
    .expect("valid frozen projection transcript");
    assert_eq!(frozen, actual);
}
