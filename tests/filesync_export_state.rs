//! Export state: whether a document's persisted CRDT state may hold content
//! its Markdown file does not.

#![cfg(feature = "filesync")]

use md_crdt::doc::EquivalenceMode;
use md_crdt::filesync::{ExportState, Vault, VaultError, VaultSession};
use std::fs;
use tempfile::tempdir;

/// Edit a document in place without exporting, leaving the session persisted.
fn edit_without_exporting(vault: &mut VaultSession, rel: &str, text: &str) {
    let block_id = vault.session_mut(rel).unwrap().document().blocks_in_order()[0].id;
    vault
        .session_mut(rel)
        .unwrap()
        .insert_text(block_id, 0, text)
        .unwrap();
    vault.save_state(rel).unwrap();
}

#[test]
fn a_document_that_was_never_opened_is_exported() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();

    let vault = Vault::open(dir.path()).unwrap();

    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Exported
    );
}

#[test]
fn export_state_creates_nothing() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();

    let vault = Vault::open(dir.path()).unwrap();
    vault.export_state("note.md").unwrap();

    assert!(
        !dir.path().join(".mdcrdt").exists(),
        "asking the question must not create state; the read path depends on this"
    );
}

#[test]
fn an_edit_that_was_never_exported_is_reported_after_reopening() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    {
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.open_document("note.md").unwrap();
        edit_without_exporting(&mut vault, "note.md", " local");
    }

    let reopened = VaultSession::open(dir.path()).unwrap();

    assert_eq!(
        reopened.export_state("note.md").unwrap(),
        ExportState::Unexported,
        "the file does not hold this edit and something must say so"
    );
}

#[test]
fn exporting_clears_the_report() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    edit_without_exporting(&mut vault, "note.md", " local");
    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Unexported
    );

    let revision = vault.revision("note.md").unwrap();
    vault
        .export_markdown("note.md", &revision, handle.disk_fingerprint)
        .unwrap();

    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Exported
    );
    assert!(fs::read_to_string(&path).unwrap().contains(" local"));
}

#[test]
fn ingesting_an_external_change_clears_the_report() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    edit_without_exporting(&mut vault, "note.md", " local");

    fs::write(&path, "external\n").unwrap();
    let revision = vault.revision("note.md").unwrap();
    vault
        .refresh_markdown("note.md", Some(&revision), None)
        .unwrap();

    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Exported,
        "the session now matches the file, so nothing is unexported"
    );
}

#[test]
fn explicit_refresh_discards_unexported_work_when_the_file_is_unchanged() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    edit_without_exporting(&mut vault, "note.md", "local ");
    let revision = vault.revision("note.md").unwrap();

    vault
        .refresh_markdown("note.md", Some(&revision), None)
        .unwrap();

    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Exported,
        "an explicit refresh accepts disk even when its bytes did not change"
    );
    let refreshed = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert_eq!(refreshed, "alpha");
}

#[test]
fn failed_refresh_preserves_the_clean_session_and_allows_a_later_repair() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "# Note\n\nbefore\n").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    let opened = vault.open_document("note.md").unwrap();
    let before = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);

    fs::write(
        &path,
        "# Note\n\n```markdown\n* item:\n  ```text\n  code\n  ```\n```\n",
    )
    .unwrap();
    vault
        .refresh_markdown("note.md", Some(&opened.revision), None)
        .expect_err("the ambiguous fence cannot be ingested");

    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        before,
        "a failed refresh must not leave a partially ingested document"
    );
    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Exported
    );
    assert!(!vault.has_unexported_changes("note.md").unwrap());

    fs::write(&path, "# Note\n\nrepaired\n").unwrap();
    let revision = vault.revision("note.md").unwrap();
    vault
        .refresh_markdown("note.md", Some(&revision), None)
        .expect("repair can be ingested after the failed attempt");
    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Exported
    );
}

#[test]
fn reopening_clears_a_report_left_over_from_a_crash() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    {
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.open_document("note.md").unwrap();
        let revision = vault.revision("note.md").unwrap();
        vault.export_markdown("note.md", &revision, None).unwrap();
    }

    // Stand in for a crash between marking and persisting: the mark survives
    // over state that never moved.
    let marker = dir
        .path()
        .join(".mdcrdt")
        .join("sessions")
        .join("note.mdcrdt")
        .join("unexported");
    fs::write(&marker, b"").unwrap();

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    reopened.open_document("note.md").unwrap();

    assert_eq!(
        reopened.export_state("note.md").unwrap(),
        ExportState::Exported,
        "a false report must self-heal on the next open, or it is permanent"
    );
}

#[test]
fn reopening_keeps_the_report_while_the_session_is_ahead() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    {
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.open_document("note.md").unwrap();
        edit_without_exporting(&mut vault, "note.md", " local");
    }

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    reopened.open_document("note.md").unwrap();

    assert_eq!(
        reopened.export_state("note.md").unwrap(),
        ExportState::Unexported,
        "restoring the work is correct; staying silent about it is not"
    );
    let restored = reopened
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert!(
        restored.contains("local"),
        "the edit itself must survive the reopen: {restored}"
    );
}

#[test]
fn a_divergence_error_still_leaves_the_report_set() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "alpha\n").unwrap();
    {
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.open_document("note.md").unwrap();
        edit_without_exporting(&mut vault, "note.md", " local");
    }
    fs::write(&path, "external\n").unwrap();

    let mut reopened = VaultSession::open(dir.path()).unwrap();
    assert!(matches!(
        reopened.open_document("note.md"),
        Err(VaultError::StaleDisk { .. })
    ));

    assert_eq!(
        reopened.export_state("note.md").unwrap(),
        ExportState::Unexported,
        "refusing to reconcile must not look like agreement"
    );
}

#[test]
fn a_vault_written_before_tracking_reports_its_sessions_conservatively() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();
    {
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.open_document("note.md").unwrap();
        let revision = vault.revision("note.md").unwrap();
        vault.export_markdown("note.md", &revision, None).unwrap();
    }

    // Simulate a vault persisted by a build that predates tracking.
    let sentinel = dir.path().join(".mdcrdt").join("unexported_tracked");
    let marker = dir
        .path()
        .join(".mdcrdt")
        .join("sessions")
        .join("note.mdcrdt")
        .join("unexported");
    let _ = fs::remove_file(&sentinel);
    let _ = fs::remove_file(&marker);

    let vault = Vault::open(dir.path()).unwrap();
    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Unexported,
        "absence must not be read as agreement in an untracked vault"
    );

    // Opening migrates the vault, and the clean document then reconciles.
    let mut migrated = VaultSession::open(dir.path()).unwrap();
    migrated.open_document("note.md").unwrap();
    assert_eq!(
        migrated.export_state("note.md").unwrap(),
        ExportState::Exported,
        "the conservative answer must converge, not persist"
    );
}

#[test]
fn mutating_through_the_public_session_accessor_is_still_reported() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    // Deliberately bypasses every named mutation path.
    edit_without_exporting(&mut vault, "note.md", " local");

    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Unexported
    );
}

#[test]
fn deleting_a_document_removes_its_report() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n").unwrap();

    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    edit_without_exporting(&mut vault, "note.md", " local");
    let revision = vault.revision("note.md").unwrap();
    vault.delete_markdown("note.md", &revision, None).unwrap();

    assert_eq!(
        vault.export_state("note.md").unwrap(),
        ExportState::Exported,
        "a deleted document has nothing unexported"
    );
}
