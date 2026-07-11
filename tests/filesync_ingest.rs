//! Structure-only vault ingest (hash gate + match_blocks → block ops).

#![cfg(feature = "filesync")]

use md_crdt::doc::{BlockKind, EquivalenceMode, paragraph_visible_string};
use md_crdt::filesync::VaultSession;
use std::fs;
use tempfile::tempdir;

#[test]
fn ingest_empty_session_from_file_inserts_paragraphs() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "hello\n\nworld").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert_eq!(report.files_noop, 0);
    assert!(report.ops_emitted >= 2); // at least two structure commits

    let text = vs
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert_eq!(text, "hello\n\nworld");
}

#[test]
fn ingest_preserves_blockquote_structure() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("q.md"), "> quoted line").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert_eq!(report.files_skipped, 0);

    // The blockquote must be preserved as nested structure, not flattened.
    let session = vs.session_mut("q.md").unwrap();
    let top = &session.document().blocks_in_order()[0];
    match &top.kind {
        BlockKind::BlockQuote { children } => {
            let kids: Vec<_> = children.iter().collect();
            assert_eq!(kids.len(), 1, "one nested paragraph");
            match &kids[0].kind {
                BlockKind::Paragraph { text } => {
                    assert_eq!(paragraph_visible_string(text), "quoted line");
                }
                _ => panic!("expected nested paragraph"),
            }
        }
        _ => panic!("blockquote flattened — structure lost"),
    }
}

#[test]
fn second_ingest_is_noop_when_file_unchanged() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "stable").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let r1 = vs.ingest_all().unwrap();
    assert_eq!(r1.files_changed, 1);

    let r2 = vs.ingest_all().unwrap();
    assert_eq!(r2.files_noop, 1);
    assert_eq!(r2.files_changed, 0);
    assert_eq!(r2.ops_emitted, 0);
}

#[test]
fn ingest_add_paragraph_preserves_existing_block_id() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "first").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    let first_id = {
        let doc = vs.session_mut("doc.md").unwrap().document();
        doc.blocks_in_order()[0].id
    };

    fs::write(dir.path().join("doc.md"), "first\n\nsecond").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert!(report.ops_emitted >= 1);

    let doc = vs.session_mut("doc.md").unwrap().document();
    let blocks = doc.blocks_in_order();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].id, first_id, "matched block keeps BlockId");
    assert_eq!(
        doc.serialize(EquivalenceMode::Structural),
        "first\n\nsecond"
    );
}

#[test]
fn ingest_remove_paragraph_deletes_block() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "keep\n\ngone").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    assert_eq!(
        vs.session_mut("doc.md")
            .unwrap()
            .document()
            .blocks_in_order()
            .len(),
        2
    );

    fs::write(dir.path().join("doc.md"), "keep").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);

    let doc = vs.session_mut("doc.md").unwrap().document();
    assert_eq!(doc.blocks_in_order().len(), 1);
    match &doc.blocks_in_order()[0].kind {
        BlockKind::Paragraph { text } => {
            assert_eq!(paragraph_visible_string(text), "keep");
        }
        _ => panic!("expected paragraph"),
    }
}

#[test]
fn reorder_preserves_block_ids_via_match() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "alpha\n\nbeta").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    let (id_a, id_b) = {
        let blocks = vs
            .session_mut("doc.md")
            .unwrap()
            .document()
            .blocks_in_order();
        (blocks[0].id, blocks[1].id)
    };

    // Swap order in the file; fingerprints still match both paragraphs.
    fs::write(dir.path().join("doc.md"), "beta\n\nalpha").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    // Structure-only: pure reorder emits no add/remove when both match.
    // (CRDT order may stay alpha-then-beta; ids must both still exist.)
    let blocks = vs
        .session_mut("doc.md")
        .unwrap()
        .document()
        .blocks_in_order();
    let ids: Vec<_> = blocks.iter().map(|b| b.id).collect();
    assert!(ids.contains(&id_a));
    assert!(ids.contains(&id_b));
    assert_eq!(ids.len(), 2);
}

#[test]
fn multi_file_ingest_report_counts() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "A").unwrap();
    fs::write(dir.path().join("b.md"), "B").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let r = vs.ingest_all().unwrap();
    assert_eq!(r.files_changed, 2);
    assert_eq!(r.files_noop, 0);

    let r2 = vs.ingest_all().unwrap();
    assert_eq!(r2.files_noop, 2);
}

#[test]
fn session_snapshot_survives_reopen_after_ingest() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "persisted body").unwrap();

    {
        let mut vs = VaultSession::open(dir.path()).unwrap();
        vs.ingest_all().unwrap();
    }

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let text = vs
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert_eq!(text, "persisted body");
}
