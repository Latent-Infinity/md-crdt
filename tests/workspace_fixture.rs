use md_crdt::{
    BatchPreview, BatchReceipt, BlockDescriptor, BlockDescriptorKind, ChangeSummary,
    CheckpointRequest, ColumnAlignment, ColumnDef, DescriptorPage, DiskFingerprint,
    DocumentExportRequest, DocumentHandle, DocumentId, DocumentTombstonePolicy, EditBatch,
    MarkKind, MarkValue, PeerLease, PreviewToken, RebaseRequired, RecoveryReport, RevisionToken,
    StateVector, TargetPrecondition, TextPoint, TextPosition, TextRange, VaultId, WorkspaceEdit,
    WorkspaceMutation,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn contract_fixture() -> Value {
    let vault_id = VaultId::from_u128(1);
    let document_id = DocumentId::from_u128(2);
    let revision = RevisionToken::from_u128(3);
    let next_revision = RevisionToken::from_u128(4);
    let heading_id = uuid::Uuid::from_u128(10);
    let table_id = uuid::Uuid::from_u128(11);
    let paragraph_id = uuid::Uuid::from_u128(12);
    let token: PreviewToken =
        serde_json::from_value(json!([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5])).unwrap();
    let summary = ChangeSummary {
        created: vec![table_id],
        deleted: Vec::new(),
        moved: vec![heading_id],
        updated: vec![paragraph_id],
        affected_parents: Vec::new(),
        affected_sections: vec![heading_id],
        operation_count: 4,
        revision: next_revision.clone(),
    };
    let mut attrs = BTreeMap::new();
    attrs.insert("href".into(), MarkValue::String("#target".into()));
    let marked_range = TextRange {
        start: TextPoint {
            block_id: paragraph_id,
            position: TextPosition::Start,
        },
        end: TextPoint {
            block_id: paragraph_id,
            position: TextPosition::End,
        },
    };
    let batch = EditBatch {
        document_id,
        base_revision: revision.clone(),
        operations: vec![
            WorkspaceMutation::scoped(
                WorkspaceEdit::MoveSection {
                    heading_id,
                    after: None,
                },
                vec![TargetPrecondition::Block {
                    block_id: heading_id,
                    content_digest: 77,
                }],
            ),
            WorkspaceMutation::strict(WorkspaceEdit::InsertTable {
                parent: None,
                after: Some(heading_id),
                columns: vec![ColumnDef {
                    alignment: ColumnAlignment::Left,
                }],
                header: vec!["Name".into()],
            }),
            WorkspaceMutation::scoped(
                WorkspaceEdit::SetMark {
                    range: marked_range,
                    kind: MarkKind::Link,
                    attrs,
                },
                vec![TargetPrecondition::Text {
                    range: marked_range,
                    content_digest: 88,
                }],
            ),
            WorkspaceMutation::strict(WorkspaceEdit::SetFrontmatterField {
                key: "status".into(),
                value: Some("ready".into()),
            }),
        ],
    };
    let mut acknowledged = StateVector::new();
    acknowledged.set(7, 9);

    json!({
        "contract_version": 3,
        "document_handle": DocumentHandle {
            vault_id,
            document_id,
            revision: revision.clone(),
            disk_fingerprint: Some(DiskFingerprint(6)),
        },
        "descriptor_page": DescriptorPage {
            document_id,
            revision: revision.clone(),
            parent: None,
            traversal: md_crdt::DescriptorTraversal::DirectChildren,
            items: vec![BlockDescriptor {
                id: heading_id,
                parent: None,
                order: 0,
                kind: BlockDescriptorKind::Heading,
                heading_level: Some(1),
                source_bytes: 8,
                text_bytes: 5,
                node_digest: 99,
                direct_child_count: 0,
                descendant_count: 0,
                subtree_digest: None,
            }],
            next_cursor: None,
        },
        "change_summary": summary,
        "edit_batch": batch,
        "batch_preview": BatchPreview {
            document_id,
            revision: next_revision.clone(),
            token,
            changes: summary.clone(),
        },
        "batch_receipt": BatchReceipt {
            document_id,
            previous_revision: revision.clone(),
            revision: next_revision.clone(),
            changes: summary,
        },
        "export_request": DocumentExportRequest {
            path: "notes/example.md".into(),
            document_id,
            expected_revision: next_revision,
            expected_disk_fingerprint: Some(DiskFingerprint(6)),
        },
        "recovery_report": RecoveryReport {
            transactions_recovered: 1,
            files_recovered: 2,
        },
        "checkpoint_request": CheckpointRequest {
            max_retained_ops: 128,
            active_peer_leases: vec![PeerLease {
                peer: 42,
                acknowledged: acknowledged.clone(),
            }],
            tombstones: DocumentTombstonePolicy::KeepAll,
        },
        "rebase_required": RebaseRequired {
            checkpoint_epoch: 2,
            delta_floor: acknowledged,
        },
    })
}

#[test]
fn versioned_workspace_contract_fixture_matches_public_serialization() {
    let frozen: Value = serde_json::from_str(include_str!("fixtures/workspace-contract-v3.json"))
        .expect("valid frozen workspace contract fixture");
    assert_eq!(frozen, contract_fixture());
}
