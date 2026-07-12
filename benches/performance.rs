use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use md_crdt::core::{OpId, Sequence, StateVector};
use md_crdt::doc::{Block, BlockKind, Document, block_id_from_op};
use md_crdt::sync::{Operation, SyncState};

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

criterion_group!(benches, block_lookup, state_vector, encode_changes);
criterion_main!(benches);
