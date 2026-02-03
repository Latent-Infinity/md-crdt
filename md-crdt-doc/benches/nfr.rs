use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use md_crdt_core::OpId;
use md_crdt_doc::{Block, BlockKind, Document, EquivalenceMode};

fn generate_markdown_blocks(count: usize) -> String {
    let mut parts = Vec::with_capacity(count);
    for i in 0..count {
        parts.push(format!("Paragraph {i}"));
    }
    parts.join("\n\n")
}

fn build_large_doc(blocks: usize) -> Document {
    let mut doc = Document::new();
    for i in 0..blocks {
        let block = Block::new(
            BlockKind::Paragraph {
                text: format!("Block {i}"),
            },
            OpId {
                counter: i as u64 + 1,
                peer: 1,
            },
        );
        doc.blocks.insert(
            None,
            block,
            OpId {
                counter: i as u64 + 1,
                peer: 1,
            },
        );
    }
    doc
}

fn bench_open_file(c: &mut Criterion) {
    let markdown = generate_markdown_blocks(10_000);
    c.bench_function("open_file_10k", |b| {
        b.iter(|| {
            let doc = md_crdt_doc::Parser::parse(black_box(&markdown));
            black_box(doc);
        })
    });
}

fn bench_apply_keystroke(c: &mut Criterion) {
    let mut doc = Document::new();
    let block = Block::new(
        BlockKind::Paragraph {
            text: "a".repeat(10_000),
        },
        OpId {
            counter: 1,
            peer: 1,
        },
    );
    let block_id = block.id;
    doc.blocks.insert(
        None,
        block,
        OpId {
            counter: 1,
            peer: 1,
        },
    );

    c.bench_function("apply_keystroke", |b| {
        b.iter(|| {
            let mut working = doc.clone();
            let _ = working.insert_text(
                block_id,
                5_000,
                "x",
                OpId {
                    counter: 2,
                    peer: 1,
                },
            );
            black_box(working);
        })
    });
}

fn bench_serialize_markdown(c: &mut Criterion) {
    let doc = build_large_doc(10_000);
    c.bench_function("serialize_markdown_10k", |b| {
        b.iter(|| {
            let output = doc.serialize(EquivalenceMode::Structural);
            black_box(output);
        })
    });
}

fn bench_scale(c: &mut Criterion) {
    let sizes = [1_000usize, 10_000usize];
    let mut group = c.benchmark_group("serialize_scale");
    for size in sizes {
        let doc = build_large_doc(size);
        group.bench_with_input(BenchmarkId::new("serialize", size), &doc, |b, doc| {
            b.iter(|| {
                let output = doc.serialize(EquivalenceMode::Structural);
                black_box(output);
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_open_file,
    bench_apply_keystroke,
    bench_serialize_markdown,
    bench_scale
);
criterion_main!(benches);
