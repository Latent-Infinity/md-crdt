#![cfg(feature = "filesync")]

use md_crdt::doc::{EquivalenceMode, Parser};
use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{CollaborativeDocument, DocumentExportRequest, StateVector, ValidationLimits};
use std::fs;
use tempfile::tempdir;

fn render(vault: &mut VaultSession, mode: EquivalenceMode) -> String {
    vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(mode)
}

fn first_id(vault: &mut VaultSession) -> md_crdt::BlockId {
    vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id
}

#[test]
fn replay_reconciles_clock_before_next_edit_and_snapshot_restore() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "a").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let id = first_id(&mut vault);
    let mut replica = CollaborativeDocument::restore_from_snapshot(
        vault
            .session_mut("note.md")
            .unwrap()
            .save_snapshot()
            .unwrap(),
    )
    .unwrap();
    replica.rebind_peer(vault.peer().wrapping_add(1).max(1));
    let first = vault
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 1, "b").unwrap()
        })
        .unwrap();
    drop(vault);

    let mut vault = VaultSession::open(root.path()).unwrap();
    let second = vault
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 2, "c").unwrap()
        })
        .unwrap();
    assert!(second.value.unwrap().counter > first.value.unwrap().counter);
    assert_eq!(render(&mut vault, EquivalenceMode::Structural), "abc");
    let session = vault.session_mut("note.md").unwrap();
    let message = session
        .encode_changes_since(&replica.state_vector())
        .unwrap();
    replica
        .apply_remote(message, &ValidationLimits::default())
        .unwrap();
    assert_eq!(
        replica.document().serialize(EquivalenceMode::Structural),
        "abc"
    );
    assert_eq!(replica.state_vector(), session.state_vector());
    let restored =
        CollaborativeDocument::restore_from_snapshot(session.save_snapshot().unwrap()).unwrap();
    assert_eq!(restored.peek_next_id(), session.peek_next_id());
    assert_eq!(
        restored.document().serialize(EquivalenceMode::Structural),
        "abc"
    );
}

#[test]
fn oversized_payload_flush_reopens_and_remains_editable() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "a").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let id = first_id(&mut vault);
    let before = vault.session_mut("note.md").unwrap().state_vector();
    let paste = "b".repeat(120_000);
    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(id, 1, &paste)
        .unwrap();
    let message = vault
        .session_mut("note.md")
        .unwrap()
        .encode_changes_since(&before)
        .unwrap();
    assert!(message.ops.iter().map(|op| op.payload.len()).sum::<usize>() > 10 * 1024 * 1024);
    vault.save_state("note.md").unwrap();
    drop(message);
    drop(vault);
    let mut vault = VaultSession::open(root.path()).unwrap();
    assert_eq!(
        render(&mut vault, EquivalenceMode::Structural),
        format!("a{paste}")
    );
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 120_001, "c").unwrap()
        })
        .unwrap();
    drop(vault);
    let mut vault = VaultSession::open(root.path()).unwrap();
    assert_eq!(
        render(&mut vault, EquivalenceMode::Structural),
        format!("a{paste}c")
    );
}

#[test]
fn operation_count_over_replay_boundary_uses_reopenable_snapshot() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "# heading\n").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let id = first_id(&mut vault);
    let before = vault.session_mut("note.md").unwrap().state_vector();
    let session = vault.session_mut("note.md").unwrap();
    for index in 0..10_001 {
        session
            .convert_text_block(
                id,
                md_crdt::TextBlockKind::Heading {
                    level: 1 + (index % 2) as u8,
                },
            )
            .unwrap();
    }
    assert_eq!(
        session.encode_changes_since(&before).unwrap().ops.len(),
        10_001
    );
    vault.save_state("note.md").unwrap();
    drop(vault);
    let mut vault = VaultSession::open(root.path()).unwrap();
    assert_eq!(render(&mut vault, EquivalenceMode::Structural), "# heading");
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 7, "!").unwrap()
        })
        .unwrap();
    assert_eq!(
        render(&mut vault, EquivalenceMode::Structural),
        "# heading!"
    );
}

