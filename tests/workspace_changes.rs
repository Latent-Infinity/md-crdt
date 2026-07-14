#![cfg(feature = "filesync")]

use md_crdt::filesync::VaultSession;
use md_crdt::{BlockDescriptorKind, MarkKind, ValidationLimits};
use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

#[test]
fn descriptor_pages_are_body_free_and_follow_the_document_hierarchy() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "# Scope\n\nalpha\n\n- one\n- two\n\n> nested\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();

    let first = vault.descriptor_page("note.md", None, 0, 2).unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(first.next_offset, Some(2));
    assert_eq!(first.items[0].kind, BlockDescriptorKind::Heading);
    assert_eq!(first.items[0].heading_level, Some(1));
    assert_eq!(first.items[0].text_bytes, "Scope".len());
    assert!(first.items[0].source_bytes >= first.items[0].text_bytes);
    assert_ne!(first.items[0].content_digest, 0);
    assert_eq!(first.items[1].kind, BlockDescriptorKind::Paragraph);

    let remainder = vault.descriptor_page("note.md", None, 2, 8).unwrap();
    assert_eq!(remainder.next_offset, None);
    let list = remainder
        .items
        .iter()
        .find(|item| item.kind == BlockDescriptorKind::List)
        .unwrap();
    let quote = remainder
        .items
        .iter()
        .find(|item| item.kind == BlockDescriptorKind::BlockQuote)
        .unwrap();

    let list_items = vault
        .descriptor_page("note.md", Some(list.id), 0, 8)
        .unwrap();
    assert_eq!(list_items.items.len(), 2);
    assert!(
        list_items
            .items
            .iter()
            .all(|item| item.kind == BlockDescriptorKind::ListItem && item.parent == Some(list.id))
    );
    let item_children = vault
        .descriptor_page("note.md", Some(list_items.items[0].id), 0, 8)
        .unwrap();
    assert_eq!(item_children.items.len(), 1);
    assert_eq!(item_children.items[0].parent, Some(list_items.items[0].id));

    let quote_children = vault
        .descriptor_page("note.md", Some(quote.id), 0, 8)
        .unwrap();
    assert_eq!(quote_children.items.len(), 1);
    assert_eq!(quote_children.items[0].parent, Some(quote.id));

    let json = serde_json::to_value(&first).unwrap();
    assert!(json.get("items").is_some());
    assert!(!json.to_string().contains("alpha"));
}

#[test]
fn local_edit_summary_is_bounded_and_identifies_the_owning_section() {
    let dir = tempdir().unwrap();
    let mut markdown = String::from("# Scope\n\n");
    for index in 0..100 {
        markdown.push_str(&format!("item {index}\n\n"));
    }
    fs::write(dir.path().join("note.md"), markdown).unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let opened = vault.open_document("note.md").unwrap();
    let descriptors = vault.descriptor_page("note.md", None, 0, 101).unwrap();
    let heading = descriptors.items[0].id;
    let target = descriptors.items[51].id;

    let outcome = vault
        .with_local_edit("note.md", |session| session.insert_text(target, 7, "!"))
        .unwrap();
    outcome.value.unwrap();

    assert!(outcome.changes.created.is_empty());
    assert!(outcome.changes.deleted.is_empty());
    assert!(outcome.changes.moved.is_empty());
    assert_eq!(outcome.changes.updated, vec![target]);
    assert_eq!(outcome.changes.affected_sections, vec![heading]);
    assert_eq!(outcome.changes.operation_count, 1);
    assert_ne!(outcome.changes.revision, opened.revision);
}

#[test]
fn remote_apply_returns_created_ids_and_post_revision() {
    let source_dir = tempdir().unwrap();
    let target_dir = tempdir().unwrap();
    fs::write(source_dir.path().join("note.md"), "remote\n").unwrap();
    fs::write(target_dir.path().join("note.md"), "").unwrap();
    let mut source = VaultSession::open(source_dir.path()).unwrap();
    let mut target = VaultSession::open(target_dir.path()).unwrap();
    source.open_document("note.md").unwrap();
    let before = target.open_document("note.md").unwrap();
    let expected_id = source.descriptor_page("note.md", None, 0, 1).unwrap().items[0].id;
    let changes = source
        .encode_changes_since("note.md", &target.state_vector("note.md").unwrap())
        .unwrap();

    let outcome = target
        .apply_remote("note.md", changes, &ValidationLimits::default())
        .unwrap();

    assert!(!outcome.applied.is_empty());
    assert_eq!(outcome.changes.created, vec![expected_id]);
    assert_eq!(outcome.changes.operation_count, outcome.applied.len());
    assert_ne!(outcome.changes.revision, before.revision);
    assert_eq!(
        outcome.changes.revision,
        target.revision("note.md").unwrap()
    );
}

