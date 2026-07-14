use md_crdt::{
    BatchPreview, BatchReceipt, BlockDescriptor, BlockDescriptorKind, ChangeSummary,
    CheckpointRequest, ColumnAlignment, ColumnDef, DescriptorPage, DiskFingerprint,
    DocumentExportRequest, DocumentHandle, DocumentId, DocumentTombstonePolicy, EditBatch,
    MarkKind, MarkValue, PeerLease, PreviewToken, RebaseRequired, RecoveryReport, RevisionToken,
    StateVector, VaultId, WorkspaceEdit,
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
    let batch = EditBatch {
        document_id,
        expected_revision: revision.clone(),
        operations: vec![
            WorkspaceEdit::MoveSection {
                heading_id,
                after: None,
            },
            WorkspaceEdit::InsertTable {
                parent: None,
                after: Some(heading_id),
                columns: vec![ColumnDef {
                    alignment: ColumnAlignment::Left,
                }],
                header: vec!["Name".into()],
            },
            WorkspaceEdit::SetMark {
                block_id: paragraph_id,
                start: 0,
                end: 4,
                kind: MarkKind::Link,
                attrs,
            },
            WorkspaceEdit::SetFrontmatterField {
                key: "status".into(),
                value: Some("ready".into()),
            },
        ],
    };
    let mut acknowledged = StateVector::new();
    acknowledged.set(7, 9);

    json!({
        "contract_version": 1,
        "document_handle": DocumentHandle {
            vault_id,
            document_id,
            revision: revision.clone(),
            disk_fingerprint: Some(DiskFingerprint(6)),
        },
        "descriptor_page": DescriptorPage {
            items: vec![BlockDescriptor {
                id: heading_id,
                parent: None,
                order: 0,
                kind: BlockDescriptorKind::Heading,
                heading_level: Some(1),
                source_bytes: 8,
                text_bytes: 5,
                content_digest: 99,
            }],
            next_offset: Some(1),
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
    let frozen: Value = serde_json::from_str(include_str!("fixtures/workspace-contract-v1.json"))
        .expect("valid frozen workspace contract fixture");
    assert_eq!(frozen, contract_fixture());
}