#[test]
fn refresh_reconciles_heading_code_and_raw_semantics_before_adopting_source() {
    let root = tempdir().unwrap();
    let initial =
        "# same heading\n\n```rust\nlet shared = old;\n```\n\n:::custom\nshared raw old\n";
    let changed =
        "## same heading\n\n~~~~python\nlet shared = new;\n~~~~\n\n:::custom\nshared raw new\n";
    fs::write(root.path().join("note.md"), initial).unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let original_ids: Vec<_> = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()
        .iter()
        .map(|block| block.id)
        .collect();
    let mut replica = CollaborativeDocument::restore_from_snapshot(
        vault
            .session_mut("note.md")
            .unwrap()
            .save_snapshot()
            .unwrap(),
    )
    .unwrap();
    replica.rebind_peer(vault.peer().wrapping_add(1).max(1));
    fs::write(root.path().join("note.md"), changed).unwrap();
    vault.ingest_markdown("note.md", None, None).unwrap();
    let expected = Parser::parse(changed).serialize(EquivalenceMode::Structural);
    assert_eq!(render(&mut vault, EquivalenceMode::Exact), changed);
    assert_eq!(render(&mut vault, EquivalenceMode::Structural), expected);
    let session = vault.session_mut("note.md").unwrap();
    let ids: Vec<_> = session
        .document()
        .blocks_in_order()
        .iter()
        .map(|block| block.id)
        .collect();
    assert_eq!(ids, original_ids);
    let delta = session
        .encode_changes_since(&replica.state_vector())
        .unwrap();
    replica
        .apply_remote(delta, &ValidationLimits::default())
        .unwrap();
    assert_eq!(
        replica.document().serialize(EquivalenceMode::Structural),
        expected
    );
    assert_eq!(replica.state_vector(), session.state_vector());
    let heading = ids[0];
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(heading, 12, "!").unwrap()
        })
        .unwrap();
    assert!(render(&mut vault, EquivalenceMode::Exact).starts_with("## same heading!"));
    let expected = render(&mut vault, EquivalenceMode::Structural);
    drop(vault);
    let mut vault = VaultSession::open(root.path()).unwrap();
    assert_eq!(render(&mut vault, EquivalenceMode::Structural), expected);
}

#[test]
fn candidate_refusal_rolls_back_frontmatter_clocks_ids_and_baseline() {
    let root = tempdir().unwrap();
    let original = format!(
        "---\ntitle: before\n---\n\n{}",
        vec!["repeat words"; 501].join("\n\n")
    );
    fs::write(root.path().join("note.md"), &original).unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let before = vault
        .session_mut("note.md")
        .unwrap()
        .save_snapshot()
        .unwrap();
    let revision = vault.revision("note.md").unwrap();
    fs::write(
        root.path().join("note.md"),
        original.replace("title: before", "title: after"),
    )
    .unwrap();
    assert!(matches!(
        vault.ingest_markdown("note.md", None, None),
        Err(VaultError::IngestMatchLimit { .. })
    ));
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .save_snapshot()
            .unwrap(),
        before
    );
    assert_eq!(vault.revision("note.md").unwrap(), revision);
    fs::write(root.path().join("note.md"), &original).unwrap();
    assert!(
        !vault
            .ingest_markdown("note.md", None, None)
            .unwrap()
            .changed
    );
}

