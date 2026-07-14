//! VaultSession: shared peer id + lazy multi-doc CollaborativeDocument store.

#![cfg(feature = "filesync")]

use md_crdt::doc::EquivalenceMode;
use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{
    CheckpointRequest, DocumentTombstonePolicy, StateVector, SyncResponse, ValidationLimits,
};
use std::fs;
use tempfile::tempdir;

fn document_text(vault: &mut VaultSession, path: &str) -> String {
    vault
        .session_mut(path)
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural)
}

#[test]
fn two_vaults_exchange_external_edits_and_persist_convergence() {
    let first_dir = tempdir().unwrap();
    let second_dir = tempdir().unwrap();
    fs::write(first_dir.path().join("note.md"), "base").unwrap();
    fs::write(second_dir.path().join("note.md"), "").unwrap();

    let mut first = VaultSession::open(first_dir.path()).unwrap();
    let mut second = VaultSession::open(second_dir.path()).unwrap();
    first.ingest_all().unwrap();

    let second_vector = second.state_vector("note.md").unwrap();
    let initial = first
        .encode_changes_since("note.md", &second_vector)
        .unwrap();
    second
        .apply_remote("note.md", initial, &ValidationLimits::default())
        .unwrap();

    fs::write(first_dir.path().join("note.md"), "base from first").unwrap();
    fs::write(second_dir.path().join("note.md"), "base from second").unwrap();
    first.ingest_all().unwrap();
    second.ingest_all().unwrap();

    assert_eq!(document_text(&mut first, "note.md"), "base from first");
    assert_eq!(document_text(&mut second, "note.md"), "base from second");

    let first_vector = first.state_vector("note.md").unwrap();
    let second_vector = second.state_vector("note.md").unwrap();
    let to_first = second
        .encode_changes_since("note.md", &first_vector)
        .unwrap();
    let to_second = first
        .encode_changes_since("note.md", &second_vector)
        .unwrap();
    assert!(!to_first.ops.is_empty());
    assert!(!to_second.ops.is_empty());
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
    let first_text = document_text(&mut first, "note.md");
    let second_text = document_text(&mut second, "note.md");
    assert_eq!(first_text, second_text);

    drop(first);
    drop(second);
    let mut first = VaultSession::open(first_dir.path()).unwrap();
    let mut second = VaultSession::open(second_dir.path()).unwrap();
    assert_eq!(
        document_text(&mut first, "note.md"),
        document_text(&mut second, "note.md")
    );
}

#[test]
fn peer_id_persists_across_reopen() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("readme.md"), "# hi").unwrap();

    let peer = {
        let vs = VaultSession::open(dir.path()).unwrap();
        assert!(dir.path().join(".mdcrdt").join("peer_id").exists());
        vs.peer()
    };
    let vs2 = VaultSession::open(dir.path()).unwrap();
    assert_eq!(vs2.peer(), peer);
}

#[test]
fn two_files_share_peer_keep_independent_docs() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("one.md"), "").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub").join("two.md"), "").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let peer = vs.peer();

    vs.session_mut("one.md")
        .unwrap()
        .insert_paragraph(None, "alpha")
        .unwrap();
    vs.session_mut(std::path::Path::new("sub").join("two.md"))
        .unwrap()
        .insert_paragraph(None, "beta")
        .unwrap();

    assert_eq!(vs.session_mut("one.md").unwrap().peer(), peer);
    assert_eq!(
        vs.session_mut(std::path::Path::new("sub").join("two.md"))
            .unwrap()
            .peer(),
        peer
    );
    assert_eq!(
        vs.session_mut("one.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        "alpha"
    );
    assert_eq!(
        vs.session_mut(std::path::Path::new("sub").join("two.md"))
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        "beta"
    );
}

#[test]
fn snapshot_round_trip_across_process_boundary() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "").unwrap();

    let peer = {
        let mut vs = VaultSession::open(dir.path()).unwrap();
        vs.session_mut("doc.md")
            .unwrap()
            .insert_paragraph(None, "round-trip")
            .unwrap();
        vs.save_all_state().unwrap();
        vs.peer()
    };

    let mut vs = VaultSession::open(dir.path()).unwrap();
    assert_eq!(vs.peer(), peer);
    assert!(!vs.is_open("doc.md"));
    let text = vs
        .session_mut("doc.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert_eq!(text, "round-trip");
}

#[test]
fn invalid_relative_path_rejected() {
    let dir = tempdir().unwrap();
    let mut vs = VaultSession::open(dir.path()).unwrap();
    assert!(matches!(
        vs.session_mut("/abs.md"),
        Err(VaultError::InvalidRelativePath(_))
    ));
    assert!(matches!(
        vs.session_mut("../x.md"),
        Err(VaultError::InvalidRelativePath(_))
    ));
}

#[test]
fn path_scoped_sync_reports_retention_and_returns_a_rebase_checkpoint() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let document = vault.session_mut("note.md").unwrap();
    document.insert_paragraph(None, "retained state").unwrap();
    document
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();

    assert!(matches!(
        vault
            .encode_changes_since("note.md", &StateVector::new())
            .unwrap_err(),
        VaultError::RebaseRequired(_)
    ));
    assert!(matches!(
        vault.sync_since("note.md", &StateVector::new()).unwrap(),
        SyncResponse::Rebase { .. }
    ));
}
