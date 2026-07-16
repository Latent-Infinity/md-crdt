#![cfg(feature = "filesync")]

use md_crdt::filesync::VaultSession;
use md_crdt::{EditBatch, ProjectionFields, ProjectionRequest, WorkspaceEdit, WorkspaceMutation};
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
        "# Alpha\n\nfirst **bold**\n\n# Beta\n\nsecond\n",
    )
    .unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let map = vault.descriptor_page("note.md", None, None, 4).unwrap();
    let section_ids = map.items[..2].iter().map(|item| item.id).collect();
    let initial_read = vault
        .project_blocks("note.md", projection_request(&handle, section_ids))
        .unwrap();

    let paragraph_id = map.items[1].id;
    let edit = WorkspaceEdit::InsertText {
        at: vault.text_point("note.md", paragraph_id, 5).unwrap(),
        text: "!".into(),
    };
    let receipt = vault
        .apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision,
                operations: vec![WorkspaceMutation::strict(edit)],
            },
        )
        .unwrap();
    let current = vault.open_document("note.md").unwrap();
    let affected_read = vault
        .project_blocks(
            "note.md",
            projection_request(&current, receipt.changes.updated.clone()),
        )
        .unwrap();

    let map_bytes = serde_json::to_vec(&map).unwrap().len();
    let edit_bytes = serde_json::to_vec(&receipt).unwrap().len();
    let total_core_response_bytes =
        map_bytes + initial_read.bytes_used + edit_bytes + affected_read.bytes_used;
    json!({
        "fixture_version": 2,
        "map": map,
        "initial_read": initial_read,
        "edit_receipt": receipt,
        "affected_read": affected_read,
        "response_bytes": {
            "map": map_bytes,
            "initial_read": initial_read.bytes_used,
            "edit": edit_bytes,
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
            .join("tests/fixtures/workspace-projection-transcript-v2.json");
        fs::write(path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
        return;
    }
    let frozen: Value = serde_json::from_str(include_str!(
        "fixtures/workspace-projection-transcript-v2.json"
    ))
    .expect("valid frozen projection transcript");
    assert_eq!(frozen, actual);
}
