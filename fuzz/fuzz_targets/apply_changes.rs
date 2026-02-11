#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt::core::{OpId, StateVector};
use md_crdt::sync::{ChangeMessage, Operation, SyncState};

fuzz_target!(|data: &[u8]| {
    let mut ops = Vec::new();
    for chunk in data.chunks(10) {
        if chunk.len() < 2 {
            break;
        }
        let counter = if chunk[1] == 0 { 1 } else { chunk[1] } as u64;
        let op = Operation {
            id: OpId {
                counter,
                peer: chunk[0] as u64,
            },
            payload: chunk[2..].to_vec(),
        };
        ops.push(op);
    }
    let message = ChangeMessage {
        since: StateVector::new(),
        ops,
    };
    let mut doc = SyncState::new();
    let _ = doc.apply_changes(message);
});
