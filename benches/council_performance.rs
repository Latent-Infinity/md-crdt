//! Focused council-correction workloads; compare identical source inputs and profiles.
//! Allocation probes execute once outside Criterion's timed loops in this isolated binary.
use criterion::{BatchSize, BenchmarkId, Criterion};
use md_crdt::{EquivalenceMode, OpId, Parser, Sequence, SequenceOp};
#[path = "support/allocations.rs"]
mod allocation_support;
use allocation_support::allocations;
use std::hint::black_box;
#[path = "support/storage_open.rs"]
mod storage_open;
#[path = "support/structure.rs"]
mod structure;
use structure::{cycle2_deletion_context, cycle2_lookup, cycle2_move_history, cycle2_structure};

fn id(counter: u64, peer: u64) -> OpId {
    OpId { counter, peer }
}
fn table(rows: usize, columns: usize) -> String {
    let mut text = format!(
        "{}\n{}\n",
        vec!["head"; columns].join("|"),
        vec!["---"; columns].join("|")
    );
    for row in 0..rows {
        text.push_str(
            &(0..columns)
                .map(|column| format!("r{row}c{column}"))
                .collect::<Vec<_>>()
                .join("|"),
        );
        text.push('\n');
    }
    text
}
fn document_kernels(c: &mut Criterion) {
    let mut group = c.benchmark_group("council_document");
    for count in [100usize, 1_000, 5_000] {
        let source = (0..count)
            .map(|i| format!("paragraph {i}\n\n"))
            .collect::<String>();
        let document = Parser::try_parse(&source).unwrap();
        assert_eq!(document.serialize(EquivalenceMode::Exact), source);
        allocations(&format!("exact/{count}"), || {
            document.serialize(EquivalenceMode::Exact)
        });
        group.bench_with_input(
            BenchmarkId::new("exact", count),
            &document,
            |b, document| {
                b.iter(|| black_box(document.serialize(EquivalenceMode::Exact)));
            },
        );
    }
    for (rows, columns) in [(10, 5), (100, 20)] {
        let source = table(rows, columns);
        let document = Parser::try_parse(&source).unwrap();
        let label = format!("{rows}x{columns}");
        allocations(&format!("table_parse/{label}"), || {
            Parser::try_parse(&source).unwrap()
        });
        allocations(&format!("table_render/{label}"), || {
            document.serialize(EquivalenceMode::Structural)
        });
        group.bench_function(format!("table_parse/{label}"), |b| {
            b.iter(|| black_box(Parser::try_parse(black_box(&source)).unwrap()));
        });
        group.bench_function(format!("table_render/{label}"), |b| {
            b.iter(|| black_box(document.serialize(EquivalenceMode::Structural)));
        });
    }
    for count in [32usize, 256] {
        let source = "plain **bold** and [label](target) ".repeat(count);
        let document = Parser::try_parse(&source).unwrap();
        allocations(&format!("inline_parse/{count}"), || {
            Parser::try_parse(&source).unwrap()
        });
        allocations(&format!("inline_render/{count}"), || {
            document.serialize(EquivalenceMode::Structural)
        });
        group.bench_function(format!("inline_parse/{count}"), |b| {
            b.iter(|| black_box(Parser::try_parse(black_box(&source)).unwrap()));
        });
        group.bench_function(format!("inline_render/{count}"), |b| {
            b.iter(|| black_box(document.serialize(EquivalenceMode::Structural)));
        });
    }
    let base = Parser::parse(&"x".repeat(1_000));
    let block_id = base.blocks_in_order()[0].id;
    for length in [1usize, 128] {
        let text = "y".repeat(length);
        let mut probe = base.clone();
        allocations(&format!("direct_insert/{length}"), || {
            probe.insert_text(block_id, 500, &text, id(1, 20)).unwrap()
        });
        group.bench_function(format!("direct_insert/{length}"), |b| {
            b.iter_batched(
                || base.clone(),
                |mut document| {
                    let result = document
                        .insert_text(block_id, 500, &text, id(1, 20))
                        .unwrap();
                    black_box((document, result));
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}
fn sequence_kernels(c: &mut Criterion) {
    let mut group = c.benchmark_group("council_sequence");
    for count in [1_000usize, 5_000] {
        let base = Sequence::from_ordered(
            (1..=count as u64)
                .map(|counter| (id(counter, 2), counter))
                .collect(),
        );
        group.bench_with_input(
            BenchmarkId::new("concurrent_tail", count),
            &base,
            |b, base| {
                b.iter_batched(
                    || base.clone(),
                    |mut sequence| {
                        sequence.apply(SequenceOp::Insert {
                            after: Some(id(1, 2)),
                            id: id(1, 1),
                            value: 0,
                            right_origin: None,
                        });
                        black_box(sequence);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.bench_function("owned_payload_64KiB", |b| {
        b.iter_batched(
            || vec![7u8; 64 * 1024],
            |value| {
                let mut sequence = Sequence::new();
                sequence.insert(None, value, id(1, 1));
                black_box(sequence);
            },
            BatchSize::SmallInput,
        );
    });
    let value = vec![7u8; 64 * 1024];
    allocations("owned_payload_64KiB", || {
        let mut sequence = Sequence::new();
        sequence.insert(None, value, id(1, 1));
        sequence
    });
    group.finish();
}
fn alignment_kernel(c: &mut Criterion) {
    let mut group = c.benchmark_group("council_alignment");
    for count in [128usize, 1_000] {
        let old = (0..count)
            .map(|i| if i % 7 == 0 { "a" } else { "b" })
            .collect::<Vec<_>>();
        let new = (0..count)
            .map(|i| if i % 11 == 0 { "a" } else { "b" })
            .collect::<Vec<_>>();
        allocations(&format!("lcs/{count}"), || {
            md_crdt::filesync::lcs_steps(&old, &new).unwrap()
        });
        group.bench_function(BenchmarkId::new("lcs", count), |b| {
            b.iter(|| {
                black_box(md_crdt::filesync::lcs_steps(black_box(&old), black_box(&new)).unwrap())
            });
        });
    }
    group.finish();
}
fn snapshot_kernel(c: &mut Criterion) {
    let mut session = md_crdt::CollaborativeDocument::new(1);
    session
        .insert_paragraph(None, &"text ".repeat(1_000))
        .unwrap();
    let snapshot = session.save_snapshot().unwrap();
    allocations("snapshot_encode/5000", || snapshot.to_bytes().unwrap());
    c.bench_function("council_snapshot/encode_5000", |b| {
        b.iter(|| black_box(snapshot.to_bytes().unwrap()))
    });
}
fn mark_kernels(c: &mut Criterion) {
    use md_crdt::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
    use std::collections::BTreeMap;
    let order = (1..=10_000)
        .map(|counter| id(counter, 1))
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("council_marks");
    for intervals in [1usize, 64] {
        let mut marks = MarkSet::new();
        for index in 0..intervals {
            let mark_id = id(index as u64 + 1, 2);
            marks.set_mark(
                mark_id,
                MarkKind::Link,
                Anchor {
                    elem_id: order[0],
                    bias: AnchorBias::Before,
                },
                Anchor {
                    elem_id: *order.last().unwrap(),
                    bias: AnchorBias::After,
                },
                BTreeMap::from([("href".into(), MarkValue::String("target".repeat(40)))]),
                mark_id,
            );
        }
        let spans = marks.render_spans(&order, order.len());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].marks.len(), intervals);
        allocations(&format!("mark_render/{intervals}"), || {
            marks.render_spans(&order, order.len())
        });
        allocations(&format!("mark_encode/{intervals}"), || {
            serde_json::to_vec(&marks).unwrap()
        });
        group.bench_function(BenchmarkId::new("render_10000", intervals), |b| {
            b.iter(|| black_box(marks.render_spans(black_box(&order), order.len())));
        });
        group.bench_function(BenchmarkId::new("encode", intervals), |b| {
            b.iter(|| black_box(serde_json::to_vec(black_box(&marks)).unwrap()));
        });
    }
    group.finish();
}

fn admission_base(units: usize, pending: bool) -> md_crdt::CollaborativeDocument {
    use md_crdt::codec::{DocOp, Envelope, OpBody, WIRE_VERSION};
    use md_crdt::session::snapshot::{BlockKindDto, SequenceOpDto};
    let mut source = md_crdt::CollaborativeDocument::new(1);
    let block_elem = source.insert_paragraph(None, &"x".repeat(units)).unwrap();
    if !pending {
        return source;
    }
    // Consistent persisted state of a received deletion waiting for its target.
    // Use real snapshot validation/restore, not a mock or private session mutation.
    let delete_id = id(1, 2);
    let missing = id(1, 99);
    let envelope = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::DeleteText {
            block_elem,
            block_id: md_crdt::block_id_from_op(block_elem),
            id: delete_id,
            targets: vec![missing],
        }),
    };
    let mut snapshot = source.save_snapshot().unwrap();
    snapshot
        .ops
        .push((delete_id, serde_json::to_vec(&envelope).unwrap()));
    snapshot.state_vector.set(delete_id.peer, delete_id.counter);
    let block = snapshot
        .document
        .blocks
        .elements
        .iter_mut()
        .filter_map(|element| element.value.as_mut())
        .find(|block| block.elem_id == block_elem)
        .unwrap();
    let BlockKindDto::Paragraph { units } = &mut block.kind else {
        unreachable!()
    };
    units.pending.push(SequenceOpDto::Delete {
        target: missing,
        id: delete_id,
    });
    md_crdt::CollaborativeDocument::restore_from_snapshot(snapshot).unwrap()
}
fn empty_message() -> md_crdt::sync::ChangeMessage {
    md_crdt::sync::ChangeMessage {
        since: md_crdt::StateVector::new(),
        ops: Vec::new(),
    }
}
fn pending_delivery(units: usize) -> md_crdt::sync::ChangeMessage {
    use md_crdt::codec::{DocOp, Envelope, OpBody, TextUnitWire, WIRE_VERSION};
    let target = id(1, 99);
    let block_elem = id(1, 1);
    let envelope = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertText {
            block_elem,
            block_id: md_crdt::block_id_from_op(block_elem),
            units: vec![TextUnitWire {
                id: target,
                after: Some(id(units as u64 + 1, 1)),
                right_origin: None,
                grapheme: "z".into(),
            }],
        }),
    };
    md_crdt::sync::ChangeMessage {
        since: md_crdt::StateVector::new(),
        ops: vec![md_crdt::sync::Operation {
            id: target,
            payload: serde_json::to_vec(&envelope).unwrap().into(),
        }],
    }
}
fn remediation_admission(c: &mut Criterion) {
    use criterion::Throughput;
    const EMPTY_BATCH: usize = 8;
    let limits = md_crdt::ValidationLimits::default();
    let mut group = c.benchmark_group("remediation_admission");
    for units in [1_000usize, 10_000, 100_000] {
        for pending in [false, true] {
            let mut base = admission_base(units, pending);
            let label = format!(
                "empty_{}/{units}",
                if pending { "pending" } else { "ready" }
            );
            allocations(&label, || {
                base.apply_remote(empty_message(), &limits).unwrap()
            });
            let expected = "x".repeat(units);
            assert_eq!(
                base.document().serialize(EquivalenceMode::Structural),
                expected
            );
            group.throughput(Throughput::Elements(EMPTY_BATCH as u64));
            group.bench_function(format!("{label}_batch{EMPTY_BATCH}"), |b| {
                b.iter(|| {
                    for _ in 0..EMPTY_BATCH {
                        black_box(base.apply_remote(empty_message(), &limits).unwrap());
                    }
                });
            });
        }
        let base = admission_base(units, true);
        let delivery = pending_delivery(units);
        let mut probe = base.clone();
        let allocation_delivery = delivery.clone();
        allocations(&format!("dependency_delivery/{units}"), || {
            probe.apply_remote(allocation_delivery, &limits).unwrap()
        });
        assert_eq!(
            probe.document().serialize(EquivalenceMode::Structural),
            "x".repeat(units)
        );
        let restored =
            md_crdt::CollaborativeDocument::restore_from_snapshot(probe.save_snapshot().unwrap())
                .unwrap();
        assert_eq!(
            restored.document().serialize(EquivalenceMode::Structural),
            "x".repeat(units)
        );
        group.throughput(Throughput::Elements(1));
        group.bench_function(format!("dependency_delivery/{units}"), |b| {
            b.iter_batched(
                || (base.clone(), delivery.clone()),
                |(mut target, message)| {
                    let outcome = target.apply_remote(message, &limits).unwrap();
                    black_box((target, outcome))
                },
                BatchSize::PerIteration,
            );
        });
    }
    for operations in [1usize, 64, 512] {
        let mut source = md_crdt::CollaborativeDocument::new(1);
        let elem = source.insert_paragraph(None, "base").unwrap();
        let block = md_crdt::block_id_from_op(elem);
        let frontier = source.state_vector();
        let mut base = source.clone();
        base.rebind_peer(2);
        for index in 0..operations {
            source.insert_text(block, 4 + index * 6, "abcdef").unwrap();
        }
        let message = source.encode_changes_since(&frontier).unwrap();
        let mut probe = base.clone();
        let result = probe.apply_remote(message.clone(), &limits).unwrap();
        assert!(result.buffered.is_empty());
        assert_eq!(
            probe.document().serialize(EquivalenceMode::Structural),
            format!("base{}", "abcdef".repeat(operations))
        );
        let mut allocation_target = base.clone();
        let allocation_message = message.clone();
        allocations(&format!("ready_batch/{operations}"), || {
            allocation_target
                .apply_remote(allocation_message, &limits)
                .unwrap()
        });
        group.throughput(Throughput::Elements(operations as u64));
        group.bench_function(format!("ready_batch/{operations}"), |b| {
            b.iter_batched(
                || (base.clone(), message.clone()),
                |(mut target, message)| {
                    let result = target.apply_remote(message, &limits).unwrap();
                    black_box((target, result))
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}
fn remediation_held(c: &mut Criterion) {
    use md_crdt::doc::{BlockKind, CodeFenceStyle};
    const EMPTY_BATCH: usize = 8;
    let limits = md_crdt::ValidationLimits::default();
    let mut group = c.benchmark_group("remediation_held");
    for operations in [1usize, 8, 16] {
        let mut creator = md_crdt::CollaborativeDocument::new(77);
        let elem = creator
            .insert_block(
                None,
                BlockKind::CodeFence {
                    style: CodeFenceStyle::default(),
                    info: None,
                    text: String::new(),
                },
            )
            .unwrap();
        let block = md_crdt::block_id_from_op(elem);
        let mut writer = creator.clone();
        writer.rebind_peer(78);
        let frontier = writer.state_vector();
        for index in 0..operations {
            writer
                .set_code_fence(
                    block,
                    CodeFenceStyle::default(),
                    None,
                    format!("{index}:{}", "q".repeat(4096)),
                )
                .unwrap();
        }
        let message = writer.encode_changes_since(&frontier).unwrap();
        let mut held = md_crdt::CollaborativeDocument::new(79);
        let result = held.apply_remote(message, &limits).unwrap();
        assert_eq!(result.buffered.len(), operations);
        for restored in [false, true] {
            let mut base = if restored {
                md_crdt::CollaborativeDocument::restore_from_snapshot(held.save_snapshot().unwrap())
                    .unwrap()
            } else {
                held.clone()
            };
            let before = base.state_vector();
            let label = format!(
                "empty_{}/{operations}",
                if restored { "restored" } else { "live" }
            );
            allocations(&label, || {
                base.apply_remote(empty_message(), &limits).unwrap()
            });
            assert_eq!(base.state_vector(), before);
            assert_eq!(base.document().serialize(EquivalenceMode::Structural), "");
            group.bench_function(format!("{label}_batch{EMPTY_BATCH}"), |b| {
                b.iter(|| {
                    for _ in 0..EMPTY_BATCH {
                        black_box(base.apply_remote(empty_message(), &limits).unwrap());
                    }
                });
            });
            base.apply_remote(
                creator
                    .encode_changes_since(&md_crdt::StateVector::new())
                    .unwrap(),
                &limits,
            )
            .unwrap();
            let BlockKind::CodeFence { text, .. } =
                &base.document().find_block_by_id(block).unwrap().kind
            else {
                unreachable!()
            };
            assert_eq!(text, &format!("{}:{}", operations - 1, "q".repeat(4096)));
        }
    }
    group.finish();
}
fn matching_workload(
    blocks: usize,
    common: bool,
) -> (
    md_crdt::filesync::LastFlushedState,
    Vec<md_crdt::filesync::ParsedBlock>,
) {
    let words = (0..45)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let before = (0..blocks)
        .map(|index| {
            if common {
                format!("Paragraph{index} {words}.")
            } else {
                (0..12)
                    .map(|word| format!("paragraph{index}word{word}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let after = if common {
        before.replacen("word0", "replacement0", 1)
    } else {
        before.replacen("paragraph0word0", "replacement0", 1)
    };
    let old = Parser::try_parse(&before).unwrap();
    let new = Parser::try_parse(&after).unwrap();
    (
        md_crdt::filesync::LastFlushedState {
            content_hash: 0,
            blocks: md_crdt::filesync::fingerprint_document(&old),
        },
        md_crdt::filesync::parsed_blocks_from_doc(&new),
    )
}
fn remediation_matching(c: &mut Criterion) {
    use md_crdt::filesync::{MatchConfig, Score, match_blocks};
    let config = MatchConfig {
        min_match_score: Score(5000),
        ..Default::default()
    };
    let mut group = c.benchmark_group("remediation_matching");
    for blocks in [16usize, 64, 128] {
        for common in [false, true] {
            let (old, new) = matching_workload(blocks, common);
            let result = match_blocks(&old, &new, &config).unwrap();
            assert_eq!(result.matched.len(), blocks);
            for matched in &result.matched {
                assert_eq!(matched.old_id, old.blocks[matched.new_index].block_id);
            }
            let label = format!("{}/{blocks}", if common { "common" } else { "rare" });
            allocations(&format!("matching/{label}"), || {
                match_blocks(&old, &new, &config).unwrap()
            });
            group.throughput(criterion::Throughput::Elements(blocks as u64));
            group.bench_function(label, |b| {
                b.iter(|| {
                    black_box(match_blocks(black_box(&old), black_box(&new), &config).unwrap())
                })
            });
        }
    }
    group.finish();
}
fn storage_fixture(bytes: usize) -> (tempfile::TempDir, md_crdt::storage::Storage, Vec<u8>) {
    let directory = tempfile::tempdir().unwrap();
    let storage = md_crdt::storage::Storage::open(directory.path()).unwrap();
    let mut state = 0x729e_14c3_5b60_a91cu64;
    let payload = (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect::<Vec<_>>();
    storage.write_snapshot(&payload, &[], false).unwrap();
    storage.write_snapshot(&payload, &[], false).unwrap();
    storage
        .append_op_segment(b"acknowledged operation payload")
        .unwrap();
    (directory, storage, payload)
}
fn remediation_storage(c: &mut Criterion) {
    use md_crdt::storage::TombstoneRetention;
    let mut group = c.benchmark_group("remediation_storage");
    for bytes in [1usize << 20, 8 << 20] {
        let (_directory, storage, payload) = storage_fixture(bytes);
        allocations(&format!("storage_compact/{bytes}"), || {
            storage
                .compact(
                    &payload,
                    &[],
                    false,
                    TombstoneRetention::KeepAll,
                    &[1, 2, 3],
                )
                .unwrap()
        });
        assert_eq!(storage.read_snapshot().unwrap().0, payload);
        assert!(storage.read_op_segments().unwrap().is_empty());
        assert_eq!(storage.read_tombstones().unwrap(), vec![1, 2, 3]);
        group.throughput(criterion::Throughput::Bytes(bytes as u64));
        group.bench_function(BenchmarkId::new("compact", bytes), |b| {
            b.iter_batched(
                || storage_fixture(bytes),
                |(directory, storage, payload)| {
                    let report = storage
                        .compact(
                            &payload,
                            &[],
                            false,
                            TombstoneRetention::KeepAll,
                            &[1, 2, 3],
                        )
                        .unwrap();
                    black_box((directory, storage, payload, report))
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}
fn storage_io_probe(bytes: usize) {
    let (directory, storage, payload) = storage_fixture(bytes);
    println!("STORAGE_MEASURE_BEGIN root={}", directory.path().display());
    let report = storage
        .compact(
            &payload,
            &[],
            false,
            md_crdt::storage::TombstoneRetention::KeepAll,
            &[1, 2, 3],
        )
        .unwrap();
    println!(
        "STORAGE_MEASURE_END archived_segments={} archived_ops={}",
        report.archived_segments, report.archived_ops
    );
    assert_eq!(storage.read_snapshot().unwrap().0, payload);
    assert!(storage.read_op_segments().unwrap().is_empty());
}

/// P2 baseline: public restore APIs over materialized text and held envelopes.
/// Fixture construction, encoding, storage publication and semantic assertions
/// stay outside the timed/allocation boundaries.
fn cycle2_restore(c: &mut Criterion) {
    use md_crdt::codec::{DocOp, JsonOpCodec, OpBody, OpCodec};
    use md_crdt::{
        BlockDraft, CollaborativeDocument, SessionSnapshot, StateVector, StructuredEditLimits,
        ValidationLimits,
    };
    let mut group = c.benchmark_group("cycle2_restore");
    for (units, held, deferred) in [
        (0usize, 0usize, false),
        (100, 0, false),
        (10_000, 0, false),
        (100, 16, false),
        (10_000, 128, false),
        (100, 16, true),
        (10_000, 128, true),
    ] {
        let mut session = CollaborativeDocument::new(1);
        if units != 0 {
            session.insert_paragraph(None, &"é".repeat(units)).unwrap();
        }
        for index in 0..held {
            let mut prerequisite = CollaborativeDocument::new(100 + index as u64);
            let parent = prerequisite
                .insert_draft_in(
                    None,
                    None,
                    &BlockDraft::BlockQuote {
                        children: Vec::new(),
                    },
                    StructuredEditLimits::default(),
                )
                .unwrap();
            let mut producer = if deferred {
                CollaborativeDocument::restore_from_snapshot(prerequisite.save_snapshot().unwrap())
                    .unwrap()
            } else {
                prerequisite.clone()
            };
            if deferred {
                producer.rebind_peer(10_000 + index as u64);
            }
            producer
                .insert_draft_in(
                    Some(parent),
                    None,
                    &BlockDraft::RawBlock {
                        raw: "held".repeat(1024),
                    },
                    StructuredEditLimits::default(),
                )
                .unwrap();
            let mut message = producer.encode_changes_since(&StateVector::new()).unwrap();
            message.ops.retain(|operation| {
                matches!(
                    JsonOpCodec.decode(&operation.payload).unwrap().body,
                    OpBody::Doc(DocOp::InsertBlock {
                        parent: Some(_),
                        ..
                    })
                )
            });
            session
                .apply_remote(message, &ValidationLimits::default())
                .unwrap();
        }
        let snapshot = session.save_snapshot().unwrap();
        assert_eq!(snapshot.pending.len(), if deferred { 0 } else { held });
        assert_eq!(snapshot.deferred.len(), if deferred { held } else { 0 });
        let bytes = snapshot.to_bytes().unwrap();
        let expected = session.document().serialize(EquivalenceMode::Structural);
        let restored = CollaborativeDocument::restore_from_snapshot_bytes(&bytes).unwrap();
        assert_eq!(
            restored.document().serialize(EquivalenceMode::Structural),
            expected
        );
        assert_eq!(restored.save_snapshot().unwrap(), snapshot);
        let label = format!(
            "units{units}_held{held}_{}",
            if deferred { "deferred" } else { "pending" }
        );
        println!(
            "restore_fixture name={label} stored_bytes={} json_bytes={} held_payload_bytes={}",
            bytes.len(),
            serde_json::to_vec(&snapshot).unwrap().len(),
            held * 4096
        );
        allocations(&format!("restore_bytes/{label}"), || {
            CollaborativeDocument::restore_from_snapshot_bytes(&bytes).unwrap()
        });
        let dto = snapshot.clone();
        allocations(&format!("restore_dto/{label}"), || {
            CollaborativeDocument::restore_from_snapshot(dto).unwrap()
        });
        if std::env::var_os("MD_CRDT_RESTORE_INPUT_MEMORY").is_some() {
            let (result, stats) = allocation_support::capture_with_input(
                || snapshot.clone(),
                |dto| CollaborativeDocument::restore_from_snapshot(dto).unwrap(),
            );
            println!(
                "restore_input_memory name={label} input_live_bytes={} operation_calls={} operation_requested_bytes={} peak_tracked_live_bytes={} retained_tracked_live_bytes={}",
                stats.input_live_bytes,
                stats.operation_calls,
                stats.operation_requested_bytes,
                stats.peak_tracked_live_bytes,
                stats.retained_tracked_live_bytes,
            );
            assert_eq!(result.save_snapshot().unwrap(), snapshot);
            black_box(result);
        }
        let directory = tempfile::tempdir().unwrap();
        let storage = md_crdt::storage::Storage::open(directory.path()).unwrap();
        session.write_to_storage(&storage).unwrap();
        allocations(&format!("restore_storage/{label}"), || {
            CollaborativeDocument::read_from_storage(&storage).unwrap()
        });
        group.bench_function(format!("bytes/{label}"), |b| {
            b.iter(|| {
                black_box(
                    CollaborativeDocument::restore_from_snapshot_bytes(black_box(&bytes)).unwrap(),
                )
            });
        });
        group.bench_function(format!("dto/{label}"), |b| {
            b.iter_batched(
                || snapshot.clone(),
                |snapshot| {
                    black_box(CollaborativeDocument::restore_from_snapshot(snapshot).unwrap())
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function(format!("decode/{label}"), |b| {
            b.iter(|| black_box(SessionSnapshot::from_bytes(black_box(&bytes)).unwrap()));
        });
        group.bench_function(format!("read_from_storage/{label}"), |b| {
            b.iter(|| {
                black_box(CollaborativeDocument::read_from_storage(black_box(&storage)).unwrap())
            });
        });
    }
    group.finish();
}

#[derive(Clone, Copy)]
struct SnapshotPrefixCodec;

impl md_crdt::codec::OpCodec for SnapshotPrefixCodec {
    type Error = md_crdt::codec::CodecError;

    fn encode(&self, envelope: &md_crdt::codec::Envelope) -> Result<Vec<u8>, Self::Error> {
        let mut payload = b"PRE".to_vec();
        payload.extend(md_crdt::codec::JsonOpCodec.encode(envelope)?);
        Ok(payload)
    }

    fn decode(&self, payload: &[u8]) -> Result<md_crdt::codec::Envelope, Self::Error> {
        md_crdt::codec::JsonOpCodec.decode(
            payload
                .strip_prefix(b"PRE")
                .ok_or(Self::Error::Invalid("missing prefix"))?,
        )
    }

    fn snapshot_payload<'a>(
        &self,
        payload: &'a [u8],
        _envelope: Option<&md_crdt::codec::Envelope>,
    ) -> Result<std::borrow::Cow<'a, [u8]>, md_crdt::codec::CodecError> {
        Ok(std::borrow::Cow::Borrowed(
            payload
                .strip_prefix(b"PRE")
                .ok_or(Self::Error::Invalid("missing prefix"))?,
        ))
    }

    fn snapshot_payload_len(
        &self,
        payload: &[u8],
        _envelope: &md_crdt::codec::Envelope,
    ) -> Result<usize, md_crdt::codec::CodecError> {
        Ok(payload
            .strip_prefix(b"PRE")
            .ok_or(Self::Error::Invalid("missing prefix"))?
            .len())
    }
}

fn snapshot_save_fixture<C: md_crdt::codec::OpCodec>(
    codec: C,
    units: usize,
    raw_bytes: usize,
) -> md_crdt::CollaborativeDocument<C> {
    let mut session = md_crdt::CollaborativeDocument::with_codec(1, codec, true);
    let paragraph = session.insert_paragraph(None, &"é".repeat(units)).unwrap();
    session
        .insert_draft_in(
            None,
            Some(paragraph),
            &md_crdt::BlockDraft::RawBlock {
                raw: "\\\n".repeat(raw_bytes / 2),
            },
            md_crdt::StructuredEditLimits::default(),
        )
        .unwrap();
    session
}

/// Matched correctness baseline for any later borrowed-log iteration study.
/// Timed work is save_snapshot only, not compression, restore or retransmission.
fn snapshot_save_codec<C: md_crdt::codec::OpCodec + Clone>(
    c: &mut Criterion,
    codec: C,
    name: &str,
) {
    use md_crdt::codec::{JsonOpCodec, OpCodec};
    use md_crdt::{CollaborativeDocument, StateVector, ValidationLimits};
    let mut group = c.benchmark_group(format!("cycle2_snapshot_save/{name}"));
    for (units, raw_bytes) in [(32usize, 128usize), (10_000, 4096), (1000, 1024 * 1024)] {
        let session = snapshot_save_fixture(codec.clone(), units, raw_bytes);
        let label = format!("units{units}_raw{raw_bytes}");
        {
            let snapshot = session.save_snapshot().unwrap();
            let native = snapshot_save_fixture(JsonOpCodec, units, raw_bytes);
            assert_eq!(snapshot, native.save_snapshot().unwrap());
            for (_, payload) in &snapshot.ops {
                JsonOpCodec.decode(payload).unwrap();
            }
            let canonical_bytes = snapshot
                .ops
                .iter()
                .map(|(_, payload)| payload.len())
                .sum::<usize>();
            println!(
                "snapshot_save_fixture codec={name} name={label} ops={} canonical_log_bytes={canonical_bytes} json_bytes={}",
                snapshot.ops.len(),
                serde_json::to_vec(&snapshot).unwrap().len()
            );
            let restored =
                CollaborativeDocument::restore_from_snapshot_bytes(&snapshot.to_bytes().unwrap())
                    .unwrap();
            assert_eq!(restored.document(), session.document());
            let mut receiver = CollaborativeDocument::new(2);
            receiver
                .apply_remote(
                    restored.encode_changes_since(&StateVector::new()).unwrap(),
                    &ValidationLimits::default(),
                )
                .unwrap();
            assert_eq!(receiver.document(), session.document());
            assert_eq!(receiver.state_vector(), session.state_vector());
        }
        allocations(&format!("snapshot_save/{name}/{label}"), || {
            session.save_snapshot().unwrap()
        });
        group.bench_function(&label, |b| {
            b.iter(|| black_box(session.save_snapshot().unwrap()));
        });
    }
    group.finish();
}

fn cycle2_snapshot_save(c: &mut Criterion) {
    snapshot_save_codec(c, md_crdt::codec::JsonOpCodec, "json");
    snapshot_save_codec(c, md_crdt::codec::BinaryOpCodec, "binary");
    snapshot_save_codec(c, SnapshotPrefixCodec, "borrowed_prefix");
}
fn frontmatter_source_kernels(c: &mut Criterion) {
    use md_crdt::doc::Frontmatter;
    use md_crdt::session::CollaborativeDocument;
    let mut group = c.benchmark_group("frontmatter_source_projection");
    for (padding_bytes, fields) in [(256usize, 1usize), (4096, 32), (65536, 512)] {
        let mut raw = format!(
            "# {}\ntitle: ready # retained comment",
            "x".repeat(padding_bytes)
        );
        for index in 1..fields {
            raw.push_str(&format!("\nkey_{index}: value_{index}"));
        }
        for newline in ["\n", "\r\n"] {
            let source = format!("---\n{raw}\n---\n\nbody\n").replace('\n', newline);
            for scenario in ["adopted", "same_value_write", "edited_write"] {
                let mut session = CollaborativeDocument::new(1);
                session.insert_paragraph(None, "body").unwrap();
                session
                    .initialize_frontmatter(Frontmatter::parse(raw.clone()))
                    .unwrap();
                session
                    .set_frontmatter_field("title", Some("ready".into()))
                    .unwrap();
                for index in 1..fields {
                    session
                        .set_frontmatter_field(
                            format!("key_{index}"),
                            Some(format!("value_{index}")),
                        )
                        .unwrap();
                }
                let mut document = session.document().clone();
                document.set_raw_source(source.clone());
                let mut snapshot = session.save_snapshot().unwrap();
                snapshot.document = md_crdt::session::DocumentDto::from_document(&document);
                drop(document);
                session = CollaborativeDocument::restore_from_snapshot(snapshot).unwrap();
                match scenario {
                    "same_value_write" => {
                        session
                            .set_frontmatter_field("title", Some("ready".into()))
                            .unwrap();
                    }
                    "edited_write" => {
                        session
                            .set_frontmatter_field("title", Some("edited".into()))
                            .unwrap();
                    }
                    _ => {}
                }
                let expected = if scenario == "edited_write" {
                    source.replace("title: ready", "title: edited")
                } else {
                    source.clone()
                };
                assert_eq!(
                    session.document().serialize(EquivalenceMode::Exact),
                    expected
                );
                let label = format!(
                    "{scenario}/{}-bytes/{fields}-fields/{}",
                    source.len(),
                    if newline == "\n" { "lf" } else { "crlf" },
                );
                allocations(&format!("frontmatter/{label}"), || {
                    session.document().serialize(EquivalenceMode::Exact)
                });
                group.throughput(criterion::Throughput::Bytes(source.len() as u64));
                group.bench_function(label, |b| {
                    b.iter(|| {
                        black_box(black_box(session.document()).serialize(EquivalenceMode::Exact))
                    });
                });
            }
        }
    }
    group.finish();
}

fn benches() {
    type Kernel = fn(&mut Criterion);
    let registry: &[(&str, Kernel)] = &[
        ("council_document", document_kernels),
        ("council_sequence", sequence_kernels),
        ("council_alignment", alignment_kernel),
        ("council_snapshot", snapshot_kernel),
        ("council_marks", mark_kernels),
        ("remediation_admission", remediation_admission),
        ("remediation_held", remediation_held),
        ("remediation_matching", remediation_matching),
        ("remediation_storage", remediation_storage),
        ("cycle2_restore", cycle2_restore),
        ("cycle2_snapshot_save", cycle2_snapshot_save),
        ("frontmatter_source_projection", frontmatter_source_kernels),
        ("storage_open", storage_open::benchmarks),
        ("cycle2_lookup", cycle2_lookup),
        ("cycle2_structure", cycle2_structure),
        ("cycle2_move_history", cycle2_move_history),
        ("cycle2_deletion_context", cycle2_deletion_context),
    ];
    if let Ok(selected) = std::env::var("MD_CRDT_BENCH_GROUPS") {
        assert!(
            selected
                .split(',')
                .all(|name| registry.iter().any(|(known, _)| *known == name)),
            "MD_CRDT_BENCH_GROUPS requires comma-separated exact registry names"
        );
    }
    let mut criterion = Criterion::default().configure_from_args();
    for (name, run) in registry {
        if structure::group_enabled(name) {
            run(&mut criterion);
        }
    }
    criterion.final_summary();
}
fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    if structure::dispatch(&arguments) {
        return;
    }
    if storage_open::dispatch(&arguments) {
        return;
    }
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "--io-probe")
    {
        let bytes = arguments
            .get(2)
            .map_or(1 << 20, |value| value.parse().expect("valid probe size"));
        storage_io_probe(bytes);
        return;
    }
    benches();
}
