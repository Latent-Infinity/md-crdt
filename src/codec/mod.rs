//! Versioned wire codec for collaborative document operations.
//!
//! The codec owns encode/decode of [`Envelope`] payloads only. Causal logging
//! stays in `sync`; document apply stays in `session` / `doc`.

mod wire;

pub use wire::{
    BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert, DocOp, Envelope, ListItemSkeleton,
    MAX_WIRE_NEST_DEPTH, OpBody, TextUnitWire, WIRE_VERSION, insert_block_paragraph_is_empty,
};

use thiserror::Error;

/// Errors from encoding or decoding wire envelopes.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CodecError {
    #[error("serde: {0}")]
    Serde(String),
    #[error("unknown wire version {0}")]
    UnknownVersion(u16),
    #[error("nest depth exceeded")]
    NestDepthExceeded,
    #[error("invalid envelope: {0}")]
    Invalid(&'static str),
}

/// Trait for encoding and decoding collaborative operation envelopes.
pub trait OpCodec {
    type Error: std::error::Error + Send + Sync + 'static;

    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>, Self::Error>;
    fn decode(&self, bytes: &[u8]) -> Result<Envelope, Self::Error>;
}

/// Default JSON codec for 0.1 (human-inspectable ops).
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonOpCodec;

impl OpCodec for JsonOpCodec {
    type Error = CodecError;

    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>, Self::Error> {
        wire::validate_envelope_structure(envelope)?;
        serde_json::to_vec(envelope).map_err(|e| CodecError::Serde(e.to_string()))
    }

    fn decode(&self, bytes: &[u8]) -> Result<Envelope, Self::Error> {
        let envelope: Envelope = serde_json::from_slice(bytes).map_err(|e| {
            let msg = e.to_string();
            // Extremely deep JSON may trip serde before our depth walk runs.
            if msg.contains("recursion limit") {
                CodecError::NestDepthExceeded
            } else {
                CodecError::Serde(msg)
            }
        })?;
        if envelope.version != WIRE_VERSION {
            return Err(CodecError::UnknownVersion(envelope.version));
        }
        wire::validate_envelope_structure(&envelope)?;
        Ok(envelope)
    }
}
