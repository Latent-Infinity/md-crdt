#![cfg(feature = "filesync")]

use md_crdt::filesync::VaultSession;
use md_crdt::{
    BatchReceipt, BlockDescriptor, BlockDescriptorKind, ChangeSummary, DiskFingerprint, DocumentId,
    EditBatch, ExportOutcome, RevisionToken, VaultId,
};
use std::fs;
use tempfile::tempdir;

#[test]
fn vault_and_document_identity_survive_content_changes_and_reopen() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();

    let mut first = VaultSession::open(dir.path()).unwrap();
    let first_handle = first.open_document("note.md").unwrap();
    fs::write(dir.path().join("note.md"), "alpha beta\n").unwrap();
    first
        .refresh_markdown("note.md", Some(&first_handle.revision), None)
        .unwrap();
    let changed = first.open_document("note.md").unwrap();

    assert_eq!(first_handle.vault_id, changed.vault_id);
    assert_eq!(first_handle.document_id, changed.document_id);
    assert_ne!(first_handle.revision, changed.revision);

    drop(first);
    let mut reopened = VaultSession::open(dir.path()).unwrap();
    let reopened_handle = reopened.open_document("note.md").unwrap();
    assert_eq!(changed.vault_id, reopened_handle.vault_id);
    assert_eq!(changed.document_id, reopened_handle.document_id);
    assert_eq!(changed.revision, reopened_handle.revision);
}

#[test]
fn workspace_contract_types_are_concrete_and_mcp_agnostic() {
    let vault_id = VaultId::from_u128(1);
    let document_id = DocumentId::from_u128(2);
    let revision = RevisionToken::from_u128(3);
    let summary = ChangeSummary {
        created: Vec::new(),
        deleted: Vec::new(),
        moved: Vec::new(),
        updated: Vec::new(),
        affected_parents: Vec::new(),
        affected_sections: Vec::new(),
        operation_count: 0,
        revision: revision.clone(),
    };
    let descriptor = BlockDescriptor {
        id: uuid::Uuid::from_u128(4),
        parent: None,
        order: 0,
        kind: BlockDescriptorKind::Paragraph,
        heading_level: None,
        source_bytes: 5,
        text_bytes: 5,
        content_digest: 9,
    };
    let batch = EditBatch {
        document_id,
        expected_revision: revision.clone(),
        operations: Vec::new(),
    };
    let receipt = BatchReceipt {
        document_id,
        previous_revision: revision.clone(),
        revision: revision.clone(),
        changes: summary,
    };
    let outcome = ExportOutcome {
        document_id,
        revision: revision.clone(),
        disk_fingerprint: None,
        bytes_written: 0,
        changed: false,
        changes: receipt.changes.clone(),
    };

    assert_eq!(vault_id.to_string().len(), 36);
    assert_eq!(document_id.to_string().len(), 36);
    assert_eq!(revision.to_string().len(), 32);
    assert_eq!(descriptor.kind, BlockDescriptorKind::Paragraph);
    assert!(batch.operations.is_empty());
    assert_eq!(receipt.document_id, document_id);
    assert!(!outcome.changed);
}

#[test]
fn corrupt_persistent_identity_fails_closed() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".mdcrdt")).unwrap();
    fs::write(dir.path().join(".mdcrdt/vault_id"), "not-a-uuid\n").unwrap();

    let error = match VaultSession::open(dir.path()) {
        Ok(_) => panic!("corrupt identity unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        md_crdt::filesync::VaultError::InvalidIdentity { .. }
    ));
}

#[test]
fn opaque_identity_helpers_round_trip_without_exposing_revision_structure() {
    let uuid = uuid::Uuid::from_u128(42);
    let vault = VaultId::from_uuid(uuid);
    let document = DocumentId::from_uuid(uuid);
    assert_eq!(vault.as_uuid(), uuid);
    assert_eq!(document.as_uuid(), uuid);
    assert_eq!(vault.to_string().parse::<VaultId>().unwrap(), vault);
    assert_eq!(
        document.to_string().parse::<DocumentId>().unwrap(),
        document
    );

    let revision = RevisionToken::from_u128(42);
    assert_eq!(revision.as_bytes(), &42u128.to_be_bytes());
    assert_eq!(DiskFingerprint(42).to_string(), "000000000000002a");
}
