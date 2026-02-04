//! Tests for CRDT merge behavior.
//!
//! These tests verify that concurrent operations from multiple peers
//! converge to the same state regardless of application order.

use md_crdt_core::{MarkSet, OpId, SequenceOp};
use md_crdt_doc::{Block, BlockKind, Document, SerializeConfig};
use uuid::Uuid;

/// Create a simple paragraph block with the given text
#[allow(dead_code)]
fn para_block(text: &str, peer: u64, counter: u64) -> Block {
    let elem_id = OpId { counter, peer };
    Block {
        id: Uuid::new_v4(),
        elem_id,
        kind: BlockKind::Paragraph {
            text: text.to_string(),
        },
        marks: MarkSet::new(),
    }
}

/// Create a block with a fixed UUID for deterministic testing
fn para_block_fixed(text: &str, peer: u64, counter: u64, uuid_seed: u128) -> Block {
    let elem_id = OpId { counter, peer };
    Block {
        id: Uuid::from_u128(uuid_seed),
        elem_id,
        kind: BlockKind::Paragraph {
            text: text.to_string(),
        },
        marks: MarkSet::new(),
    }
}

fn serialize_structural(doc: &Document) -> String {
    doc.serialize_with_config(&SerializeConfig::structural())
}

// =============================================================================
// Commutativity Tests
// =============================================================================

mod commutativity {
    use super::*;

    #[test]
    fn two_peers_insert_different_blocks_order_ab() {
        // Peer A inserts "Hello", Peer B inserts "World"
        // Apply in order A then B
        let block_a = para_block_fixed("Hello", 1, 1, 1);
        let block_b = para_block_fixed("World", 2, 1, 2);

        let op_a = SequenceOp::Insert {
            after: None,
            id: block_a.elem_id,
            value: block_a.clone(),
            right_origin: None,
        };
        let op_b = SequenceOp::Insert {
            after: None,
            id: block_b.elem_id,
            value: block_b.clone(),
            right_origin: None,
        };

        let mut doc = Document::new();
        doc.blocks.apply(op_a.clone());
        doc.blocks.apply(op_b.clone());

        let output_ab = serialize_structural(&doc);

        // Apply in order B then A
        let mut doc2 = Document::new();
        doc2.blocks.apply(op_b);
        doc2.blocks.apply(op_a);

        let output_ba = serialize_structural(&doc2);

        assert_eq!(
            output_ab, output_ba,
            "Operation order should not affect final state"
        );
    }

    #[test]
    fn three_peers_concurrent_inserts() {
        let block_a = para_block_fixed("Alice", 1, 1, 1);
        let block_b = para_block_fixed("Bob", 2, 1, 2);
        let block_c = para_block_fixed("Carol", 3, 1, 3);

        let op_a = SequenceOp::Insert {
            after: None,
            id: block_a.elem_id,
            value: block_a.clone(),
            right_origin: None,
        };
        let op_b = SequenceOp::Insert {
            after: None,
            id: block_b.elem_id,
            value: block_b.clone(),
            right_origin: None,
        };
        let op_c = SequenceOp::Insert {
            after: None,
            id: block_c.elem_id,
            value: block_c.clone(),
            right_origin: None,
        };

        // Test all 6 permutations
        let permutations = [
            vec![op_a.clone(), op_b.clone(), op_c.clone()],
            vec![op_a.clone(), op_c.clone(), op_b.clone()],
            vec![op_b.clone(), op_a.clone(), op_c.clone()],
            vec![op_b.clone(), op_c.clone(), op_a.clone()],
            vec![op_c.clone(), op_a.clone(), op_b.clone()],
            vec![op_c.clone(), op_b.clone(), op_a.clone()],
        ];

        let mut outputs = Vec::new();
        for ops in &permutations {
            let mut doc = Document::new();
            for op in ops {
                doc.blocks.apply(op.clone());
            }
            outputs.push(serialize_structural(&doc));
        }

        // All permutations should produce the same output
        for (i, output) in outputs.iter().enumerate().skip(1) {
            assert_eq!(
                outputs[0], *output,
                "Permutation {} differs from permutation 0",
                i
            );
        }
    }

