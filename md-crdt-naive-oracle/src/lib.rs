//! A naive, simple oracle implementation for differential testing.
use md_crdt_core::{OpId, StateVector};
use std::collections::BTreeMap;

#[derive(Debug, PartialEq)]
pub struct Sequence<T> {
    elements: BTreeMap<OpId, T>,
}

impl<T: Clone> Sequence<T> {
    pub fn new() -> Self {
        Sequence {
            elements: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, op: (OpId, T)) {
        self.elements.entry(op.0).or_insert(op.1);
    }

    pub fn elements(&self) -> Vec<T> {
        self.elements.values().rev().cloned().collect()
    }
}

impl<T: Clone> Default for Sequence<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A simple sync oracle that stores operations and can compute state vectors
#[derive(Debug, Clone, Default)]
pub struct SyncOracle {
    ops: BTreeMap<OpId, Vec<u8>>,
}

impl SyncOracle {
    pub fn new() -> Self {
        Self {
            ops: BTreeMap::new(),
        }
    }

    /// Apply an operation
    pub fn apply(&mut self, op_id: OpId, payload: Vec<u8>) {
        self.ops.entry(op_id).or_insert(payload);
    }

    /// Get the state vector
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

    /// Get operations since a given state vector
    pub fn changes_since(&self, since: &StateVector) -> Vec<(OpId, Vec<u8>)> {
        self.ops
            .iter()
            .filter(|(op_id, _)| {
                let seen = since.get(op_id.peer).unwrap_or(0);
                op_id.counter > seen
            })
            .map(|(id, payload)| (*id, payload.clone()))
            .collect()
    }

    /// Get all operations sorted by OpId
    pub fn all_ops(&self) -> Vec<(OpId, Vec<u8>)> {
        self.ops.iter().map(|(id, p)| (*id, p.clone())).collect()
    }

    /// Check if two oracles have the same state
    pub fn same_state(&self, other: &SyncOracle) -> bool {
        self.ops == other.ops
    }
}
