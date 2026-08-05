//! Binary `ChangeMessage` transport (`md_crdt_bin_v1`).
//!
//! Frame (little-endian):
//!
//! ```text
//! magic:        b"MDCRBIN1"     (8 bytes)
//! format:       u16 = 1
//! since_len:    u32
//! since[]:      (peer u64, counter u64) × since_len
//! ops_len:      u32
//! ops[]:        peer u64, counter u64, payload_len u32, payload[]
//! ```
//!
//! Operation payloads are opaque bytes (typically [`crate::codec::BinaryOpCodec`]
//! envelopes). Fail closed on truncated or oversized frames.

use super::{ChangeMessage, Operation};
use crate::codec::{BinaryOpCodec, CodecError, JsonOpCodec, OpCodec};
use crate::core::{OpId, StateVector};
use std::sync::Arc;

/// Magic for a binary ChangeMessage frame.
pub const BINARY_CHANGE_MAGIC: &[u8; 8] = b"MDCRBIN1";

/// Binary ChangeMessage frame version.
pub const BINARY_CHANGE_FORMAT: u16 = 1;

/// Codec label for competitive Tier C′ and product docs.
pub const MD_CRDT_BIN_V1_LABEL: &str = "md_crdt_bin_v1";

/// Encode a change message into the `md_crdt_bin_v1` frame (payloads as-is).
pub fn encode_change_message_bin_v1(message: &ChangeMessage) -> Result<Vec<u8>, CodecError> {
    let since_pairs: Vec<(u64, u64)> = message.since.iter().collect();
    let since_len =
        u32::try_from(since_pairs.len()).map_err(|_| CodecError::Invalid("since too large"))?;
    let ops_len =
        u32::try_from(message.ops.len()).map_err(|_| CodecError::Invalid("ops too large"))?;

    let mut out = Vec::with_capacity(64 + message.ops.len() * 32);
    out.extend_from_slice(BINARY_CHANGE_MAGIC);
    out.extend_from_slice(&BINARY_CHANGE_FORMAT.to_le_bytes());
    out.extend_from_slice(&since_len.to_le_bytes());
    for (peer, counter) in since_pairs {
        out.extend_from_slice(&peer.to_le_bytes());
        out.extend_from_slice(&counter.to_le_bytes());
    }
    out.extend_from_slice(&ops_len.to_le_bytes());
    for op in &message.ops {
        out.extend_from_slice(&op.id.peer.to_le_bytes());
        out.extend_from_slice(&op.id.counter.to_le_bytes());
        let plen = u32::try_from(op.payload.len())
            .map_err(|_| CodecError::Invalid("payload too large"))?;
        out.extend_from_slice(&plen.to_le_bytes());
        out.extend_from_slice(&op.payload);
    }
    Ok(out)
}

/// Decode a `md_crdt_bin_v1` change message frame.
pub fn decode_change_message_bin_v1(bytes: &[u8]) -> Result<ChangeMessage, CodecError> {
    let mut offset = 0usize;
    let magic = read_slice(bytes, &mut offset, 8)?;
    if magic != BINARY_CHANGE_MAGIC.as_slice() {
        return Err(CodecError::Invalid("bad binary change magic"));
    }
    let format = read_u16(bytes, &mut offset)?;
    if format != BINARY_CHANGE_FORMAT {
        return Err(CodecError::UnknownVersion(format));
    }
    let since_len = read_u32(bytes, &mut offset)? as usize;
    let remaining = bytes.len().saturating_sub(offset);
    if since_len > remaining.saturating_sub(4) / 16 {
        return Err(CodecError::Invalid(
            "state vector count exceeds binary change frame",
        ));
    }
    let mut since = StateVector::new();
    for _ in 0..since_len {
        let peer = read_u64(bytes, &mut offset)?;
        let counter = read_u64(bytes, &mut offset)?;
        since.set(peer, counter);
    }
    let ops_len = read_u32(bytes, &mut offset)? as usize;
    if ops_len > bytes.len().saturating_sub(offset) / 20 {
        return Err(CodecError::Invalid(
            "operation count exceeds binary change frame",
        ));
    }
    let mut ops = Vec::with_capacity(ops_len);
    for _ in 0..ops_len {
        let peer = read_u64(bytes, &mut offset)?;
        let counter = read_u64(bytes, &mut offset)?;
        let plen = read_u32(bytes, &mut offset)? as usize;
        let payload = read_slice(bytes, &mut offset, plen)?;
        ops.push(Operation {
            id: OpId { counter, peer },
            payload: Arc::<[u8]>::from(payload.to_vec()),
        });
    }
    if offset != bytes.len() {
        return Err(CodecError::Invalid(
            "trailing bytes after binary change message",
        ));
    }
    Ok(ChangeMessage { since, ops })
}

