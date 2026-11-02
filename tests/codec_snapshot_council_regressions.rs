use md_crdt::codec::{
    BINARY_ENVELOPE_FORMAT, BINARY_ENVELOPE_MAGIC, BinaryOpCodec, BlockKindSkeleton, BlockSkeleton,
    BlockSkeletonInsert, CodecError, DocOp, Envelope, JsonOpCodec, ListItemSkeleton,
    MAX_WIRE_NEST_DEPTH, OpBody, OpCodec, TextUnitWire, WIRE_VERSION,
};
use md_crdt::core::OpId;
use md_crdt::doc::{CodeFenceStyle, ListStyle, Parser, TaskState};
use md_crdt::session::snapshot::{BlockKindDto, SequenceOpDto, TextUnitDto};
use md_crdt::session::{CollaborativeDocument, DocumentDto, SessionSnapshot, SnapshotError};
use md_crdt::sync::ValidationLimits;
use uuid::Uuid;

fn id(counter: u64) -> OpId {
    OpId { counter, peer: 7 }
}

fn insert(kind: BlockKindSkeleton) -> Envelope {
    Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: id(1),
            right_origin: None,
            block: BlockSkeleton {
                block_id: Uuid::from_u128(1),
                kind,
            },
        }),
    }
}

fn child(kind: BlockKindSkeleton, counter: u64) -> BlockSkeletonInsert {
    BlockSkeletonInsert {
        after: None,
        id: id(counter),
        right_origin: None,
        block: BlockSkeleton {
            block_id: Uuid::from_u128(counter.into()),
            kind,
        },
    }
}

fn nested(depth: u32, with_lists: bool) -> Envelope {
    let mut kind = BlockKindSkeleton::Paragraph {
        text: "leaf".into(),
    };
    for level in 0..depth {
        let children = vec![child(kind, u64::from(level) + 2)];
        kind = if with_lists && level % 2 == 0 {
            BlockKindSkeleton::List {
                style: ListStyle::default(),
                items: vec![ListItemSkeleton {
                    after: None,
                    id: id(u64::from(level) + 600),
                    right_origin: None,
                    block_id: Uuid::from_u128(u128::from(level) + 600),
                    task: Some(TaskState::Checked),
                    task_op: id(u64::from(level) + 600),
                    children,
                }],
            }
        } else {
            BlockKindSkeleton::BlockQuote { children }
        };
    }
    insert(kind)
}

