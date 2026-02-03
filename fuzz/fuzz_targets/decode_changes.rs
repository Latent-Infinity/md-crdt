#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt_sync::{validate_changes, ChangeMessage, Document, ValidationLimits};

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = bincode::deserialize::<ChangeMessage>(data) {
        let _ = validate_changes(&message, &ValidationLimits::default(), 0);
        let mut doc = Document::new();
        let _ = doc.apply_changes(message);
    }
});
