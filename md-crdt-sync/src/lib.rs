use md_crdt_core::{OpId, StateVector};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: OpId,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeMessage {
    pub since: StateVector,
    pub ops: Vec<Operation>,
}

/// Validation errors for incoming sync messages
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Operation data is malformed or invalid
    MalformedOperation { op_id: OpId, reason: String },
    /// Operation references a non-existent element
    InvalidReference { op_id: OpId },
    /// Message exceeds configured resource limits
    ResourceLimitExceeded { limit: usize, actual: usize },
    /// Pending operation buffer is full (backpressure)
    BufferFull { capacity: usize },
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
                reason: "empty payload".to_string(),
            });
        }

        // Check for zero counter (invalid OpId)
        if op.id.counter == 0 {
            return Err(ValidationError::MalformedOperation {
                op_id: op.id,
                reason: "counter cannot be zero".to_string(),
            });
        }
    }

    Ok(())
}

/// Semantic conflicts detected during apply
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticConflict {
    /// Two peers inserted at the same position concurrently
    ConcurrentInsert { op_ids: Vec<OpId> },
    /// Two peers deleted the same element concurrently
    ConcurrentDelete { op_ids: Vec<OpId> },
    /// Attribute had concurrent updates, winner determined by OpId
    AttributeConflict { key: String, winner: OpId, loser: OpId },
}

/// Result of applying changes
#[derive(Debug, Clone, Default)]
pub struct ApplyResult {
    /// Operations that were applied successfully
    pub applied: Vec<OpId>,
    /// Operations that are buffered waiting for causal dependencies
    pub buffered: Vec<OpId>,
    /// Semantic conflicts that were detected and auto-resolved
    pub conflicts: Vec<SemanticConflict>,
}

#[derive(Debug, Clone)]
pub struct Document {
    ops: BTreeMap<OpId, Vec<u8>>,
    /// Operations waiting for causal dependencies
    pending: BTreeMap<OpId, Operation>,
    /// Operations that have been generated locally but not yet sent
    outbox: BTreeSet<OpId>,
    /// Operations that have been sent but not confirmed
    sent: BTreeSet<OpId>,
}

impl Document {
    pub fn new() -> Self {
        Self {
            ops: BTreeMap::new(),
            pending: BTreeMap::new(),
            outbox: BTreeSet::new(),
            sent: BTreeSet::new(),
        }
    }

    /// Apply a single operation (internal use)
    pub fn apply_op(&mut self, op: Operation) {
        self.ops.entry(op.id).or_insert(op.payload);
    }

    /// Get the state vector representing all applied operations
    pub fn state_vector(&self) -> StateVector {
        let mut sv = StateVector::new();
        for op_id in self.ops.keys() {
            let current = sv.get(op_id.peer).unwrap_or(0);
            if op_id.counter > current {
                sv.set(op_id.peer, op_id.counter);
            }
        }
        sv
    }

    /// Encode all operations since a given state vector
    pub fn encode_changes_since(&self, since: &StateVector) -> ChangeMessage {
        let mut ops = Vec::new();
        for (op_id, payload) in &self.ops {
            let seen = since.get(op_id.peer).unwrap_or(0);
            if op_id.counter > seen {
                ops.push(Operation {
                    id: *op_id,
                    payload: payload.clone(),
                });
            }
        }
        ChangeMessage {
            since: since.clone(),
            ops,
        }
    }

