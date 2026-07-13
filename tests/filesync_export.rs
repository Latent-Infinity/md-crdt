#![cfg(feature = "filesync")]

use md_crdt::filesync::{VaultError, VaultSession};
use std::fs;
use tempfile::tempdir;

#[test]
fn export_is_revision_checked_durable_and_exact_after_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "#  Title  ##\r\n\r\nalpha beta\r\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let opened = vault.open_document("note.md").unwrap();
    let block_id = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[1]
        .id;
    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(block_id, 6, "brave ")
        .unwrap();
    let edited = vault.open_document("note.md").unwrap();

    let stale = vault
        .export_markdown("note.md", &opened.revision, opened.disk_fingerprint)
        .unwrap_err();
    assert!(matches!(stale, VaultError::StaleRevision { .. }));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "#  Title  ##\r\n\r\nalpha beta\r\n"
    );

    let outcome = vault
        .export_markdown("note.md", &edited.revision, edited.disk_fingerprint)
        .unwrap();
    assert!(outcome.changed);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "#  Title  ##\r\n\r\nalpha brave beta\r\n"
    );

    drop(vault);
    let mut reopened = VaultSession::open(dir.path()).unwrap();
    let handle = reopened.open_document("note.md").unwrap();
    assert_eq!(handle.document_id, outcome.document_id);
    assert_eq!(handle.revision, outcome.revision);
}

#[test]
fn snapshot_save_does_not_publish_markdown() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let block_id = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id;
    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(block_id, 5, " beta")
        .unwrap();

    vault.save_state("note.md").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\n");
}

#[test]
fn export_rejects_an_unexpected_disk_fingerprint() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let opened = vault.open_document("note.md").unwrap();
    fs::write(&path, "external\n").unwrap();

    let error = vault
        .export_markdown("note.md", &opened.revision, opened.disk_fingerprint)
        .unwrap_err();
    assert!(matches!(error, VaultError::StaleDisk { .. }));
    assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");
}

#[test]
fn refresh_rejects_stale_disk_and_revision_without_mutating_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let opened = vault.open_document("note.md").unwrap();

    fs::write(&path, "external\n").unwrap();
    let disk_error = vault
        .refresh_markdown("note.md", Some(&opened.revision), opened.disk_fingerprint)
        .unwrap_err();
    assert!(matches!(disk_error, VaultError::StaleDisk { .. }));
    assert_eq!(vault.revision("note.md").unwrap(), opened.revision);

    let block_id = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id;
    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(block_id, 5, " local")
        .unwrap();
    let edited_revision = vault.revision("note.md").unwrap();

    let revision_error = vault
        .ingest_markdown("note.md", Some(&opened.revision), None)
        .unwrap_err();
    assert!(matches!(revision_error, VaultError::StaleRevision { .. }));
    assert_eq!(vault.revision("note.md").unwrap(), edited_revision);
}

#[test]
fn re_export_of_unchanged_document_does_not_rewrite() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let block_id = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id;
    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(block_id, 5, " beta")
        .unwrap();
    let edited = vault.open_document("note.md").unwrap();

    let first = vault
        .export_markdown("note.md", &edited.revision, edited.disk_fingerprint)
        .unwrap();
    assert!(first.changed);
    let published = fs::read_to_string(&path).unwrap();

    // Re-export with the post-export revision/fingerprint: content already matches disk,
    // so the write path is skipped and the file is left byte-identical.
    let second = vault
        .export_markdown("note.md", &first.revision, first.disk_fingerprint)
        .unwrap();
    assert!(!second.changed, "unchanged re-export must not rewrite");
    assert_eq!(fs::read_to_string(&path).unwrap(), published);
    assert_eq!(second.revision, first.revision);
}

#[test]
fn export_publishes_into_a_nested_subdirectory() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("projects")).unwrap();
    let path = dir.path().join("projects/alpha.md");
    fs::write(&path, "hello\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("projects/alpha.md").unwrap();
    let block_id = vault
        .session_mut("projects/alpha.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id;
    vault
        .session_mut("projects/alpha.md")
        .unwrap()
        .insert_text(block_id, 5, " world")
        .unwrap();
    let edited = vault.open_document("projects/alpha.md").unwrap();

    let outcome = vault
        .export_markdown(
            "projects/alpha.md",
            &edited.revision,
            edited.disk_fingerprint,
        )
        .unwrap();
    assert!(outcome.changed);
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello world\n");
    // Temp file for the subdirectory write is cleaned up.
    assert!(!dir.path().join("projects/.alpha.md.md-crdt.tmp").exists());
}
