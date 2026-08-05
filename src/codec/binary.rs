//! Compact binary envelope codec (`md_crdt_bin_v1`).
//!
//! Frame layout (little-endian):
//!
//! ```text
//! magic:        b"MDE1"            (4 bytes)
//! format:       u16 = 1            (binary envelope frame version)
//! body_len:     u32
//! body:         postcard(Envelope)
//! ```
//!
//! Decode fails closed on bad magic, unknown format, truncated length, or
//! postcard / structure validation errors. Semantic wire version still lives
//! inside [`Envelope::version`] and must equal [`WIRE_VERSION`].

use super::wire::{Envelope, WIRE_VERSION, validate_envelope_structure};
use super::{CodecError, OpCodec};

/// Magic bytes for a binary envelope frame.
pub const BINARY_ENVELOPE_MAGIC: &[u8; 4] = b"MDE1";

/// Version of the binary envelope *frame* (not [`WIRE_VERSION`]).
pub const BINARY_ENVELOPE_FORMAT: u16 = 1;

const HEADER_LEN: usize = 4 + 2 + 4;

/// Binary postcard envelope codec for sync payloads.
#[derive(Debug, Default, Clone, Copy)]
pub struct BinaryOpCodec;

impl OpCodec for BinaryOpCodec {
    type Error = CodecError;

    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>, Self::Error> {
        validate_envelope_structure(envelope)?;
        if envelope.version != WIRE_VERSION {
            return Err(CodecError::UnknownVersion(envelope.version));
        }
        let body = postcard::to_allocvec(envelope).map_err(|e| CodecError::Serde(e.to_string()))?;
        let body_len =
            u32::try_from(body.len()).map_err(|_| CodecError::Invalid("body too large"))?;
        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        out.extend_from_slice(BINARY_ENVELOPE_MAGIC);
        out.extend_from_slice(&BINARY_ENVELOPE_FORMAT.to_le_bytes());
        out.extend_from_slice(&body_len.to_le_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    fn decode(&self, bytes: &[u8]) -> Result<Envelope, Self::Error> {
        if bytes.len() < HEADER_LEN {
            return Err(CodecError::Invalid("truncated binary envelope header"));
        }
        if &bytes[0..4] != BINARY_ENVELOPE_MAGIC.as_slice() {
            return Err(CodecError::Invalid("bad binary envelope magic"));
        }
        let format = u16::from_le_bytes([bytes[4], bytes[5]]);
        if format != BINARY_ENVELOPE_FORMAT {
            return Err(CodecError::UnknownVersion(format));
        }
        let body_len = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as usize;
        let body = bytes
            .get(HEADER_LEN..HEADER_LEN + body_len)
            .ok_or(CodecError::Invalid("truncated binary envelope body"))?;
        if bytes.len() != HEADER_LEN + body_len {
            return Err(CodecError::Invalid("trailing bytes after binary envelope"));
        }
        let (envelope, remainder): (Envelope, &[u8]) =
            postcard::take_from_bytes(body).map_err(|e| CodecError::Serde(e.to_string()))?;
        if !remainder.is_empty() {
            return Err(CodecError::Invalid(
                "trailing bytes in binary envelope body",
            ));
        }
        if envelope.version != WIRE_VERSION {
            return Err(CodecError::UnknownVersion(envelope.version));
        }
        validate_envelope_structure(&envelope)?;
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::wire::{BlockKindSkeleton, BlockSkeleton, DocOp, OpBody, TextUnitWire};
    use crate::core::OpId;
    use uuid::Uuid;

    fn sample_insert_text() -> Envelope {
        Envelope {
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
                    after: Some(OpId {
                        counter: 1,
                        peer: 1,
                    }),
                    right_origin: None,
                    grapheme: "y".into(),
                }],
            }),
        }
    }

    #[test]
    fn binary_roundtrip_insert_text() {
        let codec = BinaryOpCodec;
        let env = sample_insert_text();
        let bytes = codec.encode(&env).unwrap();
        assert!(bytes.starts_with(BINARY_ENVELOPE_MAGIC));
        assert_eq!(codec.decode(&bytes).unwrap(), env);
    }

    #[test]
    fn bad_magic_fails_closed() {
        let mut bytes = BinaryOpCodec.encode(&sample_insert_text()).unwrap();
        bytes[0] ^= 0xff;
        assert!(matches!(
            BinaryOpCodec.decode(&bytes),
            Err(CodecError::Invalid(_))
        ));
    }

    #[test]
    fn truncated_body_fails_closed() {
        let bytes = BinaryOpCodec.encode(&sample_insert_text()).unwrap();
        assert!(matches!(
            BinaryOpCodec.decode(&bytes[..bytes.len().saturating_sub(1)]),
            Err(CodecError::Invalid(_))
        ));
    }

    #[test]
    fn unknown_frame_format_rejected() {
        let mut bytes = BinaryOpCodec.encode(&sample_insert_text()).unwrap();
        bytes[4] = 0xFF;
        bytes[5] = 0xFF;
        assert!(matches!(
            BinaryOpCodec.decode(&bytes),
            Err(CodecError::UnknownVersion(_))
        ));
    }

    #[test]
    fn trailing_bytes_inside_declared_body_fail_closed() {
        let mut bytes = BinaryOpCodec.encode(&sample_insert_text()).unwrap();
        bytes.push(0);
        let body_len = u32::try_from(bytes.len() - HEADER_LEN).unwrap();
        bytes[6..10].copy_from_slice(&body_len.to_le_bytes());
        assert!(matches!(
            BinaryOpCodec.decode(&bytes),
            Err(CodecError::Invalid(
                "trailing bytes in binary envelope body"
            ))
        ));
    }

    #[test]
    fn insert_block_skeleton_roundtrip() {
        let env = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertBlock {
                parent: None,
                after: None,
                id: OpId {
                    counter: 1,
                    peer: 7,
                },
                right_origin: None,
                block: BlockSkeleton {
                    block_id: Uuid::from_u128(99),
                    kind: BlockKindSkeleton::Paragraph {
                        text: String::new(),
                    },
                },
            }),
        };
        let bytes = BinaryOpCodec.encode(&env).unwrap();
        assert_eq!(BinaryOpCodec.decode(&bytes).unwrap(), env);
    }
}
