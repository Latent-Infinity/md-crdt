#![cfg(feature = "filesync")]

use md_crdt::DocumentExportRequest;
use md_crdt::filesync::{VaultError, VaultSession};
use serde_json::json;
use std::fs;
use tempfile::tempdir;

#[test]
fn create_rename_delete_preserves_then_retires_document_identity() {
    let dir = tempdir().unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let created = vault.create_markdown("draft.md", "alpha\n").unwrap();

    let renamed = vault
        .rename_markdown(
            "draft.md",
            "notes/final.md",
            &created.revision,
            created.disk_fingerprint,
        )
        .unwrap();
    assert_eq!(renamed.document_id, created.document_id);
    assert!(!dir.path().join("draft.md").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("notes/final.md")).unwrap(),
        "alpha\n"
    );

    drop(vault);
    let mut reopened = VaultSession::open(dir.path()).unwrap();
    let reopened_handle = reopened.open_document("notes/final.md").unwrap();
    assert_eq!(reopened_handle.document_id, created.document_id);
    let deleted = reopened
        .delete_markdown(
            "notes/final.md",
            &reopened_handle.revision,
            reopened_handle.disk_fingerprint,
        )
        .unwrap();
    assert_eq!(deleted.document_id, created.document_id);
    assert!(!dir.path().join("notes/final.md").exists());

    let replacement = reopened
        .create_markdown("notes/final.md", "replacement\n")
        .unwrap();
    assert_ne!(replacement.document_id, created.document_id);
}

#[test]
fn multi_document_export_publishes_every_file_and_cleans_the_journal() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "alpha\n").unwrap();
    fs::write(dir.path().join("b.md"), "beta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let a = vault.open_document("a.md").unwrap();
    assert_eq!(vault.vault_id(), vault.vault_id());
    assert!(!vault.is_open("../outside.md"));
    vault.close("occupied.md").unwrap();
    assert!(!vault.is_open("occupied.md"));
    assert!(matches!(
        vault.ingest_markdown("missing.md", None, None),
        Err(VaultError::PathDoesNotExist(_))
    ));
    let b = vault.open_document("b.md").unwrap();
    let a_id = vault.descriptor_page("a.md", None, None, 1).unwrap().items[0].id;
    let b_id = vault.descriptor_page("b.md", None, None, 1).unwrap().items[0].id;
    vault
        .with_local_edit("a.md", |session| session.insert_text(a_id, 5, " one"))
        .unwrap()
        .value
        .unwrap();
    vault
        .with_local_edit("b.md", |session| session.insert_text(b_id, 4, " two"))
        .unwrap()
        .value
        .unwrap();
    let edited_a = vault.open_document("a.md").unwrap();
    let edited_b = vault.open_document("b.md").unwrap();

    let outcome = vault
        .export_markdown_transaction(vec![
            DocumentExportRequest {
                path: "a.md".into(),
                document_id: a.document_id,
                expected_revision: edited_a.revision,
                expected_disk_fingerprint: edited_a.disk_fingerprint,
            },
            DocumentExportRequest {
                path: "b.md".into(),
                document_id: b.document_id,
                expected_revision: edited_b.revision,
                expected_disk_fingerprint: edited_b.disk_fingerprint,
            },
        ])
        .unwrap();

    assert_eq!(outcome.documents.len(), 2);
    assert!(outcome.documents.iter().all(|document| document.changed));
    assert_eq!(
        fs::read_to_string(dir.path().join("a.md")).unwrap(),
        "alpha one\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("b.md")).unwrap(),
        "beta two\n"
    );
    let transaction_dir = dir.path().join(".mdcrdt/transactions");
    assert_eq!(fs::read_dir(transaction_dir).unwrap().count(), 0);
}

