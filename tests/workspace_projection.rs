#![cfg(feature = "filesync")]

use md_crdt::filesync::{VaultError, VaultSession};
use md_crdt::{
    BlockProjectionKind, BlockProjectionStructure, EditBatch, EquivalenceMode, MarkKind,
    ProjectionError, ProjectionFields, ProjectionRequest, TextPosition, WorkspaceEdit,
    WorkspaceMutation,
};
use std::fs;
use tempfile::tempdir;

fn request(
    handle: &md_crdt::DocumentHandle,
    block_ids: Vec<md_crdt::BlockId>,
    fields: ProjectionFields,
) -> ProjectionRequest {
    ProjectionRequest {
        document_id: handle.document_id,
        base_revision: handle.revision.clone(),
        block_ids,
        fields,
        max_items: 32,
        max_bytes: 64 * 1024,
        continuation: None,
    }
}

#[test]
fn semantic_projection_matches_authoritative_markdown_model() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "# Title\n\nalpha **bold** [link](#target)\n\n- one\n- two\n\n> quote\n\n| A | B |\n| :--- | ---: |\n| x | y |\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let descriptors = vault.descriptor_page("note.md", None, None, 8).unwrap();
    let ids: Vec<_> = descriptors.items.iter().map(|item| item.id).collect();

    let page = vault
        .project_blocks(
            "note.md",
            request(&handle, ids.clone(), ProjectionFields::SEMANTIC),
        )
        .unwrap();

    assert_eq!(page.document_id, handle.document_id);
    assert_eq!(page.revision, handle.revision);
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        ids
    );
    assert!(page.omitted_ids.is_empty());
    assert!(page.continuation.is_none());
    assert_eq!(page.bytes_used, serde_json::to_vec(&page).unwrap().len());
    assert!(page.bytes_used <= 64 * 1024);

    let heading = &page.items[0];
    assert_eq!(
        heading.kind,
        Some(BlockProjectionKind::Heading { level: 1 })
    );
    assert_eq!(heading.text.as_deref(), Some("Title"));
    assert_eq!(heading.text_ranges.as_ref().unwrap().len(), 1);

    let paragraph = &page.items[1];
    assert_eq!(paragraph.text.as_deref(), Some("alpha bold link"));
    let marks = paragraph.marks.as_ref().unwrap();
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0].kind, MarkKind::Bold);
    assert_eq!(marks[1].kind, MarkKind::Link);
    assert_eq!(
        marks[1].attrs.get("href"),
        Some(&md_crdt::MarkValue::String("#target".into()))
    );
    assert_eq!(
        vault
            .resolve_text_range("note.md", &marks[0].range)
            .unwrap(),
        6..10
    );
    assert!(matches!(
        paragraph.text_ranges.as_ref().unwrap()[0].start.position,
        TextPosition::Start
    ));

    let list = &page.items[2];
    assert_eq!(list.text.as_deref(), Some("one\ntwo"));
    assert!(matches!(
        list.structure,
        Some(BlockProjectionStructure::ListItems { ref item_ids }) if item_ids.len() == 2
    ));
    let quote = &page.items[3];
    assert_eq!(quote.text.as_deref(), Some("quote"));
    assert!(matches!(
        quote.structure,
        Some(BlockProjectionStructure::Children { ref block_ids }) if block_ids.len() == 1
    ));
    let table = &page.items[4];
    assert_eq!(table.text.as_deref(), Some("A\tB\nx\ty"));
    assert!(matches!(
        table.structure,
        Some(BlockProjectionStructure::Table { ref rows, .. }) if rows.len() == 1
    ));

    for (projection, descriptor) in page.items.iter().zip(&descriptors.items) {
        assert!(projection.content_digest.is_some());
        assert_ne!(descriptor.node_digest, 0);
    }

    let list_items = vault
        .descriptor_page("note.md", Some(ids[2]), None, 2)
        .unwrap();
    let item = vault
        .project_blocks(
            "note.md",
            request(
                &handle,
                vec![list_items.items[0].id],
                ProjectionFields::SEMANTIC,
            ),
        )
        .unwrap();
    assert_eq!(item.items[0].kind, Some(BlockProjectionKind::ListItem));
    assert_eq!(item.items[0].text.as_deref(), Some("one"));
    assert!(item.items[0].content_digest.is_some());
    assert_ne!(list_items.items[0].node_digest, 0);
    assert!(matches!(
        item.items[0].structure,
        Some(BlockProjectionStructure::Children { ref block_ids }) if block_ids.len() == 1
    ));

    let round_trip =
        serde_json::from_slice::<md_crdt::ProjectionPage>(&serde_json::to_vec(&page).unwrap())
            .unwrap();
    assert_eq!(round_trip, page);
}

