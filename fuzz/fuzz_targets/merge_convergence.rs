#![no_main]

//! Fuzz target for CRDT convergence property.
//!
//! This fuzzer generates random block operations and applies them to multiple
//! documents in different orders, asserting they converge to the same state.
//!
//! Key insight: Operations must have fixed, predetermined targets (OpIds) so
//! that applying them in any order produces the same result. We pre-generate
//! all operations with their targets, then apply in different orders.

use libfuzzer_sys::fuzz_target;
use md_crdt_core::{MarkSet, OpId, SequenceOp};
use md_crdt_doc::{Block, BlockKind, Document, SerializeConfig};
use uuid::Uuid;

/// Pre-computed operation with fixed targets
#[derive(Clone)]
enum FixedOp {
    Insert { block: Block, after: Option<OpId> },
    Delete { target: OpId, delete_id: OpId },
}

impl FixedOp {
    fn to_sequence_op(&self) -> SequenceOp<Block> {
        match self {
            FixedOp::Insert { block, after } => SequenceOp::Insert {
                after: *after,
                id: block.elem_id,
                value: block.clone(),
                right_origin: None,
            },
            FixedOp::Delete { target, delete_id } => SequenceOp::Delete {
                target: *target,
                id: *delete_id,
            },
        }
    }
}

/// Generate operations from fuzz input with pre-determined targets
fn parse_ops(data: &[u8]) -> Vec<FixedOp> {
    let mut ops = Vec::new();
    let mut offset = 0;
    let mut all_elem_ids: Vec<OpId> = Vec::new();
    let mut peer_counters: [u64; 4] = [1, 1, 1, 1];

    while offset + 3 <= data.len() {
        let op_type = data[offset] % 4;
        let peer = (data[offset + 1] % 4) as u64;
        let position_hint = data[offset + 2] as usize;
        offset += 3;

        let counter = peer_counters[peer as usize];
        peer_counters[peer as usize] += 1;
        let elem_id = OpId { counter, peer };

        if op_type < 3 {
            // Insert operation
            // Determine "after" target from existing elem_ids
            let after = if all_elem_ids.is_empty() || position_hint == 0 {
                None
            } else {
                Some(all_elem_ids[(position_hint - 1) % all_elem_ids.len()])
            };

            // Create block with deterministic UUID based on elem_id
            let uuid_seed = ((peer as u128) << 64) | (counter as u128);
            let block = Block {
                id: Uuid::from_u128(uuid_seed),
                elem_id,
                kind: BlockKind::Paragraph {
                    text: format!("p{}c{}", peer, counter),
                },
                marks: MarkSet::new(),
            };

            all_elem_ids.push(elem_id);
            ops.push(FixedOp::Insert { block, after });
        } else {
            // Delete operation - only if we have something to delete
            if !all_elem_ids.is_empty() {
                let target = all_elem_ids[position_hint % all_elem_ids.len()];
                ops.push(FixedOp::Delete {
                    target,
                    delete_id: elem_id,
                });
            }
        }
    }

    ops
}

fn apply_ops(ops: &[FixedOp]) -> Document {
    let mut doc = Document::new();
    for op in ops {
        doc.blocks.apply(op.to_sequence_op());
    }
    doc
}

fn serialize(doc: &Document) -> String {
    doc.serialize_with_config(&SerializeConfig::structural())
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }

    let ops = parse_ops(data);
    if ops.len() < 2 {
        return;
    }

    // Apply in forward order
    let doc1 = apply_ops(&ops);
    let output1 = serialize(&doc1);

    // Apply in reverse order
    let reversed: Vec<_> = ops.iter().rev().cloned().collect();
    let doc2 = apply_ops(&reversed);
    let output2 = serialize(&doc2);

    // CRDT invariant: must converge
    assert_eq!(
        output1,
        output2,
        "CRDT convergence violation!\nForward: {:?}\nReverse: {:?}\nOps: {} operations",
        output1,
        output2,
        ops.len()
    );

    // Also test with a deterministic shuffle
    if data.len() >= 8 {
        let mut shuffled = ops.clone();
        for i in 0..shuffled.len() {
            let j = (data[i % data.len()] as usize) % shuffled.len();
            shuffled.swap(i, j);
        }
        let doc3 = apply_ops(&shuffled);
        let output3 = serialize(&doc3);

        assert_eq!(
            output1, output3,
            "CRDT convergence violation (shuffled)!\nForward: {:?}\nShuffled: {:?}",
            output1, output3
        );
    }
});