fn unchecked_frame(envelope: &Envelope) -> Vec<u8> {
    let body = postcard::to_allocvec(envelope).unwrap();
    let mut frame = Vec::new();
    frame.extend_from_slice(BINARY_ENVELOPE_MAGIC);
    frame.extend_from_slice(&BINARY_ENVELOPE_FORMAT.to_le_bytes());
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

#[test]
fn overdeep_binary_is_rejected_on_a_64k_stack_before_recursive_allocation() {
    for with_lists in [false, true] {
        // Build and serialize the deliberately invalid tree on a separate large
        // stack; only the bounded public decoder runs on the small stack.
        let frame = std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || unchecked_frame(&nested(512, with_lists)))
            .unwrap()
            .join()
            .unwrap();
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || {
                assert!(matches!(
                    BinaryOpCodec.decode(&frame),
                    Err(CodecError::NestDepthExceeded)
                ));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[test]
fn binary_nesting_boundary_and_structural_variants_preserve_the_dto_format() {
    for with_lists in [false, true] {
        let envelope = nested(MAX_WIRE_NEST_DEPTH, with_lists);
        let bytes = BinaryOpCodec.encode(&envelope).unwrap();
        assert_eq!(bytes, unchecked_frame(&envelope));
        assert_eq!(BinaryOpCodec.decode(&bytes).unwrap(), envelope);
        assert!(matches!(
            BinaryOpCodec.decode(&unchecked_frame(&nested(
                MAX_WIRE_NEST_DEPTH + 1,
                with_lists
            ))),
            Err(CodecError::NestDepthExceeded)
        ));
    }
    let leaf_kinds = vec![
        BlockKindSkeleton::Paragraph { text: "é".into() },
        BlockKindSkeleton::Heading {
            level: 3,
            text: "heading".into(),
        },
        BlockKindSkeleton::List {
            style: ListStyle::default(),
            items: Vec::new(),
        },
        BlockKindSkeleton::CodeFence {
            style: CodeFenceStyle::default(),
            info: Some("rust".into()),
            text: "fn main() {}".into(),
        },
        BlockKindSkeleton::BlockQuote {
            children: Vec::new(),
        },
        BlockKindSkeleton::RawBlock {
            raw: "<div>raw</div>".into(),
        },
        BlockKindSkeleton::Table,
    ];
    let children = leaf_kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| child(kind, index as u64 + 2))
        .collect();
    let envelope = insert(BlockKindSkeleton::BlockQuote { children });
    let bytes = BinaryOpCodec.encode(&envelope).unwrap();
    assert_eq!(bytes, unchecked_frame(&envelope));
    assert_eq!(BinaryOpCodec.decode(&bytes).unwrap(), envelope);
    for cut in 10..bytes.len() {
        let mut truncated = bytes[..cut].to_vec();
        truncated[6..10].copy_from_slice(&u32::try_from(cut - 10).unwrap().to_le_bytes());
        assert!(BinaryOpCodec.decode(&truncated).is_err());
    }
}

fn empty_snapshot() -> SessionSnapshot {
    CollaborativeDocument::new(10).save_snapshot().unwrap()
}

#[test]
fn invalid_source_offsets_fail_direct_restore_and_both_snapshot_encodings() {
    let mut snapshot = empty_snapshot();
    snapshot.document = DocumentDto::from_document(&Parser::parse("é source\n"));
    let original = serde_json::to_value(&snapshot).unwrap();
    for (field, value) in [("preamble_end", 99999), ("trailer_start", 1)] {
        let mut invalid = original.clone();
        invalid["document"]["source"][field] = value.into();
        let snapshot: SessionSnapshot = serde_json::from_value(invalid).unwrap();
        assert!(matches!(
            CollaborativeDocument::restore_from_snapshot(snapshot.clone()),
            Err(SnapshotError::InvalidState(_))
        ));
        assert!(matches!(
            SessionSnapshot::from_bytes(&serde_json::to_vec(&snapshot).unwrap()),
            Err(SnapshotError::InvalidState(_))
        ));
        assert!(matches!(
            SessionSnapshot::from_bytes(&snapshot.to_bytes().unwrap()),
            Err(SnapshotError::InvalidState(_))
        ));
    }
    let mut invalid = original;
    let region = invalid["document"]["source"]["regions"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    region["body_end"] = 99999.into();
    let snapshot: SessionSnapshot = serde_json::from_value(invalid).unwrap();
    assert!(matches!(
        CollaborativeDocument::restore_from_snapshot(snapshot),
        Err(SnapshotError::InvalidState(_))
    ));
}

fn buffered_delete(counter: u64) -> (OpId, Vec<u8>) {
    let op_id = id(counter);
    let envelope = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::DeleteBlock {
            parent: None,
            target: id(1),
            id: op_id,
            observed: md_crdt::StateVector::new(),
            subtree: Vec::new(),
        }),
    };
    (op_id, JsonOpCodec.encode(&envelope).unwrap())
}

#[test]
fn malformed_buffered_ids_spans_and_frontiers_fail_before_restore() {
    let pending = buffered_delete(3);
    let mut cases = Vec::new();
    let mut mismatch = empty_snapshot();
    mismatch.pending.push((id(4), pending.1.clone()));
    cases.push(mismatch);
    let mut duplicate = empty_snapshot();
    duplicate.pending = vec![pending.clone(), pending.clone()];
    cases.push(duplicate);
    let mut applied_pending = empty_snapshot();
    applied_pending.state_vector.set(7, 3);
    applied_pending.pending.push(pending.clone());
    cases.push(applied_pending);
    let mut unlogged_deferred = empty_snapshot();
    unlogged_deferred.state_vector.set(7, 3);
    unlogged_deferred.deferred.push(pending);
    cases.push(unlogged_deferred);
    let mut noncontiguous = empty_snapshot();
    let envelope = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertText {
            block_elem: id(1),
            block_id: Uuid::from_u128(1),
            units: vec![
                TextUnitWire {
                    id: id(2),
                    after: None,
                    right_origin: None,
                    grapheme: "a".into(),
                },
                TextUnitWire {
                    id: id(u64::MAX),
                    after: Some(id(2)),
                    right_origin: None,
                    grapheme: "b".into(),
                },
            ],
        }),
    };
    noncontiguous
        .pending
        .push((id(u64::MAX), JsonOpCodec.encode(&envelope).unwrap()));
    cases.push(noncontiguous);
    let mut invalid_sequence = empty_snapshot();
    invalid_sequence
        .document
        .blocks
        .pending
        .push(SequenceOpDto::Delete {
            id: id(0),
            target: id(1),
        });
    cases.push(invalid_sequence);
    for snapshot in cases {
        assert!(matches!(
            SessionSnapshot::from_bytes(&snapshot.to_bytes().unwrap()),
            Err(SnapshotError::InvalidState(_))
        ));
        assert!(matches!(
            CollaborativeDocument::restore_from_snapshot(snapshot),
            Err(SnapshotError::InvalidState(_))
        ));
    }
}

#[test]
fn snapshot_buffer_count_limit_is_checked_before_installation() {
    let mut snapshot = empty_snapshot();
    snapshot.pending = vec![buffered_delete(3); ValidationLimits::default().max_pending_buffer + 1];
    assert!(matches!(
        CollaborativeDocument::restore_from_snapshot(snapshot),
        Err(SnapshotError::InvalidState(_))
    ));
}