    #[test]
    fn insert_after_existing_block_commutes() {
        // First block exists, two peers insert after it
        let block_base = para_block_fixed("Base", 1, 1, 1);
        let block_a = para_block_fixed("After-A", 2, 1, 2);
        let block_b = para_block_fixed("After-B", 3, 1, 3);

        let op_base = SequenceOp::Insert {
            after: None,
            id: block_base.elem_id,
            value: block_base.clone(),
            right_origin: None,
        };
        let op_a = SequenceOp::Insert {
            after: Some(block_base.elem_id),
            id: block_a.elem_id,
            value: block_a.clone(),
            right_origin: None,
        };
        let op_b = SequenceOp::Insert {
            after: Some(block_base.elem_id),
            id: block_b.elem_id,
            value: block_b.clone(),
            right_origin: None,
        };

        // Order: base, a, b
        let mut doc1 = Document::new();
        doc1.blocks.apply(op_base.clone());
        doc1.blocks.apply(op_a.clone());
        doc1.blocks.apply(op_b.clone());

        // Order: base, b, a
        let mut doc2 = Document::new();
        doc2.blocks.apply(op_base.clone());
        doc2.blocks.apply(op_b.clone());
        doc2.blocks.apply(op_a.clone());

        assert_eq!(
            serialize_structural(&doc1),
            serialize_structural(&doc2),
            "Concurrent inserts after same block should converge"
        );
    }
}

// =============================================================================
// Idempotency Tests
// =============================================================================

mod idempotency {
    use super::*;

    #[test]
    fn applying_same_op_twice_is_idempotent() {
        let block = para_block_fixed("Hello", 1, 1, 1);
        let op = SequenceOp::Insert {
            after: None,
            id: block.elem_id,
            value: block.clone(),
            right_origin: None,
        };

        let mut doc = Document::new();
        doc.blocks.apply(op.clone());
        let output1 = serialize_structural(&doc);

        doc.blocks.apply(op.clone());
        let output2 = serialize_structural(&doc);

        assert_eq!(
            output1, output2,
            "Applying same op twice should be idempotent"
        );
    }

    #[test]
    fn applying_ops_twice_each_is_idempotent() {
        let block_a = para_block_fixed("A", 1, 1, 1);
        let block_b = para_block_fixed("B", 2, 1, 2);

        let op_a = SequenceOp::Insert {
            after: None,
            id: block_a.elem_id,
            value: block_a.clone(),
            right_origin: None,
        };
        let op_b = SequenceOp::Insert {
            after: Some(block_a.elem_id),
            id: block_b.elem_id,
            value: block_b.clone(),
            right_origin: None,
        };

        let mut doc = Document::new();
        doc.blocks.apply(op_a.clone());
        doc.blocks.apply(op_b.clone());
        let output1 = serialize_structural(&doc);

        // Apply again
        doc.blocks.apply(op_a);
        doc.blocks.apply(op_b);
        let output2 = serialize_structural(&doc);

        assert_eq!(output1, output2);
    }
}

// =============================================================================
// Delete Behavior Tests
// =============================================================================

mod delete_behavior {
    use super::*;

