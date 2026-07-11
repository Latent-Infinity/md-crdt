//! Wire codec round-trip and validation tests.
//!
//! Covers Envelope / DocOp encode-decode, version rejection, and nest depth.

use md_crdt::codec::{
    BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert, CodecError, DocOp, Envelope,
    JsonOpCodec, ListItemSkeleton, MAX_WIRE_NEST_DEPTH, OpBody, OpCodec, WIRE_VERSION,
    insert_block_paragraph_is_empty,
};
use md_crdt::core::OpId;
use md_crdt::doc::BlockId;
use uuid::Uuid;

fn op(counter: u64, peer: u64) -> OpId {
    OpId { counter, peer }
}

fn block_id(n: u128) -> BlockId {
    Uuid::from_u128(n)
}

fn sample_insert_block(text: &str) -> Envelope {
    Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: op(1, 1),
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id(1),
                kind: BlockKindSkeleton::Paragraph {
                    text: text.to_string(),
                },
            },
        }),
    }
}

#[test]
fn insert_block_round_trip() {
    let codec = JsonOpCodec;
    let env = sample_insert_block("hello");
    let bytes = codec.encode(&env).expect("encode");
    let decoded = codec.decode(&bytes).expect("decode");
    assert_eq!(decoded, env);
}

#[test]
fn insert_block_with_after_and_right_origin_round_trip() {
    let codec = JsonOpCodec;
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: Some(op(2, 1)),
            id: op(3, 2),
            right_origin: Some(op(4, 1)),
            block: BlockSkeleton {
                block_id: block_id(99),
                kind: BlockKindSkeleton::CodeFence {
                    info: Some("rust".into()),
                    text: "fn main() {}".into(),
                },
            },
        }),
    };
    let bytes = codec.encode(&env).expect("encode");
    assert_eq!(codec.decode(&bytes).expect("decode"), env);
}

#[test]
fn delete_block_round_trip() {
    let codec = JsonOpCodec;
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::DeleteBlock {
            parent: None,
            target: op(5, 1),
            id: op(6, 1),
        }),
    };
    let bytes = codec.encode(&env).expect("encode");
    assert_eq!(codec.decode(&bytes).expect("decode"), env);
}

#[test]
fn unknown_wire_version_rejected() {
    let codec = JsonOpCodec;
    let mut env = sample_insert_block("");
    env.version = WIRE_VERSION + 99;
    let bytes = codec.encode(&env).expect("encode still serializes");
    let err = codec.decode(&bytes).expect_err("unknown version");
    assert!(matches!(err, CodecError::UnknownVersion(v) if v == WIRE_VERSION + 99));
}

#[test]
fn malformed_json_rejected() {
    let codec = JsonOpCodec;
    let err = codec.decode(b"not-json").expect_err("malformed");
    assert!(matches!(err, CodecError::Serde(_)));
}

#[test]
fn nest_depth_exceeded_on_encode() {
    let codec = JsonOpCodec;
    // Build a chain of nested BlockQuotes deeper than MAX_WIRE_NEST_DEPTH.
    let mut kind = BlockKindSkeleton::Paragraph {
        text: "leaf".into(),
    };
    for i in 0..=MAX_WIRE_NEST_DEPTH {
        kind = BlockKindSkeleton::BlockQuote {
            children: vec![BlockSkeletonInsert {
                after: None,
                id: op(i as u64 + 1, 1),
                right_origin: None,
                block: BlockSkeleton {
                    block_id: block_id(i as u128 + 1),
                    kind,
                },
            }],
        };
    }
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: op(100, 1),
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id(1000),
                kind,
            },
        }),
    };
    let err = codec.encode(&env).expect_err("nest too deep");
    assert!(matches!(err, CodecError::NestDepthExceeded));
}

#[test]
fn nest_depth_exceeded_on_decode() {
    let codec = JsonOpCodec;
    // Craft JSON that passes serde but exceeds depth when validated.
    let mut kind = BlockKindSkeleton::Paragraph {
        text: "leaf".into(),
    };
    for i in 0..=MAX_WIRE_NEST_DEPTH {
        kind = BlockKindSkeleton::BlockQuote {
            children: vec![BlockSkeletonInsert {
                after: None,
                id: op(i as u64 + 1, 1),
                right_origin: None,
                block: BlockSkeleton {
                    block_id: block_id(i as u128 + 1),
                    kind,
                },
            }],
        };
    }
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: op(100, 1),
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id(1000),
                kind,
            },
        }),
    };
    // Bypass encode validation by serializing with serde_json directly.
    let bytes = serde_json::to_vec(&env).expect("serde");
    let err = codec.decode(&bytes).expect_err("nest too deep on decode");
    assert!(matches!(err, CodecError::NestDepthExceeded), "got {err:?}");
}

#[test]
fn insert_block_paragraph_is_empty_helper() {
    assert!(insert_block_paragraph_is_empty(&sample_insert_block("")));
    assert!(!insert_block_paragraph_is_empty(&sample_insert_block("x")));
    // Non-paragraph / non-InsertBlock: vacuously true for the predicate (only
    // Paragraph InsertBlock can violate the unit-mode empty rule).
    let del = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::DeleteBlock {
            parent: None,
            target: op(1, 1),
            id: op(2, 1),
        }),
    };
    assert!(insert_block_paragraph_is_empty(&del));
}

#[test]
fn raw_and_quote_kinds_round_trip() {
    let codec = JsonOpCodec;
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: op(1, 1),
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id(7),
                kind: BlockKindSkeleton::BlockQuote {
                    children: vec![BlockSkeletonInsert {
                        after: None,
                        id: op(2, 1),
                        right_origin: None,
                        block: BlockSkeleton {
                            block_id: block_id(8),
                            kind: BlockKindSkeleton::RawBlock {
                                raw: ":::note\nhi".into(),
                            },
                        },
                    }],
                },
            },
        }),
    };
    let bytes = codec.encode(&env).expect("encode");
    assert_eq!(codec.decode(&bytes).expect("decode"), env);
}

#[test]
fn heading_and_list_kinds_round_trip() {
    let codec = JsonOpCodec;
    let env = Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertBlock {
            parent: None,
            after: None,
            id: op(1, 1),
            right_origin: None,
            block: BlockSkeleton {
                block_id: block_id(7),
                kind: BlockKindSkeleton::List {
                    ordered: false,
                    items: vec![ListItemSkeleton {
                        after: None,
                        id: op(2, 1),
                        right_origin: None,
                        block_id: block_id(8),
                        children: vec![BlockSkeletonInsert {
                            after: None,
                            id: op(3, 1),
                            right_origin: None,
                            block: BlockSkeleton {
                                block_id: block_id(9),
                                kind: BlockKindSkeleton::Heading {
                                    level: 2,
                                    text: "child".into(),
                                },
                            },
                        }],
                    }],
                },
            },
        }),
    };

    let bytes = codec.encode(&env).expect("encode");
    assert_eq!(codec.decode(&bytes).expect("decode"), env);
}