#[test]
fn oversized_snapshot_pending_payload_is_refused_before_decoding() {
    let mut snapshot = empty_snapshot();
    snapshot.pending.push((
        id(3),
        vec![b' '; ValidationLimits::default().max_retained_bytes + 1],
    ));
    assert!(matches!(
        CollaborativeDocument::restore_from_snapshot(snapshot),
        Err(SnapshotError::InvalidState(_))
    ));
}

#[test]
fn snapshot_declared_length_mismatch_is_refused() {
    let mut session = CollaborativeDocument::new(11);
    session
        .insert_paragraph(None, &"mixed é text ".repeat(4_000))
        .unwrap();
    let snapshot = session.save_snapshot().unwrap();
    let bytes = snapshot.to_bytes().unwrap();
    let declared_length = u64::from_le_bytes(bytes[5..13].try_into().unwrap());
    let mut wrong_length = bytes;
    wrong_length[5..13].copy_from_slice(&(declared_length + 1).to_le_bytes());
    assert!(matches!(
        SessionSnapshot::from_bytes(&wrong_length),
        Err(SnapshotError::CorruptEncoding(_))
    ));
}

#[test]
fn imported_clock_reserves_text_and_nested_pending_sequence_ids() {
    let mut base = DocumentDto::from_document(&Parser::parse("x"));
    let BlockKindDto::Paragraph { units } =
        &mut base.blocks.elements[0].value.as_mut().unwrap().kind
    else {
        panic!("expected paragraph");
    };
    units.elements[0].id = id(1000);
    units.elements[0].after = None;
    let missing_anchor = OpId {
        counter: 1,
        peer: 99,
    };
    let mut cases = vec![(base.clone(), 1001)];

    let mut pending_delete = base.clone();
    let BlockKindDto::Paragraph { units } = &mut pending_delete.blocks.elements[0]
        .value
        .as_mut()
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    units.pending.push(SequenceOpDto::Delete {
        id: id(2000),
        target: missing_anchor,
    });
    cases.push((pending_delete, 2001));

    let mut pending_text = base.clone();
    let BlockKindDto::Paragraph { units } =
        &mut pending_text.blocks.elements[0].value.as_mut().unwrap().kind
    else {
        unreachable!()
    };
    units.pending.push(SequenceOpDto::Insert {
        after: Some(missing_anchor),
        id: id(3000),
        right_origin: None,
        value: TextUnitDto {
            grapheme: "y".into(),
        },
    });
    cases.push((pending_text, 3001));

    let mut pending_value = base.blocks.elements[0].value.clone().unwrap();
    pending_value.id = Uuid::from_u128(7000);
    pending_value.elem_id = id(4000);
    pending_value.kind_op = id(4000);
    let BlockKindDto::Paragraph { units } = &mut pending_value.kind else {
        unreachable!()
    };
    units.elements[0].id = id(6000);
    base.blocks.pending.push(SequenceOpDto::Insert {
        after: Some(missing_anchor),
        id: id(4000),
        right_origin: None,
        value: pending_value,
    });
    cases.push((base, 6001));

    for (document, expected) in cases {
        let restored = CollaborativeDocument::import_state(
            document,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            7,
            true,
        )
        .unwrap();
        assert_eq!(restored.peek_next_id(), id(expected));
    }
}

#[test]
fn imported_clock_reserves_frontmatter_register_ids_including_deletions() {
    let mut document = DocumentDto::from_document(&Parser::parse("---\ntitle: old\n---\n"));
    document
        .frontmatter
        .as_mut()
        .unwrap()
        .set("title".into(), None, id(8000))
        .unwrap();
    let restored =
        CollaborativeDocument::import_state(document, Vec::new(), Vec::new(), Vec::new(), 7, true)
            .unwrap();
    assert_eq!(restored.peek_next_id(), id(8001));
}

#[test]
fn prior_wire_semantics_fail_closed_in_both_codecs() {
    for version in 0..WIRE_VERSION {
        let mut envelope = insert(BlockKindSkeleton::Paragraph {
            text: String::new(),
        });
        envelope.version = version;
        assert!(matches!(
            JsonOpCodec.decode(&serde_json::to_vec(&envelope).unwrap()),
            Err(CodecError::UnknownVersion(found)) if found == version
        ));
        assert!(matches!(
            BinaryOpCodec.decode(&unchecked_frame(&envelope)),
            Err(CodecError::UnknownVersion(found)) if found == version
        ));
    }
}

