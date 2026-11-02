//! Structure-only vault ingest (hash gate + match_blocks → block ops).

#![cfg(feature = "filesync")]

use md_crdt::doc::{BlockKind, EquivalenceMode, paragraph_visible_string};
use md_crdt::filesync::VaultSession;
use std::fs;
use tempfile::tempdir;

#[test]
fn ingest_empty_session_from_file_inserts_paragraphs() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "hello\n\nworld").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert_eq!(report.files_noop, 0);
    assert!(report.ops_emitted >= 2); // at least two structure commits

    let text = vs
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert_eq!(text, "hello\n\nworld");
}

#[test]
fn ingest_preserves_blockquote_structure() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("q.md"), "> quoted line").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert_eq!(report.files_skipped, 0);

    // The blockquote must be preserved as nested structure, not flattened.
    let session = vs.session_mut("q.md").unwrap();
    let top = &session.document().blocks_in_order()[0];
    match &top.kind {
        BlockKind::BlockQuote { children } => {
            let kids: Vec<_> = children.iter().collect();
            assert_eq!(kids.len(), 1, "one nested paragraph");
            match &kids[0].kind {
                BlockKind::Paragraph { text } => {
                    assert_eq!(paragraph_visible_string(text), "quoted line");
                }
                _ => panic!("expected nested paragraph"),
            }
        }
        _ => panic!("blockquote flattened — structure lost"),
    }
}

#[test]
fn ingest_preserves_list_structure_and_text() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("l.md"), "- alpha\n- beta").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert_eq!(report.files_skipped, 0);

    let doc = vs.session_mut("l.md").unwrap().document();
    let top = &doc.blocks_in_order()[0];
    match &top.kind {
        BlockKind::List { style, items, .. } => {
            assert!(!style.ordered);
            let its: Vec<_> = items.iter().collect();
            assert_eq!(its.len(), 2, "two list items");
            let text = |it: &md_crdt::doc::ListItem| {
                it.children.iter().next().map(|b| match &b.kind {
                    BlockKind::Paragraph { text } => paragraph_visible_string(text),
                    _ => String::new(),
                })
            };
            assert_eq!(
                text(its[0]).as_deref(),
                Some("alpha"),
                "item text preserved"
            );
            assert_eq!(text(its[1]).as_deref(), Some("beta"));
        }
        _ => panic!("list flattened / item text lost"),
    }

    // Idempotent: re-ingest of the unchanged file is a NoOp.
    let r2 = vs.ingest_all().unwrap();
    assert_eq!(r2.files_noop, 1);
    assert_eq!(r2.ops_emitted, 0);
}

#[test]
fn reingest_add_list_item_preserves_list_and_existing_item_id() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("l.md");
    fs::write(&path, "- alpha\n").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    let (list_id, first_item_id) = {
        let top = &vs.session_mut("l.md").unwrap().document().blocks_in_order()[0];
        let BlockKind::List { items, .. } = &top.kind else {
            panic!("expected list");
        };
        (top.id, items.iter().next().unwrap().id)
    };

    fs::write(&path, "- alpha\n- beta\n").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert!(report.ops_emitted > 0);

    let top = &vs.session_mut("l.md").unwrap().document().blocks_in_order()[0];
    assert_eq!(top.id, list_id);
    let BlockKind::List { items, .. } = &top.kind else {
        panic!("expected list");
    };
    let items: Vec<_> = items.iter().collect();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, first_item_id);
    let text = |item: &md_crdt::doc::ListItem| {
        item.children.iter().next().map(|block| match &block.kind {
            BlockKind::Paragraph { text } => paragraph_visible_string(text),
            _ => String::new(),
        })
    };
    assert_eq!(text(items[0]).as_deref(), Some("alpha"));
    assert_eq!(text(items[1]).as_deref(), Some("beta"));
}

