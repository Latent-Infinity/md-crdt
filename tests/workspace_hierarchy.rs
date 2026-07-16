#![cfg(feature = "filesync")]

use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{BlockDescriptorKind, DescriptorError, MarkKind, MarkValue};
use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

#[test]
fn cursor_pages_are_gap_free_when_page_limits_change() {
    let dir = tempdir().unwrap();
    let markdown = (0..17)
        .map(|index| format!("item {index}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    fs::write(dir.path().join("note.md"), markdown).unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();

    let mut cursor = None;
    let mut ids = Vec::new();
    for limit in [1, 4, 2, 8, 3] {
        let page = vault
            .descriptor_page("note.md", None, cursor.as_ref(), limit)
            .unwrap();
        assert_eq!(page.document_id, handle.document_id);
        assert_eq!(page.revision, handle.revision);
        assert_eq!(page.parent, None);
        ids.extend(page.items.iter().map(|item| item.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(ids.len(), 17);
    let unique = ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), ids.len());
    let cursor = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .next_cursor
        .unwrap();
    assert!(std::mem::size_of_val(&cursor) <= 256);
    let encoded = serde_json::to_vec(&cursor).unwrap();
    assert!(encoded.len() <= 256);
    assert_eq!(
        serde_json::from_slice::<md_crdt::DescriptorCursor>(&encoded).unwrap(),
        cursor
    );
}

#[test]
fn cursors_fail_closed_for_wrong_scope_revision_and_encoding() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "a\n\nb\n\nc\n").unwrap();
    fs::write(dir.path().join("b.md"), "x\n\ny\n\nz\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("a.md").unwrap();
    vault.open_document("b.md").unwrap();

    let first = vault.descriptor_page("a.md", None, None, 1).unwrap();
    let cursor = first.next_cursor.unwrap();
    assert!(matches!(
        vault.descriptor_page("b.md", None, Some(&cursor), 1),
        Err(VaultError::Descriptor(
            DescriptorError::CursorDocumentMismatch { .. }
        ))
    ));
    assert!(matches!(
        vault.descriptor_page("a.md", Some(first.items[0].id), Some(&cursor), 1),
        Err(VaultError::Descriptor(
            DescriptorError::CursorParentMismatch { .. }
        ))
    ));

    vault
        .with_local_edit("a.md", |session| session.insert_paragraph(None, "new"))
        .unwrap()
        .value
        .unwrap();
    assert!(matches!(
        vault.descriptor_page("a.md", None, Some(&cursor), 1),
        Err(VaultError::Descriptor(
            DescriptorError::CursorRevisionMismatch { .. }
        ))
    ));

    let mut encoded = serde_json::to_value(&cursor)
        .unwrap()
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    let last = encoded.last_mut().unwrap();
    *last = if *last == b'0' { b'1' } else { b'0' };
    let corrupt = serde_json::from_value(serde_json::Value::String(
        String::from_utf8(encoded).unwrap(),
    ))
    .unwrap();
    assert!(matches!(
        vault.descriptor_page("a.md", None, Some(&corrupt), 1),
        Err(VaultError::Descriptor(DescriptorError::CorruptCursor))
    ));
    assert!(matches!(
        vault.descriptor_page("a.md", None, None, 0),
        Err(VaultError::Descriptor(DescriptorError::InvalidLimit))
    ));
}

#[test]
fn insert_delete_and_move_between_pages_require_a_fresh_traversal() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "a\n\nb\n\nc\n\nd\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("note.md").unwrap();

    let inserted_cursor = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .next_cursor
        .unwrap();
    vault
        .with_local_edit("note.md", |session| session.insert_paragraph(None, "new"))
        .unwrap()
        .value
        .unwrap();
    assert!(matches!(
        vault.descriptor_page("note.md", None, Some(&inserted_cursor), 1),
        Err(VaultError::Descriptor(
            DescriptorError::CursorRevisionMismatch { .. }
        ))
    ));

    let page = vault.descriptor_page("note.md", None, None, 8).unwrap();
    let deleted_cursor = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .next_cursor
        .unwrap();
    let victim = page.items[1].id;
    vault
        .with_local_edit("note.md", |session| {
            let elem = session.document().block_elem_id(victim).unwrap();
            session.delete_block(elem)
        })
        .unwrap()
        .value
        .unwrap();
    assert!(matches!(
        vault.descriptor_page("note.md", None, Some(&deleted_cursor), 1),
        Err(VaultError::Descriptor(
            DescriptorError::CursorRevisionMismatch { .. }
        ))
    ));

    let page = vault.descriptor_page("note.md", None, None, 8).unwrap();
    let moved_cursor = vault
        .descriptor_page("note.md", None, None, 1)
        .unwrap()
        .next_cursor
        .unwrap();
    let moved = page.items.last().unwrap().id;
    vault
        .with_local_edit("note.md", |session| session.move_block(moved, None, None))
        .unwrap()
        .value
        .unwrap();
    assert!(matches!(
        vault.descriptor_page("note.md", None, Some(&moved_cursor), 1),
        Err(VaultError::Descriptor(
            DescriptorError::CursorRevisionMismatch { .. }
        ))
    ));

    let expected: Vec<_> = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()
        .iter()
        .map(|block| block.id)
        .collect();
    let mut actual = Vec::new();
    let mut cursor = None;
    loop {
        let page = vault
            .descriptor_page("note.md", None, cursor.as_ref(), 2)
            .unwrap();
        actual.extend(page.items.iter().map(|item| item.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn descriptors_report_node_semantics_and_hierarchy_counts() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("asterisk.md"),
        "- one\n  - nested\n\n> alpha beta\n",
    )
    .unwrap();
    fs::write(dir.path().join("underscore.md"), "> alpha beta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.open_document("asterisk.md").unwrap();
    vault.open_document("underscore.md").unwrap();

    let roots = vault.descriptor_page("asterisk.md", None, None, 8).unwrap();
    let list = roots
        .items
        .iter()
        .find(|item| item.kind == BlockDescriptorKind::List)
        .unwrap();
    assert_eq!(list.direct_child_count, 1);
    assert!(list.descendant_count >= 2);
    assert_eq!(list.subtree_digest, None);

    let quote = roots
        .items
        .iter()
        .find(|item| item.kind == BlockDescriptorKind::BlockQuote)
        .unwrap();
    assert_eq!(quote.direct_child_count, 1);
    assert_eq!(quote.descendant_count, 1);
    assert_ne!(quote.node_digest, 0);

    let asterisk_child_id = vault
        .descriptor_page("asterisk.md", Some(quote.id), None, 1)
        .unwrap()
        .items[0]
        .id;
    let underscore_quote = vault
        .descriptor_page("underscore.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let underscore_child_id = vault
        .descriptor_page("underscore.md", Some(underscore_quote), None, 1)
        .unwrap()
        .items[0]
        .id;
    for (path, block_id, delimiter) in [
        ("asterisk.md", asterisk_child_id, "*"),
        ("underscore.md", underscore_child_id, "_"),
    ] {
        let attrs = BTreeMap::from([(
            "delimiter".to_string(),
            MarkValue::String(delimiter.to_string()),
        )]);
        vault
            .with_local_edit(path, |session| {
                session.set_mark(block_id, 6..10, MarkKind::Italic, attrs)
            })
            .unwrap()
            .value
            .unwrap();
    }
    let asterisk_child = vault
        .descriptor_page("asterisk.md", Some(quote.id), None, 1)
        .unwrap()
        .items[0]
        .node_digest;
    let underscore_child = vault
        .descriptor_page("underscore.md", Some(underscore_quote), None, 1)
        .unwrap()
        .items[0]
        .node_digest;
    assert_eq!(asterisk_child, underscore_child);
}