#[test]
fn raised_pending_count_cannot_override_the_durable_ceiling() {
    use md_crdt::session::SessionError;
    use md_crdt::{ChangeMessage, Operation, StateVector, ValidationError};
    let count = md_crdt::limits::MAX_DURABLE_PENDING_OPERATIONS;
    let limits = ValidationLimits {
        max_ops_per_message: count + 1,
        max_payload_bytes: md_crdt::limits::MAX_DURABLE_RETAINED_BYTES,
        max_pending_buffer: count + 1,
        ..Default::default()
    };
    let operations = (3..count as u64 + 4)
        .map(|counter| {
            let (id, payload) = buffered_delete(counter);
            Operation {
                id,
                payload: payload.into(),
            }
        })
        .collect::<Vec<_>>();
    let mut target = CollaborativeDocument::new(99);
    let before = target.save_snapshot().unwrap();
    assert!(matches!(
        target.apply_remote(ChangeMessage { since: StateVector::new(), ops: operations }, &limits),
        Err(SessionError::Validation(ValidationError::BufferFull { capacity })) if capacity == count
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
}

#[test]
fn tightened_pending_count_boundary_remains_durably_readable() {
    use md_crdt::session::SessionError;
    use md_crdt::{ChangeMessage, StateVector, ValidationError};
    // Both durable ceilings apply. Large decoded envelopes can exhaust the
    // byte ceiling before 100,000 operations; a caller-tightened count isolates
    // a reachable count boundary without weakening either durable ceiling.
    let count = 128;
    let limits = ValidationLimits {
        max_ops_per_message: count + 1,
        max_pending_buffer: count,
        ..Default::default()
    };
    let mut producer = CollaborativeDocument::new(7);
    producer
        .initialize_frontmatter(md_crdt::doc::Frontmatter::parse("seed: initial".into()))
        .unwrap();
    producer
        .set_frontmatter_field("prefix", Some("ready".into()))
        .unwrap();
    let prefix = producer.encode_changes_since(&StateVector::new()).unwrap();
    let prefix_vector = producer.state_vector();
    for index in 0..=count {
        producer
            .set_frontmatter_field("k", Some(format!("value-{index}")))
            .unwrap();
    }
    let operations = producer.encode_changes_since(&prefix_vector).unwrap().ops;
    let mut target = CollaborativeDocument::new(99);
    let before = target.save_snapshot().unwrap();
    assert!(matches!(
        target.apply_remote(ChangeMessage { since: StateVector::new(), ops: operations.clone() }, &limits),
        Err(SessionError::Validation(ValidationError::BufferFull { capacity })) if capacity == count
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
    target
        .apply_remote(
            ChangeMessage {
                since: StateVector::new(),
                ops: operations.into_iter().take(count).collect(),
            },
            &limits,
        )
        .unwrap();
    let bytes = target.save_snapshot().unwrap().to_bytes().unwrap();
    let mut restored = CollaborativeDocument::restore_from_snapshot_bytes(&bytes).unwrap();
    assert_eq!(
        restored.save_snapshot().unwrap(),
        target.save_snapshot().unwrap()
    );
    #[cfg(feature = "storage")]
    {
        let directory = tempfile::tempdir().unwrap();
        let storage = md_crdt::storage::Storage::open(directory.path()).unwrap();
        target.write_to_storage(&storage).unwrap();
        assert_eq!(
            CollaborativeDocument::read_from_storage(&storage)
                .unwrap()
                .save_snapshot()
                .unwrap(),
            target.save_snapshot().unwrap()
        );
    }
    restored.apply_remote(prefix, &limits).unwrap();
    let frontmatter = restored.document().frontmatter.as_ref().unwrap();
    assert_eq!(frontmatter.get("seed"), Some("initial"));
    assert_eq!(frontmatter.get("prefix"), Some("ready"));
    assert_eq!(
        frontmatter.get("k"),
        Some(format!("value-{}", count - 1).as_str())
    );
    let drained = restored.save_snapshot().unwrap();
    assert!(drained.pending.is_empty());
    assert!(drained.deferred.is_empty());
}

#[test]
fn raised_retained_bytes_cannot_admit_an_unreadable_snapshot() {
    use md_crdt::session::SessionError;
    use md_crdt::{ChangeMessage, Operation, StateVector, ValidationError};
    let ceiling = md_crdt::limits::MAX_DURABLE_RETAINED_BYTES;
    let limits = ValidationLimits {
        max_retained_bytes: ceiling * 2,
        max_payload_bytes: ceiling,
        ..Default::default()
    };
    let raw = "x".repeat(1024 * 1024);
    let ops = (1..=33)
        .map(|peer| {
            let op_id = OpId { peer, counter: 2 };
            let envelope = Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::InsertBlock {
                    parent: None,
                    after: None,
                    id: op_id,
                    right_origin: None,
                    block: BlockSkeleton {
                        block_id: Uuid::from_u128(peer.into()),
                        kind: BlockKindSkeleton::RawBlock { raw: raw.clone() },
                    },
                }),
            };
            Operation {
                id: op_id,
                payload: JsonOpCodec.encode(&envelope).unwrap().into(),
            }
        })
        .collect();
    let mut target = CollaborativeDocument::new(99);
    let before = target.save_snapshot().unwrap();
    assert!(matches!(
        target.apply_remote(ChangeMessage { since: StateVector::new(), ops }, &limits),
        Err(SessionError::Validation(ValidationError::ResourceLimitExceeded { limit, actual }))
            if limit == ceiling && actual > ceiling
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
}

#[test]
fn restore_reports_conflicting_pending_identity_as_typed_validation() {
    use md_crdt::sync::{MalformedKind, ValidationError};
    let mut snapshot = empty_snapshot();
    for peer in [1, 2] {
        let op_id = OpId { peer, counter: 2 };
        let envelope = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertBlock {
                parent: None,
                after: None,
                id: op_id,
                right_origin: None,
                block: BlockSkeleton {
                    block_id: Uuid::from_u128(100),
                    kind: BlockKindSkeleton::Paragraph {
                        text: String::new(),
                    },
                },
            }),
        };
        snapshot
            .pending
            .push((op_id, JsonOpCodec.encode(&envelope).unwrap()));
    }
    for encoded in [false, true] {
        let result = if encoded {
            CollaborativeDocument::restore_from_snapshot_bytes(&snapshot.to_bytes().unwrap())
        } else {
            CollaborativeDocument::restore_from_snapshot(snapshot.clone())
        };
        assert!(matches!(
            result,
            Err(SnapshotError::Validation(
                ValidationError::MalformedOperation {
                    kind: MalformedKind::InvalidSequence,
                    ..
                }
            ))
        ));
    }
}

#[test]
fn generic_applied_snapshot_retransmits_and_checkpoint_rebases_as_json() {
    use md_crdt::{CheckpointRequest, DocumentTombstonePolicy, StateVector, SyncResponse};
    let mut author = CollaborativeDocument::with_codec(1, BinaryOpCodec, true);
    author
        .insert_paragraph(None, "binary persisted text")
        .unwrap();
    let mut binary_peer = CollaborativeDocument::with_codec(2, BinaryOpCodec, true);
    binary_peer
        .apply_remote(
            author.encode_changes_since(&StateVector::new()).unwrap(),
            &ValidationLimits::default(),
        )
        .unwrap();
    let restored = CollaborativeDocument::restore_from_snapshot_bytes(
        &binary_peer.save_snapshot().unwrap().to_bytes().unwrap(),
    )
    .unwrap();
    let mut json_peer = CollaborativeDocument::new(3);
    json_peer
        .apply_remote(
            restored.encode_changes_since(&StateVector::new()).unwrap(),
            &ValidationLimits::default(),
        )
        .unwrap();
    assert_eq!(json_peer.document(), restored.document());
    binary_peer
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let SyncResponse::Rebase { checkpoint } = binary_peer.sync_since(&StateVector::new()).unwrap()
    else {
        panic!("late peer requires a full checkpoint");
    };
    let mut late = CollaborativeDocument::rebase_from_snapshot(*checkpoint, 4).unwrap();
    assert_eq!(late.document(), restored.document());
    late.insert_paragraph(None, "late edit").unwrap();
    let mut resumed =
        CollaborativeDocument::restore_from_snapshot(binary_peer.save_snapshot().unwrap()).unwrap();
    resumed
        .apply_remote(
            late.encode_changes_since(&resumed.state_vector()).unwrap(),
            &ValidationLimits::default(),
        )
        .unwrap();
    assert_eq!(resumed.document(), late.document());
}

#[test]
fn generic_pending_and_deferred_snapshots_resume_after_late_dependencies() {
    use md_crdt::{ChangeMessage, CheckpointRequest, DocumentTombstonePolicy, StateVector};
    let limits = ValidationLimits::default();
    let mut author = CollaborativeDocument::with_codec(1, BinaryOpCodec, true);
    let block = author.insert_paragraph(None, "").unwrap();
    let base = author.encode_changes_since(&StateVector::new()).unwrap();
    let mut editor = CollaborativeDocument::with_codec(2, BinaryOpCodec, true);
    editor.apply_remote(base, &limits).unwrap();
    editor
        .insert_text(md_crdt::block_id_from_op(block), 0, "deferred text")
        .unwrap();
    author
        .insert_text(md_crdt::block_id_from_op(block), 0, "pending text")
        .unwrap();
    for deferred in [false, true] {
        let message = if deferred {
            editor.encode_changes_since(&author.state_vector()).unwrap()
        } else {
            let mut delta = author.encode_changes_since(&StateVector::new()).unwrap();
            delta.ops.retain(|op| op.id != block);
            delta
        };
        let mut target = CollaborativeDocument::with_codec(3, BinaryOpCodec, true);
        target.apply_remote(message, &limits).unwrap();
        for compacted in [false, true] {
            if compacted {
                target
                    .checkpoint_history(&CheckpointRequest {
                        max_retained_ops: 0,
                        active_peer_leases: Vec::new(),
                        tombstones: DocumentTombstonePolicy::KeepAll,
                    })
                    .unwrap();
            }
            let snapshot = target.save_snapshot().unwrap();
            assert_eq!(snapshot.pending.len(), usize::from(!deferred));
            assert_eq!(snapshot.deferred.len(), usize::from(deferred));
            let mut restored =
                CollaborativeDocument::restore_from_snapshot_bytes(&snapshot.to_bytes().unwrap())
                    .unwrap();
            let canonical_base = author
                .save_snapshot()
                .unwrap()
                .ops
                .into_iter()
                .filter(|(id, _)| *id == block)
                .map(|(id, payload)| md_crdt::Operation {
                    id,
                    payload: payload.into(),
                })
                .collect();
            restored
                .apply_remote(
                    ChangeMessage {
                        since: StateVector::new(),
                        ops: canonical_base,
                    },
                    &limits,
                )
                .unwrap();
            let expected = if deferred {
                editor.document()
            } else {
                author.document()
            };
            assert_eq!(restored.document(), expected);
            assert!(restored.save_snapshot().unwrap().pending.is_empty());
            assert!(restored.save_snapshot().unwrap().deferred.is_empty());
            let rebased =
                CollaborativeDocument::rebase_from_snapshot(restored.save_snapshot().unwrap(), 5)
                    .unwrap();
            assert_eq!(rebased.document(), expected);
            if !compacted {
                let mut retransmit = CollaborativeDocument::new(6);
                retransmit
                    .apply_remote(
                        restored.encode_changes_since(&StateVector::new()).unwrap(),
                        &limits,
                    )
                    .unwrap();
                assert_eq!(retransmit.document(), expected);
            }
        }
    }
}

#[test]
fn binary_pending_admission_charges_the_larger_json_snapshot_representation() {
    use md_crdt::session::SessionError;
    use md_crdt::{ChangeMessage, Operation, StateVector, ValidationError};
    let ceiling = md_crdt::limits::MAX_DURABLE_RETAINED_BYTES;
    // Backslashes occupy one byte in postcard but two bytes in canonical JSON.
    // 17 MiB of binary raw bytes plus about 34 MiB of decoded charge fits, but
    // 34 MiB of persisted JSON raw copies plus 34 MiB of decoded charge does not.
    let raw = "\\".repeat(1024 * 1024);
    let ops = (1..=17)
        .map(|peer| {
            let op_id = OpId { peer, counter: 2 };
            let envelope = Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::InsertBlock {
                    parent: None,
                    after: None,
                    id: op_id,
                    right_origin: None,
                    block: BlockSkeleton {
                        block_id: Uuid::from_u128(peer.into()),
                        kind: BlockKindSkeleton::RawBlock { raw: raw.clone() },
                    },
                }),
            };
            Operation {
                id: op_id,
                payload: BinaryOpCodec.encode(&envelope).unwrap().into(),
            }
        })
        .collect();
    let mut target = CollaborativeDocument::with_codec(99, BinaryOpCodec, true);
    let before = target.save_snapshot().unwrap();
    assert!(matches!(
        target.apply_remote(ChangeMessage { since: StateVector::new(), ops }, &ValidationLimits {
            max_payload_bytes: ceiling, max_retained_bytes: ceiling * 2, ..Default::default()
        }),
        Err(SessionError::Validation(ValidationError::ResourceLimitExceeded { limit, actual }))
            if limit == ceiling && actual > ceiling
    ));
    assert_eq!(target.save_snapshot().unwrap(), before);
}

