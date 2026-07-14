use md_crdt::{
    ChangeMessage, CheckpointError, CheckpointRequest, CollaborativeDocument,
    DocumentTombstonePolicy, Operation, PeerLease, StateVector, SyncResponse, SyncState,
    ValidationLimits,
};

fn operation(counter: u64) -> Operation {
    Operation {
        id: md_crdt::OpId { counter, peer: 1 },
        payload: vec![counter as u8].into(),
    }
}

#[test]
fn checkpoint_respects_active_peer_acknowledgements_and_delta_floor() {
    let mut sync = SyncState::new();
    for counter in 1..=5 {
        sync.apply_op(operation(counter));
    }
    let mut acknowledged = StateVector::new();
    acknowledged.set(1, 2);
    let report = sync
        .checkpoint(&CheckpointRequest {
            max_retained_ops: 3,
            active_peer_leases: vec![PeerLease {
                peer: 9,
                acknowledged,
            }],
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();

    assert_eq!(report.checkpoint_epoch, 1);
    assert_eq!(report.pruned_ops, 2);
    assert_eq!(report.retained_ops, 3);
    assert_eq!(report.delta_floor.get(1), Some(2));

    let mut stale = StateVector::new();
    stale.set(1, 1);
    assert!(sync.encode_changes_since(&stale).is_err());

    let mut current = StateVector::new();
    current.set(1, 2);
    let counters: Vec<_> = sync
        .encode_changes_since(&current)
        .unwrap()
        .ops
        .iter()
        .map(|operation| operation.id.counter)
        .collect();
    assert_eq!(counters, vec![3, 4, 5]);
}

#[test]
fn checkpoint_blocked_by_a_leased_peer_is_non_mutating() {
    let mut sync = SyncState::new();
    for counter in 1..=4 {
        sync.apply_op(operation(counter));
    }
    let before = sync.applied_ops();

    let error = sync
        .checkpoint(&CheckpointRequest {
            max_retained_ops: 2,
            active_peer_leases: vec![PeerLease {
                peer: 9,
                acknowledged: StateVector::new(),
            }],
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap_err();

    assert!(matches!(
        error,
        CheckpointError::RetentionBlocked {
            required_prune: 2,
            eligible_prune: 0
        }
    ));
    assert_eq!(sync.applied_ops(), before);
    assert_eq!(sync.checkpoint_epoch(), 0);
    assert!(sync.delta_floor().is_empty());
}

#[test]
fn lagging_peer_rebases_then_resumes_incremental_exchange() {
    let mut source = CollaborativeDocument::new(1);
    let block = source.insert_paragraph(None, "alpha").unwrap();
    let mut target = CollaborativeDocument::new(2);
    target
        .apply_remote(
            source.encode_changes_since(&target.state_vector()).unwrap(),
            &ValidationLimits::default(),
        )
        .unwrap();

    source
        .insert_text(md_crdt::block_id_from_op(block), 5, " beta")
        .unwrap();
    source
        .insert_text(md_crdt::block_id_from_op(block), 10, " gamma")
        .unwrap();
    source
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 1,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();

    let checkpoint = match source.sync_since(&target.state_vector()).unwrap() {
        SyncResponse::Delta(ChangeMessage { .. }) => panic!("lagging peer received a delta"),
        SyncResponse::Rebase { checkpoint } => checkpoint,
    };
    target = CollaborativeDocument::rebase_from_snapshot(*checkpoint, 2).unwrap();
    assert_eq!(
        source
            .document()
            .serialize(md_crdt::EquivalenceMode::Structural),
        target
            .document()
            .serialize(md_crdt::EquivalenceMode::Structural)
    );

    source
        .insert_text(md_crdt::block_id_from_op(block), 16, " delta")
        .unwrap();
    let delta = match source.sync_since(&target.state_vector()).unwrap() {
        SyncResponse::Delta(delta) => delta,
        SyncResponse::Rebase { .. } => panic!("rebased peer should accept deltas"),
    };
    assert_eq!(delta.ops.len(), 1);
    target
        .apply_remote(delta, &ValidationLimits::default())
        .unwrap();
    assert_eq!(source.state_vector(), target.state_vector());
}

#[test]
fn checkpoint_frontier_survives_snapshot_round_trip() {
    let mut source = CollaborativeDocument::new(1);
    source.insert_paragraph(None, "alpha").unwrap();
    source
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let snapshot = source.save_snapshot().unwrap();
    assert_eq!(snapshot.checkpoint_epoch, 1);
    assert!(!snapshot.delta_floor.is_empty());

    let restored = CollaborativeDocument::restore_from_snapshot(
        md_crdt::SessionSnapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(restored.state_vector(), source.state_vector());
    assert!(restored.encode_changes_since(&StateVector::new()).is_err());
    assert!(matches!(
        restored.sync_since(&StateVector::new()).unwrap(),
        SyncResponse::Rebase { .. }
    ));
}

#[test]
fn checkpoint_uses_the_minimum_acknowledgement_across_multiple_leases() {
    let mut sync = SyncState::new();
    for counter in 1..=5 {
        sync.apply_op(operation(counter)); // peer 1, counters 1..=5
    }
    let lease = |peer, ack: u64| {
        let mut acknowledged = StateVector::new();
        acknowledged.set(1, ack);
        PeerLease { peer, acknowledged }
    };
    let before = sync.applied_ops();

    // One lease acked peer 1 to 4, another only to 2. Pruning 4 ops satisfies the ack=4 lease
    // but strands the ack=2 lease. Correct "minimum acknowledged across ALL leases" semantics
    // caps eligible pruning at 2, so a request to prune 4 is blocked and nothing mutates.
    // A `.any()` / max-instead-of-min regression would report eligible_prune: 4 and succeed.
    let error = sync
        .checkpoint(&CheckpointRequest {
            max_retained_ops: 1, // required_prune = 5 - 1 = 4
            active_peer_leases: vec![lease(9, 4), lease(10, 2)],
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap_err();
    assert!(matches!(
        error,
        CheckpointError::RetentionBlocked {
            required_prune: 4,
            eligible_prune: 2
        }
    ));
    assert_eq!(sync.applied_ops(), before);

    // A request within the min-eligible bound prunes exactly to the least-acknowledged counter.
    let report = sync
        .checkpoint(&CheckpointRequest {
            max_retained_ops: 3, // required_prune = 2 == eligible
            active_peer_leases: vec![lease(9, 4), lease(10, 2)],
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    assert_eq!(report.pruned_ops, 2);
    assert_eq!(report.delta_floor.get(1), Some(2));

    // Ops 3,4,5 — still needed by the least-acknowledged lease — remain deliverable.
    let mut since = StateVector::new();
    since.set(1, 2);
    let counters: Vec<_> = sync
        .encode_changes_since(&since)
        .unwrap()
        .ops
        .iter()
        .map(|operation| operation.id.counter)
        .collect();
    assert_eq!(counters, vec![3, 4, 5]);
}

#[test]
fn checkpoint_rejects_duplicate_peer_leases() {
    let mut sync = SyncState::new();
    sync.apply_op(operation(1));
    let before = sync.applied_ops();
    let error = sync
        .checkpoint(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: vec![
                PeerLease {
                    peer: 9,
                    acknowledged: StateVector::new(),
                },
                PeerLease {
                    peer: 9,
                    acknowledged: StateVector::new(),
                },
            ],
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap_err();
    assert!(matches!(error, CheckpointError::DuplicatePeerLease(9)));
    assert_eq!(sync.applied_ops(), before);
    assert_eq!(sync.checkpoint_epoch(), 0);
}