#[test]
fn projection_masks_order_missing_duplicates_and_revision_fail_closed() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "alpha\n\nbeta\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let descriptors = vault.descriptor_page("note.md", None, None, 2).unwrap();
    let alpha = descriptors.items[0].id;
    let beta = descriptors.items[1].id;
    let missing = uuid::Uuid::from_u128(u128::MAX);

    let page = vault
        .project_blocks(
            "note.md",
            request(
                &handle,
                vec![beta, missing, alpha],
                ProjectionFields::KIND | ProjectionFields::CONTENT_DIGEST,
            ),
        )
        .unwrap();
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![beta, alpha]
    );
    assert_eq!(page.omitted_ids, vec![missing]);
    assert!(page.items.iter().all(|item| {
        item.kind.is_some()
            && item.content_digest.is_some()
            && item.text.is_none()
            && item.marks.is_none()
            && item.structure.is_none()
            && item.text_ranges.is_none()
            && item.exact.is_none()
    }));

    let duplicate = request(&handle, vec![alpha, alpha], ProjectionFields::MINIMAL);
    assert!(matches!(
        vault.project_blocks("note.md", duplicate),
        Err(VaultError::Projection(ProjectionError::DuplicateBlockId { block_id }))
            if block_id == alpha
    ));

    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(beta, 4, "!")
        .unwrap();
    assert!(matches!(
        vault.project_blocks(
            "note.md",
            request(&handle, vec![alpha], ProjectionFields::MINIMAL),
        ),
        Err(VaultError::StaleRevision { .. })
    ));

    let current = vault.open_document("note.md").unwrap();
    let mut wrong_document = request(&current, vec![alpha], ProjectionFields::MINIMAL);
    wrong_document.document_id = md_crdt::DocumentId::from_u128(u128::MAX);
    assert!(matches!(
        vault.project_blocks("note.md", wrong_document),
        Err(VaultError::DocumentIdMismatch { .. })
    ));
}

#[test]
fn continuation_and_serialized_byte_limits_are_hard() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "one\n\ntwo\n\nthree\n").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let ids: Vec<_> = vault
        .descriptor_page("note.md", None, None, 3)
        .unwrap()
        .items
        .iter()
        .map(|item| item.id)
        .collect();
    let mut projection_request = request(&handle, ids.clone(), ProjectionFields::SEMANTIC);
    projection_request.max_items = 1;
    projection_request.max_bytes = 1_024;

    let first = vault
        .project_blocks("note.md", projection_request.clone())
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(first.bytes_used <= projection_request.max_bytes);
    projection_request.continuation = first.continuation.clone();
    let encoded_request = serde_json::to_vec(&projection_request).unwrap();
    assert_eq!(
        serde_json::from_slice::<ProjectionRequest>(&encoded_request).unwrap(),
        projection_request
    );
    let second = vault
        .project_blocks("note.md", projection_request.clone())
        .unwrap();
    assert_eq!(second.items.len(), 1);
    projection_request.continuation = second.continuation.clone();
    let third = vault
        .project_blocks("note.md", projection_request.clone())
        .unwrap();
    assert_eq!(third.items.len(), 1);
    assert!(third.continuation.is_none());
    assert_eq!(
        first
            .items
            .iter()
            .chain(&second.items)
            .chain(&third.items)
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        ids
    );

    let mut changed_shape = projection_request.clone();
    changed_shape.continuation = first.continuation;
    changed_shape.fields = ProjectionFields::EXACT;
    assert!(matches!(
        vault.project_blocks("note.md", changed_shape),
        Err(VaultError::Projection(ProjectionError::InvalidContinuation))
    ));

    let mut too_small = request(&handle, vec![ids[0]], ProjectionFields::EXACT);
    too_small.max_bytes = 64;
    assert!(matches!(
        vault.project_blocks("note.md", too_small),
        Err(VaultError::Projection(
            ProjectionError::ItemTooLarge { block_id, .. }
                | ProjectionError::PageTooLarge { block_id: Some(block_id), .. }
        )) if block_id == ids[0]
    ));

    let full_item = vault
        .project_blocks(
            "note.md",
            request(&handle, vec![ids[0]], ProjectionFields::SEMANTIC),
        )
        .unwrap();
    let item_overflow = (1..full_item.bytes_used).rev().find_map(|max_bytes| {
        let mut candidate = request(&handle, vec![ids[0]], ProjectionFields::SEMANTIC);
        candidate.max_bytes = max_bytes;
        match vault.project_blocks("note.md", candidate) {
            Err(VaultError::Projection(error @ ProjectionError::ItemTooLarge { .. })) => {
                Some(error)
            }
            _ => None,
        }
    });
    assert!(matches!(
        item_overflow,
        Some(ProjectionError::ItemTooLarge { block_id, required_bytes, max_bytes })
            if block_id == ids[0] && required_bytes > max_bytes
    ));

    let full_page = vault
        .project_blocks(
            "note.md",
            request(&handle, ids.clone(), ProjectionFields::SEMANTIC),
        )
        .unwrap();
    let byte_page = (1..full_page.bytes_used).find_map(|max_bytes| {
        let mut candidate = request(&handle, ids.clone(), ProjectionFields::SEMANTIC);
        candidate.max_bytes = max_bytes;
        match vault.project_blocks("note.md", candidate) {
            Ok(page) if !page.items.is_empty() && page.items.len() < ids.len() => {
                Some((page, max_bytes))
            }
            _ => None,
        }
    });
    let (byte_page, max_bytes) = byte_page.expect("a byte limit fits a strict item prefix");
    assert!(byte_page.continuation.is_some());
    assert!(byte_page.bytes_used <= max_bytes);

    let mut invalid_limits = request(&handle, vec![ids[0]], ProjectionFields::MINIMAL);
    invalid_limits.max_items = 0;
    assert!(matches!(
        vault.project_blocks("note.md", invalid_limits),
        Err(VaultError::Projection(ProjectionError::InvalidLimits))
    ));

    let unknown_fields = serde_json::from_value(serde_json::json!(32768)).unwrap();
    assert!(matches!(
        vault.project_blocks("note.md", request(&handle, vec![ids[0]], unknown_fields),),
        Err(VaultError::Projection(ProjectionError::UnknownFields))
    ));
}