#[test]
fn table_ingest_preserves_column_row_and_unrelated_prose_ids() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("table.md"),
        "before\n\n| name | value |\n| --- | ---: |\n| a | 1 |\n| b | 2 |\n\nafter",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let first = vault.ingest_all().unwrap();
    assert_eq!(first.files_skipped, 0);
    let (prose_ids, table_id, column_ids, row_ids) = {
        let blocks = vault
            .session_mut("table.md")
            .unwrap()
            .document()
            .blocks_in_order();
        let BlockKind::Table { table } = &blocks[1].kind else {
            panic!("table expected")
        };
        (
            (blocks[0].id, blocks[2].id),
            blocks[1].id,
            table
                .columns_in_order()
                .into_iter()
                .map(|column| column.id)
                .collect::<Vec<_>>(),
            table.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        )
    };

    fs::write(
        dir.path().join("table.md"),
        "before\n\n| name | value |\n| :--- | ---: |\n| b | 22 |\n| a | 1 |\n\nafter",
    )
    .unwrap();
    let changed = vault.ingest_all().unwrap();
    assert_eq!(changed.files_changed, 1);
    let doc = vault.session_mut("table.md").unwrap().document();
    let blocks = doc.blocks_in_order();
    assert_eq!((blocks[0].id, blocks[2].id), prose_ids);
    assert_eq!(blocks[1].id, table_id);
    let BlockKind::Table { table } = &blocks[1].kind else {
        panic!("table expected")
    };
    assert_eq!(
        table
            .columns_in_order()
            .into_iter()
            .map(|column| column.id)
            .collect::<Vec<_>>(),
        column_ids
    );
    assert_eq!(
        table
            .rows
            .iter()
            .map(|row| row.id)
            .collect::<std::collections::HashSet<_>>(),
        row_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
    );
    assert_eq!(
        table
            .rows
            .iter()
            .map(|row| table.row_cells(row.id))
            .collect::<Vec<_>>(),
        vec![
            vec![String::from("b"), String::from("22")],
            vec![String::from("a"), String::from("1")],
        ]
    );
}

#[test]
fn external_semantic_replacement_projects_existing_mark_over_unicode() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("mark.md"), "**a🇺🇸b**").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    fs::write(dir.path().join("mark.md"), "a🇺🇸xb").unwrap();
    vault.ingest_all().unwrap();
    let doc = vault.session_mut("mark.md").unwrap().document();
    let block = doc.blocks_in_order()[0];
    let spans = doc.render_paragraph_spans(block.id).unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].start, spans[0].end), (0, 4));
    assert_eq!(doc.serialize(EquivalenceMode::Structural), "**a🇺🇸xb**");
}

#[test]
fn whole_external_replacement_drops_unjustified_mark() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("mark.md"), "**old**").unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    fs::write(dir.path().join("mark.md"), "new").unwrap();
    vault.ingest_all().unwrap();
    let doc = vault.session_mut("mark.md").unwrap().document();
    assert_eq!(doc.serialize(EquivalenceMode::Structural), "new");
    assert!(
        doc.blocks_in_order()[0]
            .marks
            .iter_active_intervals()
            .next()
            .is_none()
    );
}

#[test]
fn parsed_frontmatter_ingests_as_collaborative_lossless_state() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("frontmatter.md"),
        "---\n# note\ntitle: 'old'\n---\n\nbody",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    assert_eq!(
        vault
            .session_mut("frontmatter.md")
            .unwrap()
            .document()
            .frontmatter_field("title"),
        Some("'old'")
    );
    fs::write(
        dir.path().join("frontmatter.md"),
        "---\n# note\ntitle: 'new'\n---\n\nbody",
    )
    .unwrap();
    vault.ingest_all().unwrap();
    let doc = vault.session_mut("frontmatter.md").unwrap().document();
    assert_eq!(doc.frontmatter_field("title"), Some("'new'"));
    assert!(doc.serialize(EquivalenceMode::Exact).contains("# note\n"));
}

#[test]
fn initialized_frontmatter_preserves_adopted_crlf_source_through_refresh_and_reopen() {
    for opaque in [false, true] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("frontmatter.md");
        let nested = if opaque {
            "parent:\r\n  child: value\r\n"
        } else {
            ""
        };
        let original = format!(
            "---\r\n# keep\r\ntitle: 'old' # title comment\r\n{nested}untouched: yes # untouched comment\r\n---\r\n\r\nbody\r\n"
        );
        fs::write(&path, &original).unwrap();
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.ingest_all().unwrap();
        let initialized = vault.session_mut("frontmatter.md").unwrap().state_vector();
        assert_eq!(
            vault
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Exact),
            original
        );
        vault.ingest_all().unwrap();
        assert_eq!(
            vault.session_mut("frontmatter.md").unwrap().state_vector(),
            initialized
        );
        assert_eq!(
            vault
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Exact),
            original
        );
        drop(vault);
        let mut vault = VaultSession::open(dir.path()).unwrap();
        assert_eq!(
            vault
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Exact),
            original
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        if opaque {
            continue;
        }

        // An external field edit is adopted as the next exact source baseline.
        let external = original.replace("'old'", "'external'");
        fs::write(&path, &external).unwrap();
        vault.ingest_all().unwrap();
        assert_eq!(
            vault
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Exact),
            external
        );
        let refreshed = vault.session_mut("frontmatter.md").unwrap().state_vector();
        vault.ingest_all().unwrap();
        assert_eq!(
            vault.session_mut("frontmatter.md").unwrap().state_vector(),
            refreshed
        );
        assert_eq!(
            vault
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Exact),
            external
        );

        // A later explicit write must invalidate that source without resetting
        // base/register versions, while retaining comments and CRLF formatting.
        vault
            .session_mut("frontmatter.md")
            .unwrap()
            .set_frontmatter_field("title", Some("'local'".into()))
            .unwrap();
        let expected = original.replace("'old'", "'local'");
        assert_eq!(
            vault
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Exact),
            expected
        );
        vault.flush_document("frontmatter.md").unwrap();
        drop(vault);
        let mut reopened = VaultSession::open(dir.path()).unwrap();
        assert_eq!(
            reopened
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .serialize(EquivalenceMode::Exact),
            expected
        );
        assert_eq!(
            reopened
                .session_mut("frontmatter.md")
                .unwrap()
                .document()
                .frontmatter_field("title"),
            Some("'local'")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
    }
}

