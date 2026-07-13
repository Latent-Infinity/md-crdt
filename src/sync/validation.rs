use super::ChangeMessage;
use crate::core::OpId;

/// Validation errors for incoming sync messages
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// Operation data is malformed or invalid
    #[error("malformed operation {op_id:?}: {kind}")]
    MalformedOperation { op_id: OpId, kind: MalformedKind },
    /// Operation references a non-existent element
    #[error("invalid reference in operation {op_id:?}")]
    InvalidReference { op_id: OpId },
    /// Message exceeds configured resource limits
    #[error("resource limit exceeded: {actual} > {limit}")]
    ResourceLimitExceeded { limit: usize, actual: usize },
    /// Pending operation buffer is full (backpressure)
    #[error("buffer full (capacity: {capacity})")]
    BufferFull { capacity: usize },
}

/// Kinds of malformed operations (avoids String allocation on error path)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedKind {
    EmptyPayload,
    ZeroCounter,
    InvalidPayload,
    InvalidSequence,
    UnexpectedFormat,
}

impl std::fmt::Display for MalformedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MalformedKind::EmptyPayload => write!(f, "empty payload"),
            MalformedKind::ZeroCounter => write!(f, "counter cannot be zero"),
            MalformedKind::InvalidPayload => write!(f, "invalid payload"),
            MalformedKind::InvalidSequence => write!(f, "invalid sequence"),
            MalformedKind::UnexpectedFormat => write!(f, "unexpected format"),
        }
    }
}

/// Configuration for validation limits
#[derive(Debug, Clone)]
pub struct ValidationLimits {
    pub max_ops_per_message: usize,
    pub max_payload_bytes: usize,
    pub max_pending_buffer: usize,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self {
            max_ops_per_message: 10_000,
            max_payload_bytes: 10 * 1024 * 1024, // 10 MB
            max_pending_buffer: 100_000,
        }
    }
}

/// Validate a change message against configured limits
pub fn validate_changes(
    message: &ChangeMessage,
    limits: &ValidationLimits,
    pending_count: usize,
) -> Result<(), ValidationError> {
    // Check operation count limit
    if message.ops.len() > limits.max_ops_per_message {
        return Err(ValidationError::ResourceLimitExceeded {
            limit: limits.max_ops_per_message,
            actual: message.ops.len(),
        });
    }

    // Check total payload size
    let total_payload: usize = message.ops.iter().map(|op| op.payload.len()).sum();
    if total_payload > limits.max_payload_bytes {
        return Err(ValidationError::ResourceLimitExceeded {
            limit: limits.max_payload_bytes,
            actual: total_payload,
        });
    }

    // Check if pending buffer would overflow (backpressure)
    if pending_count + message.ops.len() > limits.max_pending_buffer {
        return Err(ValidationError::BufferFull {
            capacity: limits.max_pending_buffer,
        });
    }

    // Validate each operation
    for op in &message.ops {
        // Check for empty payload (malformed)
        if op.payload.is_empty() {
            return Err(ValidationError::MalformedOperation {
                op_id: op.id,
                kind: MalformedKind::EmptyPayload,
            });
        }

        // Check for zero counter (invalid OpId)
        if op.id.counter == 0 {
            return Err(ValidationError::MalformedOperation {
                op_id: op.id,
                kind: MalformedKind::ZeroCounter,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_kinds_have_stable_messages() {
        let cases = [
            (MalformedKind::EmptyPayload, "empty payload"),
            (MalformedKind::ZeroCounter, "counter cannot be zero"),
            (MalformedKind::InvalidPayload, "invalid payload"),
            (MalformedKind::InvalidSequence, "invalid sequence"),
            (MalformedKind::UnexpectedFormat, "unexpected format"),
        ];

        for (kind, expected) in cases {
            assert_eq!(kind.to_string(), expected);
        }
    }
}
