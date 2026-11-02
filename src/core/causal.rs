//! Causally maximal multi-value register state with a deterministic visible winner.

use super::{OpId, StateVector};
use serde::{Deserialize, Serialize};

/// One causally maximal write. The visible winner remains inline in its owner;
/// only concurrent non-winning candidates occupy the owner's conflict vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausalCandidate<T> {
    pub value: T,
    pub op_id: OpId,
    pub observed: StateVector,
}

fn observes(observed: &StateVector, writer: OpId, other: OpId) -> bool {
    // An origin's writes are ordered even for direct document APIs whose callers
    // do not supply an explicit observed frontier.
    (writer.peer == other.peer && writer.counter > other.counter)
        || observed.get(other.peer).unwrap_or(0) >= other.counter
}

/// Borrowed selection result; conflict indices refer to the unchanged input slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WinnerAfter {
    Current,
    Incoming,
    Conflict(usize),
}

/// Preview `apply_write` without copying payloads or mutating register state.
pub(crate) fn winner_after<T>(
    current_id: OpId,
    current_observed: &StateVector,
    conflicts: &[CausalCandidate<T>],
    incoming_id: OpId,
    incoming_observed: &StateVector,
) -> WinnerAfter {
    if incoming_id == current_id
        || observes(current_observed, current_id, incoming_id)
        || conflicts.iter().any(|candidate| {
            candidate.op_id == incoming_id
                || observes(&candidate.observed, candidate.op_id, incoming_id)
        })
    {
        return WinnerAfter::Current;
    }
    let (mut winner, mut winner_id) =
        if observes(incoming_observed, incoming_id, current_id) || incoming_id > current_id {
            (WinnerAfter::Incoming, incoming_id)
        } else {
            (WinnerAfter::Current, current_id)
        };
    for (index, candidate) in conflicts.iter().enumerate() {
        if candidate.op_id > winner_id && !observes(incoming_observed, incoming_id, candidate.op_id)
        {
            winner = WinnerAfter::Conflict(index);
            winner_id = candidate.op_id;
        }
    }
    winner
}

/// Retain exactly the causally maximal candidates, selecting the largest OpId
/// among them. Payloads move rather than clone; an uncontended write allocates no
/// conflict storage. Returns whether the visible winning operation changed.
pub(crate) fn apply_write<T>(
    value: &mut T,
    op_id: &mut OpId,
    observed: &mut StateVector,
    conflicts: &mut Vec<CausalCandidate<T>>,
    incoming: CausalCandidate<T>,
) -> bool {
    if incoming.op_id == *op_id
        || observes(observed, *op_id, incoming.op_id)
        || conflicts.iter().any(|candidate| {
            candidate.op_id == incoming.op_id
                || observes(&candidate.observed, candidate.op_id, incoming.op_id)
        })
    {
        return false;
    }
    conflicts.retain(|candidate| !observes(&incoming.observed, incoming.op_id, candidate.op_id));
    let dominates_winner = observes(&incoming.observed, incoming.op_id, *op_id);
    if !dominates_winner && incoming.op_id < *op_id {
        let at = conflicts.partition_point(|candidate| candidate.op_id < incoming.op_id);
        conflicts.insert(at, incoming);
        return false;
    }
    let old = CausalCandidate {
        value: std::mem::replace(value, incoming.value),
        op_id: std::mem::replace(op_id, incoming.op_id),
        observed: std::mem::replace(observed, incoming.observed),
    };
    if !dominates_winner {
        let at = conflicts.partition_point(|candidate| candidate.op_id < old.op_id);
        conflicts.insert(at, old);
    }
    if let Some(winner) = conflicts.pop_if(|candidate| candidate.op_id > *op_id) {
        let old = CausalCandidate {
            value: std::mem::replace(value, winner.value),
            op_id: std::mem::replace(op_id, winner.op_id),
            observed: std::mem::replace(observed, winner.observed),
        };
        let at = conflicts.partition_point(|candidate| candidate.op_id < old.op_id);
        conflicts.insert(at, old);
    }
    true
}
