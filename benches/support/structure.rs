//! Phase-one P4/P5 study harness: no production component is enabled here.
//! Dataset identity is `cycle2-structure-v2`; one iteration is one named operation
//! (or the explicitly named sixteen-append group), never fixture construction.
//! Cold means an empty Document index, not a flushed CPU/allocator/page cache.
//! Document::clone resets its index (src/doc/mod.rs); session cloning delegates
//! to it. Every cold iteration gets its own clone outside the timer. Assertions,
//! checkpointing, snapshot encoding, metadata and allocation output are untimed.
//! No Criterion sampling/time configuration is overridden by this module.
use criterion::{BatchSize, Criterion, Throughput};
use md_crdt::{
    Block, BlockDraft, BlockId, BlockKind, CollaborativeDocument, Document, EquivalenceMode,
    ListItemDraft, OpId, Parser, Sequence, SessionSnapshot,
};
use serde_json::json;
use std::hint::black_box;

const DATASET: &str = "cycle2-structure-v2";
const GROUPS: [&str; 4] = [
    "cycle2_lookup",
    "cycle2_structure",
    "cycle2_move_history",
    "cycle2_deletion_context",
];

/// Comma-separated exact group names; applied before unrelated fixture creation.
/// The top-level benchmark registry validates names against all available groups.
pub fn group_enabled(name: &str) -> bool {
    std::env::var("MD_CRDT_BENCH_GROUPS")
        .map_or(true, |groups| groups.split(',').any(|group| group == name))
}

