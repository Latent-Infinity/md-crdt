//! Concrete, transport-agnostic workspace contract types.

use crate::doc::{BlockId, EditOp};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! persistent_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn from_u128(value: u128) -> Self {
                Self(Uuid::from_u128(value))
            }

            pub fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}

persistent_id!(VaultId);
persistent_id!(DocumentId);

/// Opaque precondition token for the observable state of one open document.
///
/// This is a content digest, not a sequence number: it is only ever compared for
/// equality (staleness preconditions). It deliberately does **not** implement
/// `Ord`/`PartialOrd`, since ordering two digests is meaningless — reverting to a
/// prior state yields the same token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionToken([u8; 16]);

impl RevisionToken {
    pub fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for RevisionToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Fingerprint of the Markdown bytes currently observed on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DiskFingerprint(pub u64);

impl fmt::Display for DiskFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

/// Stable handle returned when a document is opened or refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentHandle {
    pub vault_id: VaultId,
    pub document_id: DocumentId,
    pub revision: RevisionToken,
    pub disk_fingerprint: Option<DiskFingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockDescriptorKind {
    Paragraph,
    Heading,
    List,
    CodeFence,
    BlockQuote,
    RawBlock,
    Table,
}

/// Body-free structural description for bounded workspace inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDescriptor {
    pub id: BlockId,
    pub parent: Option<BlockId>,
    pub order: u32,
    pub kind: BlockDescriptorKind,
    pub heading_level: Option<u8>,
    pub source_bytes: usize,
    pub text_bytes: usize,
    pub content_digest: u64,
}

/// Bounded identities affected by one workspace transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSummary {
    pub created: Vec<BlockId>,
    pub deleted: Vec<BlockId>,
    pub moved: Vec<BlockId>,
    pub updated: Vec<BlockId>,
    pub operation_count: usize,
    pub revision: RevisionToken,
}

/// Concrete single-document edit batch. Atomic execution is added by the workspace layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditBatch {
    pub document_id: DocumentId,
    pub expected_revision: RevisionToken,
    pub operations: Vec<EditOp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReceipt {
    pub document_id: DocumentId,
    pub previous_revision: RevisionToken,
    pub revision: RevisionToken,
    pub changes: ChangeSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOutcome {
    pub document_id: DocumentId,
    pub revision: RevisionToken,
    pub disk_fingerprint: Option<DiskFingerprint>,
    pub bytes_written: usize,
    pub changed: bool,
}
