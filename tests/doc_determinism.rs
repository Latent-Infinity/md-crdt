use md_crdt::core::{MarkSet, OpId, SequenceOp};
use md_crdt::doc::{Block, BlockKind, Document, EquivalenceMode, SerializeConfig};
use uuid::Uuid;

fn fixed_block(id: Uuid, elem_id: OpId, text: &str) -> Block {
    Block {
        id,
        elem_id,
        kind: BlockKind::Paragraph {
            text: text.to_string(),
        },
        marks: MarkSet::new(),
    }
}

#[test]
fn dc1_same_state_same_config_is_identical() {
    let mut doc = Document::new();
    let block = Block::new(
        BlockKind::Paragraph {
            text: "Hello".to_string(),
        },
        OpId {
            counter: 1,
            peer: 1,
        },
    );
    doc.blocks.insert(
        None,
        block,
        OpId {
            counter: 1,
            peer: 1,
        },
    );

    let config = SerializeConfig {
        equivalence: EquivalenceMode::Structural,
        prefer_raw_source: false,
    };

    let first = doc.serialize_with_config(&config);
    let second = doc.serialize_with_config(&config);
    assert_eq!(first, second);
}

#[test]
fn dc1_different_op_order_same_state_same_output() {
    let id_a = Uuid::from_u128(1);
    let id_b = Uuid::from_u128(2);
    let elem_a = OpId {
        counter: 1,
        peer: 1,
    };
    let elem_b = OpId {
        counter: 2,
        peer: 1,
    };

    let op_insert_a = SequenceOp::Insert {
        after: None,
        id: elem_a,
        value: fixed_block(id_a, elem_a, "A"),
        right_origin: None,
    };
    let op_insert_b = SequenceOp::Insert {
        after: Some(elem_a),
        id: elem_b,
        value: fixed_block(id_b, elem_b, "B"),
        right_origin: None,
    };

    let mut doc_a = Document::new();
    doc_a.blocks.apply(op_insert_a.clone());
    doc_a.blocks.apply(op_insert_b.clone());

    let mut doc_b = Document::new();
    doc_b.blocks.apply(op_insert_b);
    doc_b.blocks.apply(op_insert_a);

    let config = SerializeConfig {
        equivalence: EquivalenceMode::Structural,
        prefer_raw_source: false,
    };

    let output_a = doc_a.serialize_with_config(&config);
    let output_b = doc_b.serialize_with_config(&config);
    assert_eq!(output_a, output_b);
}

#[test]
fn dc1_serialization_stable_across_restarts() {
    let input = "Hello\n\nWorld\n";
    let doc = md_crdt::doc::Parser::parse(input);
    let config = SerializeConfig {
        equivalence: EquivalenceMode::Structural,
        prefer_raw_source: false,
    };

    let output = doc.serialize_with_config(&config);
    let doc_reloaded = md_crdt::doc::Parser::parse(&output);
    let output_again = doc_reloaded.serialize_with_config(&config);
    assert_eq!(output, output_again);
}

#[test]
fn kd7_round_trip_exact_idempotent() {
    let input = "---\nkey: value\n---\n\nHello\n";
    let doc = md_crdt::doc::Parser::parse(input);
    let config = SerializeConfig::exact();
    let output = doc.serialize_with_config(&config);
    let doc_reloaded = md_crdt::doc::Parser::parse(&output);
    let output_again = doc_reloaded.serialize_with_config(&config);
    assert_eq!(output, output_again);
}

#[test]
fn kd7_round_trip_structural_idempotent() {
    let input = "Hello\n\n\nWorld";
    let doc = md_crdt::doc::Parser::parse(input);
    let config = SerializeConfig::structural();
    let output = doc.serialize_with_config(&config);
    let doc_reloaded = md_crdt::doc::Parser::parse(&output);
    let output_again = doc_reloaded.serialize_with_config(&config);
    assert_eq!(output, output_again);
}