#[test]
fn ingest_and_export_return_scoped_change_summaries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.md");
    fs::write(&path, "# Scope\n\nalpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let opened = vault.open_document("note.md").unwrap();
    let descriptors = vault.descriptor_page("note.md", None, 0, 3).unwrap();
    let heading = descriptors.items[0].id;
    let alpha = descriptors.items[1].id;

    fs::write(&path, "# Scope\n\nalpha!\n\nbeta\n").unwrap();
    let ingest = vault
        .refresh_markdown("note.md", Some(&opened.revision), None)
        .unwrap();
    assert!(ingest.changed);
    assert_eq!(ingest.changes.updated, vec![alpha]);
    assert_eq!(ingest.changes.affected_sections, vec![heading]);
    assert!(ingest.changes.operation_count > 0);

    let local = vault
        .with_local_edit("note.md", |session| session.insert_text(alpha, 6, " local"))
        .unwrap();
    local.value.unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let export = vault
        .export_markdown("note.md", &handle.revision, handle.disk_fingerprint)
        .unwrap();
    assert!(export.changed);
    assert_eq!(export.changes.operation_count, 0);
    assert_eq!(export.changes.revision, export.revision);
    assert_eq!(export.changes.updated, vec![alpha]);
    assert_eq!(export.changes.affected_sections, vec![heading]);
}

#[test]
fn insertions_do_not_report_shifted_siblings_as_moves() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "one\n\ntwo\n\nthree\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();

    let inserted = vault
        .with_local_edit("note.md", |session| session.insert_paragraph(None, "zero"))
        .unwrap();
    let inserted_id = md_crdt::block_id_from_op(inserted.value.unwrap());
    assert_eq!(inserted.changes.created, vec![inserted_id]);
    assert!(inserted.changes.moved.is_empty());

    let page = vault.descriptor_page("note.md", None, 0, 8).unwrap();
    let last = page.items.last().unwrap().id;
    let moved = vault
        .with_local_edit("note.md", |session| session.move_block(last, None, None))
        .unwrap();
    moved.value.unwrap();
    assert_eq!(moved.changes.moved, vec![last]);
    assert!(moved.changes.created.is_empty());
    assert!(moved.changes.deleted.is_empty());
}

#[test]
fn parent_moves_and_mark_updates_report_precise_metadata() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "plain\n\n> nested\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 8).unwrap();
    let paragraph = page.items[0].id;
    let quote = page.items[1].id;
    let quote_elem = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .block_elem_id(quote)
        .unwrap();

    let marked = vault
        .with_local_edit("note.md", |session| {
            session.set_mark(paragraph, 0..5, MarkKind::Bold, BTreeMap::new())
        })
        .unwrap();
    marked.value.unwrap();
    assert_eq!(marked.changes.updated, vec![paragraph]);

    let moved = vault
        .with_local_edit("note.md", |session| {
            session.move_block(paragraph, Some(quote_elem), None)
        })
        .unwrap();
    moved.value.unwrap();
    assert_eq!(moved.changes.moved, vec![paragraph]);
    assert_eq!(moved.changes.affected_parents, vec![quote]);
}

#[test]
fn descriptor_page_handles_zero_limit_and_unknown_parent() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "body\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();

    let empty = vault.descriptor_page("note.md", None, 0, 0).unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.next_offset, None);
    let missing = uuid::Uuid::from_u128(u128::MAX);
    assert!(matches!(
        vault.descriptor_page("note.md", Some(missing), 0, 10),
        Err(md_crdt::filesync::VaultError::DescriptorParentNotFound(id)) if id == missing
    ));
}

#[test]
fn local_edit_summary_reports_deleted_block_ids() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n\ngamma\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();
    let page = vault.descriptor_page("note.md", None, 0, 8).unwrap();
    let victim = page.items[1].id; // the "beta" paragraph

    let outcome = vault
        .with_local_edit("note.md", |session| {
            let elem = session
                .document()
                .block_elem_id(victim)
                .expect("target block exists");
            session.delete_block(elem)
        })
        .unwrap();
    outcome.value.unwrap();

    assert_eq!(outcome.changes.deleted, vec![victim]);
    assert!(outcome.changes.created.is_empty());
    assert!(outcome.changes.updated.is_empty());
    assert_eq!(outcome.changes.operation_count, 1);

    // The deleted block no longer appears in the descriptor outline.
    let after = vault.descriptor_page("note.md", None, 0, 8).unwrap();
    assert!(after.items.iter().all(|item| item.id != victim));
}