#[test]
fn malformed_recovery_intents_never_modify_outside_or_forged_paths() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("vault");
    fs::create_dir(&root).unwrap();
    let outside = parent.path().join("outside.md");
    fs::write(&outside, "untouched").unwrap();
    let transaction_dir = root.join(".mdcrdt/transactions");
    fs::create_dir_all(&transaction_dir).unwrap();
    let id = uuid::Uuid::new_v4();
    for path in [
        outside.clone(),
        std::path::PathBuf::from("../outside.md"),
        std::path::PathBuf::from("."),
    ] {
        let journal = serde_json::json!({"kind":"delete", "path":path, "document_id":id});
        fs::write(
            transaction_dir.join("intent.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        assert!(VaultSession::open(&root).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
    }
    fs::write(root.join("note.md"), "original").unwrap();
    fs::write(root.join("innocent.md"), "must remain").unwrap();
    let journal = serde_json::json!({"kind":"export", "entries":[{
        "target":"note.md", "pending":"innocent.md", "backup":format!(".note.md.{id}.backup")
    }]});
    fs::write(
        transaction_dir.join("intent.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        VaultSession::open(&root),
        Err(VaultError::InvalidJournal(_))
    ));
    assert_eq!(
        fs::read_to_string(root.join("note.md")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(root.join("innocent.md")).unwrap(),
        "must remain"
    );
    let pending = format!(".note.md.{id}.pending");
    let journal = serde_json::json!({"kind":"export", "entries":[{
        "target":"note.md", "pending":pending, "backup":"innocent.md"
    }]});
    fs::write(
        transaction_dir.join("intent.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    let valid = serde_json::json!({"kind":"delete", "path":"innocent.md", "document_id":id});
    fs::write(
        transaction_dir.join("000-first.json"),
        serde_json::to_vec(&valid).unwrap(),
    )
    .unwrap();
    assert!(VaultSession::open(&root).is_err());
    assert_eq!(
        fs::read_to_string(root.join("innocent.md")).unwrap(),
        "must remain"
    );
}

#[cfg(unix)]
#[test]
fn single_and_multi_exports_preserve_existing_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "private").unwrap();
    fs::set_permissions(
        root.path().join("note.md"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(root.path().join("other.md"), "shared").unwrap();
    fs::set_permissions(
        root.path().join("other.md"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let id = first_id(&mut vault);
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 7, " one").unwrap()
        })
        .unwrap();
    let revision = vault.revision("note.md").unwrap();
    vault.export_markdown("note.md", &revision, None).unwrap();
    assert_eq!(
        fs::metadata(root.path().join("note.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let document_id = vault.document_id("note.md").unwrap();
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 11, " two").unwrap()
        })
        .unwrap();
    let other_id = vault
        .session_mut("other.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id;
    vault
        .with_local_edit("other.md", |session| {
            session.insert_text(other_id, 6, " changed").unwrap()
        })
        .unwrap();
    let other_document_id = vault.document_id("other.md").unwrap();
    let other_revision = vault.revision("other.md").unwrap();
    let revision = vault.revision("note.md").unwrap();
    vault
        .export_markdown_transaction(vec![
            DocumentExportRequest {
                path: "note.md".into(),
                document_id,
                expected_revision: revision,
                expected_disk_fingerprint: None,
            },
            DocumentExportRequest {
                path: "other.md".into(),
                document_id: other_document_id,
                expected_revision: other_revision,
                expected_disk_fingerprint: None,
            },
        ])
        .unwrap();
    assert_eq!(
        fs::metadata(root.path().join("note.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        fs::read_to_string(root.path().join("note.md"))
            .unwrap()
            .contains("private one two")
    );
    assert_eq!(
        fs::metadata(root.path().join("other.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert!(
        fs::read_to_string(root.path().join("other.md"))
            .unwrap()
            .contains("shared changed")
    );
}

#[test]
fn revision_stream_retains_the_exact_original_hash_contract() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "# Styled\r\n\r\nbody\r\n").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let session = vault.session_mut("note.md").unwrap();
    let mut material = Vec::new();
    material.extend_from_slice(&session.peer().to_le_bytes());
    material.push(u8::from(session.unit_mode()));
    for (peer, counter) in session.state_vector().iter() {
        material.extend_from_slice(&peer.to_le_bytes());
        material.extend_from_slice(&counter.to_le_bytes());
    }
    material.extend_from_slice(
        session
            .document()
            .serialize(EquivalenceMode::Exact)
            .as_bytes(),
    );
    let expected = md_crdt::RevisionToken::from_u64(rapidhash::v3::rapidhash_v3(&material));
    assert_eq!(vault.revision("note.md").unwrap(), expected);
}

#[test]
fn persisted_replay_resource_refusal_keeps_its_typed_cause() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "a").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let storage =
        md_crdt::storage::Storage::open(root.path().join(".mdcrdt/sessions/note.mdcrdt")).unwrap();
    let operation = md_crdt::sync::Operation {
        id: md_crdt::OpId {
            peer: 7,
            counter: 1,
        },
        payload: std::sync::Arc::from(&b"x"[..]),
    };
    let message = md_crdt::sync::ChangeMessage {
        since: StateVector::default(),
        ops: vec![operation; 10_001],
    };
    storage
        .append_op_segment(&serde_json::to_vec(&message).unwrap())
        .unwrap();
    drop(vault);
    let mut vault = VaultSession::open(root.path()).unwrap();
    assert!(matches!(
        vault.session_mut("note.md"),
        Err(VaultError::Session(md_crdt::SessionError::Validation(
            md_crdt::sync::ValidationError::ResourceLimitExceeded {
                limit: 10_000,
                actual: 10_001
            }
        )))
    ));
}

#[test]
fn sparse_fingerprints_keep_all_identities_without_dense_candidates() {
    use md_crdt::filesync::{
        BlockFingerprint, Fingerprint, LastFlushedState, MatchConfig, ParsedBlock, Score,
        match_blocks,
    };
    let blocks: Vec<_> = (0..1_000usize)
        .map(|index| BlockFingerprint {
            block_id: uuid::Uuid::from_u128(index as u128 + 1),
            fingerprint: Fingerprint::from_content(&format!("unique-{index}")),
            container_path: Vec::new(),
            position: index,
        })
        .collect();
    let parsed: Vec<_> = blocks
        .iter()
        .map(|block| ParsedBlock {
            fingerprint: block.fingerprint.clone(),
            container_path: Vec::new(),
            position: block.position,
        })
        .collect();
    let state = LastFlushedState {
        content_hash: 0,
        blocks,
    };
    let mapping = match_blocks(
        &state,
        &parsed,
        &MatchConfig {
            min_match_score: Score(5000),
            ..MatchConfig::default()
        },
    )
    .unwrap();
    assert!(mapping.added.is_empty());
    assert!(mapping.removed.is_empty());
    assert_eq!(mapping.matched.len(), state.blocks.len());
    for matched in mapping.matched {
        assert_eq!(matched.old_id, state.blocks[matched.new_index].block_id);
    }
}

#[test]
fn indexed_row_matching_preserves_duplicate_row_tie_breaking() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("note.md"),
        "| value |\n| --- |\n| same |\n| same |\n| other |\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let old_ids = match &vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .kind
    {
        md_crdt::BlockKind::Table { table } => {
            table.rows.iter().map(|row| row.id).collect::<Vec<_>>()
        }
        _ => panic!("table expected"),
    };
    let changed = "| value |\n| --- |\n| other |\n| same |\n| same |\n";
    fs::write(root.path().join("note.md"), changed).unwrap();
    vault.ingest_markdown("note.md", None, None).unwrap();
    let new_ids = match &vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .kind
    {
        md_crdt::BlockKind::Table { table } => {
            table.rows.iter().map(|row| row.id).collect::<Vec<_>>()
        }
        _ => panic!("table expected"),
    };
    assert_eq!(new_ids, vec![old_ids[2], old_ids[0], old_ids[1]]);
    assert_eq!(
        render(&mut vault, EquivalenceMode::Structural),
        Parser::parse(changed).serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn peer_identity_publication_remains_stable_across_fresh_opens() {
    let root = tempdir().unwrap();
    let first = VaultSession::open(root.path()).unwrap().peer();
    assert_ne!(first, 0);
    let raw = fs::read_to_string(root.path().join(".mdcrdt/peer_id")).unwrap();
    assert_eq!(raw, format!("{first}\n"));
    for _ in 0..4 {
        assert_eq!(VaultSession::open(root.path()).unwrap().peer(), first);
    }
}

#[test]
fn invalid_snapshot_keeps_its_typed_restore_cause() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "a").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let mut snapshot = vault
        .session_mut("note.md")
        .unwrap()
        .save_snapshot()
        .unwrap();
    snapshot.format_version = u16::MAX;
    let storage =
        md_crdt::storage::Storage::open(root.path().join(".mdcrdt/sessions/note.mdcrdt")).unwrap();
    storage
        .write_snapshot(&serde_json::to_vec(&snapshot).unwrap(), &[], false)
        .unwrap();
    drop(vault);
    let mut vault = VaultSession::open(root.path()).unwrap();
    assert!(matches!(
        vault.session_mut("note.md"),
        Err(VaultError::Snapshot(
            md_crdt::SnapshotError::ReinitializeRequired {
                found: u16::MAX,
                ..
            }
        ))
    ));
}

#[test]
fn six_grapheme_batch_below_payload_limit_flushes_and_reopens() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "a").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let id = first_id(&mut vault);
    let before = vault.session_mut("note.md").unwrap().state_vector();
    vault
        .apply_local_edit("note.md", |session| {
            for index in 0..9_999 {
                session.insert_text(id, 1 + index * 6, "abcdef").unwrap();
            }
        })
        .unwrap();
    let delta = vault
        .session_mut("note.md")
        .unwrap()
        .encode_changes_since(&before)
        .unwrap();
    assert_eq!(delta.ops.len(), 9_999);
    assert!(
        delta.ops.iter().map(|op| op.payload.len()).sum::<usize>()
            < ValidationLimits::default().max_payload_bytes
    );
    let expected_vector = vault.session_mut("note.md").unwrap().state_vector();
    vault.flush_document("note.md").unwrap();
    drop(delta);
    drop(vault);

    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let expected = format!("a{}", "abcdef".repeat(9_999));
    assert_eq!(render(&mut vault, EquivalenceMode::Structural), expected);
    assert_eq!(
        vault.session_mut("note.md").unwrap().state_vector(),
        expected_vector
    );
    vault
        .with_local_edit("note.md", |session| {
            session.insert_text(id, 1 + 9_999 * 6, "!").unwrap();
        })
        .unwrap();
    drop(vault);
    let mut vault = VaultSession::open(root.path()).unwrap();
    assert_eq!(
        render(&mut vault, EquivalenceMode::Structural),
        format!("{expected}!")
    );
}

#[test]
fn valid_fence_marker_content_ingests_exports_and_reopens() {
    for symbol in ["`", "~"] {
        let root = tempdir().unwrap();
        let body = format!("{} aaa\n{}\tcontent", symbol.repeat(3), symbol.repeat(4));
        let source = format!("{}\n{body}\n{}", symbol.repeat(3), symbol.repeat(3));
        fs::write(root.path().join("note.md"), &source).unwrap();
        let mut vault = VaultSession::open(root.path()).unwrap();
        vault.ingest_all().unwrap();
        let id = first_id(&mut vault);
        let block = vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .find_block_by_id(id)
            .unwrap();
        assert!(matches!(&block.kind, md_crdt::BlockKind::CodeFence { text, .. } if text == &body));
        assert_eq!(render(&mut vault, EquivalenceMode::Structural), source);
        assert_eq!(render(&mut vault, EquivalenceMode::Exact), source);
        vault.flush_document("note.md").unwrap();
        drop(vault);
        let mut vault = VaultSession::open(root.path()).unwrap();
        assert_eq!(first_id(&mut vault), id);
        assert_eq!(render(&mut vault, EquivalenceMode::Structural), source);
        assert_eq!(render(&mut vault, EquivalenceMode::Exact), source);
    }
}

#[test]
fn raised_pending_limit_refuses_before_vault_persistence_and_reopens_unchanged() {
    use md_crdt::codec::{DocOp, Envelope, JsonOpCodec, OpBody, OpCodec, WIRE_VERSION};
    use md_crdt::{ChangeMessage, OpId, Operation, SessionError, ValidationError};
    let root = tempdir().unwrap();
    fs::write(root.path().join("note.md"), "durable before rejection").unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let before = vault
        .session_mut("note.md")
        .unwrap()
        .save_snapshot()
        .unwrap();
    let ceiling = md_crdt::limits::MAX_DURABLE_PENDING_OPERATIONS;
    let foreign = vault.peer().wrapping_add(1).max(1);
    let ops = (2..ceiling as u64 + 3)
        .map(|counter| {
            let id = OpId {
                counter,
                peer: foreign,
            };
            let envelope = Envelope {
                version: WIRE_VERSION,
                body: OpBody::Doc(DocOp::DeleteBlock {
                    parent: None,
                    target: OpId {
                        counter: 1,
                        peer: foreign,
                    },
                    id,
                    observed: StateVector::new(),
                    subtree: Vec::new(),
                }),
            };
            Operation {
                id,
                payload: JsonOpCodec.encode(&envelope).unwrap().into(),
            }
        })
        .collect();
    let result = vault.apply_remote(
        "note.md",
        ChangeMessage {
            since: StateVector::new(),
            ops,
        },
        &ValidationLimits {
            max_ops_per_message: ceiling + 1,
            max_payload_bytes: md_crdt::limits::MAX_DURABLE_RETAINED_BYTES,
            max_pending_buffer: ceiling + 1,
            ..Default::default()
        },
    );
    assert!(matches!(
        result,
        Err(VaultError::Session(SessionError::Validation(ValidationError::BufferFull { capacity })))
            if capacity == ceiling
    ));
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .save_snapshot()
            .unwrap(),
        before
    );
    drop(vault);
    let mut reopened = VaultSession::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .session_mut("note.md")
            .unwrap()
            .save_snapshot()
            .unwrap(),
        before
    );
}
