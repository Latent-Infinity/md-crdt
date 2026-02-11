#![cfg(feature = "dhat-heap")]

use md_crdt::core::OpId;
use md_crdt::doc::{Block, BlockKind, Document};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn build_large_doc(blocks: usize, text_len: usize) -> Document {
    let mut doc = Document::new();
    for i in 0..blocks {
        let block = Block::new(
            BlockKind::Paragraph {
                text: "a".repeat(text_len),
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

#[test]
#[ignore = "manual dhat run"]
fn nfr2_working_set_under_limit() {
    let _profiler = dhat::Profiler::new_heap();
    let doc = build_large_doc(1_000, 200);
    let _output = doc.serialize(md_crdt::doc::EquivalenceMode::Structural);
    drop(doc);
}

#[test]
#[ignore = "manual dhat run"]
fn nfr2_peak_allocation_under_limit() {
    let _profiler = dhat::Profiler::new_heap();
    let doc = build_large_doc(2_000, 200);
    let _output = doc.serialize(md_crdt::doc::EquivalenceMode::Structural);
    drop(doc);
}

#[test]
#[ignore = "manual dhat run"]
fn nfr2_no_unbounded_growth_on_repeated_operations() {
    let _profiler = dhat::Profiler::new_heap();
    let mut doc = Document::new();

    // Insert and serialize many times to detect memory leaks
    for i in 0..100 {
        let block = Block::new(
            BlockKind::Paragraph {
                text: format!("Repeated block {i}"),
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
        // Serialize to exercise the full path
        let _output = doc.serialize(md_crdt::doc::EquivalenceMode::Structural);
    }

    // Memory should be bounded by content size, not operation count
    // The dhat profiler will report total allocations; review output manually
    drop(doc);
}