    #[test]
    fn delete_then_insert_converges() {
        let block = para_block_fixed("Original", 1, 1, 1);
        let block_new = para_block_fixed("New", 2, 2, 2);

        let op_insert = SequenceOp::Insert {
            after: None,
            id: block.elem_id,
            value: block.clone(),
            right_origin: None,
        };
        let op_delete = SequenceOp::Delete {
            target: block.elem_id,
            id: OpId {
                counter: 1,
                peer: 2,
            },
        };
        let op_insert_new = SequenceOp::Insert {
            after: None,
            id: block_new.elem_id,
            value: block_new.clone(),
            right_origin: None,
        };

        // Order 1: insert, delete, insert_new
        let mut doc1 = Document::new();
        doc1.blocks.apply(op_insert.clone());
        doc1.blocks.apply(op_delete.clone());
        doc1.blocks.apply(op_insert_new.clone());

        // Order 2: insert, insert_new, delete
        let mut doc2 = Document::new();
        doc2.blocks.apply(op_insert.clone());
        doc2.blocks.apply(op_insert_new.clone());
        doc2.blocks.apply(op_delete.clone());

        assert_eq!(
            serialize_structural(&doc1),
            serialize_structural(&doc2),
            "Delete and insert should converge regardless of order"
        );
    }

    #[test]
    fn concurrent_deletes_converge() {
        let block = para_block_fixed("ToDelete", 1, 1, 1);

        let op_insert = SequenceOp::Insert {
            after: None,
            id: block.elem_id,
            value: block.clone(),
            right_origin: None,
        };
        let op_delete_a = SequenceOp::Delete {
            target: block.elem_id,
            id: OpId {
                counter: 1,
                peer: 2,
            },
        };
        let op_delete_b = SequenceOp::Delete {
            target: block.elem_id,
            id: OpId {
                counter: 1,
                peer: 3,
            },
        };

        // Both peers delete the same block
        let mut doc1 = Document::new();
        doc1.blocks.apply(op_insert.clone());
        doc1.blocks.apply(op_delete_a.clone());
        doc1.blocks.apply(op_delete_b.clone());

        let mut doc2 = Document::new();
        doc2.blocks.apply(op_insert.clone());
        doc2.blocks.apply(op_delete_b);
        doc2.blocks.apply(op_delete_a);

        assert_eq!(
            serialize_structural(&doc1),
            serialize_structural(&doc2),
            "Concurrent deletes should converge"
        );
    }

    #[test]
    fn delete_nonexistent_is_safe() {
        let op_delete = SequenceOp::<Block>::Delete {
            target: OpId {
                counter: 999,
                peer: 999,
            },
            id: OpId {
                counter: 1,
                peer: 1,
            },
        };

        let mut doc = Document::new();
        doc.blocks.apply(op_delete);

        // Should not panic, document should be empty
        assert!(doc.blocks.iter_asc().next().is_none());
    }
}

// =============================================================================
// Convergence with Parsed Documents
// =============================================================================

mod parse_and_merge {
    use super::*;
    use md_crdt_doc::Parser;

    #[test]
    fn parsed_documents_have_consistent_serialization() {
        let input = "# Title\n\nParagraph one.\n\nParagraph two.";

        let doc1 = Parser::parse(input);
        let doc2 = Parser::parse(input);

        let output1 = serialize_structural(&doc1);
        let output2 = serialize_structural(&doc2);

        assert_eq!(
            output1, output2,
            "Parsing same input should produce consistent output"
        );
    }

    #[test]
    fn edit_then_serialize_is_stable() {
        let input = "Hello world";
        let doc = Parser::parse(input);

        // Get the first block
        let blocks: Vec<_> = doc.blocks.iter_asc().collect();
        assert!(!blocks.is_empty());

        // Serialize twice
        let output1 = serialize_structural(&doc);
        let output2 = serialize_structural(&doc);

        assert_eq!(output1, output2);
    }
}

