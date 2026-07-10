//! Wire DTOs for collaborative document operations.
//!
//! These types are the **only** forms serialized onto the sync wire. Live
//! `Sequence` maps, pending buffers, and runtime `Block` state are never
//! encoded here.

use crate::core::OpId;
use crate::doc::BlockId;
use serde::{Deserialize, Serialize};

/// Current wire protocol version for [`Envelope`].
pub const WIRE_VERSION: u16 = 1;

/// Maximum nested BlockQuote / structure depth accepted on encode and decode.
///
/// Kept modest so well-formed over-deep payloads can be deserialized far enough
/// for this crate's depth check to run (serde's default recursion limit is 128;
/// each quote level costs several enum layers in JSON).
pub const MAX_WIRE_NEST_DEPTH: u32 = 16;

/// Versioned operation envelope carried as `sync::Operation` payload bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u16,
    pub body: OpBody,
}

/// Top-level body tag for an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpBody {
    /// One logical document operation (may expand to multiple nested RGA effects later).
    Doc(DocOp),
}

/// Serializable document operations for the collaborative wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocOp {
    /// Block-level RGA insert with skeleton only (no nested live Sequence).
    InsertBlock {
        after: Option<OpId>,
        id: OpId,
        right_origin: Option<OpId>,
        block: BlockSkeleton,
    },
    /// Block-level RGA delete.
    DeleteBlock {
        target: OpId,
        /// Delete-op id; also `Operation.id` when this is the sole effect.
        id: OpId,
    },
}

/// Serializable block creation payload — no Sequence maps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSkeleton {
    pub block_id: BlockId,
    pub kind: BlockKindSkeleton,
}

/// Wire form of block kinds. Paragraph `text` is the body in string-mode;
/// unit-mode sessions require empty paragraph text (session-enforced).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKindSkeleton {
    Paragraph { text: String },
    CodeFence { info: Option<String>, text: String },
    BlockQuote { children: Vec<BlockSkeletonInsert> },
    RawBlock { raw: String },
}

/// Nested block insert used inside blockquotes on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSkeletonInsert {
    pub after: Option<OpId>,
    pub id: OpId,
    pub right_origin: Option<OpId>,
    pub block: BlockSkeleton,
}

/// Returns `true` when the envelope does not violate the unit-mode empty-paragraph
/// rule: only `InsertBlock` with non-empty `Paragraph` text returns `false`.
///
/// Callers in unit-mode sessions use this to reject; the codec never rejects on
/// this predicate during decode of historical/string-mode payloads.
pub fn insert_block_paragraph_is_empty(envelope: &Envelope) -> bool {
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock { block, .. }) => match &block.kind {
            BlockKindSkeleton::Paragraph { text } => text.is_empty(),
            _ => true,
        },
        OpBody::Doc(DocOp::DeleteBlock { .. }) => true,
    }
}

/// Validate structural limits on an envelope (nest depth). Does not enforce unit-mode rules.
pub(crate) fn validate_envelope_structure(envelope: &Envelope) -> Result<(), super::CodecError> {
    match &envelope.body {
        OpBody::Doc(DocOp::InsertBlock { block, .. }) => {
            check_kind_depth(&block.kind, 0)?;
        }
        OpBody::Doc(DocOp::DeleteBlock { .. }) => {}
    }
    Ok(())
}

fn check_kind_depth(kind: &BlockKindSkeleton, depth: u32) -> Result<(), super::CodecError> {
    if depth > MAX_WIRE_NEST_DEPTH {
        return Err(super::CodecError::NestDepthExceeded);
    }
    match kind {
        BlockKindSkeleton::BlockQuote { children } => {
            for child in children {
                // Child block sits one level deeper than the quote itself.
                check_kind_depth(&child.block.kind, depth + 1)?;
            }
            Ok(())
        }
        BlockKindSkeleton::Paragraph { .. }
        | BlockKindSkeleton::CodeFence { .. }
        | BlockKindSkeleton::RawBlock { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_paragraph_predicate() {
        let empty = Envelope {
            version: WIRE_VERSION,
            body: OpBody::Doc(DocOp::InsertBlock {
                after: None,
                id: OpId {
                    counter: 1,
                    peer: 1,
                },
                right_origin: None,
                block: BlockSkeleton {
                    block_id: BlockId::nil(),
                    kind: BlockKindSkeleton::Paragraph {
                        text: String::new(),
                    },
                },
            }),
        };
        assert!(insert_block_paragraph_is_empty(&empty));
    }
}
