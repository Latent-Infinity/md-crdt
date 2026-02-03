use criterion::{Criterion, black_box, criterion_group, criterion_main};
use md_crdt_core::OpId;
use md_crdt_sync::{ChangeMessage, Document, Operation};

fn make_message(ops: usize) -> ChangeMessage {
    let mut operations = Vec::with_capacity(ops);
    for i in 0..ops {
        operations.push(Operation {
            id: OpId {
                counter: (i + 1) as u64,
                peer: 1,
            },
            payload: vec![0u8; 8],
        });
    }
    ChangeMessage {
        since: md_crdt_core::StateVector::new(),
        ops: operations,
    }
}

fn bench_merge_remote(c: &mut Criterion) {
    let message = make_message(1_000);
    c.bench_function("merge_remote_1k", |b| {
        b.iter(|| {
            let mut doc = Document::new();
            let result = doc.apply_changes(black_box(message.clone()));
            black_box(result);
        })
    });
}

criterion_group!(benches, bench_merge_remote);
criterion_main!(benches);