/// Numeric filters select subsets of the declared inventory, not new sizes.
/// Missing axes are not filtered (e.g. construct has no history axis).
fn axis_enabled(name: &str, value: usize) -> bool {
    std::env::var(name).map_or(true, |values| {
        values
            .split(',')
            .map(|value| value.parse::<usize>().expect("numeric study filter"))
            .any(|selected| selected == value)
    })
}
fn selected(count: usize, depth: Option<usize>, history: Option<usize>) -> bool {
    axis_enabled("MD_CRDT_STRUCTURE_COUNTS", count)
        && depth.is_none_or(|depth| axis_enabled("MD_CRDT_STRUCTURE_DEPTHS", depth))
        && history.is_none_or(|history| axis_enabled("MD_CRDT_STRUCTURE_HISTORIES", history))
}
fn shape_enabled(shape: &str) -> bool {
    std::env::var("MD_CRDT_STRUCTURE_SHAPES")
        .map_or(true, |shapes| shapes.split(',').any(|value| value == shape))
}
fn emit(value: serde_json::Value) {
    println!("{}", value);
}
fn probe<T>(enabled: bool, name: &str, operation: impl FnOnce() -> T) -> T {
    if enabled {
        let (result, stats) = super::allocation_support::capture(operation);
        emit(json!({"kind": "structure_allocation", "dataset": DATASET,
            "name": name, "stats": stats}));
        result
    } else {
        operation()
    }
}
fn checkpoint(session: &mut CollaborativeDocument) -> SessionSnapshot {
    let topology_before = session.save_snapshot().unwrap().document.block_topology;
    session
        .checkpoint_history(&md_crdt::CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: md_crdt::DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let snapshot = session.save_snapshot().unwrap();
    assert!(snapshot.ops.is_empty());
    assert!(snapshot.pending.is_empty());
    assert!(snapshot.deferred.is_empty());
    assert_eq!(
        snapshot.document.block_topology, topology_before,
        "transport checkpoint must preserve every durable topology record"
    );
    snapshot
}
fn metadata(name: &str, snapshot: &SessionSnapshot) {
    let topology = &snapshot.document.block_topology;
    emit(
        json!({"kind": "structure_metadata", "dataset": DATASET, "name": name,
        "wire_version": md_crdt::codec::WIRE_VERSION,
        "snapshot_version": snapshot.format_version,
        "retained_transport_ops": snapshot.ops.len(), "pending": snapshot.pending.len(),
        "deferred": snapshot.deferred.len(),
        "initial_blocks": topology.initial.len(), "initial_items": topology.initial_items.len(),
        "block_moves": topology.moves.len(), "item_moves": topology.item_moves.len(),
        "block_origins": topology.block_origins.len(), "item_origins": topology.item_origins.len(),
        "item_aliases": topology.item_aliases.len(), "deletions": topology.deletions.len(),
        "captured_deletion_targets": topology.deletions.iter().map(|d| d.subtree.len()).sum::<usize>(),
        "deleted_blocks": topology.deleted.len(), "deleted_items": topology.deleted_items.len(),
        "direct_deleted": topology.direct_deleted.len(), "imported_origins": topology.imported_origins.len(),
        "topology_json_bytes": serde_json::to_vec(topology).unwrap().len(),
        "document_json_bytes": serde_json::to_vec(&snapshot.document).unwrap().len(),
        "snapshot_bytes": snapshot.to_bytes().unwrap().len()}),
    );
}
fn lookup_target(document: &Document) -> (BlockId, OpId) {
    fn descend(sequence: &Sequence<Block>) -> Option<(BlockId, OpId)> {
        let block = sequence.iter().last()?;
        match &block.kind {
            BlockKind::BlockQuote { children } => descend(children),
            BlockKind::List { items, .. } => descend(&items.iter().last()?.children),
            _ => Some((block.id, block.elem_id)),
        }
    }
    descend(document.blocks()).expect("nonempty lookup fixture")
}
fn item_target(document: &Document) -> (BlockId, OpId) {
    let mut block = document.blocks().iter().next().unwrap();
    loop {
        match &block.kind {
            BlockKind::BlockQuote { children } => block = children.iter().next().unwrap(),
            BlockKind::List { items, .. } => {
                let item = items.iter().last().unwrap();
                return (item.id, item.elem_id);
            }
            _ => panic!("list fixture"),
        }
    }
}
fn cold_document(base: &Document) -> Document {
    let mut cold = base.clone();
    // Public mutable sequence access safely advances the index generation without
    // modifying IDs, topology or content. Source metadata is irrelevant to lookup.
    let _ = cold.blocks_mut();
    cold
}
fn cold_session(base: &CollaborativeDocument) -> CollaborativeDocument {
    // The derived session clone invokes Document::clone, which installs None.
    // Do not query this clone in setup unless the case is explicitly warm.
    base.clone()
}

pub fn cycle2_lookup(c: &mut Criterion) {
    lookup(Some(c), true);
}
fn lookup(mut criterion: Option<&mut Criterion>, probes: bool) {
    let mut group = criterion
        .as_mut()
        .map(|c| c.benchmark_group("cycle2_lookup"));
    let mut cases = 0usize;
    for shape in ["list", "quote"] {
        if !shape_enabled(shape) {
            continue;
        }
        for count in [1usize, 256, 4_096, 10_000] {
            for depth in [0usize, 8, 32] {
                if !selected(count, Some(depth), None) {
                    continue;
                }
                let prefix = "> ".repeat(depth);
                let source = (0..count)
                    .map(|index| {
                        if shape == "list" {
                            format!("{prefix}- p-{index}\n")
                        } else {
                            format!("{prefix}p-{index}\n{prefix}\n")
                        }
                    })
                    .collect::<String>();
                let document = Parser::try_parse(&source).unwrap();
                assert_eq!(document.serialize(EquivalenceMode::Exact), source);
                let (logical, physical) = lookup_target(&document);
                assert_eq!(document.find_block(physical).unwrap().id, logical);
                assert_eq!(
                    document.find_block_by_id(logical).unwrap().elem_id,
                    physical
                );
                let label = if shape == "list" {
                    format!("{count}/depth-{depth}")
                } else {
                    format!("quote/{count}/depth-{depth}")
                };
                emit(json!({"kind": "structure_fixture", "dataset": DATASET,
                    "group": "cycle2_lookup", "name": label, "shape": shape,
                    "breadth": count, "quote_depth": depth,
                    "target_block_path_depth": depth + usize::from(shape == "list"),
                    "source_bytes": source.len(), "logical": logical, "physical": physical,
                    "history_control": "parser initial topology; no move events"}));
                for by_id in [false, true] {
                    for warm in [false, true] {
                        let regime = match (warm, by_id) {
                            (false, false) => "cold",
                            (false, true) => "cold_logical",
                            (true, false) => "warm",
                            (true, true) => "warm_logical",
                        };
                        let name = format!("{regime}/{label}");
                        let perform = |doc: &Document| {
                            if by_id {
                                black_box(
                                    black_box(doc)
                                        .find_block_by_id(black_box(logical))
                                        .unwrap()
                                        .elem_id,
                                )
                            } else {
                                black_box(
                                    black_box(doc)
                                        .find_block(black_box(physical))
                                        .unwrap()
                                        .elem_id,
                                )
                            }
                        };
                        let input = if warm {
                            document.clone()
                        } else {
                            cold_document(&document)
                        };
                        if warm {
                            input.find_block(physical).unwrap();
                        }
                        let result =
                            probe(probes, &format!("cycle2_lookup/{name}"), || perform(&input));
                        assert_eq!(result, physical);
                        cases += 1;
                        if let Some(group) = &mut group {
                            group.throughput(Throughput::Elements(1));
                            group.bench_function(name, |b| {
                                if warm {
                                    b.iter(|| perform(&input));
                                } else {
                                    b.iter_batched_ref(
                                        || cold_document(&document),
                                        |cold| perform(cold),
                                        BatchSize::PerIteration,
                                    );
                                }
                            });
                        }
                    }
                }
                if shape == "list" {
                    let (logical_item, physical_item) = item_target(&document);
                    assert_eq!(
                        document.find_list_item(physical_item).unwrap().id,
                        logical_item
                    );
                    assert_eq!(
                        document.find_list_item_by_id(logical_item).unwrap().elem_id,
                        physical_item
                    );
                    for by_id in [true, false] {
                        for warm in [false, true] {
                            let regime = match (warm, by_id) {
                                (false, true) => "cold_item",
                                (true, true) => "warm_item",
                                (false, false) => "cold_item_physical",
                                (true, false) => "warm_item_physical",
                            };
                            let name = format!("{regime}/{label}");
                            let perform = |doc: &Document| {
                                if by_id {
                                    black_box(
                                        black_box(doc)
                                            .find_list_item_by_id(black_box(logical_item))
                                            .unwrap()
                                            .elem_id,
                                    )
                                } else {
                                    black_box(
                                        black_box(doc)
                                            .find_list_item(black_box(physical_item))
                                            .unwrap()
                                            .elem_id,
                                    )
                                }
                            };
                            let input = if warm {
                                document.clone()
                            } else {
                                cold_document(&document)
                            };
                            if warm {
                                input.find_list_item(physical_item).unwrap();
                            }
                            let result =
                                probe(probes, &format!("cycle2_lookup/{name}"), || perform(&input));
                            assert_eq!(result, physical_item);
                            cases += 1;
                            if let Some(group) = &mut group {
                                group.throughput(Throughput::Elements(1));
                                group.bench_function(name, |b| {
                                    if warm {
                                        b.iter(|| perform(&input));
                                    } else {
                                        b.iter_batched_ref(
                                            || cold_document(&document),
                                            |cold| perform(cold),
                                            BatchSize::PerIteration,
                                        );
                                    }
                                });
                            }
                        }
                    }
                }
                let prepare = || {
                    let warm = cold_document(&document);
                    warm.find_block(physical).unwrap();
                    warm
                };
                let mutate = |doc: &mut Document| {
                    black_box(
                        doc.insert_text(
                            black_box(logical),
                            black_box(0),
                            black_box("X"),
                            super::id(1, 91),
                        )
                        .unwrap(),
                    )
                };
                let mut proof = prepare();
                let _ = probe(
                    probes,
                    &format!("cycle2_lookup/warm_content_mutation/{label}"),
                    || mutate(&mut proof),
                );
                assert!(
                    proof
                        .serialize(EquivalenceMode::Structural)
                        .contains(&format!("Xp-{}", count - 1))
                );
                assert_eq!(proof.find_block(physical).unwrap().id, logical);
                cases += 1;
                if let Some(group) = &mut group {
                    group.throughput(Throughput::Elements(1));
                    group.bench_function(format!("warm_content_mutation/{label}"), |b| {
                        b.iter_batched_ref(prepare, mutate, BatchSize::PerIteration);
                    });
                }
            }
        }
    }
    assert!(cases > 0, "lookup filters selected no cases");
    if let Some(group) = group {
        group.finish();
    }
    emit(
        json!({"kind": "structure_group_complete", "dataset": DATASET, "group": "cycle2_lookup", "cases": cases}),
    );
}

fn container_draft(list: bool, count: usize, depth: usize) -> BlockDraft {
    let children = (0..count)
        .map(|index| BlockDraft::Paragraph {
            text: format!("p-{index}"),
        })
        .collect::<Vec<_>>();
    let mut draft = if list {
        BlockDraft::List {
            style: Default::default(),
            items: children
                .into_iter()
                .map(|child| ListItemDraft {
                    task: None,
                    children: vec![child],
                })
                .collect(),
        }
    } else {
        BlockDraft::BlockQuote { children }
    };
    for _ in 0..depth {
        draft = BlockDraft::BlockQuote {
            children: vec![draft],
        };
    }
    draft
}
fn construct(draft: &BlockDraft) -> CollaborativeDocument {
    let mut session = CollaborativeDocument::new(20);
    // Depth-32 fixtures deliberately exceed the public default depth of 16.
    // Only the fixture's validation allowance changes; production limits do not.
    session
        .insert_draft_in(
            None,
            None,
            draft,
            md_crdt::StructuredEditLimits {
                max_depth: 64,
                ..Default::default()
            },
        )
        .unwrap();
    session
}
fn container_target(document: &Document, depth: usize) -> BlockId {
    let mut block = document.blocks().iter().next().unwrap();
    for _ in 0..depth {
        let BlockKind::BlockQuote { children } = &block.kind else {
            panic!("quote wrapper")
        };
        block = children.iter().next().unwrap();
    }
    block.id
}
fn child_count(document: &Document, list: bool, container: BlockId) -> usize {
    if list {
        document.list_items(container).unwrap().len_visible()
    } else {
        document
            .container_children(Some(document.block_elem_id(container).unwrap()))
            .unwrap()
            .len_visible()
    }
}
fn mutate(session: &mut CollaborativeDocument, list: bool, container: BlockId) {
    let root_elem = session
        .document()
        .block_elem_id(black_box(container))
        .unwrap();
    let mut after = if list {
        session
            .document()
            .list_items(container)
            .unwrap()
            .iter()
            .last()
            .map(|item| item.elem_id)
    } else {
        session
            .document()
            .container_children(Some(root_elem))
            .unwrap()
            .iter()
            .last()
            .map(|block| block.elem_id)
    };
    for index in 0..16 {
        let text = format!("append-{index}");
        let paragraph = if list {
            let item = session
                .insert_list_item(black_box(container), after, None)
                .unwrap();
            after = Some(item);
            session
                .insert_paragraph_in(Some(item), None, black_box(&text))
                .unwrap()
        } else {
            let paragraph = session
                .insert_paragraph_in(Some(root_elem), after, black_box(&text))
                .unwrap();
            after = Some(paragraph);
            paragraph
        };
        black_box(
            black_box(session.document())
                .find_block(black_box(paragraph))
                .unwrap()
                .id,
        );
        black_box(
            black_box(session.document())
                .find_block_by_id(black_box(container))
                .unwrap()
                .elem_id,
        );
    }
    black_box(session);
}
fn root_history(session: &mut CollaborativeDocument, history: usize) {
    let root = session.document().blocks().iter().next().unwrap().id;
    let root_elem = session.document().block_elem_id(root).unwrap();
    let tail = session
        .insert_paragraph(Some(root_elem), "history-tail")
        .unwrap();
    for step in 0..history {
        session
            .move_block(root, None, (step % 2 == 0).then_some(tail))
            .unwrap();
    }
}
fn roundtrip(session: &CollaborativeDocument) {
    let snapshot = session.save_snapshot().unwrap();
    let recovered = CollaborativeDocument::restore_from_snapshot(snapshot.clone()).unwrap();
    assert_eq!(recovered.document(), session.document());
    assert_eq!(
        recovered.save_snapshot().unwrap().document.block_topology,
        snapshot.document.block_topology
    );
}

pub fn cycle2_structure(c: &mut Criterion) {
    structure(Some(c), true);
}
fn structure(mut criterion: Option<&mut Criterion>, probes: bool) {
    let mut group = criterion
        .as_mut()
        .map(|c| c.benchmark_group("cycle2_structure"));
    let mut cases = 0usize;
    for list in [false, true] {
        let kind = if list { "list" } else { "quote" };
        if !shape_enabled(kind) {
            continue;
        }
        for count in [1usize, 32, 256, 1_024] {
            for depth in [0usize, 8, 32] {
                if !selected(count, Some(depth), None) {
                    continue;
                }
                let label = if depth == 0 {
                    format!("{kind}/{count}")
                } else {
                    format!("{kind}/{count}/depth-{depth}")
                };
                let draft = container_draft(list, count, depth);
                let built = construct(&draft);
                let container = container_target(built.document(), depth);
                assert_eq!(child_count(built.document(), list, container), count);
                let expected = built.document().serialize(EquivalenceMode::Structural);
                assert!(expected.contains(&format!("p-{}", count - 1)));
                let proof = probe(
                    probes,
                    &format!("cycle2_structure/construct/{label}"),
                    || construct(black_box(&draft)),
                );
                assert_eq!(proof.document(), built.document());
                roundtrip(&proof);
                let constructed = proof.save_snapshot().unwrap();
                let construction_ops = 1 + depth + count * if list { 3 } else { 2 };
                assert_eq!(constructed.ops.len(), construction_ops);
                emit(json!({"kind": "structure_workload", "dataset": DATASET,
                    "name": format!("cycle2_structure/construct/{label}"),
                    "paragraphs": count, "wire_operations": construction_ops,
                    "structured_validation_max_depth": 64}));
                cases += 1;
                if let Some(group) = &mut group {
                    group.throughput(Throughput::Elements(count as u64));
                    group.bench_function(format!("construct/{label}"), |b| {
                        b.iter_batched(
                            || (),
                            |()| black_box(construct(black_box(&draft))),
                            BatchSize::PerIteration,
                        );
                    });
                }
                for history in [0usize, 32] {
                    if !selected(count, Some(depth), Some(history)) {
                        continue;
                    }
                    let mut base = built.clone();
                    root_history(&mut base, history);
                    let snapshot = checkpoint(&mut base);
                    assert_eq!(snapshot.document.block_topology.moves.len(), history);
                    let name = if history == 0 {
                        format!("mutate_lookup/{label}")
                    } else {
                        format!("mutate_lookup/{label}/history-{history}")
                    };
                    metadata(&format!("cycle2_structure/{name}"), &snapshot);
                    // Both history controls keep all original content and IDs.
                    // Sixteen appends allocate identical payloads and wire operations.
                    let mut proof = cold_session(&base);
                    probe(probes, &format!("cycle2_structure/{name}"), || {
                        mutate(&mut proof, list, container)
                    });
                    assert_eq!(child_count(proof.document(), list, container), count + 16);
                    let actual = proof.document().serialize(EquivalenceMode::Structural);
                    assert!(actual.contains("p-0"));
                    for index in 0..16 {
                        assert!(actual.contains(&format!("append-{index}")));
                    }
                    assert_eq!(
                        proof.document().block_elem_id(container),
                        base.document().block_elem_id(container)
                    );
                    roundtrip(&proof);
                    let after = proof.save_snapshot().unwrap();
                    let mutation_ops = 16 * if list { 3 } else { 2 };
                    assert_eq!(after.ops.len(), mutation_ops);
                    assert_eq!(after.document.block_topology.moves.len(), history);
                    for (logical, placement) in &snapshot.document.block_topology.initial {
                        assert_eq!(
                            after.document.block_topology.initial.get(logical),
                            Some(placement)
                        );
                        assert_eq!(
                            proof.document().block_elem_id(*logical),
                            base.document().block_elem_id(*logical)
                        );
                    }
                    for (logical, placement) in &snapshot.document.block_topology.initial_items {
                        assert_eq!(
                            after.document.block_topology.initial_items.get(logical),
                            Some(placement)
                        );
                        assert_eq!(
                            proof
                                .document()
                                .find_list_item_by_id(*logical)
                                .unwrap()
                                .elem_id,
                            base.document()
                                .find_list_item_by_id(*logical)
                                .unwrap()
                                .elem_id
                        );
                    }
                    emit(json!({"kind": "structure_workload", "dataset": DATASET,
                        "name": format!("cycle2_structure/{name}"), "appends": 16,
                        "explicit_post_append_lookups": 32, "wire_operations": mutation_ops,
                        "history_moves_before": history, "history_moves_after": after.document.block_topology.moves.len()}));
                    cases += 1;
                    if let Some(group) = &mut group {
                        group.throughput(Throughput::Elements(16));
                        group.bench_function(name, |b| {
                            b.iter_batched_ref(
                                || cold_session(&base),
                                |session| mutate(session, black_box(list), black_box(container)),
                                BatchSize::PerIteration,
                            );
                        });
                    }
                }
            }
        }
    }
    assert!(cases > 0, "structure filters selected no cases");
    if let Some(group) = group {
        group.finish();
    }
    emit(
        json!({"kind": "structure_group_complete", "dataset": DATASET, "group": "cycle2_structure", "cases": cases}),
    );
}

fn move_once(
    session: &mut CollaborativeDocument,
    source: BlockId,
    destination: BlockId,
    item: Option<BlockId>,
    step: usize,
) -> OpId {
    if let Some(item) = item {
        session
            .move_list_item(
                black_box(item),
                if step.is_multiple_of(2) {
                    destination
                } else {
                    source
                },
                None,
            )
            .unwrap()
    } else {
        let destination = session
            .document()
            .block_elem_id(black_box(destination))
            .unwrap();
        session
            .move_block(
                black_box(source),
                if step.is_multiple_of(2) {
                    Some(destination)
                } else {
                    None
                },
                if step.is_multiple_of(2) {
                    None
                } else {
                    Some(destination)
                },
            )
            .unwrap()
    }
}
fn warm_move(
    session: &CollaborativeDocument,
    source: BlockId,
    destination: BlockId,
    item: Option<BlockId>,
) {
    session.document().find_block_by_id(source).unwrap();
    session.document().find_block_by_id(destination).unwrap();
    if let Some(item) = item {
        session.document().find_list_item_by_id(item).unwrap();
    }
}
pub fn cycle2_move_history(c: &mut Criterion) {
    move_history(Some(c), true);
}
fn move_history(mut criterion: Option<&mut Criterion>, probes: bool) {
    let mut group = criterion
        .as_mut()
        .map(|c| c.benchmark_group("cycle2_move_history"));
    let mut cases = 0usize;
    for list in [false, true] {
        if !shape_enabled(if list { "list" } else { "quote" }) {
            continue;
        }
        for count in [1usize, 64] {
            for history in [0usize, 1, 32, 256] {
                if !selected(count, None, Some(history)) {
                    continue;
                }
                let draft = container_draft(list, count, 0);
                let mut base = construct(&draft);
                let source = base.document().blocks().iter().next().unwrap().id;
                let source_elem = base.document().block_elem_id(source).unwrap();
                let item = list.then(|| {
                    base.document()
                        .list_items(source)
                        .unwrap()
                        .iter()
                        .last()
                        .unwrap()
                        .id
                });
                let destination_elem = base
                    .insert_draft_in(None, Some(source_elem), &draft, Default::default())
                    .unwrap();
                let destination = md_crdt::block_id_from_op(destination_elem);
                for step in 0..history {
                    move_once(&mut base, source, destination, item, step);
                }
                let snapshot = checkpoint(&mut base);
                assert_eq!(
                    snapshot.document.block_topology.moves.len(),
                    if list { 0 } else { history }
                );
                assert_eq!(
                    snapshot.document.block_topology.item_moves.len(),
                    if list { history } else { 0 }
                );
                let label = format!(
                    "{}/{count}/history-{history}",
                    if list { "item" } else { "quote" }
                );
                metadata(&format!("cycle2_move_history/{label}"), &snapshot);
                for warm in [false, true] {
                    let regime = if warm { "warm" } else { "cold" };
                    let prepare = || {
                        let session = cold_session(&base);
                        if warm {
                            warm_move(&session, source, destination, item);
                        }
                        session
                    };
                    let mut proof = prepare();
                    let operation_id = probe(
                        probes,
                        &format!("cycle2_move_history/{regime}/{label}"),
                        || {
                            black_box(move_once(
                                black_box(&mut proof),
                                black_box(source),
                                black_box(destination),
                                black_box(item),
                                black_box(history),
                            ))
                        },
                    );
                    let rendered = proof.document().serialize(EquivalenceMode::Structural);
                    assert_eq!(rendered.matches(&format!("p-{}", count - 1)).count(), 2);
                    if let Some(item) = item {
                        let expected = if history % 2 == 0 {
                            destination
                        } else {
                            source
                        };
                        assert_eq!(
                            proof.document().list_containing_item(item).unwrap().0,
                            expected
                        );
                        assert_eq!(
                            proof.document().find_list_item_by_id(item).unwrap().elem_id,
                            operation_id
                        );
                    } else {
                        assert_eq!(
                            proof.document().block_parent(source).unwrap(),
                            if history % 2 == 0 {
                                Some(destination_elem)
                            } else {
                                None
                            }
                        );
                        assert_eq!(proof.document().block_elem_id(source), Some(operation_id));
                    }
                    roundtrip(&proof);
                    let after = proof.save_snapshot().unwrap();
                    assert_eq!(
                        after.document.block_topology.moves.len(),
                        if list { 0 } else { history + 1 }
                    );
                    assert_eq!(
                        after.document.block_topology.item_moves.len(),
                        if list { history + 1 } else { 0 }
                    );
                    cases += 1;
                    if let Some(group) = &mut group {
                        group.throughput(Throughput::Elements(1));
                        group.bench_function(format!("{regime}/{label}"), |b| {
                            b.iter_batched_ref(
                                prepare,
                                |session| {
                                    black_box(move_once(
                                        black_box(session),
                                        black_box(source),
                                        black_box(destination),
                                        black_box(item),
                                        black_box(history),
                                    ))
                                },
                                BatchSize::PerIteration,
                            );
                        });
                    }
                }
            }
        }
    }
    assert!(cases > 0, "move filters selected no cases");
    if let Some(group) = group {
        group.finish();
    }
    emit(
        json!({"kind": "structure_group_complete", "dataset": DATASET, "group": "cycle2_move_history", "cases": cases}),
    );
}

pub fn cycle2_deletion_context(c: &mut Criterion) {
    deletion_context(Some(c), true);
}
fn deletion_context(mut criterion: Option<&mut Criterion>, probes: bool) {
    let mut group = criterion
        .as_mut()
        .map(|c| c.benchmark_group("cycle2_deletion_context"));
    let mut cases = 0usize;
    for list in [false, true] {
        if !shape_enabled(if list { "list" } else { "quote" }) {
            continue;
        }
        for count in [1usize, 32, 256, 1_024] {
            for history in [0usize, 32] {
                if !selected(count, None, Some(history)) {
                    continue;
                }
                let mut base = construct(&container_draft(list, count, 0));
                let source = base.document().blocks().iter().next().unwrap().id;
                let source_element = base.document().block_elem_id(source).unwrap();
                let tail = base
                    .insert_paragraph(Some(source_element), "surviving-tail")
                    .unwrap();
                for step in 0..history {
                    base.move_block(source, None, (step % 2 == 0).then_some(tail))
                        .unwrap();
                }
                let target = base.document().block_elem_id(source).unwrap();
                let snapshot = checkpoint(&mut base);
                assert_eq!(snapshot.document.block_topology.moves.len(), history);
                let label = format!(
                    "{}/{count}/history-{history}",
                    if list { "list" } else { "quote" }
                );
                metadata(
                    &format!("cycle2_deletion_context/before/{label}"),
                    &snapshot,
                );
                for warm in [false, true] {
                    let regime = if warm { "warm" } else { "cold" };
                    let prepare = || {
                        let session = cold_session(&base);
                        if warm {
                            session.document().find_block(target).unwrap();
                        }
                        session
                    };
                    let mut proof = prepare();
                    let _ = probe(
                        probes,
                        &format!("cycle2_deletion_context/{regime}/{label}"),
                        || black_box(proof.delete_block(black_box(target)).unwrap()),
                    );
                    assert_eq!(
                        proof.document().serialize(EquivalenceMode::Structural),
                        "surviving-tail"
                    );
                    assert!(proof.document().find_block_by_id(source).is_none());
                    let payload_bytes = proof
                        .encode_changes_since(&snapshot.state_vector)
                        .unwrap()
                        .ops
                        .iter()
                        .map(|operation| operation.payload.len())
                        .sum::<usize>();
                    let after = checkpoint(&mut proof);
                    assert_eq!(after.document.block_topology.moves.len(), history);
                    assert_eq!(after.document.block_topology.deletions.len(), 1);
                    assert_eq!(
                        after.document.block_topology.deletions[0].subtree.len(),
                        1 + count * if list { 2 } else { 1 }
                    );
                    roundtrip(&proof);
                    metadata(&format!("cycle2_deletion_context/{regime}/{label}"), &after);
                    emit(
                        json!({"kind": "structure_delete_payload", "dataset": DATASET,
                        "name": format!("{regime}/{label}"), "payload_bytes": payload_bytes}),
                    );
                    cases += 1;
                    if let Some(group) = &mut group {
                        group.throughput(Throughput::Elements(1));
                        group.bench_function(format!("{regime}/{label}"), |b| {
                            b.iter_batched_ref(
                                prepare,
                                |session| {
                                    black_box(session.delete_block(black_box(target)).unwrap())
                                },
                                BatchSize::PerIteration,
                            );
                        });
                    }
                }
            }
        }
    }
    assert!(cases > 0, "deletion filters selected no cases");
    if let Some(group) = group {
        group.finish();
    }
    emit(
        json!({"kind": "structure_group_complete", "dataset": DATASET, "group": "cycle2_deletion_context", "cases": cases}),
    );
}

/// Native correctness/probe modes share the actual fixtures and untimed proofs
/// with Criterion. They never initialize or execute unrelated benchmark groups.
pub fn dispatch(arguments: &[String]) -> bool {
    let Some(mode) = arguments.get(1).map(String::as_str) else {
        return false;
    };
    if !matches!(mode, "--structure-smoke" | "--structure-probe") {
        return false;
    }
    let probes = mode == "--structure-probe";
    if let Ok(selected) = std::env::var("MD_CRDT_BENCH_GROUPS") {
        assert!(
            selected.split(',').all(|name| GROUPS.contains(&name)),
            "structure mode requires structure group names"
        );
    }
    let mut groups = 0usize;
    for name in GROUPS {
        if !group_enabled(name) {
            continue;
        }
        match name {
            "cycle2_lookup" => lookup(None, probes),
            "cycle2_structure" => structure(None, probes),
            "cycle2_move_history" => move_history(None, probes),
            "cycle2_deletion_context" => deletion_context(None, probes),
            _ => unreachable!(),
        }
        groups += 1;
    }
    assert!(groups > 0, "structure mode selected no groups");
    emit(json!({"kind": "structure_complete", "dataset": DATASET, "mode": mode, "groups": groups}));
    true
}
