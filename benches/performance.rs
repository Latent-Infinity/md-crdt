use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use md_crdt::CollaborativeDocument;
use md_crdt::core::{OpId, Sequence, SequenceOp, StateVector};
use md_crdt::doc::{
    Block, BlockKind, Document, EquivalenceMode, Parser, TextUnit, block_id_from_op,
    units_from_str_at,
};
use md_crdt::sync::{Operation, SyncState};
use std::time::{Duration, Instant};

fn op(counter: u64, peer: u64) -> OpId {
    OpId { counter, peer }
}

fn document_with_blocks(count: usize) -> Document {
    let mut document = Document::new();
    let mut after = None;
    for counter in 1..=count as u64 {
        let id = op(counter, 1);
        document.insert_block_at(
            None,
            after,
            id,
            Block::new(
                BlockKind::Paragraph {
                    text: Sequence::new(),
                },
                id,
            ),
            None,
        );
        after = Some(id);
    }
    document
}

fn sync_with_ops(count: usize, peers: usize, payload_size: usize) -> SyncState {
    let mut sync = SyncState::new();
    for index in 0..count {
        let peer = (index % peers) as u64 + 1;
        let counter = (index / peers) as u64 + 1;
        sync.apply_op(Operation {
            id: op(counter, peer),
            payload: vec![0; payload_size].into(),
        });
    }
    sync
}

fn block_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_id_lookup");
    for count in [1_000usize, 10_000] {
        let document = document_with_blocks(count);
        let target = block_id_from_op(op(count as u64, 1));
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| black_box(document.find_block_by_id(black_box(target))))
        });
    }
    group.finish();
}

fn state_vector(c: &mut Criterion) {
    let mut group = c.benchmark_group("state_vector");
    for (ops, peers) in [(10_000usize, 10usize), (10_000, 1_000)] {
        let sync = sync_with_ops(ops, peers, 8);
        group.throughput(Throughput::Elements(ops as u64));
        group.bench_with_input(
            BenchmarkId::new("ops_peers", format!("{ops}_{peers}")),
            &(ops, peers),
            |b, _| b.iter(|| black_box(sync.state_vector())),
        );
    }
    group.finish();
}

fn encode_changes(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode_changes_since");
    for payload_size in [32usize, 1_024] {
        let sync = sync_with_ops(10_000, 10, payload_size);
        let since = StateVector::new();
        group.throughput(Throughput::Bytes((10_000 * payload_size) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(payload_size),
            &payload_size,
            |b, _| b.iter(|| black_box(sync.encode_changes_since(black_box(&since)))),
        );
    }
    group.finish();
}

fn sequence_insert_middle(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_insert_middle");
    for count in [1_000usize, 10_000] {
        let items = (1..=count as u64).map(|counter| (op(counter, 1), counter));
        let base = Sequence::from_ordered(items.collect());
        let after = Some(op(count as u64 / 2, 1));
        let right_origin = base.compute_right_origin(after);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let mut sequence = base.clone();
                    let start = Instant::now();
                    sequence.apply(SequenceOp::Insert {
                        after,
                        id: op(1, 2),
                        value: 0,
                        right_origin,
                    });
                    elapsed += start.elapsed();
                    black_box(sequence);
                }
                elapsed
            });
        });
    }
    group.finish();
}

fn nested_text_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_text_insert");
    for count in [1_000usize, 10_000] {
        let base = units_from_str_at(&"x".repeat(count), op(1, 1));
        let after = Some(op(count as u64 / 2, 1));
        let right_origin = base.compute_right_origin(after);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let mut sequence = base.clone();
                    let start = Instant::now();
                    sequence.apply(SequenceOp::Insert {
                        after,
                        id: op(1, 2),
                        value: TextUnit {
                            grapheme: "y".into(),
                        },
                        right_origin,
                    });
                    elapsed += start.elapsed();
                    black_box(sequence);
                }
                elapsed
            });
        });
    }
    group.finish();
}

fn session_insert_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("session_insert_text");
    for count in [1_000usize, 10_000] {
        let mut base = CollaborativeDocument::new(1);
        let block_elem = base.insert_paragraph(None, &"x".repeat(count)).unwrap();
        let block_id = block_id_from_op(block_elem);
        let snapshot = base.save_snapshot().unwrap();
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let mut session =
                        CollaborativeDocument::restore_from_snapshot(snapshot.clone()).unwrap();
                    let start = Instant::now();
                    let inserted = session.insert_text(block_id, count / 2, "y").unwrap();
                    elapsed += start.elapsed();
                    black_box((session, inserted));
                }
                elapsed
            });
        });
    }
    group.finish();
}

fn document_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("document_serialize");
    for count in [1_000usize, 10_000] {
        let document = Parser::parse(&"x".repeat(count));
        group.throughput(Throughput::Bytes(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| black_box(document.serialize(EquivalenceMode::Structural)))
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    block_lookup,
    state_vector,
    encode_changes,
    sequence_insert_middle,
    nested_text_insert,
    session_insert_text,
    document_serialize
);
criterion_main!(benches);
