use criterion::{Criterion, black_box, criterion_group, criterion_main};
use md_crdt_core::OpId;
use md_crdt_doc::{Block, BlockKind, Document};

fn bench_insert_text(c: &mut Criterion) {
    let mut doc = Document::new();
    let block = Block::new(
        BlockKind::Paragraph {
            text: "Hello world".to_string(),
        },
        OpId {
            counter: 1,
            peer: 0,
        },
    );
    let block_id = block.id;
    doc.blocks.apply_op((block.elem_id, block));

    c.bench_function("insert_text", |b| {
        b.iter(|| {
            let mut working = doc.clone();
            let _ = working.insert_text(
                block_id,
                5,
                "x",
                OpId {
                    counter: 2,
                    peer: 0,
                },
            );
            black_box(working);
        })
    });
}

criterion_group!(benches, bench_insert_text);
criterion_main!(benches);