#[test]
fn multi_document_export_prevalidation_writes_nothing_on_one_stale_request() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "alpha\n").unwrap();
    fs::write(dir.path().join("b.md"), "beta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let a = vault.open_document("a.md").unwrap();
    let b = vault.open_document("b.md").unwrap();
    let a_id = vault.descriptor_page("a.md", None, None, 1).unwrap().items[0].id;
    vault
        .with_local_edit("a.md", |session| session.insert_text(a_id, 5, " changed"))
        .unwrap()
        .value
        .unwrap();
    let edited_a = vault.open_document("a.md").unwrap();
    let stale_b = md_crdt::RevisionToken::from_u128(999);

    let error = vault
        .export_markdown_transaction(vec![
            DocumentExportRequest {
                path: "a.md".into(),
                document_id: a.document_id,
                expected_revision: edited_a.revision,
                expected_disk_fingerprint: edited_a.disk_fingerprint,
            },
            DocumentExportRequest {
                path: "b.md".into(),
                document_id: b.document_id,
                expected_revision: stale_b,
                expected_disk_fingerprint: b.disk_fingerprint,
            },
        ])
        .unwrap_err();
    assert!(matches!(error, VaultError::StaleRevision { .. }));
    assert_eq!(
        fs::read_to_string(dir.path().join("a.md")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("b.md")).unwrap(),
        "beta\n"
    );
}

#[test]
fn open_recovers_a_half_applied_multi_file_export_intent() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "new-a\n").unwrap();
    fs::write(dir.path().join("b.md"), "old-b\n").unwrap();
    fs::write(dir.path().join(".a.md.tx.backup"), "old-a\n").unwrap();
    fs::write(dir.path().join(".b.md.tx.pending"), "new-b\n").unwrap();
    fs::create_dir_all(dir.path().join(".mdcrdt/transactions")).unwrap();
    let journal = json!({
        "kind": "export",
        "entries": [
            {
                "target": "a.md",
                "pending": ".a.md.tx.pending",
                "backup": ".a.md.tx.backup"
            },
            {
                "target": "b.md",
                "pending": ".b.md.tx.pending",
                "backup": ".b.md.tx.backup"
            }
        ]
    });
    fs::write(
        dir.path().join(".mdcrdt/transactions/interrupted.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("a.md")).unwrap(),
        "new-a\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("b.md")).unwrap(),
        "new-b\n"
    );
    assert!(!dir.path().join(".a.md.tx.backup").exists());
    assert!(!dir.path().join(".b.md.tx.pending").exists());
    assert!(
        !dir.path()
            .join(".mdcrdt/transactions/interrupted.json")
            .exists()
    );

    let b = vault.open_document("b.md").unwrap();
    assert_eq!(
        vault.descriptor_page("b.md", None, None, 1).unwrap().items[0].text_bytes,
        "new-b".len()
    );
    assert_eq!(
        b.disk_fingerprint,
        vault.open_document("b.md").unwrap().disk_fingerprint
    );
}

