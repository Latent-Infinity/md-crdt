//! Session snapshot save/restore and late-join import.

use md_crdt::doc::{EquivalenceMode, Parser, block_id_from_op};
use md_crdt::session::{
    CollaborativeDocument, DocumentDto, SNAPSHOT_FORMAT_VERSION, SessionSnapshot, SnapshotError,
};
use md_crdt::sync::{ChangeMessage, ValidationLimits};
use md_crdt::{
    BlockDraft, CheckpointRequest, DocumentTombstonePolicy, ListItemDraft, ListStyle,
    StructuredEditLimits,
};

fn exchange(source: &CollaborativeDocument, target: &mut CollaborativeDocument) {
    let message = source.encode_changes_since(&target.state_vector()).unwrap();
    target
        .apply_remote(message, &ValidationLimits::default())
        .unwrap();
}

#[test]
fn save_restore_round_trip_preserves_document_and_sv() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "hello").expect("insert");
    a.insert_paragraph(None, "world").expect("insert");

    let snap = a.save_snapshot().expect("save");
    assert_eq!(snap.format_version, SNAPSHOT_FORMAT_VERSION);
    assert_eq!(snap.peer, 1);
    // Two paragraphs × (InsertBlock + InsertText)
    assert_eq!(snap.ops.len(), 4);
    assert!(snap.next_counter > 2);
    assert!(snap.unit_mode);

    let b = CollaborativeDocument::restore_from_snapshot(snap).expect("restore");
    assert_eq!(b.peer(), 1);
    assert!(b.unit_mode());
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), b.state_vector());
    assert_eq!(a.peek_next_id().counter, b.peek_next_id().counter);
}

#[test]
fn save_restore_round_trip_preserves_table_rows() {
    use md_crdt::doc::{ColumnAlignment, ColumnDef, block_id_from_op};

    let mut a = CollaborativeDocument::new(1);
    let table_elem = a
        .insert_table(
            None,
            vec![
                ColumnDef {
                    alignment: ColumnAlignment::Left,
                },
                ColumnDef {
                    alignment: ColumnAlignment::Right,
                },
            ],
            vec!["Name".into(), "Score".into()],
        )
        .expect("table");
    let table_id = block_id_from_op(table_elem);
    let row1 = a
        .insert_table_row(table_id, None, vec!["Alice".into(), "10".into()])
        .expect("row1");
    a.insert_table_row(table_id, Some(row1), vec!["Bob".into(), "8".into()])
        .expect("row2");
    a.set_table_row_cells(table_id, row1, vec!["Alice".into(), "11".into()])
        .expect("update row1");

    let before = a.document().serialize(EquivalenceMode::Structural);
    assert!(before.contains("| Alice | 11 |"), "sanity: {before}");

    // Table DTO (columns/alignment/header + row RGA + LWW cells) survives snapshot.
    let snap = a.save_snapshot().expect("save");
    let mut b = CollaborativeDocument::restore_from_snapshot(snap).expect("restore");
    assert_eq!(before, b.document().serialize(EquivalenceMode::Structural));
    assert_eq!(a.state_vector(), b.state_vector());
    // next_counter recovery must account for row OpIds, or a post-restore insert collides.
    assert_eq!(a.peek_next_id().counter, b.peek_next_id().counter);

    let fresh = b
        .insert_table_row(table_id, None, vec!["Carol".into(), "7".into()])
        .expect("post-restore row");
    assert!(
        fresh.counter >= a.peek_next_id().counter,
        "restored clock must not reissue an existing counter"
    );
}

#[test]
fn save_restore_round_trip_preserves_split_merge_history() {
    use md_crdt::doc::block_id_from_op;

    let mut a = CollaborativeDocument::new(2);
    let left = a.insert_paragraph(None, "abcd").expect("paragraph");
    let left_id = block_id_from_op(left);
    let right = a.split_block(left_id, 2).expect("split");
    a.merge_blocks(left_id, block_id_from_op(right))
        .expect("merge");

    let snapshot = a.save_snapshot().expect("save");
    let mut restored = CollaborativeDocument::restore_from_snapshot(snapshot).expect("restore");
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        restored.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), restored.state_vector());
    assert_eq!(a.peek_next_id(), restored.peek_next_id());

    restored
        .insert_text(left_id, 4, "!")
        .expect("post-restore edit");
    assert!(restored.peek_next_id().counter > a.peek_next_id().counter);
}