#[test]
fn code_and_raw_blocks_project_typed_kind_and_visible_content() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "```rust\nlet value = 1;\n```\n\n:::opaque\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let ids: Vec<_> = vault
        .descriptor_page("note.md", None, None, 2)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.id)
        .collect();
    let page = vault
        .project_blocks("note.md", request(&handle, ids, ProjectionFields::SEMANTIC))
        .unwrap();

    assert_eq!(
        page.items[0].kind,
        Some(BlockProjectionKind::CodeFence {
            info: Some("rust".into())
        })
    );
    assert_eq!(page.items[0].text.as_deref(), Some("let value = 1;"));
    assert_eq!(page.items[1].kind, Some(BlockProjectionKind::RawBlock));
    assert_eq!(page.items[1].text.as_deref(), Some(":::opaque"));
}

#[test]
fn exact_projection_uses_only_the_owned_source_region_before_and_after_edit() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "first  \r\n\r\nsecond *styled*\r\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let ids: Vec<_> = vault
        .descriptor_page("note.md", None, None, 2)
        .unwrap()
        .items
        .iter()
        .map(|item| item.id)
        .collect();
    let exact = vault
        .project_blocks(
            "note.md",
            request(&handle, vec![ids[1]], ProjectionFields::EXACT),
        )
        .unwrap();
    let selected = exact.items[0].exact.as_ref().unwrap();
    assert_eq!(selected.owner_block_id, ids[1]);
    assert_eq!(selected.markdown, "second *styled*");

    let edit = WorkspaceEdit::InsertText {
        at: vault.text_point("note.md", ids[1], 6).unwrap(),
        text: " changed".into(),
    };
    vault
        .apply_edit_batch(
            "note.md",
            EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision,
                operations: vec![WorkspaceMutation::strict(edit)],
            },
        )
        .unwrap();
    let current = vault.open_document("note.md").unwrap();
    let dirty = vault
        .project_blocks(
            "note.md",
            request(&current, vec![ids[1]], ProjectionFields::EXACT),
        )
        .unwrap();
    let selected = dirty.items[0].exact.as_ref().unwrap();
    assert_eq!(selected.owner_block_id, ids[1]);
    assert_eq!(selected.markdown, "second changed *styled*");
    assert!(!selected.markdown.contains("first"));
    assert_eq!(
        vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Structural),
        "first\n\nsecond changed *styled*"
    );
}

#[test]
fn nested_exact_projection_identifies_and_rerenders_only_its_source_owner() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("note.md"),
        "> first  \n>\n> second *styled*\n\noutside\n",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let handle = vault.open_document("note.md").unwrap();
    let roots = vault.descriptor_page("note.md", None, None, 2).unwrap();
    let quote_id = roots.items[0].id;
    let child_id = vault
        .descriptor_page("note.md", Some(quote_id), None, 2)
        .unwrap()
        .items[1]
        .id;

    let exact = vault
        .project_blocks(
            "note.md",
            request(&handle, vec![child_id], ProjectionFields::EXACT),
        )
        .unwrap();
    let selected = exact.items[0].exact.as_ref().unwrap();
    assert_eq!(selected.owner_block_id, quote_id);
    assert_eq!(selected.markdown, "> first  \n>\n> second *styled*");
    assert!(!selected.markdown.contains("outside"));

    vault
        .session_mut("note.md")
        .unwrap()
        .insert_text(child_id, 6, " changed")
        .unwrap();
    let current = vault.open_document("note.md").unwrap();
    let dirty = vault
        .project_blocks(
            "note.md",
            request(&current, vec![child_id], ProjectionFields::EXACT),
        )
        .unwrap();
    let selected = dirty.items[0].exact.as_ref().unwrap();
    assert_eq!(selected.owner_block_id, quote_id);
    assert_eq!(selected.markdown, "> first\n>\n> second changed *styled*");
    assert!(!selected.markdown.contains("outside"));
}
