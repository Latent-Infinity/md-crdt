#![cfg(feature = "filesync")]

use md_crdt::{
    filesync::VaultSession,
    limits::{MAX_MARKDOWN_BYTES, MAX_QUOTE_DEPTH},
};

#[test]
fn refused_creation_does_not_publish_a_file() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    let source = format!("{}text", "> ".repeat(MAX_QUOTE_DEPTH + 1));
    assert!(vault.create_markdown("deep.md", &source).is_err());
    assert!(!root.path().join("deep.md").exists());
}

#[test]
fn oversized_ingest_preserves_the_existing_session() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    let original = vault.create_markdown("note.md", "original").unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("note.md"))
        .unwrap();
    file.set_len(MAX_MARKDOWN_BYTES as u64 + 1).unwrap();
    assert!(vault.ingest_all().is_err());
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(md_crdt::EquivalenceMode::Exact),
        "original"
    );
    assert_eq!(vault.document_id("note.md").unwrap(), original.document_id);
}

#[test]
fn alignment_work_refusal_preserves_the_live_document() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    let original = "a".repeat(2100);
    vault.create_markdown("note.md", &original).unwrap();
    std::fs::write(
        root.path().join("note.md"),
        format!("{}b", "a".repeat(2099)),
    )
    .unwrap();
    assert!(vault.ingest_all().is_err());
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(md_crdt::EquivalenceMode::Exact),
        original
    );
}

#[test]
fn oversized_identity_is_rejected_before_materialization() {
    let root = tempfile::tempdir().unwrap();
    drop(VaultSession::open(root.path()).unwrap());
    let identity = root.path().join(".mdcrdt").join("vault_id");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(identity)
        .unwrap();
    file.set_len(129).unwrap();
    assert!(VaultSession::open(root.path()).is_err());
}