#[test]
fn open_completes_interrupted_rename_and_delete_intents() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("rename-me.md"), "rename\n").unwrap();
    fs::write(dir.path().join("delete-me.md"), "delete\n").unwrap();
    let (rename_id, delete_id) = {
        let mut vault = VaultSession::open(dir.path()).unwrap();
        (
            vault.open_document("rename-me.md").unwrap().document_id,
            vault.open_document("delete-me.md").unwrap().document_id,
        )
    };
    let transactions = dir.path().join(".mdcrdt/transactions");
    fs::create_dir_all(&transactions).unwrap();
    fs::write(
        transactions.join("rename.json"),
        serde_json::to_vec(&json!({
            "kind": "rename",
            "from": "rename-me.md",
            "to": "renamed.md",
            "document_id": rename_id,
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        transactions.join("delete.json"),
        serde_json::to_vec(&json!({
            "kind": "delete",
            "path": "delete-me.md",
            "document_id": delete_id,
        }))
        .unwrap(),
    )
    .unwrap();

    let mut recovered = VaultSession::open(dir.path()).unwrap();
    assert!(!dir.path().join("rename-me.md").exists());
    assert!(!dir.path().join("delete-me.md").exists());
    assert_eq!(
        recovered.open_document("renamed.md").unwrap().document_id,
        rename_id
    );
    assert_eq!(fs::read_dir(transactions).unwrap().count(), 0);
}

#[test]
fn lifecycle_and_export_preconditions_reject_ambiguous_requests() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "alpha\n").unwrap();
    fs::write(dir.path().join("occupied.md"), "occupied\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let a = vault.open_document("a.md").unwrap();

    assert!(matches!(
        vault.create_markdown("a.md", "replacement\n"),
        Err(VaultError::PathAlreadyExists(_))
    ));
    assert!(matches!(
        vault.rename_markdown("a.md", "occupied.md", &a.revision, a.disk_fingerprint),
        Err(VaultError::PathAlreadyExists(_))
    ));

    let wrong_identity = DocumentExportRequest {
        path: "a.md".into(),
        document_id: md_crdt::DocumentId::from_u128(1),
        expected_revision: a.revision.clone(),
        expected_disk_fingerprint: a.disk_fingerprint,
    };
    assert!(matches!(
        vault.export_markdown_transaction(vec![wrong_identity]),
        Err(VaultError::DocumentIdMismatch { .. })
    ));

    let request = DocumentExportRequest {
        path: "a.md".into(),
        document_id: a.document_id,
        expected_revision: a.revision.clone(),
        expected_disk_fingerprint: a.disk_fingerprint,
    };
    assert!(matches!(
        vault.export_markdown_transaction(vec![request.clone(), request.clone()]),
        Err(VaultError::DuplicateDocumentBatch(path)) if path == std::path::Path::new("a.md")
    ));

    let unchanged = vault.export_markdown_transaction(vec![request]).unwrap();
    assert_eq!(unchanged.documents.len(), 1);
    assert!(!unchanged.documents[0].changed);
    let transaction_dir = dir.path().join(".mdcrdt/transactions");
    assert!(!transaction_dir.exists() || fs::read_dir(transaction_dir).unwrap().next().is_none());

    let recovered = vault.recover_transactions().unwrap();
    assert_eq!(recovered.transactions_recovered, 0);
    assert_eq!(recovered.files_recovered, 0);

    fs::write(dir.path().join("a.md"), "externally changed\n").unwrap();
    assert!(matches!(
        vault.export_markdown("a.md", &a.revision, a.disk_fingerprint),
        Err(VaultError::StaleDisk { .. })
    ));
    assert!(matches!(
        vault.rename_markdown("a.md", "renamed.md", &a.revision, a.disk_fingerprint),
        Err(VaultError::StaleDisk { .. })
    ));
    assert!(matches!(
        vault.delete_markdown("a.md", &a.revision, a.disk_fingerprint),
        Err(VaultError::StaleDisk { .. })
    ));
    assert!(matches!(
        vault.export_markdown_transaction(vec![DocumentExportRequest {
            path: "a.md".into(),
            document_id: a.document_id,
            expected_revision: a.revision,
            expected_disk_fingerprint: a.disk_fingerprint,
        }]),
        Err(VaultError::StaleDisk { .. })
    ));
    assert!(dir.path().join("a.md").exists());
    assert!(!dir.path().join("renamed.md").exists());
}

#[test]
fn recovery_sweeps_orphan_transaction_pendings_left_without_a_journal() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();

    // Crash after content pendings were fsynced but before the journal landed: real-UUID
    // transaction temps with no journal referencing them, plus the dir the transaction created.
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let orphan_pending = dir.path().join(format!(".note.md.{uuid}.pending"));
    let orphan_backup = dir.path().join(format!(".note.md.{uuid}.backup"));
    fs::write(&orphan_pending, "uncommitted\n").unwrap();
    fs::write(&orphan_backup, "prior\n").unwrap();
    fs::create_dir_all(dir.path().join(".mdcrdt/transactions")).unwrap();

    // Unrelated dotfiles that must survive: no `.pending`/`.backup` suffix, or a non-UUID segment.
    let editor_swap = dir.path().join(".note.md.swp");
    let non_uuid_pending = dir.path().join(".draft.notauuid.pending");
    fs::write(&editor_swap, "x").unwrap();
    fs::write(&non_uuid_pending, "x").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();

    assert!(!orphan_pending.exists(), "orphan pending must be swept");
    assert!(!orphan_backup.exists(), "orphan backup must be swept");
    assert!(editor_swap.exists(), "unrelated dotfile must be preserved");
    assert!(
        non_uuid_pending.exists(),
        "non-UUID .pending must be preserved (strict match)"
    );
    // The real document is untouched — the interrupted transaction never committed.
    assert_eq!(
        fs::read_to_string(dir.path().join("note.md")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        vault.open_document("note.md").unwrap().disk_fingerprint,
        vault.open_document("note.md").unwrap().disk_fingerprint
    );
}