/// Transport framing may expose canonical JSON as a borrowed proper subslice.
#[derive(Clone, Copy)]
struct BorrowedPrefixCodec;

impl OpCodec for BorrowedPrefixCodec {
    type Error = CodecError;

    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>, Self::Error> {
        let mut payload = b"PRE".to_vec();
        payload.extend(JsonOpCodec.encode(envelope)?);
        Ok(payload)
    }

    fn decode(&self, payload: &[u8]) -> Result<Envelope, Self::Error> {
        JsonOpCodec.decode(
            payload
                .strip_prefix(b"PRE")
                .ok_or(CodecError::Invalid("missing prefix"))?,
        )
    }

    fn snapshot_payload<'a>(
        &self,
        payload: &'a [u8],
        _envelope: Option<&Envelope>,
    ) -> Result<std::borrow::Cow<'a, [u8]>, CodecError> {
        Ok(std::borrow::Cow::Borrowed(
            payload
                .strip_prefix(b"PRE")
                .ok_or(CodecError::Invalid("missing prefix"))?,
        ))
    }

    fn snapshot_payload_len(
        &self,
        payload: &[u8],
        _envelope: &Envelope,
    ) -> Result<usize, CodecError> {
        Ok(payload
            .strip_prefix(b"PRE")
            .ok_or(CodecError::Invalid("missing prefix"))?
            .len())
    }
}