// =============================================================================
// Edge Cases
// =============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn empty_document_operations() {
        let mut doc = Document::new();

        // Delete from empty should not panic
        let op_delete = SequenceOp::<Block>::Delete {
            target: OpId {
                counter: 1,
                peer: 1,
            },
            id: OpId {
                counter: 2,
                peer: 1,
            },
        };
        doc.blocks.apply(op_delete);

        assert_eq!(serialize_structural(&doc), "");
    }

    #[test]
    fn many_concurrent_peers() {
        let mut ops = Vec::new();
        for peer in 1..=10u64 {
            let block = para_block_fixed(&format!("Peer{}", peer), peer, 1, peer as u128);
            ops.push(SequenceOp::Insert {
                after: None,
                id: block.elem_id,
                value: block,
                right_origin: None,
            });
        }

        // Apply in forward order
        let mut doc1 = Document::new();
        for op in &ops {
            doc1.blocks.apply(op.clone());
        }

        // Apply in reverse order
        let mut doc2 = Document::new();
        for op in ops.iter().rev() {
            doc2.blocks.apply(op.clone());
        }

        assert_eq!(
            serialize_structural(&doc1),
            serialize_structural(&doc2),
            "10 concurrent peers should converge"
        );
    }

    #[test]
    fn interleaved_operations_from_multiple_peers() {
        // Simulate realistic interleaving where peers' operations arrive mixed
        let block_a1 = para_block_fixed("A1", 1, 1, 11);
        let block_a2 = para_block_fixed("A2", 1, 2, 12);
        let block_b1 = para_block_fixed("B1", 2, 1, 21);
        let block_b2 = para_block_fixed("B2", 2, 2, 22);

        let op_a1 = SequenceOp::Insert {
            after: None,
            id: block_a1.elem_id,
            value: block_a1.clone(),
            right_origin: None,
        };
        let op_a2 = SequenceOp::Insert {
            after: Some(block_a1.elem_id),
            id: block_a2.elem_id,
            value: block_a2.clone(),
            right_origin: None,
        };
        let op_b1 = SequenceOp::Insert {
            after: None,
            id: block_b1.elem_id,
            value: block_b1.clone(),
            right_origin: None,
        };
        let op_b2 = SequenceOp::Insert {
            after: Some(block_b1.elem_id),
            id: block_b2.elem_id,
            value: block_b2.clone(),
            right_origin: None,
        };

        // Interleaved order 1: a1, b1, a2, b2
        let mut doc1 = Document::new();
        doc1.blocks.apply(op_a1.clone());
        doc1.blocks.apply(op_b1.clone());
        doc1.blocks.apply(op_a2.clone());
        doc1.blocks.apply(op_b2.clone());

        // Interleaved order 2: b1, a1, b2, a2
        let mut doc2 = Document::new();
        doc2.blocks.apply(op_b1.clone());
        doc2.blocks.apply(op_a1.clone());
        doc2.blocks.apply(op_b2.clone());
        doc2.blocks.apply(op_a2.clone());

        // Sequential order: a1, a2, b1, b2
        let mut doc3 = Document::new();
        doc3.blocks.apply(op_a1);
        doc3.blocks.apply(op_a2);
        doc3.blocks.apply(op_b1);
        doc3.blocks.apply(op_b2);

        let out1 = serialize_structural(&doc1);
        let out2 = serialize_structural(&doc2);
        let out3 = serialize_structural(&doc3);

        assert_eq!(out1, out2, "Interleaved orders should converge");
        assert_eq!(out2, out3, "Sequential vs interleaved should converge");
    }

    #[test]
    fn high_counter_values() {
        let block = para_block_fixed("High", 1, u64::MAX - 1, 1);
        let op = SequenceOp::Insert {
            after: None,
            id: block.elem_id,
            value: block,
            right_origin: None,
        };

        let mut doc = Document::new();
        doc.blocks.apply(op);

        // Should not panic
        let output = serialize_structural(&doc);
        assert!(output.contains("High"));
    }

    #[test]
    fn zero_peer_id() {
        let block = para_block_fixed("Zero", 0, 1, 1);
        let op = SequenceOp::Insert {
            after: None,
            id: block.elem_id,
            value: block,
            right_origin: None,
        };

        let mut doc = Document::new();
        doc.blocks.apply(op);

        let output = serialize_structural(&doc);
        assert!(output.contains("Zero"));
    }
}
