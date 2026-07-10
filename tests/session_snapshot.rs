//! Session snapshot save/restore and late-join import.

use md_crdt::doc::{BlockKind, EquivalenceMode};
use md_crdt::session::{
    CollaborativeDocument, SNAPSHOT_FORMAT_VERSION, SessionSnapshot, SnapshotError,
};
use md_crdt::sync::ValidationLimits;

fn para(text: &str) -> BlockKind {
    use md_crdt::core::OpId;
    BlockKind::paragraph(
        text,
        OpId {
            counter: 1,
            peer: 0,
        },
    )
}

#[test]
fn save_restore_round_trip_preserves_document_and_sv() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_block(None, para("hello")).expect("insert");
    a.insert_block(None, para("world")).expect("insert");

    let snap = a.save_snapshot().expect("save");
    assert_eq!(snap.format_version, SNAPSHOT_FORMAT_VERSION);
    assert_eq!(snap.peer, 1);
    assert_eq!(snap.ops.len(), 2);
    assert!(snap.next_counter > 2);

    let b = CollaborativeDocument::restore_from_snapshot(snap).expect("restore");
    assert_eq!(b.peer(), 1);
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), b.state_vector());
    assert_eq!(a.peek_next_id().counter, b.peek_next_id().counter);
}

#[test]
fn restore_rejects_clock_behind_max_op() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_block(None, para("x")).expect("insert");
    let mut snap = a.save_snapshot().expect("save");
    snap.next_counter = 1; // ops use counter 1; next must be > max
    match CollaborativeDocument::restore_from_snapshot(snap) {
        Err(SnapshotError::ClockBehind { .. }) => {}
        Err(e) => panic!("expected ClockBehind, got {e:?}"),
        Ok(_) => panic!("expected ClockBehind, got Ok"),
    }
}

#[test]
fn import_state_rebinds_to_local_peer() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_block(None, para("shared")).expect("insert");
    let snap = a.save_snapshot().expect("save");

    let mut b = CollaborativeDocument::import_state(
        snap.document,
        snap.ops,
        snap.pending,
        99, // local peer
        false,
    );
    assert_eq!(b.peer(), 99);
    // No ops from peer 99 yet → next_counter starts at 1
    assert_eq!(b.peek_next_id().counter, 1);
    assert_eq!(b.document().blocks_in_order().len(), 1);

    // Local edits use peer 99
    let id = b.insert_block(None, para("local")).expect("local");
    assert_eq!(id.peer, 99);
}

#[test]
fn rebind_peer_after_restore() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_block(None, para("doc")).expect("insert");
    let snap = a.save_snapshot().expect("save");
    let mut b = CollaborativeDocument::restore_from_snapshot(snap).expect("restore");
    b.rebind_peer(7);
    assert_eq!(b.peer(), 7);
    assert_eq!(b.peek_next_id().peer, 7);
    assert_eq!(b.peek_next_id().counter, 1);
}

#[test]
fn exchange_after_restore_continues() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_block(None, para("a1")).expect("a1");
    let snap = a.save_snapshot().expect("save");

    let mut b = CollaborativeDocument::restore_from_snapshot(snap).expect("restore");
    // a continues editing
    a.insert_block(None, para("a2")).expect("a2");
    let msg = a.encode_changes_since(&b.state_vector());
    b.apply_remote(msg, &ValidationLimits::default())
        .expect("remote");
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn snapshot_bytes_round_trip() {
    let mut a = CollaborativeDocument::new(3);
    a.insert_block(None, para("bytes")).expect("insert");
    let snap = a.save_snapshot().expect("save");
    let bytes = snap.to_bytes().expect("to_bytes");
    let loaded = SessionSnapshot::from_bytes(&bytes).expect("from_bytes");
    assert_eq!(loaded.peer, snap.peer);
    assert_eq!(loaded.ops.len(), snap.ops.len());
}

#[cfg(feature = "storage")]
#[test]
fn storage_write_read_round_trip() {
    use md_crdt::storage::Storage;
    use tempfile::tempdir;

    let dir = tempdir().expect("temp");
    let storage = Storage::open(dir.path()).expect("open");
    let mut a = CollaborativeDocument::new(1);
    a.insert_block(None, para("persist")).expect("insert");
    a.write_to_storage(&storage).expect("write");

    let b = CollaborativeDocument::read_from_storage(&storage).expect("read");
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), b.state_vector());
}