#[test]
fn borrowed_canonical_subslice_applied_snapshot_retransmits_and_retained_rebase_agrees() {
    use md_crdt::{CheckpointRequest, DocumentTombstonePolicy, StateVector, SyncResponse};
    let limits = ValidationLimits::default();
    let mut author = CollaborativeDocument::with_codec(1, BorrowedPrefixCodec, true);
    author
        .insert_paragraph(None, "framed applied text")
        .unwrap();
    let snapshot = author.save_snapshot().unwrap();
    for (_, payload) in &snapshot.ops {
        JsonOpCodec.decode(payload).unwrap();
    }
    let recovered =
        CollaborativeDocument::restore_from_snapshot_bytes(&snapshot.to_bytes().unwrap()).unwrap();
    let mut receiver = CollaborativeDocument::new(2);
    receiver
        .apply_remote(
            recovered.encode_changes_since(&StateVector::new()).unwrap(),
            &limits,
        )
        .unwrap();
    assert_eq!(receiver.document(), author.document());
    // Keep one retained framed operation after checkpoint so the full rebase
    // still exercises applied-payload normalization, not only materialization.
    author
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 1,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let SyncResponse::Rebase { checkpoint } = author.sync_since(&StateVector::new()).unwrap()
    else {
        panic!("checkpointed prefix must require rebase");
    };
    assert_eq!(checkpoint.ops.len(), 1);
    for (_, payload) in &checkpoint.ops {
        JsonOpCodec.decode(payload).unwrap();
    }
    let mut late = CollaborativeDocument::rebase_from_snapshot(*checkpoint, 3).unwrap();
    assert_eq!(late.document(), author.document());
    late.insert_paragraph(None, "post-rebase").unwrap();
    let mut resumed =
        CollaborativeDocument::restore_from_snapshot(author.save_snapshot().unwrap()).unwrap();
    resumed
        .apply_remote(
            late.encode_changes_since(&resumed.state_vector()).unwrap(),
            &limits,
        )
        .unwrap();
    assert_eq!(resumed.document(), late.document());
}