#[test]
fn second_ingest_is_noop_when_file_unchanged() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "stable").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let r1 = vs.ingest_all().unwrap();
    assert_eq!(r1.files_changed, 1);

    let r2 = vs.ingest_all().unwrap();
    assert_eq!(r2.files_noop, 1);
    assert_eq!(r2.files_changed, 0);
    assert_eq!(r2.ops_emitted, 0);
}

#[test]
fn comment_only_frontmatter_refresh_remains_exact_after_later_field_writes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("comments.md");
    let original =
        "---\r\n# old header\r\ntitle: ready # old inline\r\nuntouched: yes\r\n---\r\n\r\nbody\r\n";
    fs::write(&path, original).unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    let before = vault.session_mut("comments.md").unwrap().state_vector();
    let refreshed = original
        .replace("old header", "new header")
        .replace("old inline", "new inline");
    fs::write(&path, &refreshed).unwrap();
    vault.ingest_all().unwrap();
    assert_eq!(
        vault.session_mut("comments.md").unwrap().state_vector(),
        before
    );
    assert_eq!(
        vault
            .session_mut("comments.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Exact),
        refreshed
    );
    vault.ingest_all().unwrap();
    drop(vault);
    let mut vault = VaultSession::open(dir.path()).unwrap();
    assert_eq!(
        vault
            .session_mut("comments.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Exact),
        refreshed
    );
    vault
        .session_mut("comments.md")
        .unwrap()
        .set_frontmatter_field("title", Some("edited".into()))
        .unwrap();
    let expected = refreshed.replace("title: ready", "title: edited");
    assert_eq!(
        vault
            .session_mut("comments.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Exact),
        expected
    );
    vault.flush_document("comments.md").unwrap();
    drop(vault);
    let mut vault = VaultSession::open(dir.path()).unwrap();
    assert_eq!(
        vault
            .session_mut("comments.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Exact),
        expected
    );
}

#[test]
fn external_supported_and_opaque_frontmatter_transitions_remain_source_only() {
    for initially_opaque in [false, true] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("transition.md");
        let structured = "---\r\n# structured\r\ntitle: ready\r\n---\r\n\r\nbody\r\n";
        let opaque =
            "---\r\n# opaque\r\ntitle: ready\r\nparent:\r\n  child: value\r\n---\r\n\r\nbody\r\n";
        let (original, changed) = if initially_opaque {
            (opaque, structured)
        } else {
            (structured, opaque)
        };
        fs::write(&path, original).unwrap();
        let mut vault = VaultSession::open(dir.path()).unwrap();
        vault.ingest_all().unwrap();
        let vector = vault.session_mut("transition.md").unwrap().state_vector();
        for source in [changed, original] {
            fs::write(&path, source).unwrap();
            vault.ingest_all().unwrap();
            assert_eq!(
                vault.session_mut("transition.md").unwrap().state_vector(),
                vector
            );
            assert_eq!(
                vault
                    .session_mut("transition.md")
                    .unwrap()
                    .document()
                    .serialize(EquivalenceMode::Exact),
                source
            );
            // Source adoption must not silently replace the replicated initializer
            // or enable unsupported mutation of its opaque YAML.
            assert_eq!(
                vault
                    .session_mut("transition.md")
                    .unwrap()
                    .document()
                    .frontmatter
                    .as_ref()
                    .unwrap()
                    .is_structured(),
                !initially_opaque
            );
            vault.ingest_all().unwrap();
            drop(vault);
            vault = VaultSession::open(dir.path()).unwrap();
            assert_eq!(
                vault
                    .session_mut("transition.md")
                    .unwrap()
                    .document()
                    .serialize(EquivalenceMode::Exact),
                source
            );
        }
    }
}

#[test]
fn adopted_opaque_source_rejects_local_mutation_and_retains_remote_writes() {
    use md_crdt::doc::{Frontmatter, FrontmatterError};
    use md_crdt::session::{CollaborativeDocument, SessionError};
    use md_crdt::{StateVector, ValidationLimits};
    let dir = tempdir().unwrap();
    let path = dir.path().join("opaque.md");
    let structured = "---\r\n# original\r\ntitle: ready\r\n---\r\n\r\nbody\r\n";
    let opaque = "---\r\n# opaque adopted\r\ntitle: ready\r\nparent:\r\n  child: retained\r\n---\r\n\r\nbody\r\n";
    fs::write(&path, structured).unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    vault.ingest_all().unwrap();
    fs::write(&path, opaque).unwrap();
    vault.ingest_all().unwrap();
    let session = vault.session_mut("opaque.md").unwrap();
    let before = session.save_snapshot().unwrap();
    assert!(matches!(
        session.set_frontmatter_field("title", Some("not allowed".into())),
        Err(SessionError::Frontmatter(FrontmatterError::Opaque))
    ));
    assert_eq!(session.save_snapshot().unwrap(), before);
    assert!(
        session
            .document()
            .frontmatter
            .as_ref()
            .unwrap()
            .is_structured()
    );
    let writer_peer = if session.peer() == 42 { 43 } else { 42 };
    let mut writer = CollaborativeDocument::new(writer_peer);
    writer
        .set_frontmatter_field("title", Some("remote hidden".into()))
        .unwrap();
    session
        .apply_remote(
            writer.encode_changes_since(&StateVector::new()).unwrap(),
            &ValidationLimits::default(),
        )
        .unwrap();
    assert_eq!(session.document().serialize(EquivalenceMode::Exact), opaque);
    // The global supported register does receive the write; only the source-local
    // Exact view is opaque/read-only. This is not a replicated base transition.
    assert_eq!(
        session.document().frontmatter_field("title"),
        Some("remote hidden")
    );
    vault.flush_document("opaque.md").unwrap();
    drop(vault);
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let session = vault.session_mut("opaque.md").unwrap();
    assert_eq!(session.document().serialize(EquivalenceMode::Exact), opaque);
    assert_eq!(
        session.document().frontmatter_field("title"),
        Some("remote hidden")
    );
    let before = session.save_snapshot().unwrap();
    assert!(matches!(
        session.set_frontmatter_field("title", Some("still not allowed".into())),
        Err(SessionError::Frontmatter(FrontmatterError::Opaque))
    ));
    assert_eq!(session.save_snapshot().unwrap(), before);

    // A new winning initializer invalidates only the source baseline, retaining
    // the independently received explicit title write.
    let mut fork = CollaborativeDocument::restore_from_snapshot(before).unwrap();
    let init_peer = if writer_peer == 44 || session.peer() == 44 {
        45
    } else {
        44
    };
    let mut initializer = CollaborativeDocument::new(init_peer);
    for _ in 0..16 {
        initializer.set_frontmatter_field("advance", None).unwrap();
    }
    initializer
        .initialize_frontmatter(Frontmatter::parse("# new base\ntitle: seed".into()))
        .unwrap();
    fork.apply_remote(
        initializer
            .encode_changes_since(&StateVector::new())
            .unwrap(),
        &ValidationLimits::default(),
    )
    .unwrap();
    assert_eq!(
        fork.document().frontmatter_field("title"),
        Some("remote hidden")
    );
    assert!(
        fork.document()
            .serialize(EquivalenceMode::Exact)
            .contains("# new base")
    );
    assert!(
        !fork
            .document()
            .serialize(EquivalenceMode::Exact)
            .contains("child: retained")
    );

    // A genuinely parsed structured refresh may pass the internal ingress-only
    // validation path even though the previous source baseline was opaque.
    let adopted = "---\r\n# supported again\r\ntitle: external supported\r\n---\r\n\r\nbody\r\n";
    fs::write(&path, adopted).unwrap();
    vault.ingest_all().unwrap();
    let session = vault.session_mut("opaque.md").unwrap();
    assert_eq!(
        session.document().serialize(EquivalenceMode::Exact),
        adopted
    );
    session
        .set_frontmatter_field("title", Some("local supported".into()))
        .unwrap();
    let expected = adopted.replace("external supported", "local supported");
    assert_eq!(
        session.document().serialize(EquivalenceMode::Exact),
        expected
    );
    vault.flush_document("opaque.md").unwrap();
    drop(vault);
    let mut vault = VaultSession::open(dir.path()).unwrap();
    assert_eq!(
        vault
            .session_mut("opaque.md")
            .unwrap()
            .document()
            .serialize(EquivalenceMode::Exact),
        expected
    );
}

#[test]
fn ingest_add_paragraph_preserves_existing_block_id() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "first").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    let first_id = {
        let doc = vs.session_mut("doc.md").unwrap().document();
        doc.blocks_in_order()[0].id
    };

    fs::write(dir.path().join("doc.md"), "first\n\nsecond").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert!(report.ops_emitted >= 1);

    let doc = vs.session_mut("doc.md").unwrap().document();
    let blocks = doc.blocks_in_order();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].id, first_id, "matched block keeps BlockId");
    assert_eq!(
        doc.serialize(EquivalenceMode::Structural),
        "first\n\nsecond"
    );
}

