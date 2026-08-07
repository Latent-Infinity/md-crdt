//! Golden vectors and cross-codec pins for `md_crdt_bin_v1`.
//!
//! Golden expected bytes are produced by the encoder for fixed inputs and locked
//! here so format drift is intentional (bump frame or semantic wire version),
//! not accidental.

use md_crdt::codec::{
    BinaryOpCodec, DocOp, Envelope, JsonOpCodec, OpBody, OpCodec, TextUnitWire, WIRE_VERSION,
};
use md_crdt::core::{OpId, StateVector};
use md_crdt::{
    BINARY_CHANGE_MAGIC, BINARY_ENVELOPE_MAGIC, ChangeMessage, Operation,
    decode_bin_v1_change_message_to_json_payloads, decode_change_message_bin_v1,
    encode_change_message_bin_v1, encode_json_change_message_as_bin_v1,
};
use uuid::Uuid;

fn op(counter: u64, peer: u64) -> OpId {
    OpId { counter, peer }
}

fn fixed_insert_text_envelope() -> Envelope {
    Envelope {
        version: WIRE_VERSION,
        body: OpBody::Doc(DocOp::InsertText {
            block_elem: op(1, 1),
            block_id: Uuid::from_u128(0x1111_2222_3333_4444),
            units: vec![TextUnitWire {
                id: op(2, 1),
                after: Some(op(1, 1)),
                right_origin: None,
                grapheme: "a".into(),
            }],
        }),
    }
}

#[test]
fn golden_binary_envelope_prefix_and_roundtrip() {
    let env = fixed_insert_text_envelope();
    let bytes = BinaryOpCodec.encode(&env).expect("encode");
    // Format is specified by test: magic + frame version 1 LE.
    assert_eq!(&bytes[0..4], BINARY_ENVELOPE_MAGIC.as_slice());
    assert_eq!(&bytes[4..6], &1u16.to_le_bytes());
    let body_len = u32::from_le_bytes(bytes[6..10].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), 10 + body_len);
    assert_eq!(
        bytes,
        [
            77, 68, 69, 49, 1, 0, 31, 0, 0, 0, 5, 0, 3, 1, 1, 16, 0, 0, 0, 0, 0, 0, 0, 0, 17, 17,
            34, 34, 51, 51, 68, 68, 1, 2, 1, 1, 1, 1, 0, 1, 97,
        ],
        "binary envelope bytes changed without a frame or semantic version bump"
    );
    assert_eq!(BinaryOpCodec.decode(&bytes).unwrap(), env);
}

#[test]
fn binary_roundtrip_equals_json_roundtrip() {
    let env = fixed_insert_text_envelope();
    let json_bytes = JsonOpCodec.encode(&env).unwrap();
    let bin_bytes = BinaryOpCodec.encode(&env).unwrap();
    let from_json = JsonOpCodec.decode(&json_bytes).unwrap();
    let from_bin = BinaryOpCodec.decode(&bin_bytes).unwrap();
    assert_eq!(from_json, from_bin);
    assert_eq!(from_json, env);
    // Binary is not JSON text.
    assert!(serde_json::from_slice::<serde_json::Value>(&bin_bytes).is_err());
}

#[test]
fn truncated_or_corrupt_input_fails_closed() {
    let env = fixed_insert_text_envelope();
    let good = BinaryOpCodec.encode(&env).unwrap();
    assert!(BinaryOpCodec.decode(&good[..3]).is_err());
    assert!(BinaryOpCodec.decode(&good[..9]).is_err());
    let mut corrupt = good.clone();
    *corrupt.last_mut().unwrap() ^= 0xff;
    assert!(BinaryOpCodec.decode(&corrupt).is_err());
    let mut short_len = good;
    // Claim a huge body length.
    short_len[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(BinaryOpCodec.decode(&short_len).is_err());
}

#[test]
fn unknown_version_header_is_rejected() {
    let env = fixed_insert_text_envelope();
    let mut bytes = BinaryOpCodec.encode(&env).unwrap();
    // Frame format version (not envelope semantic version).
    bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
    assert!(matches!(
        BinaryOpCodec.decode(&bytes),
        Err(md_crdt::CodecError::UnknownVersion(99))
    ));

    // Semantic envelope version inside postcard body: build env with bad version.
    let mut bad = env;
    bad.version = 999;
    assert!(matches!(
        BinaryOpCodec.encode(&bad),
        Err(md_crdt::CodecError::UnknownVersion(999))
    ));
}

#[test]
fn change_message_bin_v1_golden_prefix() {
    let env = fixed_insert_text_envelope();
    let payload = JsonOpCodec.encode(&env).unwrap();
    let mut since = StateVector::new();
    since.set(1, 1);
    let msg = ChangeMessage {
        since,
        ops: vec![Operation {
            id: op(2, 1),
            payload: payload.into(),
        }],
    };
    let wire = encode_json_change_message_as_bin_v1(&msg).unwrap();
    assert_eq!(&wire[0..8], BINARY_CHANGE_MAGIC.as_slice());
    assert_eq!(&wire[8..10], &1u16.to_le_bytes());
    assert_eq!(
        wire,
        [
            77, 68, 67, 82, 66, 73, 78, 49, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
            0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 41, 0, 0, 0,
            77, 68, 69, 49, 1, 0, 31, 0, 0, 0, 5, 0, 3, 1, 1, 16, 0, 0, 0, 0, 0, 0, 0, 0, 17, 17,
            34, 34, 51, 51, 68, 68, 1, 2, 1, 1, 1, 1, 0, 1, 97,
        ],
        "binary change-message bytes changed without a frame or semantic version bump"
    );
    let restored = decode_bin_v1_change_message_to_json_payloads(&wire).unwrap();
    assert_eq!(restored.ops.len(), 1);
    assert_eq!(JsonOpCodec.decode(&restored.ops[0].payload).unwrap(), env);
}

#[test]
fn change_message_opaque_frame_roundtrip() {
    let env = fixed_insert_text_envelope();
    let payload = BinaryOpCodec.encode(&env).unwrap();
    let msg = ChangeMessage {
        since: StateVector::new(),
        ops: vec![Operation {
            id: op(5, 9),
            payload: payload.into(),
        }],
    };
    let bytes = encode_change_message_bin_v1(&msg).unwrap();
    assert_eq!(decode_change_message_bin_v1(&bytes).unwrap(), msg);
}

#[test]
fn binary_wire_is_smaller_than_json_for_sample_insert() {
    use md_crdt::{CollaborativeDocument, StateVector, block_id_from_op};
    let mut session = CollaborativeDocument::new(1);
    let elem = session.insert_paragraph(None, &"x".repeat(1_000)).unwrap();
    let bid = block_id_from_op(elem);
    session.insert_text(bid, 500, "y").unwrap();
    let msg = session
        .encode_changes_since(&StateVector::default())
        .unwrap();
    let json = serde_json::to_vec(&msg).unwrap();
    let bin = encode_json_change_message_as_bin_v1(&msg).unwrap();
    assert!(
        bin.len() < json.len(),
        "binary {} should beat JSON {} for dense unit history",
        bin.len(),
        json.len()
    );
    // Indicative ratio for plan-state (debug build; order-of-magnitude only).
    eprintln!(
        "codec_size_sample n=1000_full_history json={} bin={} ratio={:.2}",
        json.len(),
        bin.len(),
        json.len() as f64 / bin.len() as f64
    );
}