#[test]
fn borrowed_canonical_pending_and_deferred_restore_then_retransmit_after_dependencies() {
    use md_crdt::{ChangeMessage, StateVector};
    let limits = ValidationLimits::default();
    let mut author = CollaborativeDocument::with_codec(1, BorrowedPrefixCodec, true);
    let block = author.insert_paragraph(None, "").unwrap();
    let mut editor = CollaborativeDocument::with_codec(2, BorrowedPrefixCodec, true);
    editor
        .apply_remote(
            author.encode_changes_since(&StateVector::new()).unwrap(),
            &limits,
        )
        .unwrap();
    editor
        .insert_text(md_crdt::block_id_from_op(block), 0, "deferred framed text")
        .unwrap();
    author
        .insert_text(md_crdt::block_id_from_op(block), 0, "pending framed text")
        .unwrap();
    for deferred in [false, true] {
        let message = if deferred {
            editor.encode_changes_since(&author.state_vector()).unwrap()
        } else {
            let mut delta = author.encode_changes_since(&StateVector::new()).unwrap();
            delta.ops.retain(|operation| operation.id != block);
            delta
        };
        let mut held = CollaborativeDocument::with_codec(3, BorrowedPrefixCodec, true);
        held.apply_remote(message, &limits).unwrap();
        let snapshot = held.save_snapshot().unwrap();
        assert_eq!(snapshot.pending.len(), usize::from(!deferred));
        assert_eq!(snapshot.deferred.len(), usize::from(deferred));
        for (_, payload) in snapshot
            .ops
            .iter()
            .chain(&snapshot.pending)
            .chain(&snapshot.deferred)
        {
            JsonOpCodec.decode(payload).unwrap();
        }
        let mut recovered = CollaborativeDocument::rebase_from_snapshot(snapshot, 4).unwrap();
        let ops = author
            .save_snapshot()
            .unwrap()
            .ops
            .into_iter()
            .filter(|(id, _)| *id == block)
            .map(|(id, payload)| md_crdt::Operation {
                id,
                payload: payload.into(),
            })
            .collect();
        recovered
            .apply_remote(
                ChangeMessage {
                    since: StateVector::new(),
                    ops,
                },
                &limits,
            )
            .unwrap();
        let expected = if deferred {
            editor.document()
        } else {
            author.document()
        };
        assert_eq!(recovered.document(), expected);
        let mut receiver = CollaborativeDocument::new(5);
        receiver
            .apply_remote(
                recovered.encode_changes_since(&StateVector::new()).unwrap(),
                &limits,
            )
            .unwrap();
        assert_eq!(receiver.document(), expected);
        let final_snapshot = recovered.save_snapshot().unwrap();
        assert!(final_snapshot.pending.is_empty());
        assert!(final_snapshot.deferred.is_empty());
        assert_eq!(
            CollaborativeDocument::restore_from_snapshot_bytes(&final_snapshot.to_bytes().unwrap())
                .unwrap()
                .document(),
            expected
        );
    }
}