#[test]
fn ingest_remove_paragraph_deletes_block() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "keep\n\ngone").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    assert_eq!(
        vs.session_mut("doc.md")
            .unwrap()
            .document()
            .blocks_in_order()
            .len(),
        2
    );

    fs::write(dir.path().join("doc.md"), "keep").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);

    let doc = vs.session_mut("doc.md").unwrap().document();
    assert_eq!(doc.blocks_in_order().len(), 1);
    match &doc.blocks_in_order()[0].kind {
        BlockKind::Paragraph { text } => {
            assert_eq!(paragraph_visible_string(text), "keep");
        }
        _ => panic!("expected paragraph"),
    }
}

#[test]
fn reorder_preserves_block_ids_via_match() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "alpha\n\nbeta").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    let (id_a, id_b) = {
        let blocks = vs
            .session_mut("doc.md")
            .unwrap()
            .document()
            .blocks_in_order();
        (blocks[0].id, blocks[1].id)
    };

    // Swap order in the file; fingerprints still match both paragraphs.
    fs::write(dir.path().join("doc.md"), "beta\n\nalpha").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    // Structure-only: pure reorder emits no add/remove when both match.
    // (CRDT order may stay alpha-then-beta; ids must both still exist.)
    let blocks = vs
        .session_mut("doc.md")
        .unwrap()
        .document()
        .blocks_in_order();
    let ids: Vec<_> = blocks.iter().map(|b| b.id).collect();
    assert!(ids.contains(&id_a));
    assert!(ids.contains(&id_b));
    assert_eq!(ids.len(), 2);
}