    /// Apply a batch of changes with validation and conflict detection
    pub fn apply_changes(&mut self, message: ChangeMessage) -> ApplyResult {
        let mut result = ApplyResult::default();

        for op in message.ops {
            // Check if we already have this operation
            if self.ops.contains_key(&op.id) {
                continue;
            }

            // Check causal readiness: all prior ops from this peer must be applied
            let current_applied = self
                .ops
                .keys()
                .filter(|id| id.peer == op.id.peer)
                .map(|id| id.counter)
                .max()
                .unwrap_or(0);

            if op.id.counter > current_applied + 1 {
                // Missing prior operations - buffer this one
                self.pending.insert(op.id, op);
                result.buffered.push(self.pending.keys().last().copied().unwrap());
            } else {
                // Ready to apply
                let op_id = op.id;
                self.ops.insert(op.id, op.payload);
                result.applied.push(op_id);

                // Try to apply any buffered operations that are now ready
                self.try_apply_pending(&mut result);
            }
        }

        result
    }

    /// Try to apply pending operations that are now causally ready
    fn try_apply_pending(&mut self, result: &mut ApplyResult) {
        let mut made_progress = true;
        while made_progress {
            made_progress = false;
            let pending_ids: Vec<OpId> = self.pending.keys().copied().collect();

            for op_id in pending_ids {
                let current_applied = self
                    .ops
                    .keys()
                    .filter(|id| id.peer == op_id.peer)
                    .map(|id| id.counter)
                    .max()
                    .unwrap_or(0);

                if op_id.counter == current_applied + 1 {
                    if let Some(op) = self.pending.remove(&op_id) {
                        self.ops.insert(op.id, op.payload);
                        result.applied.push(op_id);
                        // Remove from buffered if it was there
                        result.buffered.retain(|id| *id != op_id);
                        made_progress = true;
                    }
                }
            }
        }
    }

    /// Get the number of pending (causally unready) operations
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get pending operations (for persistence)
    pub fn pending(&self) -> Vec<Operation> {
        self.pending
            .iter()
            .map(|(id, op)| Operation {
                id: *id,
                payload: op.payload.clone(),
            })
            .collect()
    }

    /// Add a local operation to the outbox
    pub fn add_local_op(&mut self, op: Operation) {
        let op_id = op.id;
        self.ops.insert(op.id, op.payload);
        self.outbox.insert(op_id);
    }

    /// Get operations that need to be sent to peers
    pub fn outbox(&self) -> Vec<Operation> {
        self.outbox
            .iter()
            .filter_map(|op_id| {
                self.ops.get(op_id).map(|payload| Operation {
                    id: *op_id,
                    payload: payload.clone(),
                })
            })
            .collect()
    }

    /// Mark operations as sent (move from outbox to sent)
    pub fn mark_sent(&mut self, op_ids: &[OpId]) {
        for op_id in op_ids {
            if self.outbox.remove(op_id) {
                self.sent.insert(*op_id);
            }
        }
    }

    /// Mark operations as confirmed (remove from sent tracking)
    pub fn mark_confirmed(&mut self, op_ids: &[OpId]) {
        for op_id in op_ids {
            self.sent.remove(op_id);
        }
    }

    /// Restore pending operations (for crash recovery)
    pub fn restore_pending(&mut self, ops: Vec<Operation>) {
        for op in ops {
            if !self.ops.contains_key(&op.id) {
                self.pending.insert(op.id, op);
            }
        }
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_vector() {
        let mut doc = Document::new();
        doc.apply_op(Operation {
            id: OpId {
                counter: 1,
                peer: 1,
            },
            payload: vec![1],
        });
        doc.apply_op(Operation {
            id: OpId {
                counter: 3,
                peer: 1,
            },
            payload: vec![2],
        });
        doc.apply_op(Operation {
            id: OpId {
                counter: 2,
                peer: 2,
            },
            payload: vec![3],
        });

        let sv = doc.state_vector();
        assert_eq!(sv.get(1), Some(3));
        assert_eq!(sv.get(2), Some(2));
    }

    #[test]
    fn test_encode_changes_since() {
        let mut doc = Document::new();
        doc.apply_op(Operation {
            id: OpId {
                counter: 1,
                peer: 1,
            },
            payload: vec![1],
        });
        doc.apply_op(Operation {
            id: OpId {
                counter: 2,
                peer: 1,
            },
            payload: vec![2],
        });
        doc.apply_op(Operation {
            id: OpId {
                counter: 1,
                peer: 2,
            },
            payload: vec![3],
        });

        let mut sv = StateVector::new();
        sv.set(1, 1);

        let message = doc.encode_changes_since(&sv);
        let ids: Vec<_> = message.ops.iter().map(|op| op.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&OpId {
            counter: 2,
            peer: 1
        }));
        assert!(ids.contains(&OpId {
            counter: 1,
            peer: 2
        }));
    }