#[test]
fn bytes_and_mutable_dto_restore_normalize_queues_and_reject_changed_raw_identity() {
    use md_crdt::doc::{ColumnAlignment, ColumnDef, PendingColumnAlignment};
    use md_crdt::{StateVector, block_id_from_op};
    let limits = ValidationLimits::default();
    let mut base = CollaborativeDocument::new(1);
    let table = base
        .insert_table(
            None,
            vec![ColumnDef {
                alignment: ColumnAlignment::Left,
            }],
            vec!["base".into()],
        )
        .unwrap();
    let table_id = block_id_from_op(table);
    let common = base.save_snapshot().unwrap();
    let mut columns = CollaborativeDocument::rebase_from_snapshot(common.clone(), 3).unwrap();
    let column = columns
        .insert_table_column(table_id, None, ColumnAlignment::Left, "late".into())
        .unwrap();
    let mut editor =
        CollaborativeDocument::rebase_from_snapshot(columns.save_snapshot().unwrap(), 2).unwrap();
    editor
        .set_table_column_alignment(table_id, block_id_from_op(column), ColumnAlignment::Right)
        .unwrap();
    let delta = editor
        .encode_changes_since(&columns.state_vector())
        .unwrap();
    let operation = &delta.ops[0];
    let envelope = JsonOpCodec.decode(&operation.payload).unwrap();
    let OpBody::Doc(DocOp::SetTableColumnAlignment {
        column_id,
        observed,
        ..
    }) = envelope.body
    else {
        panic!("expected alignment producer");
    };
    // Represent an imported durable document queue using the real producer's
    // exact operation identity, observed frontier and retained raw payload.
    let mut queued = common;
    queued
        .state_vector
        .set(operation.id.peer, operation.id.counter);
    queued.ops.push((operation.id, operation.payload.to_vec()));
    let BlockKindDto::Table { table: dto } = &mut queued
        .document
        .blocks
        .elements
        .iter_mut()
        .filter_map(|element| element.value.as_mut())
        .find(|block| block.id == table_id)
        .unwrap()
        .kind
    else {
        panic!("expected table");
    };
    dto.pending_column_alignments.push(PendingColumnAlignment {
        column_id,
        alignment: ColumnAlignment::Right,
        id: operation.id,
        observed,
    });
    let bytes = queued.to_bytes().unwrap();
    let mut normalized = None;
    for native_bytes in [false, true] {
        let mut restored = if native_bytes {
            CollaborativeDocument::restore_from_snapshot_bytes(&bytes).unwrap()
        } else {
            CollaborativeDocument::restore_from_snapshot(
                SessionSnapshot::from_bytes(&bytes).unwrap(),
            )
            .unwrap()
        };
        let snapshot = restored.save_snapshot().unwrap();
        assert_eq!(snapshot.deferred.len(), 1);
        if let Some(expected) = &normalized {
            assert_eq!(&snapshot, expected);
        } else {
            normalized = Some(snapshot);
        }
        restored
            .apply_remote(
                columns
                    .encode_changes_since(&restored.state_vector())
                    .unwrap(),
                &limits,
            )
            .unwrap();
        assert_eq!(restored.document(), editor.document());
        assert!(restored.save_snapshot().unwrap().deferred.is_empty());
    }
    // A mutable public DTO cannot carry a validation proof. Changing queued
    // meaning after decode must be caught again after normalization, even
    // though the queued operation retains the same ID and valid shape.
    let mut changed = SessionSnapshot::from_bytes(&bytes).unwrap();
    let BlockKindDto::Table { table: dto } = &mut changed
        .document
        .blocks
        .elements
        .iter_mut()
        .filter_map(|element| element.value.as_mut())
        .find(|block| block.id == table_id)
        .unwrap()
        .kind
    else {
        panic!("expected table");
    };
    dto.pending_column_alignments[0].alignment = ColumnAlignment::Center;
    for native_bytes in [false, true] {
        let result = if native_bytes {
            CollaborativeDocument::restore_from_snapshot_bytes(&changed.to_bytes().unwrap())
        } else {
            CollaborativeDocument::restore_from_snapshot(changed.clone())
        };
        assert!(matches!(result, Err(SnapshotError::InvalidState(_))));
    }
    let mut changed_frontier = SessionSnapshot::from_bytes(&bytes).unwrap();
    changed_frontier.state_vector = StateVector::new();
    assert!(matches!(
        CollaborativeDocument::restore_from_snapshot(changed_frontier),
        Err(SnapshotError::InvalidState(_))
    ));
}