#[test]
fn multi_file_ingest_report_counts() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), "A").unwrap();
    fs::write(dir.path().join("b.md"), "B").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let r = vs.ingest_all().unwrap();
    assert_eq!(r.files_changed, 2);
    assert_eq!(r.files_noop, 0);

    let r2 = vs.ingest_all().unwrap();
    assert_eq!(r2.files_noop, 2);
}

#[test]
fn session_snapshot_survives_reopen_after_ingest() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.md"), "persisted body").unwrap();

    {
        let mut vs = VaultSession::open(dir.path()).unwrap();
        vs.ingest_all().unwrap();
    }

    let mut vs = VaultSession::open(dir.path()).unwrap();
    let text = vs
        .session_mut("note.md")
        .unwrap()
        .document()
        .serialize(EquivalenceMode::Structural);
    assert_eq!(text, "persisted body");
}

#[test]
fn reingest_unchanged_quote_is_noop() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("q.md"), "> quoted line").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    assert_eq!(vs.ingest_all().unwrap().files_changed, 1);

    let r2 = vs.ingest_all().unwrap();
    assert_eq!(r2.files_noop, 1);
    assert_eq!(r2.files_changed, 0);
    assert_eq!(r2.files_skipped, 0);
    assert_eq!(r2.ops_emitted, 0);
}

#[test]
fn reingest_add_paragraph_inside_quote_preserves_quote_and_sibling_ids() {
    let dir = tempdir().unwrap();
    // One blockquote with a single paragraph.
    fs::write(dir.path().join("q.md"), "> first").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();

    let (quote_id, child_id) = {
        let top = &vs.session_mut("q.md").unwrap().document().blocks_in_order()[0];
        match &top.kind {
            BlockKind::BlockQuote { children } => {
                let kids: Vec<_> = children.iter().collect();
                (top.id, kids[0].id)
            }
            _ => panic!("expected blockquote"),
        }
    };

    // Same quote, second paragraph added (CommonMark merges consecutive > lines).
    fs::write(dir.path().join("q.md"), "> first\n>\n> second").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert_eq!(report.files_skipped, 0);
    assert!(report.ops_emitted >= 1);

    let top = &vs.session_mut("q.md").unwrap().document().blocks_in_order()[0];
    assert_eq!(top.id, quote_id, "quote container id preserved");
    match &top.kind {
        BlockKind::BlockQuote { children } => {
            let kids: Vec<_> = children.iter().collect();
            assert_eq!(kids.len(), 2);
            assert_eq!(
                kids[0].id, child_id,
                "matched nested paragraph id preserved"
            );
            match &kids[1].kind {
                BlockKind::Paragraph { text } => {
                    assert_eq!(paragraph_visible_string(text), "second");
                }
                _ => panic!("expected second nested paragraph"),
            }
        }
        _ => panic!("expected blockquote"),
    }
}

