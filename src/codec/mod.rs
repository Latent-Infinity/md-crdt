//! Versioned wire codec for collaborative document operations.
//!
//! The codec owns encode/decode of [`Envelope`] payloads only. Causal logging
//! stays in `sync`; document apply stays in `session` / `doc`.
//!
//! Two envelope codecs are provided:
//! - [`JsonOpCodec`] — default, human-inspectable (`md_crdt_serde_json_v1` on the wire)
//! - [`BinaryOpCodec`] — compact postcard frame (`md_crdt_bin_v1`)

mod binary;
mod wire;

pub use binary::{BINARY_ENVELOPE_FORMAT, BINARY_ENVELOPE_MAGIC, BinaryOpCodec};
pub use wire::{
    BlockKindSkeleton, BlockSkeleton, BlockSkeletonInsert, ColumnAlignmentWire, DocOp, Envelope,
    ListItemSkeleton, MAX_WIRE_NEST_DEPTH, MovedBlockWire, MovedTextUnitWire, OpBody,
    TableCellWire, TextBlockKindWire, TextUnitWire, WIRE_VERSION, insert_block_paragraph_is_empty,
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

    /// Normalize retained transport bytes for JSON-backed snapshots and rebases.
    /// `envelope`, when supplied, is the already decoded payload.
    /// Borrowed output may be a proper subslice, such as unframed JSON bytes.
    fn snapshot_payload<'a>(
        &self,
        payload: &'a [u8],
        envelope: Option<&Envelope>,
    ) -> Result<std::borrow::Cow<'a, [u8]>, CodecError> {
        let decoded;
        let envelope = match envelope {
            Some(envelope) => envelope,
            None => {
                decoded = self
                    .decode(payload)
                    .map_err(|error| CodecError::Serde(error.to_string()))?;
                &decoded
            }
        };
        JsonOpCodec.encode(envelope).map(std::borrow::Cow::Owned)
    }

    /// Snapshot raw-byte charge without allocating the normalized payload.
    /// Implementations overriding `snapshot_payload` must return its exact length.
    fn snapshot_payload_len(
        &self,
        _payload: &[u8],
        envelope: &Envelope,
    ) -> Result<usize, CodecError> {
        struct Count(usize);
        impl std::io::Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        wire::validate_envelope_structure(envelope)?;
        let mut count = Count(0);
        serde_json::to_writer(&mut count, envelope)
            .map_err(|error| CodecError::Serde(error.to_string()))?;
        Ok(count.0)
    }
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
            #[derive(serde::Deserialize)]
            struct EnvelopeVersion {
                version: u16,
            }
            // Older wire bodies can lack newly required fields. Classify the
            // version on failure without parsing successful envelopes twice.
            if let Ok(header) = serde_json::from_slice::<EnvelopeVersion>(bytes)
                && header.version != WIRE_VERSION
            {
                return CodecError::UnknownVersion(header.version);
            }
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

    fn snapshot_payload<'a>(
        &self,
        payload: &'a [u8],
        _envelope: Option<&Envelope>,
    ) -> Result<std::borrow::Cow<'a, [u8]>, CodecError> {
        // The default transport already is the snapshot representation.
        Ok(std::borrow::Cow::Borrowed(payload))
    }

    fn snapshot_payload_len(
        &self,
        payload: &[u8],
        _envelope: &Envelope,
    ) -> Result<usize, CodecError> {
        Ok(payload.len())
    }
}
