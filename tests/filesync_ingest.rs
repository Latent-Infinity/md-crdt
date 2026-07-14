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
        BlockKind::List { ordered, items } => {
            assert!(!ordered);
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
fn table_ingest_preserves_table_row_and_unrelated_prose_ids() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("table.md"),
        "before\n\n| name | value |\n| --- | ---: |\n| a | 1 |\n| b | 2 |\n\nafter",
    )
    .unwrap();
    let mut vault = VaultSession::open(dir.path()).unwrap();
    let first = vault.ingest_all().unwrap();
    assert_eq!(first.files_skipped, 0);
    let (prose_ids, table_id, row_ids) = {
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
            .map(|row| row.cells.get())
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