#[test]
fn restore_rejects_clock_behind_max_op() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "x").expect("insert");
    let mut snap = a.save_snapshot().expect("save");
    snap.next_counter = 1; // ops use counter ≥1; next must be > max
    match CollaborativeDocument::restore_from_snapshot(snap) {
        Err(SnapshotError::ClockBehind { .. }) => {}
        Err(e) => panic!("expected ClockBehind, got {e:?}"),
        Ok(_) => panic!("expected ClockBehind, got Ok"),
    }
}

#[test]
fn restore_rejects_clock_behind_compacted_frontier() {
    let mut session = CollaborativeDocument::new(1);
    let block = session.insert_paragraph(None, "transient").unwrap();
    session.delete_block(block).unwrap();
    session
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let mut snapshot = session.save_snapshot().unwrap();
    assert!(snapshot.ops.is_empty());
    snapshot.next_counter = snapshot.state_vector.get(1).unwrap();

    assert!(matches!(
        CollaborativeDocument::restore_from_snapshot(snapshot),
        Err(SnapshotError::ClockBehind { .. })
    ));
}

#[test]
fn import_state_rebinds_to_local_peer() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "shared").expect("insert");
    let snap = a.save_snapshot().expect("save");

    let mut b = CollaborativeDocument::import_state(
        snap.document,
        snap.ops,
        snap.pending,
        snap.deferred,
        99, // local peer
        true,
    )
    .unwrap();
    assert_eq!(b.peer(), 99);
    // No ops from peer 99 yet → next_counter starts at 1
    assert_eq!(b.peek_next_id().counter, 1);
    assert_eq!(b.document().blocks_in_order().len(), 1);

    // Local edits use peer 99
    let id = b.insert_paragraph(None, "local").expect("local");
    assert_eq!(id.peer, 99);
}

#[test]
fn rebind_peer_after_restore() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "doc").expect("insert");
    let snap = a.save_snapshot().expect("save");
    let mut b = CollaborativeDocument::restore_from_snapshot(snap).expect("restore");
    b.rebind_peer(7);
    assert_eq!(b.peer(), 7);
    assert_eq!(b.peek_next_id().peer, 7);
    assert_eq!(b.peek_next_id().counter, 1);
}

#[test]
fn snapshot_preserves_list_insert_waiting_for_a_missing_anchor() {
    let mut author = CollaborativeDocument::new(1);
    let list_elem = author
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: ListStyle::default(),
                items: vec![ListItemDraft {
                    task: None,
                    children: Vec::new(),
                }],
            },
            StructuredEditLimits::default(),
        )
        .unwrap();
    let list_id = block_id_from_op(list_elem);
    let first = author
        .document()
        .list_items(list_id)
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .elem_id;
    let mut editor = CollaborativeDocument::new(2);
    let mut delayed = CollaborativeDocument::new(3);
    exchange(&author, &mut editor);
    exchange(&author, &mut delayed);

    let anchor = author.insert_list_item(list_id, Some(first), None).unwrap();
    exchange(&author, &mut editor);
    editor
        .insert_list_item(list_id, Some(anchor), None)
        .unwrap();

    let editor_delta = editor
        .encode_changes_since(&delayed.state_vector())
        .unwrap();
    delayed
        .apply_remote(
            ChangeMessage {
                since: editor_delta.since,
                ops: editor_delta
                    .ops
                    .into_iter()
                    .filter(|operation| operation.id.peer == 2)
                    .collect(),
            },
            &ValidationLimits::default(),
        )
        .unwrap();
    delayed
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let bytes = delayed.save_snapshot().unwrap().to_bytes().unwrap();
    let snapshot = SessionSnapshot::from_bytes(&bytes).unwrap();
    let mut restored = CollaborativeDocument::restore_from_snapshot(snapshot).unwrap();
    exchange(&author, &mut restored);

    assert_eq!(restored.document(), editor.document());
    assert_eq!(
        restored
            .document()
            .list_items(list_id)
            .unwrap()
            .len_visible(),
        3
    );
}