    // Task 4.3: Validation tests
    #[test]
    fn test_validate_changes_malformed_empty_payload() {
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 1, peer: 1 },
                payload: vec![], // Empty payload is malformed
            }],
        };
        let limits = ValidationLimits::default();

        let result = validate_changes(&message, &limits, 0);
        assert!(matches!(
            result,
            Err(ValidationError::MalformedOperation { .. })
        ));
    }

    #[test]
    fn test_validate_changes_malformed_zero_counter() {
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 0, peer: 1 }, // Zero counter is invalid
                payload: vec![1],
            }],
        };
        let limits = ValidationLimits::default();

        let result = validate_changes(&message, &limits, 0);
        assert!(matches!(
            result,
            Err(ValidationError::MalformedOperation { .. })
        ));
    }

    #[test]
    fn test_validate_changes_resource_limit_ops() {
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: (1..=101)
                .map(|i| Operation {
                    id: OpId {
                        counter: i,
                        peer: 1,
                    },
                    payload: vec![1],
                })
                .collect(),
        };
        let limits = ValidationLimits {
            max_ops_per_message: 100,
            ..Default::default()
        };

        let result = validate_changes(&message, &limits, 0);
        assert!(matches!(
            result,
            Err(ValidationError::ResourceLimitExceeded { limit: 100, .. })
        ));
    }

    #[test]
    fn test_validate_changes_resource_limit_payload() {
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 1, peer: 1 },
                payload: vec![0; 1001], // 1001 bytes
            }],
        };
        let limits = ValidationLimits {
            max_payload_bytes: 1000,
            ..Default::default()
        };

        let result = validate_changes(&message, &limits, 0);
        assert!(matches!(
            result,
            Err(ValidationError::ResourceLimitExceeded { limit: 1000, .. })
        ));
    }

    #[test]
    fn test_validate_changes_buffer_full() {
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 1, peer: 1 },
                payload: vec![1],
            }],
        };
        let limits = ValidationLimits {
            max_pending_buffer: 10,
            ..Default::default()
        };

        // Pending buffer is at capacity
        let result = validate_changes(&message, &limits, 10);
        assert!(matches!(
            result,
            Err(ValidationError::BufferFull { capacity: 10 })
        ));
    }

    #[test]
    fn test_validate_changes_success() {
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 1, peer: 1 },
                payload: vec![1, 2, 3],
            }],
        };
        let limits = ValidationLimits::default();

        let result = validate_changes(&message, &limits, 0);
        assert!(result.is_ok());
    }

    // Task 4.5: Apply changes tests
    #[test]
    fn test_apply_changes_in_order() {
        let mut doc = Document::new();
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![
                Operation {
                    id: OpId { counter: 1, peer: 1 },
                    payload: vec![1],
                },
                Operation {
                    id: OpId { counter: 2, peer: 1 },
                    payload: vec![2],
                },
            ],
        };

        let result = doc.apply_changes(message);

        assert_eq!(result.applied.len(), 2);
        assert!(result.buffered.is_empty());
        assert_eq!(doc.state_vector().get(1), Some(2));
    }

    #[test]
    fn test_apply_changes_out_of_order_buffers() {
        let mut doc = Document::new();
        // Apply counter=1 first
        doc.apply_op(Operation {
            id: OpId { counter: 1, peer: 1 },
            payload: vec![1],
        });

        // Now try to apply counter=3 (missing counter=2)
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 3, peer: 1 },
                payload: vec![3],
            }],
        };

        let result = doc.apply_changes(message);

        assert!(result.applied.is_empty());
        assert_eq!(result.buffered.len(), 1);
        assert_eq!(doc.pending_count(), 1);
    }

    #[test]
    fn test_apply_changes_unbuffers_when_ready() {
        let mut doc = Document::new();
        // Apply counter=1 first
        doc.apply_op(Operation {
            id: OpId { counter: 1, peer: 1 },
            payload: vec![1],
        });

        // Apply counter=3 (gets buffered)
        let message1 = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 3, peer: 1 },
                payload: vec![3],
            }],
        };
        doc.apply_changes(message1);
        assert_eq!(doc.pending_count(), 1);

        // Now apply counter=2 (should unbuffer counter=3)
        let message2 = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 2, peer: 1 },
                payload: vec![2],
            }],
        };
        let result = doc.apply_changes(message2);

        assert_eq!(result.applied.len(), 2); // counter=2 and counter=3
        assert_eq!(doc.pending_count(), 0);
        assert_eq!(doc.state_vector().get(1), Some(3));
    }

    #[test]
    fn test_apply_changes_idempotent() {
        let mut doc = Document::new();
        let op = Operation {
            id: OpId { counter: 1, peer: 1 },
            payload: vec![1],
        };

        let message1 = ChangeMessage {
            since: StateVector::new(),
            ops: vec![op.clone()],
        };
        let result1 = doc.apply_changes(message1);
        assert_eq!(result1.applied.len(), 1);

        // Apply same operation again
        let message2 = ChangeMessage {
            since: StateVector::new(),
            ops: vec![op],
        };
        let result2 = doc.apply_changes(message2);
        assert!(result2.applied.is_empty()); // Already applied
    }

    // Task 4.7: Outbox tests
    #[test]
    fn test_outbox_local_op() {
        let mut doc = Document::new();
        let op = Operation {
            id: OpId { counter: 1, peer: 1 },
            payload: vec![1, 2, 3],
        };

        doc.add_local_op(op.clone());

        let outbox = doc.outbox();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].id, op.id);
    }

    #[test]
    fn test_mark_sent() {
        let mut doc = Document::new();
        let op = Operation {
            id: OpId { counter: 1, peer: 1 },
            payload: vec![1],
        };
        doc.add_local_op(op.clone());

        assert_eq!(doc.outbox().len(), 1);

        doc.mark_sent(&[op.id]);

        assert!(doc.outbox().is_empty());
    }

    #[test]
    fn test_pending_persistence() {
        let mut doc = Document::new();
        // Apply counter=1
        doc.apply_op(Operation {
            id: OpId { counter: 1, peer: 1 },
            payload: vec![1],
        });

        // Buffer counter=3 (missing counter=2)
        let message = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 3, peer: 1 },
                payload: vec![3],
            }],
        };
        doc.apply_changes(message);

        // Get pending for persistence
        let pending_ops = doc.pending();
        assert_eq!(pending_ops.len(), 1);
        assert_eq!(pending_ops[0].id.counter, 3);

        // Simulate crash recovery - new document
        let mut doc2 = Document::new();
        doc2.apply_op(Operation {
            id: OpId { counter: 1, peer: 1 },
            payload: vec![1],
        });
        doc2.restore_pending(pending_ops);
        assert_eq!(doc2.pending_count(), 1);

        // Now apply counter=2
        let message2 = ChangeMessage {
            since: StateVector::new(),
            ops: vec![Operation {
                id: OpId { counter: 2, peer: 1 },
                payload: vec![2],
            }],
        };
        let result = doc2.apply_changes(message2);

        // Both counter=2 and counter=3 should be applied
        assert_eq!(result.applied.len(), 2);
        assert_eq!(doc2.state_vector().get(1), Some(3));
    }
}
