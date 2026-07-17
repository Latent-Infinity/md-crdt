use md_crdt::core::{OpId, Sequence, SequenceOp, StateVector};
use md_crdt::doc::{Block, BlockKind, Document, ListItem, block_id_from_op};
use md_crdt::sync::{IntegrateResult, Operation, SyncState};
use std::sync::Arc;

fn op(counter: u64, peer: u64) -> OpId {
    OpId { counter, peer }
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn document_remains_send_and_sync() {
    assert_send_sync::<Document>();
}

fn paragraph(id: OpId) -> Block {
    Block::new(
        BlockKind::Paragraph {
            text: Sequence::new(),
        },
        id,
    )
}

#[test]
fn block_id_index_tracks_top_level_and_nested_mutations() {
    let mut document = Document::new();
    let quote_id = op(1, 1);
    document.insert_block_at(
        None,
        None,
        quote_id,
        Block::new(
            BlockKind::BlockQuote {
                children: Sequence::new(),
            },
            quote_id,
        ),
        None,
    );
    let nested_id = op(2, 1);
    document.insert_block_at(Some(quote_id), None, nested_id, paragraph(nested_id), None);

    assert_eq!(
        document.block_elem_id(block_id_from_op(quote_id)),
        Some(quote_id)
    );
    assert_eq!(
        document.block_elem_id(block_id_from_op(nested_id)),
        Some(nested_id)
    );
    assert_eq!(
        document
            .find_block_by_id(block_id_from_op(nested_id))
            .map(|block| block.elem_id),
        Some(nested_id)
    );

    document.delete_block_at(Some(quote_id), nested_id, op(3, 1));
    assert_eq!(document.block_elem_id(block_id_from_op(nested_id)), None);
}

#[test]
fn direct_sequence_access_invalidates_the_block_index() {
    let mut document = Document::new();
    let first = op(1, 2);
    document.insert_block_at(None, None, first, paragraph(first), None);
    assert_eq!(document.block_elem_id(block_id_from_op(first)), Some(first));

    let second = op(2, 2);
    document.blocks_mut().apply(SequenceOp::Insert {
        after: Some(first),
        id: second,
        value: paragraph(second),
        right_origin: None,
    });
    assert_eq!(
        document.block_elem_id(block_id_from_op(second)),
        Some(second)
    );
}

#[test]
fn block_index_follows_list_item_paths_and_survives_document_clone() {
    let mut document = Document::new();
    let list_id = op(1, 4);
    let item_id = op(2, 4);
    let nested_id = op(3, 4);
    let mut children = Sequence::new();
    children.insert(None, paragraph(nested_id), nested_id);
    let mut items = Sequence::new();
    items.insert(
        None,
        ListItem {
            id: block_id_from_op(item_id),
            elem_id: item_id,
            task: None,
            task_op: item_id,
            task_observed: StateVector::new(),
            placement_observed: StateVector::new(),
            children,
        },
        item_id,
    );
    document.insert_block_at(
        None,
        None,
        list_id,
        Block::new(
            BlockKind::List {
                style: md_crdt::ListStyle::default(),
                items,
                pending_moves: Vec::new(),
            },
            list_id,
        ),
        None,
    );

    assert_eq!(
        document
            .find_block_by_id(block_id_from_op(nested_id))
            .map(|block| block.elem_id),
        Some(nested_id)
    );
    assert_eq!(
        document.find_block(nested_id).map(|block| block.id),
        Some(block_id_from_op(nested_id))
    );
    document
        .with_block_mut(nested_id, |block| {
            block.kind = BlockKind::heading(2, "nested", op(10, 4));
        })
        .expect("mutate nested block through indexed path");
    assert!(matches!(
        document.find_block(nested_id).map(|block| &block.kind),
        Some(BlockKind::Heading { level: 2, .. })
    ));

    let mut cloned = document.clone();
    assert_eq!(cloned, document);
    let extra = op(20, 4);
    cloned.insert_block_at(None, Some(list_id), extra, paragraph(extra), None);
    assert_ne!(cloned, document);
}

#[test]
fn block_index_repairs_after_public_sequence_replacement() {
    let mut left = Document::new();
    let left_id = op(1, 5);
    left.insert_block_at(None, None, left_id, paragraph(left_id), None);
    let mut right = Document::new();
    let right_id = op(1, 6);
    right.insert_block_at(None, None, right_id, paragraph(right_id), None);
    assert_eq!(left.block_elem_id(block_id_from_op(left_id)), Some(left_id));
    assert_eq!(
        right.block_elem_id(block_id_from_op(right_id)),
        Some(right_id)
    );

    std::mem::swap(&mut left.blocks, &mut right.blocks);

    assert_eq!(
        left.block_elem_id(block_id_from_op(right_id)),
        Some(right_id)
    );
    assert_eq!(left.block_elem_id(block_id_from_op(left_id)), None);
    assert_eq!(right.find_block(right_id), None);
    assert_eq!(
        right.find_block(left_id).map(|block| block.id),
        Some(block_id_from_op(left_id))
    );
}

#[test]
fn encoded_changes_share_immutable_payload_storage() {
    let mut sync = SyncState::new();
    let payload: Arc<[u8]> = vec![7; 1024].into();
    sync.apply_op(Operation {
        id: op(1, 3),
        payload: payload.clone(),
    });

    let changes = sync.encode_changes_since(&StateVector::new()).unwrap();
    assert_eq!(changes.ops.len(), 1);
    assert!(Arc::ptr_eq(&changes.ops[0].payload, &payload));
}

#[test]
fn shared_payload_keeps_the_existing_json_byte_array_shape() {
    let operation = Operation {
        id: op(1, 7),
        payload: vec![1, 2, 3].into(),
    };
    let json = serde_json::to_value(&operation).expect("serialize operation");
    assert_eq!(json["payload"], serde_json::json!([1, 2, 3]));
    let restored: Operation = serde_json::from_value(json).expect("deserialize operation");
    assert_eq!(restored, operation);
}

#[test]
fn cached_state_vector_updates_on_every_applied_path() {
    let mut sync = SyncState::new();
    sync.apply_op(Operation {
        id: op(2, 1),
        payload: vec![1].into(),
    });
    sync.add_local_op(Operation {
        id: op(4, 2),
        payload: vec![2].into(),
    });
    assert_eq!(sync.state_vector().get(1), Some(2));
    assert_eq!(sync.state_vector().get(2), Some(4));

    assert_eq!(
        sync.apply_one(
            Operation {
                id: op(4, 1),
                payload: vec![3].into(),
            },
            1,
        ),
        IntegrateResult::Buffered
    );
    assert_eq!(
        sync.apply_one(
            Operation {
                id: op(3, 1),
                payload: vec![4].into(),
            },
            1,
        ),
        IntegrateResult::Applied
    );
    assert_eq!(sync.promote_ready_pending().len(), 1);
    assert_eq!(sync.state_vector().get(1), Some(4));

    let mut restored = SyncState::new();
    restored.restore_applied(sync.applied_ops());
    assert_eq!(restored.state_vector(), sync.state_vector());
}

#[test]
fn cached_state_vector_matches_independent_recompute() {
    // Drive every applied path into one SyncState, then compare the incrementally
    // maintained cache against an *independent* max-per-peer recompute over the applied
    // op ids. The existing test compares the cache to explicit expected values and to a
    // restore that reuses the same cache; this pins it to a from-scratch oracle so a
    // systematic `observe` bug (which both cache-based sides would share) is caught.
    let mut sync = SyncState::new();
    sync.add_local_op(Operation {
        id: op(3, 1),
        payload: vec![1].into(),
    });
    sync.apply_op(Operation {
        id: op(5, 2),
        payload: vec![2].into(),
    });
    // Buffered-then-promoted chain on peer 3 (counter 2 buffered until 1 applies).
    assert_eq!(
        sync.apply_one(
            Operation {
                id: op(2, 3),
                payload: vec![3].into(),
            },
            1,
        ),
        IntegrateResult::Buffered
    );
    assert_eq!(
        sync.apply_one(
            Operation {
                id: op(1, 3),
                payload: vec![4].into(),
            },
            1,
        ),
        IntegrateResult::Applied
    );
    assert_eq!(sync.promote_ready_pending().len(), 1);

    let mut oracle = StateVector::new();
    for (id, _) in sync.applied_ops() {
        if id.counter > oracle.get(id.peer).unwrap_or(0) {
            oracle.set(id.peer, id.counter);
        }
    }
    assert_eq!(sync.state_vector(), oracle, "cache must equal recompute");
    assert_eq!(oracle.get(1), Some(3));
    assert_eq!(oracle.get(2), Some(5));
    assert_eq!(oracle.get(3), Some(2));

    let mut restored = SyncState::new();
    restored.restore_applied(sync.applied_ops());
    assert_eq!(restored.state_vector(), oracle);
}
