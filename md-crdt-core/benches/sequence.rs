use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use md_crdt_core::{OpId, Sequence, SequenceOp};

/// Create a sequence with N elements for benchmarking
fn create_sequence(size: usize) -> Sequence<u64> {
    let mut seq = Sequence::new();
    let mut prev = None;
    for i in 0..size {
        let id = OpId {
            counter: i as u64 + 1,
            peer: 1,
        };
        seq.insert(prev, i as u64, id);
        prev = Some(id);
    }
    seq
}

fn bench_insert_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_start");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let mut seq = create_sequence(size);
            let mut counter = size as u64 + 1;
            b.iter(|| {
                let id = OpId { counter, peer: 2 };
                counter += 1;
                seq.insert(None, 999, id);
                black_box(&seq);
            });
        });
    }

    group.finish();
}

fn bench_insert_middle(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_middle");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let mut seq = create_sequence(size);
            let ids = seq.element_ids();
            let after = ids.get(size / 2).copied();
            let mut counter = size as u64 + 1;
            b.iter(|| {
                let id = OpId { counter, peer: 2 };
                counter += 1;
                seq.insert(after, 999, id);
                black_box(&seq);
            });
        });
    }

    group.finish();
}

fn bench_insert_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_end");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let mut seq = create_sequence(size);
            let ids = seq.element_ids();
            let after = ids.last().copied();
            let mut counter = size as u64 + 1;
            b.iter(|| {
                let id = OpId { counter, peer: 2 };
                counter += 1;
                seq.insert(after, 999, id);
                black_box(&seq);
            });
        });
    }

    group.finish();
}

fn bench_delete_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_start");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let mut seq = create_sequence(size);
            let target = seq.element_ids()[0];
            let mut counter = size as u64 + 1;
            b.iter(|| {
                seq.delete(target, OpId { counter, peer: 2 });
                counter += 1;
                black_box(&seq);
            });
        });
    }

    group.finish();
}

fn bench_delete_middle(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_middle");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let mut seq = create_sequence(size);
            let target = seq.element_ids()[size / 2];
            let mut counter = size as u64 + 1;
            b.iter(|| {
                seq.delete(target, OpId { counter, peer: 2 });
                counter += 1;
                black_box(&seq);
            });
        });
    }

    group.finish();
}

fn bench_delete_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_end");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let mut seq = create_sequence(size);
            let ids = seq.element_ids();
            let target = *ids.last().unwrap();
            let mut counter = size as u64 + 1;
            b.iter(|| {
                seq.delete(target, OpId { counter, peer: 2 });
                counter += 1;
                black_box(&seq);
            });
        });
    }

    group.finish();
}

fn bench_random_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("random_access");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let seq = create_sequence(size);
            let ids = seq.element_ids();
            b.iter(|| {
                let idx = size / 3;
                let id = &ids[idx];
                let elem = seq.get_element(id);
                black_box(elem);
            });
        });
    }

    group.finish();
}

fn bench_sequential_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_iteration");

    for size in [10usize, 100, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let seq = create_sequence(size);
            b.iter(|| {
                let values: Vec<_> = seq.iter().copied().collect();
                black_box(values);
            });
        });
    }

    group.finish();
}

fn bench_apply_remote_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_remote_ops");

    for size in [10usize, 100, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            // Create "remote" operations to apply
            let ops: Vec<_> = (0..size)
                .map(|i| {
                    let id = OpId {
                        counter: i as u64 + 1,
                        peer: 2,
                    };
                    let after = if i == 0 {
                        None
                    } else {
                        Some(OpId {
                            counter: i as u64,
                            peer: 2,
                        })
                    };
                    SequenceOp::Insert {
                        after,
                        id,
                        value: i as u64,
                        right_origin: None,
                    }
                })
                .collect();

            b.iter(|| {
                let mut seq = Sequence::new();
                for op in &ops {
                    seq.apply(op.clone());
                }
                black_box(&seq);
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_insert_start,
    bench_insert_middle,
    bench_insert_end,
    bench_delete_start,
    bench_delete_middle,
    bench_delete_end,
    bench_random_access,
    bench_sequential_iteration,
    bench_apply_remote_operations
);
criterion_main!(benches);
