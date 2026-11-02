//! Public block matching preserves identities without dense unchanged work.

#![cfg(feature = "filesync")]

use md_crdt::filesync::{
    BlockFingerprint, Fingerprint, LastFlushedState, MatchConfig, MatchType, ParsedBlock, Score,
    VaultError, match_blocks,
};

fn old_blocks(contents: &[(&str, &[usize])]) -> LastFlushedState {
    LastFlushedState {
        content_hash: 0,
        blocks: contents
            .iter()
            .enumerate()
            .map(|(index, (content, path))| BlockFingerprint {
                block_id: uuid::Uuid::from_u128(index as u128 + 1),
                fingerprint: Fingerprint::from_content(content),
                container_path: path.to_vec(),
                position: index,
            })
            .collect(),
    }
}

fn parsed_blocks(contents: &[(&str, &[usize])]) -> Vec<ParsedBlock> {
    contents
        .iter()
        .enumerate()
        .map(|(index, (content, path))| ParsedBlock {
            fingerprint: Fingerprint::from_content(content),
            container_path: path.to_vec(),
            position: index,
        })
        .collect()
}

fn content_config() -> MatchConfig {
    MatchConfig {
        min_match_score: Score(5000),
        ..MatchConfig::default()
    }
}

#[test]
fn distinctive_exact_matches_keep_identity_when_common_tokens_favor_position() {
    let common = (0..32)
        .map(|index| format!("shared{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let first = format!("first {common}");
    let second = format!("second {common}");
    let old = old_blocks(&[(&first, &[]), (&second, &[])]);
    let parsed = parsed_blocks(&[(&second, &[]), (&first, &[])]);
    let mapping = match_blocks(&old, &parsed, &content_config()).unwrap();

    assert!(mapping.added.is_empty());
    assert!(mapping.removed.is_empty());
    assert_eq!(mapping.matched.len(), 2);
    for matched in &mapping.matched {
        assert_eq!(matched.old_id, old.blocks[1 - matched.new_index].block_id);
        // The existing score threshold, not the matching phase, classifies it.
        assert_eq!(matched.match_type, MatchType::FuzzyContent);
    }

    // Raising only the supported score floor disqualifies the distant exact
    // pairs and exposes the higher-scored same-position fuzzy alternatives.
    let strict = MatchConfig {
        min_match_score: Score(9400),
        ..content_config()
    };
    let fuzzy = match_blocks(&old, &parsed, &strict).unwrap();
    assert_eq!(fuzzy.matched.len(), 2);
    for matched in fuzzy.matched {
        assert_eq!(matched.old_id, old.blocks[matched.new_index].block_id);
        let exact = mapping
            .matched
            .iter()
            .find(|exact| exact.new_index == matched.new_index)
            .unwrap();
        assert!(matched.confidence > exact.confidence);
        assert!(exact.confidence < strict.min_match_score);
        assert!(matched.confidence >= strict.min_match_score);
    }
}

#[test]
fn duplicate_fingerprints_keep_scored_ties_and_copy_removed_new_classification() {
    let old = old_blocks(&[
        ("common repeated", &[]),
        ("common repeated", &[]),
        ("rare stable", &[]),
        ("removed unrelated", &[]),
    ]);
    let parsed = parsed_blocks(&[
        ("common repeated", &[]),
        ("rare stable", &[]),
        ("common repeated", &[]),
        ("common repeated", &[]),
        ("entirely novel", &[]),
    ]);
    let mapping = match_blocks(&old, &parsed, &content_config()).unwrap();

    assert_eq!(mapping.matched.len(), 3);
    for (old_index, new_index) in [(0, 0), (1, 2), (2, 1)] {
        let matched = mapping
            .matched
            .iter()
            .find(|matched| matched.new_index == new_index)
            .unwrap();
        assert_eq!(matched.old_id, old.blocks[old_index].block_id);
    }
    assert_eq!(mapping.removed, vec![old.blocks[3].block_id]);
    assert_eq!(mapping.added.len(), 2);
    assert_eq!(mapping.added[0].new_index, 3);
    assert_eq!(
        mapping.added[0].probable_copy_of,
        Some(old.blocks[0].block_id)
    );
    assert_eq!(mapping.added[1].new_index, 4);
    assert_eq!(mapping.added[1].probable_copy_of, None);
    assert_eq!(
        mapping,
        match_blocks(&old, &parsed, &content_config()).unwrap()
    );
}

#[test]
fn reordered_and_repeated_tokens_retain_existing_fingerprint_identity() {
    let old = old_blocks(&[("alpha beta", &[])]);
    let parsed = parsed_blocks(&[("beta alpha alpha", &[])]);
    let mapping = match_blocks(&old, &parsed, &content_config()).unwrap();

    assert_eq!(mapping.matched.len(), 1);
    assert_eq!(mapping.matched[0].old_id, old.blocks[0].block_id);
    assert_eq!(mapping.matched[0].match_type, MatchType::ExactFingerprint);
    assert!(mapping.added.is_empty());
    assert!(mapping.removed.is_empty());
}

#[test]
fn exact_content_retains_container_compatibility_and_score_floor() {
    for (old_path, new_path, compatible) in [
        (vec![0, 0], vec![0, 1], true),
        (vec![], vec![9], true),
        (vec![0, 0], vec![1, 0], false),
    ] {
        let old = old_blocks(&[("same content", &old_path)]);
        let parsed = parsed_blocks(&[("same content", &new_path)]);
        let mapping = match_blocks(&old, &parsed, &content_config()).unwrap();
        if compatible {
            assert_eq!(mapping.matched.len(), 1);
            assert_eq!(mapping.matched[0].old_id, old.blocks[0].block_id);
            assert!(mapping.removed.is_empty());
            assert!(mapping.added.is_empty());
        } else {
            assert!(mapping.matched.is_empty());
            assert_eq!(mapping.removed, vec![old.blocks[0].block_id]);
            assert_eq!(mapping.added.len(), 1);
            assert_eq!(mapping.added[0].probable_copy_of, None);
        }
    }

    let old = old_blocks(&[("same content", &[])]);
    let parsed = parsed_blocks(&[("same content", &[])]);
    let config = MatchConfig {
        min_match_score: Score(10001),
        ..content_config()
    };
    let mapping = match_blocks(&old, &parsed, &config).unwrap();
    assert!(mapping.matched.is_empty());
    assert_eq!(mapping.removed, vec![old.blocks[0].block_id]);
    assert_eq!(mapping.added.len(), 1);
}

#[test]
fn configured_floor_keeps_zero_overlap_position_and_container_matches() {
    let old = old_blocks(&[("old words", &[])]);
    let parsed = parsed_blocks(&[("new tokens", &[])]);

    for (floor, expected_matches) in [(0, 1), (2000, 1), (4000, 1), (4001, 0)] {
        let config = MatchConfig {
            min_match_score: Score(floor),
            ..MatchConfig::default()
        };
        let mapping = match_blocks(&old, &parsed, &config).unwrap();
        assert_eq!(mapping.matched.len(), expected_matches);
        if expected_matches == 1 {
            assert_eq!(mapping.matched[0].old_id, old.blocks[0].block_id);
            assert_eq!(mapping.matched[0].confidence, Score(4000));
            assert_eq!(mapping.matched[0].match_type, MatchType::FuzzyContent);
            assert!(mapping.added.is_empty());
            assert!(mapping.removed.is_empty());
        } else {
            assert_eq!(mapping.removed, vec![old.blocks[0].block_id]);
            assert_eq!(mapping.added.len(), 1);
            assert_eq!(mapping.added[0].probable_copy_of, None);
        }
    }
}

#[test]
fn empty_duplicate_fingerprints_match_and_classify_copy_without_postings() {
    let old = old_blocks(&[("", &[]), ("", &[])]);
    let parsed = parsed_blocks(&[("", &[]), ("", &[]), ("", &[])]);
    let mapping = match_blocks(&old, &parsed, &content_config()).unwrap();

    assert_eq!(mapping.matched.len(), 2);
    for matched in mapping.matched {
        assert_eq!(matched.old_id, old.blocks[matched.new_index].block_id);
    }
    assert!(mapping.removed.is_empty());
    assert_eq!(mapping.added.len(), 1);
    assert_eq!(mapping.added[0].new_index, 2);
    assert_eq!(
        mapping.added[0].probable_copy_of,
        Some(old.blocks[0].block_id)
    );
}

#[test]
fn ambiguous_duplicates_still_refuse_at_candidate_edge_bound() {
    let contents = vec![("repeat words", &[][..]); 501];
    let old = old_blocks(&contents);
    let parsed = parsed_blocks(&contents);

    assert!(matches!(
        match_blocks(&old, &parsed, &content_config()),
        Err(VaultError::IngestMatchLimit {
            resource: "candidate edges",
            limit: 250_000,
        })
    ));
}

#[test]
fn all_changed_common_token_paragraphs_still_refuse_at_work_bound() {
    let common = (0..50)
        .map(|index| format!("common-{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let before: Vec<_> = (0..300)
        .map(|index| format!("before-{index} {common}"))
        .collect();
    let after: Vec<_> = (0..300)
        .map(|index| format!("after-{index} {common}"))
        .collect();
    let before_refs: Vec<_> = before.iter().map(|text| (text.as_str(), &[][..])).collect();
    let after_refs: Vec<_> = after.iter().map(|text| (text.as_str(), &[][..])).collect();
    let old = old_blocks(&before_refs);
    let parsed = parsed_blocks(&after_refs);

    assert!(matches!(
        match_blocks(&old, &parsed, &content_config()),
        Err(VaultError::IngestMatchLimit {
            resource: "scoring work",
            limit: 4_000_000,
        })
    ));
}

#[test]
fn one_word_refresh_of_three_hundred_common_paragraphs_preserves_logical_ids() {
    use md_crdt::doc::{BlockKind, EquivalenceMode, paragraph_visible_string};
    use md_crdt::filesync::VaultSession;
    use std::fs;
    use tempfile::tempdir;

    let common = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim ad minim veniam quis nostrud exercitation ullamco laboris nisi aliquip ex ea commodo consequat duis aute irure reprehenderit voluptate velit esse cillum fugiat nulla pariatur";
    let source = (0..300)
        .map(|index| format!("Paragraph{index} {common}."))
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_eq!(source.len(), 94_688);
    let root = tempdir().unwrap();
    let path = root.path().join("note.md");
    fs::write(&path, &source).unwrap();
    let mut vault = VaultSession::open(root.path()).unwrap();
    vault.ingest_all().unwrap();
    let before: Vec<_> = vault
        .session_mut("note.md")
        .unwrap()
        .document()
        .blocks_in_order()
        .iter()
        .map(|block| block.id)
        .collect();
    assert_eq!(before.len(), 300);

    let edited = source.replacen("lorem", "changed", 1);
    fs::write(&path, &edited).unwrap();
    vault.ingest_markdown("note.md", None, None).unwrap();

    let document = vault.session_mut("note.md").unwrap().document();
    let blocks = document.blocks_in_order();
    assert_eq!(
        blocks.iter().map(|block| block.id).collect::<Vec<_>>(),
        before
    );
    for (block, paragraph) in blocks.iter().zip(edited.split("\n\n")) {
        let BlockKind::Paragraph { text } = &block.kind else {
            panic!("expected paragraph");
        };
        assert_eq!(paragraph_visible_string(text), paragraph);
    }
    assert_eq!(document.serialize(EquivalenceMode::Exact), edited);
}

#[test]
fn ineligible_unique_exact_pairs_leave_fuzzy_alternatives_available() {
    let common = "one two three four five six seven eight nine ten eleven twelve";
    let exact = format!("first {common}");
    let fuzzy = format!("second {common}");
    for (old_path, exact_path, floor) in [(vec![0, 0], vec![1, 0], 5000), (vec![], vec![], 9000)] {
        let old = old_blocks(&[(&exact, &old_path)]);
        let parsed = parsed_blocks(&[(&fuzzy, &old_path), (&exact, &exact_path)]);
        let config = MatchConfig {
            min_match_score: Score(floor),
            ..MatchConfig::default()
        };
        let mapping = match_blocks(&old, &parsed, &config).unwrap();
        assert_eq!(mapping.matched.len(), 1);
        assert_eq!(mapping.matched[0].new_index, 0);
        assert_eq!(mapping.matched[0].old_id, old.blocks[0].block_id);
        assert!(mapping.matched[0].confidence >= config.min_match_score);
        assert!(mapping.removed.is_empty());
        assert_eq!(mapping.added.len(), 1);
        assert_eq!(mapping.added[0].new_index, 1);
    }
}

#[test]
fn exact_first_refresh_keeps_marks_and_unit_anchors_with_reordered_content() {
    use md_crdt::MarkKind;
    use md_crdt::doc::{BlockKind, paragraph_visible_ids};
    use md_crdt::filesync::VaultSession;
    use std::fs;
    use tempfile::tempdir;

    let common = (0..32)
        .map(|index| format!("shared{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let initial = format!("**first** {common}\n\n[second](target) {common}");
    let updated = format!("second {common}\n\nfirst {common}");
    let directory = tempdir().unwrap();
    let path = directory.path().join("note.md");
    fs::write(&path, initial).unwrap();
    let mut vault = VaultSession::open(directory.path()).unwrap();
    vault.ingest_all().unwrap();
    let (before, anchors) = {
        let document = vault.session_mut("note.md").unwrap().document();
        let before = document
            .blocks_in_order()
            .into_iter()
            .map(|block| {
                let BlockKind::Paragraph { text } = &block.kind else {
                    panic!("paragraph expected")
                };
                (block.id, paragraph_visible_ids(text))
            })
            .collect::<Vec<_>>();
        let anchors = before
            .iter()
            .map(|(id, _)| document.text_point(*id, 1).unwrap())
            .collect::<Vec<_>>();
        (before, anchors)
    };
    fs::write(&path, updated).unwrap();
    vault.refresh_markdown("note.md", None, None).unwrap();
    let document = vault.session_mut("note.md").unwrap().document();
    let blocks = document.blocks_in_order();
    assert_eq!(blocks.len(), 2);
    for (new_index, old_index, mark_kind, end) in
        [(0, 1, MarkKind::Link, 6), (1, 0, MarkKind::Bold, 5)]
    {
        let block = blocks[new_index];
        assert_eq!(block.id, before[old_index].0);
        let BlockKind::Paragraph { text } = &block.kind else {
            panic!("paragraph expected")
        };
        assert_eq!(paragraph_visible_ids(text), before[old_index].1);
        assert_eq!(document.resolve_text_point(&anchors[old_index]).unwrap(), 1);
        let marks = block.marks.resolved_intervals_in_sequence(text);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].0.kind, mark_kind);
        assert_eq!((marks[0].1, marks[0].2), (0, end));
    }
}