#[test]
fn snapshot_preserves_list_delete_waiting_for_a_missing_item() {
    let mut author = CollaborativeDocument::new(1);
    let list_elem = author
        .insert_draft_in(
            None,
            None,
            &BlockDraft::List {
                style: ListStyle::default(),
                items: Vec::new(),
            },
            StructuredEditLimits::default(),
        )
        .unwrap();
    let list_id = block_id_from_op(list_elem);
    let mut editor = CollaborativeDocument::new(2);
    let mut delayed = CollaborativeDocument::new(3);
    exchange(&author, &mut editor);
    exchange(&author, &mut delayed);

    let inserted = author.insert_list_item(list_id, None, None).unwrap();
    let item_id = block_id_from_op(inserted);
    exchange(&author, &mut editor);
    editor.delete_list_item(item_id).unwrap();

    let editor_delta = editor
        .encode_changes_since(&delayed.state_vector())
        .unwrap();
    delayed
        .apply_remote(
            ChangeMessage {
                since: editor_delta.since,
                ops: editor_delta
                    .ops
                    .into_iter()
                    .filter(|operation| operation.id.peer == 2)
                    .collect(),
            },
            &ValidationLimits::default(),
        )
        .unwrap();
    delayed
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let bytes = delayed.save_snapshot().unwrap().to_bytes().unwrap();
    let snapshot = SessionSnapshot::from_bytes(&bytes).unwrap();
    let mut restored = CollaborativeDocument::restore_from_snapshot(snapshot).unwrap();
    exchange(&author, &mut restored);

    assert_eq!(restored.document(), editor.document());
    assert_eq!(
        restored
            .document()
            .list_items(list_id)
            .unwrap()
            .len_visible(),
        0
    );
}

#[test]
fn checkpoint_rebase_advances_past_the_local_peer_frontier() {
    let mut source = CollaborativeDocument::new(1);
    source.insert_paragraph(None, "base").unwrap();
    let mut peer = CollaborativeDocument::new(2);
    exchange(&source, &mut peer);
    let transient = peer.insert_paragraph(None, "transient").unwrap();
    peer.delete_block(transient).unwrap();
    exchange(&peer, &mut source);
    source
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let snapshot = source.save_snapshot().unwrap();
    let frontier = snapshot.state_vector.get(2).unwrap();

    let rebased = CollaborativeDocument::rebase_from_snapshot(snapshot, 2).unwrap();

    assert!(rebased.peek_next_id().counter > frontier);
}

#[test]
fn exchange_after_restore_continues() {
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "a1").expect("a1");
    let snap = a.save_snapshot().expect("save");

    let mut b = CollaborativeDocument::restore_from_snapshot(snap).expect("restore");
    // a continues editing
    a.insert_paragraph(None, "a2").expect("a2");
    let msg = a.encode_changes_since(&b.state_vector()).unwrap();
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
    a.insert_paragraph(None, "bytes").expect("insert");
    let snap = a.save_snapshot().expect("save");
    let bytes = snap.to_bytes().expect("to_bytes");
    let loaded = SessionSnapshot::from_bytes(&bytes).expect("from_bytes");
    assert_eq!(loaded.peer, snap.peer);
    assert_eq!(loaded.ops.len(), snap.ops.len());
}

#[test]
fn non_current_snapshot_versions_require_reinitialize() {
    let mut session = CollaborativeDocument::new(3);
    session.insert_paragraph(None, "current").unwrap();
    let current = session.save_snapshot().unwrap();

    for format_version in [1, 2, SNAPSHOT_FORMAT_VERSION + 1] {
        let mut old = current.clone();
        old.format_version = format_version;
        let error = SessionSnapshot::from_bytes(&old.to_bytes().unwrap()).unwrap_err();
        assert!(matches!(
            error,
            SnapshotError::ReinitializeRequired {
                found,
                expected: SNAPSHOT_FORMAT_VERSION,
            } if found == format_version
        ));
        assert!(error.to_string().contains("reinitialize and re-ingest"));
    }
}

#[test]
fn document_snapshot_preserves_headings_and_nested_lists() {
    let document = Parser::parse("# heading\n\n- parent\n  1. child");
    let restored = DocumentDto::from_document(&document).into_document();

    assert_eq!(
        document.serialize(EquivalenceMode::Structural),
        restored.serialize(EquivalenceMode::Structural)
    );
}

#[cfg(feature = "storage")]
#[test]
fn storage_write_read_round_trip() {
    use md_crdt::storage::Storage;
    use tempfile::tempdir;

    let dir = tempdir().expect("temp");
    let storage = Storage::open(dir.path()).expect("open");
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "persist").expect("insert");
    a.write_to_storage(&storage).expect("write");

    let b = CollaborativeDocument::read_from_storage(&storage).expect("read");
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
    assert_eq!(a.state_vector(), b.state_vector());
}