#[test]
fn reingest_remove_paragraph_inside_quote() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("q.md"), "> keep\n>\n> gone").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();

    let quote_id = vs.session_mut("q.md").unwrap().document().blocks_in_order()[0].id;

    fs::write(dir.path().join("q.md"), "> keep").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert_eq!(report.files_skipped, 0);

    let top = &vs.session_mut("q.md").unwrap().document().blocks_in_order()[0];
    assert_eq!(top.id, quote_id);
    match &top.kind {
        BlockKind::BlockQuote { children } => {
            let kids: Vec<_> = children.iter().collect();
            assert_eq!(kids.len(), 1);
            match &kids[0].kind {
                BlockKind::Paragraph { text } => {
                    assert_eq!(paragraph_visible_string(text), "keep");
                }
                _ => panic!("expected nested paragraph"),
            }
        }
        _ => panic!("expected blockquote"),
    }
}

#[test]
fn reingest_text_change_inside_quote_keeps_quote_id() {
    // Structure-only: changed leaf text rematches as remove+add; quote container stays.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("q.md"), "> alpha").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    let quote_id = vs.session_mut("q.md").unwrap().document().blocks_in_order()[0].id;

    fs::write(dir.path().join("q.md"), "> beta").unwrap();
    vs.ingest_all().unwrap();

    let top = &vs.session_mut("q.md").unwrap().document().blocks_in_order()[0];
    assert_eq!(top.id, quote_id);
    match &top.kind {
        BlockKind::BlockQuote { children } => {
            let kids: Vec<_> = children.iter().collect();
            assert_eq!(kids.len(), 1);
            match &kids[0].kind {
                BlockKind::Paragraph { text } => {
                    assert_eq!(paragraph_visible_string(text), "beta");
                }
                _ => panic!("expected paragraph"),
            }
        }
        _ => panic!("expected blockquote"),
    }
}

#[test]
fn ingest_paragraph_text_edit_preserves_block_and_prefix_unit_ids() {
    use md_crdt::doc::paragraph_visible_ids;
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "hello").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();

    let (block_id, prefix_ids) = {
        let doc = vs.session_mut("doc.md").unwrap().document();
        let b = &doc.blocks_in_order()[0];
        let ids = match &b.kind {
            BlockKind::Paragraph { text } => paragraph_visible_ids(text),
            _ => panic!("paragraph"),
        };
        // "hel" prefix of "hello"
        (b.id, ids[..3].to_vec())
    };

    fs::write(dir.path().join("doc.md"), "help").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);
    assert!(report.ops_emitted >= 1);

    let doc = vs.session_mut("doc.md").unwrap().document();
    let b = &doc.blocks_in_order()[0];
    assert_eq!(b.id, block_id, "BlockId preserved across text edit");
    match &b.kind {
        BlockKind::Paragraph { text } => {
            assert_eq!(paragraph_visible_string(text), "help");
            let ids = paragraph_visible_ids(text);
            assert_eq!(ids.len(), 4);
            let retained = ids.iter().filter(|id| prefix_ids.contains(id)).count();
            assert!(
                retained >= 2,
                "LCS should retain shared unit OpIds, retained={retained}"
            );
        }
        _ => panic!("paragraph"),
    }
}

#[test]
fn ingest_full_paragraph_rewrite_preserves_block_id_via_position_pairing() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("doc.md"), "alpha").unwrap();
    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();
    let id = vs
        .session_mut("doc.md")
        .unwrap()
        .document()
        .blocks_in_order()[0]
        .id;

    // Completely different text (zero shared graphemes) falls below the fingerprint
    // match floor, so only position-pairing keeps the BlockId — otherwise this would be
    // a remove+add with a fresh id.
    fs::write(dir.path().join("doc.md"), "zzzzz").unwrap();
    let report = vs.ingest_all().unwrap();
    assert_eq!(report.files_changed, 1);

    let doc = vs.session_mut("doc.md").unwrap().document();
    assert_eq!(doc.blocks_in_order().len(), 1);
    let b = &doc.blocks_in_order()[0];
    assert_eq!(
        b.id, id,
        "BlockId preserved via position pairing on full rewrite"
    );
    match &b.kind {
        BlockKind::Paragraph { text } => assert_eq!(paragraph_visible_string(text), "zzzzz"),
        _ => panic!("paragraph"),
    }
}

