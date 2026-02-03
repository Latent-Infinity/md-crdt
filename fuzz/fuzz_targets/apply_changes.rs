#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt_core::OpId;
use md_crdt_sync::{ChangeMessage, Document, Operation};

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
        since: md_crdt_core::StateVector::new(),
        ops,
    };
    let mut doc = Document::new();
    let _ = doc.apply_changes(message);
});