/// Convert a session change message (JSON envelope payloads) to binary wire bytes:
/// each payload is re-encoded with [`BinaryOpCodec`], then framed as `md_crdt_bin_v1`.
pub fn encode_json_change_message_as_bin_v1(
    message: &ChangeMessage,
) -> Result<Vec<u8>, CodecError> {
    let json = JsonOpCodec;
    let bin = BinaryOpCodec;
    let mut converted = ChangeMessage {
        since: message.since.clone(),
        ops: Vec::with_capacity(message.ops.len()),
    };
    for op in &message.ops {
        let envelope = match bin.decode(&op.payload) {
            Ok(env) => env,
            Err(_) => json.decode(&op.payload)?,
        };
        let payload = bin.encode(&envelope)?;
        converted.ops.push(Operation {
            id: op.id,
            payload: payload.into(),
        });
    }
    encode_change_message_bin_v1(&converted)
}

/// Decode `md_crdt_bin_v1` bytes into a change message whose payloads are JSON
/// envelopes (so a default [`crate::session::CollaborativeDocument`] can apply them).
pub fn decode_bin_v1_change_message_to_json_payloads(
    bytes: &[u8],
) -> Result<ChangeMessage, CodecError> {
    let message = decode_change_message_bin_v1(bytes)?;
    let json = JsonOpCodec;
    let bin = BinaryOpCodec;
    let mut out = ChangeMessage {
        since: message.since,
        ops: Vec::with_capacity(message.ops.len()),
    };
    for op in message.ops {
        let envelope = bin.decode(&op.payload)?;
        let payload = json.encode(&envelope)?;
        out.ops.push(Operation {
            id: op.id,
            payload: payload.into(),
        });
    }
    Ok(out)
}

fn read_slice<'a>(bytes: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], CodecError> {
    let end = offset
        .checked_add(len)
        .ok_or(CodecError::Invalid("length overflow"))?;
    let slice = bytes
        .get(*offset..end)
        .ok_or(CodecError::Invalid("truncated binary change message"))?;
    *offset = end;
    Ok(slice)
}

fn read_u16(bytes: &[u8], offset: &mut usize) -> Result<u16, CodecError> {
    let s = read_slice(bytes, offset, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, CodecError> {
    let s = read_slice(bytes, offset, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, CodecError> {
    let s = read_slice(bytes, offset, 8)?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{
        BinaryOpCodec, DocOp, Envelope, JsonOpCodec, OpBody, TextUnitWire, WIRE_VERSION,
    };
    use crate::core::OpId;
    use uuid::Uuid;

    fn sample_message() -> ChangeMessage {
        let env = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertText {
                block_elem: OpId {
                    counter: 1,
                    peer: 1,
                },
                block_id: Uuid::from_u128(1),
                units: vec![TextUnitWire {
                    id: OpId {
                        counter: 2,
                        peer: 1,
                    },
                    after: None,
                    right_origin: None,
                    grapheme: "z".into(),
                }],
            }),
        };
        let payload = JsonOpCodec.encode(&env).unwrap();
        let mut since = StateVector::new();
        since.set(1, 1);
        ChangeMessage {
            since,
            ops: vec![Operation {
                id: OpId {
                    counter: 2,
                    peer: 1,
                },
                payload: payload.into(),
            }],
        }
    }

    #[test]
    fn change_message_bin_roundtrip_opaque_payloads() {
        let msg = sample_message();
        let bytes = encode_change_message_bin_v1(&msg).unwrap();
        assert!(bytes.starts_with(BINARY_CHANGE_MAGIC));
        let decoded = decode_change_message_bin_v1(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn json_to_bin_wire_preserves_decoded_meaning() {
        let msg = sample_message();
        let wire = encode_json_change_message_as_bin_v1(&msg).unwrap();
        let back = decode_bin_v1_change_message_to_json_payloads(&wire).unwrap();
        assert_eq!(back.since, msg.since);
        assert_eq!(back.ops.len(), 1);
        let env_a = JsonOpCodec.decode(&msg.ops[0].payload).unwrap();
        let env_b = JsonOpCodec.decode(&back.ops[0].payload).unwrap();
        assert_eq!(env_a, env_b);
        // Binary wire payloads are not JSON.
        assert!(
            BinaryOpCodec
                .decode(&decode_change_message_bin_v1(&wire).unwrap().ops[0].payload)
                .is_ok()
        );
    }

    #[test]
    fn truncated_change_message_fails_closed() {
        let bytes = encode_change_message_bin_v1(&sample_message()).unwrap();
        assert!(decode_change_message_bin_v1(&bytes[..10]).is_err());
    }

    #[test]
    fn bad_magic_fails_closed() {
        let mut bytes = encode_change_message_bin_v1(&sample_message()).unwrap();
        bytes[0] ^= 1;
        assert!(matches!(
            decode_change_message_bin_v1(&bytes),
            Err(CodecError::Invalid(_))
        ));
    }

    #[test]
    fn impossible_operation_count_fails_before_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(BINARY_CHANGE_MAGIC);
        bytes.extend_from_slice(&BINARY_CHANGE_FORMAT.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode_change_message_bin_v1(&bytes),
            Err(CodecError::Invalid(
                "operation count exceeds binary change frame"
            ))
        ));
    }
}