#[test]
fn ingest_quote_inner_text_edit_preserves_quote_and_lcs_units() {
    use md_crdt::doc::paragraph_visible_ids;
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("q.md"), "> hello").unwrap();

    let mut vs = VaultSession::open(dir.path()).unwrap();
    vs.ingest_all().unwrap();

    let (quote_id, child_id, prefix) = {
        let top = &vs.session_mut("q.md").unwrap().document().blocks_in_order()[0];
        match &top.kind {
            BlockKind::BlockQuote { children } => {
                let kid = children.iter().next().unwrap();
                let ids = match &kid.kind {
                    BlockKind::Paragraph { text } => paragraph_visible_ids(text),
                    _ => panic!("para"),
                };
                (top.id, kid.id, ids[..3].to_vec())
            }
            _ => panic!("quote"),
        }
    };

    fs::write(dir.path().join("q.md"), "> help").unwrap();
    vs.ingest_all().unwrap();

    let top = &vs.session_mut("q.md").unwrap().document().blocks_in_order()[0];
    assert_eq!(top.id, quote_id);
    match &top.kind {
        BlockKind::BlockQuote { children } => {
            let kid = children.iter().next().unwrap();
            assert_eq!(kid.id, child_id, "nested paragraph BlockId preserved");
            match &kid.kind {
                BlockKind::Paragraph { text } => {
                    assert_eq!(paragraph_visible_string(text), "help");
                    let ids = paragraph_visible_ids(text);
                    let retained = ids.iter().filter(|id| prefix.contains(id)).count();
                    assert!(
                        retained >= 2,
                        "LCS retain inside quote, retained={retained}"
                    );
                }
                _ => panic!("para"),
            }
        }
        _ => panic!("quote"),
    }
}

#[test]
fn repeated_grapheme_refresh_preserves_retained_anchors_and_projects_only_their_marks() {
    use md_crdt::doc::paragraph_visible_ids;
    use md_crdt::{Anchor, AnchorBias, TextPoint, TextPosition, WorkspaceTargetError};

    for (initial, updated, retained_new, expected_markdown) in [
        ("a**a**", "a", 0, "**a**"),
        ("a**b**", "bb", 1, "b**b**"),
        ("e\u{301}**e\u{301}**", "e\u{301}", 0, "**e\u{301}**"),
        ("**a**b", "ba", 0, "ba"),
    ] {
        let directory = tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, initial).unwrap();
        let mut vault = VaultSession::open(directory.path()).unwrap();
        vault.ingest_all().unwrap();
        let (block_id, old_ids) = {
            let document = vault.session_mut("note.md").unwrap().document();
            let block = document.blocks_in_order()[0];
            let BlockKind::Paragraph { text } = &block.kind else {
                panic!("paragraph expected")
            };
            (block.id, paragraph_visible_ids(text))
        };
        let point = |unit| TextPoint {
            block_id,
            position: TextPosition::Unit(Anchor {
                elem_id: unit,
                bias: AnchorBias::Before,
            }),
        };
        let retained_anchor = point(old_ids[1]);
        let deleted_anchor = point(old_ids[0]);
        fs::write(&path, updated).unwrap();
        vault.refresh_markdown("note.md", None, None).unwrap();
        let document = vault.session_mut("note.md").unwrap().document();
        let block = document.find_block_by_id(block_id).unwrap();
        let BlockKind::Paragraph { text } = &block.kind else {
            panic!("paragraph expected")
        };
        let ids = paragraph_visible_ids(text);
        assert_eq!(paragraph_visible_string(text), updated);
        assert_eq!(ids[retained_new], old_ids[1]);
        assert!(!ids.contains(&old_ids[0]));
        assert_eq!(
            document.resolve_text_point(&retained_anchor).unwrap(),
            retained_new
        );
        assert!(matches!(
            document.resolve_text_point(&deleted_anchor),
            Err(WorkspaceTargetError::DeletedAnchor { .. })
        ));
        assert_eq!(
            document.serialize(EquivalenceMode::Structural),
            expected_markdown
        );
    }
}

#[test]
fn long_local_refresh_preserves_exact_units_marks_and_anchors() {
    use md_crdt::doc::paragraph_visible_ids;
    use md_crdt::{Anchor, AnchorBias, TextPoint, TextPosition, WorkspaceTargetError};

    for scenario in [
        "unique_end_substitute",
        "unicode_middle_insert",
        "repeated_delete",
    ] {
        let before = match scenario {
            "unique_end_substitute" => (0..2_001)
                .map(|i| char::from_u32(0xf0000 + i).unwrap())
                .collect::<String>(),
            "unicode_middle_insert" => ["e\u{301}", "👩\u{200d}💻", "🇺🇸", "🧑🏽\u{200d}🚀", "界"]
                .into_iter()
                .cycle()
                .take(2_001)
                .collect::<String>(),
            "repeated_delete" => "e\u{301}".repeat(2_001),
            _ => unreachable!(),
        };
        let old = md_crdt::filesync::graphemes_of(&before);
        let initial = format!(
            "{}**{}**{}",
            old[..1_000].concat(),
            old[1_000],
            old[1_001..].concat()
        );
        let mut new = old.clone();
        let (marked_new, deleted_old) = match scenario {
            "unique_end_substitute" => {
                new[2_000] = "§";
                (1_000, Some(2_000))
            }
            "unicode_middle_insert" => {
                new.insert(1_000, "§");
                (1_001, None)
            }
            "repeated_delete" => {
                new.remove(0);
                (999, Some(0))
            }
            _ => unreachable!(),
        };
        let updated = new.concat();
        let expected_markdown = format!(
            "{}**{}**{}",
            new[..marked_new].concat(),
            new[marked_new],
            new[marked_new + 1..].concat()
        );
        let directory = tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, initial).unwrap();
        let mut vault = VaultSession::open(directory.path()).unwrap();
        vault.ingest_all().unwrap();
        let (block_id, old_ids) = {
            let document = vault.session_mut("note.md").unwrap().document();
            let block = document.blocks_in_order()[0];
            let BlockKind::Paragraph { text } = &block.kind else {
                panic!("paragraph expected")
            };
            (block.id, paragraph_visible_ids(text))
        };
        let point = |unit| TextPoint {
            block_id,
            position: TextPosition::Unit(Anchor {
                elem_id: unit,
                bias: AnchorBias::Before,
            }),
        };
        fs::write(&path, &updated).unwrap();
        vault.refresh_markdown("note.md", None, None).unwrap();
        let document = vault.session_mut("note.md").unwrap().document();
        assert_eq!(document.blocks_in_order().len(), 1);
        let block = document.find_block_by_id(block_id).unwrap();
        let BlockKind::Paragraph { text } = &block.kind else {
            panic!("paragraph expected")
        };
        let ids = paragraph_visible_ids(text);
        assert_eq!(paragraph_visible_string(text), updated);
        match scenario {
            "unique_end_substitute" => {
                assert_eq!(&ids[..2_000], &old_ids[..2_000]);
                assert!(!old_ids.contains(&ids[2_000]));
            }
            "unicode_middle_insert" => {
                assert_eq!(&ids[..1_000], &old_ids[..1_000]);
                assert!(!old_ids.contains(&ids[1_000]));
                assert_eq!(&ids[1_001..], &old_ids[1_000..]);
            }
            "repeated_delete" => assert_eq!(ids, old_ids[1..]),
            _ => unreachable!(),
        }
        assert_eq!(
            document.resolve_text_point(&point(old_ids[1_000])).unwrap(),
            marked_new
        );
        if let Some(deleted) = deleted_old {
            assert!(matches!(
                document.resolve_text_point(&point(old_ids[deleted])),
                Err(WorkspaceTargetError::DeletedAnchor { .. })
            ));
        }
        assert_eq!(
            document.serialize(EquivalenceMode::Structural),
            expected_markdown
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), updated);
    }
}

#[test]
fn dissimilar_long_refresh_refusal_preserves_live_snapshot_anchors_and_edited_disk() {
    use md_crdt::doc::paragraph_visible_ids;
    use md_crdt::filesync::VaultError;
    use md_crdt::limits::InputLimitError;
    use md_crdt::{Anchor, AnchorBias, TextPoint, TextPosition};

    let directory = tempdir().unwrap();
    let path = directory.path().join("note.md");
    let before = "x".repeat(5_000);
    let edited = "y".repeat(5_000);
    fs::write(&path, &before).unwrap();
    let mut vault = VaultSession::open(directory.path()).unwrap();
    vault.ingest_all().unwrap();
    let session = vault.session_mut("note.md").unwrap();
    let snapshot = session.save_snapshot().unwrap().to_bytes().unwrap();
    let block = session.document().blocks_in_order()[0];
    let BlockKind::Paragraph { text } = &block.kind else {
        panic!("paragraph expected")
    };
    let anchor = TextPoint {
        block_id: block.id,
        position: TextPosition::Unit(Anchor {
            elem_id: paragraph_visible_ids(text)[0],
            bias: AnchorBias::Before,
        }),
    };
    fs::write(&path, &edited).unwrap();
    assert!(matches!(
        vault.refresh_markdown("note.md", None, None),
        Err(VaultError::InputLimit(InputLimitError::DiffWork))
    ));
    let session = vault.session_mut("note.md").unwrap();
    assert_eq!(
        session.save_snapshot().unwrap().to_bytes().unwrap(),
        snapshot
    );
    assert_eq!(
        session.document().serialize(EquivalenceMode::Structural),
        before
    );
    assert_eq!(session.document().resolve_text_point(&anchor).unwrap(), 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
}
